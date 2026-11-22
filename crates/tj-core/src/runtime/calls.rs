//! Reliable server calls: at least once from devices, at most once on the server.
//!
//! Devices: every call to an `@server` object goes into a durable outbox and carries a
//! unique id. If the server cannot be reached, the call is retried with backoff (the caller's
//! `await` simply waits), and the outbox survives restarts. Calls to one object are sent one
//! at a time, so they arrive in the order they were made.
//!
//! Servers: a call that carries an id runs once. The object records the id, who made the
//! call, the method, a hash of the arguments and the result with its state, in the same
//! write. A retry gets the first result without running again, even after a restart; the
//! same id from another caller or for another call gets 422 and never the stored result.
//! A repeat that arrives while the first is still running waits for it.
//!
//! Devices scope the outbox to the signed-in user (the `identity` input): calls queued by
//! someone else are dropped, never sent as the new user.

use super::access::Who;
use super::objects::CallRecord;
use super::protocol::{Effect, Output};
use super::{By, Entry, Runtime, TimerKind};
use crate::vm::value::{ObjRef, Value};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use std::collections::HashSet;
use std::rc::Rc;

/// An `invoke` waiting for its calling fiber to suspend: the promise it settles, the call,
/// and who it acts for, in which chain.
pub(crate) struct Invocation {
    promise: u32,
    object: String,
    method: String,
    args: Vec<Value>,
    who: Who,
    chain: u64,
}

const OUTBOX_KEY: &str = "tj:outbox";
const FIRST_RETRY_MS: f64 = 1000.0;
const MAX_RETRY_MS: f64 = 30_000.0;
/// Reply ids from here up are the runtime's own: the results of calls made once.
pub(crate) const CALL_REPLY_BASE: u64 = 1 << 62;
/// Reply ids from here up to `CALL_REPLY_BASE` settle the promise of an `invoke`.
pub(crate) const INVOKE_REPLY_BASE: u64 = 1 << 61;
const MAX_CALL_ID: usize = 128;

#[derive(Serialize, Deserialize, Default)]
pub(crate) struct Outbox {
    /// A random id for this device, so call ids are unique across devices.
    device: String,
    next: u64,
    pending: Vec<Pending>,
    /// The signed-in user calls are queued for (`identity`).
    #[serde(default)]
    pub(crate) user: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Pending {
    call: String,
    object: String,
    method: String,
    args: Vec<Json>,
    /// The user it was queued for; it is never sent as anyone else.
    #[serde(default)]
    user: Option<String>,
    /// The promise the caller awaits. Callers do not survive a restart, so this is not stored.
    #[serde(skip)]
    promise: Option<u32>,
    /// The attempt in flight.
    #[serde(skip)]
    effect: Option<u32>,
    #[serde(skip)]
    attempts: u32,
    #[serde(skip)]
    retry_at: f64,
}

/// splitmix64, to turn the random state into a device id without consuming it.
impl Runtime {
    /// Loads the outbox at boot. What is still pending is sent on the first input, not at
    /// boot: if that input says a different user signed in (`identity`), their calls are
    /// dropped first, so nothing queued by one user leaves under another's token.
    pub(crate) fn load_outbox(&mut self) {
        let stored = self.store.load(OUTBOX_KEY).unwrap_or_else(|e| {
            self.outputs.push(Output::Error { message: format!("could not read queued server calls: {e}"), trace: Vec::new() });
            None
        });
        if let Some(outbox) = stored.and_then(|t| serde_json::from_str::<Outbox>(&t).ok()) {
            self.outbox_held = !outbox.pending.is_empty();
            self.outbox = outbox;
        }
    }

    /// The first input after boot releases the held outbox (see [`Runtime::load_outbox`]).
    pub(crate) fn release_outbox(&mut self) {
        if std::mem::take(&mut self.outbox_held) {
            self.dispatch_calls();
        }
    }

    fn save_outbox(&mut self) {
        let text = serde_json::to_string(&self.outbox).expect("the outbox serializes");
        if let Err(e) = self.store.save(OUTBOX_KEY, &text) {
            self.outputs.push(Output::Error { message: format!("could not save the outbox of server calls: {e}"), trace: Vec::new() });
        }
    }

    /// Queues a call to an `@server` object and returns the promise its caller awaits.
    pub(crate) fn queue_call(&mut self, object: String, method: String, args: Vec<Json>) -> u32 {
        if self.outbox.device.is_empty() {
            // 128 bits from the generator, unguessable once the host sent `entropy`.
            let mut id = [0u8; 16];
            self.drbg.fill(&mut id);
            self.outbox.device = crate::crypto::hex(&id);
        }
        self.outbox.next += 1;
        let call = format!("{}-{}", self.outbox.device, self.outbox.next);
        self.next_effect += 1;
        let promise = self.next_effect;
        let user = self.outbox.user.clone();
        self.outbox.pending.push(Pending { call, object, method, args, user, promise: Some(promise), effect: None, attempts: 0, retry_at: 0.0 });
        self.save_outbox();
        self.dispatch_calls();
        promise
    }

    /// Sends the first waiting call of each object, when its retry time has come.
    pub(crate) fn dispatch_calls(&mut self) {
        let mut heads = HashSet::new();
        let ready: Vec<usize> = (0..self.outbox.pending.len())
            .filter(|&i| {
                let p = &self.outbox.pending[i];
                heads.insert(p.object.clone()) && p.effect.is_none() && p.retry_at <= self.now
            })
            .collect();
        for i in ready {
            // The first attempt answers the caller's promise directly; retries get new ids.
            let id = match self.outbox.pending[i].promise {
                Some(promise) if self.outbox.pending[i].attempts == 0 => promise,
                _ => {
                    self.next_effect += 1;
                    self.next_effect
                }
            };
            let p = &mut self.outbox.pending[i];
            p.effect = Some(id);
            let effect = Effect::Call { object: p.object.clone(), method: p.method.clone(), args: p.args.clone(), call: p.call.clone() };
            self.outputs.push(Output::Effect { id, effect });
        }
    }

    /// The server is reachable again: send waiting calls without waiting out their backoff.
    pub(crate) fn retry_calls_now(&mut self) {
        for p in &mut self.outbox.pending {
            p.retry_at = 0.0;
        }
        self.dispatch_calls();
    }

    pub(crate) fn is_queued_call(&self, effect: u32) -> bool {
        self.outbox.pending.iter().any(|p| p.effect == Some(effect))
    }

    /// The host finished an attempt. If the server was unreachable, the call is retried later;
    /// otherwise it leaves the outbox and its caller gets the result.
    pub(crate) fn call_attempt_done(&mut self, effect: u32, ok: Option<Json>, error: Option<String>, field: Option<String>, unreachable: bool) {
        let Some(i) = self.outbox.pending.iter().position(|p| p.effect == Some(effect)) else { return };
        if unreachable {
            let p = &mut self.outbox.pending[i];
            p.effect = None;
            p.attempts += 1;
            p.retry_at = self.now + (FIRST_RETRY_MS * 2f64.powi(p.attempts as i32 - 1)).min(MAX_RETRY_MS);
            let at = p.retry_at;
            self.push_timer(at, TimerKind::Outbox);
            return;
        }
        let p = self.outbox.pending.remove(i);
        self.save_outbox();
        match (p.promise, error) {
            (Some(promise), error) => self.resume(promise, ok, error, field),
            // Queued before a restart: nobody awaits it any more, but a failure must not vanish.
            (None, Some(e)) => self.outputs.push(Output::Error { message: format!("{}.{} (queued before a restart) failed: {e}", p.object, p.method), trace: Vec::new() }),
            (None, None) => {}
        }
        self.dispatch_calls();
    }

    /// The signed-in user changed: calls queued by anyone else are dropped (each reported
    /// to its caller, or as an error), and replicas read as the previous user are cleared.
    pub(crate) fn set_identity(&mut self, user: Option<String>, roles: Vec<String>) {
        let roles: Rc<[Rc<str>]> = roles.iter().map(|r| Rc::from(r.as_str())).collect();
        let new_roles = roles != self.device_roles;
        if new_roles {
            self.device_roles = roles;
            self.ran_code = true;
        }
        if self.outbox.user == user {
            // Same person, other roles: what only the old roles could read goes; their queued
            // calls stay, as the server checks each one again.
            if new_roles {
                self.clear_replicas();
            }
            self.release_outbox();
            return;
        }
        let (keep, dropped): (Vec<Pending>, Vec<Pending>) = std::mem::take(&mut self.outbox.pending).into_iter().partition(|p| p.user == user);
        self.outbox.pending = keep;
        // Someone was signed in and is no longer: the device's own objects go too. An app
        // nobody signs in to keeps them.
        let someone_left = self.outbox.user.is_some();
        self.outbox.user = user;
        // `caller.id` changed: the UI is drawn again for the new user.
        self.ran_code = true;
        // The calls they left are answered first, so what their code does with the answer (a
        // form showing the error) is cleared with the rest. The message names no object id,
        // which may be the person's.
        for p in dropped {
            let class = p.object.split('/').next().unwrap_or_default();
            let why = format!("{class}.{} was queued by another user and was not sent", p.method);
            match p.promise {
                Some(promise) => self.resume(promise, None, Some(why), None),
                None => self.outputs.push(Output::Error { message: why, trace: Vec::new() }),
            }
        }
        self.run_queued();
        if someone_left {
            self.clear_device_objects();
        }
        self.save_outbox();
        self.clear_replicas();
        self.release_outbox();
    }

    /// `invoke(object, method, args)`: queues a call of an app method as the current caller,
    /// run once the calling fiber has suspended (so its promise has someone waiting on it).
    pub(crate) fn invoke_as_caller(&mut self, object: String, method: String, args: Vec<Value>) -> u32 {
        self.next_effect += 1;
        let promise = self.next_effect;
        let (who, chain) = (self.running_caller.clone(), self.running_chain);
        self.invocations.push_back(Invocation { promise, object, method, args, who, chain });
        promise
    }

    /// Runs queued `invoke`s, each checked against the rules for its caller, exactly as a
    /// request from them would be. Each joins its caller's chain, so it may enter objects the
    /// caller holds. Whether any ran.
    pub(crate) fn run_invocations(&mut self) -> bool {
        let mut ran = false;
        while let Some(i) = self.invocations.pop_front() {
            ran = true;
            let reply = Some(INVOKE_REPLY_BASE + u64::from(i.promise));
            if let Some(why) = self.refusal(&i.object, Some(&i.method), &i.who) {
                self.count_refused(&i.object, &i.method);
                self.reply_status(reply, 403, why);
                continue;
            }
            // Arguments from a model are checked like any from outside.
            if matches!(i.who, Who::Remote { .. }) {
                let args: Vec<Json> = i.args.iter().map(|a| a.to_json().unwrap_or(Json::Null)).collect();
                if let Some(why) = self.bad_args(&i.object, &i.method, &args) {
                    self.reply_status(reply, 400, why);
                    continue;
                }
            }
            if !self.touched.is_empty() {
                self.flush();
            }
            match self.resolve_object(&i.object) {
                Ok(obj) => self.invoke(obj, &i.method, i.args, true, reply, Entry { who: i.who, chain: i.chain, by: By::Invoke }),
                Err(e) => self.reply(reply, Err((e, None))),
            }
        }
        ran
    }

    /// Refuses a call before it started. A call made once is forgotten, not recorded, so a
    /// retry with the same id runs.
    pub(crate) fn refuse_start(&mut self, reply: Option<u64>, status: u16, why: String) {
        let Some(internal) = reply.filter(|&id| id >= CALL_REPLY_BASE) else { return self.reply_status(reply, status, why) };
        let Some((_, call, _)) = self.call_replies.remove(&internal) else { return };
        let Some((_, waiting)) = self.running_calls.remove(&call) else { return };
        for reply in waiting {
            self.reply_status(reply, status, why.clone());
        }
    }

    /// Pending server calls, for automation.
    pub(crate) fn outbox_json(&self) -> Json {
        Json::Array(self.outbox.pending.iter().map(|p| json!({ "call": p.call, "object": p.object, "method": p.method, "attempts": p.attempts })).collect())
    }

    /// Runs a call that carries an id at most once (see the module docs).
    pub(crate) fn call_once(&mut self, obj: Rc<ObjRef>, method: &str, args: &[Json], reply: Option<u64>, call: String, who: Who) {
        if call.is_empty() || call.len() > MAX_CALL_ID {
            self.reply_status(reply, 400, format!("a call id must be 1-{MAX_CALL_ID} characters"));
            return;
        }
        let by = match self.caller_tag(&who) {
            Ok(by) => by,
            Err(e) => return self.reply_status(reply, 503, e),
        };
        let mine = CallRecord::new(&call, by, method, args, self.now);
        if let Some(done) = self.call_record(&obj, &call) {
            match done.same_call(&mine) {
                true => self.reply_with_status(reply, done.result(), done.status()),
                false => self.reply_status(reply, 422, "this call id was already used for another call".into()),
            }
            return;
        }
        if let Some((running, waiting)) = self.running_calls.get_mut(&call) {
            if running.same_call(&mine) {
                waiting.push(reply);
            } else {
                self.reply_status(reply, 422, "this call id was already used for another call".into());
            }
            return;
        }
        self.next_call_reply += 1;
        let internal = CALL_REPLY_BASE + self.next_call_reply;
        self.call_replies.insert(internal, (obj.clone(), call.clone(), true));
        self.running_calls.insert(call, (mine, vec![reply]));
        let args = args.iter().map(Value::from_json).collect();
        self.invoke(obj, method, args, true, Some(internal), Entry::new(who));
    }

    /// Records the result of a call run by [`Runtime::call_once`] and answers everyone waiting.
    pub(crate) fn finish_call(&mut self, internal: u64, r: Result<Json, (String, Option<String>)>, status: Option<u16>) {
        let Some((obj, call, keep)) = self.call_replies.remove(&internal) else { return };
        let Some((record, waiting)) = self.running_calls.remove(&call) else { return };
        if keep {
            self.record_call(&obj, record.with_result(&r, status));
        }
        for reply in waiting {
            self.reply_with_status(reply, r.clone(), status);
        }
    }
}

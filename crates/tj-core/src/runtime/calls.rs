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
use super::{Runtime, TimerKind};
use crate::vm::value::{ObjRef, Value};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use std::collections::HashSet;
use std::rc::Rc;

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
    user: Option<String>,
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
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

impl Runtime {
    /// Loads the outbox at boot. What is still pending is sent on the first input, not at
    /// boot: if that input says a different user signed in (`identity`), their calls are
    /// dropped first, so nothing queued by one user leaves under another's token.
    pub(crate) fn load_outbox(&mut self) {
        if let Some(outbox) = self.store.load(OUTBOX_KEY).and_then(|t| serde_json::from_str::<Outbox>(&t).ok()) {
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
            // 128 bits from the host's seed, which hosts take from a secure random source.
            let a = mix(self.rng ^ self.now.to_bits());
            self.outbox.device = format!("{a:016x}{:016x}", mix(a));
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
    pub(crate) fn call_attempt_done(&mut self, effect: u32, ok: Option<Json>, error: Option<String>, unreachable: bool) {
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
            (Some(promise), error) => self.resume(promise, ok, error),
            // Queued before a restart: nobody awaits it any more, but a failure must not vanish.
            (None, Some(e)) => self.outputs.push(Output::Error { message: format!("{}.{} (queued before a restart) failed: {e}", p.object, p.method), trace: Vec::new() }),
            (None, None) => {}
        }
        self.dispatch_calls();
    }

    /// The signed-in user changed: calls queued by anyone else are dropped (each reported
    /// to its caller, or as an error), and replicas read as the previous user are cleared.
    pub(crate) fn set_identity(&mut self, user: Option<String>) {
        if self.outbox.user == user {
            self.release_outbox();
            return;
        }
        let (keep, dropped): (Vec<Pending>, Vec<Pending>) = std::mem::take(&mut self.outbox.pending).into_iter().partition(|p| p.user == user);
        self.outbox.pending = keep;
        self.outbox.user = user;
        for p in dropped {
            let why = format!("{}.{} was queued by another user and was not sent", p.object, p.method);
            match p.promise {
                Some(promise) => self.resume(promise, None, Some(why)),
                None => self.outputs.push(Output::Error { message: why, trace: Vec::new() }),
            }
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
        let who = self.running_caller.clone();
        self.invocations.push_back((promise, object, method, args, who));
        promise
    }

    /// Runs queued `invoke`s, each checked against the rules for its caller, exactly as a
    /// request from them would be.
    pub(crate) fn run_invocations(&mut self) {
        while let Some((promise, object, method, args, who)) = self.invocations.pop_front() {
            let reply = Some(INVOKE_REPLY_BASE + u64::from(promise));
            if let Some(why) = self.refusal(&object, Some(&method), &who) {
                self.reply_status(reply, 403, why);
                continue;
            }
            match self.resolve_object(&object) {
                Ok(obj) => self.invoke(obj, &method, args, true, reply, who),
                Err(e) => self.reply(reply, Err(e)),
            }
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
        let mine = CallRecord::new(&call, &who, method, args);
        if let Some(done) = self.call_record(&obj, &call) {
            match done.same_call(&mine) {
                true => self.reply(reply, done.result()),
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
        self.call_replies.insert(internal, (obj.clone(), call.clone()));
        self.running_calls.insert(call, (mine, vec![reply]));
        let args = args.iter().map(Value::from_json).collect();
        self.invoke(obj, method, args, true, Some(internal), who);
    }

    /// Records the result of a call run by [`Runtime::call_once`] and answers everyone waiting.
    pub(crate) fn finish_call(&mut self, internal: u64, r: Result<Json, String>) {
        let Some((obj, call)) = self.call_replies.remove(&internal) else { return };
        let Some((record, waiting)) = self.running_calls.remove(&call) else { return };
        self.record_call(&obj, record.with_result(&r));
        for reply in waiting {
            self.reply(reply, r.clone());
        }
    }
}

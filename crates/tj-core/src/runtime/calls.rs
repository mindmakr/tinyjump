//! Reliable server calls: at least once from devices, at most once on the server.
//!
//! Devices: every call to an `@server` object goes into a durable outbox and carries a
//! unique id. If the server cannot be reached, the call is retried with backoff (the caller's
//! `await` simply waits), and the outbox survives restarts. Calls to one object are sent one
//! at a time, so they arrive in the order they were made.
//!
//! Servers: a call that carries an id runs once. The object records the id and the result
//! with its state, in the same write, so a retry gets the first result without running
//! again, even after a restart. A repeat that arrives while the first is still running waits
//! for it.

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

#[derive(Serialize, Deserialize, Default)]
pub(crate) struct Outbox {
    /// A random id for this device, so call ids are unique across devices.
    device: String,
    next: u64,
    pending: Vec<Pending>,
}

#[derive(Serialize, Deserialize)]
struct Pending {
    call: String,
    object: String,
    method: String,
    args: Vec<Json>,
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
    /// Loads the outbox at boot and sends what is still pending.
    pub(crate) fn load_outbox(&mut self) {
        if let Some(outbox) = self.store.load(OUTBOX_KEY).and_then(|t| serde_json::from_str(&t).ok()) {
            self.outbox = outbox;
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
            self.outbox.device = format!("{:016x}", mix(self.rng ^ self.now.to_bits()));
        }
        self.outbox.next += 1;
        let call = format!("{}-{}", self.outbox.device, self.outbox.next);
        self.next_effect += 1;
        let promise = self.next_effect;
        self.outbox.pending.push(Pending { call, object, method, args, promise: Some(promise), effect: None, attempts: 0, retry_at: 0.0 });
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

    /// Pending server calls, for automation.
    pub(crate) fn outbox_json(&self) -> Json {
        Json::Array(self.outbox.pending.iter().map(|p| json!({ "call": p.call, "object": p.object, "method": p.method, "attempts": p.attempts })).collect())
    }

    /// Runs a call that carries an id at most once (see the module docs).
    pub(crate) fn call_once(&mut self, obj: Rc<ObjRef>, method: &str, args: Vec<Value>, reply: Option<u64>, call: String) {
        if let Some(r) = self.call_result(&obj, &call) {
            self.reply(reply, r);
            return;
        }
        if let Some(waiting) = self.running_calls.get_mut(&call) {
            waiting.push(reply);
            return;
        }
        self.next_call_reply += 1;
        let internal = CALL_REPLY_BASE + self.next_call_reply;
        self.call_replies.insert(internal, (obj.clone(), call.clone()));
        self.running_calls.insert(call, vec![reply]);
        self.invoke(obj, method, args, true, Some(internal));
    }

    /// Records the result of a call run by [`Runtime::call_once`] and answers everyone waiting.
    pub(crate) fn finish_call(&mut self, internal: u64, r: Result<Json, String>) {
        let Some((obj, call)) = self.call_replies.remove(&internal) else { return };
        self.record_call(&obj, &call, &r);
        for reply in self.running_calls.remove(&call).unwrap_or_default() {
            self.reply(reply, r.clone());
        }
    }
}

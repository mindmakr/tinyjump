//! `@exclusive` methods: actor-style turns on an object, only where they are needed.
//!
//! A method without `await` already runs to the end before anything else happens. Across an
//! `await`, other calls can run on the same object. An exclusive method holds its object from
//! the moment it starts until it returns or throws: other exclusive calls on that object wait
//! their turn, in order. Calls that are not exclusive, reads and rendering never wait.
//!
//! - Calls belong to a chain: a request, timer, alarm or UI event, and everything it runs,
//!   including `invoke` (so `tj:agent` tools). A chain that holds an object enters it again
//!   at once, so an agent calling back into its own object never waits for itself.
//! - Before a call waits, the wait is checked for a cycle: if the holder is itself waiting,
//!   directly or through others, for something this chain holds, the call fails at once
//!   instead of hanging.
//! - At most [`MAX_WAITING`] calls wait per object. A call someone is waiting to hear back
//!   from waits at most [`WAIT_MS`]: a request then answers 503 (a call made once is not
//!   recorded, so its retry runs), and a call already running gets an exception it can catch.
//! - Timers and alarms wait without a limit, and `@every` beats that pile up merge into one.
//!
//! Few objects are held at once and waits are rare, so plain vectors searched in order keep
//! this small (device builds pay for every collection type).

use super::{By, Entry, Runtime, TimerKind};
use crate::vm::interp::Fiber;
use crate::vm::value::{Exc, ObjRef, Value};
use serde_json::{json, Map, Value as Json};
use std::rc::Rc;

pub(crate) const MAX_WAITING: usize = 100;
pub(crate) const WAIT_MS: f64 = 30_000.0;

/// An object: class index and id.
pub(crate) type Key = (u16, Rc<str>);

/// A held object: which chain holds it, how many of that chain's frames are inside its
/// exclusive methods, and who waits, in order. A depth of 0 means the object is reserved for
/// a waiting call that runs next.
pub(crate) struct Lock {
    key: Key,
    chain: u64,
    depth: u32,
    queue: Vec<Waiter>,
}

pub(crate) struct Waiter {
    chain: u64,
    fiber: Fiber,
    reply: Option<u64>,
    /// Requests, timers and alarms wait before their method starts. A call already running
    /// waits in the middle and runs its call instruction again when its turn comes.
    started: bool,
    deadline: Option<f64>,
    /// An `@every` beat: a second beat of the same method merges into the waiting one.
    beat: bool,
}

/// A call whose turn has come, to run once the current one has finished.
pub(crate) struct Ready {
    fiber: Fiber,
    reply: Option<u64>,
    /// For a call that was already running: the object reserved for it.
    reserved: Option<Key>,
}

impl Runtime {
    fn is_exclusive(&self, class: u16, method: &str) -> bool {
        self.any_exclusive && self.program.objects[class as usize].methods.get(method).is_some_and(|m| m.exclusive)
    }

    fn describe(&self, key: &Key) -> String {
        format!("{}/{}", self.program.objects[key.0 as usize].name, key.1)
    }

    fn lock(&self, key: &Key) -> Option<&Lock> {
        self.locks.iter().find(|l| &l.key == key)
    }

    /// Whether `chain` may enter `key` now (`Ok(false)`: it must wait), or why it cannot wait:
    /// a cycle (409) or a full queue (503).
    fn may_enter(&self, key: &Key, chain: u64) -> Result<bool, (u16, String)> {
        let Some(lock) = self.lock(key) else { return Ok(true) };
        if lock.chain == chain {
            return Ok(true);
        }
        // Follow holder -> what it waits for -> its holder ...; reaching this chain is a cycle.
        let mut holder = lock.chain;
        for _ in 0..self.locks.len() {
            let Some(next) = self.locks.iter().find(|l| l.queue.iter().any(|w| w.chain == holder)) else { break };
            if next.chain == chain {
                let object = self.describe(key);
                return Err((409, format!("{object} is held by a call that is waiting for an object this call holds; both would wait forever, so this call fails (enter exclusive objects in the same order, or do not wait on one while holding another)")));
            }
            holder = next.chain;
        }
        if lock.queue.len() >= MAX_WAITING {
            return Err((503, format!("{} has {MAX_WAITING} calls waiting; try again later", self.describe(key))));
        }
        Ok(false)
    }

    fn acquire(&mut self, key: Key, chain: u64) {
        match self.locks.iter_mut().find(|l| l.key == key) {
            Some(lock) => lock.depth += 1,
            None => self.locks.push(Lock { key, chain, depth: 1, queue: Vec::new() }),
        }
    }

    pub(crate) fn release_object(&mut self, o: &ObjRef) {
        self.release(&(o.class, o.id.clone()));
    }

    fn release(&mut self, key: &Key) {
        let Some(lock) = self.locks.iter_mut().find(|l| &l.key == key) else { return };
        lock.depth = lock.depth.saturating_sub(1);
        if lock.depth == 0 {
            self.grant_next(key);
        }
    }

    /// Releases what the frames of `f` above `keep` hold, as they end (return, throw, fail).
    pub(crate) fn release_frames(&mut self, f: &mut Fiber, keep: usize) {
        let mut held = Vec::new();
        for fr in f.frames.iter_mut().skip(keep).filter(|fr| fr.exclusive) {
            fr.exclusive = false;
            if let Some(o) = &fr.this {
                held.push((o.class, o.id.clone()));
            }
        }
        for key in held.iter().rev() {
            self.release(key);
        }
    }

    /// Hands `key` to the first waiter, or frees it.
    fn grant_next(&mut self, key: &Key) {
        let Some(i) = self.locks.iter().position(|l| &l.key == key) else { return };
        if self.locks[i].queue.is_empty() {
            self.locks.swap_remove(i);
            return;
        }
        let lock = &mut self.locks[i];
        let mut w = lock.queue.remove(0);
        lock.chain = w.chain;
        if w.started {
            lock.depth = 0;
        } else {
            lock.depth = 1;
            w.fiber.frames[0].exclusive = true;
        }
        self.ready.push(Ready { fiber: w.fiber, reply: w.reply, reserved: w.started.then(|| key.clone()) });
    }

    fn enqueue(&mut self, key: &Key, w: Waiter) {
        if let Some(at) = w.deadline {
            self.push_timer(at, TimerKind::Expire);
        }
        if let Some(lock) = self.locks.iter_mut().find(|l| &l.key == key) {
            lock.queue.push(w);
        }
    }

    /// Starts a method fiber from a request, timer, alarm or `invoke`: now, or when its object
    /// is free if the method is exclusive.
    pub(crate) fn start(&mut self, mut f: Fiber, reply: Option<u64>, entry: Entry) {
        f.caller = entry.who;
        f.chain = if entry.chain == 0 { self.new_chain() } else { entry.chain };
        self.origin(&mut f, entry.by);
        let key = f.stat.filter(|_| self.any_exclusive).and_then(|(c, m)| {
            let (_, def) = self.program.objects[c as usize].methods.get_index(m as usize)?;
            def.exclusive.then(|| f.frames[0].this.as_ref().map(|o| (o.class, o.id.clone()))).flatten()
        });
        let Some(key) = key else { return self.drive(f, reply) };
        match self.may_enter(&key, f.chain) {
            Ok(true) => {
                self.acquire(key, f.chain);
                f.frames[0].exclusive = true;
                self.drive(f, reply);
            }
            Ok(false) => {
                let beat = entry.by == By::Timer;
                let merged = beat && self.lock(&key).is_some_and(|l| l.queue.iter().any(|w| w.beat && w.fiber.stat == f.stat));
                if !merged {
                    let deadline = reply.map(|_| self.now + WAIT_MS);
                    self.enqueue(&key, Waiter { chain: f.chain, fiber: f, reply, started: false, deadline, beat });
                }
            }
            Err((status, why)) => self.refuse_start(reply, status, why),
        }
    }

    /// Before an object method call: the object to wait for, if the method is exclusive and
    /// another chain holds it. Throws if waiting would never end or the queue is full.
    pub(crate) fn must_wait(&self, f: &Fiber, name: &str, argc: usize) -> Result<Option<Key>, Exc> {
        if !self.any_exclusive {
            return Ok(None);
        }
        let Value::Obj(o) = &f.stack[f.stack.len() - 1 - argc] else { return Ok(None) };
        if self.is_remote(o) || !self.is_exclusive(o.class, name) {
            return Ok(None);
        }
        let key = (o.class, o.id.clone());
        match self.may_enter(&key, f.chain) {
            Ok(true) => Ok(None),
            Ok(false) => Ok(Some(key)),
            Err((_, why)) => Err(Exc::busy(why)),
        }
    }

    /// After a frame for `name` on `o` was pushed: holds `o` if the method is exclusive.
    pub(crate) fn entered(&mut self, f: &mut Fiber, class: u16, id: &Rc<str>, name: &str) {
        if self.is_exclusive(class, name) {
            self.acquire((class, id.clone()), f.chain);
            if let Some(fr) = f.frames.last_mut() {
                fr.exclusive = true;
            }
        }
    }

    /// A running call that must wait for `key` (see [`Runtime::must_wait`]).
    pub(crate) fn park(&mut self, key: Key, f: Fiber, reply: Option<u64>) {
        let deadline = Some(self.now + WAIT_MS);
        self.enqueue(&key, Waiter { chain: f.chain, fiber: f, reply, started: true, deadline, beat: false });
    }

    /// Runs the calls whose turn has come. Whether any ran.
    pub(crate) fn run_ready(&mut self) -> bool {
        let ran = !self.ready.is_empty();
        while !self.ready.is_empty() {
            if !self.touched.is_empty() {
                self.flush();
            }
            let r = self.ready.remove(0);
            let mut f = r.fiber;
            let chain = f.chain;
            let mark = self.outputs.len();
            self.ran_code = true;
            let result = self.run(&mut f);
            self.settle(f, r.reply, result, mark);
            // A call that failed before entering gives its reserved turn to the next.
            if let Some(key) = r.reserved {
                if self.lock(&key).is_some_and(|l| l.chain == chain && l.depth == 0) {
                    self.grant_next(&key);
                }
            }
        }
        ran
    }

    /// Gives up on calls that waited too long.
    pub(crate) fn expire_waiters(&mut self) {
        let now = self.now;
        let mut expired = Vec::new();
        for lock in &mut self.locks {
            let mut i = 0;
            while i < lock.queue.len() {
                if lock.queue[i].deadline.is_some_and(|d| d <= now) {
                    expired.push((lock.key.clone(), lock.queue.remove(i)));
                } else {
                    i += 1;
                }
            }
        }
        for (key, w) in expired {
            let why = format!("{} was busy for {} s with another exclusive call; try again", self.describe(&key), WAIT_MS / 1000.0);
            if w.started {
                if !self.touched.is_empty() {
                    self.flush();
                }
                let mut f = w.fiber;
                let mark = self.outputs.len();
                self.ran_code = true;
                let r = self.unwind(&mut f, Exc::busy(why)).and_then(|()| self.run(&mut f));
                self.settle(f, w.reply, r, mark);
            } else {
                self.refuse_start(w.reply, 503, why);
            }
        }
    }

    /// Whether a call holds the object, or waits to; a lock held by `own_chain` with nobody
    /// waiting does not count (the call asking holds it).
    #[cfg(feature = "server")]
    pub(crate) fn exclusive_busy(&self, class: u16, id: &str, own_chain: Option<u64>) -> bool {
        self.locks.iter().any(|l| l.key.0 == class && &*l.key.1 == id && (Some(l.chain) != own_chain || !l.queue.is_empty()))
    }

    /// Calls waiting, per class, for statistics.
    pub(crate) fn waiting_json(&self) -> Json {
        let mut by_class = Map::new();
        for lock in self.locks.iter().filter(|l| !l.queue.is_empty()) {
            let name = self.program.objects[lock.key.0 as usize].name.to_string();
            let n = by_class.get(&name).and_then(Json::as_u64).unwrap_or(0);
            by_class.insert(name, json!(n + lock.queue.len() as u64));
        }
        Json::Object(by_class)
    }

    pub(crate) fn blocked_failure(&self, f: &Fiber, key: &Key) -> crate::vm::interp::Failure {
        self.failure(f, format!("this calls an exclusive method of {}, which another call holds; top-level code, field defaults and rendering cannot wait", self.describe(key)))
    }
}

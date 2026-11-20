//! The I/O-free runtime: boots a program, takes [`Input`]s with the current time,
//! and returns [`Output`]s for the host to carry out.

pub mod access;
mod calls;
mod objects;
pub mod protocol;
mod render;
mod stats;
pub mod store;

#[cfg(feature = "layout")]
use crate::ui::layout::Layout;
use crate::ui::layout::Measure;
use crate::ui::tree::Tree;
use crate::vm::builtins::GLOBALS;
use crate::vm::interp::{Failure, Fiber, Run};
use crate::vm::program::Program;
use crate::vm::value::{Closure, Exc, ObjRef, Value};
use indexmap::IndexSet;
use access::Who;
use calls::{Outbox, CALL_REPLY_BASE, INVOKE_REPLY_BASE};
use objects::Instance;
use protocol::{Effect, Input, Output};
use serde_json::Value as Json;
use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeSet, BinaryHeap, HashMap};
use std::fmt;
use std::rc::Rc;
use store::Store;

/// Host-chosen settings.
pub struct Options {
    /// Current time, epoch milliseconds.
    pub now: f64,
    /// Seed for `Math.random`, so runs can be replayed.
    pub seed: u64,
    /// Render `App` into patches. Servers usually turn this off.
    pub render: bool,
    /// This runtime is the server: `@server` objects live here instead of being replicas.
    pub server: bool,
    /// Say why a request was refused (`tj dev`). Off on real servers, so a refusal does not
    /// reveal which objects and methods exist.
    pub explain: bool,
    /// Remote requests may not load more objects than this at once (0: no limit).
    pub max_objects: usize,
}

impl Options {
    /// A device or test runtime at time `now`.
    pub fn new(now: f64, seed: u64) -> Options {
        Options { now, seed, render: true, server: false, explain: true, max_objects: 0 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BootError {
    pub message: String,
    pub trace: Vec<String>,
}

impl fmt::Display for BootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        for t in &self.trace {
            write!(f, "\n    {t}")?;
        }
        Ok(())
    }
}

impl From<Failure> for BootError {
    fn from(f: Failure) -> Self {
        BootError { message: f.message, trace: f.trace }
    }
}

/// Work counters for performance tests. Counted, not timed, so they are the same on every run.
#[derive(Default, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
    pub inputs: u64,
    pub renders: u64,
    pub patch_ops: u64,
    /// Bytecode instructions executed.
    pub vm_steps: u64,
}

struct Timer {
    at: f64,
    seq: u64,
    kind: TimerKind,
}

enum TimerKind {
    Every { obj: Rc<ObjRef>, method: Rc<str>, interval: f64 },
    Alarm { obj: Rc<ObjRef>, alarm: u64 },
    /// Retry server calls waiting in the outbox.
    Outbox,
}

/// Why a request was refused, with its HTTP status.
fn refused(status: u16, message: impl Into<String>) -> Result<Json, (u16, String)> {
    Err((status, message.into()))
}

impl PartialEq for Timer {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Timer {}
impl PartialOrd for Timer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Timer {
    fn cmp(&self, other: &Self) -> Ordering {
        self.at.total_cmp(&other.at).then(self.seq.cmp(&other.seq))
    }
}

pub struct Runtime {
    pub(crate) program: Rc<Program>,
    pub(crate) globals: Vec<Value>,
    /// Prelude functions implementing array methods, by method name.
    pub(crate) prelude_array: HashMap<Rc<str>, u32>,
    /// One closure per object method, in declaration order.
    methods: Vec<Vec<Rc<Closure>>>,
    store: Box<dyn Store>,
    instances: HashMap<(u16, Rc<str>), Instance>,
    /// Objects whose state may have changed during the current input.
    touched: IndexSet<(u16, Rc<str>)>,
    /// Keys of objects with pending alarms, so they are loaded at boot.
    alarm_index: BTreeSet<String>,
    timers: BinaryHeap<Reverse<Timer>>,
    seq: u64,
    /// The last change number given to an object state (see `objects.rs`).
    change_seq: u64,
    next_alarm: u64,
    next_effect: u32,
    /// Fibers suspended on an effect, with their reply id.
    waiting: HashMap<u32, (Fiber, Option<u64>)>,
    /// Devices: server calls not yet answered (see `calls.rs`).
    outbox: Outbox,
    /// Calls loaded at boot wait for the first input (see `calls.rs`).
    outbox_held: bool,
    /// `invoke`s waiting for their calling fiber to suspend: promise, object, method, args, who.
    invocations: std::collections::VecDeque<(u32, String, String, Vec<Value>, Who)>,
    /// Who the fiber running now belongs to (see `vm::interp::Runtime::run`).
    pub(crate) running_caller: Who,
    /// Servers: calls with an id that are still running, and who waits for each.
    running_calls: HashMap<String, (objects::CallRecord, Vec<Option<u64>>)>,
    call_replies: HashMap<u64, (Rc<ObjRef>, String)>,
    next_call_reply: u64,
    tree: Tree,
    #[cfg(feature = "layout")]
    layout: Layout,
    /// Native hosts measure their widgets for layout; the web and servers do not need it.
    measure: Option<Box<dyn Measure>>,
    render: bool,
    server: bool,
    explain: bool,
    max_objects: usize,
    ran_code: bool,
    now: f64,
    rng: u64,
    outputs: Vec<Output>,
    pub(crate) metrics: Metrics,
    stats: stats::Table,
}

impl Runtime {
    /// Runs the program's top-level code, restores objects with timers, and renders.
    pub fn boot(program: Program, store: Box<dyn Store>, opts: Options) -> Result<(Runtime, Vec<Output>), BootError> {
        let program = Rc::new(program);
        let mut globals = vec![Value::Null; program.globals.len()];
        for (slot, (_, make)) in globals.iter_mut().zip(GLOBALS) {
            *slot = make();
        }
        let prelude_array = program.globals.iter().enumerate().filter_map(|(i, n)| n.strip_prefix("__arr_").map(|m| (Rc::from(m), i as u32))).collect();
        let methods = program
            .objects
            .iter()
            .map(|o| o.methods.values().map(|m| Rc::new(Closure { proto: m.proto, upvals: Vec::new(), this: None })).collect())
            .collect();
        let mut rt = Runtime {
            program: program.clone(),
            globals,
            prelude_array,
            methods,
            store,
            instances: HashMap::new(),
            touched: IndexSet::new(),
            alarm_index: BTreeSet::new(),
            timers: BinaryHeap::new(),
            seq: 0,
            change_seq: 0,
            next_alarm: 0,
            next_effect: 0,
            waiting: HashMap::new(),
            outbox: Outbox::default(),
            outbox_held: false,
            invocations: std::collections::VecDeque::new(),
            running_caller: Who::System,
            running_calls: HashMap::new(),
            call_replies: HashMap::new(),
            next_call_reply: 0,
            tree: Tree::default(),
            #[cfg(feature = "layout")]
            layout: Layout::default(),
            measure: None,
            render: opts.render,
            server: opts.server,
            explain: opts.explain,
            max_objects: opts.max_objects,
            ran_code: false,
            now: opts.now,
            rng: if opts.seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { opts.seed },
            outputs: Vec::new(),
            metrics: Metrics::default(),
            stats: stats::table(&program),
        };
        for &main in &program.main {
            let closure = Rc::new(Closure { proto: main, upvals: Vec::new(), this: None });
            let f = rt.fiber_for(&Value::Func(closure), Vec::new()).map_err(|e| BootError { message: e.message(), trace: Vec::new() })?;
            rt.run_sync(f)?;
        }
        rt.restore()?;
        if !rt.server {
            rt.load_outbox();
        }
        if rt.render {
            rt.render_app();
        }
        rt.flush();
        let out = rt.drain();
        Ok((rt, out))
    }

    /// Handles one input at time `now` (epoch milliseconds) and returns what the host must do.
    pub fn handle(&mut self, now: f64, input: Input) -> Vec<Output> {
        if now > self.now {
            self.now = now;
        }
        self.metrics.inputs += 1;
        if !matches!(input, Input::Identity { .. }) {
            self.release_outbox();
        }
        match input {
            Input::Call { object, method, args, reply, call, caller } => match self.admit(&object, Some(&method), caller) {
                Err((status, why)) => {
                    if status == 403 {
                        self.count_refused(&object, &method);
                    }
                    self.reply_status(reply, status, why)
                }
                Ok(who) => match (self.resolve_object(&object), call) {
                    (Ok(obj), Some(call)) => self.call_once(obj, &method, &args, reply, call, who),
                    (Ok(obj), None) => {
                        let args = args.iter().map(Value::from_json).collect();
                        self.invoke(obj, &method, args, true, reply, who)
                    }
                    (Err(e), _) => self.reply(reply, Err(e)),
                },
            },
            Input::Get { object, reply, caller } => match self.admit(&object, None, caller) {
                Err((status, why)) => self.reply_status(reply, status, why),
                Ok(_) => {
                    let state = self.resolve_object(&object).and_then(|o| self.object_state(&o));
                    if state.is_ok() {
                        // Reading is an event too: subscribers learn who is looking. It never
                        // changes state, so a replica fetching state cannot start a loop.
                        self.outputs.push(Output::Emit { object: object.clone(), name: "read".into(), data: Json::Null });
                    }
                    self.reply(reply, state);
                }
            },
            Input::Identity { user } => self.set_identity(user),
            Input::Ui { target, event, value, reply } => {
                let handler = self.tree.find(&target).and_then(|node| self.tree.handler(node, &event).cloned());
                match handler {
                    Some(h) => match self.fiber_for(&h, vec![Value::from_json(&value)]) {
                        Ok(mut f) => {
                            f.caller = self.local();
                            self.drive(f, reply)
                        }
                        Err(e) => self.reply(reply, Err(e.message())),
                    },
                    None => self.reply(reply, Err(format!("no {event} handler on {target:?}"))),
                }
            }
            Input::EffectDone { id, ok, error, unreachable } => {
                if self.is_queued_call(id) {
                    self.call_attempt_done(id, ok, error, unreachable);
                } else {
                    self.resume(id, ok, error);
                }
            }
            Input::Sync { object, state, seq } => {
                if let Err(e) = self.sync(&object, &state, seq) {
                    self.reply(None, Err(e));
                }
            }
            // Without the layout engine (the web build) there is nothing to compute.
            #[cfg(not(feature = "layout"))]
            Input::Layout { .. } => {}
            #[cfg(feature = "layout")]
            Input::Layout { width, height, rtl } => {
                if let Some(m) = self.measure.as_deref_mut() {
                    let frames = self.layout.compute(&self.tree, width, height, rtl, m);
                    if !frames.is_empty() {
                        self.outputs.push(Output::Frames { frames });
                    }
                }
            }
            Input::Online => self.retry_calls_now(),
            Input::Tick => {}
            Input::Inspect { reply } => {
                let snapshot = self.inspect();
                self.reply(reply.or(Some(0)), Ok(snapshot));
            }
        }
        self.run_invocations();
        self.fire_timers();
        self.run_invocations();
        if std::mem::take(&mut self.ran_code) && self.render {
            self.render_app();
        }
        self.flush();
        self.drain()
    }

    /// Native hosts: how to measure widgets, which turns on [`Input::Layout`].
    pub fn set_measure(&mut self, measure: Box<dyn Measure>) {
        self.measure = Some(measure);
    }

    /// When the host should next send [`Input::Tick`].
    pub fn next_wake(&self) -> Option<f64> {
        self.timers.peek().map(|Reverse(t)| t.at)
    }

    fn drain(&mut self) -> Vec<Output> {
        let mut out = std::mem::take(&mut self.outputs);
        out.push(Output::Wake { at: self.next_wake() });
        out
    }

    // ---------- host services used by builtins ----------

    pub(crate) fn now(&self) -> f64 {
        self.now
    }

    /// xorshift64*: deterministic for a given seed.
    pub(crate) fn random(&mut self) -> f64 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let x = self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (x >> 11) as f64 / (1u64 << 53) as f64
    }

    pub(crate) fn log(&mut self, level: &str, message: String) {
        self.outputs.push(Output::Log { level: level.into(), message });
    }

    pub(crate) fn plugin_call(&mut self, plugin: String, method: String, args: Json) -> u32 {
        self.next_effect += 1;
        let id = self.next_effect;
        self.outputs.push(Output::Effect { id, effect: Effect::Plugin { plugin, method, args } });
        id
    }

    pub(crate) fn fetch(&mut self, url: String, method: String, headers: Json, body: Option<String>) -> u32 {
        self.next_effect += 1;
        let id = self.next_effect;
        self.outputs.push(Output::Effect { id, effect: Effect::Http { url, method, headers, body } });
        id
    }

    // ---------- fibers ----------

    /// A fiber that calls a function value with arguments.
    fn fiber_for(&self, callee: &Value, args: Vec<Value>) -> Result<Fiber, Exc> {
        let Value::Func(c) = callee else {
            return Err(Exc::msg(format!("expected a function, got a {}", callee.type_name())));
        };
        let mut f = Fiber::default();
        f.stack.push(callee.clone());
        f.stack.extend(args);
        self.push_frame(&mut f, c.clone(), 1, 0, c.this.clone())?;
        Ok(f)
    }

    /// Runs a fiber that must not suspend (top-level code, object init, render).
    fn run_sync(&mut self, mut f: Fiber) -> Result<Value, Failure> {
        match self.run(&mut f)? {
            Run::Done(v) => Ok(v),
            Run::Await(_) => Err(self.failure(&f, "`await` is not allowed here (top-level code, object field defaults and rendering must not wait)".into())),
        }
    }

    /// Who a `call` or `get` comes from, after the access rules and the object cap; or the
    /// refusal. Checked before anything is loaded.
    fn admit(&self, object: &str, method: Option<&str>, caller: Option<protocol::Caller>) -> Result<Who, (u16, String)> {
        let who = match caller {
            Some(c) => Who::from_caller(&c, self.server),
            None if self.server => return refused(400, "a request to a server must say who is calling (caller)").map(|_| Who::System),
            None => Who::Local,
        };
        if let Some(why) = self.refusal(object, method, &who) {
            return refused(403, why).map(|_| Who::System);
        }
        if matches!(who, Who::Remote { .. }) && self.max_objects > 0 && self.instances.len() >= self.max_objects && !self.is_loaded(object) {
            return refused(503, "too many objects are loaded; try again later").map(|_| Who::System);
        }
        Ok(who)
    }

    /// Who runs code that no request started: the system on a server, the user on a device.
    fn local(&self) -> Who {
        if self.server {
            Who::System
        } else {
            Who::Local
        }
    }

    fn invoke(&mut self, obj: Rc<ObjRef>, method: &str, args: Vec<Value>, external: bool, reply: Option<u64>, who: Who) {
        match self.method_fiber(obj, method, args, external) {
            Ok(mut f) => {
                f.caller = who;
                self.drive(f, reply)
            }
            Err(e) => self.reply(reply, Err(e.message())),
        }
    }

    fn drive(&mut self, mut f: Fiber, reply: Option<u64>) {
        self.ran_code = true;
        let r = self.run(&mut f);
        self.settle(f, reply, r);
    }

    fn settle(&mut self, f: Fiber, reply: Option<u64>, r: Result<Run, Failure>) {
        match r {
            Ok(Run::Await(p)) => {
                self.waiting.insert(p, (f, reply));
            }
            Ok(Run::Done(v)) => {
                if reply.is_some() {
                    self.reply(reply, v.to_json());
                }
            }
            Err(fail) => {
                self.count_error(f.stat);
                match reply {
                    Some(_) => self.reply(reply, Err(fail.message)),
                    None => self.outputs.push(Output::Error { message: fail.message, trace: fail.trace }),
                }
            }
        }
    }

    /// Continues the fiber waiting on promise `id` with an effect's result.
    fn resume(&mut self, id: u32, ok: Option<Json>, error: Option<String>) {
        if let Some((mut f, reply)) = self.waiting.remove(&id) {
            self.ran_code = true;
            let r = match error {
                Some(e) => self.unwind(&mut f, Exc::msg(e)).and_then(|()| self.run(&mut f)),
                None => {
                    f.stack.push(Value::from_json(&ok.unwrap_or(Json::Null)));
                    self.run(&mut f)
                }
            };
            self.settle(f, reply, r);
        }
    }

    /// Refuses a request with an HTTP-like status (see `Output::Reply`).
    fn reply_status(&mut self, reply: Option<u64>, status: u16, why: String) {
        match reply {
            Some(id) if id >= INVOKE_REPLY_BASE => self.reply(reply, Err(why)),
            Some(id) => self.outputs.push(Output::Reply { reply: id, ok: None, error: Some(why), status: Some(status) }),
            None => self.outputs.push(Output::Error { message: why, trace: Vec::new() }),
        }
    }

    /// Answers a request; without a reply id, errors are reported and results dropped.
    fn reply(&mut self, reply: Option<u64>, r: Result<Json, String>) {
        if let Some(internal) = reply.filter(|&id| id >= CALL_REPLY_BASE) {
            self.finish_call(internal, r);
            return;
        }
        if let Some(internal) = reply.filter(|&id| id >= INVOKE_REPLY_BASE) {
            let (ok, error) = match r {
                Ok(v) => (Some(v), None),
                Err(e) => (None, Some(e)),
            };
            self.resume((internal - INVOKE_REPLY_BASE) as u32, ok, error);
            return;
        }
        match (reply, r) {
            (Some(id), Ok(j)) => self.outputs.push(Output::Reply { reply: id, ok: Some(j), error: None, status: None }),
            (Some(id), Err(e)) => self.outputs.push(Output::Reply { reply: id, ok: None, error: Some(e), status: None }),
            (None, Err(e)) => self.outputs.push(Output::Error { message: e, trace: Vec::new() }),
            (None, Ok(_)) => {}
        }
    }

    // ---------- timers ----------

    fn push_timer(&mut self, at: f64, kind: TimerKind) {
        self.seq += 1;
        self.timers.push(Reverse(Timer { at, seq: self.seq, kind }));
    }

    fn fire_timers(&mut self) {
        while self.timers.peek().is_some_and(|Reverse(t)| t.at <= self.now) {
            let Some(Reverse(t)) = self.timers.pop() else { break };
            match t.kind {
                TimerKind::Every { obj, method, interval } => {
                    // Skip missed beats instead of firing a burst after a pause.
                    let mut next = t.at + interval;
                    if next <= self.now {
                        next = self.now + interval;
                    }
                    self.push_timer(next, TimerKind::Every { obj: obj.clone(), method: method.clone(), interval });
                    self.invoke(obj, &method, Vec::new(), false, None, Who::System);
                }
                TimerKind::Alarm { obj, alarm } => {
                    if let Some(a) = self.take_alarm(&obj, alarm) {
                        let args = a.args.iter().map(Value::from_json).collect();
                        // An alarm runs as whoever scheduled it, so rules on data still hold.
                        let who = a.caller.as_ref().map_or(Who::System, |c| Who::from_caller(c, self.server));
                        self.invoke(obj, &a.method, args, false, None, who);
                    }
                }
                TimerKind::Outbox => self.dispatch_calls(),
            }
        }
    }
}

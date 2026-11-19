//! The I/O-free runtime: boots a program, takes [`Input`]s with the current time,
//! and returns [`Output`]s for the host to carry out.

mod objects;
pub mod protocol;
mod render;
pub mod store;

use crate::ui::tree::Tree;
use crate::vm::builtins::GLOBALS;
use crate::vm::interp::{Failure, Fiber, Run};
use crate::vm::program::Program;
use crate::vm::value::{Closure, Exc, ObjRef, Value};
use indexmap::IndexSet;
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

struct Timer {
    at: f64,
    seq: u64,
    kind: TimerKind,
}

enum TimerKind {
    Every { obj: Rc<ObjRef>, method: Rc<str>, interval: f64 },
    Alarm { obj: Rc<ObjRef>, alarm: u64 },
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
    next_alarm: u64,
    next_effect: u32,
    /// Fibers suspended on an effect, with their reply id.
    waiting: HashMap<u32, (Fiber, Option<u64>)>,
    tree: Tree,
    render: bool,
    server: bool,
    ran_code: bool,
    now: f64,
    rng: u64,
    outputs: Vec<Output>,
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
            next_alarm: 0,
            next_effect: 0,
            waiting: HashMap::new(),
            tree: Tree::default(),
            render: opts.render,
            server: opts.server,
            ran_code: false,
            now: opts.now,
            rng: if opts.seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { opts.seed },
            outputs: Vec::new(),
        };
        for &main in &program.main {
            let closure = Rc::new(Closure { proto: main, upvals: Vec::new(), this: None });
            let f = rt.fiber_for(&Value::Func(closure), Vec::new()).map_err(|e| BootError { message: e.message(), trace: Vec::new() })?;
            rt.run_sync(f)?;
        }
        rt.restore()?;
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
        match input {
            Input::Call { object, method, args, reply } => {
                let args = args.iter().map(Value::from_json).collect();
                match self.resolve_object(&object) {
                    Ok(obj) => self.invoke(obj, &method, args, true, reply),
                    Err(e) => self.reply(reply, Err(e)),
                }
            }
            Input::Get { object, reply } => {
                let state = self.resolve_object(&object).and_then(|o| self.object_state(&o));
                self.reply(reply, state);
            }
            Input::Ui { target, event, value, reply } => {
                let handler = self.tree.find(&target).and_then(|node| self.tree.handler(node, &event).cloned());
                match handler {
                    Some(h) => match self.fiber_for(&h, vec![Value::from_json(&value)]) {
                        Ok(f) => self.drive(f, reply),
                        Err(e) => self.reply(reply, Err(e.message())),
                    },
                    None => self.reply(reply, Err(format!("no {event} handler on {target:?}"))),
                }
            }
            Input::EffectDone { id, ok, error } => {
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
            Input::Sync { object, state } => {
                if let Err(e) = self.sync(&object, &state) {
                    self.reply(None, Err(e));
                }
            }
            Input::Tick => {}
            Input::Inspect { reply } => {
                let snapshot = self.inspect();
                self.reply(reply.or(Some(0)), Ok(snapshot));
            }
        }
        self.fire_timers();
        if std::mem::take(&mut self.ran_code) && self.render {
            self.render_app();
        }
        self.flush();
        self.drain()
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

    fn invoke(&mut self, obj: Rc<ObjRef>, method: &str, args: Vec<Value>, external: bool, reply: Option<u64>) {
        match self.method_fiber(obj, method, args, external) {
            Ok(f) => self.drive(f, reply),
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
            Err(fail) => match reply {
                Some(_) => self.reply(reply, Err(fail.message)),
                None => self.outputs.push(Output::Error { message: fail.message, trace: fail.trace }),
            },
        }
    }

    /// Answers a request; without a reply id, errors are reported and results dropped.
    fn reply(&mut self, reply: Option<u64>, r: Result<Json, String>) {
        match (reply, r) {
            (Some(id), Ok(j)) => self.outputs.push(Output::Reply { reply: id, ok: Some(j), error: None }),
            (Some(id), Err(e)) => self.outputs.push(Output::Reply { reply: id, ok: None, error: Some(e) }),
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
                    self.invoke(obj, &method, Vec::new(), false, None);
                }
                TimerKind::Alarm { obj, alarm } => {
                    if let Some(a) = self.take_alarm(&obj, alarm) {
                        let args = a.args.iter().map(Value::from_json).collect();
                        self.invoke(obj, &a.method, args, false, None);
                    }
                }
            }
        }
    }
}

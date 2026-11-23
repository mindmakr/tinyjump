//! The I/O-free runtime: boots a program, takes [`Input`]s with the current time,
//! and returns [`Output`]s for the host to carry out.

pub mod access;
mod calls;
pub(crate) mod exclusive;
mod objects;
pub mod protocol;
mod render;
mod privacy;
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
    /// Maximum serialized byte size of a single stored object record (0: no limit).
    pub max_object_bytes: usize,
    /// On a server: `@server` class names owned by another server (`remotes.json`). Instances of
    /// these classes behave as replicas (synced via `Input::Sync`, calls queued in `tj:outbox`
    /// and emitted as `Effect::Call` with `Idempotency-Key`).
    #[cfg(feature = "server")]
    pub remote_classes: Vec<String>,
}

impl Options {
    /// A device or test runtime at time `now`.
    pub fn new(now: f64, seed: u64) -> Options {
        Options {
            now,
            seed,
            render: true,
            server: false,
            explain: true,
            max_objects: 0,
            max_object_bytes: 0,
            #[cfg(feature = "server")]
            remote_classes: Vec::new(),
        }
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
    /// Give up on calls that waited too long for an exclusive object.
    Expire,
    /// The next step of the retention sweep (`privacy.rs`).
    #[cfg(feature = "server")]
    Retain,
    /// The start of the next minute, for a screen that shows the time.
    Clock,
}

/// Snapshot of output, invocation, and device-outbox queue lengths at the start of a turn,
/// so an aborted turn rolls back every side effect it queued.
#[derive(Clone, Copy)]
pub(crate) struct TurnMark {
    pub(crate) out: usize,
    pub(crate) inv: usize,
    pub(crate) outbox: usize,
}

/// How a method call starts: who it is for, its chain (0: a new one, see `exclusive.rs`),
/// and what started it.
pub(crate) struct Entry {
    pub who: Who,
    pub chain: u64,
    pub by: By,
    #[cfg(feature = "server")]
    pub call_id: Option<Rc<str>>,
}

impl Entry {
    pub(crate) fn new(who: Who) -> Entry {
        Entry {
            who,
            chain: 0,
            by: By::Request,
            #[cfg(feature = "server")]
            call_id: None,
        }
    }
}

/// What started a method call. A server reports calls nobody requested ([`Output::Ran`]),
/// for its audit log; requests are logged where they arrive.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum By {
    Request,
    /// An `@every` beat; a beat that finds one waiting merges into it.
    Timer,
    Alarm,
    Invoke,
}

/// A server's record of a call nobody requested, kept on its fiber until the call ends.
pub struct Origin {
    by: &'static str,
    obj: Rc<ObjRef>,
    at: f64,
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
    /// `invoke`s waiting for their calling fiber to suspend.
    invocations: std::collections::VecDeque<calls::Invocation>,
    /// Who the fiber running now belongs to, and its chain (see `vm::interp::Runtime::run`).
    pub(crate) running_caller: Who,
    /// On a device: server classes whose whole stream `Class.all()` asked for.
    pub(crate) class_streams: std::collections::HashSet<u16>,
    /// On a device, the signed-in user's roles (`Input::Identity`), for `caller.roles` in UI
    /// code.
    pub(crate) device_roles: Rc<[Rc<str>]>,
    pub(crate) running_chain: u64,
    #[cfg(feature = "server")]
    pub(crate) running_idempotency: Option<crate::vm::interp::IdempotencyCtx>,
    #[cfg(feature = "server")]
    pub(crate) running_this: Option<Rc<ObjRef>>,
    next_chain: u64,
    /// `@exclusive` (see `exclusive.rs`): held objects and calls whose turn has come.
    locks: Vec<exclusive::Lock>,
    ready: Vec<exclusive::Ready>,
    any_exclusive: bool,
    /// Servers: calls with an id that are still running, and who waits for each.
    running_calls: HashMap<String, (objects::CallRecord, Vec<Option<u64>>)>,
    /// Calls run once (`call_once`), by their internal reply: the object, the call id, and
    /// whether to keep the result on the object (not when the call erased it).
    pub(crate) call_replies: HashMap<u64, (Rc<ObjRef>, String, bool)>,
    next_call_reply: u64,
    tree: Tree,
    #[cfg(feature = "layout")]
    layout: Layout,
    /// Native hosts measure their widgets for layout; the web and servers do not need it.
    measure: Option<Box<dyn Measure>>,
    render: bool,
    pub(crate) server: bool,
    #[cfg(feature = "server")]
    pub(crate) remote_classes: std::collections::HashSet<u16>,
    explain: bool,
    max_objects: usize,
    #[cfg(feature = "server")]
    max_object_bytes: usize,
    ran_code: bool,
    /// Set by `Date.now()`: a screen that read it is drawn again each minute.
    pub(crate) read_clock: bool,
    /// When the pending `Clock` timer fires.
    clock: Option<f64>,
    now: f64,
    rng: u64,
    /// Behind `crypto.randomId()` and device ids; unguessable once `strong`.
    drbg: crate::crypto::Drbg,
    /// Whether the host has sent secure random bytes (`entropy`).
    strong: bool,
    /// Plain-HTTP origins `fetch` has already warned about.
    plain_http: BTreeSet<String>,
    /// Objects the retention sweep still has to look at in this round.
    #[cfg(feature = "server")]
    retain_queue: Vec<String>,
    /// Erasures waiting for a legal hold to end: object -> when (epoch ms). Saved.
    #[cfg(feature = "server")]
    erasures: std::collections::BTreeMap<String, f64>,
    /// The secret behind `caller_tag`, once read or made.
    secret: Option<Rc<str>>,
    /// Alarms already fired and consumed during the current turn: kept removed even if the
    /// alarm handler throws or fails an `@invariant`, so a failing alarm never re-fires forever.
    pub(crate) consumed_alarms: Vec<((u16, Rc<str>), u64)>,
    /// True while evaluating an `@invariant` method (forbids state mutation and I/O).
    pub(crate) in_invariant: bool,
    /// Active BCP-47 locale tag for `t()` translations (default `"en"`, or `"pseudo"`).
    pub(crate) locale: Rc<str>,
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
        #[cfg(feature = "server")]
        let mut remote_classes = std::collections::HashSet::new();
        #[cfg(feature = "server")]
        if opts.server {
            for name in &opts.remote_classes {
                let Some(c) = program.object(name) else {
                    return Err(BootError { message: format!("remotes.json names {name:?}, which is not declared in this app"), trace: Vec::new() });
                };
                if !program.objects[c as usize].server {
                    return Err(BootError { message: format!("remotes.json names {name:?}, which must be declared with @server"), trace: Vec::new() });
                }
                remote_classes.insert(c);
            }
        }
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
            class_streams: std::collections::HashSet::new(),
            device_roles: Rc::from([]),
            running_chain: 0,
            #[cfg(feature = "server")]
            running_idempotency: None,
            #[cfg(feature = "server")]
            running_this: None,
            next_chain: 0,
            locks: Vec::new(),
            ready: Vec::new(),
            any_exclusive: program.objects.iter().any(|o| o.methods.values().any(|m| m.exclusive)),
            running_calls: HashMap::new(),
            call_replies: HashMap::new(),
            next_call_reply: 0,
            tree: Tree::default(),
            #[cfg(feature = "layout")]
            layout: Layout::default(),
            measure: None,
            render: opts.render,
            server: opts.server,
            #[cfg(feature = "server")]
            remote_classes,
            explain: opts.explain,
            max_objects: opts.max_objects,
            #[cfg(feature = "server")]
            max_object_bytes: opts.max_object_bytes,
            ran_code: false,
            read_clock: false,
            clock: None,
            now: opts.now,
            rng: if opts.seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { opts.seed },
            drbg: crate::crypto::Drbg::new(&[opts.seed.to_le_bytes(), opts.now.to_bits().to_le_bytes()].concat()),
            strong: false,
            plain_http: BTreeSet::new(),
            #[cfg(feature = "server")]
            retain_queue: Vec::new(),
            #[cfg(feature = "server")]
            erasures: std::collections::BTreeMap::new(),
            secret: None,
            consumed_alarms: Vec::new(),
            in_invariant: false,
            locale: Rc::from("en"),
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
        #[cfg(feature = "server")]
        rt.start_retention().map_err(|message| BootError { message, trace: Vec::new() })?;
        if !rt.server {
            rt.load_outbox();
        }
        #[cfg(feature = "server")]
        if rt.server && !rt.remote_classes.is_empty() {
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
        // Held calls wait for the identity; entropy comes before it on every host.
        if !matches!(input, Input::Identity { .. } | Input::Entropy { .. }) {
            self.release_outbox();
        }
        match input {
            Input::Call { object, method, args, reply, call, caller } => {
                #[cfg(feature = "server")]
                let remote_target = self.is_remote_key(&object);
                #[cfg(not(feature = "server"))]
                let remote_target = false;
                if remote_target {
                    self.reply_status(reply, 400, format!("{object} lives on another server; call it there"));
                } else {
                    match self.admit(&object, Some(&method), caller) {
                        Err((status, why)) => {
                            if status == 403 {
                                self.count_refused(&object, &method);
                            }
                            self.reply_status(reply, status, why)
                        }
                        // A call from outside gives JSON: it must fit the method's declared types, or it
                        // is refused before the method runs, naming the parameter.
                        Ok(who) => match (matches!(who, Who::Remote { .. }).then(|| self.bad_args(&object, &method, &args)).flatten(), self.resolve_object(&object), call) {
                            (Some(why), _, _) => self.reply_status(reply, 400, why),
                            (None, Ok(obj), Some(call)) => self.call_once(obj, &method, &args, reply, call, who),
                            (None, Ok(obj), None) => {
                                let args = args.iter().map(Value::from_json).collect();
                                self.invoke(obj, &method, args, true, reply, Entry::new(who))
                            }
                            // The rules already admitted the class, so this is the store failing.
                            (None, Err(e), _) => self.reply_status(reply, 503, e),
                        },
                    }
                }
            }
            Input::Get { object, reply, caller } => {
                #[cfg(feature = "server")]
                let remote_target = self.is_remote_key(&object);
                #[cfg(not(feature = "server"))]
                let remote_target = false;
                if remote_target {
                    self.reply_status(reply, 400, format!("{object} lives on another server; read it there"));
                } else {
                    match self.admit(&object, None, caller) {
                        Err((status, why)) => self.reply_status(reply, status, why),
                        Ok(who) => {
                            // Each field reaches the readers its rule names; tools see personal data masked.
                            let state = self.resolve_object(&object).and_then(|o| {
                                let mut s = self.object_state(&o)?;
                                let hidden = self.hidden(o.class, &o.id, &s, &who);
                                self.hide(o.class, &mut s, &hidden);
                                Ok(s)
                            });
                            match state {
                                Ok(s) => {
                                    // Reading is an event too: subscribers learn who is looking. It never
                                    // changes state, so a replica fetching state cannot start a loop.
                                    self.outputs.push(Output::Emit { object: object.clone(), name: "read".into(), data: Json::Null });
                                    self.reply(reply, Ok(s));
                                }
                                // The rules admitted it, so this is the store failing.
                                Err(e) => self.reply_status(reply, 503, e),
                            }
                        }
                    }
                }
            }
            Input::Identity { user, roles } => self.set_identity(user, roles),
            Input::Entropy { bytes } => match crate::crypto::unhex(&bytes).filter(|b| b.len() >= 32) {
                Some(b) => {
                    self.drbg.reseed(&b);
                    self.strong = true;
                    #[cfg(feature = "server")]
                    if self.server && !self.remote_classes.is_empty() {
                        self.release_outbox();
                    }
                }
                None => self.reply(None, Err(("entropy needs at least 32 bytes, as hex".into(), None))),
            },
            Input::Locale { locale } => {
                let next = locale.trim();
                let next = if next.is_empty() { "en" } else { next };
                if &*self.locale != next {
                    self.locale = Rc::from(next);
                    #[cfg(feature = "layout")]
                    self.layout.clear_cache();
                    if self.render {
                        self.render_app();
                    }
                }
            }
            Input::Ui { target, event, value, reply } => {
                let handler = self.tree.find(&target).and_then(|node| self.tree.handler(node, &event).cloned());
                match handler {
                    Some(h) => match self.fiber_for(&h, vec![Value::from_json(&value)]) {
                        Ok(mut f) => {
                            f.caller = self.local();
                            self.ran_code = true;
                            // A press is answered when its handler has started its work, as for a
                            // person: once it returns a promise it does not await (`onPress={() =>
                            // cart.add(x)}`) or first waits (for a server that may be offline). What
                            // happens after is on the screen, and errors are reported as usual.
                            let mark = self.turn_mark();
                            match self.run(&mut f) {
                                Ok(Run::Done(Value::Promise(_))) => {
                                    self.settle(f, reply, Ok(Run::Done(Value::Null)), mark);
                                }
                                Ok(Run::Await(p)) => {
                                    match self.settle(f, None, Ok(Run::Await(p)), mark) {
                                        None => self.reply(reply, Ok(Json::Null)),
                                        Some(status) if reply.is_some() => {
                                            // Constraint failed before suspending; replace the trailing Output::Error with Output::Reply.
                                            let msg = match self.outputs.last() {
                                                Some(Output::Error { .. }) => match self.outputs.pop() {
                                                    Some(Output::Error { message, .. }) => message,
                                                    _ => unreachable!(),
                                                },
                                                _ => "turn constraint failed".into(),
                                            };
                                            self.reply_status(reply, status, msg);
                                        }
                                        Some(_) => {}
                                    }
                                }
                                r => {
                                    self.settle(f, reply, r, mark);
                                }
                            }
                        }
                        Err(e) => self.reply(reply, Err((e.message(), e.field()))),
                    },
                    None => self.reply(reply, Err((format!("no {event} handler on {target:?}"), None))),
                }
            }
            Input::EffectDone { id, ok, error, field, unreachable } => {
                if self.is_queued_call(id) {
                    self.call_attempt_done(id, ok, error, field, unreachable);
                } else {
                    self.resume(id, ok, error, field, unreachable);
                }
            }
            Input::Sync { object, state, seq } => {
                if let Err(e) = self.sync(&object, &state, seq) {
                    self.reply(None, Err((e, None)));
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
        self.run_queued();
        self.fire_timers();
        self.run_queued();
        self.flush();
        if std::mem::take(&mut self.ran_code) && self.render {
            self.render_app();
        }
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

    /// Bytes no one can guess, once the host has sent `entropy`.
    pub(crate) fn secure_bytes(&mut self, out: &mut [u8]) -> Result<(), Exc> {
        if !self.strong {
            return Err(Exc::msg("this host has not given the runtime secure random bytes: hosts send an `entropy` input right after boot (docs/host-protocol.md)"));
        }
        self.drbg.fill(out);
        Ok(())
    }

    /// xorshift64*: deterministic for a given seed.
    pub(crate) fn random(&mut self) -> f64 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let x = self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (x >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Derives a deterministic, crash-stable external idempotency key for an `@server` method
    /// (`docs/retried-effects.md`), failing closed on store secret errors or duplicate labels in
    /// the same live call.
    #[cfg(feature = "server")]
    pub(crate) fn idempotency_key(&mut self, label: &str) -> Result<String, Exc> {
        if !self.server {
            return Err(Exc::msg("crypto.idempotencyKey() is only available on @server objects"));
        }
        if self.in_invariant {
            return Err(Exc::msg("@invariant methods cannot call crypto.idempotencyKey()"));
        }
        if label.is_empty() || label.len() > 128 {
            return Err(Exc::msg("crypto.idempotencyKey(label) needs a label of 1-128 characters"));
        }
        let Some(mut ctx) = self.running_idempotency.take() else {
            return Err(Exc::msg("crypto.idempotencyKey() must be called inside an @server object method"));
        };
        let res = (|| -> Result<String, Exc> {
            let active_object = self.running_this.as_ref().map(|o| o.key()).unwrap_or_else(|| ctx.object.to_string());
            let label_key = (active_object.clone(), label.to_string());
            if ctx.used_labels.contains(&label_key) {
                return Err(Exc::msg(format!(
                    "crypto.idempotencyKey({label:?}) was already called in this call; include a unique id in the label (e.g. crypto.idempotencyKey(\"{label}:\" + item.id))"
                )));
            }
            let secret = self.store_secret().map_err(Exc::busy)?;
            let who = self.running_caller.clone();
            let caller_tag = self.caller_tag(&who).map_err(Exc::busy)?;
            let call_scope = match &ctx.call_id {
                Some(id) => format!("call:{id}"),
                None => {
                    let root = *ctx.fallback_root.get_or_insert_with(|| {
                        let mut b = [0u8; 16];
                        self.drbg.fill(&mut b);
                        b
                    });
                    format!("turn:{}", crate::crypto::hex(&root))
                }
            };
            let parts: &[&[u8]] = &[b"tj-idem-v1", ctx.object.as_bytes(), active_object.as_bytes(), ctx.method.as_bytes(), caller_tag.as_bytes(), call_scope.as_bytes(), label.as_bytes()];
            let mut framed = Vec::with_capacity(parts.iter().map(|p| 4 + p.len()).sum());
            for p in parts {
                framed.extend_from_slice(&(p.len() as u32).to_be_bytes());
                framed.extend_from_slice(p);
            }
            let mac = crate::crypto::hmac(secret.as_bytes(), &[&framed]);
            ctx.used_labels.insert(label_key);
            Ok(format!("tj_{}", &crate::crypto::hex(&mac)[..32]))
        })();
        self.running_idempotency = Some(ctx);
        res
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
        // Anyone on the way can read or change plain HTTP. Warn once per origin; this machine
        // and the Android emulator's view of it are fine.
        if let Some(rest) = url.strip_prefix("http://") {
            let origin = rest.split(['/', '?', '#']).next().unwrap_or("");
            let host = match origin.find(']') {
                Some(end) if origin.starts_with('[') => &origin[..=end],
                _ => origin.split(':').next().unwrap_or(""),
            };
            let local = matches!(host, "localhost" | "[::1]" | "10.0.2.2") || host.starts_with("127.");
            if !local && self.plain_http.insert(origin.to_string()) {
                self.log("warn", format!("fetch uses plain HTTP to {origin}: anyone on the way can read and change it; use https://"));
            }
        }
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
        let r = match self.run(&mut f) {
            Ok(Run::Done(v)) => return Ok(v),
            Ok(Run::Await(_)) => Err(self.failure(&f, "`await` is not allowed here (top-level code, object field defaults and rendering must not wait)".into())),
            Ok(Run::Blocked(key)) => Err(self.blocked_failure(&f, &key)),
            Err(e) => Err(e),
        };
        self.release_frames(&mut f, 0);
        r
    }

    pub(crate) fn new_chain(&mut self) -> u64 {
        self.next_chain += 1;
        self.next_chain
    }

    /// Runs `invoke`s and calls whose turn has come, until neither is left.
    fn run_queued(&mut self) {
        while self.run_invocations() | self.run_ready() {}
    }

    /// Who a `call` or `get` comes from, after the access rules and the object cap; or the
    /// refusal. Checked before anything is loaded.
    fn admit(&self, object: &str, method: Option<&str>, caller: Option<protocol::Caller>) -> Result<Who, (u16, String)> {
        let who = match caller.map(|c| Who::from_caller(&c, self.server)) {
            Some(Who::Local { .. }) | None if !self.server => self.local(),
            Some(who) => who,
            None => return refused(400, "a request to a server must say who is calling (caller)").map(|_| Who::System),
        };
        if let Some(why) = self.refusal(object, method, &who) {
            return refused(403, why).map(|_| Who::System);
        }
        if matches!(who, Who::Remote { .. }) && self.max_objects > 0 && self.instances.len() >= self.max_objects && !self.is_loaded(object) {
            return refused(503, "too many objects are loaded; try again later").map(|_| Who::System);
        }
        Ok(who)
    }

    /// Why `args` do not fit `Class.method`'s declared parameters, if they do not.
    pub(crate) fn bad_args(&self, object: &str, method: &str, args: &[Json]) -> Option<String> {
        let class = object.split_once('/').and_then(|(c, _)| self.program.object(c))?;
        let m = self.program.objects[class as usize].methods.get(method)?;
        let def = &self.program.objects[class as usize];
        crate::vm::program::check_args(&m.params, args).err().map(|e| format!("{}.{method}: {e}", def.name))
    }

    /// Who runs code that no request started: the system on a server, the signed-in user on
    /// a device.
    fn local(&self) -> Who {
        if self.server {
            Who::System
        } else {
            Who::Local { id: Rc::from(self.outbox.user.as_deref().unwrap_or_default()), roles: self.device_roles.clone() }
        }
    }

    fn invoke(&mut self, obj: Rc<ObjRef>, method: &str, args: Vec<Value>, external: bool, reply: Option<u64>, entry: Entry) {
        match self.method_fiber(obj, method, args, external) {
            Ok(f) => self.start(f, reply, entry),
            Err(e) => self.reply(reply, Err((e.message(), e.field()))),
        }
    }

    pub(crate) fn turn_mark(&self) -> TurnMark {
        TurnMark { out: self.outputs.len(), inv: self.invocations.len(), outbox: self.outbox_len() }
    }

    fn drive(&mut self, mut f: Fiber, reply: Option<u64>) {
        let mark = self.turn_mark();
        self.ran_code = true;
        let r = self.run(&mut f);
        self.settle(f, reply, r, mark);
    }

    #[inline(never)]
    fn rollback_queued(&mut self, mark: TurnMark) {
        self.rollback_turn(mark.out);
        self.invocations.truncate(mark.inv);
        self.rollback_outbox(mark.outbox);
    }

    #[inline(never)]
    fn abort_turn(&mut self, f: &mut Fiber, mark: TurnMark) {
        self.ran(f, false);
        self.release_frames(f, 0);
        self.rollback_queued(mark);
        self.count_error(f.stat);
    }

    /// Completes or suspends a fiber, enforcing `@invariant`s, per-object size caps, and atomic
    /// rollback per turn BEFORE committing `waiting`, `park`, `ran`, or `reply`.
    /// Returns `None` when the turn committed or suspended cleanly, or `Some(status)` when rolled back.
    pub(crate) fn settle(&mut self, mut f: Fiber, reply: Option<u64>, r: Result<Run, Failure>, mark: TurnMark) -> Option<u16> {
        let ok = match r {
            Ok(ok) => ok,
            Err(fail) => {
                self.ran(&mut f, false);
                self.release_frames(&mut f, 0);
                // Atomic turns: an uncaught error rolls back every object, invocation, and
                // outbox call from this turn before recording the error reply.
                self.rollback_queued(mark);
                match fail.status {
                    Some(403) => self.count_refused_slot(f.stat),
                    _ => self.count_error(f.stat),
                }
                #[cfg(feature = "server")]
                let effective_status = if fail.status.is_none() && fail.fault && self.server { Some(500) } else { fail.status };
                #[cfg(not(feature = "server"))]
                let effective_status = fail.status;
                let status = effective_status.unwrap_or(400);
                match (reply, effective_status) {
                    // Through `refuse_start`, so a call run once (a device's) keeps the status
                    // and no record of the refusal or server fault is cached for it.
                    (Some(_), Some(status)) => self.refuse_start(reply, status, fail.message),
                    (Some(_), None) => self.reply(reply, Err((fail.message, fail.field))),
                    (None, _) => self.outputs.push(Output::Error { message: fail.message, trace: fail.trace }),
                }
                self.flush_from(mark.out);
                return Some(status);
            }
        };
        if let Err((status, msg)) = self.check_turn_constraints() {
            self.abort_turn(&mut f, mark);
            self.reply_constraint_err(reply, status, msg);
            self.flush_from(mark.out);
            return Some(status);
        }
        match ok {
            Run::Done(v) => {
                self.ran(&mut f, true);
                if reply.is_some() {
                    self.reply(reply, v.to_json().map_err(|e| (e, None)));
                }
                self.flush_from(mark.out);
                None
            }
            Run::Await(_) | Run::Blocked(_) if !self.flush_from(mark.out) => {
                self.abort_turn(&mut f, mark);
                if reply.is_some() {
                    #[cfg(feature = "server")]
                    {
                        let mut i = 0;
                        self.outputs.retain(|o| {
                            let keep = i < mark.out || !matches!(o, Output::Error { .. });
                            i += 1;
                            keep
                        });
                    }
                    self.refuse_start(reply, 503, "the change could not be saved; try again".into());
                }
                Some(503)
            }
            Run::Await(p) => {
                self.waiting.insert(p, (f, reply));
                None
            }
            Run::Blocked(key) => {
                self.park(key, f, reply);
                None
            }
        }
    }

    fn check_turn_constraints(&mut self) -> Result<(), (u16, String)> {
        self.check_touched_invariants().map_err(|m| (422, m))?;
        #[cfg(feature = "server")]
        self.check_touched_object_sizes().map_err(|m| (413, m))?;
        Ok(())
    }

    fn reply_constraint_err(&mut self, reply: Option<u64>, status: u16, msg: String) {
        if status == 413 {
            // Do not persist a `CallRecord` on an object that just exceeded its size cap.
            self.refuse_start(reply, 413, msg);
            return;
        }
        match reply {
            Some(id) if id >= CALL_REPLY_BASE => self.finish_call(id, Err((msg, None)), Some(status)),
            Some(id) => self.reply_status(Some(id), status, msg),
            None => self.outputs.push(Output::Error { message: msg, trace: Vec::new() }),
        }
    }

    /// Remembers on a server what started a call nobody requested, to report when it ends.
    fn origin(&self, f: &mut Fiber, by: By) {
        let by = match by {
            _ if !self.server => return,
            By::Request => return,
            By::Timer => "timer",
            By::Alarm => "alarm",
            By::Invoke => "invoke",
        };
        if let Some(obj) = f.frames[0].this.clone() {
            f.origin = Some(Origin { by, obj, at: self.now });
        }
    }

    /// Reports a call nobody requested as it ends (see [`Runtime::origin`]).
    fn ran(&mut self, f: &mut Fiber, ok: bool) {
        let (Some(o), Some((class, m))) = (f.origin.take(), f.stat) else { return };
        let method = self.program.objects[class as usize].methods.get_index(m as usize).map_or("", |(name, _)| name).to_string();
        let caller = match &f.caller {
            Who::Remote { id, .. } => Some(id.to_string()),
            Who::System | Who::Local { .. } => None,
        };
        self.outputs.push(Output::Ran { object: o.obj.key(), method, by: o.by.into(), caller, ok, ms: (self.now - o.at).max(0.0) as u64 });
    }

    /// Continues the fiber waiting on promise `id` with an effect's result.
    /// When `unreachable` is true (a transient downstream `fetch` transport error), an uncaught
    /// failure unwinds with `Exc::busy` (HTTP 503 via `refuse_start`) instead of caching a
    /// permanent `400` `CallRecord`, so devices safely retry the same `Idempotency-Key`.
    fn resume(&mut self, id: u32, ok: Option<Json>, error: Option<String>, field: Option<String>, unreachable: bool) {
        if let Some((mut f, reply)) = self.waiting.remove(&id) {
            if !self.touched.is_empty() {
                self.flush();
            }
            let mark = self.turn_mark();
            self.ran_code = true;
            let r = match error {
                Some(e) => {
                    #[cfg(feature = "server")]
                    let exc = if self.server && unreachable { Exc::busy(e) } else { Exc::with_field(e, field) };
                    #[cfg(not(feature = "server"))]
                    let exc = {
                        let _ = unreachable;
                        Exc::with_field(e, field)
                    };
                    self.unwind(&mut f, exc).and_then(|()| self.run(&mut f))
                }
                None => {
                    f.stack.push(Value::from_json(&ok.unwrap_or(Json::Null)));
                    self.run(&mut f)
                }
            };
            self.settle(f, reply, r, mark);
        }
    }

    /// Refuses a request with an HTTP-like status (see `Output::Reply`).
    fn reply_status(&mut self, reply: Option<u64>, status: u16, why: String) {
        match reply {
            Some(id) if id >= INVOKE_REPLY_BASE => self.reply_with_status(reply, Err((why, None)), Some(status)),
            Some(id) => self.outputs.push(Output::Reply { reply: id, ok: None, error: Some(why), field: None, status: Some(status) }),
            None => self.outputs.push(Output::Error { message: why, trace: Vec::new() }),
        }
    }

    /// Answers a request; without a reply id, errors are reported and results dropped.
    fn reply(&mut self, reply: Option<u64>, r: Result<Json, (String, Option<String>)>) {
        self.reply_with_status(reply, r, None);
    }

    pub(crate) fn reply_with_status(&mut self, reply: Option<u64>, r: Result<Json, (String, Option<String>)>, status: Option<u16>) {
        if let Some(internal) = reply.filter(|&id| id >= CALL_REPLY_BASE) {
            self.finish_call(internal, r, status);
            return;
        }
        if let Some(internal) = reply.filter(|&id| id >= INVOKE_REPLY_BASE) {
            let (ok, error, field) = match r {
                Ok(v) => (Some(v), None, None),
                Err((e, f)) => (None, Some(e), f),
            };
            self.resume((internal - INVOKE_REPLY_BASE) as u32, ok, error, field, cfg!(feature = "server") && status == Some(503));
            return;
        }
        match (reply, r) {
            (Some(id), Ok(j)) => self.outputs.push(Output::Reply { reply: id, ok: Some(j), error: None, field: None, status: None }),
            (Some(id), Err((e, field))) => self.outputs.push(Output::Reply { reply: id, ok: None, error: Some(e), field, status }),
            (None, Err((e, _))) => self.outputs.push(Output::Error { message: e, trace: Vec::new() }),
            (None, Ok(_)) => {}
        }
    }

    // ---------- timers ----------

    fn push_timer(&mut self, at: f64, kind: TimerKind) {
        self.seq += 1;
        self.timers.push(Reverse(Timer { at, seq: self.seq, kind }));
    }

    fn fire_timers(&mut self) {
        if !self.touched.is_empty() {
            self.flush();
        }
        // Only fire timers that already existed on entry: a timer re-queued by a failed disk
        // save gets seq > cutoff and retries on the next tick instead of looping forever.
        let cutoff = self.seq;
        let mut deferred = Vec::new();
        while self.timers.peek().is_some_and(|Reverse(t)| t.at <= self.now) {
            let Some(Reverse(t)) = self.timers.pop() else { break };
            if t.seq > cutoff {
                deferred.push(Reverse(t));
                continue;
            }
            match t.kind {
                // A deleted object's timers stop.
                TimerKind::Every { obj, .. } if self.is_gone(&obj) => {}
                TimerKind::Every { obj, method, interval } => {
                    // Skip missed beats instead of firing a burst after a pause.
                    let mut next = t.at + interval;
                    if next <= self.now {
                        next = self.now + interval;
                    }
                    self.push_timer(next, TimerKind::Every { obj: obj.clone(), method: method.clone(), interval });
                    self.invoke(
                        obj,
                        &method,
                        Vec::new(),
                        false,
                        None,
                        Entry {
                            who: Who::System,
                            chain: 0,
                            by: By::Timer,
                            #[cfg(feature = "server")]
                            call_id: None,
                        },
                    );
                }
                TimerKind::Alarm { obj, alarm } => {
                    if let Some(a) = self.take_alarm(&obj, alarm) {
                        let args = a.args.iter().map(Value::from_json).collect();
                        // An alarm runs as whoever scheduled it, so rules on data still hold.
                        let who = a.caller.as_ref().map_or(Who::System, |c| Who::from_caller(c, self.server));
                        self.invoke(
                            obj,
                            &a.method,
                            args,
                            false,
                            None,
                            Entry {
                                who,
                                chain: 0,
                                by: By::Alarm,
                                #[cfg(feature = "server")]
                                call_id: None,
                            },
                        );
                    }
                }
                TimerKind::Outbox => self.dispatch_calls(),
                TimerKind::Clock => {
                    self.clock = None;
                    self.ran_code = true;
                }
                TimerKind::Expire => self.expire_waiters(),
                #[cfg(feature = "server")]
                TimerKind::Retain => self.retain_step(),
            }
        }
        for t in deferred {
            self.timers.push(t);
        }
    }
}

//! The interpreter loop. A [`Fiber`] owns its whole call stack, so a fiber can
//! suspend at `await` and resume later without involving the Rust stack.

use super::builtins;
use super::program::{Capture, Op, Source};
use super::value::{fmt_num, Closure, Elem, Exc, ObjRef, Value};
use crate::runtime::Runtime;
use std::cell::RefCell;
use std::rc::Rc;

const MAX_FRAMES: usize = 256;
/// Instructions one run may execute before it is stopped as a runaway.
/// Instructions one run may take before it fails as an endless loop. A server runs every
/// object on one thread, so its limit is lower: about 0.3 s on a laptop, against 3 s.
const STEP_BUDGET: u64 = 200_000_000;
pub(crate) const SERVER_STEP_BUDGET: u64 = 20_000_000;

/// Per-fiber context for deterministic `crypto.idempotencyKey(label)` derivation on servers
/// (`docs/retried-effects.md`).
#[cfg(feature = "server")]
pub(crate) struct IdempotencyCtx {
    pub object: Rc<str>,
    pub method: Rc<str>,
    pub call_id: Option<Rc<str>>,
    pub fallback_root: Option<[u8; 16]>,
    pub used_labels: std::collections::HashSet<(String, String)>,
}

#[derive(Default)]
pub struct Fiber {
    pub stack: Vec<Value>,
    pub frames: Vec<Frame>,
    handlers: Vec<Handler>,
    /// Who the call belongs to: `caller` in code, and alarms it schedules.
    pub caller: crate::runtime::access::Who,
    /// The method this fiber runs, by class and method index, for statistics.
    pub stat: Option<(u16, u16)>,
    /// The chain the call belongs to: its request, timer, alarm or event, and everything it
    /// runs, `invoke` included (see `runtime/exclusive.rs`). 0 until it first runs.
    pub chain: u64,
    /// On a server, what started a call nobody requested (a timer, alarm or `invoke`).
    pub origin: Option<crate::runtime::Origin>,
    /// Server-side scope for `crypto.idempotencyKey(label)` and duplicate-label detection.
    #[cfg(feature = "server")]
    pub(crate) idempotency: Option<IdempotencyCtx>,
}

pub struct Frame {
    pub closure: Rc<Closure>,
    pub ip: usize,
    pub base: usize,
    /// Stack height to restore on return.
    pub ret: usize,
    pub this: Option<Rc<ObjRef>>,
    /// The frame runs an `@exclusive` method and holds its object until it ends.
    pub exclusive: bool,
}

struct Handler {
    frames: usize,
    stack: usize,
    ip: usize,
}

pub enum Run {
    Done(Value),
    /// Suspended on the promise with this id.
    Await(u32),
    /// Waiting for an object an exclusive method of which another call holds.
    Blocked(crate::runtime::exclusive::Key),
}

/// An uncaught exception, with a trace from innermost to outermost frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub message: String,
    pub trace: Vec<String>,
    /// How a request fails, when not the default (`Exc::refused`).
    pub status: Option<u16>,
    /// The field named by `check(ok, field, message)`, if any.
    pub field: Option<String>,
    /// True when the failure came from an internal VM/runtime fault (null deref, bad call,
    /// stack overflow, runaway step budget) rather than user `throw`/`check` or access refusal.
    #[cfg(feature = "server")]
    pub fault: bool,
}

impl Runtime {
    /// Pushes a call frame for `closure`. Arguments are already on the stack from `base`.
    pub(crate) fn push_frame(&self, f: &mut Fiber, closure: Rc<Closure>, base: usize, ret: usize, this: Option<Rc<ObjRef>>) -> Result<(), Exc> {
        if f.frames.len() >= MAX_FRAMES {
            return Err(Exc::fault("stack overflow: too much recursion"));
        }
        let proto = &self.program.protos[closure.proto as usize];
        let params = proto.params as usize;
        if f.stack.len() > base + params {
            f.stack.truncate(base + params);
        }
        f.stack.resize(base + proto.locals as usize, Value::Null);
        f.frames.push(Frame { closure, ip: 0, base, ret, this, exclusive: false });
        Ok(())
    }

    /// Runs the fiber until it finishes, suspends or fails.
    pub(crate) fn run(&mut self, f: &mut Fiber) -> Result<Run, Failure> {
        let limit = if self.server { SERVER_STEP_BUDGET } else { STEP_BUDGET };
        let mut budget = limit;
        if f.chain == 0 {
            f.chain = self.new_chain();
        }
        // Builtins that act for the caller (`invoke`, `crypto.idempotencyKey`) need the active fiber's context.
        let outer = std::mem::replace(&mut self.running_caller, f.caller.clone());
        let outer_chain = std::mem::replace(&mut self.running_chain, f.chain);
        #[cfg(feature = "server")]
        let outer_idem = std::mem::replace(&mut self.running_idempotency, f.idempotency.take());
        let r = self.run_steps(f, &mut budget);
        #[cfg(feature = "server")]
        {
            f.idempotency = std::mem::replace(&mut self.running_idempotency, outer_idem);
        }
        self.running_caller = outer;
        self.running_chain = outer_chain;
        self.metrics.vm_steps += limit - budget;
        self.count_steps(f.stat, limit - budget);
        r
    }

    fn run_steps(&mut self, f: &mut Fiber, budget: &mut u64) -> Result<Run, Failure> {
        let prog = self.program.clone();
        'frames: loop {
            let fi = f.frames.len() - 1;
            let (closure, mut ip, base) = {
                let fr = &f.frames[fi];
                (fr.closure.clone(), fr.ip, fr.base)
            };
            let proto = &prog.protos[closure.proto as usize];
            let code = &proto.code;

            macro_rules! throw {
                ($e:expr) => {{
                    f.frames[fi].ip = ip;
                    let exc: Exc = $e;
                    match self.unwind(f, exc) {
                        Ok(()) => continue 'frames,
                        Err(fail) => return Err(fail),
                    }
                }};
            }
            macro_rules! tri {
                ($e:expr) => {
                    match $e {
                        Ok(v) => v,
                        Err(e) => throw!(e),
                    }
                };
            }
            let floor = base + proto.locals as usize;
            const MAX_STACK: usize = 65_536;
            const MAX_HANDLERS: usize = 1_024;
            macro_rules! pop {
                () => {{
                    if f.stack.len() <= floor {
                        throw!(Exc::fault("stack underflow"));
                    }
                    f.stack.pop().unwrap()
                }};
            }
            macro_rules! top {
                () => {{
                    if f.stack.len() <= floor {
                        throw!(Exc::fault("stack underflow"));
                    }
                    f.stack.last().unwrap()
                }};
            }
            macro_rules! name {
                ($k:expr) => {
                    match &prog.consts[$k as usize] {
                        Value::Str(s) => s.clone(),
                        _ => unreachable!("name constant"),
                    }
                };
            }

            loop {
                *budget -= 1;
                if *budget == 0 {
                    f.frames[fi].ip = ip;
                    return Err(self.failure(f, "the program ran too long without finishing (an endless loop?)".into()));
                }
                if f.stack.len() >= MAX_STACK {
                    throw!(Exc::fault("stack overflow"));
                }
                let op = code[ip];
                ip += 1;
                match op {
                    Op::Const(k) => f.stack.push(prog.consts[k as usize].clone()),
                    Op::Null => f.stack.push(Value::Null),
                    Op::True => f.stack.push(Value::Bool(true)),
                    Op::False => f.stack.push(Value::Bool(false)),
                    Op::Pop => {
                        pop!();
                    }
                    Op::Dup => {
                        let v = top!().clone();
                        f.stack.push(v);
                    }
                    Op::Dup2 => {
                        let n = f.stack.len();
                        if n < floor + 2 {
                            throw!(Exc::fault("stack underflow"));
                        }
                        let (a, b) = (f.stack[n - 2].clone(), f.stack[n - 1].clone());
                        f.stack.push(a);
                        f.stack.push(b);
                    }
                    Op::LoadLocal(s) => {
                        let v = f.stack[base + s as usize].clone();
                        if matches!(v, Value::Cell(_)) {
                            throw!(Exc::fault("invalid cell load"));
                        }
                        f.stack.push(v);
                    }
                    Op::StoreLocal(s) => {
                        let v = top!().clone();
                        f.stack[base + s as usize] = v;
                    }
                    Op::MakeCell(s) => {
                        let slot = &mut f.stack[base + s as usize];
                        if !matches!(slot, Value::Cell(_)) {
                            let v = std::mem::replace(slot, Value::Null);
                            *slot = Value::Cell(Rc::new(RefCell::new(v)));
                        }
                    }
                    Op::LoadCell(s) => {
                        let v = match &f.stack[base + s as usize] {
                            Value::Cell(c) => c.borrow().clone(),
                            other => other.clone(),
                        };
                        f.stack.push(v);
                    }
                    Op::StoreCell(s) => {
                        let v = top!().clone();
                        let i = base + s as usize;
                        if let Value::Cell(c) = &f.stack[i] {
                            *c.borrow_mut() = v;
                        } else {
                            f.stack[i] = v;
                        }
                    }
                    Op::Recell(s) => {
                        let i = base + s as usize;
                        if let Value::Cell(c) = &f.stack[i] {
                            let v = c.borrow().clone();
                            f.stack[i] = Value::Cell(Rc::new(RefCell::new(v)));
                        }
                    }
                    Op::LoadUpval(i) => f.stack.push(closure.upvals[i as usize].borrow().clone()),
                    Op::StoreUpval(i) => *closure.upvals[i as usize].borrow_mut() = top!().clone(),
                    Op::LoadGlobal(g) => f.stack.push(self.globals[g as usize].clone()),
                    Op::StoreGlobal(g) => self.globals[g as usize] = top!().clone(),
                    Op::GetField(k) => {
                        let obj = pop!();
                        let v = tri!(self.get_field(f.frames[fi].this.as_deref(), &obj, &name!(k)));
                        f.stack.push(v);
                    }
                    Op::SetField(k) => {
                        let v = pop!();
                        let obj = pop!();
                        tri!(self.set_field(f.frames[fi].this.as_deref(), &obj, &name!(k), v.clone()));
                        f.stack.push(v);
                    }
                    Op::GetIndex => {
                        let idx = pop!();
                        let obj = pop!();
                        let v = tri!(self.get_index(f.frames[fi].this.as_deref(), &obj, &idx));
                        f.stack.push(v);
                    }
                    Op::SetIndex => {
                        let v = pop!();
                        let idx = pop!();
                        let obj = pop!();
                        tri!(self.set_index(f.frames[fi].this.as_deref(), &obj, &idx, v.clone()));
                        f.stack.push(v);
                    }
                    Op::NewArr => f.stack.push(Value::arr(Vec::new())),
                    Op::ArrPush => {
                        let v = pop!();
                        if let Value::Arr(a) = top!() {
                            a.borrow_mut().push(v);
                        }
                    }
                    Op::ArrSpread => {
                        let v = pop!();
                        let items: Vec<Value> = match &v {
                            Value::Arr(other) => other.borrow().iter().map(|x| Value::inside(other.readonly, x.clone())).collect(),
                            Value::Str(s) => s.chars().map(|c| Value::str(&c.to_string())).collect(),
                            other => throw!(Exc::fault(format!("cannot spread a {} into an array", other.type_name()))),
                        };
                        if let Value::Arr(a) = top!() {
                            a.borrow_mut().extend(items);
                        }
                    }
                    Op::NewRec => f.stack.push(Value::rec(Default::default())),
                    Op::RecSet(k) => {
                        let v = pop!();
                        if let Value::Rec(r) = top!() {
                            r.borrow_mut().insert(name!(k), v);
                        }
                    }
                    Op::RecSetDyn => {
                        let v = pop!();
                        let key = pop!();
                        let key: Rc<str> = match key {
                            Value::Str(s) => s,
                            Value::Num(n) => Rc::from(fmt_num(n).as_str()),
                            other => throw!(Exc::fault(format!("record keys must be strings, got {}", other.type_name()))),
                        };
                        if let Value::Rec(r) = top!() {
                            r.borrow_mut().insert(key, v);
                        }
                    }
                    Op::RecSpread => {
                        let v = pop!();
                        let entries: Vec<(Rc<str>, Value)> = match &v {
                            Value::Rec(src) => src.borrow().iter().map(|(k, v)| (k.clone(), Value::inside(src.readonly, v.clone()))).collect(),
                            Value::Null => Vec::new(),
                            other => throw!(Exc::fault(format!("cannot spread a {} into a record", other.type_name()))),
                        };
                        if let Value::Rec(r) = top!() {
                            r.borrow_mut().extend(entries);
                        }
                    }
                    Op::Add => {
                        let b = pop!();
                        let a = pop!();
                        let v = match (&a, &b) {
                            (Value::Num(x), Value::Num(y)) => Value::Num(x + y),
                            (Value::Str(_), _) | (_, Value::Str(_)) => Value::str(&(a.display() + &b.display())),
                            _ => throw!(Exc::fault(format!("cannot add {} and {}", a.type_name(), b.type_name()))),
                        };
                        f.stack.push(v);
                    }
                    Op::Sub | Op::Mul | Op::Div | Op::Mod => {
                        let b = pop!();
                        let a = pop!();
                        let (Value::Num(x), Value::Num(y)) = (&a, &b) else {
                            throw!(Exc::fault(format!("arithmetic needs numbers, got {} and {}", a.type_name(), b.type_name())))
                        };
                        f.stack.push(Value::Num(match op {
                            Op::Sub => x - y,
                            Op::Mul => x * y,
                            Op::Div => x / y,
                            _ => x % y,
                        }));
                    }
                    Op::Neg => match pop!() {
                        Value::Num(n) => f.stack.push(Value::Num(-n)),
                        other => throw!(Exc::fault(format!("cannot negate a {}", other.type_name()))),
                    },
                    Op::Not => {
                        let v = pop!();
                        f.stack.push(Value::Bool(!v.truthy()));
                    }
                    Op::Eq | Op::Ne => {
                        let b = pop!();
                        let a = pop!();
                        f.stack.push(Value::Bool(a.strict_eq(&b) == (op == Op::Eq)));
                    }
                    Op::Lt | Op::Le | Op::Gt | Op::Ge => {
                        let b = pop!();
                        let a = pop!();
                        let ord = match (&a, &b) {
                            (Value::Num(x), Value::Num(y)) => x.partial_cmp(y),
                            (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
                            _ => throw!(Exc::fault(format!("cannot compare {} and {}", a.type_name(), b.type_name()))),
                        };
                        let r = ord.is_some_and(|o| match op {
                            Op::Lt => o.is_lt(),
                            Op::Le => o.is_le(),
                            Op::Gt => o.is_gt(),
                            _ => o.is_ge(),
                        });
                        f.stack.push(Value::Bool(r));
                    }
                    Op::Concat(n) => {
                        let need = n as usize;
                        if f.stack.len() < floor + need {
                            throw!(Exc::fault("stack underflow"));
                        }
                        let start = f.stack.len() - need;
                        let s: String = f.stack.drain(start..).map(|v| v.display()).collect();
                        f.stack.push(Value::str(&s));
                    }
                    Op::Len => {
                        let n = match pop!() {
                            Value::Arr(a) => a.borrow().len(),
                            Value::Str(s) => s.chars().count(),
                            other => throw!(Exc::fault(format!("cannot loop over a {}; for...of works on arrays and strings (use Object.keys for records)", other.type_name()))),
                        };
                        f.stack.push(Value::Num(n as f64));
                    }
                    Op::Jump(t) => ip = t as usize,
                    Op::JumpIfFalse(t) => {
                        if !pop!().truthy() {
                            ip = t as usize;
                        }
                    }
                    Op::JumpIfFalseKeep(t) => {
                        if top!().truthy() {
                            pop!();
                        } else {
                            ip = t as usize;
                        }
                    }
                    Op::JumpIfTrueKeep(t) => {
                        if top!().truthy() {
                            ip = t as usize;
                        } else {
                            pop!();
                        }
                    }
                    Op::JumpIfNotNullKeep(t) => {
                        if matches!(top!(), Value::Null) {
                            pop!();
                        } else {
                            ip = t as usize;
                        }
                    }
                    Op::JumpIfNullKeep(t) => {
                        if matches!(top!(), Value::Null) {
                            ip = t as usize;
                        }
                    }
                    Op::Call(argc) => {
                        let need = 1 + argc as usize;
                        if f.stack.len() < floor + need {
                            throw!(Exc::fault("stack underflow"));
                        }
                        f.frames[fi].ip = ip;
                        let pos = f.stack.len() - need;
                        tri!(self.call_value(f, pos));
                        continue 'frames;
                    }
                    Op::CallMethod(k, argc) => {
                        let need = 1 + argc as usize;
                        if f.stack.len() < floor + need {
                            throw!(Exc::fault("stack underflow"));
                        }
                        f.frames[fi].ip = ip;
                        let name = name!(k);
                        if let Some(key) = tri!(self.must_wait(f, &name, argc as usize)) {
                            // Wait for the object, then run this call instruction again.
                            f.frames[fi].ip = ip - 1;
                            return Ok(Run::Blocked(key));
                        }
                        tri!(self.call_method(f, &name, argc as usize));
                        continue 'frames;
                    }
                    Op::Return => {
                        let mut v = pop!();
                        let fr = f.frames.pop().expect("frame");
                        // A method handing its own containers to code outside its object hands
                        // read-only views (`list() { return this.items }`).
                        if let (Some(o), Some(caller)) = (&fr.this, f.frames.last()) {
                            if caller.this.as_deref() != Some(&**o) {
                                v = self.protect_returned(o, v);
                            }
                        }
                        if let (true, Some(o)) = (fr.exclusive, &fr.this) {
                            self.release_object(o);
                        }
                        f.stack.truncate(fr.ret);
                        let depth = f.frames.len();
                        f.handlers.retain(|h| h.frames <= depth);
                        if depth == 0 {
                            return Ok(Run::Done(v));
                        }
                        f.stack.push(v);
                        continue 'frames;
                    }
                    Op::Closure(p) => {
                        let captures = &prog.protos[p as usize].captures;
                        let upvals = captures
                            .iter()
                            .map(|c| match c {
                                Capture::Local(s) => match &f.stack[base + *s as usize] {
                                    Value::Cell(cell) => cell.clone(),
                                    other => Rc::new(RefCell::new(other.clone())),
                                },
                                Capture::Upval(i) => closure.upvals[*i as usize].clone(),
                            })
                            .collect();
                        let this = f.frames[fi].this.clone();
                        f.stack.push(Value::Func(Rc::new(Closure { proto: p, upvals, this })));
                    }
                    Op::Elem(d) => {
                        let desc = &prog.elems[d as usize];
                        let np = desc.props.len();
                        let need = np.saturating_add(desc.children as usize);
                        if f.stack.len() < floor + need {
                            throw!(Exc::fault("stack underflow"));
                        }
                        let vals = f.stack.split_off(f.stack.len() - need);
                        let (props, children) = vals.split_at(np);
                        let elem = Elem { kind: desc.kind.clone(), props: desc.props.iter().cloned().zip(props.iter().cloned()).collect(), children: children.to_vec() };
                        f.stack.push(Value::Elem(Rc::new(elem)));
                    }
                    Op::Caller => f.stack.push(f.caller.record()),
                    Op::NeedsConsent(k) => {
                        let o = pop!();
                        let Value::Str(purpose) = &self.program.consts[k as usize] else { unreachable!("the purpose is a string constant") };
                        #[cfg(feature = "server")]
                        let refused = match &o {
                            Value::Obj(o) => self.needs_consent(o, &purpose.clone()).err(),
                            _ => Some(Exc::msg("@needs runs on objects")),
                        };
                        #[cfg(not(feature = "server"))]
                        let refused = {
                            drop(o);
                            Some(Exc::msg(format!("@needs(\"consent:{purpose}\") is checked on servers only")))
                        };
                        if let Some(e) = refused {
                            throw!(e);
                        }
                    }
                    Op::Await => {
                        let v = pop!();
                        if let Value::Promise(id) = v {
                            f.frames[fi].ip = ip;
                            return Ok(Run::Await(id));
                        }
                        f.stack.push(v);
                    }
                    Op::Throw => {
                        let v = pop!();
                        #[cfg(feature = "server")]
                        let kind = if self.server && matches!(&v, Value::Rec(r) if matches!(r.borrow().get("retryable"), Some(Value::Bool(true)))) {
                            crate::vm::value::ExcKind::Busy
                        } else {
                            crate::vm::value::ExcKind::User
                        };
                        #[cfg(not(feature = "server"))]
                        let kind = crate::vm::value::ExcKind::User;
                        throw!(Exc(v, kind));
                    }
                    Op::TryBegin(t) => {
                        if f.handlers.len() >= MAX_HANDLERS {
                            throw!(Exc::fault("handler stack overflow"));
                        }
                        f.handlers.push(Handler { frames: f.frames.len(), stack: f.stack.len(), ip: t as usize });
                    }
                    Op::TryEnd => {
                        f.handlers.pop();
                    }
                }
            }
        }
    }

    /// Transfers control to the innermost `catch`, or fails the fiber.
    pub(crate) fn unwind(&mut self, f: &mut Fiber, exc: Exc) -> Result<(), Failure> {
        match f.handlers.pop() {
            Some(h) => {
                self.release_frames(f, h.frames);
                f.frames.truncate(h.frames);
                f.stack.truncate(h.stack);
                if let Some(fr) = f.frames.last_mut() {
                    fr.ip = h.ip;
                }
                f.stack.push(exc.0);
                Ok(())
            }
            None => Err(Failure {
                status: exc.status(),
                field: exc.field(),
                #[cfg(feature = "server")]
                fault: exc.is_fault(),
                ..self.failure(f, exc.message())
            }),
        }
    }

    pub(crate) fn failure(&self, f: &Fiber, message: String) -> Failure {
        let trace = f
            .frames
            .iter()
            .rev()
            .map(|fr| {
                let proto = &self.program.protos[fr.closure.proto as usize];
                let at = match proto.source {
                    Source::User => {
                        let off = proto.offsets.get(fr.ip.saturating_sub(1)).copied().unwrap_or(0);
                        if self.program.user_source.is_empty() {
                            format!("bytecode+{off}")
                        } else {
                            #[cfg(feature = "compiler")]
                            match crate::modules::locate(&self.program.user_source, off as usize) {
                                (Some(file), line, col) => format!("{file}:{line}:{col}"),
                                (None, line, col) => format!("{line}:{col}"),
                            }
                            #[cfg(not(feature = "compiler"))]
                            format!("bytecode+{off}")
                        }
                    }
                    Source::Prelude => "prelude".into(),
                };
                format!("at {} ({at})", proto.name)
            })
            .collect();
        Failure {
            message,
            trace,
            status: None,
            field: None,
            #[cfg(feature = "server")]
            fault: true,
        }
    }

    fn call_value(&mut self, f: &mut Fiber, pos: usize) -> Result<(), Exc> {
        match f.stack[pos].clone() {
            Value::Func(c) => {
                let this = c.this.clone();
                self.push_frame(f, c, pos + 1, pos, this)
            }
            Value::Native(n) => {
                let args = f.stack.split_off(pos + 1);
                f.stack.truncate(pos);
                #[cfg(feature = "server")]
                let prev_this = std::mem::replace(
                    &mut self.running_this,
                    f.frames.iter().rev().find_map(|fr| fr.this.clone()),
                );
                let r = (n.f)(self, &args);
                #[cfg(feature = "server")]
                {
                    self.running_this = prev_this;
                }
                f.stack.push(r?);
                Ok(())
            }
            Value::Class(c) => {
                let name = &self.program.objects[c as usize].name;
                Err(Exc::fault(format!("{name} is an object type, not a function; use {name}.get(id)")))
            }
            other => Err(Exc::fault(format!("a {} is not a function", other.type_name()))),
        }
    }

    fn call_method(&mut self, f: &mut Fiber, name: &str, argc: usize) -> Result<(), Exc> {
        let pos = f.stack.len() - 1 - argc;
        let recv = f.stack[pos].clone();
        match &recv {
            Value::Obj(o) => {
                let caller_this = f.frames.last().and_then(|fr| fr.this.clone());
                let own = caller_this.as_deref() == Some(&**o);
                match name {
                    "emit" | "schedule" | "unschedule" | "delete" if !own => Err(Exc::msg(format!("{name}() can only be called inside {}'s own methods", o.class_name))),
                    "emit" => {
                        let args = f.stack.split_off(pos + 1);
                        f.stack.truncate(pos);
                        self.emit_event(o, &args)?;
                        f.stack.push(Value::Null);
                        Ok(())
                    }
                    "schedule" => {
                        let args = f.stack.split_off(pos + 1);
                        f.stack.truncate(pos);
                        let handle = self.schedule_alarm(o, &args, &f.caller)?;
                        f.stack.push(handle);
                        Ok(())
                    }
                    "delete" => {
                        f.stack.truncate(pos);
                        self.delete_object(o)?;
                        f.stack.push(Value::Null);
                        Ok(())
                    }
                    "unschedule" => {
                        let args = f.stack.split_off(pos + 1);
                        f.stack.truncate(pos);
                        let gone = self.unschedule_alarm(o, args.first().unwrap_or(&Value::Null))?;
                        f.stack.push(gone);
                        Ok(())
                    }
                    _ if self.is_remote(o) => {
                        let args = f.stack.split_off(pos + 1);
                        f.stack.truncate(pos);
                        let promise = self.remote_call(o, name, &args)?;
                        f.stack.push(promise);
                        Ok(())
                    }
                    _ => {
                        let (closure, private) = self.method(o.class, name)?;
                        if private && !own {
                            return Err(Exc::msg(format!("'{name}' is private to {}", o.class_name)));
                        }
                        self.touch(o);
                        self.push_frame(f, closure, pos, pos, Some(o.clone()))?;
                        self.entered(f, o.class, &o.id, name);
                        Ok(())
                    }
                }
            }
            Value::Class(c) => {
                if name == "all" {
                    f.stack.truncate(pos);
                    let list = self.all_objects(*c)?.into_iter().map(Value::Obj).collect();
                    f.stack.push(Value::arr(list));
                    return Ok(());
                }
                if name != "get" {
                    let n = &self.program.objects[*c as usize].name;
                    return Err(Exc::msg(format!("{n}.{name} does not exist; use {n}.get(id) or {n}.all()")));
                }
                let id = match f.stack.get(pos + 1) {
                    Some(Value::Str(s)) => s.to_string(),
                    Some(Value::Num(n)) => fmt_num(*n),
                    _ => return Err(Exc::msg("get(id) needs a string id")),
                };
                f.stack.truncate(pos);
                let handle = self.instance(*c, &id)?;
                f.stack.push(Value::Obj(handle));
                Ok(())
            }
            Value::Rec(r) => {
                let field = r.borrow().get(name).cloned();
                match field {
                    Some(callee @ (Value::Func(_) | Value::Native(_))) => {
                        f.stack[pos] = callee;
                        self.call_value(f, pos)
                    }
                    Some(other) => Err(Exc::fault(format!("'{name}' is a {}, not a function", other.type_name()))),
                    None => Err(Exc::fault(format!("the record has no function '{name}'"))),
                }
            }
            Value::Arr(_) | Value::Str(_) | Value::Num(_) => {
                if let Some(m) = builtins::method(&recv, name) {
                    let args = f.stack.split_off(pos + 1);
                    f.stack.truncate(pos);
                    let r = m(self, &recv, &args)?;
                    f.stack.push(r);
                    return Ok(());
                }
                if let (Value::Arr(_), Some(&slot)) = (&recv, self.prelude_array.get(name)) {
                    // Call the prelude function with the array as its first argument.
                    f.stack.insert(pos, self.globals[slot as usize].clone());
                    return self.call_value(f, pos);
                }
                Err(Exc::fault(format!("{}s have no method '{name}'", recv.type_name())))
            }
            Value::Null => Err(Exc::fault(format!("cannot call .{name}() on null; check for null first or use ?."))),
            other => Err(Exc::fault(format!("a {} has no method '{name}'", other.type_name()))),
        }
    }

    pub(crate) fn get_field(&mut self, this: Option<&ObjRef>, obj: &Value, name: &str) -> Result<Value, Exc> {
        match obj {
            Value::Rec(r) => Ok(Value::inside(r.readonly, r.borrow().get(name).cloned().unwrap_or(Value::Null))),
            Value::Arr(a) if name == "length" => Ok(Value::Num(a.borrow().len() as f64)),
            Value::Str(s) if name == "length" => Ok(Value::Num(s.chars().count() as f64)),
            Value::Obj(o) => self.obj_field(this, o, name),
            Value::Null => Err(Exc::fault(format!("cannot read '{name}' of null; check for null first or use ?."))),
            other => Err(Exc::fault(format!("a {} has no field '{name}'", other.type_name()))),
        }
    }

    fn set_field(&mut self, this: Option<&ObjRef>, obj: &Value, name: &str, v: Value) -> Result<(), Exc> {
        match obj {
            Value::Rec(r) => {
                r.writable()?.borrow_mut().insert(Rc::from(name), v);
                Ok(())
            }
            Value::Obj(o) => self.set_obj_field(this, o, name, v),
            Value::Null => Err(Exc::fault(format!("cannot set '{name}' of null"))),
            other => Err(Exc::fault(format!("cannot set fields on a {}", other.type_name()))),
        }
    }

    fn get_index(&mut self, this: Option<&ObjRef>, obj: &Value, idx: &Value) -> Result<Value, Exc> {
        match (obj, idx) {
            (Value::Arr(a), Value::Num(n)) => Ok(Value::inside(a.readonly, if *n >= 0.0 && n.fract() == 0.0 { a.borrow().get(*n as usize).cloned().unwrap_or(Value::Null) } else { Value::Null })),
            (Value::Str(s), Value::Num(n)) => Ok(if *n >= 0.0 && n.fract() == 0.0 {
                s.chars().nth(*n as usize).map_or(Value::Null, |c| Value::str(&c.to_string()))
            } else {
                Value::Null
            }),
            (Value::Rec(r), Value::Str(k)) => Ok(Value::inside(r.readonly, r.borrow().get(&**k).cloned().unwrap_or(Value::Null))),
            (Value::Rec(r), Value::Num(n)) => Ok(Value::inside(r.readonly, r.borrow().get(fmt_num(*n).as_str()).cloned().unwrap_or(Value::Null))),
            (Value::Obj(o), Value::Str(k)) => self.obj_field(this, o, k),
            (Value::Null, _) => Err(Exc::fault("cannot index null; check for null first or use ?.[]")),
            (o, i) => Err(Exc::fault(format!("cannot index a {} with a {}", o.type_name(), i.type_name()))),
        }
    }

    fn set_index(&mut self, this: Option<&ObjRef>, obj: &Value, idx: &Value, v: Value) -> Result<(), Exc> {
        match (obj, idx) {
            (Value::Arr(a), Value::Num(n)) if *n >= 0.0 && n.fract() == 0.0 => {
                let i = *n as usize;
                let mut a = a.writable()?.borrow_mut();
                if i > a.len() + 1_000_000 {
                    return Err(Exc::msg("array index too far past the end"));
                }
                if i >= a.len() {
                    a.resize(i + 1, Value::Null);
                }
                a[i] = v;
                Ok(())
            }
            (Value::Rec(r), Value::Str(k)) => {
                r.writable()?.borrow_mut().insert(k.clone(), v);
                Ok(())
            }
            (Value::Rec(r), Value::Num(n)) => {
                r.writable()?.borrow_mut().insert(Rc::from(fmt_num(*n).as_str()), v);
                Ok(())
            }
            (Value::Obj(o), Value::Str(k)) => self.set_obj_field(this, o, k, v),
            (o, i) => Err(Exc::fault(format!("cannot set index {} on a {}", i.type_name(), o.type_name()))),
        }
    }
}

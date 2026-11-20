//! Durable objects: loading, field access rules, events, alarms and persistence.

use super::access::Who;
use super::protocol::{Caller, Output};
use super::{BootError, Runtime, TimerKind};
use crate::vm::interp::Fiber;
use crate::vm::value::{Closure, Exc, ObjRef, Record, Value};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value as Json};
use std::cell::RefCell;
use std::rc::Rc;

const ALARM_INDEX_KEY: &str = "tj:alarms";
/// The last change number handed out, so numbers keep growing across restarts.
const CHANGE_SEQ_KEY: &str = "tj:seq";
const MAX_ID_LEN: usize = 128;
/// How many objects a class stream starts with, newest changes first.
pub(crate) const CLASS_STREAM_CAP: usize = 1000;
/// How many recent call ids an object remembers, so retries of them do not run twice.
const MAX_CALL_RECORDS: usize = 100;
/// How long the result of a call made once is kept for repeats (72 hours). After that,
/// repeating the call id runs it again.
pub(crate) const CALL_RECORD_MS: f64 = 72.0 * 3_600_000.0;

pub(crate) struct Instance {
    pub(crate) handle: Rc<ObjRef>,
    /// A device-side replica of an `@server` object: methods run on the server.
    remote: bool,
    pub(crate) state: Rc<RefCell<Record>>,
    pub(crate) alarms: Vec<Alarm>,
    /// The change number of the current state: assigned here for objects this runtime owns,
    /// the server's for replicas. Streams resume from it and replicas ignore older states.
    seq: u64,
    /// Results of recent calls made with an id (see `calls.rs`), oldest first.
    pub(crate) calls: Vec<CallRecord>,
    /// The last persisted record, to skip unchanged writes.
    saved: String,
    /// The state part of it: only a new state gets a change number and is broadcast.
    state_text: String,
    /// When it was created and last changed (epoch ms), for `@retain` and `@legalHold`.
    pub(crate) created: f64,
    pub(crate) changed: f64,
    /// Just erased: saving the blank does not start its clock; the next change does.
    pub(crate) cleared: bool,
}

/// A call made with an id: who made it and with what, so a repeat is recognised, and its
/// result (see `calls.rs`).
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct CallRecord {
    id: String,
    /// Who made it: a keyed hash of the caller's id (`Runtime::caller_tag`), so shared
    /// objects keep no readable ids.
    by: String,
    /// When it was made (epoch ms); kept for `CALL_RECORD_MS`.
    #[serde(default)]
    at: f64,
    method: String,
    /// A hash of the arguments.
    args: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ok: Option<Json>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl CallRecord {
    pub(crate) fn new(id: &str, by: String, method: &str, args: &[Json], at: f64) -> CallRecord {
        let text = serde_json::to_string(args).expect("JSON values serialize");
        // FNV-1a: telling one call's arguments from another's, not hiding them.
        let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
        CallRecord { id: id.to_string(), by, at, method: method.to_string(), args: format!("{hash:016x}"), ok: None, error: None }
    }

    fn expired(&self, now: f64) -> bool {
        self.at + CALL_RECORD_MS < now
    }

    /// The same caller, method and arguments.
    pub(crate) fn same_call(&self, other: &CallRecord) -> bool {
        self.by == other.by && self.method == other.method && self.args == other.args
    }

    pub(crate) fn with_result(mut self, r: &Result<Json, String>) -> CallRecord {
        match r {
            Ok(v) => self.ok = Some(v.clone()),
            Err(e) => self.error = Some(e.clone()),
        }
        self
    }

    pub(crate) fn result(&self) -> Result<Json, String> {
        match &self.error {
            Some(e) => Err(e.clone()),
            None => Ok(self.ok.clone().unwrap_or(Json::Null)),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct Alarm {
    /// The handle `this.schedule` returned, kept across restarts so `this.unschedule` finds
    /// it. Records from before handles were saved read as 0 and get a new one.
    #[serde(default)]
    id: u64,
    at: f64,
    pub method: String,
    pub args: Vec<Json>,
    /// Who scheduled it, if a remote caller: the alarm runs as them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<Caller>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Stored {
    pub(crate) state: Map<String, Json>,
    #[serde(default)]
    pub(crate) alarms: Vec<Alarm>,
    #[serde(default)]
    seq: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) calls: Vec<CallRecord>,
    /// When the object was created and last changed (epoch ms); 0 in records from before
    /// they were kept, which then count from the first time they are seen.
    #[serde(default)]
    pub(crate) created: f64,
    #[serde(default)]
    pub(crate) changed: f64,
}

impl Runtime {
    /// Returns the handle of `Class/id`, loading or creating the instance.
    pub(crate) fn instance(&mut self, class: u16, id: &str) -> Result<Rc<ObjRef>, Exc> {
        // `*` names a whole class in a stream (`Slot/*`), so it is no object's id.
        if id.is_empty() || id == "*" || id.len() > MAX_ID_LEN || id.contains('/') || id.chars().any(char::is_control) {
            return Err(Exc::msg(format!("invalid object id {id:?}: use 1-{MAX_ID_LEN} characters without '/', and not `*`")));
        }
        let key = (class, Rc::<str>::from(id));
        if let Some(inst) = self.instances.get(&key) {
            return Ok(inst.handle.clone());
        }
        let program = self.program.clone();
        let def = &program.objects[class as usize];
        let handle = Rc::new(ObjRef { class, class_name: def.name.clone(), id: key.1.clone() });
        let state = self.defaults(class)?;
        let mut alarms = Vec::new();
        let mut seq = 0;
        let mut calls = Vec::new();
        let mut saved = String::new();
        let (mut created, mut changed) = (self.now, self.now);
        let mut dated = true;
        if let Some(text) = self.store.load(&handle.key()).map_err(|e| Exc::msg(format!("{} could not be read: {e}", handle.key())))? {
            let stored: Stored = serde_json::from_str(&text).map_err(|e| Exc::msg(format!("{} has unreadable stored state: {e}", handle.key())))?;
            // Stored fields override defaults; fields no longer declared are dropped.
            let mut s = state.borrow_mut();
            for (k, v) in &stored.state {
                if let Some(slot) = s.get_mut(k.as_str()) {
                    *slot = Value::from_json(v);
                }
            }
            alarms = stored.alarms;
            seq = stored.seq;
            calls = stored.calls.into_iter().filter(|c| !c.expired(self.now)).collect();
            saved = text;
            dated = stored.created > 0.0 && stored.changed > 0.0;
            if dated {
                (created, changed) = (stored.created, stored.changed);
            } else if alarms.is_empty() && calls.is_empty() && Value::Rec(self.defaults(class)?.into()).to_json().ok() == Some(Json::Object(stored.state.clone())) {
                // An erased record: no clock until it holds something again, and reading it
                // writes nothing.
                (created, changed, dated) = (0.0, 0.0, true);
            }
        }
        let remote = def.server && !self.server;
        if remote {
            // Timers and alarms of a replica run on the server, not here.
            alarms.clear();
            self.outputs.push(Output::Subscribe { object: handle.key(), seq });
        } else {
            for (name, m) in &def.methods {
                if let Some(interval) = m.every {
                    let kind = TimerKind::Every { obj: handle.clone(), method: name.clone(), interval };
                    self.push_timer(self.now + interval, kind);
                }
            }
            for a in &mut alarms {
                self.keep_alarm_id(a);
                self.push_timer(a.at, TimerKind::Alarm { obj: handle.clone(), alarm: a.id });
            }
        }
        let state_text = Value::Rec(state.clone().into()).to_json().map(|j| j.to_string()).unwrap_or_default();
        self.instances.insert(key.clone(), Instance { handle: handle.clone(), remote, state, alarms, seq, calls, saved, state_text, created, changed, cleared: false });
        if !dated && !remote && self.server {
            // Saved with the times from now on, so retention never counts from zero.
            self.touched.insert(key);
        }
        Ok(handle)
    }

    /// Whether `Class/id` is loaded already (so a request for it adds nothing to memory).
    pub(crate) fn is_loaded(&self, key: &str) -> bool {
        let Some((class, id)) = key.split_once('/') else { return false };
        self.program.object(class).is_some_and(|c| self.instances.contains_key(&(c, Rc::from(id))))
    }

    /// A fresh record of the default state of class `class`.
    pub(crate) fn defaults(&mut self, class: u16) -> Result<Rc<RefCell<Record>>, Exc> {
        let init = Rc::new(Closure { proto: self.program.objects[class as usize].init, upvals: Vec::new(), this: None });
        let f = self.fiber_for(&Value::Func(init), Vec::new())?;
        match self.run_sync(f).map_err(|fail| Exc::msg(fail.message))? {
            Value::Rec(state) => Ok(Rc::clone(&state)),
            _ => unreachable!("object init returns a record"),
        }
    }

    /// Resets the device's own objects (drafts, screen state, anything not `@server`) to their
    /// defaults, loaded or only stored, after a signed-in user signed out or another signed
    /// in: what one person typed or viewed must not stay for the next on a shared device.
    pub(crate) fn clear_device_objects(&mut self) {
        for class in 0..self.program.objects.len() as u16 {
            let def = &self.program.objects[class as usize];
            if def.server {
                continue;
            }
            let prefix = format!("{}/", def.name);
            let mut ids: Vec<String> = self.store.keys(&prefix).unwrap_or_default().into_iter().filter_map(|k| k.strip_prefix(&prefix).map(str::to_string)).collect();
            ids.extend(self.instances.values().filter(|i| i.handle.class == class).map(|i| i.handle.id.to_string()));
            ids.sort();
            ids.dedup();
            for id in ids {
                let (Ok(o), Ok(fresh)) = (self.instance(class, &id), self.defaults(class)) else { continue };
                let fresh = fresh.borrow().clone();
                let inst = self.inst_mut(&o);
                *inst.state.borrow_mut() = fresh;
                // Its alarms were set for the person who left; their timers find nothing.
                inst.alarms.clear();
                self.touch(&o);
                self.ran_code = true;
            }
        }
    }

    /// Resets every replica to its defaults and resubscribes from scratch, after the device's
    /// user changed: what one user could read must not stay for the next.
    pub(crate) fn clear_replicas(&mut self) {
        // Class streams start over too: the screen asks again for what this user may list.
        self.class_streams.clear();
        let replicas: Vec<Rc<ObjRef>> = self.instances.values().filter(|i| i.remote).map(|i| i.handle.clone()).collect();
        for o in replicas {
            let Ok(fresh) = self.defaults(o.class) else { continue };
            let inst = self.inst_mut(&o);
            let fresh = fresh.borrow().clone();
            *inst.state.borrow_mut() = fresh;
            inst.seq = 0;
            self.touch(&o);
            self.ran_code = true;
            self.outputs.push(Output::Subscribe { object: o.key(), seq: 0 });
        }
    }

    pub(crate) fn is_remote(&self, o: &ObjRef) -> bool {
        self.inst(o).remote
    }

    /// Calls a method of a replica on the server: returns a promise for the result.
    pub(crate) fn remote_call(&mut self, o: &ObjRef, method: &str, args: &[Value]) -> Result<Value, Exc> {
        let (_, private) = self.method(o.class, method)?;
        if private {
            return Err(Exc::msg(format!("'{method}' is private to {}", o.class_name)));
        }
        let args = args.iter().map(Value::to_json).collect::<Result<Vec<_>, _>>().map_err(|e| Exc::msg(format!("arguments to a server call must be data: {e}")))?;
        Ok(Value::Promise(self.queue_call(o.key(), method.to_string(), args)))
    }

    /// The record of call `id` on `o`, if it already ran.
    pub(crate) fn call_record(&self, o: &ObjRef, id: &str) -> Option<CallRecord> {
        self.inst(o).calls.iter().find(|c| c.id == id).cloned()
    }

    /// Remembers a finished call; it is saved with the object's state.
    pub(crate) fn record_call(&mut self, o: &ObjRef, record: CallRecord) {
        let now = self.now;
        let calls = &mut self.inst_mut(o).calls;
        calls.retain(|c| !c.expired(now));
        calls.push(record);
        if calls.len() > MAX_CALL_RECORDS {
            calls.remove(0);
        }
        self.touch(o);
    }

    /// Applies the server's state to a replica; it is cached locally and re-rendered.
    /// A state numbered `seq` no newer than the replica's is stale (it was overtaken in
    /// flight) and is ignored.
    pub(crate) fn sync(&mut self, key: &str, state: &Json, seq: Option<u64>) -> Result<(), String> {
        let o = self.resolve_object(key)?;
        if !self.is_remote(&o) {
            return Err(format!("{key} is not a replica of a server object"));
        }
        let Json::Object(fields) = state else { return Err(format!("state for {key} must be a record")) };
        if let Some(seq) = seq {
            let inst = self.inst_mut(&o);
            if seq <= inst.seq {
                return Ok(());
            }
            inst.seq = seq;
        }
        let inst = self.inst(&o);
        let mut s = inst.state.borrow_mut();
        for (k, v) in fields {
            if let Some(slot) = s.get_mut(k.as_str()) {
                *slot = Value::from_json(v);
            }
        }
        drop(s);
        self.touch(&o);
        self.ran_code = true;
        Ok(())
    }

    /// Parses `"Class/id"` and loads the instance.
    pub(crate) fn resolve_object(&mut self, key: &str) -> Result<Rc<ObjRef>, String> {
        let (class, id) = key.split_once('/').ok_or_else(|| format!("object must be \"Class/id\", got {key:?}"))?;
        let c = self.program.object(class).ok_or_else(|| format!("no object type named {class}"))?;
        self.instance(c, id).map_err(|e| e.message())
    }

    pub(crate) fn inst(&self, o: &ObjRef) -> &Instance {
        self.instances.get(&(o.class, o.id.clone())).expect("object handles always refer to loaded instances")
    }

    fn inst_mut(&mut self, o: &ObjRef) -> &mut Instance {
        self.instances.get_mut(&(o.class, o.id.clone())).expect("object handles always refer to loaded instances")
    }

    pub(crate) fn method(&self, class: u16, name: &str) -> Result<(Rc<Closure>, bool), Exc> {
        let def = &self.program.objects[class as usize];
        match def.methods.get_full(name) {
            Some((i, _, m)) => Ok((self.methods[class as usize][i].clone(), m.private)),
            None => {
                let list: Vec<&str> = def.methods.keys().map(|k| &**k).collect();
                Err(Exc::msg(format!("{} has no method '{name}' (methods: {})", def.name, list.join(", "))))
            }
        }
    }

    pub(crate) fn method_fiber(&mut self, obj: Rc<ObjRef>, name: &str, args: Vec<Value>, external: bool) -> Result<Fiber, Exc> {
        let (closure, private) = self.method(obj.class, name)?;
        if external && private {
            return Err(Exc::msg(format!("'{name}' is private to {}", obj.class_name)));
        }
        if self.is_remote(&obj) {
            return Err(Exc::msg(format!("{} lives on the server; call it there", obj.key())));
        }
        self.touch(&obj);
        let mut f = Fiber::default();
        f.stat = self.program.objects[obj.class as usize].methods.get_index_of(name).map(|m| (obj.class, m as u16));
        if let Some(slot) = f.stat {
            self.count_call(slot);
        }
        f.stack.push(Value::Obj(obj.clone()));
        f.stack.extend(args);
        self.push_frame(&mut f, closure, 0, 0, Some(obj))?;
        Ok(f)
    }

    pub(crate) fn touch(&mut self, o: &ObjRef) {
        self.touched.insert((o.class, o.id.clone()));
    }

    /// A field of `o`, read by code running in `this`'s methods (or none). Only `o`'s own
    /// methods get containers they can change; everyone else gets a read-only view.
    pub(crate) fn obj_field(&mut self, this: Option<&ObjRef>, o: &Rc<ObjRef>, name: &str) -> Result<Value, Exc> {
        if name == "id" {
            return Ok(Value::Str(o.id.clone()));
        }
        let v = self.inst(o).state.borrow().get(name).cloned();
        match v {
            Some(v) if this != Some(&**o) => Ok(v.read_only()),
            Some(v) => {
                // Containers can be changed in place, so the object may need saving.
                if matches!(v, Value::Arr(_) | Value::Rec(_)) {
                    self.touch(o);
                }
                Ok(v)
            }
            None if self.program.objects[o.class as usize].methods.contains_key(name) => {
                Err(Exc::msg(format!("'{name}' is a method; call it: .{name}()")))
            }
            None => Err(Exc::msg(format!("{} has no field '{name}'", o.class_name))),
        }
    }

    /// What a method of `o` returns to code outside `o`: every container of `o`'s state in it,
    /// at any depth (`this.items`, `this.items[0]`, `{ list: this.items }`), becomes a
    /// read-only view. New containers around them stay the caller's to change.
    pub(crate) fn protect_returned(&self, o: &ObjRef, v: Value) -> Value {
        if !matches!(v, Value::Arr(_) | Value::Rec(_)) {
            return v;
        }
        let Some(inst) = self.instances.get(&(o.class, o.id.clone())) else { return v };
        // Containers can refer to themselves in memory (`w.self = w`): each is visited once.
        type Seen = std::collections::HashSet<usize>;
        fn owned(v: &Value, out: &mut Seen) {
            match v {
                Value::Arr(a) if out.insert(Rc::as_ptr(a) as *const () as usize) => a.borrow().iter().for_each(|x| owned(x, out)),
                Value::Rec(r) if out.insert(Rc::as_ptr(r) as *const () as usize) => r.borrow().values().for_each(|x| owned(x, out)),
                _ => {}
            }
        }
        fn protect(v: Value, own: &Seen, seen: &mut Seen) -> Value {
            match &v {
                Value::Arr(a) if !a.readonly => {
                    let ptr = Rc::as_ptr(a) as *const () as usize;
                    if own.contains(&ptr) {
                        return v.read_only();
                    }
                    if seen.insert(ptr) {
                        let items: Vec<Value> = a.borrow().iter().cloned().collect();
                        *a.borrow_mut() = items.into_iter().map(|x| protect(x, own, seen)).collect();
                    }
                    v
                }
                Value::Rec(r) if !r.readonly => {
                    let ptr = Rc::as_ptr(r) as *const () as usize;
                    if own.contains(&ptr) {
                        return v.read_only();
                    }
                    if seen.insert(ptr) {
                        let fields: Vec<(Rc<str>, Value)> = r.borrow().iter().map(|(k, x)| (k.clone(), x.clone())).collect();
                        *r.borrow_mut() = fields.into_iter().map(|(k, x)| (k, protect(x, own, seen))).collect();
                    }
                    v
                }
                _ => v,
            }
        }
        let mut own = Seen::new();
        owned(&Value::Rec(inst.state.clone().into()), &mut own);
        protect(v, &own, &mut Seen::new())
    }

    pub(crate) fn set_obj_field(&mut self, this: Option<&ObjRef>, o: &Rc<ObjRef>, name: &str, v: Value) -> Result<(), Exc> {
        if this != Some(&**o) {
            return Err(Exc::msg(format!("only {}'s own methods can change its fields; call one of its methods instead", o.class_name)));
        }
        let state = self.inst(o).state.clone();
        let mut s = state.borrow_mut();
        match s.get_mut(name) {
            Some(slot) => *slot = v,
            None => return Err(Exc::msg(format!("{} has no field '{name}'; declare it in the object", o.class_name))),
        }
        drop(s);
        self.touch(o);
        Ok(())
    }

    /// `this.emit(name, data?)`
    pub(crate) fn emit_event(&mut self, o: &ObjRef, args: &[Value]) -> Result<(), Exc> {
        let Some(Value::Str(name)) = args.first() else {
            return Err(Exc::msg("emit(name, data) needs an event name"));
        };
        let data = args.get(1).unwrap_or(&Value::Null).to_json().map_err(|e| Exc::msg(format!("emit data: {e}")))?;
        self.outputs.push(Output::Emit { object: o.key(), name: name.to_string(), data });
        Ok(())
    }

    /// `this.schedule(delayMs, "method", ...args)`: a durable one-shot timer.
    /// `this.schedule(ms, "method", ...args)`: its handle, for `this.unschedule`.
    pub(crate) fn schedule_alarm(&mut self, o: &Rc<ObjRef>, args: &[Value], who: &Who) -> Result<Value, Exc> {
        let (Some(Value::Num(delay)), Some(Value::Str(method))) = (args.first(), args.get(1)) else {
            return Err(Exc::msg("schedule(delayMs, \"method\", ...args) needs a delay and a method name"));
        };
        if !delay.is_finite() || *delay < 0.0 {
            return Err(Exc::msg("schedule delay must be a non-negative number of milliseconds"));
        }
        self.method(o.class, method)?;
        let args = args[2..].iter().map(Value::to_json).collect::<Result<Vec<_>, _>>().map_err(|e| Exc::msg(format!("schedule arguments: {e}")))?;
        let alarm = Alarm { id: self.new_alarm_id(), at: self.now + delay, method: method.to_string(), args, caller: who.to_caller() };
        let handle = alarm.id;
        self.push_timer(alarm.at, TimerKind::Alarm { obj: o.clone(), alarm: alarm.id });
        self.inst_mut(o).alarms.push(alarm);
        self.touch(o);
        Ok(Value::Num(handle as f64))
    }

    /// `this.unschedule(handle)`: cancels the alarm; false when it already ran or never was.
    pub(crate) fn unschedule_alarm(&mut self, o: &Rc<ObjRef>, handle: &Value) -> Value {
        let Value::Num(h) = handle else { return Value::Bool(false) };
        let inst = self.inst_mut(o);
        let before = inst.alarms.len();
        inst.alarms.retain(|a| a.id as f64 != *h);
        let removed = inst.alarms.len() < before;
        if removed {
            // Its timer still fires, finds nothing and does nothing.
            self.touch(o);
        }
        Value::Bool(removed)
    }

    /// A handle no alarm had before, not even before a restart: at least the time in
    /// microseconds, as handles are saved with their alarms.
    fn new_alarm_id(&mut self) -> u64 {
        self.next_alarm = (self.next_alarm + 1).max((self.now * 1000.0) as u64);
        self.next_alarm
    }

    /// A loaded alarm keeps its handle; one saved before handles were saved gets one.
    fn keep_alarm_id(&mut self, a: &mut Alarm) {
        if a.id == 0 {
            a.id = self.new_alarm_id();
        } else {
            self.next_alarm = self.next_alarm.max(a.id);
        }
    }

    pub(crate) fn take_alarm(&mut self, o: &ObjRef, id: u64) -> Option<Alarm> {
        let inst = self.instances.get_mut(&(o.class, o.id.clone()))?;
        let i = inst.alarms.iter().position(|a| a.id == id)?;
        let a = inst.alarms.remove(i);
        self.touch(o);
        Some(a)
    }

    pub(crate) fn object_state(&self, o: &ObjRef) -> Result<Json, String> {
        Value::Rec(self.inst(o).state.clone().into()).to_json()
    }

    /// For a server resuming a client's stream: the current state of each of `objects` that
    /// changed after change number `since`, oldest change first. Unknown objects are skipped.
    pub fn changed_since(&mut self, objects: &[String], since: u64) -> Vec<Output> {
        let mut out = Vec::new();
        // A class stream (`Slot/*`) starts with the class's objects that changed: the most
        // recent CLASS_STREAM_CAP of them, so a client resuming never misses a new change.
        let mut keys: Vec<String> = Vec::new();
        let mut streamed: Vec<String> = Vec::new();
        for key in objects {
            match key.strip_suffix("/*").and_then(|c| self.program.object(c).map(|class| (c, class))) {
                Some((name, class)) => {
                    streamed.push(format!("{name}/"));
                    keys.extend(self.class_ids(class).into_iter().map(|id| format!("{name}/{id}")));
                }
                None => keys.push(key.clone()),
            }
        }
        keys.sort();
        keys.dedup();
        for key in &keys {
            let Ok(o) = self.resolve_object(key) else { continue };
            let seq = self.inst(&o).seq;
            if seq > since {
                if let Ok(state) = self.object_state(&o) {
                    out.push((seq, Output::Changed { object: o.key(), state, seq }));
                }
            }
        }
        out.sort_by_key(|(seq, _)| *seq);
        // Over the cap: drop the oldest changes of class streams, never a listed object.
        let in_class = |o: &Output| matches!(o, Output::Changed { object, .. } if streamed.iter().any(|p| object.starts_with(p.as_str())));
        let mut extra = out.iter().filter(|(_, o)| in_class(o)).count().saturating_sub(CLASS_STREAM_CAP);
        out.retain(|(_, o)| {
            let drop = extra > 0 && in_class(o);
            extra -= usize::from(drop);
            !drop
        });
        out.into_iter().map(|(_, o)| o).collect()
    }

    /// The ids of a class's objects: stored or loaded, never an erased person's blank, sorted.
    pub(crate) fn class_ids(&mut self, class: u16) -> Vec<String> {
        let prefix = format!("{}/", self.program.objects[class as usize].name);
        let mut ids: Vec<String> = self.store.keys(&prefix).unwrap_or_default().into_iter().filter_map(|k| k.strip_prefix(&prefix).map(str::to_string)).collect();
        ids.extend(self.instances.values().filter(|i| i.handle.class == class).map(|i| i.handle.id.to_string()));
        ids.sort();
        ids.dedup();
        ids.retain(|id| !self.store.is_erased(&format!("{prefix}{id}")).unwrap_or(true));
        ids
    }

    /// `Class.all()`: every object of the class. On a device, a server class's replicas, and
    /// the first call subscribes to the whole class, so the list fills as the server sends it.
    pub(crate) fn all_objects(&mut self, class: u16) -> Result<Vec<Rc<ObjRef>>, Exc> {
        let def = &self.program.objects[class as usize];
        if def.server && !self.server {
            if self.class_streams.insert(class) {
                let stream = format!("{}/*", def.name);
                // The replicas kept on the device come first, so the list shows offline too.
                for id in self.class_ids(class) {
                    self.instance(class, &id)?;
                }
                self.outputs.push(Output::Subscribe { object: stream, seq: 0 });
            }
            // A replica the server never sent (seq 0) is not a member: one left blank by a
            // change of user, or asked for by id and not there.
            let mut list: Vec<Rc<ObjRef>> = self.instances.values().filter(|i| i.handle.class == class && i.remote && i.seq > 0).map(|i| i.handle.clone()).collect();
            list.sort_by(|a, b| a.id.cmp(&b.id));
            return Ok(list);
        }
        let defaults = Value::Rec(self.defaults(class)?.into()).to_json().map_err(Exc::msg)?;
        let mut list = Vec::new();
        for id in self.class_ids(class) {
            let o = self.instance(class, &id)?;
            // An erased object is a blank with nothing in it: not a member any more.
            let blank = { let i = self.inst(&o); i.alarms.is_empty() && i.calls.is_empty() && i.created == 0.0 && Value::Rec(i.state.clone().into()).to_json().ok().as_ref() == Some(&defaults) };
            if !blank {
                list.push(o);
            }
        }
        Ok(list)
    }

    /// Every loaded object, for tools: personal data is always masked here.
    pub(crate) fn object_states(&self) -> Json {
        let state = |i: &Instance| {
            let mut s = Value::Rec(i.state.clone().into()).to_json().unwrap_or(Json::Null);
            self.mask(i.handle.class, &mut s);
            s
        };
        let mut list: Vec<(String, Json)> = self.instances.values().map(|i| (i.handle.key(), state(i))).collect();
        list.sort_by(|a, b| a.0.cmp(&b.0));
        Json::Array(list.into_iter().map(|(object, state)| json!({ "object": object, "state": state })).collect())
    }

    /// Gives an object a new state and drops its alarms and call records (erasure). A failed
    /// save puts all of it back (see `revert`).
    #[cfg(feature = "server")]
    pub(crate) fn reset(&mut self, o: &ObjRef, state: Rc<RefCell<Record>>) {
        let inst = self.inst_mut(o);
        inst.state = state;
        inst.calls.clear();
        inst.alarms.clear();
        // No clock until it holds something again, so a person who comes back starts afresh.
        inst.created = 0.0;
        inst.changed = 0.0;
        inst.cleared = true;
        self.touched.insert((o.class, o.id.clone()));
    }

    /// Whether `o` holds nothing: default state, no alarms, no call records.
    #[cfg(feature = "server")]
    pub(crate) fn is_blank(&self, o: &ObjRef, defaults: &Json) -> Result<bool, String> {
        let inst = self.inst(o);
        Ok(inst.alarms.is_empty() && inst.calls.is_empty() && &Value::Rec(inst.state.clone().into()).to_json()? == defaults)
    }

    /// Persists every object touched by the last input whose record changed. Whether every
    /// save worked.
    pub(crate) fn flush(&mut self) -> bool {
        let mut touched = std::mem::take(&mut self.touched);
        let mut index_changed = false;
        let mut seq_changed = false;
        let mut failed = false;
        // An object that now has alarms goes into the alarm index before it is saved, so a
        // restart always finds it. If the index cannot be saved, nothing is.
        let joining: Vec<String> = touched.iter().filter_map(|k| self.instances.get(k)).filter(|i| !i.alarms.is_empty()).map(|i| i.handle.key()).filter(|o| !self.alarm_index.contains(o)).collect();
        if !joining.is_empty() {
            self.alarm_index.extend(joining.iter().cloned());
            let text = serde_json::to_string(&self.alarm_index).expect("strings serialize");
            if let Err(e) = self.store.save(ALARM_INDEX_KEY, &text) {
                for o in &joining {
                    self.alarm_index.remove(o);
                }
                self.outputs.push(Output::Error { message: format!("could not save the alarm index: {e}"), trace: Vec::new() });
                for key in std::mem::take(&mut touched) {
                    self.revert(&key);
                }
                failed = true;
            }
        }
        for key in touched {
            let Some(inst) = self.instances.get(&key) else { continue };
            let object = inst.handle.key();
            let state = match Value::Rec(inst.state.clone().into()).to_json() {
                Ok(Json::Object(m)) => m,
                Ok(_) => unreachable!("state is a record"),
                Err(e) => {
                    self.outputs.push(Output::Error { message: format!("{object}: object state must be data ({e})"), trace: Vec::new() });
                    continue;
                }
            };
            let has_alarms = !inst.alarms.is_empty();
            let state_text = serde_json::to_string(&state).expect("JSON values serialize");
            let state_changed = state_text != inst.state_text;
            let remote = inst.remote;
            let mut record = Stored { state, alarms: inst.alarms.clone(), seq: inst.seq, calls: inst.calls.clone(), created: inst.created, changed: inst.changed };
            let mut text = serde_json::to_string(&record).expect("JSON values serialize");
            if text == inst.saved {
                continue;
            }
            // Alarms and call records are bookkeeping; only a new state is a change to share.
            if state_changed && !remote {
                // A new state of an object this runtime owns gets the next change number.
                self.change_seq += 1;
                seq_changed = true;
                record.seq = self.change_seq;
                if !inst.cleared {
                    record.changed = self.now;
                    if record.created <= 0.0 {
                        record.created = self.now;
                    }
                }
                text = serde_json::to_string(&record).expect("JSON values serialize");
            }
            if let Err(e) = self.store.save(&object, &text) {
                // The change did not happen: back to the last saved record, and the input's
                // answers become errors below, so nobody is told it worked.
                self.outputs.push(Output::Error { message: format!("{object}: could not save: {e}"), trace: Vec::new() });
                self.revert(&key);
                failed = true;
                continue;
            }
            if let Some(inst) = self.instances.get_mut(&key) {
                inst.saved = text;
                inst.seq = record.seq;
                inst.state_text = state_text;
                inst.changed = record.changed;
                inst.created = record.created;
                inst.cleared = false;
            }
            // Objects joined the index above; leaving it can wait, as a stale entry only loads
            // an object without alarms at boot.
            index_changed |= !has_alarms && self.alarm_index.remove(&object);
            if state_changed {
                self.outputs.push(Output::Changed { object, state: Json::Object(record.state), seq: record.seq });
            }
        }
        if failed {
            for o in &mut self.outputs {
                if let Output::Reply { ok, error, status, .. } = o {
                    if error.is_none() {
                        *ok = None;
                        *error = Some("the change could not be saved; try again".into());
                        *status = Some(503);
                    }
                }
            }
        }
        if seq_changed {
            if let Err(e) = self.store.save(CHANGE_SEQ_KEY, &self.change_seq.to_string()) {
                self.outputs.push(Output::Error { message: format!("could not save the change number: {e}"), trace: Vec::new() });
            }
        }
        if index_changed {
            let text = serde_json::to_string(&self.alarm_index).expect("strings serialize");
            if let Err(e) = self.store.save(ALARM_INDEX_KEY, &text) {
                self.outputs.push(Output::Error { message: format!("could not save the alarm index: {e}"), trace: Vec::new() });
            }
        }
        !failed
    }

    /// Puts an object's state, call records and alarms back to its last saved record, after a
    /// failed save. An alarm scheduled by the failed call does not fire; one that fired in it
    /// runs again, as its work was undone too. Timers left from the failed call find no alarm
    /// with their id and do nothing.
    fn revert(&mut self, key: &(u16, Rc<str>)) {
        let Some(inst) = self.instances.get(key) else { return };
        let (class, saved) = (inst.handle.class, inst.saved.clone());
        let fresh = match self.defaults(class) {
            Ok(f) => f,
            Err(_) => return,
        };
        let stored: Option<Stored> = serde_json::from_str(&saved).ok();
        {
            let mut s = fresh.borrow_mut();
            for (k, v) in stored.iter().flat_map(|st| st.state.iter()) {
                if let Some(slot) = s.get_mut(k.as_str()) {
                    *slot = Value::from_json(v);
                }
            }
        }
        let (calls, mut alarms) = stored.map(|s| (s.calls, s.alarms)).unwrap_or_default();
        let Some(inst) = self.instances.get_mut(key) else { return };
        let restored = fresh.borrow().clone();
        *inst.state.borrow_mut() = restored;
        inst.calls = calls;
        inst.state_text = Value::Rec(inst.state.clone().into()).to_json().map(|j| j.to_string()).unwrap_or_default();
        let handle = inst.handle.clone();
        for a in &mut alarms {
            self.keep_alarm_id(a);
            self.push_timer(a.at, TimerKind::Alarm { obj: handle.clone(), alarm: a.id });
        }
        if let Some(inst) = self.instances.get_mut(key) {
            inst.alarms = alarms;
        }
        self.ran_code = true;
    }

    /// Loads instances that have timers: every stored instance of a type with `@every`
    /// methods, and every instance with pending alarms.
    pub(crate) fn restore(&mut self) -> Result<(), BootError> {
        let boot_err = |e: Exc| BootError { message: e.message(), trace: Vec::new() };
        // Change numbers start from the clock (x1000), so they keep growing even if the stored
        // counter is lost, as long as fewer than 1000 changes happen per millisecond.
        let read = |e: String| BootError { message: format!("could not read the store: {e}"), trace: Vec::new() };
        let stored_seq = self.store.load(CHANGE_SEQ_KEY).map_err(read)?.and_then(|s| s.parse().ok()).unwrap_or(0);
        self.change_seq = stored_seq.max((self.now.max(0.0) as u64).saturating_mul(1000));
        if let Some(text) = self.store.load(ALARM_INDEX_KEY).map_err(read)? {
            // Without it, sleeping objects' alarms would silently never fire.
            self.alarm_index = serde_json::from_str(&text).map_err(|e| read(format!("{ALARM_INDEX_KEY} is damaged ({e}); restore it from a backup")))?;
        }
        let mut keys: Vec<String> = self.alarm_index.iter().cloned().collect();
        let program = self.program.clone();
        for def in program.objects.iter().filter(|d| d.methods.values().any(|m| m.every.is_some())) {
            keys.extend(self.store.keys(&format!("{}/", def.name)).map_err(read)?);
        }
        for key in keys {
            let Some((class, id)) = key.split_once('/') else { continue };
            if let Some(c) = program.object(class) {
                self.instance(c, id).map_err(boot_err)?;
            }
        }
        Ok(())
    }
}

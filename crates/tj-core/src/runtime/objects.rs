//! Durable objects: loading, field access rules, events, alarms and persistence.

use super::protocol::Output;
use super::{BootError, Runtime, TimerKind};
use crate::vm::interp::Fiber;
use crate::vm::value::{Closure, Exc, ObjRef, Record, Value};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value as Json};
use std::cell::RefCell;
use std::rc::Rc;

const ALARM_INDEX_KEY: &str = "tj:alarms";
const MAX_ID_LEN: usize = 128;

pub(crate) struct Instance {
    handle: Rc<ObjRef>,
    state: Rc<RefCell<Record>>,
    alarms: Vec<Alarm>,
    /// The last persisted record, to skip unchanged writes.
    saved: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct Alarm {
    #[serde(skip)]
    id: u64,
    at: f64,
    pub method: String,
    pub args: Vec<Json>,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    state: Map<String, Json>,
    #[serde(default)]
    alarms: Vec<Alarm>,
}

impl Runtime {
    /// Returns the handle of `Class/id`, loading or creating the instance.
    pub(crate) fn instance(&mut self, class: u16, id: &str) -> Result<Rc<ObjRef>, Exc> {
        if id.is_empty() || id.len() > MAX_ID_LEN || id.contains('/') || id.chars().any(char::is_control) {
            return Err(Exc::msg(format!("invalid object id {id:?}: use 1-{MAX_ID_LEN} characters without '/'")));
        }
        let key = (class, Rc::<str>::from(id));
        if let Some(inst) = self.instances.get(&key) {
            return Ok(inst.handle.clone());
        }
        let program = self.program.clone();
        let def = &program.objects[class as usize];
        let handle = Rc::new(ObjRef { class, class_name: def.name.clone(), id: key.1.clone() });
        let init = Rc::new(Closure { proto: def.init, upvals: Vec::new(), this: None });
        let f = self.fiber_for(&Value::Func(init), Vec::new())?;
        let Value::Rec(state) = self.run_sync(f).map_err(|fail| Exc::msg(fail.message))? else {
            unreachable!("object init returns a record")
        };
        let mut alarms = Vec::new();
        let mut saved = String::new();
        if let Some(text) = self.store.load(&handle.key()) {
            let stored: Stored = serde_json::from_str(&text).map_err(|e| Exc::msg(format!("{} has unreadable stored state: {e}", handle.key())))?;
            // Stored fields override defaults; fields no longer declared are dropped.
            let mut s = state.borrow_mut();
            for (k, v) in &stored.state {
                if let Some(slot) = s.get_mut(k.as_str()) {
                    *slot = Value::from_json(v);
                }
            }
            alarms = stored.alarms;
            saved = text;
        }
        for (name, m) in &def.methods {
            if let Some(interval) = m.every {
                let kind = TimerKind::Every { obj: handle.clone(), method: name.clone(), interval };
                self.push_timer(self.now + interval, kind);
            }
        }
        for a in &mut alarms {
            self.next_alarm += 1;
            a.id = self.next_alarm;
            self.push_timer(a.at, TimerKind::Alarm { obj: handle.clone(), alarm: a.id });
        }
        self.instances.insert(key, Instance { handle: handle.clone(), state, alarms, saved });
        Ok(handle)
    }

    /// Parses `"Class/id"` and loads the instance.
    pub(crate) fn resolve_object(&mut self, key: &str) -> Result<Rc<ObjRef>, String> {
        let (class, id) = key.split_once('/').ok_or_else(|| format!("object must be \"Class/id\", got {key:?}"))?;
        let c = self.program.object(class).ok_or_else(|| format!("no object type named {class}"))?;
        self.instance(c, id).map_err(|e| e.message())
    }

    fn inst(&self, o: &ObjRef) -> &Instance {
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
        self.touch(&obj);
        let mut f = Fiber::default();
        f.stack.push(Value::Obj(obj.clone()));
        f.stack.extend(args);
        self.push_frame(&mut f, closure, 0, 0, Some(obj))?;
        Ok(f)
    }

    pub(crate) fn touch(&mut self, o: &ObjRef) {
        self.touched.insert((o.class, o.id.clone()));
    }

    pub(crate) fn obj_field(&mut self, o: &Rc<ObjRef>, name: &str) -> Result<Value, Exc> {
        if name == "id" {
            return Ok(Value::Str(o.id.clone()));
        }
        let v = self.inst(o).state.borrow().get(name).cloned();
        match v {
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
    pub(crate) fn schedule_alarm(&mut self, o: &Rc<ObjRef>, args: &[Value]) -> Result<(), Exc> {
        let (Some(Value::Num(delay)), Some(Value::Str(method))) = (args.first(), args.get(1)) else {
            return Err(Exc::msg("schedule(delayMs, \"method\", ...args) needs a delay and a method name"));
        };
        if !delay.is_finite() || *delay < 0.0 {
            return Err(Exc::msg("schedule delay must be a non-negative number of milliseconds"));
        }
        self.method(o.class, method)?;
        let args = args[2..].iter().map(Value::to_json).collect::<Result<Vec<_>, _>>().map_err(|e| Exc::msg(format!("schedule arguments: {e}")))?;
        self.next_alarm += 1;
        let alarm = Alarm { id: self.next_alarm, at: self.now + delay, method: method.to_string(), args };
        self.push_timer(alarm.at, TimerKind::Alarm { obj: o.clone(), alarm: alarm.id });
        self.inst_mut(o).alarms.push(alarm);
        self.touch(o);
        Ok(())
    }

    pub(crate) fn take_alarm(&mut self, o: &ObjRef, id: u64) -> Option<Alarm> {
        let inst = self.instances.get_mut(&(o.class, o.id.clone()))?;
        let i = inst.alarms.iter().position(|a| a.id == id)?;
        let a = inst.alarms.remove(i);
        self.touch(o);
        Some(a)
    }

    pub(crate) fn object_state(&self, o: &ObjRef) -> Result<Json, String> {
        Value::Rec(self.inst(o).state.clone()).to_json()
    }

    pub(crate) fn object_states(&self) -> Json {
        let mut list: Vec<(String, Json)> = self.instances.values().map(|i| (i.handle.key(), Value::Rec(i.state.clone()).to_json().unwrap_or(Json::Null))).collect();
        list.sort_by(|a, b| a.0.cmp(&b.0));
        Json::Array(list.into_iter().map(|(object, state)| json!({ "object": object, "state": state })).collect())
    }

    /// Persists every object touched by the last input whose record changed.
    pub(crate) fn flush(&mut self) {
        let touched = std::mem::take(&mut self.touched);
        let mut index_changed = false;
        for key in touched {
            let Some(inst) = self.instances.get(&key) else { continue };
            let object = inst.handle.key();
            let state = match Value::Rec(inst.state.clone()).to_json() {
                Ok(Json::Object(m)) => m,
                Ok(_) => unreachable!("state is a record"),
                Err(e) => {
                    self.outputs.push(Output::Error { message: format!("{object}: object state must be data ({e})"), trace: Vec::new() });
                    continue;
                }
            };
            let has_alarms = !inst.alarms.is_empty();
            let record = Stored { state, alarms: inst.alarms.clone() };
            let text = serde_json::to_string(&record).expect("JSON values serialize");
            if text == inst.saved {
                continue;
            }
            if let Err(e) = self.store.save(&object, &text) {
                self.outputs.push(Output::Error { message: format!("{object}: could not save: {e}"), trace: Vec::new() });
                continue;
            }
            if let Some(inst) = self.instances.get_mut(&key) {
                inst.saved = text;
            }
            index_changed |= if has_alarms { self.alarm_index.insert(object.clone()) } else { self.alarm_index.remove(&object) };
            self.outputs.push(Output::Changed { object, state: Json::Object(record.state) });
        }
        if index_changed {
            let text = serde_json::to_string(&self.alarm_index).expect("strings serialize");
            if let Err(e) = self.store.save(ALARM_INDEX_KEY, &text) {
                self.outputs.push(Output::Error { message: format!("could not save the alarm index: {e}"), trace: Vec::new() });
            }
        }
    }

    /// Loads instances that have timers: every stored instance of a type with `@every`
    /// methods, and every instance with pending alarms.
    pub(crate) fn restore(&mut self) -> Result<(), BootError> {
        let boot_err = |e: Exc| BootError { message: e.message(), trace: Vec::new() };
        if let Some(text) = self.store.load(ALARM_INDEX_KEY) {
            self.alarm_index = serde_json::from_str(&text).unwrap_or_default();
        }
        let mut keys: Vec<String> = self.alarm_index.iter().cloned().collect();
        let program = self.program.clone();
        for def in program.objects.iter().filter(|d| d.methods.values().any(|m| m.every.is_some())) {
            keys.extend(self.store.keys(&format!("{}/", def.name)));
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

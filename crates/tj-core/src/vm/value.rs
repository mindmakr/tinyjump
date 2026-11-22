//! Runtime values.

use crate::runtime::Runtime;
use indexmap::IndexMap;
use serde_json::{Map, Number, Value as Json};
use std::cell::RefCell;
use std::rc::Rc;

pub type Record = IndexMap<Rc<str>, Value>;

/// An array or record, shared by everything that holds it. `readonly` when it was reached
/// from outside the object whose field it is: only that object's own methods may change
/// it. Reading is never restricted, and costs nothing extra.
#[derive(Clone)]
pub struct Shared<T> {
    cell: Rc<RefCell<T>>,
    pub readonly: bool,
}

impl<T> Shared<T> {
    pub fn new(v: T) -> Shared<T> {
        Shared { cell: Rc::new(RefCell::new(v)), readonly: false }
    }

    /// The same container, read-only.
    pub fn read_only(&self) -> Shared<T> {
        Shared { cell: self.cell.clone(), readonly: true }
    }

    /// For changing it in place: refused when it belongs to another object.
    pub fn writable(&self) -> Result<&Rc<RefCell<T>>, Exc> {
        if self.readonly {
            return Err(Exc::msg("this belongs to another object's field, and only that object's own methods can change it; call one of its methods, or change a copy (.slice(), [...a] or { ...r })"));
        }
        Ok(&self.cell)
    }
}

impl<T> std::ops::Deref for Shared<T> {
    type Target = Rc<RefCell<T>>;
    fn deref(&self) -> &Rc<RefCell<T>> {
        &self.cell
    }
}

impl<T> From<Rc<RefCell<T>>> for Shared<T> {
    fn from(cell: Rc<RefCell<T>>) -> Shared<T> {
        Shared { cell, readonly: false }
    }
}
pub type NativeFn = fn(&mut Runtime, &[Value]) -> Result<Value, Exc>;

#[derive(Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(Rc<str>),
    Arr(Shared<Vec<Value>>),
    Rec(Shared<Record>),
    Func(Rc<Closure>),
    Native(Native),
    /// A handle to a durable object instance.
    Obj(Rc<ObjRef>),
    /// An object declaration, e.g. `Counter` in `Counter.get("main")`.
    Class(u16),
    /// A UI element built by JSX.
    Elem(Rc<Elem>),
    /// The pending result of an effect; `await` suspends on it.
    Promise(u32),
    /// A boxed local captured by a closure. Never visible to programs.
    Cell(Rc<RefCell<Value>>),
}

pub struct Closure {
    pub proto: u32,
    pub upvals: Vec<Rc<RefCell<Value>>>,
    /// The object whose method created this closure. It may change that object's
    /// fields and call its private methods, like the method itself.
    pub this: Option<Rc<ObjRef>>,
}

#[derive(Clone, Copy)]
pub struct Native {
    pub name: &'static str,
    pub f: NativeFn,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjRef {
    pub class: u16,
    pub class_name: Rc<str>,
    pub id: Rc<str>,
}

impl ObjRef {
    /// Storage and protocol key: `Class/id`.
    pub fn key(&self) -> String {
        format!("{}/{}", self.class_name, self.id)
    }
}

pub struct Elem {
    pub kind: Rc<str>,
    pub props: Vec<(Rc<str>, Value)>,
    pub children: Vec<Value>,
}

/// A thrown value. Runtime errors are records `{ name: "Error", message }`, like `new Error(msg)`.
pub struct Exc(pub Value, pub ExcKind);

/// Classifies how an exception originated: set only by the runtime, so app code cannot
/// forge an HTTP status or hide a VM crash.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExcKind {
    /// Explicit user `throw`, `check(ok, field, msg)`, or effect/plugin error (400).
    #[default]
    User,
    /// Access/privacy refusal (`Exc::refused`, 403).
    Refused,
    /// Internal VM/runtime fault (null deref, bad call, stack overflow, runaway: 500 on server).
    Fault,
}

impl Exc {
    pub fn msg(message: impl Into<String>) -> Exc {
        Exc(error_record(&message.into()), ExcKind::Fault)
    }

    /// An error with an optional `field` (from `check(ok, field, message)` or effect failure).
    pub fn with_field(message: impl Into<String>, field: Option<String>) -> Exc {
        let e = error_record(&message.into());
        if let (Some(field), Value::Rec(r)) = (field, &e) {
            r.borrow_mut().insert("field".into(), Value::Str(Rc::from(field.as_str())));
        }
        Exc(e, ExcKind::User)
    }

    /// The `field` named by `check(ok, field, message)`, if any.
    pub fn field(&self) -> Option<String> {
        let Value::Rec(r) = &self.0 else { return None };
        match r.borrow().get("field") {
            Some(Value::Str(f)) if !f.is_empty() => Some(f.to_string()),
            _ => None,
        }
    }

    /// An error that refuses the request, answered with 403 when nothing catches it.
    pub fn refused(message: impl Into<String>) -> Exc {
        Exc(error_record(&message.into()), ExcKind::Refused)
    }

    /// The status a request fails with when nothing catches this error (see [`Exc::refused`]).
    pub fn status(&self) -> Option<u16> {
        (self.1 == ExcKind::Refused).then_some(403)
    }

    /// Whether this exception is an internal VM/runtime fault rather than a user `throw`/`check`.
    pub fn is_fault(&self) -> bool {
        self.1 == ExcKind::Fault
    }

    pub fn message(&self) -> String {
        if let Value::Rec(r) = &self.0 {
            if let Some(Value::Str(m)) = r.borrow().get("message") {
                return m.to_string();
            }
        }
        format!("uncaught {}", self.0.display())
    }
}

pub fn error_record(message: &str) -> Value {
    let mut r = Record::new();
    r.insert("name".into(), Value::str("Error"));
    r.insert("message".into(), Value::str(message));
    Value::rec(r)
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::Str(Rc::from(s))
    }

    pub fn arr(v: Vec<Value>) -> Value {
        Value::Arr(Shared::new(v))
    }

    pub fn rec(r: Record) -> Value {
        Value::Rec(Shared::new(r))
    }

    /// The value as seen from outside its object: containers become read-only.
    pub fn read_only(&self) -> Value {
        match self {
            Value::Arr(a) => Value::Arr(a.read_only()),
            Value::Rec(r) => Value::Rec(r.read_only()),
            v => v.clone(),
        }
    }

    /// `v`, read out of a container: what is inside a read-only container is read-only too.
    pub fn inside(readonly: bool, v: Value) -> Value {
        if readonly {
            v.read_only()
        } else {
            v
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0 && !n.is_nan(),
            Value::Str(s) => !s.is_empty(),
            _ => true,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Num(_) => "number",
            Value::Str(_) => "string",
            Value::Arr(_) => "array",
            Value::Rec(_) => "record",
            Value::Func(_) | Value::Native(_) => "function",
            Value::Obj(_) => "object",
            Value::Class(_) => "object type",
            Value::Elem(_) => "element",
            Value::Promise(_) => "promise",
            Value::Cell(_) => "cell",
        }
    }

    /// `===`: by value for primitives and object handles, by identity for arrays, records and functions.
    pub fn strict_eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Num(a), Value::Num(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Arr(a), Value::Arr(b)) => Rc::ptr_eq(a, b),
            (Value::Rec(a), Value::Rec(b)) => Rc::ptr_eq(a, b),
            (Value::Func(a), Value::Func(b)) => Rc::ptr_eq(a, b),
            (Value::Native(a), Value::Native(b)) => a.name == b.name,
            (Value::Obj(a), Value::Obj(b)) => a == b,
            (Value::Class(a), Value::Class(b)) => a == b,
            (Value::Elem(a), Value::Elem(b)) => Rc::ptr_eq(a, b),
            (Value::Promise(a), Value::Promise(b)) => a == b,
            _ => false,
        }
    }

    /// Text form used by template strings, string `+` and `String(x)`.
    /// Arrays and records render as JSON, which is more useful than JavaScript's `[object Object]`.
    pub fn display(&self) -> String {
        match self {
            Value::Null => "null".into(),
            Value::Bool(b) => b.to_string(),
            Value::Num(n) => fmt_num(*n),
            Value::Str(s) => s.to_string(),
            Value::Arr(_) | Value::Rec(_) => self.to_json().map(|j| j.to_string()).unwrap_or_else(|e| format!("[{e}]")),
            Value::Func(_) | Value::Native(_) => "[function]".into(),
            Value::Obj(o) => o.key(),
            Value::Class(_) => "[object type]".into(),
            Value::Elem(e) => format!("<{}>", e.kind),
            Value::Promise(_) => "[promise]".into(),
            Value::Cell(c) => c.borrow().display(),
        }
    }

    /// Converts data to JSON. Fails on functions and other non-data values.
    pub fn to_json(&self) -> Result<Json, String> {
        Ok(match self {
            Value::Null => Json::Null,
            Value::Bool(b) => Json::Bool(*b),
            Value::Num(n) => Number::from_f64(*n).map_or(Json::Null, |num| {
                // Keep integers as integers in JSON.
                if n.fract() == 0.0 && n.abs() < 9.0e15 { Json::from(*n as i64) } else { Json::Number(num) }
            }),
            Value::Str(s) => Json::String(s.to_string()),
            Value::Arr(a) => Json::Array(a.borrow().iter().map(Value::to_json).collect::<Result<_, _>>()?),
            Value::Rec(r) => {
                let mut m = Map::new();
                for (k, v) in r.borrow().iter() {
                    m.insert(k.to_string(), v.to_json()?);
                }
                Json::Object(m)
            }
            Value::Obj(o) => {
                let mut m = Map::new();
                m.insert("$object".into(), Json::String(o.key()));
                Json::Object(m)
            }
            other => return Err(format!("a {} is not data", other.type_name())),
        })
    }

    pub fn from_json(j: &Json) -> Value {
        match j {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Number(n) => Value::Num(n.as_f64().unwrap_or(f64::NAN)),
            Json::String(s) => Value::str(s),
            Json::Array(a) => Value::arr(a.iter().map(Value::from_json).collect()),
            Json::Object(m) => Value::rec(m.iter().map(|(k, v)| (Rc::from(k.as_str()), Value::from_json(v))).collect()),
        }
    }
}

/// Formats a number the way JavaScript does for the common cases.
pub fn fmt_num(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.is_infinite() {
        if n > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else if n == 0.0 {
        "0".into()
    } else if n.fract() == 0.0 && n.abs() < 1e21 {
        format!("{n:.0}")
    } else {
        format!("{n}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_format_like_js() {
        assert_eq!(fmt_num(3.0), "3");
        assert_eq!(fmt_num(-0.0), "0");
        assert_eq!(fmt_num(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(fmt_num(1e20), "100000000000000000000");
    }

    #[test]
    fn json_round_trip() {
        let j: Json = serde_json::json!({"a": [1, 2.5, "x", null, true], "b": {"c": 1}});
        assert_eq!(Value::from_json(&j).to_json().unwrap(), j);
    }
}

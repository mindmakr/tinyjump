//! Bytecode: the compiled form of a program.

use super::value::Value;
use indexmap::IndexMap;
use std::rc::Rc;

/// Stack-machine instructions. Operands index into the program's constant, prototype
/// and element tables, a frame's local slots, or the closure's captured cells.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    /// `@needs("consent:<purpose>")` at the start of a method: pops `this` and throws a
    /// refusal unless the person it belongs to has consented (tj:consent).
    NeedsConsent(u32),
    Const(u32),
    Null,
    True,
    False,
    Pop,
    Dup,
    /// `a b` -> `a b a b`
    Dup2,
    LoadLocal(u16),
    /// Stores the top of the stack and leaves it there.
    StoreLocal(u16),
    /// Moves the value in a local slot into a fresh cell, so closures can share it.
    MakeCell(u16),
    LoadCell(u16),
    StoreCell(u16),
    /// Replaces a cell with a fresh one holding the same value (per-iteration loop bindings).
    Recell(u16),
    LoadUpval(u16),
    StoreUpval(u16),
    LoadGlobal(u32),
    StoreGlobal(u32),
    /// `obj` -> `obj.name` (name is a string constant).
    GetField(u32),
    /// `obj value` -> `value`
    SetField(u32),
    /// `obj index` -> `obj[index]`
    GetIndex,
    /// `obj index value` -> `value`
    SetIndex,
    NewArr,
    /// `arr value` -> `arr`
    ArrPush,
    /// `arr other` -> `arr` with `other`'s items appended.
    ArrSpread,
    NewRec,
    /// `rec value` -> `rec`
    RecSet(u32),
    /// `rec key value` -> `rec`
    RecSetDyn,
    /// `rec other` -> `rec` with `other`'s fields copied in.
    RecSpread,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Neg,
    Not,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// Pops n values and pushes their concatenated text.
    Concat(u16),
    /// Length of an array or string.
    Len,
    Jump(u32),
    /// Pops the condition.
    JumpIfFalse(u32),
    /// If the top is falsy, jump and keep it; otherwise pop it.
    JumpIfFalseKeep(u32),
    /// If the top is truthy, jump and keep it; otherwise pop it.
    JumpIfTrueKeep(u32),
    /// If the top is not null, jump and keep it; otherwise pop it.
    JumpIfNotNullKeep(u32),
    /// If the top is null, jump and keep it (optional chaining).
    JumpIfNullKeep(u32),
    /// `callee args..` -> `result`
    Call(u8),
    /// `receiver args..` -> `result`
    CallMethod(u32, u8),
    Return,
    Closure(u32),
    /// Builds a UI element from the element descriptor: `props.. children..` -> `elem`.
    Elem(u32),
    Await,
    /// Pushes the `caller` record of the running call.
    Caller,
    Throw,
    /// Installs an exception handler that jumps to the operand.
    TryBegin(u32),
    TryEnd,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Capture {
    /// A cell in the enclosing function's local slot.
    Local(u16),
    /// A cell the enclosing function itself captured.
    Upval(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    User,
    Prelude,
}

#[derive(Debug)]
pub struct Proto {
    pub name: Rc<str>,
    /// Parameter slots, including `this` for methods.
    pub params: u16,
    pub locals: u16,
    pub code: Vec<Op>,
    /// Source byte offset of each instruction, for error traces.
    pub offsets: Vec<u32>,
    pub captures: Vec<Capture>,
    pub source: Source,
}

#[derive(Debug)]
pub struct ElemDesc {
    pub kind: Rc<str>,
    pub props: Vec<Rc<str>>,
    pub children: u16,
}

#[derive(Debug)]
pub struct MethodDef {
    pub proto: u32,
    pub private: bool,
    /// `@every("…")` interval in milliseconds.
    pub every: Option<f64>,
    /// `@allow(...)`: who may call it remotely; `None` means nobody (see `runtime/access.rs`).
    pub allow: Option<Rc<[Rc<str>]>>,
    /// `@exclusive`: one exclusive call at a time per object (see `runtime/exclusive.rs`).
    pub exclusive: bool,
    /// Its parameters: name, whether it may be left out, and type. A call from outside (the
    /// HTTP API, another device) is checked against them before the method runs.
    pub params: Vec<(Rc<str>, bool, ArgTy)>,
}

/// A parameter's declared type, as far as a call from outside can be checked: JSON shapes.
#[derive(Debug, Clone, PartialEq)]
pub enum ArgTy {
    Any,
    Num,
    Str,
    Bool,
    Null,
    StrLit(Rc<str>),
    NumLit(f64),
    Arr(Box<ArgTy>),
    /// Fields: name, optional, type.
    Rec(Rc<[(Rc<str>, bool, ArgTy)]>),
    /// `Record<string, T>`.
    Dict(Box<ArgTy>),
    Union(Vec<ArgTy>),
}

impl ArgTy {
    fn fits(&self, v: &serde_json::Value) -> bool {
        use serde_json::Value as J;
        match (self, v) {
            (ArgTy::Any, _) | (ArgTy::Num, J::Number(_)) | (ArgTy::Str, J::String(_)) | (ArgTy::Bool, J::Bool(_)) | (ArgTy::Null, J::Null) => true,
            (ArgTy::StrLit(s), J::String(x)) => **s == **x,
            (ArgTy::NumLit(n), J::Number(x)) => x.as_f64() == Some(*n),
            (ArgTy::Arr(t), J::Array(xs)) => xs.iter().all(|x| t.fits(x)),
            (ArgTy::Rec(fields), J::Object(m)) => fields.iter().all(|(k, opt, t)| m.get(&**k).map_or(*opt, |x| (*opt && x.is_null()) || t.fits(x))) && m.keys().all(|k| fields.iter().any(|(f, _, _)| **f == **k)),
            (ArgTy::Dict(t), J::Object(m)) => m.values().all(|x| t.fits(x)),
            (ArgTy::Union(ts), x) => ts.iter().any(|t| t.fits(x)),
            _ => false,
        }
    }

    /// How a person would name the type, for errors.
    pub fn describe(&self) -> String {
        match self {
            ArgTy::Any => "anything".into(),
            ArgTy::Num => "a number".into(),
            ArgTy::Str => "text".into(),
            ArgTy::Bool => "true or false".into(),
            ArgTy::Null => "null".into(),
            ArgTy::StrLit(s) => format!("\"{s}\""),
            ArgTy::NumLit(n) => n.to_string(),
            ArgTy::Arr(t) => format!("a list of {}", t.describe()),
            ArgTy::Rec(fields) => format!("a record with {}", fields.iter().map(|(k, o, _)| if *o { format!("{k}?") } else { k.to_string() }).collect::<Vec<_>>().join(", ")),
            ArgTy::Dict(t) => format!("a record of {}", t.describe()),
            ArgTy::Union(ts) => ts.iter().map(ArgTy::describe).collect::<Vec<_>>().join(" or "),
        }
    }
}

/// Whether a call from outside gives `args` that fit the method's parameters; the reason if
/// not, naming the parameter.
pub fn check_args(params: &[(Rc<str>, bool, ArgTy)], args: &[serde_json::Value]) -> Result<(), String> {
    if args.len() > params.len() {
        let names: Vec<&str> = params.iter().map(|(n, _, _)| &**n).collect();
        return Err(format!("it takes {} argument(s) ({}), got {}", params.len(), names.join(", "), args.len()));
    }
    for (i, (name, optional, ty)) in params.iter().enumerate() {
        match args.get(i) {
            None if *optional => {}
            None => return Err(format!("`{name}` is missing")),
            Some(v) if *optional && v.is_null() => {}
            Some(v) if !ty.fits(v) => {
                let got = match v {
                    serde_json::Value::String(_) => "text".to_string(),
                    serde_json::Value::Number(_) => "a number".into(),
                    serde_json::Value::Bool(_) => "true or false".into(),
                    serde_json::Value::Null => "null".into(),
                    serde_json::Value::Array(_) => "a list".into(),
                    serde_json::Value::Object(_) => "a record".into(),
                };
                return Err(format!("`{name}` must be {}, got {got}: {v}", ty.describe()));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

#[derive(Debug)]
pub struct ObjectDef {
    pub name: Rc<str>,
    pub fields: Vec<Rc<str>>,
    /// Builds the default state record.
    pub init: u32,
    pub methods: IndexMap<Rc<str>, MethodDef>,
    /// `@server`: lives on the server; device runtimes hold a synced replica.
    pub server: bool,
    /// `@read(...)`: who may read it remotely; `None` means nobody.
    pub read: Option<Rc<[Rc<str>]>>,
    /// `@pii(category, purpose, ...)` fields (docs/privacy.md).
    pub pii: Vec<Pii>,
    /// `@retain(duration, "created" | "changed")`: objects are erased this long after they
    /// were created or last changed.
    pub retain: Option<Retain>,
    /// `@legalHold(duration)`: objects are kept this long after their last change, even when
    /// erased or past retention.
    pub hold: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
pub struct Retain {
    pub ms: f64,
    /// Counted from creation rather than from the last change.
    pub from_created: bool,
}

/// A field that holds personal data about the object's subject: the person whose id is the
/// object id.
#[derive(Debug, Clone)]
pub struct Pii {
    pub field: Rc<str>,
    pub category: Rc<str>,
    pub purposes: Rc<[Rc<str>]>,
}

/// Personal-data categories, after GDPR articles 9 and 10 and the Saudi PDPL.
pub const PII_CATEGORIES: &[&str] = &["contact", "identifier", "online-id", "location", "financial", "health", "genetic", "biometric", "ethnicity", "religion", "politics", "union", "sexual", "criminal", "other"];

/// The special categories, with stricter rules: GDPR article 9 (and the PDPL's sensitive
/// data), and criminal records under article 10.
pub const SPECIAL_CATEGORIES: &[&str] = &["health", "genetic", "biometric", "ethnicity", "religion", "politics", "union", "sexual", "criminal"];

impl ObjectDef {
    pub fn pii(&self, field: &str) -> Option<&Pii> {
        self.pii.iter().find(|p| &*p.field == field)
    }

    /// Whether its objects can belong to a person (the one whose id is the object id): it
    /// holds `@pii`, or has an `"owner"` rule. Erasure and per-person keys cover these.
    pub fn belongs_to_a_person(&self) -> bool {
        let owner = |r: &Option<Rc<[Rc<str>]>>| r.as_ref().is_some_and(|r| r.iter().any(|w| &**w == "owner"));
        !self.pii.is_empty() || owner(&self.read) || self.methods.values().any(|m| owner(&m.allow))
    }
}

pub struct Program {
    pub protos: Vec<Proto>,
    pub consts: Vec<Value>,
    pub elems: Vec<ElemDesc>,
    pub objects: Vec<ObjectDef>,
    /// Global slot names, in slot order: builtins, then prelude, then user globals.
    pub globals: Vec<Rc<str>>,
    /// Run top-level declarations: the prelude's, then the user's.
    pub main: Vec<u32>,
    /// Global slot of `function App()`, the UI root, if the program has one.
    pub app: Option<u32>,
    pub user_source: Rc<str>,
}

impl Program {
    pub fn global(&self, name: &str) -> Option<u32> {
        self.globals.iter().position(|g| &**g == name).map(|i| i as u32)
    }

    pub fn object(&self, name: &str) -> Option<u16> {
        self.objects.iter().position(|o| &*o.name == name).map(|i| i as u16)
    }
}

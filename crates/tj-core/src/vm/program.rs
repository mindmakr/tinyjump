//! Bytecode: the compiled form of a program.

use super::value::Value;
use indexmap::IndexMap;
use std::rc::Rc;

/// Stack-machine instructions. Operands index into the program's constant, prototype
/// and element tables, a frame's local slots, or the closure's captured cells.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
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

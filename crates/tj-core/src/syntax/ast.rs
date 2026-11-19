//! Abstract syntax tree. Type annotations are kept for the upcoming type checker.

use super::Span;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub struct Program {
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub enum Item {
    Function(Rc<FnDecl>),
    Object(ObjectDecl),
    TypeAlias { name: String, generics: Vec<String>, ty: TypeExpr, span: Span },
    /// Top-level `let` / `const`.
    Let(LetDecl),
}

#[derive(Debug, Clone)]
pub struct FnDecl {
    pub name: String,
    /// Generic type parameter names (`<T>`); they type-check as `any`.
    pub generics: Vec<String>,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub pattern: Pattern,
    pub ty: Option<TypeExpr>,
    pub default: Option<Expr>,
    /// Declared with `?`.
    pub optional: bool,
}

#[derive(Debug, Clone)]
pub enum Pattern {
    Ident(String, Span),
    /// `{ a, b: alias, c = 1 }`
    Record(Vec<(String, Pattern, Option<Expr>)>, Span),
    /// `[a, b]`
    Array(Vec<Pattern>, Span),
}

impl Pattern {
    pub fn span(&self) -> Span {
        match self {
            Pattern::Ident(_, s) | Pattern::Record(_, s) | Pattern::Array(_, s) => *s,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ObjectDecl {
    pub name: String,
    pub fields: Vec<Field>,
    pub methods: Vec<Method>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: Option<TypeExpr>,
    pub init: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Method {
    pub decorators: Vec<Decorator>,
    pub private: bool,
    pub func: Rc<FnDecl>,
}

#[derive(Debug, Clone)]
pub struct Decorator {
    pub name: String,
    pub args: Vec<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum TypeExpr {
    Named(String, Vec<TypeExpr>),
    Array(Box<TypeExpr>),
    Union(Vec<TypeExpr>),
    Record(Vec<(String, bool, TypeExpr)>),
    Func(Vec<TypeExpr>, Box<TypeExpr>),
    StrLit(String),
    NumLit(f64),
}

#[derive(Debug, Clone)]
pub struct LetDecl {
    pub pattern: Pattern,
    pub mutable: bool,
    pub ty: Option<TypeExpr>,
    pub init: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let(LetDecl),
    Function(Rc<FnDecl>),
    Expr(Expr),
    Return(Option<Expr>, Span),
    If { cond: Expr, then: Vec<Stmt>, els: Option<Vec<Stmt>> },
    While { cond: Expr, body: Vec<Stmt> },
    ForOf { pattern: Pattern, iter: Expr, body: Vec<Stmt> },
    For { init: Option<Box<Stmt>>, cond: Option<Expr>, update: Option<Expr>, body: Vec<Stmt> },
    Break(Span),
    Continue(Span),
    Block(Vec<Stmt>),
    Throw(Expr, Span),
    Try { body: Vec<Stmt>, param: Option<Pattern>, handler: Vec<Stmt> },
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LogicOp {
    And,
    Or,
    Nullish,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Debug, Clone)]
pub enum RecordEntry {
    Field(String, Expr),
    Computed(Expr, Expr),
    Spread(Expr),
}

#[derive(Debug, Clone)]
pub enum ArrayEntry {
    Item(Expr),
    Spread(Expr),
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Num(f64),
    Str(String),
    Template(Vec<TplPiece>),
    Bool(bool),
    Null,
    Ident(String),
    This,
    Array(Vec<ArrayEntry>),
    Record(Vec<RecordEntry>),
    Member { obj: Box<Expr>, name: String, optional: bool },
    Index { obj: Box<Expr>, index: Box<Expr>, optional: bool },
    Call { callee: Box<Expr>, args: Vec<Expr> },
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Logical(LogicOp, Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `op` is `None` for plain `=`, or the arithmetic operator of `+=` and friends.
    Assign { op: Option<BinOp>, target: Box<Expr>, value: Box<Expr> },
    /// `x++` / `--x`; only valid as a statement.
    Update { inc: bool, target: Box<Expr> },
    Arrow(Rc<FnDecl>),
    Await(Box<Expr>),
    Jsx(Box<JsxElement>),
}

#[derive(Debug, Clone)]
pub enum TplPiece {
    Str(String),
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub struct JsxElement {
    pub tag: String,
    pub attrs: Vec<(String, Expr)>,
    pub children: Vec<JsxChild>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum JsxChild {
    Text(String),
    Expr(Expr),
    Element(JsxElement),
}

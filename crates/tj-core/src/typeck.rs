//! Gradual type checker. Annotated code is checked the way TypeScript checks it (without
//! strict null checks yet); anything unannotated is `any`, so loose code still compiles.
//! Callbacks get their parameter types from context (`items.map(t => t.text)`).

use crate::syntax::ast::*;
use crate::syntax::{Diagnostic, Span};
use crate::ui::catalog;
use crate::vm::value::fmt_num;
use indexmap::IndexMap;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    Any,
    Num,
    Str,
    Bool,
    Null,
    Void,
    Elem,
    StrLit(Rc<str>),
    NumLit(f64),
    Arr(Box<Ty>),
    /// `Record<string, T>` / `{ [key: string]: T }`
    Dict(Box<Ty>),
    Rec(Rc<RecTy>),
    Union(Vec<Ty>),
    Func(Rc<FnTy>),
    /// An instance of an object type.
    Obj(Rc<str>),
    /// An object type itself: `Counter` in `Counter.get(id)`.
    Class(Rc<str>),
    /// A type alias, resolved on use so aliases may be recursive.
    Alias(Rc<str>),
}

#[derive(Debug, PartialEq)]
pub struct RecTy {
    /// Field name -> (type, optional).
    pub fields: IndexMap<Rc<str>, (Ty, bool)>,
    /// Built from a spread of unknown shape: unknown fields are `any`.
    pub open: bool,
}

#[derive(Debug, PartialEq)]
pub struct FnTy {
    /// (name, type, optional)
    pub params: Vec<(Rc<str>, Ty, bool)>,
    /// Type of extra arguments, for variadic builtins.
    pub rest: Option<Ty>,
    pub ret: Ty,
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Any => f.write_str("any"),
            Ty::Num => f.write_str("number"),
            Ty::Str => f.write_str("string"),
            Ty::Bool => f.write_str("boolean"),
            Ty::Null => f.write_str("null"),
            Ty::Void => f.write_str("void"),
            Ty::Elem => f.write_str("element"),
            Ty::StrLit(s) => write!(f, "{s:?}"),
            Ty::NumLit(n) => f.write_str(&fmt_num(*n)),
            Ty::Arr(t) if matches!(**t, Ty::Union(_) | Ty::Func(_)) => write!(f, "({t})[]"),
            Ty::Arr(t) => write!(f, "{t}[]"),
            Ty::Dict(t) => write!(f, "Record<string, {t}>"),
            Ty::Rec(r) => {
                let shown: Vec<String> = r.fields.iter().take(4).map(|(k, (t, opt))| format!("{k}{}: {t}", if *opt { "?" } else { "" })).collect();
                let more = if r.fields.len() > 4 { "; …" } else { "" };
                write!(f, "{{ {}{more} }}", shown.join("; "))
            }
            Ty::Union(ts) => write!(f, "{}", ts.iter().map(Ty::to_string).collect::<Vec<_>>().join(" | ")),
            Ty::Func(fun) => {
                let ps: Vec<String> = fun.params.iter().map(|(n, t, opt)| format!("{n}{}: {t}", if *opt { "?" } else { "" })).collect();
                write!(f, "({}) => {}", ps.join(", "), fun.ret)
            }
            Ty::Obj(n) | Ty::Alias(n) => f.write_str(n),
            Ty::Class(n) => write!(f, "typeof {n}"),
        }
    }
}

fn func(params: &[(&str, Ty, bool)], rest: Option<Ty>, ret: Ty) -> Ty {
    Ty::Func(Rc::new(FnTy { params: params.iter().map(|(n, t, o)| (Rc::from(*n), t.clone(), *o)).collect(), rest, ret }))
}

fn rec(fields: &[(&str, Ty)]) -> Ty {
    Ty::Rec(Rc::new(RecTy { fields: fields.iter().map(|(k, t)| (Rc::from(*k), (t.clone(), false))).collect(), open: false }))
}

/// Types of the built-in globals (see `vm::builtins::GLOBALS` and the prelude's `fetch`).
fn builtin(name: &str) -> Option<Ty> {
    let num1 = || func(&[("x", Ty::Num, false)], None, Ty::Num);
    let log = || func(&[], Some(Ty::Any), Ty::Void);
    Some(match name {
        "console" => rec(&[("log", log()), ("warn", log()), ("error", log())]),
        "Math" => rec(&[
            ("floor", num1()),
            ("ceil", num1()),
            ("round", num1()),
            ("trunc", num1()),
            ("abs", num1()),
            ("sign", num1()),
            ("sqrt", num1()),
            ("pow", func(&[("x", Ty::Num, false), ("y", Ty::Num, false)], None, Ty::Num)),
            ("sin", num1()),
            ("cos", num1()),
            ("tan", num1()),
            ("atan2", func(&[("y", Ty::Num, false), ("x", Ty::Num, false)], None, Ty::Num)),
            ("min", func(&[], Some(Ty::Num), Ty::Num)),
            ("max", func(&[], Some(Ty::Num), Ty::Num)),
            ("random", func(&[], None, Ty::Num)),
            ("PI", Ty::Num),
        ]),
        "JSON" => rec(&[
            ("stringify", func(&[("value", Ty::Any, false), ("replacer", Ty::Any, true), ("indent", Ty::Any, true)], None, Ty::Str)),
            ("parse", func(&[("text", Ty::Str, false)], None, Ty::Any)),
        ]),
        "Object" => rec(&[
            ("keys", func(&[("record", Ty::Any, false)], None, Ty::Arr(Box::new(Ty::Str)))),
            ("values", func(&[("record", Ty::Any, false)], None, Ty::Arr(Box::new(Ty::Any)))),
            ("entries", func(&[("record", Ty::Any, false)], None, Ty::Arr(Box::new(Ty::Any)))),
        ]),
        "Date" => rec(&[("now", func(&[], None, Ty::Num))]),
        "Time" => {
            let opt = |names: &[&str]| Ty::Rec(Rc::new(RecTy { fields: names.iter().map(|k| (Rc::from(*k), (Ty::Num, true))).collect(), open: false }));
            let parts = rec(&[("year", Ty::Num), ("month", Ty::Num), ("day", Ty::Num), ("hour", Ty::Num), ("minute", Ty::Num), ("second", Ty::Num), ("weekday", Ty::Num)]);
            let unit = Ty::Union(["minute", "hour", "day", "week", "month", "year"].iter().map(|u| Ty::StrLit(Rc::from(*u))).collect());
            rec(&[
                ("add", func(&[("time", Ty::Num, false), ("by", opt(&["years", "months", "weeks", "days", "hours", "minutes", "seconds"]), false)], None, Ty::Num)),
                ("startOf", func(&[("time", Ty::Num, false), ("unit", unit, false)], None, Ty::Num)),
                ("parts", func(&[("time", Ty::Num, false)], None, parts)),
                ("from", {
                    // year, month and day are needed; the time of day defaults to midnight.
                    let fields = [("year", false), ("month", false), ("day", false), ("hour", true), ("minute", true), ("second", true)];
                    let parts = Ty::Rec(Rc::new(RecTy { fields: fields.iter().map(|(k, o)| (Rc::from(*k), (Ty::Num, *o))).collect(), open: false }));
                    func(&[("parts", parts, false)], None, Ty::Union(vec![Ty::Num, Ty::Null]))
                }),
            ])
        }
        "reveal" => func(&[("value", Ty::Any, false), ("purpose", Ty::Str, false)], None, Ty::Any),
        "crypto" => rec(&[
            ("hmac", func(&[("key", Ty::Str, false), ("text", Ty::Str, false)], None, Ty::Str)),
            ("timingSafeEqual", func(&[("a", Ty::Str, false), ("b", Ty::Str, false)], None, Ty::Bool)),
            ("randomId", func(&[], None, Ty::Str)),
            ("idempotencyKey", func(&[("label", Ty::Str, false)], None, Ty::Str)),
        ]),
        "Array" => rec(&[("isArray", func(&[("value", Ty::Any, false)], None, Ty::Bool))]),
        "String" => func(&[("value", Ty::Any, false)], None, Ty::Str),
        "Number" => func(&[("value", Ty::Any, false)], None, Ty::Num),
        "parseInt" => func(&[("text", Ty::Any, false), ("radix", Ty::Num, true)], None, Ty::Num),
        "parseFloat" => func(&[("text", Ty::Any, false)], None, Ty::Num),
        "encodeURIComponent" => func(&[("text", Ty::Any, false)], None, Ty::Str),
        "Error" => func(&[("message", Ty::Any, true)], None, rec(&[("name", Ty::Str), ("message", Ty::Str)])),
        "check" => func(&[("ok", Ty::Bool, false), ("field", Ty::Str, false), ("message", Ty::Str, false)], None, Ty::Null),
        "t" => func(&[("key", Ty::Str, false), ("params", Ty::Dict(Box::new(Ty::Any)), true)], None, Ty::Str),
        "fetch" => func(&[("url", Ty::Str, false), ("init", Ty::Any, true)], None, Ty::Any),
        "caller" => rec(&[("id", Ty::Str), ("roles", Ty::Arr(Box::new(Ty::Str)))]),
        "plugin" => func(&[("name", Ty::Str, false), ("method", Ty::Str, false), ("args", Ty::Any, true)], None, Ty::Any),
        "invoke" => func(&[("object", Ty::Str, false), ("method", Ty::Str, false), ("args", Ty::Arr(Box::new(Ty::Any)), true)], None, Ty::Any),
        _ => return None,
    })
}

/// Return types of string methods; the runtime implements exactly these (see the test).
fn string_method(name: &str) -> Option<Ty> {
    Some(match name {
        "toUpperCase" | "toLowerCase" | "trim" | "trimStart" | "trimEnd" | "slice" | "substring" | "replace" | "replaceAll" | "padStart" | "padEnd" | "repeat" | "charAt" | "at" => Ty::Str,
        "includes" | "startsWith" | "endsWith" => Ty::Bool,
        "indexOf" => Ty::Num,
        "split" => Ty::Arr(Box::new(Ty::Str)),
        _ => return None,
    })
}

const ARRAY_METHODS: &[&str] = &[
    "map", "flatMap", "filter", "find", "findIndex", "indexOf", "some", "every", "includes", "forEach", "reduce", "push", "unshift", "pop", "shift", "at", "slice", "concat",
    "reverse", "splice", "sort", "join",
];

pub fn widen(t: Ty) -> Ty {
    match t {
        Ty::StrLit(_) => Ty::Str,
        Ty::NumLit(_) => Ty::Num,
        Ty::Null => Ty::Any,
        Ty::Arr(e) => Ty::Arr(Box::new(widen(*e))),
        Ty::Union(ts) => union(ts.into_iter().map(widen).collect()),
        other => other,
    }
}

pub fn union(types: Vec<Ty>) -> Ty {
    let mut out: Vec<Ty> = Vec::new();
    for t in types {
        let parts = match t {
            Ty::Union(inner) => inner,
            other => vec![other],
        };
        for p in parts {
            if p == Ty::Any {
                return Ty::Any;
            }
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    match out.len() {
        0 => Ty::Void,
        1 => out.remove(0),
        _ => Ty::Union(out),
    }
}

/// did you mean 'x'? when a candidate is close, else the full list.
fn suggest(name: &str, label: &str, candidates: &[String]) -> String {
    match crate::syntax::closest(name, candidates.iter().map(String::as_str)) {
        Some(c) => format!("did you mean '{c}'?"),
        None => format!("{label}: {}", candidates.join(", ")),
    }
}

fn strip_null(t: Ty) -> Ty {
    match t {
        Ty::Union(ts) => union(ts.into_iter().filter(|t| *t != Ty::Null).collect()),
        other => other,
    }
}

struct ObjInfo {
    fields: IndexMap<Rc<str>, Ty>,
    methods: HashMap<Rc<str>, Rc<FnTy>>,
}

struct Var {
    ty: Ty,
    mutable: bool,
}

struct FnCtx {
    ret: Option<Ty>,
    returns: Vec<Ty>,
}

struct Checker<'a> {
    aliases: HashMap<&'a str, (&'a [String], &'a TypeExpr)>,
    objects: HashMap<Rc<str>, ObjInfo>,
    globals: HashMap<String, Var>,
    scopes: Vec<HashMap<String, Var>>,
    generics: Vec<String>,
    this_obj: Option<Rc<str>>,
    #[cfg(feature = "check")]
    this_server: bool,
    fns: Vec<FnCtx>,
    depth: usize,
    /// Where the annotation being resolved sits, for unknown-type errors.
    type_span: Span,
    locales: &'a crate::i18n::Locales,
    errors: Vec<Diagnostic>,
}

/// Type-checks the top-level items of an app (all its files, in the order they run).
pub fn check(items: &[&Item]) -> Vec<Diagnostic> {
    let empty = crate::i18n::Locales::default();
    check_with_locales(items, &empty)
}

/// Like [`check`], also checking `t(key, params)` calls against `locales/*.json`.
pub fn check_with_locales(items: &[&Item], locales: &crate::i18n::Locales) -> Vec<Diagnostic> {
    let mut c = Checker {
        aliases: HashMap::new(),
        objects: HashMap::new(),
        globals: HashMap::new(),
        scopes: Vec::new(),
        generics: Vec::new(),
        this_obj: None,
        #[cfg(feature = "check")]
        this_server: false,
        fns: Vec::new(),
        depth: 0,
        type_span: Span::default(),
        locales,
        errors: Vec::new(),
    };
    for &item in items {
        match item {
            Item::TypeAlias { name, generics, ty, .. } => {
                c.aliases.insert(name, (generics, ty));
            }
            Item::Object(o) => {
                c.objects.insert(Rc::from(o.name.as_str()), ObjInfo { fields: IndexMap::new(), methods: HashMap::new() });
                c.globals.insert(o.name.clone(), Var { ty: Ty::Class(Rc::from(o.name.as_str())), mutable: false });
            }
            _ => {}
        }
    }
    for &item in items {
        match item {
            Item::Function(f) => {
                let sig = c.signature(f);
                c.globals.insert(f.name.clone(), Var { ty: Ty::Func(sig), mutable: false });
            }
            Item::Object(o) => c.declare_object(o),
            _ => {}
        }
    }
    for &item in items {
        if let Item::Let(d) = item {
            c.let_decl(d);
        }
    }
    for &item in items {
        match item {
            Item::Function(f) => {
                c.function(f, None);
            }
            Item::Object(o) => {
                let name: Rc<str> = Rc::from(o.name.as_str());
                #[cfg(feature = "check")]
                let is_server = o.decorators.iter().any(|d| d.name == "server");
                for m in &o.methods {
                    let saved = c.this_obj.replace(name.clone());
                    let is_inv = m.decorators.iter().any(|d| d.name == "invariant");
                    let is_frame = m.decorators.iter().any(|d| d.name == "frame");
                    #[cfg(feature = "check")]
                    let saved_server = std::mem::replace(&mut c.this_server, is_server && !is_inv);
                    let fn_ty = c.function(&m.func, None);
                    if is_frame {
                        if let Some((pname, pty, _)) = fn_ty.params.first() {
                            if !c.assignable(&Ty::Num, pty) {
                                c.errors.push(
                                    Diagnostic::new(
                                        format!("@frame method {}.{} parameter '{pname}' must accept number, got {pty}", o.name, m.func.name),
                                        m.func.params[0].pattern.span(),
                                    )
                                    .hint("write `@frame step(dt: number) { ... }`"),
                                );
                            }
                        }
                    }
                    if is_inv {
                        let bad_ret = match &m.func.ret {
                            Some(r) => {
                                let resolved = c.resolve(r);
                                c.deref(&resolved) != Ty::Bool
                            }
                            None => c.deref(&fn_ty.ret) != Ty::Bool && fn_ty.ret != Ty::Any,
                        };
                        if bad_ret || has_bare_return(&m.func.body) {
                            c.errors.push(
                                Diagnostic::new(format!("@invariant method {}.{} must return boolean", o.name, m.func.name), m.func.span)
                                    .hint("declare `: boolean` and return true when valid (e.g. `@invariant ok(): boolean { return this.balance >= 0 }`)"),
                            );
                        } else if !always_exits(&m.func.body) {
                            c.errors.push(
                                Diagnostic::new(format!("@invariant method {}.{} may end without returning a boolean", o.name, m.func.name), m.func.span)
                                    .hint("return true at the end of every non-throwing path (e.g. `return true` after `check(...)`)"),
                            );
                        }
                    }
                    c.this_obj = saved;
                    #[cfg(feature = "check")]
                    {
                        c.this_server = saved_server;
                    }
                }
            }
            _ => {}
        }
    }
    c.errors
}

fn always_exits(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|s| match s {
        Stmt::Return(Some(_), _) | Stmt::Throw(_, _) => true,
        Stmt::If { then, els: Some(els), .. } => always_exits(then) && always_exits(els),
        Stmt::Try { body, handler, .. } => always_exits(body) && always_exits(handler),
        Stmt::Block(b) => always_exits(b),
        _ => false,
    })
}

fn has_bare_return(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|s| match s {
        Stmt::Return(None, _) => true,
        Stmt::If { then, els, .. } => has_bare_return(then) || els.as_deref().is_some_and(has_bare_return),
        Stmt::While { body, .. } | Stmt::ForOf { body, .. } | Stmt::For { body, .. } | Stmt::Block(body) => has_bare_return(body),
        Stmt::Try { body, handler, .. } => has_bare_return(body) || has_bare_return(handler),
        _ => false,
    })
}

impl<'a> Checker<'a> {
    fn error(&mut self, message: impl Into<String>, span: Span) {
        self.errors.push(Diagnostic::new(message, span));
    }

    // ---------- types from annotations ----------

    fn resolve(&mut self, t: &TypeExpr) -> Ty {
        match t {
            TypeExpr::Named(name, args) => match (name.as_str(), args.as_slice()) {
                ("number", _) => Ty::Num,
                ("string", _) => Ty::Str,
                ("boolean" | "true" | "false", _) => Ty::Bool,
                ("null" | "undefined", _) => Ty::Null,
                ("void", _) => Ty::Void,
                ("any" | "unknown" | "object" | "never" | "this", _) => Ty::Any,
                ("Array", [e]) => Ty::Arr(Box::new(self.resolve(e))),
                ("Record", [_, v]) => Ty::Dict(Box::new(self.resolve(v))),
                ("Promise", [v]) => self.resolve(v),
                ("JSX.Element" | "Element", _) => Ty::Elem,
                (n, _) if self.generics.iter().any(|g| g == n) => Ty::Any,
                (n, _) if self.aliases.contains_key(n) => Ty::Alias(Rc::from(n)),
                (n, _) if self.objects.contains_key(n) => Ty::Obj(Rc::from(n)),
                (n, _) => {
                    self.errors.push(Diagnostic::new(format!("unknown type '{n}'"), self.type_span).hint("declare it with `type` or `interface`, or use a built-in type"));
                    Ty::Any
                }
            },
            TypeExpr::Array(e) => Ty::Arr(Box::new(self.resolve(e))),
            TypeExpr::Union(ts) => {
                let parts = ts.iter().map(|t| self.resolve(t)).collect();
                union(parts)
            }
            TypeExpr::Record(fields) => {
                let fields = fields.iter().map(|(k, opt, t)| (Rc::from(k.as_str()), (self.resolve(t), *opt))).collect();
                Ty::Rec(Rc::new(RecTy { fields, open: false }))
            }
            TypeExpr::Func(params, ret) => {
                let params = params.iter().enumerate().map(|(i, t)| (Rc::from(format!("arg{i}").as_str()), self.resolve(t), false)).collect();
                let ret = self.resolve(ret);
                Ty::Func(Rc::new(FnTy { params, rest: None, ret }))
            }
            TypeExpr::StrLit(s) => Ty::StrLit(Rc::from(s.as_str())),
            TypeExpr::NumLit(n) => Ty::NumLit(*n),
        }
    }

    /// Resolves aliases at the top of a type.
    fn deref(&mut self, t: &Ty) -> Ty {
        let mut t = t.clone();
        for _ in 0..32 {
            let Ty::Alias(name) = &t else { return t };
            let Some((generics, body)) = self.aliases.get(&**name).copied() else { return Ty::Any };
            let n = self.generics.len();
            self.generics.extend(generics.iter().cloned());
            t = self.resolve(body);
            self.generics.truncate(n);
        }
        Ty::Any
    }

    /// Whether a value of type `src` may be used where `dst` is expected.
    fn assignable(&mut self, src: &Ty, dst: &Ty) -> bool {
        if src == dst || self.depth > 48 {
            return true;
        }
        self.depth += 1;
        let (s, d) = (self.deref(src), self.deref(dst));
        let ok = match (&s, &d) {
            (Ty::Any, _) | (_, Ty::Any) | (_, Ty::Void) => true,
            // Without strict null checks, null fits anywhere.
            (Ty::Null, _) => true,
            (Ty::Union(ss), _) => ss.iter().all(|x| self.assignable(x, &d)),
            (_, Ty::Union(ds)) => ds.iter().any(|x| self.assignable(&s, x)),
            (Ty::StrLit(_), Ty::Str) | (Ty::NumLit(_), Ty::Num) => true,
            (Ty::Arr(a), Ty::Arr(b)) | (Ty::Dict(a), Ty::Dict(b)) => self.assignable(a, b),
            (Ty::Rec(r), Ty::Dict(v)) => r.fields.values().all(|(t, _)| self.assignable(t, v)),
            (Ty::Rec(a), Ty::Rec(b)) => b.fields.iter().all(|(k, (bt, opt))| match a.fields.get(k) {
                Some((at, _)) => self.assignable(at, bt),
                None => *opt || a.open,
            }),
            (Ty::Func(a), Ty::Func(b)) => {
                let required = a.params.iter().filter(|p| !p.2).count();
                required <= b.params.len() + usize::from(b.rest.is_some()) * 256 && (b.ret == Ty::Void || self.assignable(&a.ret, &b.ret))
            }
            _ => s == d,
        };
        self.depth -= 1;
        ok
    }

    // ---------- declarations ----------

    fn signature(&mut self, f: &FnDecl) -> Rc<FnTy> {
        self.type_span = f.span;
        let n = self.generics.len();
        self.generics.extend(f.generics.iter().cloned());
        let params = f
            .params
            .iter()
            .map(|p| {
                let name = match &p.pattern {
                    Pattern::Ident(n, _) => Rc::from(n.as_str()),
                    _ => Rc::from("props"),
                };
                let ty = match (&p.ty, &p.default) {
                    (Some(t), _) => self.resolve(t),
                    (None, Some(d)) => widen(self.literal_type(d)),
                    (None, None) => Ty::Any,
                };
                (name, ty, p.optional || p.default.is_some())
            })
            .collect();
        let ret = f.ret.as_ref().map_or(Ty::Any, |t| self.resolve(t));
        self.generics.truncate(n);
        Rc::new(FnTy { params, rest: None, ret })
    }

    /// The type of a default value without evaluating anything that needs scope.
    fn literal_type(&self, e: &Expr) -> Ty {
        match &e.kind {
            ExprKind::Num(_) => Ty::Num,
            ExprKind::Str(_) | ExprKind::Template(_) => Ty::Str,
            ExprKind::Bool(_) => Ty::Bool,
            _ => Ty::Any,
        }
    }

    fn declare_object(&mut self, o: &ObjectDecl) {
        let mut fields = IndexMap::new();
        for f in &o.fields {
            self.type_span = f.span;
            let declared = f.ty.as_ref().map(|t| self.resolve(t));
            let ty = match (&f.init, declared) {
                (Some(init), Some(d)) => {
                    self.check_value(init, &d, &format!("field '{}'", f.name));
                    d
                }
                (Some(init), None) => {
                    let t = self.expr(init, None);
                    widen(t)
                }
                (None, Some(d)) => d,
                (None, None) => Ty::Any,
            };
            fields.insert(Rc::from(f.name.as_str()), ty);
        }
        let methods = o.methods.iter().map(|m| (Rc::from(m.func.name.as_str()), self.signature(&m.func))).collect();
        self.objects.insert(Rc::from(o.name.as_str()), ObjInfo { fields, methods });
    }

    fn var(&self, name: &str) -> Option<&Var> {
        self.scopes.iter().rev().find_map(|s| s.get(name)).or_else(|| self.globals.get(name))
    }

    fn lookup(&self, name: &str) -> Ty {
        self.var(name).map(|v| v.ty.clone()).or_else(|| builtin(name)).unwrap_or(Ty::Any)
    }

    fn define(&mut self, name: &str, ty: Ty, mutable: bool) {
        let v = Var { ty, mutable };
        match self.scopes.last_mut() {
            Some(s) => s.insert(name.to_string(), v),
            None => self.globals.insert(name.to_string(), v),
        };
    }

    fn bind(&mut self, p: &Pattern, ty: Ty, mutable: bool) {
        match p {
            Pattern::Ident(n, _) => self.define(n, ty, mutable),
            Pattern::Record(entries, span) => {
                for (key, sub, default) in entries {
                    let mut ft = self.member(&ty, key, *span);
                    if let Some(d) = default {
                        let dt = self.expr(d, Some(&ft));
                        if ft == Ty::Any {
                            ft = widen(dt);
                        }
                    }
                    self.bind(sub, ft, mutable);
                }
            }
            Pattern::Array(items, _) => {
                let el = match self.deref(&ty) {
                    Ty::Arr(e) => *e,
                    _ => Ty::Any,
                };
                for sub in items {
                    self.bind(sub, el.clone(), mutable);
                }
            }
        }
    }

    fn let_decl(&mut self, d: &LetDecl) {
        self.type_span = d.span;
        #[cfg(feature = "check")]
        if let (Some(init), Pattern::Record(entries, span)) = (&d.init, &d.pattern) {
            if entries.iter().any(|(k, _, _)| k == "idempotencyKey") {
                self.check_idempotency_key(init, "idempotencyKey", *span);
            }
        }
        let declared = d.ty.as_ref().map(|t| self.resolve(t));
        let ty = match (&d.init, declared) {
            (Some(e), Some(dt)) => {
                self.check_value(e, &dt, "value");
                dt
            }
            (Some(e), None) => {
                let t = self.expr(e, None);
                if d.mutable { widen(t) } else { t }
            }
            (None, Some(dt)) => dt,
            (None, None) => Ty::Any,
        };
        self.bind(&d.pattern, ty, d.mutable);
    }

    /// Checks a function body; `context` supplies parameter types for unannotated callbacks.
    fn function(&mut self, f: &FnDecl, context: Option<&FnTy>) -> Rc<FnTy> {
        let n = self.generics.len();
        self.generics.extend(f.generics.iter().cloned());
        self.type_span = f.span;
        self.scopes.push(HashMap::new());
        let mut params = Vec::new();
        for (i, p) in f.params.iter().enumerate() {
            let declared = p.ty.as_ref().map(|t| self.resolve(t));
            let ctx = context.and_then(|c| c.params.get(i).map(|p| p.1.clone()).or_else(|| c.rest.clone()));
            let mut ty = declared.or(ctx).unwrap_or(Ty::Any);
            if let Some(d) = &p.default {
                if ty == Ty::Any {
                    ty = widen(self.expr(d, None));
                } else {
                    self.check_value(d, &ty, "default value");
                }
            }
            let name = match &p.pattern {
                Pattern::Ident(n, _) => Rc::from(n.as_str()),
                _ => Rc::from("props"),
            };
            params.push((name, ty.clone(), p.optional || p.default.is_some()));
            self.bind(&p.pattern, ty, true);
        }
        let declared_ret = f.ret.as_ref().map(|t| self.resolve(t));
        self.fns.push(FnCtx { ret: declared_ret.clone(), returns: Vec::new() });
        self.stmts(&f.body);
        let ctx = self.fns.pop().expect("function context");
        self.scopes.pop();
        self.generics.truncate(n);
        let ret = declared_ret.unwrap_or_else(|| if ctx.returns.is_empty() { Ty::Void } else { union(ctx.returns) });
        Rc::new(FnTy { params, rest: None, ret })
    }

    // ---------- statements ----------

    fn stmts(&mut self, stmts: &[Stmt]) {
        // Hoisted nested functions are callable before their declaration.
        for s in stmts {
            if let Stmt::Function(f) = s {
                let sig = self.signature(f);
                self.define(&f.name, Ty::Func(sig), false);
            }
        }
        for s in stmts {
            self.stmt(s);
        }
    }

    fn block(&mut self, stmts: &[Stmt]) {
        self.scopes.push(HashMap::new());
        self.stmts(stmts);
        self.scopes.pop();
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let(d) => self.let_decl(d),
            Stmt::Function(f) => {
                self.function(f, None);
            }
            Stmt::Expr(e) | Stmt::Throw(e, _) => {
                self.expr(e, None);
            }
            Stmt::Return(value, span) => {
                let expected = self.fns.last().and_then(|f| f.ret.clone());
                let t = match (value, &expected) {
                    (Some(v), Some(r)) if *r != Ty::Void => self.check_value(v, r, "return value"),
                    (Some(v), _) => self.expr(v, None),
                    (None, _) => Ty::Void,
                };
                if let (Some(Ty::Void), Some(_)) = (&expected, value) {
                    if t != Ty::Void && t != Ty::Null {
                        self.error("this function is declared to return void", *span);
                    }
                }
                if let Some(f) = self.fns.last_mut() {
                    f.returns.push(t);
                }
            }
            Stmt::If { cond, then, els } => {
                self.expr(cond, None);
                self.block(then);
                if let Some(els) = els {
                    self.block(els);
                }
            }
            Stmt::While { cond, body } => {
                self.expr(cond, None);
                self.block(body);
            }
            Stmt::ForOf { pattern, iter, body } => {
                let it = self.expr(iter, None);
                let el = match self.deref(&it) {
                    Ty::Arr(e) => *e,
                    Ty::Str | Ty::StrLit(_) => Ty::Str,
                    Ty::Any | Ty::Null => Ty::Any,
                    Ty::Union(_) => Ty::Any,
                    other => {
                        self.errors.push(Diagnostic::new(format!("cannot loop over {other}"), iter.span).hint("for...of works on arrays and strings; use Object.keys(...) for records"));
                        Ty::Any
                    }
                };
                self.scopes.push(HashMap::new());
                self.bind(pattern, el, true);
                self.stmts(body);
                self.scopes.pop();
            }
            Stmt::For { init, cond, update, body } => {
                self.scopes.push(HashMap::new());
                if let Some(i) = init {
                    self.stmt(i);
                }
                for e in cond.iter().chain(update.iter()) {
                    self.expr(e, None);
                }
                self.block(body);
                self.scopes.pop();
            }
            Stmt::Block(b) => self.block(b),
            Stmt::Try { body, param, handler } => {
                self.block(body);
                self.scopes.push(HashMap::new());
                if let Some(p) = param {
                    self.bind(p, Ty::Any, true);
                }
                self.stmts(handler);
                self.scopes.pop();
            }
            Stmt::Break(_) | Stmt::Continue(_) => {}
        }
    }

    // ---------- expressions ----------

    /// Types `e` against `expected` and reports a mismatch, unless a more precise error was already reported.
    fn check_value(&mut self, e: &Expr, expected: &Ty, what: &str) -> Ty {
        let before = self.errors.len();
        let t = self.expr(e, Some(expected));
        if self.errors.len() == before && !self.assignable(&t, expected) {
            self.error(format!("{what}: expected {expected}, got {t}"), e.span);
        }
        t
    }

    fn expr(&mut self, e: &Expr, expected: Option<&Ty>) -> Ty {
        let sp = e.span;
        match &e.kind {
            ExprKind::Num(n) => Ty::NumLit(*n),
            ExprKind::Str(s) => Ty::StrLit(Rc::from(s.as_str())),
            ExprKind::Template(pieces) => {
                for p in pieces {
                    if let TplPiece::Expr(x) = p {
                        self.expr(x, None);
                    }
                }
                Ty::Str
            }
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Null => Ty::Null,
            ExprKind::Ident(n) => self.lookup(n),
            ExprKind::This => self.this_obj.clone().map_or(Ty::Any, Ty::Obj),
            ExprKind::Array(items) => {
                let el_expected = expected.map(|t| self.deref(t)).and_then(|t| if let Ty::Arr(e) = t { Some(*e) } else { None });
                let mut types = Vec::new();
                for item in items {
                    match item {
                        ArrayEntry::Item(x) => match &el_expected {
                            Some(el) => types.push(self.check_value(x, el, "array item")),
                            None => {
                                let t = self.expr(x, None);
                                types.push(widen(t));
                            }
                        },
                        ArrayEntry::Spread(x) => {
                            let t = self.expr(x, None);
                            types.push(match self.deref(&t) {
                                Ty::Arr(e) => *e,
                                _ => Ty::Any,
                            });
                        }
                    }
                }
                Ty::Arr(Box::new(el_expected.unwrap_or_else(|| if types.is_empty() { Ty::Any } else { union(types) })))
            }
            ExprKind::Record(entries) => self.record(entries, expected, sp),
            ExprKind::Member { obj, name, .. } => {
                #[cfg(feature = "check")]
                self.check_idempotency_key(obj, name, sp);
                let t = self.expr(obj, None);
                self.member(&t, name, sp)
            }
            ExprKind::Index { obj, index, .. } => {
                let t = self.expr(obj, None);
                let it = self.expr(index, None);
                #[cfg(feature = "check")]
                if let Ty::StrLit(k) = &it {
                    self.check_idempotency_key(obj, k, sp);
                }
                match (self.deref(&t), it) {
                    (Ty::Arr(e), _) => *e,
                    (Ty::Dict(v), _) => *v,
                    (Ty::Str | Ty::StrLit(_), _) => Ty::Str,
                    (rec @ Ty::Rec(_), Ty::StrLit(k)) => self.member(&rec, &k, sp),
                    _ => Ty::Any,
                }
            }
            ExprKind::Call { callee, args } => match &callee.kind {
                ExprKind::Member { obj, name, .. } => {
                    #[cfg(feature = "check")]
                    self.check_idempotency_key(obj, name, sp);
                    let t = self.expr(obj, None);
                    self.method_call(&t, name, args, sp)
                }
                _ => {
                    if let ExprKind::Ident(name) = &callee.kind {
                        if name == "t" && self.var("t").is_none() {
                            return self.check_translate(args, sp);
                        }
                    }
                    let t = self.expr(callee, None);
                    self.call(&t, args, sp)
                }
            },
            ExprKind::Unary(UnOp::Not, x) => {
                self.expr(x, None);
                Ty::Bool
            }
            ExprKind::Unary(UnOp::Neg, x) => {
                let t = self.expr(x, None);
                if !self.numeric(&t) {
                    self.error(format!("cannot negate {t}"), sp);
                }
                Ty::Num
            }
            ExprKind::Binary(op, l, r) => {
                let (lt, rt) = (self.expr(l, None), self.expr(r, None));
                self.binary(*op, &lt, &rt, sp)
            }
            ExprKind::Logical(op, l, r) => {
                let lt = self.expr(l, None);
                let rt = self.expr(r, expected);
                match op {
                    LogicOp::Nullish => union(vec![strip_null(lt), rt]),
                    _ => union(vec![lt, rt]),
                }
            }
            ExprKind::Cond(c, a, b) => {
                self.expr(c, None);
                let at = self.expr(a, expected);
                let bt = self.expr(b, expected);
                union(vec![at, bt])
            }
            ExprKind::Assign { op, target, value } => {
                // Assigning to a constant is a compile error already; don't pile a type error on it.
                if let ExprKind::Ident(n) = &target.kind {
                    if self.var(n).is_some_and(|v| !v.mutable) {
                        return self.expr(value, None);
                    }
                }
                let place = self.expr(target, None);
                match op {
                    None => {
                        self.check_value(value, &place, "assignment");
                    }
                    Some(op) => {
                        let vt = self.expr(value, None);
                        let result = self.binary(*op, &place, &vt, sp);
                        if !self.assignable(&result, &place) {
                            self.error(format!("assignment: expected {place}, got {result}"), sp);
                        }
                    }
                }
                place
            }
            ExprKind::Update { target, .. } => {
                let t = self.expr(target, None);
                if !self.numeric(&t) {
                    self.error(format!("`++` and `--` need a number, got {t}"), sp);
                }
                Ty::Num
            }
            ExprKind::Arrow(f) => {
                let ctx = expected.map(|t| self.deref(t)).and_then(|t| if let Ty::Func(f) = t { Some(f) } else { None });
                Ty::Func(self.function(f, ctx.as_deref()))
            }
            ExprKind::Await(x) => self.expr(x, expected),
            ExprKind::Jsx(el) => self.jsx(el),
        }
    }

    fn record(&mut self, entries: &[RecordEntry], expected: Option<&Ty>, span: Span) -> Ty {
        let want = expected.and_then(|t| match strip_null(self.deref(t)) {
            Ty::Rec(r) => Some(r),
            _ => None,
        });
        let mut fields = IndexMap::new();
        let mut open = false;
        for entry in entries {
            match entry {
                RecordEntry::Field(k, v) => {
                    let field_ty = want.as_ref().map(|w| w.fields.get(k.as_str()).map(|(t, _)| t.clone()));
                    let t = match field_ty {
                        Some(Some(ft)) => self.check_value(v, &ft, &format!("field '{k}'")),
                        Some(None) if !want.as_ref().is_some_and(|w| w.open) => {
                            let t = self.expr(v, None);
                            let w = expected.map_or_else(String::new, |w| w.to_string());
                            self.errors.push(Diagnostic::new(format!("'{k}' is not a field of {w}"), v.span).hint(suggest(k, "fields", &want.as_ref().map(|w| w.fields.keys().map(|k| k.to_string()).collect::<Vec<_>>()).unwrap_or_default())));
                            t
                        }
                        _ => self.expr(v, None),
                    };
                    fields.insert(Rc::from(k.as_str()), (t, false));
                }
                RecordEntry::Computed(k, v) => {
                    self.expr(k, None);
                    self.expr(v, None);
                    open = true;
                }
                RecordEntry::Spread(x) => {
                    let t = self.expr(x, None);
                    match self.deref(&t) {
                        Ty::Rec(r) => {
                            open |= r.open;
                            fields.extend(r.fields.iter().map(|(k, v)| (k.clone(), v.clone())));
                        }
                        _ => open = true,
                    }
                }
            }
        }
        if let Some(w) = &want {
            if !open {
                let mut missing = Vec::new();
                for (k, (_, optional)) in w.fields.iter() {
                    if !*optional && !fields.contains_key(k) {
                        missing.push(k.to_string());
                    }
                }
                if !missing.is_empty() {
                    let name = expected.map_or_else(String::new, |e| e.to_string());
                    self.errors.push(Diagnostic::new(format!("missing field{} {} for {name}", if missing.len() > 1 { "s" } else { "" }, missing.join(", ")), span));
                }
            }
        }
        Ty::Rec(Rc::new(RecTy { fields, open }))
    }

    /// `crypto.idempotencyKey` is only meaningful on `@server` object methods (`docs/retried-effects.md`).
    #[cfg(feature = "check")]
    fn check_idempotency_key(&mut self, obj: &Expr, name: &str, span: Span) {
        if !self.this_server && name == "idempotencyKey" && matches!(&obj.kind, ExprKind::Ident(ns) if ns == "crypto" && self.var(ns).is_none()) {
            self.errors.push(
                Diagnostic::new("crypto.idempotencyKey() is only available inside @server object methods", span)
                    .hint("call crypto.idempotencyKey() inside an @server object method"),
            );
        }
    }

    fn numeric(&mut self, t: &Ty) -> bool {
        match strip_null(self.deref(t)) {
            Ty::Num | Ty::NumLit(_) | Ty::Any | Ty::Null => true,
            Ty::Union(ts) => ts.iter().all(|t| self.numeric(t)),
            _ => false,
        }
    }

    fn stringy(&mut self, t: &Ty) -> bool {
        match strip_null(self.deref(t)) {
            Ty::Str | Ty::StrLit(_) => true,
            Ty::Union(ts) => ts.iter().all(|t| self.stringy(t)),
            _ => false,
        }
    }

    fn binary(&mut self, op: BinOp, l: &Ty, r: &Ty, span: Span) -> Ty {
        let any = self.deref(l) == Ty::Any || self.deref(r) == Ty::Any;
        match op {
            BinOp::Add => {
                if self.stringy(l) || self.stringy(r) {
                    Ty::Str
                } else if any {
                    Ty::Any
                } else if self.numeric(l) && self.numeric(r) {
                    Ty::Num
                } else {
                    self.error(format!("cannot add {l} and {r}"), span);
                    Ty::Any
                }
            }
            BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                if !(self.numeric(l) && self.numeric(r)) {
                    self.error(format!("arithmetic needs numbers, got {l} and {r}"), span);
                }
                Ty::Num
            }
            BinOp::Eq | BinOp::Ne => {
                if !self.assignable(l, r) && !self.assignable(r, l) {
                    let word = if op == BinOp::Eq { "false" } else { "true" };
                    self.errors.push(Diagnostic::new(format!("comparing {l} with {r} is always {word}"), span).hint("the two sides can never be equal; check the types"));
                }
                Ty::Bool
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let ok = any || (self.numeric(l) && self.numeric(r)) || (self.stringy(l) && self.stringy(r));
                if !ok {
                    self.error(format!("cannot compare {l} and {r}"), span);
                }
                Ty::Bool
            }
        }
    }

    /// The type of `t.name` (not called).
    fn member(&mut self, t: &Ty, name: &str, span: Span) -> Ty {
        match self.deref(t) {
            Ty::Any | Ty::Null => Ty::Any,
            Ty::Obj(class) => {
                if name == "id" {
                    return Ty::Str;
                }
                let info = &self.objects[&class];
                if let Some(t) = info.fields.get(name) {
                    return t.clone();
                }
                if let Some(m) = info.methods.get(name) {
                    return Ty::Func(m.clone());
                }
                let members: Vec<String> = info.fields.keys().chain(info.methods.keys()).map(|k| k.to_string()).collect();
                self.errors.push(Diagnostic::new(format!("{class} has no field '{name}'"), span).hint(suggest(name, &format!("members of {class}"), &members)));
                Ty::Any
            }
            Ty::Class(class) => {
                if name != "get" {
                    self.errors.push(Diagnostic::new(format!("{class}.{name} does not exist"), span).hint(format!("use {class}.get(id)")));
                }
                Ty::Any
            }
            Ty::Arr(_) | Ty::Str | Ty::StrLit(_) if name == "length" => Ty::Num,
            Ty::Dict(v) => *v,
            Ty::Rec(r) => match r.fields.get(name) {
                Some((t, _)) => t.clone(),
                None if r.open => Ty::Any,
                None => {
                    let fields: Vec<String> = r.fields.keys().map(|k| k.to_string()).collect();
                    // Name the alias (`Todo`) when there is one; it reads better than the structure.
                    let shown = if let Ty::Alias(_) = t { t.to_string() } else { Ty::Rec(r.clone()).to_string() };
                    self.errors.push(Diagnostic::new(format!("no field '{name}' on {shown}"), span).hint(suggest(name, "fields", &fields)));
                    Ty::Any
                }
            },
            Ty::Union(ts) => {
                // Fields present on any member are fine (missing ones read as null).
                let members: Vec<Ty> = ts.into_iter().filter(|t| *t != Ty::Null).collect();
                let mut found = Vec::new();
                for m in &members {
                    if let Ty::Rec(r) = self.deref(m) {
                        if let Some((t, _)) = r.fields.get(name) {
                            found.push(t.clone());
                        }
                    } else {
                        return Ty::Any;
                    }
                }
                if found.is_empty() && !members.is_empty() {
                    self.error(format!("no field '{name}' on {}", Ty::Union(members)), span);
                    return Ty::Any;
                }
                union(found)
            }
            _ => Ty::Any,
        }
    }

    fn check_translate(&mut self, args: &[Expr], span: Span) -> Ty {
        if args.is_empty() || args.len() > 2 {
            self.error(format!("expected 1-2 arguments, got {}", args.len()), span);
        }
        if let Some(arg0) = args.first() {
            self.check_value(arg0, &Ty::Str, "argument 'key'");
        }
        let params_ty = args
            .get(1)
            .map(|a| self.check_value(a, &Ty::Dict(Box::new(Ty::Any)), "argument 'params'"));
        for extra in args.iter().skip(2) {
            self.expr(extra, None);
        }
        let Some(base_table) = self.locales.base_table() else {
            return Ty::Str;
        };
        let Some(arg0) = args.first() else {
            return Ty::Str;
        };
        let ExprKind::Str(key) = &arg0.kind else {
            return Ty::Str;
        };
        let Some(msg) = base_table.get(key.as_str()) else {
            let keys: Vec<String> = base_table.entries.iter().map(|(k, _)| k.to_string()).collect();
            self.errors.push(
                Diagnostic::new(format!("unknown translation key '{key}'"), arg0.span)
                    .hint(suggest(key, "translation keys", &keys)),
            );
            return Ty::Str;
        };
        let req = crate::i18n::required_placeholders(msg);
        let is_plural = matches!(msg, crate::i18n::Message::Plural(_));
        if !req.is_empty() {
            let Some(arg1) = args.get(1) else {
                let sample = req.iter().map(|p| format!("{p}: ...")).collect::<Vec<_>>().join(", ");
                self.errors.push(
                    Diagnostic::new(format!("translation '{key}' requires parameters: {}", req.join(", ")), span)
                        .hint(format!("pass {{ {sample} }} as the second argument to t()")),
                );
                return Ty::Str;
            };
            if let Some(pt) = &params_ty {
                match self.deref(pt) {
                    Ty::Null => {
                        self.errors.push(
                            Diagnostic::new(format!("translation '{key}' requires parameters: {}", req.join(", ")), arg1.span)
                                .hint("pass a record with the required placeholder values"),
                        );
                    }
                    Ty::Rec(r) => {
                        if !r.open {
                            for p in &req {
                                if !r.fields.contains_key(p.as_str()) {
                                    self.errors.push(
                                        Diagnostic::new(format!("missing placeholder '{p}' for translation '{key}'"), arg1.span)
                                            .hint(format!("add `{p}` to the parameter record")),
                                    );
                                }
                            }
                            for k in r.fields.keys() {
                                if !req.iter().any(|p| p == k.as_ref()) {
                                    self.errors.push(
                                        Diagnostic::new(format!("unknown placeholder '{k}' for translation '{key}'"), arg1.span)
                                            .hint(suggest(k, "placeholders", &req)),
                                    );
                                }
                            }
                        }
                        if is_plural {
                            if let Some((ct, _)) = r.fields.get("count") {
                                if !self.numeric(ct) {
                                    self.error(format!("plural translation '{key}' requires numeric 'count', got {ct}"), arg1.span);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        } else if let (Some(arg1), Some(pt)) = (args.get(1), &params_ty) {
            if let Ty::Rec(r) = self.deref(pt) {
                if !r.open {
                    if let Some(first) = r.fields.keys().next() {
                        self.errors.push(
                            Diagnostic::new(format!("translation '{key}' takes no placeholders, got '{first}'"), arg1.span)
                                .hint(format!("remove the second argument: t({key:?})")),
                        );
                    }
                }
            }
        }
        Ty::Str
    }

    fn call(&mut self, callee: &Ty, args: &[Expr], span: Span) -> Ty {
        match self.deref(callee) {
            Ty::Func(f) => {
                self.args(&f, args, span);
                f.ret.clone()
            }
            Ty::Class(c) => {
                self.errors.push(Diagnostic::new(format!("{c} is an object type, not a function"), span).hint(format!("use {c}.get(id)")));
                Ty::Any
            }
            Ty::Any | Ty::Null => {
                for a in args {
                    self.expr(a, None);
                }
                Ty::Any
            }
            other => {
                self.error(format!("{other} is not a function"), span);
                Ty::Any
            }
        }
    }

    fn args(&mut self, f: &FnTy, args: &[Expr], span: Span) {
        let required = f.params.iter().filter(|p| !p.2).count();
        let max = f.params.len();
        if args.len() < required || (f.rest.is_none() && args.len() > max) {
            let want = if required == max { format!("{max}") } else { format!("{required}-{max}") };
            self.error(format!("expected {want} argument{}, got {}", if max == 1 { "" } else { "s" }, args.len()), span);
        }
        for (i, a) in args.iter().enumerate() {
            match f.params.get(i).map(|p| (p.0.clone(), p.1.clone())).or_else(|| f.rest.clone().map(|t| (Rc::from("argument"), t))) {
                Some((name, t)) => {
                    self.check_value(a, &t, &format!("argument '{name}'"));
                }
                None => {
                    self.expr(a, None);
                }
            }
        }
    }

    /// The return type of a callback argument, typed with `context`.
    fn callback(&mut self, arg: Option<&Expr>, context: FnTy) -> Ty {
        let Some(arg) = arg else { return Ty::Any };
        match self.expr(arg, Some(&Ty::Func(Rc::new(context)))) {
            Ty::Func(f) => f.ret.clone(),
            _ => Ty::Any,
        }
    }

    fn method_call(&mut self, recv: &Ty, name: &str, args: &[Expr], span: Span) -> Ty {
        let recv = strip_null(self.deref(recv));
        match recv {
            Ty::Arr(el) => self.array_method(&el, name, args, span),
            Ty::Str | Ty::StrLit(_) => {
                for a in args {
                    self.expr(a, None);
                }
                string_method(name).unwrap_or_else(|| {
                    self.error(format!("strings have no method '{name}'"), span);
                    Ty::Any
                })
            }
            Ty::Num | Ty::NumLit(_) => {
                for a in args {
                    self.expr(a, None);
                }
                if matches!(name, "toFixed" | "toString") {
                    Ty::Str
                } else {
                    self.error(format!("numbers have no method '{name}'"), span);
                    Ty::Any
                }
            }
            Ty::Obj(class) => {
                if matches!(name, "emit" | "schedule" | "unschedule" | "delete") {
                    for a in args {
                        self.expr(a, None);
                    }
                    return match name {
                        // A handle, for `unschedule`, which says whether it cancelled one.
                        "schedule" => Ty::Num,
                        "unschedule" => Ty::Bool,
                        _ => Ty::Void,
                    };
                }
                let info = &self.objects[&class];
                let method = info.methods.get(name).cloned();
                let is_field = info.fields.contains_key(name);
                let mut methods: Vec<String> = info.methods.keys().map(|k| k.to_string()).collect();
                methods.sort();
                if let Some(m) = method {
                    self.args(&m, args, span);
                    return m.ret.clone();
                }
                let hint = suggest(name, &format!("methods of {class}"), &methods);
                let msg = if is_field { format!("'{name}' is a field of {class}, not a method") } else { format!("{class} has no method '{name}'") };
                self.errors.push(Diagnostic::new(msg, span).hint(hint));
                Ty::Any
            }
            Ty::Class(class) => {
                if name == "get" {
                    let get = FnTy { params: vec![(Rc::from("id"), Ty::Str, false)], rest: None, ret: Ty::Obj(class.clone()) };
                    self.args(&get, args, span);
                    Ty::Obj(class)
                } else if name == "all" {
                    let all = FnTy { params: Vec::new(), rest: None, ret: Ty::Arr(Box::new(Ty::Obj(class.clone()))) };
                    self.args(&all, args, span);
                    Ty::Arr(Box::new(Ty::Obj(class)))
                } else {
                    self.errors.push(Diagnostic::new(format!("{class}.{name} does not exist"), span).hint(format!("use {class}.get(id) or {class}.all()")));
                    Ty::Any
                }
            }
            Ty::Rec(_) | Ty::Dict(_) => {
                let t = self.member(&recv, name, span);
                self.call(&t, args, span)
            }
            _ => {
                for a in args {
                    self.expr(a, None);
                }
                Ty::Any
            }
        }
    }

    fn array_method(&mut self, el: &Ty, name: &str, args: &[Expr], span: Span) -> Ty {
        let item = |ret: Ty| FnTy { params: vec![(Rc::from("item"), el.clone(), false), (Rc::from("index"), Ty::Num, true)], rest: None, ret };
        let arr = |t: Ty| Ty::Arr(Box::new(t));
        let rest_exprs = |c: &mut Self, from: usize| {
            for a in args.iter().skip(from) {
                c.expr(a, None);
            }
        };
        match name {
            "map" => {
                let r = self.callback(args.first(), item(Ty::Any));
                rest_exprs(self, 1);
                arr(widen(r))
            }
            "flatMap" => {
                self.callback(args.first(), item(Ty::Any));
                arr(Ty::Any)
            }
            "filter" | "find" | "findIndex" | "some" | "every" | "forEach" => {
                self.callback(args.first(), item(Ty::Any));
                rest_exprs(self, 1);
                match name {
                    "filter" => arr(el.clone()),
                    "find" => el.clone(),
                    "findIndex" => Ty::Num,
                    "forEach" => Ty::Void,
                    _ => Ty::Bool,
                }
            }
            "reduce" => {
                let init = args.get(1).map(|a| self.expr(a, None)).map_or(Ty::Any, widen);
                let ctx = FnTy {
                    params: vec![(Rc::from("acc"), init.clone(), false), (Rc::from("item"), el.clone(), false), (Rc::from("index"), Ty::Num, true)],
                    rest: None,
                    ret: Ty::Any,
                };
                let r = self.callback(args.first(), ctx);
                if init == Ty::Any { widen(r) } else { init }
            }
            "sort" => {
                let ctx = FnTy { params: vec![(Rc::from("a"), el.clone(), false), (Rc::from("b"), el.clone(), false)], rest: None, ret: Ty::Num };
                self.callback(args.first(), ctx);
                arr(el.clone())
            }
            "push" | "unshift" => {
                for a in args {
                    self.check_value(a, el, "array item");
                }
                Ty::Num
            }
            "indexOf" | "includes" => {
                rest_exprs(self, 0);
                if name == "indexOf" { Ty::Num } else { Ty::Bool }
            }
            "pop" | "shift" | "at" => {
                rest_exprs(self, 0);
                el.clone()
            }
            "slice" | "concat" | "reverse" | "splice" => {
                rest_exprs(self, 0);
                arr(el.clone())
            }
            "join" => {
                rest_exprs(self, 0);
                Ty::Str
            }
            _ => {
                rest_exprs(self, 0);
                self.errors.push(Diagnostic::new(format!("arrays have no method '{name}'"), span).hint(suggest(name, "array methods", &ARRAY_METHODS.iter().map(|m| m.to_string()).collect::<Vec<_>>())));
                Ty::Any
            }
        }
    }

    fn jsx(&mut self, el: &JsxElement) -> Ty {
        if let Some(kind) = catalog::kind(&el.tag) {
            for (name, v) in &el.attrs {
                let t = self.expr(v, None);
                if kind.name == "Native" && name == "name" && !self.assignable(&t, &Ty::Str) {
                    self.error(format!("<Native> name expects a string, got {t}"), v.span);
                }
                if kind.is_event(name) && !matches!(self.deref(&t), Ty::Func(_) | Ty::Any | Ty::Null) {
                    self.error(format!("<{}> {name} expects a function, got {t}", el.tag), v.span);
                }
            }
        } else {
            let comp = self.lookup(&el.tag);
            let props = match self.deref(&comp) {
                Ty::Func(f) => f.params.first().map(|p| p.1.clone()),
                _ => None,
            };
            let props = props.map(|p| strip_null(self.deref(&p)));
            if let Some(Ty::Rec(r)) = &props {
                for (name, v) in &el.attrs {
                    match r.fields.get(name.as_str()) {
                        Some((t, _)) => {
                            self.check_value(v, t, &format!("prop '{name}'"));
                        }
                        None => {
                            self.expr(v, None);
                            if !r.open {
                                let known: Vec<String> = r.fields.keys().map(|k| k.to_string()).collect();
                                let hint = if name == "key" || name == "id" {
                                    format!("your own components take only the props they declare: add `{name}: string` to {}'s props and put it on the element it returns, e.g. <Row {name}={{props.{name}}}>", el.tag)
                                } else {
                                    suggest(name, "props", &known)
                                };
                                self.errors.push(Diagnostic::new(format!("<{}> has no prop '{name}'", el.tag), v.span).hint(hint));
                            }
                        }
                    }
                }
                let mut missing = Vec::new();
                for (k, (_, optional)) in r.fields.iter() {
                    if !*optional && &**k != "children" && !el.attrs.iter().any(|(a, _)| a.as_str() == &**k) {
                        missing.push(k.to_string());
                    }
                }
                if !missing.is_empty() {
                    self.error(format!("<{}> is missing prop{} {}", el.tag, if missing.len() > 1 { "s" } else { "" }, missing.join(", ")), el.span);
                }
            } else {
                for (_, v) in &el.attrs {
                    self.expr(v, None);
                }
            }
        }
        for c in &el.children {
            match c {
                JsxChild::Text(_) => {}
                JsxChild::Expr(e) => {
                    self.expr(e, None);
                }
                JsxChild::Element(e) => {
                    self.jsx(e);
                }
            }
        }
        Ty::Elem
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse;
    use crate::vm::value::Value;

    fn errors(src: &str) -> Vec<String> {
        check(&parse(src).unwrap().items.iter().collect::<Vec<_>>()).into_iter().map(|d| d.message).collect()
    }

    fn ok(src: &str) {
        let e = errors(src);
        assert!(e.is_empty(), "unexpected type errors: {e:?}");
    }

    #[test]
    fn unannotated_code_is_any() {
        ok("function f(a, b) { return a.x + b.y.z() }\nfunction g() { const r = f(1, 2); return r.anything }");
    }

    #[test]
    fn checks_annotated_values() {
        assert_eq!(errors("function f() { const n: number = \"x\" }"), vec!["value: expected number, got \"x\""]);
        assert_eq!(errors("function f() { let n = 0; n = \"x\" }"), vec!["assignment: expected number, got \"x\""]);
        assert_eq!(errors("function f(): string { return 1 }"), vec!["return value: expected string, got 1"]);
        ok("function f() { let x = null; x = 5; let s: string = null }");
    }

    #[test]
    fn checks_records_and_aliases() {
        let src = "type Todo = { id: number, text: string, done?: boolean }\n";
        ok(&format!("{src}function f(): Todo {{ return {{ id: 1, text: \"a\" }} }}"));
        assert_eq!(errors(&format!("{src}function f(): Todo {{ return {{ id: 1, txt: \"a\" }} }}")), vec!["'txt' is not a field of Todo", "missing field text for Todo"]);
        assert_eq!(errors(&format!("{src}function f(t: Todo) {{ return t.titel }}")), vec!["no field 'titel' on Todo"]);
        ok("type Tree = { name: string, kids: Tree[] }\nfunction f(t: Tree): string { return t.kids[0].kids[0].name }");
        ok("function f(d: Record<string, number>, e: { [k: string]: string }) { return d.x + 1 + e[\"y\"].length }");
    }

    #[test]
    fn checks_calls() {
        let src = "function add(a: number, b: number = 1): number { return a + b }\n";
        ok(&format!("{src}function f() {{ return add(1) + add(1, 2) }}"));
        assert_eq!(errors(&format!("{src}function f() {{ return add() }}")), vec!["expected 1-2 arguments, got 0"]);
        assert_eq!(errors(&format!("{src}function f() {{ return add(\"1\") }}")), vec!["argument 'a': expected number, got \"1\""]);
        assert_eq!(errors("function f() { return Math.floor(\"x\") }"), vec!["argument 'x': expected number, got \"x\""]);
    }

    #[test]
    fn callbacks_get_element_types() {
        let src = "type Todo = { text: string, done: boolean }\n";
        ok(&format!("{src}function f(ts: Todo[]): string[] {{ return ts.filter(t => !t.done).map(t => t.text.toUpperCase()) }}"));
        assert_eq!(errors(&format!("{src}function f(ts: Todo[]) {{ return ts.map(t => t.txt) }}")), vec!["no field 'txt' on Todo"]);
        assert_eq!(errors(&format!("{src}function f(ts: Todo[]) {{ ts.push({{ text: 1, done: false }}) }}")), vec!["field 'text': expected string, got 1"]);
        ok("function f(xs: number[]): number { return xs.reduce((s, x) => s + x, 0) }");
    }

    #[test]
    fn checks_objects() {
        let src = "object Cart {\n items: string[] = []\n total = 0\n add(item: string, price: number) { this.items.push(item); this.total += price }\n}\n";
        ok(&format!("{src}function f() {{ const c = Cart.get(\"a\"); c.add(\"x\", 2); return c.total + c.items.length }}"));
        assert_eq!(errors(&format!("{src}function f() {{ Cart.get(\"a\").remove(\"x\") }}")), vec!["Cart has no method 'remove'"]);
        assert_eq!(errors(&format!("{src}function f() {{ return Cart.get(\"a\").totl }}")), vec!["Cart has no field 'totl'"]);
        assert_eq!(errors(&format!("{src}function f() {{ Cart.get(\"a\").add(\"x\") }}")), vec!["expected 2 arguments, got 1"]);
        assert_eq!(errors("object A { n: number = \"zero\" }"), vec!["field 'n': expected number, got \"zero\""]);
        assert_eq!(errors("object A { n = 0\n bump() { this.n = \"x\" } }"), vec!["assignment: expected number, got \"x\""]);
        assert_eq!(errors("object A { s: \"on\" | \"off\" = \"on\"\n set() { this.s = \"maybe\" } }"), vec!["assignment: expected \"on\" | \"off\", got \"maybe\""]);
    }

    #[test]
    fn checks_operators() {
        assert_eq!(errors("function f(s: string) { return s - 1 }"), vec!["arithmetic needs numbers, got string and 1"]);
        assert_eq!(errors("function f(s: string, n: number) { return s === n }"), vec!["comparing string with number is always false"]);
        ok("function f(s: string, n: number | null) { return s + n + (n === null ? 1 : n * 2) }");
    }

    #[test]
    fn checks_components() {
        let src = "type Props = { title: string, count?: number }\nfunction Card(p: Props) { return <Text>{p.title}</Text> }\n";
        ok(&format!("{src}function App() {{ return <Card title=\"a\" count={{1}} /> }}"));
        assert_eq!(errors(&format!("{src}function App() {{ return <Card /> }}")), vec!["<Card> is missing prop title"]);
        assert_eq!(errors(&format!("{src}function App() {{ return <Card title=\"a\" colour=\"red\" /> }}")), vec!["<Card> has no prop 'colour'"]);
        assert_eq!(errors("function App() { return <Button onPress={1}>x</Button> }"), vec!["<Button> onPress expects a function, got 1"]);
    }

    #[test]
    fn builtin_tables_match_the_runtime() {
        for (name, _) in crate::vm::builtins::GLOBALS {
            assert!(name.starts_with("__") || builtin(name).is_some(), "no type for builtin {name}");
        }
        for name in ["toUpperCase", "toLowerCase", "trim", "trimStart", "trimEnd", "slice", "substring", "replace", "replaceAll", "padStart", "padEnd", "repeat", "charAt", "at", "includes", "startsWith", "endsWith", "indexOf", "split"] {
            assert!(string_method(name).is_some());
            assert!(crate::vm::builtins::method(&Value::str(""), name).is_some(), "runtime lacks string method {name}");
        }
        let prelude = crate::syntax::parse(crate::vm::PRELUDE).unwrap();
        for name in ARRAY_METHODS {
            let native = crate::vm::builtins::method(&Value::arr(Vec::new()), name).is_some();
            let in_prelude = prelude.items.iter().any(|i| matches!(i, Item::Function(f) if f.name == format!("__arr_{name}")));
            assert!(native || in_prelude, "runtime lacks array method {name}");
        }
    }
}

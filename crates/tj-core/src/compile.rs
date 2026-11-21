//! AST to bytecode.
//!
//! Locals live in frame slots. A local that any nested function mentions is stored in a
//! heap cell instead (`MakeCell`), and closures capture the cell, which gives closures
//! JavaScript's by-reference semantics without a separate escape analysis.

use crate::syntax::ast::*;
use crate::modules;
use crate::syntax::{parse, parse_range, Diagnostic, Span};
use crate::ui::catalog;
use crate::vm::builtins::GLOBALS;
use crate::vm::program::*;
use crate::vm::program::Program;
use crate::vm::value::Value;
use crate::vm::PRELUDE;
use indexmap::IndexMap;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// Names an object instance provides itself; fields and methods may not use them.
const OBJECT_RESERVED: &[&str] = &["id", "emit", "schedule", "unschedule", "delete"];
const MAX_ERRORS: usize = 20;

/// What the checks know about an app's personal data, for reports: `tj check --privacy
/// --record` and `--compliance`. Offsets are into `src` with the built-in modules it imports
/// added, as `modules::with_built_ins` gives it.
#[cfg(feature = "check")]
#[derive(Debug, Default)]
pub struct Facts {
    /// Every `reveal(value, purpose)`.
    pub reveals: Vec<crate::flow::Reveal>,
    /// Every place revealed personal data leaves the server.
    pub transfers: Vec<crate::flow::Transfer>,
    /// The lawful basis of each purpose, when the app declares them (`const lawfulBasis`).
    pub lawful_basis: Option<Vec<(String, String)>>,
}

#[cfg(feature = "check")]
pub fn facts(src: &str) -> Facts {
    let src = &*modules::with_built_ins(src);
    let files = modules::files(src);
    let parsed: Vec<_> = files.iter().filter_map(|f| parse_range(src, f.start, f.end).ok()).collect();
    if parsed.len() != files.len() {
        return Facts::default();
    }
    let named: Vec<(&str, &crate::syntax::ast::Program)> = files.iter().map(|f| f.name.as_str()).zip(&parsed).collect();
    let (reveals, transfers) = crate::flow::reveals_and_transfers(&named);
    Facts { reveals, transfers, lawful_basis: crate::lawful_basis::declared(&named) }
}

/// Compiles a source file (plus the prelude) to a program.
pub fn compile(src: &str) -> Result<Program, Vec<Diagnostic>> {
    let prelude = parse(PRELUDE).expect("prelude parses");
    // Plugins that ship with tinyjump come with the compiler (see `modules::BUILT_IN`).
    let src = &*modules::with_built_ins(src);
    // An app may be a bundle of files (see `modules`); each is parsed on its own.
    let files = modules::files(src);
    let mut errors = Vec::new();
    let mut parsed = Vec::new();
    for f in &files {
        match parse_range(src, f.start, f.end) {
            Ok(p) => parsed.push(p),
            Err(e) => errors.push(e),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let linked = modules::link(&files, &parsed, &mut errors);
    let all: Vec<&Item> = linked.order.iter().flat_map(|&i| &parsed[i].items).collect();
    errors.extend(crate::typeck::check(&all));
    #[cfg(feature = "check")]
    {
        let named: Vec<(&str, &crate::syntax::ast::Program)> = linked.order.iter().map(|&i| (files[i].name.as_str(), &parsed[i])).collect();
        errors.extend(crate::flow::check(&named));
        errors.extend(crate::lawful_basis::check(&named));
    }
    let mut c = Compiler::new();
    c.file_names = files.into_iter().map(|f| f.name).collect();
    c.imports = linked.imports;
    c.exports = linked.exports;
    c.declare(&prelude, Source::Prelude);
    for &i in &linked.order {
        c.file = i as u16;
        c.declare(&parsed[i], Source::User);
    }
    let mut main = vec![c.main(&prelude, Source::Prelude)];
    for &i in &linked.order {
        c.file = i as u16;
        main.push(c.main(&parsed[i], Source::User));
    }
    if !c.errors.is_empty() || !errors.is_empty() {
        errors.append(&mut c.errors);
        errors.sort_by_key(|d| d.span.start);
        errors.dedup();
        errors.truncate(MAX_ERRORS);
        return Err(errors);
    }
    let app = c.globals.get("App").filter(|g| g.kind == GKind::Function).map(|g| g.slot);
    Ok(Program {
        protos: c.protos,
        consts: c.consts,
        elems: c.elems,
        objects: c.objects,
        globals: c.global_names,
        main,
        app,
        user_source: Rc::from(src),
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GKind {
    Builtin,
    Function,
    Object,
    Var,
}

struct Global {
    slot: u32,
    mutable: bool,
    kind: GKind,
    /// The app file that declares it; `EVERYWHERE` for builtins and the prelude.
    file: u16,
}

const EVERYWHERE: u16 = u16::MAX;

struct Local {
    name: Rc<str>,
    slot: u16,
    cell: bool,
    mutable: bool,
}

struct Scope {
    locals: Vec<Local>,
    start: u16,
}

struct LoopCtx {
    breaks: Vec<usize>,
    continues: Vec<usize>,
    try_depth: usize,
}

struct FnCtx {
    proto: Proto,
    scopes: Vec<Scope>,
    next_slot: u16,
    captured: HashSet<String>,
    upvals: Vec<(Rc<str>, bool)>,
    loops: Vec<LoopCtx>,
    try_depth: usize,
    is_main: bool,
}

#[derive(Clone, Copy)]
enum Var {
    Local { slot: u16, cell: bool, mutable: bool },
    Upval { idx: u16, mutable: bool },
    Global { slot: u32, mutable: bool },
}

#[derive(Clone, Copy)]
enum FnKind<'a> {
    Plain,
    /// A method, with the consents it needs (`@needs`).
    Method(&'a [String]),
}

struct Compiler {
    source: Source,
    protos: Vec<Proto>,
    consts: Vec<Value>,
    elems: Vec<ElemDesc>,
    objects: Vec<ObjectDef>,
    global_names: Vec<Rc<str>>,
    globals: HashMap<String, Global>,
    str_consts: HashMap<String, u32>,
    num_consts: HashMap<u64, u32>,
    fns: Vec<FnCtx>,
    errors: Vec<Diagnostic>,
    /// The app file being compiled, and per file its name, imports and exports.
    file: u16,
    file_names: Vec<String>,
    imports: Vec<HashSet<String>>,
    exports: Vec<HashSet<String>>,
    /// Names already reported as used without an import, so each is reported once per file.
    unimported: Vec<(u16, String)>,
    /// Type names and the file declaring each: like values, they are shared by all files.
    types: HashMap<String, u16>,
    /// Type aliases by name, so parameters can be checked as JSON shapes (`ArgTy`). A generic
    /// alias (`type Box<T> = …`) is not followed: its parameters could be anything.
    aliases: HashMap<String, Option<TypeExpr>>,
}

impl Compiler {
    fn new() -> Self {
        let mut c = Compiler {
            source: Source::Prelude,
            protos: Vec::new(),
            consts: Vec::new(),
            elems: Vec::new(),
            objects: Vec::new(),
            global_names: Vec::new(),
            globals: HashMap::new(),
            str_consts: HashMap::new(),
            num_consts: HashMap::new(),
            fns: Vec::new(),
            errors: Vec::new(),
            file: EVERYWHERE,
            file_names: Vec::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            unimported: Vec::new(),
            types: HashMap::new(),
            aliases: HashMap::new(),
        };
        for (name, _) in GLOBALS {
            c.add_global(name, false, GKind::Builtin);
        }
        c
    }

    fn error(&mut self, d: Diagnostic) {
        if self.errors.len() < MAX_ERRORS {
            // Prelude bugs must never be blamed on user code.
            assert!(self.source == Source::User, "prelude error: {}", d.message);
            self.errors.push(d);
        }
    }

    fn add_global(&mut self, name: &str, mutable: bool, kind: GKind) -> u32 {
        let slot = self.global_names.len() as u32;
        self.global_names.push(Rc::from(name));
        let file = if self.source == Source::User { self.file } else { EVERYWHERE };
        self.globals.insert(name.to_string(), Global { slot, mutable, kind, file });
        slot
    }

    // ---------- declarations ----------

    fn check_top_name(&mut self, name: &str, span: Span) -> bool {
        if self.source == Source::User && name.starts_with("__") {
            self.error(Diagnostic::new(format!("names starting with `__` are reserved: '{name}'"), span));
            return false;
        }
        if catalog::kind(name).is_some() {
            self.error(Diagnostic::new(format!("'{name}' is a built-in component"), span).hint("pick another name"));
            return false;
        }
        if name == "caller" {
            self.error(Diagnostic::new("'caller' is built in: who is making the current call", span).hint("pick another name"));
            return false;
        }
        if let Some(g) = self.globals.get(name) {
            let d = if g.kind == GKind::Builtin {
                Diagnostic::new(format!("'{name}' is a built-in"), span)
            } else if g.file != self.file && g.file != EVERYWHERE {
                let other = self.file_names[g.file as usize].clone();
                Diagnostic::new(format!("'{name}' is already declared in {other}"), span).hint("all files of an app share their top-level names; rename one")
            } else {
                Diagnostic::new(format!("'{name}' is already declared"), span)
            };
            self.error(d);
            return false;
        }
        true
    }

    fn declare(&mut self, program: &crate::syntax::ast::Program, source: Source) {
        self.source = source;
        for item in &program.items {
            match item {
                Item::Function(f) => {
                    if self.check_top_name(&f.name, f.span) {
                        self.add_global(&f.name, false, GKind::Function);
                    }
                }
                Item::Object(o) => {
                    if self.check_top_name(&o.name, o.span) {
                        self.objects.push(ObjectDef { name: Rc::from(o.name.as_str()), fields: Vec::new(), init: 0, methods: IndexMap::new(), server: false, read: None, pii: Vec::new(), guards: Vec::new(), retain: None, hold: None });
                        self.add_global(&o.name, false, GKind::Object);
                    }
                }
                Item::Let(d) => {
                    let mut names = Vec::new();
                    pattern_names(&d.pattern, &mut names);
                    for (n, span) in names {
                        if self.check_top_name(&n, span) {
                            self.add_global(&n, d.mutable, GKind::Var);
                        }
                    }
                }
                Item::TypeAlias { name, ty, generics, .. } if self.source != Source::User => {
                    self.aliases.insert(name.clone(), generics.is_empty().then(|| ty.clone()));
                }
                Item::TypeAlias { name, span, ty, generics } => match self.types.get(name) {
                    Some(&other) if other != self.file => {
                        let d = Diagnostic::new(format!("type '{name}' is already declared in {}", self.file_names[other as usize]), *span).hint("all files of an app share their top-level names; rename one");
                        self.error(d);
                    }
                    _ => {
                        self.types.insert(name.clone(), self.file);
                        self.aliases.insert(name.clone(), generics.is_empty().then(|| ty.clone()));
                    }
                },
            }
        }
    }

    fn main(&mut self, program: &crate::syntax::ast::Program, source: Source) -> u32 {
        self.source = source;
        self.begin_fn("<main>", HashSet::new(), true);
        // Functions first (hoisting), then objects, then top-level variables in order.
        for item in &program.items {
            if let Item::Function(f) = item {
                let p = self.function(f, FnKind::Plain);
                self.emit(Op::Closure(p), f.span);
                if let Some(slot) = self.globals.get(&f.name).map(|g| g.slot) {
                    self.emit(Op::StoreGlobal(slot), f.span);
                }
                self.emit(Op::Pop, f.span);
            }
        }
        for item in &program.items {
            if let Item::Object(o) = item {
                if let Some(class) = self.objects.iter().position(|d| *d.name == *o.name) {
                    self.object(o, class);
                    let k = self.consts.len() as u32;
                    self.consts.push(Value::Class(class as u16));
                    let slot = self.globals[&o.name].slot;
                    self.emit(Op::Const(k), o.span);
                    self.emit(Op::StoreGlobal(slot), o.span);
                    self.emit(Op::Pop, o.span);
                }
            }
        }
        for item in &program.items {
            if let Item::Let(d) = item {
                self.let_decl(d);
            }
        }
        self.emit(Op::Null, Span::default());
        self.emit(Op::Return, Span::default());
        self.end_fn()
    }

    /// The words of an `@read` / `@allow` rule (see docs/access-control.md). `"self"` is for
    /// a field's rule.
    fn rule(&mut self, d: &Decorator, field: bool) -> Option<Rc<[Rc<str>]>> {
        let mut words = Vec::new();
        for a in &d.args {
            let ExprKind::Str(w) = &a.kind else {
                self.error(Diagnostic::new(format!("@{} takes rule words as strings", d.name), a.span).hint("e.g. @read(\"anyone\"), @allow(\"owner\", \"staff\")"));
                return None;
            };
            let role_like = w.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
            if !role_like || w == "system" {
                self.error(Diagnostic::new(format!("'{w}' is not a rule word or role"), a.span).hint("use \"anyone\", \"signedIn\", \"owner\" or a role name such as \"staff\" (\"system\" is reserved)"));
                return None;
            }
            if w == "self" && !field {
                self.error(Diagnostic::new("\"self\" is for a field's @read", a.span).hint("on a field holding a person's id, `@read(\"staff\", \"self\") customer: string` shows it to staff and to that person"));
                return None;
            }
            words.push(Rc::from(w.as_str()));
        }
        if words.is_empty() {
            self.error(Diagnostic::new(format!("@{} needs at least one rule word", d.name), d.span).hint("e.g. @read(\"anyone\"); without a rule, nobody outside the server may"));
            return None;
        }
        Some(words.into())
    }

    /// A field's `@read`: which of the readers the object's rule admits see the field.
    fn field_rule(&mut self, d: &Decorator, f: &crate::syntax::ast::Field, o: &ObjectDecl, server: bool, read: Option<&[Rc<str>]>, pii: bool) -> Option<Rc<[Rc<str>]>> {
        let rule = self.rule(d, true)?;
        let Some(read) = read.filter(|_| server) else {
            self.error(Diagnostic::new(format!("@read on a field narrows who reads {}, and nobody outside the server does", o.name), d.span).hint(format!("give {} @server and its own @read(...) first", o.name)));
            return None;
        };
        #[cfg(feature = "check")]
        self.check_field_rule(&rule, d, f, o, read, pii);
        #[cfg(not(feature = "check"))]
        let _ = (f, read, pii);
        Some(rule)
    }

    /// Each word of a field's rule names a reader it can show the field to. Devices run apps
    /// the CLI checked, and leave this out.
    #[cfg(feature = "check")]
    fn check_field_rule(&mut self, rule: &[Rc<str>], d: &Decorator, f: &crate::syntax::ast::Field, o: &ObjectDecl, read: &[Rc<str>], pii: bool) {
        // One person's data lives in their own object, and they see it: without "owner", the
        // object is not theirs, and export and erasure would not find it.
        if pii && !f.decorators.iter().any(|d| d.name == "by") && !rule.iter().any(|w| &**w == "owner") {
            self.error(Diagnostic::new(format!("{} is one person's data, so its readers include \"owner\"", f.name), d.span).hint("@read(\"owner\", ...) in an object whose id is the person's; for several people's entries in one object, use a @by list"));
        }
        for (w, a) in rule.iter().zip(&d.args) {
            let reaches = read.iter().any(|r| &**r == "anyone" || &**r == "signedIn" && &**w != "anyone" || r == w);
            if &**w == "self" && !matches!(&f.ty, Some(TypeExpr::Named(n, _)) if n == "string") {
                self.error(Diagnostic::new(format!("\"self\" is the person whose id {} holds, so it needs a `string`", f.name), a.span).hint(format!("declare it as `{}: string = \"\"`, set to caller.id", f.name)));
            } else if &**w == "anyone" && pii {
                self.error(Diagnostic::new(format!("{} is personal data and cannot be read by anyone", f.name), a.span).hint("name who may read it, e.g. @read(\"owner\", \"staff\")"));
            } else if !reaches {
                let who = if &**w == "self" { "the person it names".to_string() } else { format!("\"{w}\"") };
                self.error(Diagnostic::new(format!("{who} cannot read {} at all, so this rule never shows them {}", o.name, f.name), a.span).hint(format!("add {} to {}'s @read, or leave it out here", if &**w == "self" { "\"signedIn\"" } else { w }, o.name)));
            }
        }
    }

    /// A declared type as a JSON shape for checking calls from outside. Aliases are followed a
    /// few levels deep; deeper (or recursive) types accept anything.
    fn arg_ty(&self, t: &TypeExpr, depth: u8) -> ArgTy {
        if depth > 8 {
            return ArgTy::Any;
        }
        match t {
            TypeExpr::Named(n, args) => match n.as_str() {
                "number" => ArgTy::Num,
                "string" => ArgTy::Str,
                "boolean" => ArgTy::Bool,
                "null" | "undefined" | "void" => ArgTy::Null,
                "true" => ArgTy::BoolLit(true),
                "false" => ArgTy::BoolLit(false),
                "Record" if args.len() == 2 => ArgTy::Dict(Box::new(self.arg_ty(&args[1], depth + 1))),
                "Array" if args.len() == 1 => ArgTy::Arr(Box::new(self.arg_ty(&args[0], depth + 1))),
                _ => match self.aliases.get(n) {
                    Some(Some(a)) => self.arg_ty(a, depth + 1),
                    _ => ArgTy::Any,
                },
            },
            TypeExpr::Array(t) => ArgTy::Arr(Box::new(self.arg_ty(t, depth + 1))),
            TypeExpr::Union(ts) => ArgTy::Union(ts.iter().map(|t| self.arg_ty(t, depth + 1)).collect()),
            TypeExpr::Record(fields) => ArgTy::Rec(fields.iter().map(|(k, opt, t)| (Rc::from(k.as_str()), *opt, self.arg_ty(t, depth + 1))).collect()),
            TypeExpr::Func(..) => ArgTy::Any,
            TypeExpr::StrLit(s) => ArgTy::StrLit(Rc::from(s.as_str())),
            TypeExpr::NumLit(n) => ArgTy::NumLit(*n),
        }
    }

    /// `@needs("consent:<purpose>", ...)`: the purposes, for a method of a server object that
    /// belongs to a person, in an app that imports tj:consent.
    fn needs(&mut self, d: &Decorator, o: &ObjectDecl) -> Vec<String> {
        let mut purposes = Vec::new();
        for a in &d.args {
            match &a.kind {
                ExprKind::Str(s) if s.strip_prefix("consent:").is_some_and(|p| !p.is_empty()) => purposes.push(s["consent:".len()..].to_string()),
                _ => self.error(Diagnostic::new("@needs takes \"consent:<purpose>\"", a.span).hint("e.g. @needs(\"consent:marketing\")")),
            }
        }
        if d.args.is_empty() {
            self.error(Diagnostic::new("@needs takes \"consent:<purpose>\"", d.span).hint("e.g. @needs(\"consent:marketing\")"));
        }
        let words = |name: &str, ds: &[Decorator]| ds.iter().filter(|d| d.name == name).flat_map(|d| &d.args).any(|a| matches!(&a.kind, ExprKind::Str(w) if w == "owner"));
        let server = o.decorators.iter().any(|d| d.name == "server");
        let personal = words("read", &o.decorators) || o.fields.iter().any(|f| f.decorators.iter().any(|d| d.name == "pii")) || o.methods.iter().any(|m| words("allow", &m.decorators));
        if !server || !personal {
            self.error(Diagnostic::new(format!("@needs is for methods of a @server object that belongs to a person, and {} is not one", o.name), d.span).hint("consent is a person's: put the method on their object, e.g. @server @read(\"owner\") object Member, so its id is theirs"));
        } else if !self.file_names.iter().any(|f| f == "tj:consent") {
            self.error(Diagnostic::new("@needs(\"consent:...\") checks the consents kept by tj:consent", d.span).hint("add import { consented } from \"tj:consent\", and let people give consent with Consent.get(caller.id).give(purpose, text, version)"));
        }
        purposes
    }

    fn object(&mut self, o: &ObjectDecl, class: usize) {
        let mut server = false;
        let mut read = None;
        let (mut retain, mut hold) = (None, None);
        for d in &o.decorators {
            match (d.name.as_str(), d.args.is_empty()) {
                ("server", true) => server = true,
                ("read", _) if read.is_none() => read = self.rule(d, false),
                ("retain", _) if retain.is_none() => retain = self.retain(d),
                ("legalHold", _) if hold.is_none() => {
                    hold = d.args.first().and_then(duration_ms).filter(|ms| *ms > 0.0 && d.args.len() == 1);
                    if hold.is_none() {
                        self.error(Diagnostic::new("@legalHold needs one duration", d.span).hint("e.g. @legalHold(\"6y\"): kept six years after the last change, even when erased"));
                    }
                }
                _ => self.error(Diagnostic::new(format!("unknown or repeated object decorator @{}", d.name), d.span).hint("supported: @server, @read(...), @retain(duration, \"created\" | \"changed\") and @legalHold(duration)")),
            }
        }
        if !server {
            if let Some(d) = o.decorators.iter().find(|d| d.name == "retain" || d.name == "legalHold") {
                self.error(Diagnostic::new(format!("@{} is for @server objects", d.name), d.span).hint(format!("add @server to {}: retention runs on the server", o.name)));
            }
        }
        let mut seen: HashSet<&str> = HashSet::new();
        let mut fields = Vec::new();
        let (mut pii, mut guards) = (Vec::new(), Vec::new());
        for f in &o.fields {
            if OBJECT_RESERVED.contains(&f.name.as_str()) {
                self.error(Diagnostic::new(format!("'{}' is reserved on objects", f.name), f.span).hint("`this.id`, `this.emit()`, `this.schedule()` and `this.delete()` are built in"));
            } else if !seen.insert(&f.name) {
                self.error(Diagnostic::new(format!("'{}' is declared twice", f.name), f.span));
            }
            let is_pii = f.decorators.iter().any(|d| d.name == "pii");
            let (mut names, mut by, mut rule, mut personal) = (HashSet::new(), None, None, None);
            for d in &f.decorators {
                if !names.insert(d.name.as_str()) || !matches!(d.name.as_str(), "pii" | "by" | "read") || d.name == "by" && !is_pii {
                    self.error(Diagnostic::new(format!("unknown or repeated field decorator @{}", d.name), d.span).hint("fields take @pii(category, purpose), @by(field) with it for a list of several people's entries, and @read(...) for who sees the field; other decorators go on methods"));
                } else if d.name == "by" {
                    by = self.by(d, f, o);
                } else if d.name == "read" {
                    rule = self.field_rule(d, f, o, server, read.as_deref(), is_pii);
                } else if !server {
                    self.error(Diagnostic::new("personal data lives in @server objects", d.span).hint(format!("add @server to {}: the server is where it can be exported and erased", o.name)));
                } else {
                    personal = self.pii(d, &f.name);
                }
            }
            if let Some(mut p) = personal {
                // Personal data is its owner's to see, a `@by` list its readers', unless the
                // field names its own readers.
                let default = if by.is_some() { read.clone().unwrap_or_else(|| Rc::from([])) } else { Rc::from([Rc::from("owner")]) };
                guards.push(Guard { field: p.field.clone(), rule: rule.take().unwrap_or(default), pii: true });
                p.by = by;
                pii.push(p);
            } else if let Some(rule) = rule {
                guards.push(Guard { field: Rc::from(f.name.as_str()), rule, pii: false });
            }
            fields.push(Rc::from(f.name.as_str()));
        }
        // The init function builds the default state record.
        self.begin_fn(&format!("{}.<init>", o.name), HashSet::new(), false);
        self.emit(Op::NewRec, o.span);
        for f in &o.fields {
            match &f.init {
                Some(e) => self.expr(e),
                None => {
                    self.emit(Op::Null, f.span);
                }
            }
            let k = self.str_const(&f.name);
            self.emit(Op::RecSet(k), f.span);
        }
        self.emit(Op::Return, o.span);
        let init = self.end_fn();

        let mut methods = IndexMap::new();
        for m in &o.methods {
            let name = m.func.name.as_str();
            if OBJECT_RESERVED.contains(&name) {
                self.error(Diagnostic::new(format!("'{name}' is reserved on objects"), m.func.span));
                continue;
            }
            if !seen.insert(name) {
                self.error(Diagnostic::new(format!("'{name}' is declared twice"), m.func.span));
                continue;
            }
            let mut every = None;
            let mut allow = None;
            let mut exclusive = false;
            let mut needs: Vec<String> = Vec::new();
            for d in &m.decorators {
                match d.name.as_str() {
                    "needs" => {
                        let more = self.needs(d, o);
                        needs.extend(more);
                    }
                    "every" => match d.args.first().and_then(duration_ms) {
                        Some(ms) if d.args.len() == 1 && ms >= 10.0 => every = Some(ms),
                        _ => self.error(Diagnostic::new("@every needs one duration of at least 10ms", d.span).hint("e.g. @every(\"5s\"), @every(\"500ms\"), @every(\"1h\")")),
                    },
                    "allow" if m.private => self.error(Diagnostic::new("private methods cannot be called remotely", d.span).hint("remove `private` or the @allow rule")),
                    "allow" if allow.is_none() => {
                        allow = self.rule(d, false);
                        if read.is_none() {
                            self.error(Diagnostic::new(format!("{} can be called remotely but not read", o.name), d.span).hint(format!("add @read(...) to {}, e.g. @read(\"owner\") to keep it private to its user", o.name)));
                        }
                    }
                    "exclusive" if !exclusive && d.args.is_empty() => exclusive = true,
                    "exclusive" if !exclusive => self.error(Diagnostic::new("@exclusive takes no arguments", d.span).hint("write @exclusive on its own")),
                    other => self.error(Diagnostic::new(format!("unknown or repeated decorator @{other}"), d.span).hint("supported: @every(\"5s\"), @allow(...) (who may call it remotely), @exclusive (one exclusive call at a time per object) and @needs(\"consent:<purpose>\")")),
                }
            }
            let proto = self.function(&m.func, FnKind::Method(&needs));
            let params = m.func.params.iter().map(|p| {
                let name = match &p.pattern {
                    Pattern::Ident(n, _) => Rc::from(n.as_str()),
                    _ => Rc::from("argument"),
                };
                (name, p.optional || p.default.is_some(), p.ty.as_ref().map_or(ArgTy::Any, |t| self.arg_ty(t, 0)))
            });
            let params = params.collect();
            methods.insert(Rc::from(name), MethodDef { proto, private: m.private, every, allow, exclusive, params });
        }
        let def = &mut self.objects[class];
        def.fields = fields;
        def.init = init;
        def.methods = methods;
        def.server = server;
        def.read = read;
        def.pii = pii;
        def.guards = guards;
        def.retain = retain;
        def.hold = hold;
    }

    /// `@retain(duration, "created" | "changed")`; the second defaults to "changed".
    fn retain(&mut self, d: &Decorator) -> Option<Retain> {
        let ms = d.args.first().and_then(duration_ms).filter(|ms| *ms > 0.0);
        let from = match d.args.get(1).map(|a| &a.kind) {
            None => Some(false),
            Some(ExprKind::Str(s)) if s == "changed" => Some(false),
            Some(ExprKind::Str(s)) if s == "created" => Some(true),
            _ => None,
        };
        match (ms, from) {
            (Some(ms), Some(from_created)) if d.args.len() <= 2 => Some(Retain { ms, from_created }),
            _ => {
                self.error(Diagnostic::new("@retain takes a duration, then \"created\" or \"changed\"", d.span).hint("e.g. @retain(\"30d\") or @retain(\"1y\", \"created\"): erased that long after the last change, or after creation"));
                None
            }
        }
    }

    /// `@pii(category, purpose, ...)` on `field`.
    fn pii(&mut self, d: &Decorator, field: &str) -> Option<Pii> {
        let words: Option<Vec<&str>> = d.args.iter().map(|a| if let ExprKind::Str(w) = &a.kind { Some(w.as_str()) } else { None }).collect();
        let hint = "e.g. @pii(\"contact\", \"account\"): a category, then what the data is for";
        let Some([category, purposes @ ..]) = words.as_deref() else {
            self.error(Diagnostic::new("@pii takes a category and at least one purpose, as strings", d.span).hint(hint));
            return None;
        };
        if !PII_CATEGORIES.contains(category) {
            self.error(Diagnostic::new(format!("'{category}' is not a personal-data category"), d.span).hint(format!("use one of: {}", PII_CATEGORIES.join(", "))));
            return None;
        }
        let word = |w: &&str| !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if purposes.is_empty() || !purposes.iter().all(word) {
            self.error(Diagnostic::new(format!("say what '{field}' is for: @pii needs a purpose after the category"), d.span).hint(hint));
            return None;
        }
        Some(Pii { field: Rc::from(field), category: Rc::from(*category), purposes: purposes.iter().map(|p| Rc::from(*p)).collect(), by: None })
    }

    /// `@by("customer", "at")` on a `@pii` list: whose each entry is, and its own time.
    fn by(&mut self, d: &Decorator, f: &crate::syntax::ast::Field, o: &ObjectDecl) -> Option<(Rc<str>, Option<Rc<str>>)> {
        let words: Option<Vec<&str>> = d.args.iter().map(|a| if let ExprKind::Str(w) = &a.kind { Some(w.as_str()) } else { None }).collect();
        let hint = "e.g. @by(\"customer\", \"at\"): the entry field naming whose it is, and its time in milliseconds";
        let Some([who, rest @ ..]) = words.as_deref() else {
            self.error(Diagnostic::new("@by takes the field naming whose each entry is", d.span).hint(hint));
            return None;
        };
        let at = rest.first().copied();
        if rest.len() > 1 {
            self.error(Diagnostic::new("@by takes at most two fields", d.span).hint(hint));
        }
        // The entries' type must have them: a record with `who: string` (and `at: number`).
        let entry = match f.ty.as_ref() {
            Some(TypeExpr::Array(t)) => Some(&**t),
            Some(TypeExpr::Named(n, args)) if n == "Array" && args.len() == 1 => args.first(),
            _ => None,
        };
        let fields = entry.and_then(|t| match t {
            TypeExpr::Record(fs) => Some(fs.clone()),
            TypeExpr::Named(n, _) => match self.aliases.get(n) {
                Some(Some(TypeExpr::Record(fs))) => Some(fs.clone()),
                _ => None,
            },
            _ => None,
        });
        let has = |name: &str, ty: &str| fields.as_ref().is_some_and(|fs| fs.iter().any(|(k, opt, t)| k == name && !opt && matches!(t, TypeExpr::Named(n, _) if n == ty)));
        if !has(who, "string") {
            self.error(Diagnostic::new(format!("@by(\"{who}\") needs {}'s entries to be records with `{who}: string`", f.name), d.span).hint(format!("declare it as a list of records, e.g. `{}: {{ {who}: string, name: string }}[]`", f.name)));
        }
        if let Some(at) = at.filter(|at| !has(at, "number")) {
            self.error(Diagnostic::new(format!("@by's time field `{at}` must be a `number` in each entry"), d.span).hint("milliseconds, e.g. Date.now() when the entry is added"));
        }
        let rule = |name: &str| o.decorators.iter().filter(|d| d.name == name).flat_map(|d| &d.args).any(|a| matches!(&a.kind, ExprKind::Str(w) if w == "anyone"));
        if rule("read") {
            self.error(Diagnostic::new(format!("{} holds several people's personal data and cannot be read by anyone", o.name), d.span).hint("name who may read it, e.g. @read(\"staff\")"));
        }
        if o.decorators.iter().any(|d| d.name == "retain") && at.is_none() {
            self.error(Diagnostic::new(format!("{} has @retain, so each entry needs its own time", o.name), d.span).hint(format!("add the time field: @by(\"{who}\", \"at\"), with `at: number` in each entry")));
        }
        if o.decorators.iter().any(|d| d.name == "legalHold") {
            self.error(Diagnostic::new("@legalHold is not supported with @by lists yet", d.span).hint("keep held records in an object per person, whose id is theirs"));
        }
        // An object either belongs to one person (its id is theirs) or holds lists of several
        // people's entries: erasing a person blanks the first kind whole.
        let owner = |ds: &[Decorator], name: &str| ds.iter().filter(|d| d.name == name).flat_map(|d| &d.args).any(|a| matches!(&a.kind, ExprKind::Str(w) if w == "owner"));
        let personal_field = o.fields.iter().any(|g| g.decorators.iter().any(|d| d.name == "pii") && !g.decorators.iter().any(|d| d.name == "by"));
        if personal_field || owner(&o.decorators, "read") || o.methods.iter().any(|m| owner(&m.decorators, "allow")) {
            self.error(Diagnostic::new(format!("{} mixes a list of several people's entries with data of one person (an \"owner\" rule or a @pii field without @by)", o.name), d.span).hint("keep each person's own data in an object whose id is theirs, and the shared list in another object"));
        }
        Some((Rc::from(*who), at.map(Rc::from)))
    }

    // ---------- functions and scopes ----------

    fn begin_fn(&mut self, name: &str, captured: HashSet<String>, is_main: bool) {
        self.fns.push(FnCtx {
            proto: Proto { name: Rc::from(name), params: 0, locals: 0, code: Vec::new(), offsets: Vec::new(), captures: Vec::new(), source: self.source },
            scopes: vec![Scope { locals: Vec::new(), start: 0 }],
            next_slot: 0,
            captured,
            upvals: Vec::new(),
            loops: Vec::new(),
            try_depth: 0,
            is_main,
        });
    }

    fn end_fn(&mut self) -> u32 {
        let f = self.fns.pop().expect("function context");
        self.protos.push(f.proto);
        (self.protos.len() - 1) as u32
    }

    fn function(&mut self, f: &FnDecl, kind: FnKind) -> u32 {
        self.begin_fn(&f.name, nested_names(f), false);
        if let FnKind::Method(needs) = kind {
            let (this, _) = self.declare_local("this", false, f.span);
            // Checked before anything else runs, on every path into the method.
            for purpose in needs {
                let k = self.str_const(purpose);
                self.emit(Op::LoadLocal(this), f.span);
                self.emit(Op::NeedsConsent(k), f.span);
            }
        }
        let first = self.cur().next_slot;
        for p in &f.params {
            match &p.pattern {
                Pattern::Ident(n, span) => {
                    self.declare_local(n, true, *span);
                }
                other => {
                    self.declare_local("", true, other.span());
                }
            }
        }
        self.cur().proto.params = self.cur().next_slot;
        for (i, p) in f.params.iter().enumerate() {
            if let Some(d) = &p.default {
                let slot = first + i as u16;
                self.emit(Op::LoadLocal(slot), d.span);
                let j = self.emit(Op::JumpIfNotNullKeep(0), d.span);
                self.expr(d);
                self.emit(Op::StoreLocal(slot), d.span);
                self.patch(j);
                self.emit(Op::Pop, d.span);
            }
        }
        let cells: Vec<u16> = self.cur().scopes[0].locals.iter().filter(|l| l.cell).map(|l| l.slot).collect();
        for slot in cells {
            self.emit(Op::MakeCell(slot), f.span);
        }
        for (i, p) in f.params.iter().enumerate() {
            if !matches!(p.pattern, Pattern::Ident(..)) {
                self.emit(Op::LoadLocal(first + i as u16), p.pattern.span());
                self.bind_pattern(&p.pattern, true);
            }
        }
        self.stmts(&f.body);
        self.emit(Op::Null, f.span);
        self.emit(Op::Return, f.span);
        self.end_fn()
    }

    fn cur(&mut self) -> &mut FnCtx {
        self.fns.last_mut().expect("function context")
    }

    fn begin_scope(&mut self) {
        let start = self.cur().next_slot;
        self.cur().scopes.push(Scope { locals: Vec::new(), start });
    }

    fn end_scope(&mut self) {
        let s = self.cur().scopes.pop().expect("scope");
        self.cur().next_slot = s.start;
    }

    fn declare_local(&mut self, name: &str, mutable: bool, span: Span) -> (u16, bool) {
        let dup = !name.is_empty() && self.cur().scopes.last().is_some_and(|s| s.locals.iter().any(|l| &*l.name == name));
        if dup {
            self.error(Diagnostic::new(format!("'{name}' is already declared in this scope"), span));
        }
        let f = self.cur();
        let slot = f.next_slot;
        f.next_slot += 1;
        f.proto.locals = f.proto.locals.max(f.next_slot);
        let cell = f.captured.contains(name);
        f.scopes.last_mut().expect("scope").locals.push(Local { name: Rc::from(name), slot, cell, mutable });
        (slot, cell)
    }

    /// Stores the value on top of the stack into a fresh hidden local and pops it.
    fn temp(&mut self, span: Span) -> u16 {
        let (slot, _) = self.declare_local("", true, span);
        self.emit(Op::StoreLocal(slot), span);
        self.emit(Op::Pop, span);
        slot
    }

    /// Binds the value on top of the stack to a new name, popping it.
    fn define(&mut self, name: &str, mutable: bool, span: Span) {
        let top_level = self.fns.len() == 1 && self.fns[0].is_main && self.fns[0].scopes.len() == 1;
        if top_level {
            if let Some(slot) = self.globals.get(name).map(|g| g.slot) {
                self.emit(Op::StoreGlobal(slot), span);
            }
        } else {
            let (slot, cell) = self.declare_local(name, mutable, span);
            self.emit(Op::StoreLocal(slot), span);
            if cell {
                self.emit(Op::MakeCell(slot), span);
            }
        }
        self.emit(Op::Pop, span);
    }

    fn bind_pattern(&mut self, pat: &Pattern, mutable: bool) {
        match pat {
            Pattern::Ident(name, span) => self.define(name, mutable, *span),
            Pattern::Record(entries, span) => {
                let t = self.temp(*span);
                for (key, sub, default) in entries {
                    self.emit(Op::LoadLocal(t), sub.span());
                    let k = self.str_const(key);
                    self.emit(Op::GetField(k), sub.span());
                    self.default_value(default.as_ref());
                    self.bind_pattern(sub, mutable);
                }
            }
            Pattern::Array(items, span) => {
                let t = self.temp(*span);
                for (i, sub) in items.iter().enumerate() {
                    self.emit(Op::LoadLocal(t), sub.span());
                    let k = self.num_const(i as f64);
                    self.emit(Op::Const(k), sub.span());
                    self.emit(Op::GetIndex, sub.span());
                    self.bind_pattern(sub, mutable);
                }
            }
        }
    }

    fn default_value(&mut self, default: Option<&Expr>) {
        if let Some(d) = default {
            let j = self.emit(Op::JumpIfNotNullKeep(0), d.span);
            self.expr(d);
            self.patch(j);
        }
    }

    fn find_local(&self, depth: usize, name: &str) -> Option<&Local> {
        self.fns[depth].scopes.iter().rev().flat_map(|s| s.locals.iter().rev()).find(|l| &*l.name == name)
    }

    fn resolve(&mut self, name: &str) -> Option<Var> {
        let d = self.fns.len() - 1;
        if let Some(l) = self.find_local(d, name) {
            return Some(Var::Local { slot: l.slot, cell: l.cell, mutable: l.mutable });
        }
        if let Some((idx, mutable)) = self.resolve_upval(d, name) {
            return Some(Var::Upval { idx, mutable });
        }
        self.globals.get(name).map(|g| Var::Global { slot: g.slot, mutable: g.mutable })
    }

    fn resolve_upval(&mut self, d: usize, name: &str) -> Option<(u16, bool)> {
        if d == 0 || name.is_empty() {
            return None;
        }
        if let Some(i) = self.fns[d].upvals.iter().position(|u| &*u.0 == name) {
            return Some((i as u16, self.fns[d].upvals[i].1));
        }
        let (cap, mutable) = if let Some(l) = self.find_local(d - 1, name) {
            debug_assert!(l.cell, "captured local '{name}' must live in a cell");
            (Capture::Local(l.slot), l.mutable)
        } else {
            let (i, m) = self.resolve_upval(d - 1, name)?;
            (Capture::Upval(i), m)
        };
        let f = &mut self.fns[d];
        f.proto.captures.push(cap);
        f.upvals.push((Rc::from(name), mutable));
        Some(((f.upvals.len() - 1) as u16, mutable))
    }

    fn unknown_name(&mut self, name: &str, span: Span) {
        let d = if name == "this" {
            Diagnostic::new("`this` is only available inside object methods", span)
        } else {
            let visible: Vec<String> = self
                .fns
                .iter()
                .flat_map(|f| f.scopes.iter().flat_map(|s| s.locals.iter().map(|l| l.name.to_string())))
                .chain(self.globals.keys().filter(|g| !g.starts_with("__")).cloned())
                .collect();
            let hint = match crate::syntax::closest(name, visible.iter().map(String::as_str)) {
                Some(c) => format!("did you mean '{c}'?"),
                None => "declare it with `let`, `const` or `function` before using it".into(),
            };
            Diagnostic::new(format!("unknown name '{name}'"), span).hint(hint)
        };
        self.error(d);
    }

    /// A top-level name of another file must be imported, as in TypeScript.
    fn check_imported(&mut self, name: &str, span: Span) {
        let Some(owner) = self.globals.get(name).map(|g| g.file) else { return };
        let here = self.file;
        if self.source != Source::User || owner == EVERYWHERE || owner == here || self.imports[here as usize].contains(name) {
            return;
        }
        if self.unimported.iter().any(|(f, n)| *f == here && n == name) {
            return;
        }
        self.unimported.push((here, name.to_string()));
        let (owner_name, here_name) = (&self.file_names[owner as usize], &self.file_names[here as usize]);
        let import = format!("import {{ {name} }} from \"{}\"", modules::relative(here_name, owner_name));
        let hint = if self.exports[owner as usize].contains(name) { format!("add {import}") } else { format!("export it from {owner_name}, then add {import}") };
        let d = Diagnostic::new(format!("'{name}' is declared in {owner_name}"), span).hint(hint);
        self.error(d);
    }

    fn load_var(&mut self, name: &str, span: Span) {
        if let Some(Var::Global { .. }) = self.resolve(name) {
            self.check_imported(name, span);
            if matches!(name, "plugin" | "invoke") && self.source == Source::User && !self.file_names.get(self.file as usize).is_some_and(|f| f.starts_with("tj:")) {
                self.error(Diagnostic::new(format!("`{name}()` is for plugin modules"), span).hint("import the plugin's functions instead, e.g. import { complete } from \"tj:ai\""));
            }
        }
        match self.resolve(name) {
            Some(Var::Local { slot, cell: false, .. }) => self.emit(Op::LoadLocal(slot), span),
            Some(Var::Local { slot, cell: true, .. }) => self.emit(Op::LoadCell(slot), span),
            Some(Var::Upval { idx, .. }) => self.emit(Op::LoadUpval(idx), span),
            Some(Var::Global { slot, .. }) => self.emit(Op::LoadGlobal(slot), span),
            None if name == "caller" => self.emit(Op::Caller, span),
            None => {
                self.unknown_name(name, span);
                self.emit(Op::Null, span)
            }
        };
    }

    fn store_var(&mut self, name: &str, span: Span) {
        let Some(var) = self.resolve(name) else {
            if name == "caller" {
                self.error(Diagnostic::new("'caller' cannot be changed", span).hint("it is whoever made the current call; copy what you need into a variable"));
            } else {
                self.unknown_name(name, span);
            }
            return;
        };
        if let Var::Global { .. } = var {
            self.check_imported(name, span);
        }
        let mutable = match var {
            Var::Local { mutable, .. } | Var::Upval { mutable, .. } | Var::Global { mutable, .. } => mutable,
        };
        if !mutable {
            self.error(Diagnostic::new(format!("cannot assign to '{name}'"), span).hint("declare it with `let` to make it changeable"));
            return;
        }
        match var {
            Var::Local { slot, cell: false, .. } => self.emit(Op::StoreLocal(slot), span),
            Var::Local { slot, cell: true, .. } => self.emit(Op::StoreCell(slot), span),
            Var::Upval { idx, .. } => self.emit(Op::StoreUpval(idx), span),
            Var::Global { slot, .. } => self.emit(Op::StoreGlobal(slot), span),
        };
    }

    // ---------- emission ----------

    fn emit(&mut self, op: Op, span: Span) -> usize {
        let f = self.cur();
        f.proto.code.push(op);
        f.proto.offsets.push(span.start);
        f.proto.code.len() - 1
    }

    fn here(&mut self) -> u32 {
        self.cur().proto.code.len() as u32
    }

    fn patch(&mut self, at: usize) {
        let target = self.here();
        self.patch_to(at, target);
    }

    fn patch_to(&mut self, at: usize, target: u32) {
        match &mut self.cur().proto.code[at] {
            Op::Jump(t)
            | Op::JumpIfFalse(t)
            | Op::JumpIfFalseKeep(t)
            | Op::JumpIfTrueKeep(t)
            | Op::JumpIfNotNullKeep(t)
            | Op::JumpIfNullKeep(t)
            | Op::TryBegin(t) => *t = target,
            other => unreachable!("patching non-jump {other:?}"),
        }
    }

    fn str_const(&mut self, s: &str) -> u32 {
        if let Some(&k) = self.str_consts.get(s) {
            return k;
        }
        let k = self.consts.len() as u32;
        self.consts.push(Value::str(s));
        self.str_consts.insert(s.to_string(), k);
        k
    }

    fn num_const(&mut self, n: f64) -> u32 {
        if let Some(&k) = self.num_consts.get(&n.to_bits()) {
            return k;
        }
        let k = self.consts.len() as u32;
        self.consts.push(Value::Num(n));
        self.num_consts.insert(n.to_bits(), k);
        k
    }

    // ---------- statements ----------

    fn block(&mut self, stmts: &[Stmt]) {
        self.begin_scope();
        self.stmts(stmts);
        self.end_scope();
    }

    /// Compiles a statement list. Function declarations are hoisted, like JavaScript.
    fn stmts(&mut self, stmts: &[Stmt]) {
        let fns: Vec<&Rc<FnDecl>> = stmts.iter().filter_map(|s| if let Stmt::Function(f) = s { Some(f) } else { None }).collect();
        let mut slots = Vec::new();
        for f in &fns {
            let (slot, cell) = self.declare_local(&f.name, false, f.span);
            if cell {
                self.emit(Op::Null, f.span);
                self.emit(Op::StoreLocal(slot), f.span);
                self.emit(Op::MakeCell(slot), f.span);
                self.emit(Op::Pop, f.span);
            }
            slots.push((slot, cell));
        }
        for (f, (slot, cell)) in fns.iter().zip(slots) {
            let p = self.function(f, FnKind::Plain);
            self.emit(Op::Closure(p), f.span);
            self.emit(if cell { Op::StoreCell(slot) } else { Op::StoreLocal(slot) }, f.span);
            self.emit(Op::Pop, f.span);
        }
        for s in stmts {
            self.stmt(s);
        }
    }

    fn let_decl(&mut self, d: &LetDecl) {
        match &d.init {
            Some(e) => self.expr(e),
            None => {
                self.emit(Op::Null, d.span);
            }
        }
        self.bind_pattern(&d.pattern, d.mutable);
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let(d) => self.let_decl(d),
            Stmt::Function(_) => {}
            Stmt::Expr(e) => {
                if let ExprKind::Update { inc, target } = &e.kind {
                    self.update(*inc, target, e.span);
                } else {
                    self.expr(e);
                }
                self.emit(Op::Pop, e.span);
            }
            Stmt::Return(value, span) => {
                if self.cur().is_main {
                    self.error(Diagnostic::new("`return` outside a function", *span));
                }
                match value {
                    Some(v) => self.expr(v),
                    None => {
                        self.emit(Op::Null, *span);
                    }
                }
                self.emit(Op::Return, *span);
            }
            Stmt::If { cond, then, els } => {
                self.expr(cond);
                let j = self.emit(Op::JumpIfFalse(0), cond.span);
                self.block(then);
                if let Some(els) = els {
                    let j2 = self.emit(Op::Jump(0), cond.span);
                    self.patch(j);
                    self.block(els);
                    self.patch(j2);
                } else {
                    self.patch(j);
                }
            }
            Stmt::While { cond, body } => {
                let top = self.here();
                self.expr(cond);
                let exit = self.emit(Op::JumpIfFalse(0), cond.span);
                self.push_loop();
                self.block(body);
                self.emit(Op::Jump(top), cond.span);
                self.patch(exit);
                self.pop_loop(top);
            }
            Stmt::ForOf { pattern, iter, body } => {
                let span = iter.span;
                self.begin_scope();
                self.expr(iter);
                let arr = self.temp(span);
                let zero = self.num_const(0.0);
                self.emit(Op::Const(zero), span);
                let i = self.temp(span);
                let top = self.here();
                self.emit(Op::LoadLocal(i), span);
                self.emit(Op::LoadLocal(arr), span);
                self.emit(Op::Len, span);
                self.emit(Op::Lt, span);
                let exit = self.emit(Op::JumpIfFalse(0), span);
                self.push_loop();
                self.begin_scope();
                self.emit(Op::LoadLocal(arr), span);
                self.emit(Op::LoadLocal(i), span);
                self.emit(Op::GetIndex, span);
                self.bind_pattern(pattern, true);
                self.stmts(body);
                self.end_scope();
                let cont = self.here();
                let one = self.num_const(1.0);
                self.emit(Op::LoadLocal(i), span);
                self.emit(Op::Const(one), span);
                self.emit(Op::Add, span);
                self.emit(Op::StoreLocal(i), span);
                self.emit(Op::Pop, span);
                self.emit(Op::Jump(top), span);
                self.patch(exit);
                self.pop_loop(cont);
                self.end_scope();
            }
            Stmt::For { init, cond, update, body } => {
                self.begin_scope();
                if let Some(init) = init {
                    self.stmt(init);
                }
                let loop_cells: Vec<u16> = self.cur().scopes.last().map(|s| s.locals.iter().filter(|l| l.cell).map(|l| l.slot).collect()).unwrap_or_default();
                let top = self.here();
                let exit = cond.as_ref().map(|c| {
                    self.expr(c);
                    self.emit(Op::JumpIfFalse(0), c.span)
                });
                self.push_loop();
                self.block(body);
                let cont = self.here();
                // Closures made in this iteration keep this iteration's binding.
                for slot in loop_cells {
                    self.emit(Op::Recell(slot), Span::default());
                }
                if let Some(u) = update {
                    if let ExprKind::Update { inc, target } = &u.kind {
                        self.update(*inc, target, u.span);
                    } else {
                        self.expr(u);
                    }
                    self.emit(Op::Pop, u.span);
                }
                self.emit(Op::Jump(top), Span::default());
                if let Some(exit) = exit {
                    self.patch(exit);
                }
                self.pop_loop(cont);
                self.end_scope();
            }
            Stmt::Break(span) | Stmt::Continue(span) => {
                let is_break = matches!(s, Stmt::Break(_));
                let Some(depth) = self.cur().loops.last().map(|l| l.try_depth) else {
                    let word = if is_break { "break" } else { "continue" };
                    self.error(Diagnostic::new(format!("`{word}` outside a loop"), *span));
                    return;
                };
                for _ in depth..self.cur().try_depth {
                    self.emit(Op::TryEnd, *span);
                }
                let j = self.emit(Op::Jump(0), *span);
                let l = self.cur().loops.last_mut().expect("loop");
                if is_break { l.breaks.push(j) } else { l.continues.push(j) }
            }
            Stmt::Block(b) => self.block(b),
            Stmt::Throw(e, span) => {
                self.expr(e);
                self.emit(Op::Throw, *span);
            }
            Stmt::Try { body, param, handler } => {
                let t = self.emit(Op::TryBegin(0), Span::default());
                self.cur().try_depth += 1;
                self.block(body);
                self.cur().try_depth -= 1;
                self.emit(Op::TryEnd, Span::default());
                let j = self.emit(Op::Jump(0), Span::default());
                self.patch(t);
                self.begin_scope();
                match param {
                    Some(p) => self.bind_pattern(p, true),
                    None => {
                        self.emit(Op::Pop, Span::default());
                    }
                }
                self.stmts(handler);
                self.end_scope();
                self.patch(j);
            }
        }
    }

    fn push_loop(&mut self) {
        let try_depth = self.cur().try_depth;
        self.cur().loops.push(LoopCtx { breaks: Vec::new(), continues: Vec::new(), try_depth });
    }

    fn pop_loop(&mut self, continue_target: u32) {
        let l = self.cur().loops.pop().expect("loop");
        for j in l.continues {
            self.patch_to(j, continue_target);
        }
        for j in l.breaks {
            self.patch(j);
        }
    }

    // ---------- expressions ----------

    fn expr(&mut self, e: &Expr) {
        let sp = e.span;
        match &e.kind {
            ExprKind::Num(n) => {
                let k = self.num_const(*n);
                self.emit(Op::Const(k), sp);
            }
            ExprKind::Str(s) => {
                let k = self.str_const(s);
                self.emit(Op::Const(k), sp);
            }
            ExprKind::Template(pieces) => {
                for p in pieces {
                    match p {
                        TplPiece::Str(s) => {
                            let k = self.str_const(s);
                            self.emit(Op::Const(k), sp);
                        }
                        TplPiece::Expr(x) => self.expr(x),
                    }
                }
                self.emit(Op::Concat(pieces.len() as u16), sp);
            }
            ExprKind::Bool(b) => {
                self.emit(if *b { Op::True } else { Op::False }, sp);
            }
            ExprKind::Null => {
                self.emit(Op::Null, sp);
            }
            ExprKind::Ident(name) => self.load_var(name, sp),
            ExprKind::This => self.load_var("this", sp),
            ExprKind::Array(items) => {
                self.emit(Op::NewArr, sp);
                for item in items {
                    match item {
                        ArrayEntry::Item(x) => {
                            self.expr(x);
                            self.emit(Op::ArrPush, x.span);
                        }
                        ArrayEntry::Spread(x) => {
                            self.expr(x);
                            self.emit(Op::ArrSpread, x.span);
                        }
                    }
                }
            }
            ExprKind::Record(entries) => {
                self.emit(Op::NewRec, sp);
                for entry in entries {
                    match entry {
                        RecordEntry::Field(k, v) => {
                            self.expr(v);
                            let k = self.str_const(k);
                            self.emit(Op::RecSet(k), v.span);
                        }
                        RecordEntry::Computed(k, v) => {
                            self.expr(k);
                            self.expr(v);
                            self.emit(Op::RecSetDyn, v.span);
                        }
                        RecordEntry::Spread(x) => {
                            self.expr(x);
                            self.emit(Op::RecSpread, x.span);
                        }
                    }
                }
            }
            ExprKind::Member { .. } | ExprKind::Index { .. } | ExprKind::Call { .. } => {
                let mut exits = Vec::new();
                self.chain(e, &mut exits);
                for j in exits {
                    self.patch(j);
                }
            }
            ExprKind::Unary(op, x) => {
                self.expr(x);
                self.emit(if *op == UnOp::Neg { Op::Neg } else { Op::Not }, sp);
            }
            ExprKind::Binary(op, l, r) => {
                self.expr(l);
                self.expr(r);
                self.emit(bin_op(*op), sp);
            }
            ExprKind::Logical(op, l, r) => {
                self.expr(l);
                let j = self.emit(
                    match op {
                        LogicOp::And => Op::JumpIfFalseKeep(0),
                        LogicOp::Or => Op::JumpIfTrueKeep(0),
                        LogicOp::Nullish => Op::JumpIfNotNullKeep(0),
                    },
                    sp,
                );
                self.expr(r);
                self.patch(j);
            }
            ExprKind::Cond(c, a, b) => {
                self.expr(c);
                let j = self.emit(Op::JumpIfFalse(0), sp);
                self.expr(a);
                let j2 = self.emit(Op::Jump(0), sp);
                self.patch(j);
                self.expr(b);
                self.patch(j2);
            }
            ExprKind::Assign { op, target, value } => self.assign(*op, target, |c| c.expr(value), sp),
            ExprKind::Update { .. } => {
                self.error(Diagnostic::new("`++` and `--` can only be used as statements", sp).hint("use `x += 1` and read `x` on the next line"));
            }
            ExprKind::Arrow(f) => {
                let p = self.function(f, FnKind::Plain);
                self.emit(Op::Closure(p), sp);
            }
            ExprKind::Await(x) => {
                self.expr(x);
                self.emit(Op::Await, sp);
            }
            ExprKind::Jsx(el) => self.jsx(el),
        }
    }

    /// Compiles member/index/call chains. Every `?.` in the chain jumps to the chain's end.
    fn chain(&mut self, e: &Expr, exits: &mut Vec<usize>) {
        let sp = e.span;
        match &e.kind {
            ExprKind::Member { obj, name, optional } => {
                self.chain(obj, exits);
                if *optional {
                    exits.push(self.emit(Op::JumpIfNullKeep(0), sp));
                }
                let k = self.str_const(name);
                self.emit(Op::GetField(k), sp);
            }
            ExprKind::Index { obj, index, optional } => {
                self.chain(obj, exits);
                if *optional {
                    exits.push(self.emit(Op::JumpIfNullKeep(0), sp));
                }
                self.expr(index);
                self.emit(Op::GetIndex, sp);
            }
            ExprKind::Call { callee, args } => {
                if args.len() > u8::MAX as usize {
                    self.error(Diagnostic::new("too many arguments", sp));
                    return;
                }
                if let ExprKind::Member { obj, name, optional } = &callee.kind {
                    self.chain(obj, exits);
                    if *optional {
                        exits.push(self.emit(Op::JumpIfNullKeep(0), sp));
                    }
                    for a in args {
                        self.expr(a);
                    }
                    let k = self.str_const(name);
                    self.emit(Op::CallMethod(k, args.len() as u8), sp);
                } else {
                    self.chain(callee, exits);
                    for a in args {
                        self.expr(a);
                    }
                    self.emit(Op::Call(args.len() as u8), sp);
                }
            }
            _ => self.expr(e),
        }
    }

    /// Assignment and compound assignment. `value` emits the right-hand side.
    fn assign(&mut self, op: Option<BinOp>, target: &Expr, value: impl FnOnce(&mut Self), sp: Span) {
        match &target.kind {
            ExprKind::Ident(name) => {
                if let Some(op) = op {
                    self.load_var(name, sp);
                    value(self);
                    self.emit(bin_op(op), sp);
                } else {
                    value(self);
                }
                self.store_var(name, target.span);
            }
            ExprKind::Member { obj, name, .. } => {
                self.expr(obj);
                let k = self.str_const(name);
                if let Some(op) = op {
                    self.emit(Op::Dup, sp);
                    self.emit(Op::GetField(k), sp);
                    value(self);
                    self.emit(bin_op(op), sp);
                } else {
                    value(self);
                }
                self.emit(Op::SetField(k), sp);
            }
            ExprKind::Index { obj, index, .. } => {
                self.expr(obj);
                self.expr(index);
                if let Some(op) = op {
                    self.emit(Op::Dup2, sp);
                    self.emit(Op::GetIndex, sp);
                    value(self);
                    self.emit(bin_op(op), sp);
                } else {
                    value(self);
                }
                self.emit(Op::SetIndex, sp);
            }
            _ => self.error(Diagnostic::new("cannot assign to this expression", target.span)),
        }
    }

    fn update(&mut self, inc: bool, target: &Expr, sp: Span) {
        let one = self.num_const(1.0);
        self.assign(Some(if inc { BinOp::Add } else { BinOp::Sub }), target, |c| {
            c.emit(Op::Const(one), sp);
        }, sp);
    }

    fn jsx(&mut self, el: &JsxElement) {
        let sp = el.span;
        if let Some(kind) = catalog::kind(&el.tag) {
            let mut names = Vec::with_capacity(el.attrs.len());
            for (name, value) in &el.attrs {
                if !kind.allows(name) {
                    let hint = match crate::syntax::closest(name, kind.all_props()) {
                        Some(c) => format!("did you mean '{c}'?"),
                        None => format!("props of <{}>: {}", el.tag, kind.all_props().join(", ")),
                    };
                    self.error(Diagnostic::new(format!("<{}> has no prop '{name}'", el.tag), value.span).hint(hint));
                }
                self.expr(value);
                names.push(Rc::from(name.as_str()));
            }
            if kind.content == catalog::Content::None && !el.children.is_empty() {
                self.error(Diagnostic::new(format!("<{}> takes no children", el.tag), sp));
            }
            for child in &el.children {
                self.jsx_child(child, sp);
            }
            self.elems.push(ElemDesc { kind: Rc::from(kind.name), props: names, children: el.children.len() as u16 });
            let d = (self.elems.len() - 1) as u32;
            self.emit(Op::Elem(d), sp);
        } else if self.resolve(&el.tag).is_some() {
            // A user component: call it with the props record.
            self.load_var(&el.tag, sp);
            self.emit(Op::NewRec, sp);
            for (name, value) in &el.attrs {
                self.expr(value);
                let k = self.str_const(name);
                self.emit(Op::RecSet(k), value.span);
            }
            if !el.children.is_empty() {
                self.emit(Op::NewArr, sp);
                for child in &el.children {
                    self.jsx_child(child, sp);
                    self.emit(Op::ArrPush, sp);
                }
                let k = self.str_const("children");
                self.emit(Op::RecSet(k), sp);
            }
            self.emit(Op::Call(1), sp);
        } else {
            self.error(
                Diagnostic::new(format!("unknown component <{}>", el.tag), sp).hint(match crate::syntax::closest(&el.tag, catalog::KINDS.iter().map(|k| k.name)) {
                    Some(c) => format!("did you mean <{c}>?"),
                    None => format!("built-in components: {}; or declare `function {}(props)`", catalog::names(), el.tag),
                }),
            );
        }
    }

    fn jsx_child(&mut self, child: &JsxChild, sp: Span) {
        match child {
            JsxChild::Text(t) => {
                let k = self.str_const(t);
                self.emit(Op::Const(k), sp);
            }
            JsxChild::Expr(e) => self.expr(e),
            JsxChild::Element(c) => self.jsx(c),
        }
    }
}

fn bin_op(op: BinOp) -> Op {
    match op {
        BinOp::Add => Op::Add,
        BinOp::Sub => Op::Sub,
        BinOp::Mul => Op::Mul,
        BinOp::Div => Op::Div,
        BinOp::Mod => Op::Mod,
        BinOp::Eq => Op::Eq,
        BinOp::Ne => Op::Ne,
        BinOp::Lt => Op::Lt,
        BinOp::Le => Op::Le,
        BinOp::Gt => Op::Gt,
        BinOp::Ge => Op::Ge,
    }
}

/// `"500ms"`, `"5s"`, `"2m"`, `"1h"`, `"1d"` or a number of milliseconds.
fn duration_ms(e: &Expr) -> Option<f64> {
    match &e.kind {
        ExprKind::Num(n) => Some(*n),
        ExprKind::Str(s) => {
            let s = s.trim();
            let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
            let (n, unit) = s.split_at(split);
            let n: f64 = n.parse().ok()?;
            let mul = match unit.trim() {
                "ms" => 1.0,
                "s" => 1e3,
                "m" => 60e3,
                "h" => 3600e3,
                "d" => 86400e3,
                "y" => 365.0 * 86400e3,
                _ => return None,
            };
            Some(n * mul)
        }
        _ => None,
    }
}

/// Every name mentioned inside functions nested in `f` (at any depth). Locals of `f` with
/// these names are stored in cells. Over-approximating is safe: cells behave like slots.
fn nested_names(f: &FnDecl) -> HashSet<String> {
    let mut w = NameWalker { out: HashSet::new(), depth: 0 };
    w.params_and_body(f);
    w.out
}

struct NameWalker {
    out: HashSet<String>,
    depth: usize,
}

impl NameWalker {
    fn params_and_body(&mut self, f: &FnDecl) {
        for p in &f.params {
            self.pattern(&p.pattern);
            if let Some(d) = &p.default {
                self.expr(d);
            }
        }
        self.stmts(&f.body);
    }

    fn nested(&mut self, f: &FnDecl) {
        self.depth += 1;
        self.params_and_body(f);
        self.depth -= 1;
    }

    fn name(&mut self, n: &str) {
        if self.depth > 0 {
            self.out.insert(n.to_string());
        }
    }

    fn pattern(&mut self, p: &Pattern) {
        match p {
            Pattern::Ident(..) => {}
            Pattern::Record(entries, _) => {
                for (_, sub, d) in entries {
                    self.pattern(sub);
                    if let Some(d) = d {
                        self.expr(d);
                    }
                }
            }
            Pattern::Array(items, _) => items.iter().for_each(|s| self.pattern(s)),
        }
    }

    fn stmts(&mut self, stmts: &[Stmt]) {
        stmts.iter().for_each(|s| self.stmt(s));
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let(d) => {
                self.pattern(&d.pattern);
                if let Some(e) = &d.init {
                    self.expr(e);
                }
            }
            Stmt::Function(f) => self.nested(f),
            Stmt::Expr(e) | Stmt::Throw(e, _) => self.expr(e),
            Stmt::Return(e, _) => {
                if let Some(e) = e {
                    self.expr(e);
                }
            }
            Stmt::If { cond, then, els } => {
                self.expr(cond);
                self.stmts(then);
                if let Some(els) = els {
                    self.stmts(els);
                }
            }
            Stmt::While { cond, body } => {
                self.expr(cond);
                self.stmts(body);
            }
            Stmt::ForOf { pattern, iter, body } => {
                self.pattern(pattern);
                self.expr(iter);
                self.stmts(body);
            }
            Stmt::For { init, cond, update, body } => {
                if let Some(i) = init {
                    self.stmt(i);
                }
                for e in cond.iter().chain(update.iter()) {
                    self.expr(e);
                }
                self.stmts(body);
            }
            Stmt::Block(b) => self.stmts(b),
            Stmt::Try { body, param, handler } => {
                self.stmts(body);
                if let Some(p) = param {
                    self.pattern(p);
                }
                self.stmts(handler);
            }
            Stmt::Break(_) | Stmt::Continue(_) => {}
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Ident(n) => self.name(n),
            ExprKind::This => self.name("this"),
            ExprKind::Num(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::Null => {}
            ExprKind::Template(pieces) => {
                for p in pieces {
                    if let TplPiece::Expr(x) = p {
                        self.expr(x);
                    }
                }
            }
            ExprKind::Array(items) => {
                for i in items {
                    match i {
                        ArrayEntry::Item(x) | ArrayEntry::Spread(x) => self.expr(x),
                    }
                }
            }
            ExprKind::Record(entries) => {
                for en in entries {
                    match en {
                        RecordEntry::Field(_, v) | RecordEntry::Spread(v) => self.expr(v),
                        RecordEntry::Computed(k, v) => {
                            self.expr(k);
                            self.expr(v);
                        }
                    }
                }
            }
            ExprKind::Member { obj, .. } => self.expr(obj),
            ExprKind::Index { obj, index, .. } => {
                self.expr(obj);
                self.expr(index);
            }
            ExprKind::Call { callee, args } => {
                self.expr(callee);
                args.iter().for_each(|a| self.expr(a));
            }
            ExprKind::Unary(_, x) | ExprKind::Await(x) => self.expr(x),
            ExprKind::Binary(_, l, r) | ExprKind::Logical(_, l, r) => {
                self.expr(l);
                self.expr(r);
            }
            ExprKind::Cond(c, a, b) => {
                self.expr(c);
                self.expr(a);
                self.expr(b);
            }
            ExprKind::Assign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            ExprKind::Update { target, .. } => self.expr(target),
            ExprKind::Arrow(f) => self.nested(f),
            ExprKind::Jsx(el) => self.jsx(el),
        }
    }

    fn jsx(&mut self, el: &JsxElement) {
        self.name(&el.tag);
        for (_, v) in &el.attrs {
            self.expr(v);
        }
        for c in &el.children {
            match c {
                JsxChild::Text(_) => {}
                JsxChild::Expr(e) => self.expr(e),
                JsxChild::Element(e) => self.jsx(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn errors(src: &str) -> Vec<String> {
        match compile(src) {
            Ok(_) => Vec::new(),
            Err(e) => e.into_iter().map(|d| d.message).collect(),
        }
    }

    #[test]
    fn prelude_compiles() {
        compile("").unwrap();
    }

    #[test]
    fn reports_unknown_names_and_const_assignment() {
        assert_eq!(errors("function f() { return y }"), vec!["unknown name 'y'"]);
        assert_eq!(errors("function f() { const a = 1; a = 2 }"), vec!["cannot assign to 'a'"]);
        assert_eq!(errors("function f() { return this }"), vec!["`this` is only available inside object methods"]);
    }

    #[test]
    fn checks_components_and_props() {
        assert_eq!(errors("function App() { return <Text colour=\"red\">x</Text> }"), vec!["<Text> has no prop 'colour'"]);
        assert_eq!(errors("function App() { return <Box /> }"), vec!["unknown component <Box>"]);
        let hints = |src: &str| compile(src).err().unwrap().into_iter().filter_map(|d| d.hint).collect::<Vec<_>>();
        assert_eq!(hints("function App() { return <Txt>x</Txt> }"), vec!["did you mean <Text>?"]);
        assert_eq!(hints("function App() { return <Text colr=\"red\">x</Text> }"), vec!["did you mean 'color'?"]);
        assert_eq!(hints("function f() { const total = 1; return totl }"), vec!["did you mean 'total'?"]);
        assert_eq!(hints("type T = { text: string }\nfunction f(): T { return { txt: \"a\" } }"), vec!["did you mean 'text'?"]);
        assert!(errors("function Card(p) { return <Text>{p.title}</Text> }\nfunction App() { return <Card title=\"x\" /> }").is_empty());
    }

    #[test]
    fn checks_objects() {
        assert_eq!(errors("object A { id = 1 }"), vec!["'id' is reserved on objects"]);
        assert_eq!(errors("object A { @every(\"soon\") t() {} }"), vec!["@every needs one duration of at least 10ms"]);
        assert!(errors("object A { n = 0\n @every(\"1s\") t() { this.n += 1 } }").is_empty());
    }

    #[test]
    fn durations() {
        let e = |s: &str| Expr { kind: ExprKind::Str(s.into()), span: Span::default() };
        assert_eq!(duration_ms(&e("1.5s")), Some(1500.0));
        assert_eq!(duration_ms(&e("2m")), Some(120_000.0));
        assert_eq!(duration_ms(&e("5 parsecs")), None);
    }
}

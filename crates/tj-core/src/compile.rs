//! AST to bytecode.
//!
//! Locals live in frame slots. A local that any nested function mentions is stored in a
//! heap cell instead (`MakeCell`), and closures capture the cell, which gives closures
//! JavaScript's by-reference semantics without a separate escape analysis.

use crate::syntax::ast::*;
use crate::syntax::{parse, Diagnostic, Span};
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
const OBJECT_RESERVED: &[&str] = &["id", "emit", "schedule"];
const MAX_ERRORS: usize = 20;

/// Compiles a source file (plus the prelude) to a program.
pub fn compile(src: &str) -> Result<Program, Vec<Diagnostic>> {
    let prelude = parse(PRELUDE).expect("prelude parses");
    let user = parse(src).map_err(|e| vec![e])?;
    let mut c = Compiler::new();
    c.declare(&prelude, Source::Prelude);
    c.declare(&user, Source::User);
    let main = vec![c.main(&prelude, Source::Prelude), c.main(&user, Source::User)];
    if !c.errors.is_empty() {
        return Err(c.errors);
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
}

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

#[derive(Clone, Copy, PartialEq)]
enum FnKind {
    Plain,
    Method,
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
        self.globals.insert(name.to_string(), Global { slot, mutable, kind });
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
        if let Some(g) = self.globals.get(name) {
            let what = if g.kind == GKind::Builtin { "a built-in" } else { "already declared" };
            self.error(Diagnostic::new(format!("'{name}' is {what}"), span));
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
                        self.objects.push(ObjectDef { name: Rc::from(o.name.as_str()), fields: Vec::new(), init: 0, methods: IndexMap::new() });
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
                Item::TypeAlias { .. } => {}
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

    fn object(&mut self, o: &ObjectDecl, class: usize) {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut fields = Vec::new();
        for f in &o.fields {
            if OBJECT_RESERVED.contains(&f.name.as_str()) {
                self.error(Diagnostic::new(format!("'{}' is reserved on objects", f.name), f.span).hint("`this.id`, `this.emit()` and `this.schedule()` are built in"));
            } else if !seen.insert(&f.name) {
                self.error(Diagnostic::new(format!("'{}' is declared twice", f.name), f.span));
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
            for d in &m.decorators {
                match d.name.as_str() {
                    "every" => match d.args.first().and_then(duration_ms) {
                        Some(ms) if d.args.len() == 1 && ms >= 10.0 => every = Some(ms),
                        _ => self.error(Diagnostic::new("@every needs one duration of at least 10ms", d.span).hint("e.g. @every(\"5s\"), @every(\"500ms\"), @every(\"1h\")")),
                    },
                    other => self.error(Diagnostic::new(format!("unknown decorator @{other}"), d.span).hint("supported: @every(\"5s\")")),
                }
            }
            let proto = self.function(&m.func, FnKind::Method);
            methods.insert(Rc::from(name), MethodDef { proto, private: m.private, every });
        }
        let def = &mut self.objects[class];
        def.fields = fields;
        def.init = init;
        def.methods = methods;
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
        if kind == FnKind::Method {
            self.declare_local("this", false, f.span);
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
            Diagnostic::new(format!("unknown name '{name}'"), span).hint("declare it with `let`, `const` or `function` before using it")
        };
        self.error(d);
    }

    fn load_var(&mut self, name: &str, span: Span) {
        match self.resolve(name) {
            Some(Var::Local { slot, cell: false, .. }) => self.emit(Op::LoadLocal(slot), span),
            Some(Var::Local { slot, cell: true, .. }) => self.emit(Op::LoadCell(slot), span),
            Some(Var::Upval { idx, .. }) => self.emit(Op::LoadUpval(idx), span),
            Some(Var::Global { slot, .. }) => self.emit(Op::LoadGlobal(slot), span),
            None => {
                self.unknown_name(name, span);
                self.emit(Op::Null, span)
            }
        };
    }

    fn store_var(&mut self, name: &str, span: Span) {
        let Some(var) = self.resolve(name) else {
            self.unknown_name(name, span);
            return;
        };
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
                    self.error(Diagnostic::new(format!("<{}> has no prop '{name}'", el.tag), value.span).hint(format!("props of <{}>: {}", el.tag, kind.all_props().join(", "))));
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
                Diagnostic::new(format!("unknown component <{}>", el.tag), sp)
                    .hint(format!("built-in components: {}; or declare `function {}(props)`", catalog::names(), el.tag)),
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
                _ => return None,
            };
            Some(n * mul)
        }
        _ => None,
    }
}

fn pattern_names(p: &Pattern, out: &mut Vec<(String, Span)>) {
    match p {
        Pattern::Ident(n, s) => out.push((n.clone(), *s)),
        Pattern::Record(entries, _) => entries.iter().for_each(|(_, sub, _)| pattern_names(sub, out)),
        Pattern::Array(items, _) => items.iter().for_each(|sub| pattern_names(sub, out)),
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

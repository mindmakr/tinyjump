//! The personal-data flow check (docs/privacy.md, part 2): every compile follows `@pii`
//! values through the code and reports each path to a place where they would leave their
//! owner, unless the code says why with `reveal(value, "purpose")`.
//!
//! - **Values, not types:** a value carries the personal fields it came from, through
//!   locals, operators, templates, containers, `JSON.stringify` and calls, so `any` does
//!   not hide it. Comparing a field with `""` or `null` gives a clean boolean.
//! - **Handles are not data:** `Account.get(id)` is a handle; reading `plan` through it is
//!   clean, reading `email` is personal data, and using the whole handle as data is all of
//!   its personal fields.
//! - **Subjects:** data read through `this`, `X.get(this.id)`, or `X.get(caller.id)` in a
//!   method only the owner may call, belongs to the caller's own object; anything else may
//!   be someone else's, and owner-only returns and personal fields refuse it.
//! - **Containers are shared cells,** so `box.push(this.email)` taints every alias of `box`,
//!   including through function parameters.
//! - **Calls** to the app's functions and methods are followed with the values passed in.
//!   A function value it cannot know is reported only when personal data goes into it.
//! - **Sinks:** `fetch`, plugins (and the tools an agent calls, which return to the model),
//!   `emit`, `throw`, logs, alarm arguments, fields that are not personal data, module-level
//!   variables, and the return value of a method someone other than the owner may call.
//!
//! Apps without `@pii` skip it entirely.

use crate::syntax::ast::*;
use crate::syntax::Diagnostic;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

/// A personal-data field: class and field name.
type Field = (Rc<str>, Rc<str>);

#[derive(Clone, Default, Debug)]
struct Taint {
    fields: BTreeSet<Field>,
    /// Some of it may be about someone other than the caller's own object.
    other: bool,
    /// How the first source was written, for messages.
    from: Option<Rc<str>>,
    /// Personal data let out with `reveal`: no longer an error anywhere, but where it is
    /// sent is recorded (transfers).
    revealed: BTreeSet<Field>,
}

impl Taint {
    fn is_clean(&self) -> bool {
        self.fields.is_empty()
    }

    /// Nothing personal at all, revealed or not.
    fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.revealed.is_empty()
    }

    fn join(&mut self, o: &Taint) {
        self.revealed.extend(o.revealed.iter().cloned());
        if o.is_clean() {
            return;
        }
        self.fields.extend(o.fields.iter().cloned());
        self.other |= o.other;
        if self.from.is_none() {
            self.from.clone_from(&o.from);
        }
    }

    fn sig(&self) -> String {
        let names = |s: &BTreeSet<Field>| s.iter().map(|(c, f)| format!("{c}.{f}")).collect::<Vec<_>>().join(",");
        format!("{}{}/{}", names(&self.fields), if self.other { "!" } else { "" }, names(&self.revealed))
    }
}

#[derive(Clone)]
enum Val {
    Data(Taint),
    Handle { class: Rc<str>, other: bool },
    /// A list of a class's objects (`Class.all()`), which may be anyone's: each element is a
    /// handle, so only their `@pii` fields count as personal data.
    Handles { class: Rc<str> },
    /// An array or record: the taint of everything put in it, shared by its aliases.
    Cell(Rc<RefCell<Taint>>),
    Func(Rc<FnDecl>),
}

fn clean() -> Val {
    Val::Data(Taint::default())
}

struct Class {
    /// Field -> (category, purposes).
    pii: HashMap<Rc<str>, (String, Vec<String>)>,
    /// Method -> (declaration, `@allow` words).
    methods: HashMap<String, (Rc<FnDecl>, Option<Vec<String>>)>,
}

/// Whose return value a method hands out.
#[derive(Clone, Copy, PartialEq)]
enum Endpoint {
    /// No `@allow`: only server code calls it.
    Internal,
    OwnerOnly,
    Others,
}

struct Frame {
    vars: HashMap<String, Val>,
    this: Option<(Rc<str>, bool)>,
    owner_only: bool,
    endpoint: Endpoint,
    ret: Taint,
}

struct Checker {
    classes: HashMap<Rc<str>, Class>,
    functions: HashMap<String, Rc<FnDecl>>,
    /// Functions of plugin modules (`tj:<name>`), by name: the module.
    plugin_fns: HashMap<String, String>,
    globals: HashSet<String>,
    global_vals: HashMap<String, Val>,
    errors: Vec<Diagnostic>,
    seen: HashSet<(u32, u32, String)>,
    memo: HashMap<String, Taint>,
    active: HashSet<String>,
    /// Every place personal data is let out on purpose, for the record of processing.
    reveals: Vec<Reveal>,
    /// Every place revealed personal data leaves the server.
    transfers: Vec<Transfer>,
    /// Revealed data written into fields that are not personal (a queue, a cache): read
    /// back from them, it is still followed.
    field_revealed: HashMap<Field, BTreeSet<Field>>,
    /// Purposes whose lawful basis is consent (`const lawfulBasis`).
    consent_purposes: HashSet<String>,
    /// The consents each method checks first (`@needs`), by the method's declaration.
    needs_of: HashMap<*const FnDecl, Vec<String>>,
    /// The consents checked in the code being followed: the method's, and its closures'.
    needs: Vec<Vec<String>>,
}

/// Where personal data goes when it leaves the server.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Destination {
    /// A `fetch` over HTTPS to this host, written as a constant in the code.
    Host(String),
    /// A `fetch` over plain HTTP to this host: readable on the way.
    Cleartext(String),
    /// A plugin module, such as `tj:ai`.
    Plugin(String),
    /// A `fetch` whose host is not a constant in the code.
    Unknown,
}

/// Revealed personal data sent somewhere: where in the source, to whom, and which fields.
#[derive(Debug, Clone, PartialEq)]
pub struct Transfer {
    pub at: u32,
    pub to: Destination,
    pub fields: Vec<String>,
}

/// The host a `fetch` URL names, when the code fixes it: a string, or a template or `+`
/// whose text up to the end of the host is written out.
fn destination(url: &Expr) -> Destination {
    let (start, whole) = constant_start(url);
    let (rest, secure) = match (start.strip_prefix("https://"), start.strip_prefix("http://")) {
        (Some(rest), _) => (rest, true),
        (None, Some(rest)) => (rest, false),
        _ => return Destination::Unknown,
    };
    // The authority runs to the path, query or fragment, which must be in the constant
    // text (or the text must be the whole URL); a user part (`a@b`) could hide the real host.
    let authority = match rest.find(['/', '?', '#']) {
        Some(i) => &rest[..i],
        None if whole => rest,
        None => return Destination::Unknown,
    };
    if authority.is_empty() || authority.contains('@') {
        return Destination::Unknown;
    }
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split_once(']').map(|(h, _)| format!("[{h}]")),
        None => authority.split(':').next().map(str::to_string),
    };
    match host.filter(|h| !h.is_empty()) {
        Some(h) if secure => Destination::Host(h.to_ascii_lowercase()),
        Some(h) => Destination::Cleartext(h.to_ascii_lowercase()),
        None => Destination::Unknown,
    }
}

/// The constant text an expression starts with, and whether that text is all of it.
fn constant_start(e: &Expr) -> (&str, bool) {
    match &e.kind {
        ExprKind::Str(s) => (s, true),
        ExprKind::Template(pieces) => match pieces.as_slice() {
            [crate::syntax::ast::TplPiece::Str(s)] => (s, true),
            [crate::syntax::ast::TplPiece::Str(s), ..] => (s, false),
            _ => ("", false),
        },
        ExprKind::Binary(crate::syntax::ast::BinOp::Add, left, _) => (constant_start(left).0, false),
        _ => ("", false),
    }
}

/// A `reveal(value, purpose)`: where (a byte offset in the app's source), for what, and
/// which personal fields it lets out.
#[derive(Debug, Clone, PartialEq)]
pub struct Reveal {
    pub at: u32,
    pub purpose: String,
    pub fields: Vec<String>,
}

const CALLBACK_METHODS: &[&str] = &["map", "filter", "find", "findIndex", "some", "every", "forEach", "reduce", "flatMap", "sort"];
const MUTATORS: &[&str] = &["push", "unshift", "splice"];

fn words(d: &Decorator) -> Vec<String> {
    d.args.iter().filter_map(|a| if let ExprKind::Str(s) = &a.kind { Some(s.clone()) } else { None }).collect()
}

fn has_pii(p: &Program) -> bool {
    p.items.iter().any(|i| matches!(i, Item::Object(o) if o.fields.iter().any(|f| f.decorators.iter().any(|d| d.name == "pii"))))
}

/// Checks the files of an app (name and parsed file, plugin modules included).
pub fn check(files: &[(&str, &Program)]) -> Vec<Diagnostic> {
    analyze(files).map(|c| c.errors).unwrap_or_default()
}

/// Every `reveal` in the app, and every place revealed personal data leaves the server,
/// in source order.
pub fn reveals_and_transfers(files: &[(&str, &Program)]) -> (Vec<Reveal>, Vec<Transfer>) {
    let Some(c) = analyze(files) else { return Default::default() };
    // The check follows each path through the code, so one place is seen many times: one
    // entry per place (and purpose or destination), with every field it lets out.
    let mut reveals: Vec<Reveal> = Vec::new();
    for r in c.reveals {
        match reveals.iter_mut().find(|x| x.at == r.at && x.purpose == r.purpose) {
            Some(x) => x.fields.extend(r.fields),
            None => reveals.push(r),
        }
    }
    let mut transfers: Vec<Transfer> = Vec::new();
    for t in c.transfers {
        match transfers.iter_mut().find(|x| x.at == t.at && x.to == t.to) {
            Some(x) => x.fields.extend(t.fields),
            None => transfers.push(t),
        }
    }
    for f in reveals.iter_mut().map(|r| &mut r.fields).chain(transfers.iter_mut().map(|t| &mut t.fields)) {
        f.sort();
        f.dedup();
    }
    reveals.sort_by_key(|x| x.at);
    transfers.sort_by(|a, b| (a.at, &a.to).cmp(&(b.at, &b.to)));
    (reveals, transfers)
}

fn analyze(files: &[(&str, &Program)]) -> Option<Checker> {
    if !files.iter().any(|(_, p)| has_pii(p)) {
        return None;
    }
    let mut c = Checker { classes: HashMap::new(), functions: HashMap::new(), plugin_fns: HashMap::new(), globals: HashSet::new(), global_vals: HashMap::new(), errors: Vec::new(), seen: HashSet::new(), memo: HashMap::new(), active: HashSet::new(), reveals: Vec::new(), transfers: Vec::new(), field_revealed: HashMap::new(), consent_purposes: HashSet::new(), needs_of: HashMap::new(), needs: Vec::new() };
    c.consent_purposes = crate::lawful_basis::declared(files).unwrap_or_default().into_iter().filter(|(_, b)| b == "consent").map(|(p, _)| p).collect();
    let user: Vec<&Program> = files.iter().filter(|(name, _)| !name.starts_with("tj:")).map(|(_, p)| *p).collect();
    for (name, p) in files.iter().filter(|(name, _)| name.starts_with("tj:")) {
        for item in &p.items {
            if let Item::Function(f) = item {
                c.plugin_fns.insert(f.name.clone(), name.to_string());
            }
        }
    }
    for p in &user {
        for item in &p.items {
            match item {
                Item::Function(f) => {
                    c.functions.insert(f.name.clone(), f.clone());
                }
                Item::Let(d) => {
                    let mut names = Vec::new();
                    pattern_names(&d.pattern, &mut names);
                    c.globals.extend(names.into_iter().map(|(n, _)| n));
                }
                Item::Object(o) => {
                    let mut pii = HashMap::new();
                    for f in &o.fields {
                        if let Some(d) = f.decorators.iter().find(|d| d.name == "pii") {
                            let w = words(d);
                            if let Some((cat, purposes)) = w.split_first() {
                                pii.insert(Rc::from(f.name.as_str()), (cat.clone(), purposes.to_vec()));
                            }
                        }
                    }
                    for m in &o.methods {
                        let needs: Vec<String> = m.decorators.iter().filter(|d| d.name == "needs").flat_map(words).filter_map(|w| w.strip_prefix("consent:").map(str::to_string)).collect();
                        c.needs_of.insert(Rc::as_ptr(&m.func), needs);
                    }
                    let methods = o.methods.iter().map(|m| (m.func.name.clone(), (m.func.clone(), m.decorators.iter().find(|d| d.name == "allow").map(words)))).collect();
                    c.classes.insert(Rc::from(o.name.as_str()), Class { pii, methods });
                }
                Item::TypeAlias { .. } => {}
            }
        }
    }
    // Module-level values first, then every method as an entry point, then every function
    // (components included, whose event handlers run when people use the app).
    let mut top = Frame { vars: HashMap::new(), this: None, owner_only: false, endpoint: Endpoint::Internal, ret: Taint::default() };
    for p in &user {
        for item in &p.items {
            if let Item::Let(d) = item {
                let v = d.init.as_ref().map_or_else(clean, |e| c.expr(e, &mut top));
                let mut names = Vec::new();
                pattern_names(&d.pattern, &mut names);
                for (n, _) in names {
                    c.global_vals.insert(n, v.clone());
                }
            }
        }
    }
    let mut entries = Vec::new();
    for (class, info) in &c.classes {
        for (decl, allow) in info.methods.values() {
            let endpoint = match allow {
                None => Endpoint::Internal,
                Some(w) if w.iter().all(|w| w == "owner") => Endpoint::OwnerOnly,
                Some(_) => Endpoint::Others,
            };
            entries.push((decl.clone(), Some((class.clone(), false)), endpoint));
        }
    }
    for f in c.functions.values() {
        entries.push((f.clone(), None, Endpoint::Internal));
    }
    // Twice, so data parked in a field or module variable by one entry point is followed
    // when another reads it; the second pass starts afresh but keeps what was parked.
    for _ in 0..2 {
        c.memo.clear();
        for (decl, this, endpoint) in &entries {
            let args = vec![clean(); decl.params.len()];
            c.call(decl, args, this.clone(), *endpoint, None);
        }
    }
    c.errors.sort_by_key(|d| d.span.start);
    Some(c)
}

impl Checker {
    fn report(&mut self, d: Diagnostic) {
        if self.seen.insert((d.span.start, d.span.end, d.message.clone())) {
            self.errors.push(d);
        }
    }

    /// Every personal field of a class's objects, as one value.
    fn whole(&self, class: &Rc<str>, other: bool) -> Taint {
        let Some(info) = self.classes.get(class) else { return Taint::default() };
        let fields: BTreeSet<Field> = info.pii.keys().map(|f| (class.clone(), f.clone())).collect();
        let from = (!fields.is_empty()).then(|| Rc::from(format!("the whole {class} object")));
        Taint { fields, other, from, ..Taint::default() }
    }

    fn flatten(&self, v: &Val) -> Taint {
        match v {
            Val::Data(t) => t.clone(),
            Val::Cell(c) => c.borrow().clone(),
            Val::Func(_) => Taint::default(),
            Val::Handle { class, other } => self.whole(class, *other),
            Val::Handles { class } => self.whole(class, true),
        }
    }

    fn describe(&self, t: &Taint) -> String {
        let from = t.from.as_deref().unwrap_or("personal data");
        let (class, field) = t.fields.iter().next().cloned().unwrap_or_default();
        match self.classes.get(&class).and_then(|c| c.pii.get(&field)) {
            Some((cat, purposes)) => format!("`{from}` (personal data: {cat}, for {})", purposes.join(", ")),
            None => format!("`{from}` (personal data)"),
        }
    }

    fn purposes(&self, t: &Taint) -> Vec<String> {
        let mut common: Option<Vec<String>> = None;
        for (class, field) in &t.fields {
            let p = self.classes.get(class).and_then(|c| c.pii.get(field)).map(|(_, p)| p.clone()).unwrap_or_default();
            common = Some(match common {
                None => p,
                Some(c) => c.into_iter().filter(|x| p.contains(x)).collect(),
            });
        }
        common.unwrap_or_default()
    }

    fn reveal_hint(&self, t: &Taint) -> String {
        let purposes = self.purposes(t);
        match purposes.as_slice() {
            [] => "these fields are for different purposes: reveal each for its own purpose before combining them".into(),
            [p] => format!("if this is intended, write reveal(value, \"{p}\"); otherwise do not send it"),
            many => format!("if this is intended, write reveal(value, \"<purpose>\") with the purpose it is sent for ({}); otherwise do not send it", many.join(", ")),
        }
    }

    /// Records where revealed personal data in `v` is sent.
    fn transfer(&mut self, v: &Val, span: crate::syntax::Span, to: &Destination) {
        let t = self.flatten(v);
        if !t.revealed.is_empty() {
            let fields = t.revealed.iter().map(|(c, f)| format!("{c}.{f}")).collect();
            self.transfers.push(Transfer { at: span.start, to: to.clone(), fields });
        }
    }

    /// Personal data must not reach `what` without `reveal`.
    fn sink(&mut self, v: &Val, span: crate::syntax::Span, what: &str) {
        let t = self.flatten(v);
        if !t.is_clean() {
            let d = Diagnostic::new(format!("{} reaches {what} here", self.describe(&t)), span).hint(self.reveal_hint(&t));
            self.report(d);
        }
    }

    /// Runs a function with these argument values; its return taint. A function written
    /// inside another (`captured`) sees that one's variables, and what it puts into them is
    /// written back.
    fn call(&mut self, f: &Rc<FnDecl>, args: Vec<Val>, this: Option<(Rc<str>, bool)>, endpoint: Endpoint, captured: Option<&mut HashMap<String, Val>>) -> Taint {
        let arg_sig: Vec<String> = args.iter().map(|a| self.flatten(a).sig()).collect();
        let key = format!("{:p}|{}|{:?}|{}", Rc::as_ptr(f), arg_sig.join(";"), this.as_ref().map(|(c, o)| format!("{c}{o}")), endpoint as u8);
        // A container passed in may be written into: run again for each call, so every
        // container that is given personal data gets it.
        let pure = captured.is_none() && !args.iter().any(|a| matches!(a, Val::Cell(_)));
        if pure {
            if let Some(t) = self.memo.get(&key) {
                return t.clone();
            }
        }
        if !self.active.insert(key.clone()) {
            // Recursion: assume everything given comes back.
            let mut t = Taint::default();
            args.iter().for_each(|a| t.join(&self.flatten(a)));
            return t;
        }
        // A closure checks what the code around it checked; a method, its own @needs.
        let needs = match &captured {
            Some(_) => self.needs.last().cloned().unwrap_or_default(),
            None => self.needs_of.get(&Rc::as_ptr(f)).cloned().unwrap_or_default(),
        };
        self.needs.push(needs);
        let owner_only = endpoint == Endpoint::OwnerOnly;
        let mut fr = Frame { vars: captured.as_deref().cloned().unwrap_or_default(), this, owner_only, endpoint, ret: Taint::default() };
        for (i, p) in f.params.iter().enumerate() {
            let mut v = args.get(i).cloned().unwrap_or_else(clean);
            if let Some(d) = &p.default {
                let dv = self.expr(d, &mut fr);
                v = self.merge(Some(&v), dv);
            }
            self.bind(&p.pattern, v, &mut fr);
        }
        // Twice, so what a loop puts into a variable late reaches uses earlier in it.
        for _ in 0..2 {
            self.block(&f.body, &mut fr);
        }
        self.needs.pop();
        self.active.remove(&key);
        if pure {
            self.memo.insert(key, fr.ret.clone());
        }
        if let Some(outer) = captured {
            for (name, v) in outer.iter_mut() {
                if let Some(inner) = fr.vars.get(name) {
                    *v = self.merge(Some(v), inner.clone());
                }
            }
        }
        fr.ret
    }

    fn bind(&mut self, p: &Pattern, v: Val, fr: &mut Frame) {
        match p {
            Pattern::Ident(n, _) => {
                let merged = self.merge(fr.vars.get(n), v);
                fr.vars.insert(n.clone(), merged);
            }
            _ => {
                let t = self.flatten(&v);
                let mut names = Vec::new();
                pattern_names(p, &mut names);
                for (n, _) in names {
                    let merged = self.merge(fr.vars.get(&n), Val::Data(t.clone()));
                    fr.vars.insert(n, merged);
                }
            }
        }
    }

    /// A variable's value after another assignment: nothing it held is forgotten.
    fn merge(&self, old: Option<&Val>, new: Val) -> Val {
        match old {
            None => new,
            Some(Val::Cell(c)) => {
                // Read first: `new` may be this same container.
                let t = self.flatten(&new);
                c.borrow_mut().join(&t);
                Val::Cell(c.clone())
            }
            Some(old) => {
                let t = self.flatten(&new);
                if t.is_empty() {
                    return old.clone();
                }
                let mut o = self.flatten(old);
                if o.is_empty() {
                    return new;
                }
                o.join(&t);
                Val::Data(o)
            }
        }
    }

    fn block(&mut self, stmts: &[Stmt], fr: &mut Frame) {
        for s in stmts {
            self.stmt(s, fr);
        }
    }

    fn stmt(&mut self, s: &Stmt, fr: &mut Frame) {
        match s {
            Stmt::Let(d) => {
                let v = d.init.as_ref().map_or_else(clean, |e| self.expr(e, fr));
                self.bind(&d.pattern, v, fr);
            }
            Stmt::Function(f) => {
                fr.vars.insert(f.name.clone(), Val::Func(f.clone()));
            }
            Stmt::Expr(e) => {
                self.expr(e, fr);
            }
            Stmt::Return(e, span) => {
                let Some(e) = e else { return };
                let v = self.expr(e, fr);
                let t = self.flatten(&v);
                fr.ret.join(&t);
                if t.is_clean() {
                    return;
                }
                match fr.endpoint {
                    Endpoint::Others => {
                        let d = Diagnostic::new(format!("{} is returned here to callers who do not own it", self.describe(&t)), *span).hint(self.reveal_hint(&t));
                        self.report(d);
                    }
                    Endpoint::OwnerOnly if t.other => {
                        let d = Diagnostic::new(format!("{} may be about someone other than the caller, and is returned here", self.describe(&t)), *span)
                            .hint("read it through this object, or through X.get(caller.id); if returning someone else's data is intended, reveal it with its purpose");
                        self.report(d);
                    }
                    _ => {}
                }
            }
            Stmt::If { cond, then, els } => {
                self.expr(cond, fr);
                self.block(then, fr);
                if let Some(e) = els {
                    self.block(e, fr);
                }
            }
            Stmt::While { cond, body } => {
                self.expr(cond, fr);
                self.block(body, fr);
            }
            Stmt::ForOf { pattern, iter, body } => {
                let v = self.expr(iter, fr);
                let elem = match &v {
                    Val::Handles { class } => Val::Handle { class: class.clone(), other: true },
                    v => Val::Data(self.flatten(v)),
                };
                self.bind(pattern, elem, fr);
                self.block(body, fr);
            }
            Stmt::For { init, cond, update, body } => {
                if let Some(i) = init {
                    self.stmt(i, fr);
                }
                if let Some(c) = cond {
                    self.expr(c, fr);
                }
                self.block(body, fr);
                if let Some(u) = update {
                    self.expr(u, fr);
                }
            }
            Stmt::Block(b) => self.block(b, fr),
            Stmt::Throw(e, span) => {
                let v = self.expr(e, fr);
                self.sink(&v, *span, "an error message (`throw`)");
            }
            Stmt::Try { body, param, handler } => {
                self.block(body, fr);
                if let Some(p) = param {
                    self.bind(p, clean(), fr);
                }
                self.block(handler, fr);
            }
            Stmt::Break(_) | Stmt::Continue(_) => {}
        }
    }

    fn union(&mut self, exprs: &[&Expr], fr: &mut Frame) -> Taint {
        let mut t = Taint::default();
        for e in exprs {
            let v = self.expr(e, fr);
            t.join(&self.flatten(&v));
        }
        t
    }

    fn expr(&mut self, e: &Expr, fr: &mut Frame) -> Val {
        match &e.kind {
            ExprKind::Num(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::Null | ExprKind::Update { .. } => clean(),
            ExprKind::Template(pieces) => {
                let exprs: Vec<&Expr> = pieces.iter().filter_map(|p| if let TplPiece::Expr(e) = p { Some(e) } else { None }).collect();
                Val::Data(self.union(&exprs, fr))
            }
            ExprKind::Ident(n) => {
                if let Some(v) = fr.vars.get(n) {
                    v.clone()
                } else if let Some(v) = self.global_vals.get(n) {
                    v.clone()
                } else if let Some(f) = self.functions.get(n) {
                    Val::Func(f.clone())
                } else {
                    clean()
                }
            }
            ExprKind::This => match &fr.this {
                Some((class, other)) => Val::Handle { class: class.clone(), other: *other },
                None => clean(),
            },
            ExprKind::Array(entries) => {
                let exprs: Vec<&Expr> = entries.iter().map(|a| match a {
                    ArrayEntry::Item(e) | ArrayEntry::Spread(e) => e,
                }).collect();
                Val::Cell(Rc::new(RefCell::new(self.union(&exprs, fr))))
            }
            ExprKind::Record(entries) => {
                self.skill(entries, e.span);
                let exprs: Vec<&Expr> = entries.iter().map(|r| match r {
                    RecordEntry::Field(_, e) | RecordEntry::Computed(_, e) | RecordEntry::Spread(e) => e,
                }).collect();
                Val::Cell(Rc::new(RefCell::new(self.union(&exprs, fr))))
            }
            ExprKind::Member { obj, name, .. } => {
                if matches!(obj.kind, ExprKind::This) && name == "id" {
                    return clean();
                }
                let v = self.expr(obj, fr);
                match v {
                    Val::Handle { class, other } => {
                        let personal = self.classes.get(&class).is_some_and(|c| c.pii.contains_key(name.as_str()));
                        if !personal {
                            // Revealed data written here earlier (a queue of mail to send, say).
                            let revealed = self.field_revealed.get(&(class.clone(), Rc::from(name.as_str()))).cloned().unwrap_or_default();
                            return Val::Data(Taint { revealed, ..Taint::default() });
                        }
                        let from = if matches!(obj.kind, ExprKind::This) { format!("this.{name}") } else { format!("{class}.{name}") };
                        let fields = BTreeSet::from([(class.clone(), Rc::from(name.as_str()))]);
                        Val::Data(Taint { fields, other, from: Some(Rc::from(from)), ..Taint::default() })
                    }
                    v => Val::Data(self.flatten(&v)),
                }
            }
            ExprKind::Index { obj, index, .. } => {
                let v = self.expr(obj, fr);
                self.expr(index, fr);
                Val::Data(self.flatten(&v))
            }
            ExprKind::Call { callee, args } => self.call_expr(callee, args, e.span, fr),
            ExprKind::Unary(_, x) => {
                let v = self.expr(x, fr);
                Val::Data(self.flatten(&v))
            }
            ExprKind::Binary(op, a, b) => {
                let empty = |x: &Expr| matches!(&x.kind, ExprKind::Null) || matches!(&x.kind, ExprKind::Str(s) if s.is_empty());
                let t = self.union(&[a, b], fr);
                // Whether a field is filled in says nothing a reader cannot already see.
                if matches!(op, BinOp::Eq | BinOp::Ne) && (empty(a) || empty(b)) {
                    clean()
                } else {
                    Val::Data(t)
                }
            }
            ExprKind::Logical(_, a, b) => Val::Data(self.union(&[a, b], fr)),
            ExprKind::Cond(c, a, b) => {
                self.expr(c, fr);
                Val::Data(self.union(&[a, b], fr))
            }
            ExprKind::Assign { target, value, .. } => {
                let v = self.expr(value, fr);
                self.consent_to_store(target, value, e.span, fr);
                self.assign(target, v.clone(), e.span, fr);
                v
            }
            ExprKind::Arrow(f) => {
                // Run it once as written (event handlers, callbacks stored for later).
                let args = vec![clean(); f.params.len()];
                let this = fr.this.clone();
                self.call(f, args, this, Endpoint::Internal, Some(&mut fr.vars));
                Val::Func(f.clone())
            }
            ExprKind::Await(x) => self.expr(x, fr),
            ExprKind::Jsx(el) => {
                self.jsx(el, fr);
                clean()
            }
        }
    }

    fn jsx(&mut self, el: &JsxElement, fr: &mut Frame) {
        for (_, v) in &el.attrs {
            self.expr(v, fr);
        }
        for c in &el.children {
            match c {
                JsxChild::Expr(e) => {
                    self.expr(e, fr);
                }
                JsxChild::Element(el) => self.jsx(el, fr),
                JsxChild::Text(_) => {}
            }
        }
    }

    /// The base of `a.b[c].d`, and the field of the base it writes into (`b`), if any.
    fn root(target: &Expr) -> (&Expr, Option<&str>) {
        let mut cur = target;
        let mut field = None;
        loop {
            match &cur.kind {
                ExprKind::Member { obj, name, .. } => {
                    field = Some(name.as_str());
                    cur = obj;
                }
                ExprKind::Index { obj, .. } => cur = obj,
                _ => return (cur, field),
            }
        }
    }

    /// `v` written into `field` of an object: its own (`this`), or another through a handle.
    fn write_field(&mut self, class: &Rc<str>, own: bool, field: &str, v: &Val, span: crate::syntax::Span) {
        let t = self.flatten(v);
        if !t.revealed.is_empty() {
            self.field_revealed.entry((class.clone(), Rc::from(field))).or_default().extend(t.revealed.iter().cloned());
        }
        if t.is_clean() {
            return;
        }
        let place = if own { format!("this.{field}") } else { format!("{class}.{field}") };
        let personal = self.classes.get(class).is_some_and(|c| c.pii.contains_key(field));
        if !personal {
            let d = Diagnostic::new(format!("{} reaches `{place}`, which is not marked as personal data", self.describe(&t)), span)
                .hint(format!("mark `{field}` with @pii(category, purpose), or {}", self.reveal_hint(&t)));
            self.report(d);
        } else if t.other {
            let d = Diagnostic::new(format!("{} may be about someone other than the owner of the object it is written into (`{place}`)", self.describe(&t)), span)
                .hint("read it through this object, or through X.get(caller.id); if copying someone else's data is intended, reveal it with its purpose");
            self.report(d);
        }
    }

    /// Writing into this object's field held only for purposes that rest on consent (collecting
    /// it) needs one of those consents checked. Clearing it to a literal does not, so data can
    /// be dropped once consent is withdrawn.
    fn consent_to_store(&mut self, target: &Expr, value: &Expr, span: crate::syntax::Span, fr: &Frame) {
        let (ExprKind::This, Some(field)) = (&Self::root(target).0.kind, Self::root(target).1) else { return };
        let Some((class, _)) = &fr.this else { return };
        let Some((_, purposes)) = self.classes.get(class).and_then(|c| c.pii.get(field)) else { return };
        let literal = matches!(&value.kind, ExprKind::Str(_) | ExprKind::Num(_) | ExprKind::Bool(_) | ExprKind::Null) || matches!(&value.kind, ExprKind::Array(a) if a.is_empty());
        if literal || purposes.is_empty() || !purposes.iter().all(|p| self.consent_purposes.contains(p)) {
            return;
        }
        if !self.needs.last().is_some_and(|n| purposes.iter().any(|p| n.contains(p))) {
            self.report(Diagnostic::new(format!("{class}.{field} is held only for {}, which rests on consent, and this method stores it without checking", purposes.join(", ")), span).hint(format!("add @needs(\"consent:{}\") to this method", purposes[0])));
        }
    }

    /// `target = v`, or `v` put into `target` (`push`, `t[i] = v`).
    fn assign(&mut self, target: &Expr, v: Val, span: crate::syntax::Span, fr: &mut Frame) {
        let (base, field) = Self::root(target);
        match &base.kind {
            ExprKind::This => {
                if let (Some(field), Some((class, _))) = (field, fr.this.clone()) {
                    self.write_field(&class, true, field, &v, span);
                }
            }
            // Another object's field, changed through a handle: `Room.get("lobby").messages.push(x)`.
            ExprKind::Ident(n) if matches!(fr.vars.get(n), Some(Val::Handle { .. })) && field.is_some() => {
                if let (Some(Val::Handle { class, other }), Some(field)) = (fr.vars.get(n).cloned(), field) {
                    self.write_field(&class, !other, field, &v, span);
                }
            }
            ExprKind::Call { .. } => {
                if let (Val::Handle { class, other }, Some(field)) = (self.expr(base, fr), field) {
                    self.write_field(&class, !other, field, &v, span);
                }
            }
            ExprKind::Ident(n) if fr.vars.contains_key(n) => {
                let is_whole = matches!(target.kind, ExprKind::Ident(_));
                if is_whole {
                    let merged = self.merge(fr.vars.get(n), v);
                    fr.vars.insert(n.clone(), merged);
                } else if let Some(Val::Cell(c)) = fr.vars.get(n) {
                    let t = self.flatten(&v);
                    c.borrow_mut().join(&t);
                } else {
                    let merged = self.merge(fr.vars.get(n), Val::Data(self.flatten(&v)));
                    fr.vars.insert(n.clone(), merged);
                }
            }
            ExprKind::Ident(n) if self.globals.contains(n) => {
                self.sink(&v, span, &format!("the module-level variable `{n}`, which every caller shares"));
                let revealed = self.flatten(&v).revealed;
                if !revealed.is_empty() {
                    let merged = self.merge(self.global_vals.get(n), Val::Data(Taint { revealed, ..Taint::default() }));
                    self.global_vals.insert(n.clone(), merged);
                }
            }
            _ => {}
        }
    }

    fn call_expr(&mut self, callee: &Expr, args: &[Expr], span: crate::syntax::Span, fr: &mut Frame) -> Val {
        let arg_refs: Vec<&Expr> = args.iter().collect();
        match &callee.kind {
            ExprKind::Ident(name) if !fr.vars.contains_key(name) => match name.as_str() {
                "reveal" => {
                    let v = args.first().map_or_else(clean, |a| self.expr(a, fr));
                    self.reveal(&v, args.get(1), span);
                    let t = self.flatten(&v);
                    Val::Data(Taint { revealed: t.fields.union(&t.revealed).cloned().collect(), ..Taint::default() })
                }
                "fetch" | "plugin" | "invoke" => {
                    let to = match name.as_str() {
                        "fetch" => Some(args.first().map_or(Destination::Unknown, destination)),
                        _ => None,
                    };
                    for a in args {
                        let v = self.expr(a, fr);
                        self.sink(&v, a.span, &format!("`{name}`"));
                        if let Some(to) = &to {
                            self.transfer(&v, a.span, to);
                        }
                    }
                    clean()
                }
                _ if self.plugin_fns.contains_key(name) => {
                    let module = self.plugin_fns[name].clone();
                    for a in args {
                        let v = self.expr(a, fr);
                        self.sink(&v, a.span, &format!("the plugin {module}"));
                        // tj:privacy is the server itself; every other plugin is a recipient.
                        if &*module != "tj:privacy" {
                            self.transfer(&v, a.span, &Destination::Plugin(module.to_string()));
                        }
                    }
                    if module == "tj:privacy" && name == "exportData" {
                        // Everything held about a person: their own when asked for by them.
                        let own = fr.owner_only && args.first().is_some_and(|a| is_member(a, "caller", "id") || is_member(a, "this", "id"));
                        let fields: BTreeSet<Field> = self.classes.iter().flat_map(|(c, info)| info.pii.keys().map(move |f| (c.clone(), f.clone()))).collect();
                        return Val::Data(Taint { fields, other: !own, from: Some(Rc::from("exportData(...)")), ..Taint::default() });
                    }
                    clean()
                }
                _ if self.functions.contains_key(name) => {
                    let f = self.functions[name].clone();
                    let vals: Vec<Val> = args.iter().map(|a| self.expr(a, fr)).collect();
                    Val::Data(self.call(&f, vals, None, Endpoint::Internal, None))
                }
                // Error, String, Number, parseInt, parseFloat and anything else: what goes in
                // comes out.
                _ => Val::Data(self.union(&arg_refs, fr)),
            },
            ExprKind::Member { obj, name, .. } => self.method_call(obj, name, args, span, fr),
            _ => {
                let f = self.expr(callee, fr);
                let vals: Vec<Val> = args.iter().map(|a| self.expr(a, fr)).collect();
                match f {
                    // A function written inside this one: it sees, and changes, its variables.
                    Val::Func(f) => {
                        let this = fr.this.clone();
                        Val::Data(self.call(&f, vals, this, Endpoint::Internal, Some(&mut fr.vars)))
                    }
                    _ => self.unknown_call(&vals, span),
                }
            }
        }
    }

    /// A call the checker cannot follow: fine unless personal data goes into it.
    fn unknown_call(&mut self, vals: &[Val], span: crate::syntax::Span) -> Val {
        let mut t = Taint::default();
        vals.iter().for_each(|v| t.join(&self.flatten(v)));
        if !t.is_clean() {
            let d = Diagnostic::new(format!("{} goes into a function the privacy check cannot follow", self.describe(&t)), span)
                .hint("call a named function or method directly, or reveal the value with its purpose first");
            self.report(d);
        }
        Val::Data(t)
    }

    fn method_call(&mut self, obj: &Expr, name: &str, args: &[Expr], span: crate::syntax::Span, fr: &mut Frame) -> Val {
        let arg_refs: Vec<&Expr> = args.iter().collect();
        if let ExprKind::Ident(ns) = &obj.kind {
            if !fr.vars.contains_key(ns) {
                match ns.as_str() {
                    "console" => {
                        for a in args {
                            let v = self.expr(a, fr);
                            self.sink(&v, a.span, "the log");
                        }
                        return clean();
                    }
                    // A keyed hash and random ids are how apps avoid storing the value itself.
                    "crypto" => {
                        self.union(&arg_refs, fr);
                        return clean();
                    }
                    _ => {}
                }
                if name == "all" && self.classes.contains_key(ns.as_str()) {
                    return Val::Handles { class: Rc::from(ns.as_str()) };
                }
                if name == "get" && self.classes.contains_key(ns.as_str()) {
                    let own = args.first().is_some_and(|a| is_member(a, "this", "id") || (fr.owner_only && is_member(a, "caller", "id")));
                    self.union(&arg_refs, fr);
                    return Val::Handle { class: Rc::from(ns.as_str()), other: !own };
                }
            }
        }
        if matches!(obj.kind, ExprKind::This) {
            match name {
                "emit" => {
                    for a in args.iter().skip(1) {
                        let v = self.expr(a, fr);
                        self.sink(&v, a.span, "`emit`, which every subscriber receives");
                    }
                    return clean();
                }
                "schedule" => {
                    let mut vals = Vec::new();
                    for a in args.iter().skip(2) {
                        let v = self.expr(a, fr);
                        self.sink(&v, a.span, "an alarm's arguments, which are stored with the alarm");
                        vals.push(v);
                    }
                    // Follow the alarm into its method, so where it sends revealed data is seen.
                    if vals.iter().any(|v| !self.flatten(v).revealed.is_empty()) {
                        let class = fr.this.as_ref().map(|(c, _)| c.clone());
                        let target = match args.get(1).map(|a| &a.kind) {
                            Some(ExprKind::Str(m)) => class.as_ref().and_then(|c| self.classes.get(c)).and_then(|c| c.methods.get(m.as_str())).map(|(d, _)| d.clone()),
                            _ => None,
                        };
                        match target {
                            Some(decl) => {
                                self.call(&decl, vals, fr.this.clone(), Endpoint::Internal, None);
                            }
                            None => self.report(Diagnostic::new("the privacy check cannot follow this alarm to its method", span).hint("name the method as a string, e.g. this.schedule(60000, \"remind\", ...)")),
                        }
                    }
                    return clean();
                }
                _ => {}
            }
        }
        let recv = self.expr(obj, fr);
        if let Val::Handle { class, other } = &recv {
            let method = self.classes.get(class).and_then(|c| c.methods.get(name)).map(|(d, _)| d.clone());
            if let Some(m) = method {
                let vals: Vec<Val> = args.iter().map(|a| self.expr(a, fr)).collect();
                return Val::Data(self.call(&m, vals, Some((class.clone(), *other)), Endpoint::Internal, None));
            }
        }
        // A list of objects keeps being one through filter, sort and slice; its elements are
        // handles in callbacks and through find.
        if let Val::Handles { class } = &recv {
            if CALLBACK_METHODS.contains(&name) || name == "slice" {
                let elem = Val::Handle { class: class.clone(), other: true };
                if let (Some(ExprKind::Arrow(f)), true) = (args.first().map(|a| &a.kind), CALLBACK_METHODS.contains(&name)) {
                    let this = fr.this.clone();
                    // reduce's callback takes the accumulator first, then the element; what
                    // it returns becomes the accumulator, so both run through it.
                    let cb_args = if name == "reduce" {
                        let init = args.get(1).map(|a| self.expr(a, fr)).unwrap_or_else(clean);
                        let mut acc = self.flatten(&init);
                        acc.join(&self.whole(class, true));
                        vec![Val::Data(acc), elem.clone(), clean(), recv.clone()]
                    } else {
                        vec![elem.clone(), clean(), recv.clone()]
                    };
                    let ret = self.call(f, cb_args, this, Endpoint::Internal, Some(&mut fr.vars));
                    return match name {
                        "map" | "flatMap" | "reduce" => Val::Data(ret),
                        "some" | "every" | "findIndex" | "forEach" => clean(),
                        "find" => elem,
                        _ => recv.clone(),
                    };
                }
                return recv.clone();
            }
            if name == "length" {
                return clean();
            }
        }
        let rt = self.flatten(&recv);
        if MUTATORS.contains(&name) {
            let t = self.union(&arg_refs, fr);
            self.assign_into(obj, Val::Data(t), span, fr);
            return clean();
        }
        if CALLBACK_METHODS.contains(&name) {
            if let Some(cb) = args.first() {
                let init = args.get(1).map(|a| self.expr(a, fr)).map(|v| self.flatten(&v)).unwrap_or_default();
                let f = match &cb.kind {
                    ExprKind::Arrow(f) => Some(f.clone()),
                    _ => match self.expr(cb, fr) {
                        Val::Func(f) => Some(f),
                        _ => None,
                    },
                };
                let Some(f) = f else {
                    return self.unknown_call(&[Val::Data(rt)], span);
                };
                let elem = Val::Data(rt.clone());
                let cb_args = if name == "reduce" {
                    let mut acc = init.clone();
                    acc.join(&rt);
                    vec![Val::Data(acc), elem]
                } else {
                    vec![elem.clone(), clean(), elem]
                };
                let this = fr.this.clone();
                let ret = self.call(&f, cb_args, this, Endpoint::Internal, Some(&mut fr.vars));
                return Val::Data(match name {
                    "map" | "flatMap" | "some" | "every" | "findIndex" => ret,
                    "reduce" => {
                        let mut t = ret;
                        t.join(&init);
                        t
                    }
                    "forEach" => Taint::default(),
                    _ => rt,
                });
            }
        }
        if let ExprKind::Ident(ns) = &obj.kind {
            if ns == "JSON" || ns == "Math" || ns == "Object" || ns == "Array" || ns == "Date" {
                return Val::Data(self.union(&arg_refs, fr));
            }
        }
        // String and array methods: what goes in comes out.
        let mut t = rt;
        t.join(&self.union(&arg_refs, fr));
        Val::Data(t)
    }

    /// Something is put into `target` (a mutating method).
    fn assign_into(&mut self, target: &Expr, v: Val, span: crate::syntax::Span, fr: &mut Frame) {
        // `this.list.push(x)` writes into `this.list`; `box.push(x)` into `box`.
        let synthetic = Expr { kind: ExprKind::Index { obj: Box::new(target.clone()), index: Box::new(Expr { kind: ExprKind::Null, span }), optional: false }, span };
        self.assign(&synthetic, v, span, fr);
    }

    fn reveal(&mut self, v: &Val, purpose: Option<&Expr>, span: crate::syntax::Span) {
        let t = self.flatten(v);
        let Some(ExprKind::Str(p)) = purpose.map(|p| &p.kind) else {
            self.report(Diagnostic::new("reveal needs the purpose as a string", span).hint("write reveal(value, \"account\"), naming one of the field's purposes"));
            return;
        };
        // Data used for a purpose that rests on consent: only where the consent is checked.
        if self.consent_purposes.contains(p) && !t.is_clean() && !self.needs.last().is_some_and(|n| n.contains(p)) {
            self.report(Diagnostic::new(format!("\"{p}\" rests on consent (lawfulBasis), and this method does not check it"), span).hint(format!("add @needs(\"consent:{p}\") to the method that reveals it; it refuses to run unless the person has consented")));
        }
        let wrong: Vec<String> = t
            .fields
            .iter()
            .filter_map(|(c, f)| {
                let (_, purposes) = self.classes.get(c)?.pii.get(f)?;
                (!purposes.contains(p)).then(|| format!("{c}.{f} is for {}", purposes.join(", ")))
            })
            .collect();
        if !wrong.is_empty() {
            let hint = if t.fields.len() > 1 && self.purposes(&t).is_empty() { "these fields are for different purposes: reveal each for its own purpose before combining them".to_string() } else { "use one of the purposes the field was declared for, or add this purpose to its @pii".to_string() };
            self.report(Diagnostic::new(format!("\"{p}\" is not a purpose of this personal data: {}", wrong.join("; ")), span).hint(hint));
        } else {
            let fields = t.fields.iter().map(|(c, f)| format!("{c}.{f}")).collect();
            self.reveals.push(Reveal { at: span.start, purpose: p.clone(), fields });
        }
    }

    /// A tool for `tj:agent` (`{ object, method, params, ... }`): what the method returns goes
    /// to the AI model.
    fn skill(&mut self, entries: &[RecordEntry], span: crate::syntax::Span) {
        let field = |k: &str| entries.iter().find_map(|e| if let RecordEntry::Field(n, v) = e { (n == k).then_some(v) } else { None });
        let (Some(object), Some(method), Some(_)) = (field("object"), field("method"), field("params")) else { return };
        let ExprKind::Str(method) = &method.kind else {
            self.report(Diagnostic::new("the privacy check cannot follow this tool's method", method.span).hint("write the method name as a string literal"));
            return;
        };
        // "Account/main", `Account/${caller.id}` or "Account/" + caller.id.
        let prefix = match &object.kind {
            ExprKind::Str(s) => Some(s),
            ExprKind::Template(pieces) => match pieces.first() {
                Some(TplPiece::Str(s)) => Some(s),
                _ => None,
            },
            ExprKind::Binary(BinOp::Add, left, _) => match &left.kind {
                ExprKind::Str(s) => Some(s),
                _ => None,
            },
            _ => None,
        };
        let class = prefix.and_then(|s| s.split_once('/')).map(|(c, _)| Rc::<str>::from(c));
        let decl = class.as_ref().and_then(|c| self.classes.get(c)).and_then(|c| c.methods.get(method.as_str())).map(|(d, _)| d.clone());
        let (Some(class), Some(decl)) = (class, decl) else {
            self.report(Diagnostic::new("the privacy check cannot follow this tool's object", object.span).hint("write it as \"Class/id\" or `Class/${...}`, naming a class and one of its methods"));
            return;
        };
        let args = vec![clean(); decl.params.len()];
        let ret = self.call(&decl, args, Some((class.clone(), false)), Endpoint::Internal, None);
        // What the tool returns goes to the AI model.
        self.transfer(&Val::Data(ret.clone()), span, &Destination::Plugin("tj:ai".into()));
        if !ret.is_clean() {
            let d = Diagnostic::new(format!("the tool {class}.{method} returns {}, which the agent sends to the AI model", self.describe(&ret)), span).hint(format!("reveal it in {method} with the purpose the model needs it for, or return something else"));
            self.report(d);
        }
    }
}

fn is_member(e: &Expr, obj: &str, name: &str) -> bool {
    let ExprKind::Member { obj: o, name: n, .. } = &e.kind else { return false };
    n == name
        && match &o.kind {
            ExprKind::This => obj == "this",
            ExprKind::Ident(i) => i == obj,
            _ => false,
        }
}

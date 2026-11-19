//! Recursive-descent parser for the TypeScript-shaped tinyjump language.

use super::ast::*;
use super::lexer::{keyword_text, Lexer, Tok, Token, TplPart};
use super::{Diagnostic, Span};
use std::rc::Rc;

type PResult<T> = Result<T, Diagnostic>;

#[derive(Clone)]
pub struct Parser<'a> {
    src: &'a str,
    lex: Lexer<'a>,
    cur: Token,
    prev_end: u32,
}

/// Parses a whole source file.
pub fn parse(src: &str) -> PResult<Program> {
    let mut p = Parser::new(src, 0)?;
    let mut items = Vec::new();
    while p.cur.tok != Tok::Eof {
        items.push(p.item()?);
    }
    Ok(Program { items })
}

fn tok_text(t: &Tok) -> String {
    let s = match t {
        Tok::Ident(n) => return format!("'{n}'"),
        Tok::Num(n) => return format!("number {n}"),
        Tok::Str(_) => "a string",
        Tok::Template(_) => "a template string",
        Tok::Eof => "end of file",
        Tok::LParen => "'('",
        Tok::RParen => "')'",
        Tok::LBrace => "'{'",
        Tok::RBrace => "'}'",
        Tok::LBracket => "'['",
        Tok::RBracket => "']'",
        Tok::Comma => "','",
        Tok::Semi => "';'",
        Tok::Colon => "':'",
        Tok::Dot => "'.'",
        Tok::Arrow => "'=>'",
        Tok::Assign => "'='",
        Tok::Lt => "'<'",
        Tok::Gt => "'>'",
        Tok::Slash => "'/'",
        other => return keyword_text(other).map_or_else(|| format!("{other:?}"), |k| format!("'{k}'")),
    };
    s.to_string()
}

fn not_supported(word: &str, span: Span) -> Diagnostic {
    let (msg, hint): (String, &str) = match word {
        "var" => ("`var` is not supported".into(), "use `let` or `const`"),
        "class" => ("`class` is not supported".into(), "use `object Name { ... }` for durable state, or plain functions and records"),
        "undefined" => ("`undefined` does not exist".into(), "use `null`"),
        "switch" => ("`switch` is not supported yet".into(), "use if / else if"),
        "import" => ("imports are not supported yet".into(), "keep the app in one file for now"),
        "finally" => ("`finally` is not supported yet".into(), "repeat the cleanup at the end of both the try and the catch block"),
        "in" => ("`in` is not supported".into(), "use `obj[key] !== null`, or `for (const key of Object.keys(obj))`"),
        "delete" => ("`delete` is not supported".into(), "build a new record without the key, or set it to null"),
        _ => (format!("`{word}` is not supported"), "see docs/language.md for the supported subset"),
    };
    Diagnostic::new(msg, span).hint(hint)
}

impl<'a> Parser<'a> {
    fn new(src: &'a str, pos: usize) -> PResult<Self> {
        let mut lex = Lexer::at(src, pos);
        let cur = lex.next_token()?;
        Ok(Parser { src, lex, cur, prev_end: pos as u32 })
    }

    fn bump(&mut self) -> PResult<Token> {
        let next = self.lex.next_token()?;
        self.prev_end = self.cur.span.end;
        Ok(std::mem::replace(&mut self.cur, next))
    }

    fn at(&self, t: &Tok) -> bool {
        &self.cur.tok == t
    }

    fn eat(&mut self, t: &Tok) -> PResult<bool> {
        if self.at(t) {
            self.bump()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn expect(&mut self, t: &Tok, what: &str) -> PResult<Token> {
        if self.at(t) {
            self.bump()
        } else {
            Err(self.unexpected(what))
        }
    }

    fn since(&self, start: Span) -> Span {
        Span { start: start.start, end: self.prev_end.max(start.end) }
    }

    fn unexpected(&self, expected: &str) -> Diagnostic {
        let span = self.cur.span;
        match &self.cur.tok {
            Tok::Reserved(w) => not_supported(w, span),
            Tok::EqEq => Diagnostic::new("`==` is not supported", span).hint("use `===`"),
            Tok::NotEq => Diagnostic::new("`!=` is not supported", span).hint("use `!==`"),
            t => Diagnostic::new(format!("expected {expected}, found {}", tok_text(t)), span),
        }
    }

    fn ident(&mut self, what: &str) -> PResult<(String, Span)> {
        if let Tok::Ident(s) = &self.cur.tok {
            let s = s.clone();
            let t = self.bump()?;
            Ok((s, t.span))
        } else {
            Err(self.unexpected(what))
        }
    }

    /// A property name: identifier, keyword or string literal.
    fn prop_name(&mut self) -> PResult<(String, Span)> {
        let name = match &self.cur.tok {
            Tok::Ident(s) | Tok::Str(s) => s.clone(),
            t => match keyword_text(t) {
                Some(k) => k.to_string(),
                None => return Err(self.unexpected("a name")),
            },
        };
        let t = self.bump()?;
        Ok((name, t.span))
    }

    fn end_stmt(&mut self) -> PResult<()> {
        if self.eat(&Tok::Semi)? || self.cur.nl_before || self.at(&Tok::RBrace) || self.at(&Tok::Eof) {
            Ok(())
        } else {
            Err(self.unexpected("';' or a new line"))
        }
    }

    /// `<T, U extends X = Y>`: returns the parameter names; constraints and defaults are ignored.
    fn generics(&mut self) -> PResult<Vec<String>> {
        let mut names = Vec::new();
        if !self.at(&Tok::Lt) {
            return Ok(names);
        }
        let mut depth = 0;
        let mut expect_name = false;
        loop {
            let t = self.bump()?.tok;
            match &t {
                Tok::Lt => depth += 1,
                Tok::Gt => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(names);
                    }
                }
                Tok::Ident(n) if expect_name && depth == 1 => names.push(n.clone()),
                Tok::Eof => return Err(self.unexpected("'>'")),
                _ => {}
            }
            expect_name = depth == 1 && matches!(t, Tok::Lt | Tok::Comma);
        }
    }

    // ---------- items ----------

    fn item(&mut self) -> PResult<Item> {
        if self.eat(&Tok::Export)? {
            self.eat(&Tok::Default)?;
        }
        let start = self.cur.span;
        match self.cur.tok {
            Tok::Function | Tok::Async => Ok(Item::Function(Rc::new(self.fn_decl()?))),
            Tok::Object => self.object_decl().map(Item::Object),
            Tok::Type => {
                self.bump()?;
                let (name, _) = self.ident("a type name")?;
                let generics = self.generics()?;
                self.expect(&Tok::Assign, "'='")?;
                let ty = self.ty()?;
                self.end_stmt()?;
                Ok(Item::TypeAlias { name, generics, ty, span: self.since(start) })
            }
            Tok::Interface => {
                self.bump()?;
                let (name, _) = self.ident("an interface name")?;
                let generics = self.generics()?;
                let ty = self.record_type()?;
                Ok(Item::TypeAlias { name, generics, ty, span: self.since(start) })
            }
            Tok::Let | Tok::Const => {
                let d = self.let_decl()?;
                self.end_stmt()?;
                Ok(Item::Let(d))
            }
            Tok::Reserved(_) => Err(self.unexpected("a declaration")),
            _ => Err(self
                .unexpected("a declaration (function, object, type, let or const)")
                .hint("statements belong inside functions or object methods")),
        }
    }

    fn fn_decl(&mut self) -> PResult<FnDecl> {
        let start = self.cur.span;
        self.eat(&Tok::Async)?;
        self.expect(&Tok::Function, "'function'")?;
        let (name, _) = self.ident("a function name")?;
        let generics = self.generics()?;
        self.fn_rest(name, generics, start)
    }

    fn fn_rest(&mut self, name: String, generics: Vec<String>, start: Span) -> PResult<FnDecl> {
        let params = self.params()?;
        let ret = if self.eat(&Tok::Colon)? { Some(self.ty()?) } else { None };
        let body = self.block()?;
        Ok(FnDecl { name, generics, params, ret, body, span: self.since(start) })
    }

    fn params(&mut self) -> PResult<Vec<Param>> {
        self.expect(&Tok::LParen, "'('")?;
        let mut out = Vec::new();
        while !self.at(&Tok::RParen) {
            if self.at(&Tok::Ellipsis) {
                return Err(Diagnostic::new("rest parameters are not supported yet", self.cur.span).hint("take an array parameter instead"));
            }
            let pattern = self.pattern()?;
            let optional = self.eat(&Tok::Question)?;
            let ty = if self.eat(&Tok::Colon)? { Some(self.ty()?) } else { None };
            let default = if self.eat(&Tok::Assign)? { Some(self.assign_expr()?) } else { None };
            out.push(Param { pattern, ty, default, optional });
            if !self.eat(&Tok::Comma)? {
                break;
            }
        }
        self.expect(&Tok::RParen, "')'")?;
        Ok(out)
    }

    fn pattern(&mut self) -> PResult<Pattern> {
        let start = self.cur.span;
        match self.cur.tok {
            Tok::LBrace => {
                self.bump()?;
                let mut entries = Vec::new();
                while !self.at(&Tok::RBrace) {
                    let (key, kspan) = self.prop_name()?;
                    let pat = if self.eat(&Tok::Colon)? { self.pattern()? } else { Pattern::Ident(key.clone(), kspan) };
                    let default = if self.eat(&Tok::Assign)? { Some(self.assign_expr()?) } else { None };
                    entries.push((key, pat, default));
                    if !self.eat(&Tok::Comma)? {
                        break;
                    }
                }
                self.expect(&Tok::RBrace, "'}'")?;
                Ok(Pattern::Record(entries, self.since(start)))
            }
            Tok::LBracket => {
                self.bump()?;
                let mut items = Vec::new();
                while !self.at(&Tok::RBracket) {
                    items.push(self.pattern()?);
                    if !self.eat(&Tok::Comma)? {
                        break;
                    }
                }
                self.expect(&Tok::RBracket, "']'")?;
                Ok(Pattern::Array(items, self.since(start)))
            }
            _ => {
                let (n, s) = self.ident("a name")?;
                Ok(Pattern::Ident(n, s))
            }
        }
    }

    fn object_decl(&mut self) -> PResult<ObjectDecl> {
        let start = self.bump()?.span;
        let (name, _) = self.ident("an object name")?;
        self.expect(&Tok::LBrace, "'{'")?;
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        while !self.at(&Tok::RBrace) {
            let mut decorators = Vec::new();
            while self.at(&Tok::At) {
                let s = self.bump()?.span;
                let (dname, _) = self.ident("a decorator name")?;
                let args = if self.at(&Tok::LParen) { self.args()? } else { Vec::new() };
                decorators.push(Decorator { name: dname, args, span: self.since(s) });
            }
            let private_span = self.cur.span;
            let private = self.eat(&Tok::Private)?;
            self.eat(&Tok::Async)?;
            let (mname, mspan) = self.prop_name()?;
            if self.at(&Tok::LParen) || self.at(&Tok::Lt) {
                let generics = self.generics()?;
                let func = Rc::new(self.fn_rest(mname, generics, mspan)?);
                methods.push(Method { decorators, private, func });
                continue;
            }
            if let Some(d) = decorators.first() {
                return Err(Diagnostic::new("decorators are only allowed on methods", d.span));
            }
            if private {
                return Err(Diagnostic::new("fields cannot be private", private_span)
                    .hint("every field is readable; only the object's own methods can change it"));
            }
            let ty = if self.eat(&Tok::Colon)? { Some(self.ty()?) } else { None };
            let init = if self.eat(&Tok::Assign)? { Some(self.assign_expr()?) } else { None };
            if !(self.eat(&Tok::Semi)? || self.eat(&Tok::Comma)? || self.cur.nl_before || self.at(&Tok::RBrace)) {
                return Err(self.unexpected("';' or a new line"));
            }
            fields.push(Field { name: mname, ty, init, span: mspan });
        }
        self.bump()?;
        Ok(ObjectDecl { name, fields, methods, span: self.since(start) })
    }

    // ---------- types (parsed and kept, not yet checked) ----------

    fn ty(&mut self) -> PResult<TypeExpr> {
        self.eat(&Tok::Pipe)?;
        let mut parts = vec![self.ty_postfix()?];
        while self.eat(&Tok::Pipe)? {
            parts.push(self.ty_postfix()?);
        }
        Ok(if parts.len() == 1 { parts.remove(0) } else { TypeExpr::Union(parts) })
    }

    fn ty_postfix(&mut self) -> PResult<TypeExpr> {
        let mut t = self.ty_primary()?;
        while self.at(&Tok::LBracket) && !self.cur.nl_before {
            self.bump()?;
            self.expect(&Tok::RBracket, "']'")?;
            t = TypeExpr::Array(Box::new(t));
        }
        Ok(t)
    }

    fn ty_primary(&mut self) -> PResult<TypeExpr> {
        match &self.cur.tok {
            Tok::Ident(_) => {
                let (mut name, _) = self.ident("a type")?;
                while self.eat(&Tok::Dot)? {
                    name.push('.');
                    name.push_str(&self.ident("a type")?.0);
                }
                let mut args = Vec::new();
                if self.eat(&Tok::Lt)? {
                    loop {
                        args.push(self.ty()?);
                        if !self.eat(&Tok::Comma)? {
                            break;
                        }
                    }
                    self.expect(&Tok::Gt, "'>'")?;
                }
                Ok(TypeExpr::Named(name, args))
            }
            Tok::Null | Tok::True | Tok::False | Tok::This => {
                let name = keyword_text(&self.cur.tok).unwrap_or("null").to_string();
                self.bump()?;
                Ok(TypeExpr::Named(name, Vec::new()))
            }
            Tok::Str(s) => {
                let s = s.clone();
                self.bump()?;
                Ok(TypeExpr::StrLit(s))
            }
            Tok::Num(n) => {
                let n = *n;
                self.bump()?;
                Ok(TypeExpr::NumLit(n))
            }
            Tok::LBrace => self.record_type(),
            Tok::LParen => {
                if self.scan_parens() == Some(Tok::Arrow) {
                    let params = self.params()?;
                    self.expect(&Tok::Arrow, "'=>'")?;
                    let ret = self.ty()?;
                    let tys = params.into_iter().map(|p| p.ty.unwrap_or(TypeExpr::Named("any".into(), Vec::new()))).collect();
                    Ok(TypeExpr::Func(tys, Box::new(ret)))
                } else {
                    self.bump()?;
                    let t = self.ty()?;
                    self.expect(&Tok::RParen, "')'")?;
                    Ok(t)
                }
            }
            _ => Err(self.unexpected("a type")),
        }
    }

    fn record_type(&mut self) -> PResult<TypeExpr> {
        self.expect(&Tok::LBrace, "'{'")?;
        let mut fields = Vec::new();
        let mut index = None;
        while !self.at(&Tok::RBrace) {
            // Index signature: `[key: string]: T`
            if self.eat(&Tok::LBracket)? {
                self.ident("a key name")?;
                self.expect(&Tok::Colon, "':'")?;
                let key = self.ty()?;
                self.expect(&Tok::RBracket, "']'")?;
                self.expect(&Tok::Colon, "':'")?;
                index = Some((key, self.ty()?));
                if !(self.eat(&Tok::Semi)? || self.eat(&Tok::Comma)? || self.cur.nl_before || self.at(&Tok::RBrace)) {
                    return Err(self.unexpected("';' or a new line"));
                }
                continue;
            }
            let (name, _) = self.prop_name()?;
            let optional = self.eat(&Tok::Question)?;
            let ty = if self.at(&Tok::LParen) {
                let params = self.params()?;
                self.expect(&Tok::Colon, "':'")?;
                let ret = self.ty()?;
                TypeExpr::Func(params.into_iter().filter_map(|p| p.ty).collect(), Box::new(ret))
            } else {
                self.expect(&Tok::Colon, "':'")?;
                self.ty()?
            };
            fields.push((name, optional, ty));
            if !(self.eat(&Tok::Semi)? || self.eat(&Tok::Comma)? || self.cur.nl_before || self.at(&Tok::RBrace)) {
                return Err(self.unexpected("';' or a new line"));
            }
        }
        self.bump()?;
        Ok(match index {
            // A pure index signature is a dictionary; named fields win otherwise.
            Some((key, value)) if fields.is_empty() => TypeExpr::Named("Record".into(), vec![key, value]),
            _ => TypeExpr::Record(fields),
        })
    }

    // ---------- statements ----------

    fn block(&mut self) -> PResult<Vec<Stmt>> {
        self.expect(&Tok::LBrace, "'{'")?;
        let mut out = Vec::new();
        while !self.at(&Tok::RBrace) {
            if self.at(&Tok::Eof) {
                return Err(self.unexpected("'}'"));
            }
            out.push(self.stmt()?);
        }
        self.bump()?;
        Ok(out)
    }

    fn body(&mut self) -> PResult<Vec<Stmt>> {
        if self.at(&Tok::LBrace) {
            self.block()
        } else {
            Ok(vec![self.stmt()?])
        }
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        let start = self.cur.span;
        match self.cur.tok {
            Tok::Let | Tok::Const => {
                let d = self.let_decl()?;
                self.end_stmt()?;
                Ok(Stmt::Let(d))
            }
            Tok::Function | Tok::Async => Ok(Stmt::Function(Rc::new(self.fn_decl()?))),
            Tok::Return => {
                self.bump()?;
                let value = if self.cur.nl_before || matches!(self.cur.tok, Tok::Semi | Tok::RBrace | Tok::Eof) {
                    None
                } else {
                    Some(self.expr()?)
                };
                self.end_stmt()?;
                Ok(Stmt::Return(value, self.since(start)))
            }
            Tok::If => {
                self.bump()?;
                self.expect(&Tok::LParen, "'('")?;
                let cond = self.expr()?;
                self.expect(&Tok::RParen, "')'")?;
                let then = self.body()?;
                let els = if self.eat(&Tok::Else)? {
                    Some(if self.at(&Tok::If) { vec![self.stmt()?] } else { self.body()? })
                } else {
                    None
                };
                Ok(Stmt::If { cond, then, els })
            }
            Tok::While => {
                self.bump()?;
                self.expect(&Tok::LParen, "'('")?;
                let cond = self.expr()?;
                self.expect(&Tok::RParen, "')'")?;
                Ok(Stmt::While { cond, body: self.body()? })
            }
            Tok::For => self.for_stmt(),
            Tok::Break => {
                self.bump()?;
                self.end_stmt()?;
                Ok(Stmt::Break(start))
            }
            Tok::Continue => {
                self.bump()?;
                self.end_stmt()?;
                Ok(Stmt::Continue(start))
            }
            Tok::LBrace => Ok(Stmt::Block(self.block()?)),
            Tok::Semi => {
                self.bump()?;
                Ok(Stmt::Block(Vec::new()))
            }
            Tok::Throw => {
                self.bump()?;
                let e = self.expr()?;
                self.end_stmt()?;
                Ok(Stmt::Throw(e, self.since(start)))
            }
            Tok::Try => {
                self.bump()?;
                let body = self.block()?;
                if self.at(&Tok::Reserved("finally")) {
                    return Err(self.unexpected("'catch'"));
                }
                self.expect(&Tok::Catch, "'catch'")?;
                let param = if self.eat(&Tok::LParen)? {
                    let p = self.pattern()?;
                    if self.eat(&Tok::Colon)? {
                        self.ty()?;
                    }
                    self.expect(&Tok::RParen, "')'")?;
                    Some(p)
                } else {
                    None
                };
                let handler = self.block()?;
                if self.at(&Tok::Reserved("finally")) {
                    return Err(self.unexpected("a statement"));
                }
                Ok(Stmt::Try { body, param, handler })
            }
            _ => {
                let e = self.expr()?;
                self.end_stmt()?;
                Ok(Stmt::Expr(e))
            }
        }
    }

    fn let_decl(&mut self) -> PResult<LetDecl> {
        let start = self.cur.span;
        let mutable = self.bump()?.tok == Tok::Let;
        let pattern = self.pattern()?;
        let ty = if self.eat(&Tok::Colon)? { Some(self.ty()?) } else { None };
        let init = if self.eat(&Tok::Assign)? { Some(self.expr()?) } else { None };
        if self.at(&Tok::Comma) {
            return Err(Diagnostic::new("declare one variable per statement", self.cur.span));
        }
        if !mutable && init.is_none() {
            return Err(Diagnostic::new("`const` needs a value", self.since(start)).hint("use `let` if you assign it later"));
        }
        Ok(LetDecl { pattern, mutable, ty, init, span: self.since(start) })
    }

    fn for_stmt(&mut self) -> PResult<Stmt> {
        self.bump()?;
        self.expect(&Tok::LParen, "'('")?;
        if matches!(self.cur.tok, Tok::Let | Tok::Const) {
            let mut probe = self.clone();
            probe.bump()?;
            probe.pattern()?;
            if probe.at(&Tok::Of) {
                self.bump()?;
                let pattern = self.pattern()?;
                self.expect(&Tok::Of, "'of'")?;
                let iter = self.expr()?;
                self.expect(&Tok::RParen, "')'")?;
                return Ok(Stmt::ForOf { pattern, iter, body: self.body()? });
            }
            if probe.at(&Tok::Reserved("in")) {
                return Err(Diagnostic::new("`for...in` is not supported", probe.cur.span).hint("use `for (const key of Object.keys(obj))`"));
            }
        }
        let init = if self.at(&Tok::Semi) {
            None
        } else if matches!(self.cur.tok, Tok::Let | Tok::Const) {
            Some(Box::new(Stmt::Let(self.let_decl()?)))
        } else {
            Some(Box::new(Stmt::Expr(self.expr()?)))
        };
        self.expect(&Tok::Semi, "';'")?;
        let cond = if self.at(&Tok::Semi) { None } else { Some(self.expr()?) };
        self.expect(&Tok::Semi, "';'")?;
        let update = if self.at(&Tok::RParen) { None } else { Some(self.expr()?) };
        self.expect(&Tok::RParen, "')'")?;
        Ok(Stmt::For { init, cond, update, body: self.body()? })
    }

    // ---------- expressions ----------

    pub fn expr(&mut self) -> PResult<Expr> {
        self.assign_expr()
    }

    fn assign_expr(&mut self) -> PResult<Expr> {
        if let Some(arrow) = self.try_arrow()? {
            return Ok(arrow);
        }
        let lhs = self.cond_expr()?;
        let op = match self.cur.tok {
            Tok::Assign => None,
            Tok::PlusAssign => Some(BinOp::Add),
            Tok::MinusAssign => Some(BinOp::Sub),
            Tok::StarAssign => Some(BinOp::Mul),
            Tok::SlashAssign => Some(BinOp::Div),
            _ => return Ok(lhs),
        };
        let assignable = matches!(lhs.kind, ExprKind::Ident(_) | ExprKind::Member { optional: false, .. } | ExprKind::Index { optional: false, .. });
        if !assignable {
            return Err(Diagnostic::new("cannot assign to this expression", lhs.span));
        }
        self.bump()?;
        let value = self.assign_expr()?;
        Ok(Expr { span: lhs.span.to(value.span), kind: ExprKind::Assign { op, target: Box::new(lhs), value: Box::new(value) } })
    }

    /// Token after the parenthesised group starting at the current `(`.
    fn scan_parens(&self) -> Option<Tok> {
        let mut l = self.lex.clone();
        let mut depth = 1;
        loop {
            match l.next_token().ok()?.tok {
                Tok::LParen | Tok::LBracket | Tok::LBrace => depth += 1,
                Tok::RParen | Tok::RBracket | Tok::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        return l.next_token().ok().map(|t| t.tok);
                    }
                }
                Tok::Eof => return None,
                _ => {}
            }
        }
    }

    fn try_arrow(&mut self) -> PResult<Option<Expr>> {
        let start = self.cur.span;
        if self.at(&Tok::Async) {
            let mut p = self.clone();
            p.bump()?;
            if let Some(e) = p.try_arrow_at(start)? {
                *self = p;
                return Ok(Some(e));
            }
            return Ok(None);
        }
        self.try_arrow_at(start)
    }

    fn try_arrow_at(&mut self, start: Span) -> PResult<Option<Expr>> {
        match &self.cur.tok {
            Tok::Ident(name) => {
                let next = self.lex.clone().next_token()?;
                if next.tok != Tok::Arrow || next.nl_before {
                    return Ok(None);
                }
                let param = Param { pattern: Pattern::Ident(name.clone(), self.cur.span), ty: None, default: None, optional: false };
                self.bump()?;
                self.bump()?;
                let body = self.arrow_body()?;
                Ok(Some(self.arrow(vec![param], body, start)))
            }
            Tok::LParen => match self.scan_parens() {
                Some(Tok::Arrow) => {
                    let params = self.params()?;
                    self.expect(&Tok::Arrow, "'=>'")?;
                    let body = self.arrow_body()?;
                    Ok(Some(self.arrow(params, body, start)))
                }
                // `(x): T => ...` — try it, and fall back to a parenthesised expression.
                Some(Tok::Colon) => {
                    let mut q = self.clone();
                    let Ok(params) = q.params() else { return Ok(None) };
                    if !(q.eat(&Tok::Colon)? && q.ty().is_ok() && q.at(&Tok::Arrow)) {
                        return Ok(None);
                    }
                    q.bump()?;
                    let body = q.arrow_body()?;
                    *self = q;
                    Ok(Some(self.arrow(params, body, start)))
                }
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    fn arrow_body(&mut self) -> PResult<Vec<Stmt>> {
        if self.at(&Tok::LBrace) {
            return self.block();
        }
        let e = self.assign_expr()?;
        let span = e.span;
        Ok(vec![Stmt::Return(Some(e), span)])
    }

    fn arrow(&self, params: Vec<Param>, body: Vec<Stmt>, start: Span) -> Expr {
        let span = self.since(start);
        Expr { kind: ExprKind::Arrow(Rc::new(FnDecl { name: "<anonymous>".into(), generics: Vec::new(), params, ret: None, body, span })), span }
    }

    fn cond_expr(&mut self) -> PResult<Expr> {
        let c = self.binary(1)?;
        if !self.at(&Tok::Question) {
            return Ok(c);
        }
        self.bump()?;
        let a = self.assign_expr()?;
        self.expect(&Tok::Colon, "':'")?;
        let b = self.assign_expr()?;
        Ok(Expr { span: c.span.to(b.span), kind: ExprKind::Cond(Box::new(c), Box::new(a), Box::new(b)) })
    }

    fn binary(&mut self, min: u8) -> PResult<Expr> {
        enum K {
            Bin(BinOp),
            Logic(LogicOp),
            As,
        }
        let mut lhs = self.unary()?;
        loop {
            let (prec, k) = match self.cur.tok {
                Tok::QQ => (1, K::Logic(LogicOp::Nullish)),
                Tok::OrOr => (2, K::Logic(LogicOp::Or)),
                Tok::AndAnd => (3, K::Logic(LogicOp::And)),
                Tok::EqEqEq => (4, K::Bin(BinOp::Eq)),
                Tok::NotEqEq => (4, K::Bin(BinOp::Ne)),
                Tok::EqEq | Tok::NotEq => return Err(self.unexpected("an operator")),
                Tok::Lt => (5, K::Bin(BinOp::Lt)),
                Tok::Gt => (5, K::Bin(BinOp::Gt)),
                Tok::Le => (5, K::Bin(BinOp::Le)),
                Tok::Ge => (5, K::Bin(BinOp::Ge)),
                Tok::As => (5, K::As),
                Tok::Plus => (6, K::Bin(BinOp::Add)),
                Tok::Minus => (6, K::Bin(BinOp::Sub)),
                Tok::Star => (7, K::Bin(BinOp::Mul)),
                Tok::Slash => (7, K::Bin(BinOp::Div)),
                Tok::Percent => (7, K::Bin(BinOp::Mod)),
                _ => break,
            };
            if prec < min {
                break;
            }
            self.bump()?;
            if let K::As = k {
                self.ty()?;
                continue;
            }
            let rhs = self.binary(prec + 1)?;
            let span = lhs.span.to(rhs.span);
            let kind = match k {
                K::Bin(op) => ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                K::Logic(op) => ExprKind::Logical(op, Box::new(lhs), Box::new(rhs)),
                K::As => unreachable!(),
            };
            lhs = Expr { kind, span };
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> PResult<Expr> {
        let start = self.cur.span;
        let wrap = |kind: ExprKind, p: &Self| Expr { kind, span: p.since(start) };
        match self.cur.tok {
            Tok::Bang => {
                self.bump()?;
                let e = self.unary()?;
                Ok(wrap(ExprKind::Unary(UnOp::Not, Box::new(e)), self))
            }
            Tok::Minus => {
                self.bump()?;
                let e = self.unary()?;
                if let ExprKind::Num(n) = e.kind {
                    return Ok(wrap(ExprKind::Num(-n), self));
                }
                Ok(wrap(ExprKind::Unary(UnOp::Neg, Box::new(e)), self))
            }
            Tok::Plus => Err(Diagnostic::new("unary `+` is not supported", start).hint("use `Number(x)`")),
            Tok::Await => {
                self.bump()?;
                let e = self.unary()?;
                Ok(wrap(ExprKind::Await(Box::new(e)), self))
            }
            Tok::PlusPlus | Tok::MinusMinus => {
                let inc = self.bump()?.tok == Tok::PlusPlus;
                let target = self.unary()?;
                Ok(wrap(ExprKind::Update { inc, target: Box::new(target) }, self))
            }
            _ => self.postfix(),
        }
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        loop {
            let start = e.span;
            let kind = match self.cur.tok {
                Tok::Dot => {
                    self.bump()?;
                    let (name, _) = self.prop_name()?;
                    ExprKind::Member { obj: Box::new(e), name, optional: false }
                }
                Tok::QDot => {
                    self.bump()?;
                    if self.eat(&Tok::LBracket)? {
                        let index = self.expr()?;
                        self.expect(&Tok::RBracket, "']'")?;
                        ExprKind::Index { obj: Box::new(e), index: Box::new(index), optional: true }
                    } else if self.at(&Tok::LParen) {
                        return Err(Diagnostic::new("`?.()` is not supported", self.cur.span).hint("check the function with `if (f !== null)` first"));
                    } else {
                        let (name, _) = self.prop_name()?;
                        ExprKind::Member { obj: Box::new(e), name, optional: true }
                    }
                }
                Tok::LParen if !self.cur.nl_before => ExprKind::Call { callee: Box::new(e), args: self.args()? },
                Tok::LBracket if !self.cur.nl_before => {
                    self.bump()?;
                    let index = self.expr()?;
                    self.expect(&Tok::RBracket, "']'")?;
                    ExprKind::Index { obj: Box::new(e), index: Box::new(index), optional: false }
                }
                Tok::PlusPlus | Tok::MinusMinus if !self.cur.nl_before => {
                    let inc = self.bump()?.tok == Tok::PlusPlus;
                    ExprKind::Update { inc, target: Box::new(e) }
                }
                // TypeScript non-null assertion `x!`: no runtime meaning.
                Tok::Bang if !self.cur.nl_before => {
                    self.bump()?;
                    continue;
                }
                _ => return Ok(e),
            };
            e = Expr { kind, span: self.since(start) };
        }
    }

    fn args(&mut self) -> PResult<Vec<Expr>> {
        self.expect(&Tok::LParen, "'('")?;
        let mut out = Vec::new();
        while !self.at(&Tok::RParen) {
            if self.at(&Tok::Ellipsis) {
                return Err(Diagnostic::new("spread arguments are not supported yet", self.cur.span).hint("pass the array itself"));
            }
            out.push(self.assign_expr()?);
            if !self.eat(&Tok::Comma)? {
                break;
            }
        }
        self.expect(&Tok::RParen, "')'")?;
        Ok(out)
    }

    fn primary(&mut self) -> PResult<Expr> {
        let start = self.cur.span;
        let kind = match &self.cur.tok {
            Tok::Num(n) => {
                let n = *n;
                self.bump()?;
                ExprKind::Num(n)
            }
            Tok::Str(s) => {
                let s = s.clone();
                self.bump()?;
                ExprKind::Str(s)
            }
            Tok::Template(parts) => {
                let parts = parts.clone();
                self.bump()?;
                self.template(parts)?
            }
            Tok::True | Tok::False => ExprKind::Bool(self.bump()?.tok == Tok::True),
            Tok::Null => {
                self.bump()?;
                ExprKind::Null
            }
            Tok::This => {
                self.bump()?;
                ExprKind::This
            }
            Tok::Ident(name) => {
                let name = name.clone();
                self.bump()?;
                ExprKind::Ident(name)
            }
            Tok::LParen => {
                self.bump()?;
                let e = self.expr()?;
                self.expect(&Tok::RParen, "')'")?;
                return Ok(Expr { kind: e.kind, span: self.since(start) });
            }
            Tok::LBracket => self.array()?,
            Tok::LBrace => self.record()?,
            Tok::Lt => {
                let el = self.jsx_element()?;
                self.bump()?;
                ExprKind::Jsx(Box::new(el))
            }
            Tok::Function | Tok::Async => {
                self.eat(&Tok::Async)?;
                self.expect(&Tok::Function, "'function'")?;
                let name = if let Tok::Ident(n) = &self.cur.tok {
                    let n = n.clone();
                    self.bump()?;
                    n
                } else {
                    "<anonymous>".to_string()
                };
                ExprKind::Arrow(Rc::new(self.fn_rest(name, Vec::new(), start)?))
            }
            Tok::New => {
                self.bump()?;
                let (name, nspan) = self.ident("a name")?;
                if name != "Error" {
                    return Err(Diagnostic::new("`new` is only supported for `new Error(message)`", self.since(start))
                        .hint("objects come from `Name.get(id)`; plain data uses `{ ... }` literals"));
                }
                let args = self.args()?;
                ExprKind::Call { callee: Box::new(Expr { kind: ExprKind::Ident(name), span: nspan }), args }
            }
            _ => return Err(self.unexpected("an expression")),
        };
        Ok(Expr { kind, span: self.since(start) })
    }

    fn template(&mut self, parts: Vec<TplPart>) -> PResult<ExprKind> {
        let mut pieces = Vec::with_capacity(parts.len());
        for part in parts {
            match part {
                TplPart::Str(s) => pieces.push(TplPiece::Str(s)),
                TplPart::Expr(span) => {
                    let mut sub = Parser::new(self.src, span.start as usize)?;
                    let e = sub.expr()?;
                    if !(sub.at(&Tok::RBrace) && sub.cur.span.start == span.end) {
                        return Err(sub.unexpected("'}'"));
                    }
                    pieces.push(TplPiece::Expr(e));
                }
            }
        }
        Ok(ExprKind::Template(pieces))
    }

    fn array(&mut self) -> PResult<ExprKind> {
        self.bump()?;
        let mut items = Vec::new();
        while !self.at(&Tok::RBracket) {
            if self.eat(&Tok::Ellipsis)? {
                items.push(ArrayEntry::Spread(self.assign_expr()?));
            } else {
                items.push(ArrayEntry::Item(self.assign_expr()?));
            }
            if !self.eat(&Tok::Comma)? {
                break;
            }
        }
        self.expect(&Tok::RBracket, "']'")?;
        Ok(ExprKind::Array(items))
    }

    fn record(&mut self) -> PResult<ExprKind> {
        self.bump()?;
        let mut entries = Vec::new();
        while !self.at(&Tok::RBrace) {
            if self.eat(&Tok::Ellipsis)? {
                entries.push(RecordEntry::Spread(self.assign_expr()?));
            } else if self.eat(&Tok::LBracket)? {
                let k = self.assign_expr()?;
                self.expect(&Tok::RBracket, "']'")?;
                self.expect(&Tok::Colon, "':'")?;
                entries.push(RecordEntry::Computed(k, self.assign_expr()?));
            } else {
                let (key, kspan) = self.prop_name()?;
                if self.eat(&Tok::Colon)? {
                    entries.push(RecordEntry::Field(key, self.assign_expr()?));
                } else if self.at(&Tok::LParen) {
                    let f = self.fn_rest(key.clone(), Vec::new(), kspan)?;
                    let span = f.span;
                    entries.push(RecordEntry::Field(key, Expr { kind: ExprKind::Arrow(Rc::new(f)), span }));
                } else {
                    entries.push(RecordEntry::Field(key.clone(), Expr { kind: ExprKind::Ident(key), span: kspan }));
                }
            }
            if !self.eat(&Tok::Comma)? {
                break;
            }
        }
        self.expect(&Tok::RBrace, "'}'")?;
        Ok(ExprKind::Record(entries))
    }

    // ---------- JSX ----------

    /// Parses `<Tag ...>children</Tag>` or `<Tag ... />`.
    /// Starts at `<` and stops with the final `>` as the current token, not consumed,
    /// so the caller decides whether to continue in JSX-text or normal mode.
    fn jsx_element(&mut self) -> PResult<JsxElement> {
        let start = self.cur.span;
        self.bump()?;
        if self.at(&Tok::Gt) {
            return Err(Diagnostic::new("fragments `<>...</>` are not supported", start).hint("wrap the children in <Column> or <Row>"));
        }
        let (tag, _) = self.ident("a component name")?;
        let mut attrs = Vec::new();
        loop {
            match &self.cur.tok {
                Tok::Slash => {
                    self.bump()?;
                    if !self.at(&Tok::Gt) {
                        return Err(self.unexpected("'>'"));
                    }
                    return Ok(JsxElement { tag, attrs, children: Vec::new(), span: start.to(self.cur.span) });
                }
                Tok::Gt => break,
                Tok::LBrace => {
                    return Err(Diagnostic::new("spread attributes are not supported yet", self.cur.span).hint("pass each attribute by name"));
                }
                _ => {
                    let (name, nspan) = self.prop_name()?;
                    let value = if self.eat(&Tok::Assign)? {
                        match &self.cur.tok {
                            Tok::Str(s) => {
                                let e = Expr { kind: ExprKind::Str(s.clone()), span: self.cur.span };
                                self.bump()?;
                                e
                            }
                            Tok::LBrace => {
                                self.bump()?;
                                let e = self.expr()?;
                                self.expect(&Tok::RBrace, "'}'")?;
                                e
                            }
                            _ => return Err(self.unexpected("\"text\" or {expression}")),
                        }
                    } else {
                        Expr { kind: ExprKind::Bool(true), span: nspan }
                    };
                    attrs.push((name, value));
                }
            }
        }
        let children = self.jsx_children(&tag, start)?;
        Ok(JsxElement { tag, attrs, children, span: start.to(self.cur.span) })
    }

    fn jsx_children(&mut self, tag: &str, open: Span) -> PResult<Vec<JsxChild>> {
        let mut out = Vec::new();
        loop {
            // The lexer sits right after the current token (`>` or `}`).
            let text = self.lex.read_jsx_text();
            if !text.is_empty() {
                out.push(JsxChild::Text(text));
            }
            match self.lex.peek_byte() {
                b'{' => {
                    self.bump()?;
                    self.bump()?;
                    if self.at(&Tok::RBrace) {
                        continue;
                    }
                    let e = self.expr()?;
                    if !self.at(&Tok::RBrace) {
                        return Err(self.unexpected("'}'"));
                    }
                    out.push(JsxChild::Expr(e));
                }
                b'<' => {
                    self.bump()?;
                    if self.lex.clone().next_token()?.tok == Tok::Slash {
                        self.bump()?;
                        self.bump()?;
                        let (close, cspan) = self.ident("a closing tag name")?;
                        if close != tag {
                            return Err(Diagnostic::new(format!("expected </{tag}>, found </{close}>"), cspan));
                        }
                        if !self.at(&Tok::Gt) {
                            return Err(self.unexpected("'>'"));
                        }
                        return Ok(out);
                    }
                    out.push(JsxChild::Element(self.jsx_element()?));
                }
                _ => return Err(Diagnostic::new(format!("<{tag}> is never closed"), open).hint(format!("add </{tag}>"))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(src: &str) -> Program {
        parse(src).unwrap_or_else(|e| panic!("{}", e.render("test", src)))
    }

    fn err(src: &str) -> Diagnostic {
        parse(src).expect_err("expected a parse error")
    }

    #[test]
    fn parses_object_with_decorators() {
        let p = ok(r#"
            object Counter {
              count: number = 0
              label = "x";
              @every("1s") tick() { this.count += 1 }
              private reset(): void { this.count = 0 }
            }
        "#);
        let Item::Object(o) = &p.items[0] else { panic!() };
        assert_eq!(o.fields.len(), 2);
        assert_eq!(o.methods.len(), 2);
        assert_eq!(o.methods[0].decorators[0].name, "every");
        assert!(o.methods[1].private);
    }

    #[test]
    fn parses_arrows_and_types() {
        ok("const f = (a: number, b = 2): number => a + b\nconst g = x => x * 2\nconst h = async () => { return 1 }");
        ok("type Todo = { id: string; done?: boolean; tags: string[] }\ninterface P { title: string, on(x: number): void }");
        ok("function f() { const c = true ? (1) : 2; return c }");
    }

    #[test]
    fn parses_jsx() {
        let p = ok(r#"
            function App() {
              return <Column gap={8}>
                <Text>Count: {n} items</Text>
                {items.map(i => <Text key={i}>{i}</Text>)}
                <Button onPress={() => inc()} disabled />
              </Column>
            }
        "#);
        let Item::Function(f) = &p.items[0] else { panic!() };
        let Stmt::Return(Some(e), _) = &f.body[0] else { panic!() };
        let ExprKind::Jsx(el) = &e.kind else { panic!() };
        assert_eq!(el.tag, "Column");
        assert_eq!(el.children.len(), 3);
        let JsxChild::Element(text) = &el.children[0] else { panic!() };
        assert!(matches!(&text.children[0], JsxChild::Text(t) if t == "Count: "));
    }

    #[test]
    fn helpful_errors() {
        assert_eq!(err("function f() { var x = 1 }").hint.as_deref(), Some("use `let` or `const`"));
        assert_eq!(err("function f() { return a == b }").hint.as_deref(), Some("use `===`"));
        assert!(err("function f() { return <Text>hi</Txt> }").message.contains("expected </Text>"));
        assert!(err("class A {}").hint.unwrap().contains("object"));
    }

    #[test]
    fn newline_does_not_continue_call() {
        let p = ok("function f() { let a = b\n(c) }");
        let Item::Function(f) = &p.items[0] else { panic!() };
        assert_eq!(f.body.len(), 2);
    }
}

//! On-demand lexer. The parser pulls one token at a time, which lets it switch
//! to raw-text mode inside JSX children.

use super::{Diagnostic, Span};

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Num(f64),
    Str(String),
    Template(Vec<TplPart>),
    // keywords
    Let,
    Const,
    Function,
    Return,
    If,
    Else,
    While,
    For,
    Of,
    Break,
    Continue,
    True,
    False,
    Null,
    Object,
    This,
    Await,
    Async,
    Export,
    Default,
    Type,
    Interface,
    Private,
    Try,
    Catch,
    Throw,
    New,
    As,
    /// Words that look like JS/TS but are not part of the language; the parser explains why.
    Reserved(&'static str),
    // punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semi,
    Colon,
    Dot,
    QDot,
    Question,
    QQ,
    Arrow,
    Ellipsis,
    At,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Bang,
    Lt,
    Gt,
    Le,
    Ge,
    EqEqEq,
    NotEqEq,
    EqEq,
    NotEq,
    AndAnd,
    OrOr,
    Pipe,
    Amp,
    Assign,
    PlusAssign,
    MinusAssign,
    StarAssign,
    SlashAssign,
    PlusPlus,
    MinusMinus,
    Eof,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TplPart {
    Str(String),
    /// Byte span of the expression inside `${ }`.
    Expr(Span),
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
    /// A line break separates this token from the previous one.
    pub nl_before: bool,
}

#[derive(Clone)]
pub struct Lexer<'a> {
    src: &'a str,
    b: &'a [u8],
    pub pos: usize,
}

const RESERVED: &[&str] = &["var", "class", "undefined", "switch", "typeof", "delete", "import", "finally", "instanceof", "in", "do", "yield", "enum"];

fn keyword(s: &str) -> Option<Tok> {
    Some(match s {
        "let" => Tok::Let,
        "const" => Tok::Const,
        "function" => Tok::Function,
        "return" => Tok::Return,
        "if" => Tok::If,
        "else" => Tok::Else,
        "while" => Tok::While,
        "for" => Tok::For,
        "of" => Tok::Of,
        "break" => Tok::Break,
        "continue" => Tok::Continue,
        "true" => Tok::True,
        "false" => Tok::False,
        "null" => Tok::Null,
        "object" => Tok::Object,
        "this" => Tok::This,
        "await" => Tok::Await,
        "async" => Tok::Async,
        "export" => Tok::Export,
        "default" => Tok::Default,
        "type" => Tok::Type,
        "interface" => Tok::Interface,
        "private" => Tok::Private,
        "try" => Tok::Try,
        "catch" => Tok::Catch,
        "throw" => Tok::Throw,
        "new" => Tok::New,
        "as" => Tok::As,
        _ => return RESERVED.iter().find(|r| **r == s).map(|r| Tok::Reserved(r)),
    })
}

/// Name of a keyword token when it is used as a property name (`x.default`, `{ type: 1 }`).
pub fn keyword_text(t: &Tok) -> Option<&'static str> {
    Some(match t {
        Tok::Let => "let",
        Tok::Const => "const",
        Tok::Function => "function",
        Tok::Return => "return",
        Tok::If => "if",
        Tok::Else => "else",
        Tok::While => "while",
        Tok::For => "for",
        Tok::Of => "of",
        Tok::Break => "break",
        Tok::Continue => "continue",
        Tok::True => "true",
        Tok::False => "false",
        Tok::Null => "null",
        Tok::Object => "object",
        Tok::This => "this",
        Tok::Await => "await",
        Tok::Async => "async",
        Tok::Export => "export",
        Tok::Default => "default",
        Tok::Type => "type",
        Tok::Interface => "interface",
        Tok::Private => "private",
        Tok::Try => "try",
        Tok::Catch => "catch",
        Tok::Throw => "throw",
        Tok::New => "new",
        Tok::As => "as",
        Tok::Reserved(r) => r,
        _ => return None,
    })
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Self::at(src, 0)
    }

    pub fn at(src: &'a str, pos: usize) -> Self {
        Lexer { src, b: src.as_bytes(), pos }
    }

    fn peek(&self) -> u8 {
        self.b.get(self.pos).copied().unwrap_or(0)
    }

    fn peek_at(&self, n: usize) -> u8 {
        self.b.get(self.pos + n).copied().unwrap_or(0)
    }

    pub fn peek_byte(&self) -> u8 {
        self.peek()
    }

    fn err(&self, msg: impl Into<String>, start: usize) -> Diagnostic {
        Diagnostic::new(msg, Span::new(start, self.pos.max(start + 1)))
    }

    /// Skips whitespace and comments. Returns true if a newline was crossed.
    fn skip_trivia(&mut self) -> Result<bool, Diagnostic> {
        let mut nl = false;
        loop {
            match self.peek() {
                b'\n' => {
                    nl = true;
                    self.pos += 1;
                }
                b' ' | b'\t' | b'\r' => self.pos += 1,
                b'/' if self.peek_at(1) == b'/' => {
                    while self.pos < self.b.len() && self.peek() != b'\n' {
                        self.pos += 1;
                    }
                }
                b'/' if self.peek_at(1) == b'*' => {
                    let start = self.pos;
                    self.pos += 2;
                    loop {
                        if self.pos >= self.b.len() {
                            return Err(self.err("unclosed comment", start));
                        }
                        if self.peek() == b'*' && self.peek_at(1) == b'/' {
                            self.pos += 2;
                            break;
                        }
                        if self.peek() == b'\n' {
                            nl = true;
                        }
                        self.pos += 1;
                    }
                }
                _ => return Ok(nl),
            }
        }
    }

    pub fn next_token(&mut self) -> Result<Token, Diagnostic> {
        let nl_before = self.skip_trivia()?;
        let start = self.pos;
        let tok = self.scan(start)?;
        Ok(Token { tok, span: Span::new(start, self.pos), nl_before })
    }

    fn scan(&mut self, start: usize) -> Result<Tok, Diagnostic> {
        let c = self.peek();
        if self.pos >= self.b.len() {
            return Ok(Tok::Eof);
        }
        if c.is_ascii_digit() || (c == b'.' && self.peek_at(1).is_ascii_digit()) {
            return self.number(start);
        }
        if c == b'"' || c == b'\'' {
            return self.string(c, start).map(Tok::Str);
        }
        if c == b'`' {
            return self.template(start);
        }
        if is_ident_start(self.src[self.pos..].chars().next().unwrap_or(' ')) {
            let rest = &self.src[self.pos..];
            let len = rest.char_indices().find(|(_, ch)| !is_ident_continue(*ch)).map_or(rest.len(), |(i, _)| i);
            let word = &rest[..len];
            self.pos += len;
            return Ok(keyword(word).unwrap_or_else(|| Tok::Ident(word.to_string())));
        }
        let three = [c, self.peek_at(1), self.peek_at(2)];
        let (tok, n) = match three {
            [b'=', b'=', b'='] => (Tok::EqEqEq, 3),
            [b'!', b'=', b'='] => (Tok::NotEqEq, 3),
            [b'.', b'.', b'.'] => (Tok::Ellipsis, 3),
            [b'=', b'=', _] => (Tok::EqEq, 2),
            [b'!', b'=', _] => (Tok::NotEq, 2),
            [b'=', b'>', _] => (Tok::Arrow, 2),
            [b'<', b'=', _] => (Tok::Le, 2),
            [b'>', b'=', _] => (Tok::Ge, 2),
            [b'&', b'&', _] => (Tok::AndAnd, 2),
            [b'|', b'|', _] => (Tok::OrOr, 2),
            [b'?', b'?', _] => (Tok::QQ, 2),
            [b'?', b'.', d] if !d.is_ascii_digit() => (Tok::QDot, 2),
            [b'+', b'=', _] => (Tok::PlusAssign, 2),
            [b'-', b'=', _] => (Tok::MinusAssign, 2),
            [b'*', b'=', _] => (Tok::StarAssign, 2),
            [b'/', b'=', _] => (Tok::SlashAssign, 2),
            [b'+', b'+', _] => (Tok::PlusPlus, 2),
            [b'-', b'-', _] => (Tok::MinusMinus, 2),
            [b'(', ..] => (Tok::LParen, 1),
            [b')', ..] => (Tok::RParen, 1),
            [b'{', ..] => (Tok::LBrace, 1),
            [b'}', ..] => (Tok::RBrace, 1),
            [b'[', ..] => (Tok::LBracket, 1),
            [b']', ..] => (Tok::RBracket, 1),
            [b',', ..] => (Tok::Comma, 1),
            [b';', ..] => (Tok::Semi, 1),
            [b':', ..] => (Tok::Colon, 1),
            [b'.', ..] => (Tok::Dot, 1),
            [b'?', ..] => (Tok::Question, 1),
            [b'@', ..] => (Tok::At, 1),
            [b'+', ..] => (Tok::Plus, 1),
            [b'-', ..] => (Tok::Minus, 1),
            [b'*', ..] => (Tok::Star, 1),
            [b'/', ..] => (Tok::Slash, 1),
            [b'%', ..] => (Tok::Percent, 1),
            [b'!', ..] => (Tok::Bang, 1),
            [b'<', ..] => (Tok::Lt, 1),
            [b'>', ..] => (Tok::Gt, 1),
            [b'|', ..] => (Tok::Pipe, 1),
            [b'&', ..] => (Tok::Amp, 1),
            [b'=', ..] => (Tok::Assign, 1),
            _ => {
                let ch = self.src[self.pos..].chars().next().unwrap_or('?');
                self.pos += ch.len_utf8();
                return Err(self.err(format!("unexpected character '{ch}'"), start));
            }
        };
        self.pos += n;
        Ok(tok)
    }

    fn number(&mut self, start: usize) -> Result<Tok, Diagnostic> {
        if self.peek() == b'0' && matches!(self.peek_at(1), b'x' | b'X') {
            self.pos += 2;
            let digits_start = self.pos;
            while self.peek().is_ascii_hexdigit() || self.peek() == b'_' {
                self.pos += 1;
            }
            let digits: String = self.src[digits_start..self.pos].chars().filter(|c| *c != '_').collect();
            return u64::from_str_radix(&digits, 16).map(|n| Tok::Num(n as f64)).map_err(|_| self.err("invalid hex number", start));
        }
        while self.peek().is_ascii_digit() || self.peek() == b'_' {
            self.pos += 1;
        }
        if self.peek() == b'.' && self.peek_at(1).is_ascii_digit() {
            self.pos += 1;
            while self.peek().is_ascii_digit() || self.peek() == b'_' {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), b'e' | b'E') {
            let save = self.pos;
            self.pos += 1;
            if matches!(self.peek(), b'+' | b'-') {
                self.pos += 1;
            }
            if self.peek().is_ascii_digit() {
                while self.peek().is_ascii_digit() {
                    self.pos += 1;
                }
            } else {
                self.pos = save;
            }
        }
        let text: String = self.src[start..self.pos].chars().filter(|c| *c != '_').collect();
        text.parse::<f64>().map(Tok::Num).map_err(|_| self.err("invalid number", start))
    }

    fn escape(&mut self, out: &mut String, start: usize) -> Result<(), Diagnostic> {
        self.pos += 1; // backslash
        let c = self.peek();
        self.pos += 1;
        match c {
            b'n' => out.push('\n'),
            b't' => out.push('\t'),
            b'r' => out.push('\r'),
            b'0' => out.push('\0'),
            b'u' => {
                let (digits, braced) = if self.peek() == b'{' {
                    let end = self.src[self.pos..].find('}').ok_or_else(|| self.err("bad unicode escape", start))?;
                    (&self.src[self.pos + 1..self.pos + end], true)
                } else {
                    (self.src.get(self.pos..self.pos + 4).ok_or_else(|| self.err("bad unicode escape", start))?, false)
                };
                let ch = u32::from_str_radix(digits, 16).ok().and_then(char::from_u32).ok_or_else(|| self.err("bad unicode escape", start))?;
                self.pos += digits.len() + if braced { 2 } else { 0 };
                out.push(ch);
            }
            b'\n' => {}
            0 => return Err(self.err("unterminated string", start)),
            _ => {
                self.pos -= 1;
                let ch = self.src[self.pos..].chars().next().unwrap_or('\\');
                self.pos += ch.len_utf8();
                out.push(ch);
            }
        }
        Ok(())
    }

    fn string(&mut self, quote: u8, start: usize) -> Result<String, Diagnostic> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            match self.peek() {
                0 if self.pos >= self.b.len() => return Err(self.err("unterminated string", start)),
                b'\n' => return Err(self.err("unterminated string", start).hint("use a template string `...` for multi-line text")),
                b'\\' => self.escape(&mut out, start)?,
                c if c == quote => {
                    self.pos += 1;
                    return Ok(out);
                }
                _ => {
                    let ch = self.src[self.pos..].chars().next().unwrap_or('\0');
                    self.pos += ch.len_utf8();
                    out.push(ch);
                }
            }
        }
    }

    fn template(&mut self, start: usize) -> Result<Tok, Diagnostic> {
        self.pos += 1;
        let mut parts = Vec::new();
        let mut cur = String::new();
        loop {
            match self.peek() {
                0 if self.pos >= self.b.len() => return Err(self.err("unterminated template string", start)),
                b'`' => {
                    self.pos += 1;
                    if !cur.is_empty() {
                        parts.push(TplPart::Str(cur));
                    }
                    return Ok(Tok::Template(parts));
                }
                b'\\' => self.escape(&mut cur, start)?,
                b'$' if self.peek_at(1) == b'{' => {
                    if !cur.is_empty() {
                        parts.push(TplPart::Str(std::mem::take(&mut cur)));
                    }
                    let expr_start = self.pos + 2;
                    // Lex through the expression to find its closing brace.
                    let mut sub = Lexer::at(self.src, expr_start);
                    let mut depth = 0usize;
                    loop {
                        let t = sub.next_token()?;
                        match t.tok {
                            Tok::LBrace => depth += 1,
                            Tok::RBrace if depth == 0 => {
                                parts.push(TplPart::Expr(Span::new(expr_start, t.span.start as usize)));
                                self.pos = t.span.end as usize;
                                break;
                            }
                            Tok::RBrace => depth -= 1,
                            Tok::Eof => return Err(self.err("unterminated ${ in template string", start)),
                            _ => {}
                        }
                    }
                }
                _ => {
                    let ch = self.src[self.pos..].chars().next().unwrap_or('\0');
                    self.pos += ch.len_utf8();
                    cur.push(ch);
                }
            }
        }
    }

    /// Reads raw JSX text up to the next `<` or `{`, applying JSX whitespace rules:
    /// text on one line is kept as is; multi-line text is trimmed per line and joined with spaces.
    pub fn read_jsx_text(&mut self) -> String {
        let start = self.pos;
        while self.pos < self.b.len() && !matches!(self.peek(), b'<' | b'{') {
            self.pos += 1;
        }
        let raw = &self.src[start..self.pos];
        if !raw.contains('\n') {
            return raw.to_string();
        }
        let lines: Vec<&str> = raw.lines().collect();
        let last = lines.len().saturating_sub(1);
        let mut out = String::new();
        for (i, line) in lines.iter().enumerate() {
            let mut l = *line;
            if i != 0 {
                l = l.trim_start();
            }
            if i != last {
                l = l.trim_end();
            }
            if l.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(l);
        }
        out
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$'
}

fn is_ident_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<Tok> {
        let mut l = Lexer::new(src);
        let mut out = vec![];
        loop {
            let t = l.next_token().unwrap();
            if t.tok == Tok::Eof {
                return out;
            }
            out.push(t.tok);
        }
    }

    #[test]
    fn lexes_operators_and_keywords() {
        assert_eq!(toks("let x = a?.b ?? 1_000 === y"), vec![
            Tok::Let,
            Tok::Ident("x".into()),
            Tok::Assign,
            Tok::Ident("a".into()),
            Tok::QDot,
            Tok::Ident("b".into()),
            Tok::QQ,
            Tok::Num(1000.0),
            Tok::EqEqEq,
            Tok::Ident("y".into()),
        ]);
    }

    #[test]
    fn lexes_strings_and_templates() {
        assert_eq!(toks(r#""a\nb" 'c'"#), vec![Tok::Str("a\nb".into()), Tok::Str("c".into())]);
        let t = toks("`x ${ {a: 1}.a } y`");
        let Tok::Template(parts) = &t[0] else { panic!() };
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], TplPart::Str("x ".into()));
    }

    #[test]
    fn tracks_newlines_and_comments() {
        let mut l = Lexer::new("a // c\n /* x */ b");
        assert!(!l.next_token().unwrap().nl_before);
        assert!(l.next_token().unwrap().nl_before);
    }

    #[test]
    fn jsx_text_whitespace() {
        let mut l = Lexer::new("\n   Hello\n   world  {");
        assert_eq!(l.read_jsx_text(), "Hello world  ");
        let mut l = Lexer::new(" Count: <");
        assert_eq!(l.read_jsx_text(), " Count: ");
    }
}

//! Source text to AST.

pub mod ast;
pub mod lexer;
pub mod parser;

pub use parser::parse;

/// Byte range in a source file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Span { start: start as u32, end: end as u32 }
    }
    pub fn to(self, other: Span) -> Span {
        Span { start: self.start, end: other.end.max(self.end) }
    }
}

/// A compile error with an optional fix hint. Hints are written for agents and people alike.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub message: String,
    pub span: Span,
    pub hint: Option<String>,
}

impl Diagnostic {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        Diagnostic { message: message.into(), span, hint: None }
    }
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
    /// `file:line:col: error: message`, the source line, a caret and the hint.
    pub fn render(&self, file: &str, src: &str) -> String {
        let (line, col) = line_col(src, self.span.start as usize);
        let text = src.lines().nth(line - 1).unwrap_or("");
        let width = (self.span.end.saturating_sub(self.span.start) as usize).clamp(1, text.len().saturating_sub(col - 1).max(1));
        let mut out = format!("{file}:{line}:{col}: error: {}\n  {text}\n  {}{}", self.message, " ".repeat(col - 1), "^".repeat(width));
        if let Some(h) = &self.hint {
            out.push_str("\n  hint: ");
            out.push_str(h);
        }
        out
    }
}

/// The closest candidate within two edits of `name`, for "did you mean" hints.
pub fn closest<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let limit = if name.chars().count() <= 3 { 1 } else { 2 };
    candidates.into_iter().filter(|c| *c != name).map(|c| (edit_distance(name, c), c)).filter(|(d, _)| *d <= limit).min_by_key(|(d, _)| *d).map(|(_, c)| c)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j] + usize::from(ca != *cb)).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// 1-based line and column (in chars) of a byte offset.
pub fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(src.len());
    let before = &src[..floor_char_boundary(src, offset)];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, col)
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

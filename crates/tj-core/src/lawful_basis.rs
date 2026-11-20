//! The lawful basis of each purpose personal data is held for (docs/privacy.md):
//!
//! ```ts
//! const lawfulBasis = { account: "contract", marketing: "consent" }
//! ```
//!
//! Optional: an app without it compiles as before. When present, every purpose a `@pii`
//! field names needs a basis, and only purposes in use may be listed. Compliance packs
//! (`tj check --compliance`) read it.

use crate::syntax::ast::{ExprKind, Item, Pattern, Program, RecordEntry};
use crate::syntax::{Diagnostic, Span};
use std::collections::BTreeSet;

/// The lawful bases, as GDPR article 6(1) and most other laws name them.
pub const BASES: &[&str] = &["consent", "contract", "legal-obligation", "vital-interest", "public-task", "legitimate-interest"];

/// The purposes and their bases, in the order written; `None` when the app declares none.
pub fn declared(files: &[(&str, &Program)]) -> Option<Vec<(String, String)>> {
    find(files).map(|f| f.entries.unwrap_or_default().into_iter().filter_map(|(p, b, _)| Some((p, b?))).collect())
}

/// Checks the declaration against the `@pii` fields.
pub fn check(files: &[(&str, &Program)]) -> Vec<Diagnostic> {
    let mut found = all(files);
    let Some(Found { entries, span, mutable }) = (!found.is_empty()).then(|| found.remove(0)) else { return Vec::new() };
    if let Some(again) = found.first() {
        return vec![Diagnostic::new("`lawfulBasis` is declared more than once", again.span).hint("declare it once, in one file, with every purpose")];
    }
    let fix = "write `const lawfulBasis = { account: \"contract\" }`, one entry per purpose";
    let Some(entries) = entries else {
        return vec![Diagnostic::new("`lawfulBasis` must be a record of purposes and their lawful bases", span).hint(fix)];
    };
    let mut errors = Vec::new();
    if mutable {
        errors.push(Diagnostic::new("`lawfulBasis` must be declared with const", span).hint(fix));
    }
    let used = used(files);
    let hint_bases = format!("the bases are {}", BASES.join(", "));
    for (purpose, basis, at) in &entries {
        match basis {
            None => errors.push(Diagnostic::new(format!("the basis of \"{purpose}\" must be a string"), *at).hint(hint_bases.clone())),
            Some(b) if !BASES.contains(&b.as_str()) => errors.push(Diagnostic::new(format!("\"{b}\" is not a lawful basis"), *at).hint(hint_bases.clone())),
            Some(_) => {}
        }
        if !used.contains(purpose) {
            errors.push(Diagnostic::new(format!("no @pii field is held for \"{purpose}\""), *at).hint("list only the purposes your @pii fields name, or remove this one"));
        }
    }
    let missing: Vec<&String> = used.iter().filter(|p| !entries.iter().any(|(e, _, _)| e == *p)).collect();
    if !missing.is_empty() {
        let names: Vec<String> = missing.iter().map(|p| format!("\"{p}\"")).collect();
        let example: Vec<String> = missing.iter().map(|p| format!("{p}: \"contract\"")).collect();
        errors.push(Diagnostic::new(format!("purposes without a lawful basis: {}", names.join(", ")), span).hint(format!("add them to `lawfulBasis`, e.g. {}; {hint_bases}", example.join(", "))));
    }
    errors
}

/// A purpose, its basis when written as a string, and where.
type Entry = (String, Option<String>, Span);

struct Found {
    /// `None` when the value is not a record of plain entries.
    entries: Option<Vec<Entry>>,
    span: Span,
    mutable: bool,
}

/// The app's top-level `lawfulBasis` declaration (the first, if there are more).
fn find(files: &[(&str, &Program)]) -> Option<Found> {
    all(files).into_iter().next()
}

/// Every top-level `lawfulBasis` declaration, in every file of the app.
fn all(files: &[(&str, &Program)]) -> Vec<Found> {
    let items = files.iter().filter(|(name, _)| !name.starts_with("tj:")).flat_map(|(_, p)| &p.items);
    items.filter_map(|i| if let Item::Let(d) = i { Some(d) } else { None }).filter(|d| matches!(&d.pattern, Pattern::Ident(n, _) if n == "lawfulBasis")).map(|d| {
        let entries = match d.init.as_ref().map(|e| &e.kind) {
            Some(ExprKind::Record(fields)) => fields
                .iter()
                .map(|f| match f {
                    RecordEntry::Field(purpose, v) => Some((purpose.clone(), if let ExprKind::Str(b) = &v.kind { Some(b.clone()) } else { None }, v.span)),
                    _ => None,
                })
                .collect(),
            _ => None,
        };
        Found { entries, span: d.span, mutable: d.mutable }
    })
    .collect()
}

/// Every purpose a `@pii` field names.
fn used(files: &[(&str, &Program)]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (_, p) in files {
        for item in &p.items {
            let Item::Object(o) = item else { continue };
            for d in o.fields.iter().flat_map(|f| &f.decorators).filter(|d| d.name == "pii") {
                out.extend(d.args.iter().skip(1).filter_map(|a| if let ExprKind::Str(s) = &a.kind { Some(s.clone()) } else { None }));
            }
        }
    }
    out
}

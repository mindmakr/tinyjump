//! `tj check --access`: every object's read rule and every method's call rule, as one table
//! for people and agents to review, with warnings for likely mistakes
//! (docs/access-control.md).

use tj_core::modules;
use tj_core::syntax::ast::{Decorator, ExprKind, Item, Pattern};
use tj_core::syntax::parse_range;

fn words(d: &Decorator) -> String {
    d.args.iter().filter_map(|a| if let ExprKind::Str(s) = &a.kind { Some(s.as_str()) } else { None }).collect::<Vec<_>>().join(", ")
}

/// Parameter names that suggest identity is being taken from the caller's arguments.
const IDENTITY_NAMES: &[&str] = &["author", "by", "user", "userId", "owner", "who"];

/// Access warnings for likely mistakes (shared by `tj check --access` and `--manifest`).
pub fn warnings(source: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    for f in modules::files(source) {
        if tj_core::i18n::is_locale_file(&f.name) {
            continue;
        }
        let Ok(program) = parse_range(source, f.start, f.end) else { continue };
        for item in &program.items {
            let Item::Object(o) = item else { continue };
            let server = o.decorators.iter().any(|d| d.name == "server");
            let read = o.decorators.iter().find(|d| d.name == "read").map(words);
            if server && read.is_none() {
                warnings.push(format!("@server object {} has no @read rule: only server code can read it, and devices cannot hold replicas (right for private data)", o.name));
            }
            for m in &o.methods {
                let name = &m.func.name;
                let allow = m.decorators.iter().find(|d| d.name == "allow").map(words);
                if allow.as_deref().is_some_and(|a| a.split(", ").any(|w| w == "anyone")) {
                    for p in &m.func.params {
                        if let Pattern::Ident(param, _) = &p.pattern {
                            if IDENTITY_NAMES.contains(&param.as_str()) {
                                warnings.push(format!("{}.{name} lets anyone pass `{param}`: take identity from `caller.id`, not from arguments", o.name));
                            }
                        }
                    }
                }
            }
        }
    }
    warnings
}

/// The access table of a compiled app (`source` is its bundle).
pub fn report(source: &str) -> String {
    let mut table = String::new();
    for f in modules::files(source) {
        if tj_core::i18n::is_locale_file(&f.name) {
            continue;
        }
        let Ok(program) = parse_range(source, f.start, f.end) else { continue };
        for item in &program.items {
            let Item::Object(o) = item else { continue };
            let server = o.decorators.iter().any(|d| d.name == "server");
            let read = o.decorators.iter().find(|d| d.name == "read").map(words);
            let where_ = if server { "@server object" } else { "object" };
            table.push_str(&format!("{where_} {}  read: {}\n", o.name, read.as_deref().unwrap_or("not remote")));
            for f in &o.fields {
                if let Some(rule) = f.decorators.iter().find(|d| d.name == "read").map(words) {
                    table.push_str(&format!("  .{}  read: {rule}\n", f.name));
                }
            }
            for m in &o.methods {
                let name = &m.func.name;
                let allow = m.decorators.iter().find(|d| d.name == "allow").map(words);
                let call = match (&allow, m.private) {
                    (_, true) => "private".to_string(),
                    (Some(a), _) => a.clone(),
                    (None, _) => "not remote".to_string(),
                };
                table.push_str(&format!("  {name}  call: {call}\n"));
            }
        }
    }
    if table.is_empty() {
        table.push_str("no objects\n");
    }
    for w in warnings(source) {
        table.push_str(&format!("warning: {w}\n"));
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_rules_and_warns_about_likely_mistakes() {
        let src = "@server @read(\"anyone\") object Room {\n  @allow(\"anyone\") post(author: string, text: string) {}\n  private clear() {}\n  tidy() {}\n}\n@server object Ledger {}\n";
        let r = report(src);
        assert!(r.contains("@server object Room  read: anyone") && r.contains("post  call: anyone") && r.contains("clear  call: private") && r.contains("tidy  call: not remote"), "{r}");
        assert!(r.contains("Ledger has no @read") && r.contains("Room.post lets anyone pass `author`"), "{r}");
    }
}

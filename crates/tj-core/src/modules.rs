//! Apps made of several files.
//!
//! An app travels as one text, a **bundle**: every file starts with a marker line
//! `//# file: <path>`, where the path is relative to the entry file's folder and the entry
//! comes first. A one-file app is just its source, without a marker. Hosts pass a bundle like
//! any other source, and the core does no I/O: `tj` builds the bundle by following imports.
//!
//! Files share one set of top-level names, so a name is declared once per app. A file uses
//! another file's name by importing it, `import { Cart } from "./cart"`, which the other
//! file must `export`. Files run in dependency order.

use crate::syntax::ast::{item_names, Program};
use crate::syntax::{closest, line_col, Diagnostic, Span};
use std::collections::HashSet;

pub const MARKER: &str = "//# file: ";

/// The modules of the plugins that ship with tinyjump (docs/plugins.md), by import name.
/// The compiler adds one to an app that imports it, so every host has them.
pub const BUILT_IN: &[(&str, &str)] = &[
    ("tj:ai", include_str!("../../../plugins/ai/plugin.tj")),
    ("tj:agent", include_str!("../../../plugins/agent/plugin.tj")),
];

/// `src` plus the built-in plugin modules it imports and does not carry itself, including
/// those the added modules import in turn.
pub fn with_built_ins(src: &str) -> std::borrow::Cow<'_, str> {
    let mut out = std::borrow::Cow::Borrowed(src);
    loop {
        let present: Vec<String> = files(&out).into_iter().map(|f| f.name).collect();
        let Some((name, module)) = BUILT_IN.iter().find(|(name, _)| out.contains(&format!("\"{name}\"")) && !present.iter().any(|p| p == name)) else {
            return out;
        };
        let text = out.to_mut();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("{MARKER}{name}\n{module}"));
        if !module.ends_with('\n') {
            text.push('\n');
        }
    }
}

/// The entry's name when a bundle does not name it (a one-file app).
pub const ENTRY: &str = "app.tj";

/// One file of a bundle: its name and where its text is.
pub struct File {
    pub name: String,
    pub start: usize,
    pub end: usize,
}

/// Splits a bundle into its files; the entry comes first.
pub fn files(src: &str) -> Vec<File> {
    let mut out = vec![File { name: ENTRY.to_string(), start: 0, end: src.len() }];
    let mut pos = 0;
    for line in src.split_inclusive('\n') {
        if let Some(name) = line.strip_prefix(MARKER) {
            if let Some(last) = out.last_mut() {
                last.end = pos;
            }
            out.push(File { name: name.trim().to_string(), start: pos + line.len(), end: src.len() });
        }
        pos += line.len();
    }
    // A bundle that starts with a marker names its entry.
    if out.len() > 1 && out[0].start == out[0].end {
        out.remove(0);
    }
    out
}

/// Builds a bundle from (name, source) pairs, entry first. One file stays as it is.
pub fn bundle(files: &[(String, String)]) -> String {
    if let [(_, only)] = files {
        return only.clone();
    }
    let mut out = String::new();
    for (name, src) in files {
        out.push_str(MARKER);
        out.push_str(name);
        out.push('\n');
        out.push_str(src);
        if !src.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// Where a byte offset of a bundle is: the file (None for a one-file app), line and column.
pub fn locate(src: &str, offset: usize) -> (Option<String>, usize, usize) {
    let mut files = files(src);
    let i = files.iter().rposition(|f| f.start <= offset).unwrap_or(0);
    let (line, col) = line_col(&src[files[i].start..files[i].end], offset.saturating_sub(files[i].start));
    let name = (files.len() > 1).then(|| std::mem::take(&mut files[i].name));
    (name, line, col)
}

/// The bundle name an import refers to, e.g. `./cart` from `lib/a.tj` is `lib/cart.tj`.
/// A plugin's module is named as imported: `tj:ai` (docs/plugins.md).
pub fn resolve(importer: &str, spec: &str) -> Result<String, Diagnostic> {
    let bad = |message: String, hint: &str| Err(Diagnostic::new(message, Span::default()).hint(hint));
    if let Some(name) = spec.strip_prefix("tj:") {
        let ok = name.chars().next().is_some_and(|c| c.is_ascii_lowercase()) && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        return if ok { Ok(spec.to_string()) } else { bad(format!("\"{spec}\" is not a plugin name"), "plugins are named like \"tj:ai\": lower-case letters, digits and -") };
    }
    if !(spec.starts_with("./") || spec.starts_with("../")) {
        return bad(format!("cannot import \"{spec}\": only files of this app can be imported"), "use a relative path like \"./cart\"; packages are not supported");
    }
    if spec.ends_with(".tj") || spec.ends_with(".ts") {
        return bad(format!("import paths have no extension: \"{spec}\""), "write \"./cart\" for cart.tj");
    }
    let mut parts: Vec<&str> = importer.split('/').collect();
    parts.pop(); // the importer's own file name
    for seg in spec.split('/') {
        match seg {
            "." | "" => {}
            ".." => {
                if parts.pop().is_none() {
                    return bad(format!("cannot import \"{spec}\": it is outside the app's folder"), "keep every file of the app under the entry file's folder");
                }
            }
            s => parts.push(s),
        }
    }
    Ok(format!("{}.tj", parts.join("/")))
}

/// The import path from one file to another, for hints: `./cart` or `../cart`.
pub fn relative(from: &str, to: &str) -> String {
    let from: Vec<&str> = from.split('/').collect();
    let to: Vec<&str> = to.trim_end_matches(".tj").split('/').collect();
    let common = from[..from.len() - 1].iter().zip(&to).take_while(|(a, b)| a == b).count();
    let ups = from.len() - 1 - common;
    let rest = to[common..].join("/");
    if ups == 0 {
        format!("./{rest}")
    } else {
        format!("{}{rest}", "../".repeat(ups))
    }
}

/// How the files of an app fit together.
pub struct Linked {
    /// Files in the order they run: each after the files it imports.
    pub order: Vec<usize>,
    /// Per file, the names it imports.
    pub imports: Vec<HashSet<String>>,
    /// Per file, the names it exports.
    pub exports: Vec<HashSet<String>>,
}

/// Resolves every import: the file must exist and export the names.
pub fn link(files: &[File], programs: &[Program], errors: &mut Vec<Diagnostic>) -> Linked {
    let exports: Vec<HashSet<String>> = programs.iter().map(|p| p.exports.iter().cloned().collect()).collect();
    let mut imports = vec![HashSet::new(); programs.len()];
    let mut deps = vec![Vec::new(); programs.len()];
    for (i, p) in programs.iter().enumerate() {
        for imp in &p.imports {
            let target = match resolve(&files[i].name, &imp.from) {
                Ok(name) => name,
                Err(d) => {
                    errors.push(Diagnostic { span: imp.span, ..d });
                    continue;
                }
            };
            let Some(j) = files.iter().position(|f| f.name == target) else {
                if target.starts_with("tj:") {
                    errors.push(Diagnostic::new(format!("no plugin {target}"), imp.span).hint("tinyjump ships tj:ai; run your app with tj, which adds plugin modules to it"));
                    continue;
                }
                let hint = match closest(&target, files.iter().map(|f| f.name.as_str())) {
                    Some(c) => format!("did you mean \"{}\"?", relative(&files[i].name, c)),
                    None => "check the path; it is relative to this file".into(),
                };
                errors.push(Diagnostic::new(format!("no file {target} in this app"), imp.span).hint(hint));
                continue;
            };
            deps[i].push(j);
            for (name, span) in &imp.names {
                if !exports[j].contains(name) {
                    let declared = programs[j].items.iter().flat_map(item_names).any(|(n, _)| n == *name);
                    let hint = if declared {
                        format!("add `export` before its declaration in {target}")
                    } else {
                        match closest(name, exports[j].iter().map(String::as_str)) {
                            Some(c) => format!("did you mean '{c}'?"),
                            None => format!("{target} exports: {}", sorted(&exports[j])),
                        }
                    };
                    errors.push(Diagnostic::new(format!("{target} does not export '{name}'"), *span).hint(hint));
                }
                imports[i].insert(name.clone());
            }
        }
    }
    let mut order = Vec::new();
    let mut seen = vec![false; programs.len()];
    for start in 0..programs.len() {
        visit(start, &deps, &mut seen, &mut order);
    }
    Linked { order, imports, exports }
}

fn sorted(set: &HashSet<String>) -> String {
    let mut v: Vec<&str> = set.iter().map(String::as_str).collect();
    v.sort_unstable();
    if v.is_empty() {
        "nothing".into()
    } else {
        v.join(", ")
    }
}

/// Depth first, dependencies before dependents; import cycles are allowed.
fn visit(i: usize, deps: &[Vec<usize>], seen: &mut [bool], order: &mut Vec<usize>) {
    if seen[i] {
        return;
    }
    seen[i] = true;
    for &d in &deps[i] {
        visit(d, deps, seen, order);
    }
    order.push(i);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundles_split_back_into_their_files() {
        let one = "function App() {}\n";
        assert_eq!(bundle(&[("app.tj".into(), one.into())]), one, "a one-file app stays as it is");
        let b = bundle(&[("app.tj".into(), "a\n".into()), ("lib/b.tj".into(), "b".into())]);
        let f = files(&b);
        assert_eq!(f.iter().map(|f| (f.name.as_str(), &b[f.start..f.end])).collect::<Vec<_>>(), [("app.tj", "a\n"), ("lib/b.tj", "b\n")]);
        let at = b.rfind("b\n").unwrap();
        assert_eq!(locate(&b, at), (Some("lib/b.tj".to_string()), 1, 1));
        assert_eq!(locate(one, 9), (None, 1, 10));
    }

    #[test]
    fn import_paths_resolve_relative_to_the_importer() {
        assert_eq!(resolve("app.tj", "./cart").unwrap(), "cart.tj");
        assert_eq!(resolve("lib/a.tj", "../cart").unwrap(), "cart.tj");
        assert_eq!(resolve("lib/a.tj", "./b").unwrap(), "lib/b.tj");
        assert!(resolve("app.tj", "lodash").unwrap_err().hint.unwrap().contains("relative"));
        assert!(resolve("app.tj", "./cart.tj").unwrap_err().hint.unwrap().contains("./cart"));
        assert!(resolve("app.tj", "../x").is_err());
        assert_eq!(relative("app.tj", "lib/b.tj"), "./lib/b");
        assert_eq!(relative("lib/a.tj", "cart.tj"), "../cart");
        assert_eq!(resolve("lib/a.tj", "tj:ai").unwrap(), "tj:ai", "plugins are named, not paths");
        assert!(resolve("app.tj", "tj:AI").is_err());
    }
}

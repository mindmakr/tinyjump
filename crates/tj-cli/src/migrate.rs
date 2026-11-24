//! `tj migrate`: CLI helper to port TypeScript / React apps to tinyjump `.tj`
//! and verify behaviour / UI parity (`compare`) against the original app.
//!
//! Strictly CLI-only: 0 bytes added to `tj-core` or device builds.

use crate::engine::{is_public, PRIVATE_FETCH_ENV};
use crate::golden::{compare as compare_png, decode_png, encode_png, DEFAULT_THRESHOLD_PCT};
use crate::{app, review, test_runner};
use serde_json::{json, Value as Json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tj_core::ui::layout::Measure;
use tj_core::{Input, MemStore, Options, Output, Runtime};

const MAX_FILES: usize = 200;
const MAX_FILE_BYTES: usize = 2 << 20;
const MAX_SNAP_BYTES: usize = 4 << 20;
const MAX_URL_BYTES: usize = 2 << 20;
const SKIP_DIRS: &[&str] = &["node_modules", ".git", "dist", "build", ".next", ".turbo", "coverage", ".tj-data"];

pub fn run(args: &[String]) -> Result<(), String> {
    if args.first().map(String::as_str) == Some("compare") {
        return run_compare(&args[1..]);
    }
    run_translate(args)
}

#[derive(Debug, Clone)]
struct MigrationDiagnostic {
    file: String,
    line: usize,
    severity: &'static str, // "manual" | "warn" | "info"
    kind: &'static str,
    message: String,
    hint: String,
}

impl MigrationDiagnostic {
    fn json(&self) -> Json {
        json!({
            "file": self.file,
            "line": self.line,
            "severity": self.severity,
            "kind": self.kind,
            "message": self.message,
            "hint": self.hint,
        })
    }
}

struct FileResult {
    input: String,
    output: String,
    code: String,
    diagnostics: Vec<MigrationDiagnostic>,
    compiles: Option<bool>,
    compile_errors: Vec<String>,
}

fn run_translate(args: &[String]) -> Result<(), String> {
    let usage = "usage:\n  tj migrate <input.tsx|ts|dir> [--out <path>] [--report <file.md>] [--strict] [--json]\n  tj migrate compare <migrated.tj> [--trace <file.trace.jsonl>]... [--old-snapshot <file.json>]\n    [--old-url <http://...>] [--old-screenshot <orig.png> --new-screenshot <migrated.png>]\n    [--threshold <pct>] [--diff <out.png>] [--width <n>] [--height <n>] [--json]";
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut report_path: Option<PathBuf> = None;
    let mut strict = false;
    let mut as_json = false;

    let mut it = args.iter().peekable();
    if let Some(first) = it.next_if(|a| !a.starts_with("--")) {
        input = Some(PathBuf::from(first));
    }
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--out" => out = Some(PathBuf::from(value()?)),
            "--report" => report_path = Some(PathBuf::from(value()?)),
            "--strict" => strict = true,
            "--json" => as_json = true,
            "--help" | "-h" => {
                println!("{usage}");
                return Ok(());
            }
            other if !other.starts_with("--") && input.is_none() => input = Some(PathBuf::from(other)),
            other => return Err(format!("unknown option {other}\n\n{usage}")),
        }
    }
    let input = input.ok_or_else(|| format!("`tj migrate` needs an input file or directory\n\n{usage}"))?;
    let outcome = migrate_path(&input, out.as_deref())?;

    if let Some(ref rpath) = report_path {
        if let Some(p) = rpath.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
        }
        fs::write(rpath, &outcome.markdown).map_err(|e| format!("cannot write {}: {e}", rpath.display()))?;
    }

    if as_json {
        println!("{}", serde_json::to_string_pretty(&outcome.json).expect("json"));
    } else {
        print!("{}", outcome.summary);
    }

    let has_manual = outcome.manual_count > 0;
    if strict && (has_manual || !outcome.all_compile) {
        return Err(format!(
            "--strict migration check failed ({} manual item(s), compiles: {})",
            outcome.manual_count, outcome.all_compile
        ));
    }
    Ok(())
}

struct MigrateOutcome {
    json: Json,
    markdown: String,
    summary: String,
    manual_count: usize,
    all_compile: bool,
}

fn migrate_path(input: &Path, out: Option<&Path>) -> Result<MigrateOutcome, String> {
    if !input.exists() {
        return Err(format!("input path {} does not exist", input.display()));
    }
    let is_dir = input.is_dir();
    let mut results: Vec<FileResult> = Vec::new();
    let out_display: PathBuf;

    if is_dir {
        let in_canon = fs::canonicalize(input).map_err(|e| format!("cannot resolve {}: {e}", input.display()))?;
        let dest_dir = out
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("{}-tj", input.display())));
        if let Ok(dest_canon) = fs::canonicalize(&dest_dir) {
            if dest_canon == in_canon || dest_canon.starts_with(&in_canon) {
                return Err(format!(
                    "refusing to write migrated directory {} inside/over input {}",
                    dest_dir.display(),
                    input.display()
                ));
            }
        }
        let files = collect_source_files(&in_canon)?;
        if files.is_empty() {
            return Err(format!("no .ts/.tsx/.js/.jsx files found under {}", input.display()));
        }
        fs::create_dir_all(&dest_dir).map_err(|e| format!("cannot create {}: {e}", dest_dir.display()))?;
        for f in &files {
            let rel = f.strip_prefix(&in_canon).unwrap_or(f.as_path());
            let mut out_rel = rel.to_path_buf();
            out_rel.set_extension("tj");
            let dest_file = dest_dir.join(&out_rel);
            if let Some(p) = dest_file.parent() {
                fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
            }
            let raw = read_bounded_utf8(f, MAX_FILE_BYTES)?;
            let disp = rel.display().to_string().replace('\\', "/");
            let (code, diagnostics) = translate_source(&raw, &disp);
            fs::write(&dest_file, &code).map_err(|e| format!("cannot write {}: {e}", dest_file.display()))?;
            results.push(FileResult {
                input: disp,
                output: dest_file.display().to_string(),
                code,
                diagnostics,
                compiles: None,
                compile_errors: Vec::new(),
            });
        }
        // Check entry file compilation via app::read (resolving multi-file imports).
        let entry_idx = pick_entry_index(&results);
        if let Some(idx) = entry_idx {
            let entry_path = PathBuf::from(&results[idx].output);
            match app::read(&entry_path).and_then(|loaded| {
                tj_core::compile(&loaded.source).map_err(|errs| {
                    errs.iter()
                        .map(|d| d.render(&results[idx].output, &loaded.source))
                        .collect::<Vec<_>>()
                        .join("\n\n")
                })
            }) {
                Ok(_) => results[idx].compiles = Some(true),
                Err(e) => {
                    results[idx].compiles = Some(false);
                    results[idx].compile_errors.push(e);
                }
            }
        }
        out_display = dest_dir;
    } else {
        if input.extension().and_then(|e| e.to_str()) == Some("tj") {
            return Err(format!("{} is already a .tj file", input.display()));
        }
        let dest_file = out
            .map(PathBuf::from)
            .unwrap_or_else(|| input.with_extension("tj"));
        if paths_match(input, &dest_file) {
            return Err(format!("refusing to overwrite input file {}", input.display()));
        }
        if let Some(p) = dest_file.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
        }
        let raw = read_bounded_utf8(input, MAX_FILE_BYTES)?;
        let disp = input
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("app.tsx")
            .to_string();
        let (code, diagnostics) = translate_source(&raw, &disp);
        fs::write(&dest_file, &code).map_err(|e| format!("cannot write {}: {e}", dest_file.display()))?;
        let (compiles, compile_errors) = match app::read(&dest_file) {
            Ok(loaded) => match tj_core::compile(&loaded.source) {
                Ok(_) => (Some(true), Vec::new()),
                Err(errs) => (
                    Some(false),
                    errs.iter()
                        .map(|d| d.render(&dest_file.display().to_string(), &loaded.source))
                        .collect(),
                ),
            },
            Err(e) => (Some(false), vec![e]),
        };
        results.push(FileResult {
            input: disp,
            output: dest_file.display().to_string(),
            code,
            diagnostics,
            compiles,
            compile_errors,
        });
        out_display = dest_file;
    }

    let mut manual_count = 0usize;
    let mut warn_count = 0usize;
    let mut info_count = 0usize;
    let mut all_compile = true;
    for r in &results {
        if r.compiles == Some(false) {
            all_compile = false;
        }
        for d in &r.diagnostics {
            match d.severity {
                "manual" => manual_count += 1,
                "warn" => warn_count += 1,
                _ => info_count += 1,
            }
        }
    }

    let md = format_markdown(&input.display().to_string(), &out_display.display().to_string(), &results);
    let mut summary = format!(
        "migrated {} -> {} ({} file(s), {} manual, {} warn, {} info, compiles: {})\n",
        input.display(),
        out_display.display(),
        results.len(),
        manual_count,
        warn_count,
        info_count,
        all_compile
    );
    for r in &results {
        for d in &r.diagnostics {
            let _ = writeln!(summary, "  [{}:{}] {}: {} -> {}", d.severity, d.file, d.line, d.message, d.hint);
        }
        for e in &r.compile_errors {
            let _ = writeln!(summary, "  [compile] {e}");
        }
    }

    let files_json: Vec<Json> = results
        .iter()
        .map(|r| {
            json!({
                "input": r.input,
                "output": r.output,
                "lines": r.code.lines().count(),
                "compiles": r.compiles,
                "compileErrors": r.compile_errors,
                "diagnostics": r.diagnostics.iter().map(MigrationDiagnostic::json).collect::<Vec<_>>(),
            })
        })
        .collect();
    let json = json!({
        "input": input.display().to_string(),
        "output": out_display.display().to_string(),
        "fileCount": results.len(),
        "manualCount": manual_count,
        "warnCount": warn_count,
        "infoCount": info_count,
        "compiles": all_compile,
        "files": files_json,
    });

    Ok(MigrateOutcome {
        json,
        markdown: md,
        summary,
        manual_count,
        all_compile,
    })
}

fn pick_entry_index(results: &[FileResult]) -> Option<usize> {
    if let Some(pos) = results.iter().position(|r| r.code.contains("function App(")) {
        return Some(pos);
    }
    for candidate in ["src/app.tsx", "src/App.tsx", "app.tsx", "App.tsx", "src/index.tsx", "index.tsx"] {
        if let Some(pos) = results.iter().position(|r| r.input.eq_ignore_ascii_case(candidate)) {
            return Some(pos);
        }
    }
    (!results.is_empty()).then_some(0)
}

fn format_markdown(input: &str, output: &str, results: &[FileResult]) -> String {
    let mut md = format!("# Migration Report: `{input}` -> `{output}`\n\n");
    for r in results {
        let status = match r.compiles {
            Some(true) => "compiles cleanly",
            Some(false) => "compile errors remain",
            None => "module",
        };
        let _ = writeln!(md, "## `{}` -> `{}` ({status})\n", r.input, r.output);
        if r.diagnostics.is_empty() && r.compile_errors.is_empty() {
            md.push_str("- Clean translation with zero warnings or manual steps.\n\n");
            continue;
        }
        for d in &r.diagnostics {
            let _ = writeln!(
                md,
                "- **[{}] line {} (`{}`):** {}\n  - *Fix hint:* {}",
                d.severity, d.line, d.kind, d.message, d.hint
            );
        }
        for e in &r.compile_errors {
            let _ = writeln!(md, "- **[compile error]:** `{}`", e.replace('\n', " | "));
        }
        md.push('\n');
    }
    md
}

fn collect_source_files(root_canon: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let mut stack = vec![root_canon.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries: Vec<_> = fs::read_dir(&dir)
            .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
            .filter_map(Result::ok)
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for ent in entries {
            let p = ent.path();
            let name = ent.file_name();
            let name_str = name.to_string_lossy();
            let Ok(meta) = fs::symlink_metadata(&p) else { continue };
            if meta.file_type().is_symlink() {
                let Ok(target) = fs::canonicalize(&p) else { continue };
                if !target.starts_with(root_canon) {
                    continue;
                }
            }
            if meta.is_dir() {
                if !SKIP_DIRS.contains(&name_str.as_ref()) {
                    stack.push(p);
                }
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            if name_str.ends_with(".d.ts")
                || name_str.contains(".test.")
                || name_str.contains(".spec.")
            {
                continue;
            }
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if matches!(ext, "ts" | "tsx" | "js" | "jsx") {
                out.push(p);
                if out.len() > MAX_FILES {
                    return Err(format!("exceeded limit of {MAX_FILES} source files under {}", root_canon.display()));
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

fn paths_match(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

fn read_bounded_utf8(path: &Path, limit: usize) -> Result<String, String> {
    let mut f = fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    f.by_ref()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if bytes.len() > limit {
        return Err(format!("{} exceeds {} MiB limit", path.display(), limit >> 20));
    }
    let s = String::from_utf8(bytes).map_err(|_| format!("{} is not valid UTF-8", path.display()))?;
    Ok(s.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(s))
}

/// Computes a per-byte mask (`true` where byte `i` is regular TypeScript/JSX code,
/// `false` inside strings, template literal prose, or `//` / `/* */` comments).
fn code_mask(src: &str) -> Vec<bool> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        Code,
        LineComment,
        BlockComment,
        SingleQuote,
        DoubleQuote,
        TemplateText,
    }
    let b = src.as_bytes();
    let mut mask = vec![false; b.len()];
    let mut state = State::Code;
    let mut tpl_braces: Vec<usize> = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        match state {
            State::Code => {
                if i + 1 < b.len() && b[i] == b'/' && b[i + 1] == b'/' {
                    state = State::LineComment;
                    i += 2;
                    continue;
                }
                if i + 1 < b.len() && b[i] == b'/' && b[i + 1] == b'*' {
                    state = State::BlockComment;
                    i += 2;
                    continue;
                }
                if b[i] == b'\'' {
                    state = State::SingleQuote;
                    i += 1;
                    continue;
                }
                if b[i] == b'"' {
                    state = State::DoubleQuote;
                    i += 1;
                    continue;
                }
                if b[i] == b'`' {
                    tpl_braces.push(0);
                    state = State::TemplateText;
                    i += 1;
                    continue;
                }
                if let Some(depth) = tpl_braces.last_mut() {
                    if b[i] == b'{' {
                        *depth += 1;
                    } else if b[i] == b'}' {
                        *depth -= 1;
                        if *depth == 0 {
                            state = State::TemplateText;
                            i += 1;
                            continue;
                        }
                    }
                }
                mask[i] = true;
                i += 1;
            }
            State::LineComment => {
                if b[i] == b'\n' {
                    state = State::Code;
                }
                i += 1;
            }
            State::BlockComment => {
                if i + 1 < b.len() && b[i] == b'*' && b[i + 1] == b'/' {
                    state = State::Code;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            State::SingleQuote => {
                if b[i] == b'\\' && i + 1 < b.len() {
                    i += 2;
                } else {
                    if b[i] == b'\'' {
                        state = State::Code;
                    }
                    i += 1;
                }
            }
            State::DoubleQuote => {
                if b[i] == b'\\' && i + 1 < b.len() {
                    i += 2;
                } else {
                    if b[i] == b'"' {
                        state = State::Code;
                    }
                    i += 1;
                }
            }
            State::TemplateText => {
                if b[i] == b'\\' && i + 1 < b.len() {
                    i += 2;
                } else if i + 1 < b.len() && b[i] == b'$' && b[i + 1] == b'{' {
                    if let Some(depth) = tpl_braces.last_mut() {
                        *depth = 1;
                    }
                    state = State::Code;
                    i += 2;
                } else if b[i] == b'`' {
                    tpl_braces.pop();
                    state = State::Code;
                    i += 1;
                } else {
                    i += 1;
                }
            }
        }
    }
    mask
}

fn translate_source(raw: &str, file_label: &str) -> (String, Vec<MigrationDiagnostic>) {
    let mut diags = Vec::new();
    let normalized = raw.strip_prefix('\u{feff}').unwrap_or(raw).replace("\r\n", "\n");
    let pass1 = translate_imports_and_exports(&normalized, file_label, &mut diags);
    detect_unsupported_patterns(&pass1, file_label, &mut diags);
    let pass2 = rewrite_ts_dialect(&pass1);
    let pass3 = lift_react_state(&pass2);
    let pass4 = translate_jsx(&pass3, file_label, &mut diags);
    let cleaned = trim_excess_blank_lines(&pass4);
    (cleaned, diags)
}

fn strip_js_ext(spec: &str) -> &str {
    for ext in [".tsx", ".ts", ".jsx", ".js"] {
        if let Some(s) = spec.strip_suffix(ext) {
            return s;
        }
    }
    spec
}

fn translate_imports_and_exports(src: &str, file: &str, diags: &mut Vec<MigrationDiagnostic>) -> String {
    let mut out = String::with_capacity(src.len());
    for (idx, line) in src.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = line.trim();
        let indent = &line[..line.len() - line.trim_start().len()];

        // Side-effect import (`import "./App.css";`)
        if trimmed.starts_with("import ") && !trimmed.contains(" from ") {
            if let Some(spec) = extract_quoted_spec(trimmed) {
                if spec.ends_with(".css") || spec.ends_with(".scss") || spec.ends_with(".less") || spec.ends_with(".sass") {
                    diags.push(MigrationDiagnostic {
                        file: file.into(),
                        line: line_no,
                        severity: "info",
                        kind: "import-css",
                        message: format!("Dropped stylesheet import `{spec}`"),
                        hint: "Style TinyJump components directly via layout/style props (`padding`, `gap`, `background`, `color`, `radius`, `size`).".into(),
                    });
                    continue;
                }
                diags.push(MigrationDiagnostic {
                    file: file.into(),
                    line: line_no,
                    severity: "manual",
                    kind: "import",
                    message: format!("Unsupported side-effect import `{spec}`"),
                    hint: "Remove side-effect imports or move initialization into `.tj` object defaults.".into(),
                });
                let _ = writeln!(out, "{indent}// TODO(tj-migrate): {trimmed}");
                continue;
            }
        }

        if let Some(rest) = trimmed.strip_prefix("import ") {
            if let Some((lhs_raw, spec)) = split_import_from(rest) {
                let is_react = matches!(spec, "react" | "react-dom" | "react-dom/client" | "react/jsx-runtime");
                if is_react {
                    continue;
                }
                if spec.starts_with("./") || spec.starts_with("../") {
                    let clean_spec = strip_js_ext(spec);
                    let lhs = lhs_raw.strip_prefix("type ").map(str::trim).unwrap_or(lhs_raw);
                    if lhs.starts_with('{') && lhs.ends_with('}') {
                        if lhs.contains(" as ") {
                            diags.push(MigrationDiagnostic {
                                file: file.into(),
                                line: line_no,
                                severity: "manual",
                                kind: "import-alias",
                                message: format!("Import aliasing (`as`) in `{trimmed}` is unsupported"),
                                hint: "Use the original exported identifier directly without `as` renaming.".into(),
                            });
                            let _ = writeln!(out, "{indent}// TODO(tj-migrate): {trimmed}");
                        } else {
                            let _ = writeln!(out, "{indent}import {lhs} from \"{clean_spec}\"");
                        }
                    } else {
                        diags.push(MigrationDiagnostic {
                            file: file.into(),
                            line: line_no,
                            severity: "manual",
                            kind: "import-default",
                            message: format!("Default or namespace import `{trimmed}` is unsupported"),
                            hint: format!("Convert default export in `{clean_spec}` to `export ...` and write `import {{ Name }} from \"{clean_spec}\"`."),
                        });
                        let _ = writeln!(out, "{indent}// TODO(tj-migrate): {trimmed}");
                    }
                    continue;
                }
                diags.push(MigrationDiagnostic {
                    file: file.into(),
                    line: line_no,
                    severity: "manual",
                    kind: "import-pkg",
                    message: format!("External package `{spec}` cannot be imported automatically"),
                    hint: format!("Replace `{spec}` with TinyJump built-ins (`fetch`, `Time`, `crypto`, `<Icon>`), or run `tj search {spec}` for a verified `tj:` module."),
                });
                let _ = writeln!(out, "{indent}// TODO(tj-migrate): {trimmed}");
                continue;
            }
        }

        if let Some(after) = trimmed.strip_prefix("export default async function ") {
            let name = ident_at(after).unwrap_or("App");
            let prefix = if name == "App" { "async function " } else { "export async function " };
            let _ = writeln!(out, "{indent}{prefix}{after}");
            continue;
        }
        if let Some(after) = trimmed.strip_prefix("export default function ") {
            if after.starts_with('(') {
                let _ = writeln!(out, "{indent}function App{after}");
            } else {
                let name = ident_at(after).unwrap_or("App");
                let prefix = if name == "App" { "function " } else { "export function " };
                let _ = writeln!(out, "{indent}{prefix}{after}");
            }
            continue;
        }
        if trimmed.starts_with("export default ") {
            diags.push(MigrationDiagnostic {
                file: file.into(),
                line: line_no,
                severity: "info",
                kind: "export-default",
                message: format!("Removed `{trimmed}`"),
                hint: "Put `export` directly before declarations, and keep `function App()` in the entry file.".into(),
            });
            continue;
        }
        if trimmed.starts_with("export {") {
            diags.push(MigrationDiagnostic {
                file: file.into(),
                line: line_no,
                severity: "manual",
                kind: "export-list",
                message: format!("Unsupported export list `{trimmed}`"),
                hint: "Move `export` onto each `function`, `object`, `type`, or `const` declaration.".into(),
            });
            let _ = writeln!(out, "{indent}// TODO(tj-migrate): {trimmed}");
            continue;
        }
        let _ = writeln!(out, "{line}");
    }
    out
}

fn split_import_from(rest: &str) -> Option<(&str, &str)> {
    let trimmed = rest.trim().trim_end_matches(';').trim();
    let idx = trimmed.rfind(" from ")?;
    let lhs = trimmed[..idx].trim();
    let rhs = trimmed[idx + 6..].trim();
    let spec = extract_quoted_spec(rhs)?;
    Some((lhs, spec))
}

fn extract_quoted_spec(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    for (i, &ch) in bytes.iter().enumerate() {
        if ch == b'"' || ch == b'\'' {
            if let Some(rel) = s[i + 1..].find(ch as char) {
                return Some(&s[i + 1..i + 1 + rel]);
            }
        }
    }
    None
}

fn detect_unsupported_patterns(src: &str, file: &str, diags: &mut Vec<MigrationDiagnostic>) {
    let mask = code_mask(src);
    let lines: Vec<&str> = src.lines().collect();
    let mut offset = 0usize;
    for (idx, line) in lines.iter().enumerate() {
        let line_no = idx + 1;
        let line_start = offset;
        let line_end = line_start + line.len();
        offset = (line_end + 1).min(mask.len());

        let has_kw = |kw: &str| -> bool { find_code_keyword(src, &mask, line_start, line_end, kw).is_some() };

        if has_kw("useEffect") {
            let look_end = (idx + 14).min(lines.len());
            let snippet = lines[idx..look_end].join("\n");
            let hint = if snippet.contains("setInterval") {
                "Replace polling `useEffect`/`setInterval` with an `@every(\"1s\")` method on a durable `object`."
            } else if snippet.contains("setTimeout") {
                "Replace `setTimeout` inside `useEffect` with durable alarms (`this.schedule(ms, \"method\")`)."
            } else if snippet.contains("fetch(") {
                "Move network fetches from `useEffect` into `@server async load()` object methods so state replicates cleanly."
            } else {
                "Replace lifecycle `useEffect` hooks with `@every(...)` timers, `this.schedule(...)`, or object methods."
            };
            diags.push(MigrationDiagnostic {
                file: file.into(),
                line: line_no,
                severity: "manual",
                kind: "hook-useEffect",
                message: "`useEffect` hook has no direct equivalent in TinyJump UI rendering".into(),
                hint: hint.into(),
            });
        }
        for (hook, hint) in [
            ("useReducer", "Model reducer state and dispatch actions as typed fields + methods on one `object`."),
            ("useRef", "Store persistent non-rendered handles as `object` fields or use `id` props on UI controls."),
            ("useContext", "Share state directly via `MyState.get(\"main\")` objects instead of React Context."),
            ("useMemo", "Compute derived values inline or in small helper functions."),
            ("useCallback", "Pass arrow functions directly (`() => obj.method()`) without `useCallback`."),
        ] {
            if has_kw(hook) {
                diags.push(MigrationDiagnostic {
                    file: file.into(),
                    line: line_no,
                    severity: "manual",
                    kind: "hook",
                    message: format!("React hook `{hook}` requires manual conversion"),
                    hint: hint.into(),
                });
            }
        }
        for (pat, kind, msg, hint) in [
            ("localStorage", "web-storage", "`localStorage` is not used in TinyJump", "Fields on `object` declarations persist automatically across restarts on every platform."),
            ("sessionStorage", "web-storage", "`sessionStorage` is not used in TinyJump", "Fields on local `object` declarations persist per device cleanly."),
            ("window.", "dom-global", "Browser global `window.*` is unavailable cross-platform", "Read screen inputs via event callbacks and call server methods directly."),
            ("document.", "dom-global", "DOM API `document.*` is unavailable in native iOS/Android/desktop", "Declare views in JSX or wrap browser-only widgets with `<Native name=\"...\" />`."),
            ("switch", "syntax-switch", "`switch` is not supported in TinyJump", "Rewrite branches as `if (...) { ... } else if (...) { ... }`."),
            ("enum", "syntax-enum", "`enum` is not supported in TinyJump", "Use string literal union types: `type Status = \"open\" | \"closed\"`."),
            ("class", "syntax-class", "`class` declarations are not supported in TinyJump", "Use durable `object Name { ... }` blocks with methods instead."),
            ("finally", "syntax-finally", "`try { ... } finally { ... }` is not supported", "Run cleanup after the `try / catch` block or inside a helper."),
        ] {
            if has_kw(pat) {
                // Only flag `switch (` or `window.`/`document.` when followed by `.` / `(` as expected
                let valid = match pat {
                    "switch" => line.contains("switch (") || line.contains("switch("),
                    _ => true,
                };
                if valid {
                    diags.push(MigrationDiagnostic {
                        file: file.into(),
                        line: line_no,
                        severity: "manual",
                        kind,
                        message: msg.into(),
                        hint: hint.into(),
                    });
                }
            }
        }
        if has_kw("new") {
            for (ctor, hint) in [
                ("Date", "Store timestamps as UTC millisecond numbers (`Date.now()`) and format UI via `<Time>` / `Time.*`."),
                ("Map", "Replace `new Map()` with `Record<string, T>` or `{}` object maps."),
                ("Set", "Replace `new Set()` with deduplicated arrays (`arr.includes(x)`) or `Record<string, boolean>`."),
            ] {
                if line.contains(&format!("new {ctor}")) {
                    diags.push(MigrationDiagnostic {
                        file: file.into(),
                        line: line_no,
                        severity: "manual",
                        kind: "builtin-new",
                        message: format!("`new {ctor}` is not supported (`new` only constructs `Error`)"),
                        hint: hint.into(),
                    });
                }
            }
        }
    }
}

fn find_code_keyword(src: &str, mask: &[bool], start: usize, end: usize, kw: &str) -> Option<usize> {
    let bytes = src.as_bytes();
    let klen = kw.len();
    if end <= start || klen == 0 || end - start < klen {
        return None;
    }
    let check_end = !kw.ends_with('.');
    let mut i = start;
    while i + klen <= end {
        if mask[i] && src.is_char_boundary(i) && bytes[i..].starts_with(kw.as_bytes()) {
            let prev_ok = i == 0 || !is_ident_char(bytes[i - 1]);
            let after_idx = i + klen;
            let next_ok = !check_end || after_idx >= b_len(src) || !is_ident_char(bytes[after_idx]);
            if prev_ok && next_ok && (i == 0 || bytes[i - 1] != b'.') {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn b_len(s: &str) -> usize {
    s.len()
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn ident_at(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    if bytes.is_empty() || !(bytes[0].is_ascii_alphabetic() || bytes[0] == b'_' || bytes[0] == b'$') {
        return None;
    }
    let mut end = 1;
    while end < bytes.len() && is_ident_char(bytes[end]) {
        end += 1;
    }
    Some(&s[..end])
}

fn rewrite_ts_dialect(src: &str) -> String {
    let strip_annots = [
        ": JSX.Element",
        ": React.ReactNode",
        ": React.ReactElement",
        ": React.FC",
    ];
    let mut pre = src.to_string();
    for pat in strip_annots {
        pre = pre.replace(pat, "");
    }
    // Simplify common React event param type annotations like `(e: React.ChangeEvent<HTMLInputElement>)`.
    pre = strip_react_event_types(&pre);

    let mask = code_mask(&pre);
    let b = pre.as_bytes();
    let mut out = String::with_capacity(pre.len() + 16);
    let mut i = 0usize;
    while i < b.len() {
        if !mask[i] {
            let ch = pre[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        if pre[i..].starts_with("===") || pre[i..].starts_with("!==") {
            out.push_str(&pre[i..i + 3]);
            i += 3;
            continue;
        }
        if pre[i..].starts_with("==") {
            out.push_str("===");
            i += 2;
            continue;
        }
        if pre[i..].starts_with("!=") {
            out.push_str("!==");
            i += 2;
            continue;
        }
        let prev_ok = i == 0 || !is_ident_char(b[i - 1]);
        if prev_ok && pre[i..].starts_with("undefined") {
            let end = i + 9;
            if (end == b.len() || !is_ident_char(b[end])) && (i == 0 || b[i - 1] != b'.') {
                out.push_str("null");
                i = end;
                continue;
            }
        }
        if prev_ok && pre[i..].starts_with("var") {
            let end = i + 3;
            if end < b.len() && matches!(b[end], b' ' | b'\t') {
                out.push_str("let");
                i = end;
                continue;
            }
        }
        let ch = pre[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn strip_react_event_types(src: &str) -> String {
    let mut out = src.to_string();
    for marker in [": React.ChangeEvent<", ": React.FormEvent<", ": React.MouseEvent<", ": ChangeEvent<", ": FormEvent<", ": MouseEvent<"] {
        while let Some(pos) = out.find(marker) {
            let after_lt = pos + marker.len();
            let mut depth = 1usize;
            let bytes = out.as_bytes();
            let mut j = after_lt;
            while j < bytes.len() && depth > 0 {
                match bytes[j] {
                    b'<' => depth += 1,
                    b'>' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            if depth == 0 {
                out.replace_range(pos..j, "");
            } else {
                break;
            }
        }
    }
    out
}

struct HookVar {
    var_name: String,
    setter_name: String,
    ty: String,
    init_expr: String,
    decl_line_range: (usize, usize), // relative offsets inside fn body [start..end)
}

fn lift_react_state(src: &str) -> String {
    // Walk top-level functions cleanly and lift `useState` hooks inside React components.
    let mask = code_mask(src);
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len() + 256);
    let mut cursor = 0usize;
    let mut i = 0usize;

    while i < b.len() {
        let prev_ok = i == 0 || !is_ident_char(b[i - 1]);
        if mask[i] && src.is_char_boundary(i) && prev_ok && b[i..].starts_with(b"function ") {
            // Find function name
            let name_start = i + 9;
            if let Some(fn_name) = ident_at(src[name_start..].trim_start()) {
                let first_ch = fn_name.chars().next().unwrap_or('a');
                if first_ch.is_ascii_uppercase() {
                    if let Some(body_open) = find_matching_body_open(src, &mask, name_start) {
                        if let Some(body_close) = find_matching_brace(src, &mask, body_open) {
                            let fn_line_start = src[..i].rfind('\n').map_or(0, |p| p + 1);
                            out.push_str(&src[cursor..fn_line_start]);
                            let header = &src[fn_line_start..=body_open];
                            let body = &src[body_open + 1..body_close];
                            let rewritten = lift_component_block(fn_name, header, body);
                            out.push_str(&rewritten);
                            out.push('}');
                            cursor = body_close + 1;
                            i = cursor;
                            continue;
                        }
                    }
                }
            }
        }
        i += 1;
    }
    out.push_str(&src[cursor..]);
    out
}

fn find_matching_body_open(src: &str, mask: &[bool], start: usize) -> Option<usize> {
    let b = src.as_bytes();
    let mut paren = 0usize;
    let mut saw_paren = false;
    let mut i = start;
    while i < b.len() {
        if !mask[i] {
            i += 1;
            continue;
        }
        match b[i] {
            b'(' => {
                paren += 1;
                saw_paren = true;
            }
            b')' => paren = paren.saturating_sub(1),
            b'{' if saw_paren && paren == 0 => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

fn find_matching_brace(src: &str, mask: &[bool], open_idx: usize) -> Option<usize> {
    let b = src.as_bytes();
    let mut depth = 0usize;
    let mut i = open_idx;
    while i < b.len() {
        if mask[i] {
            if b[i] == b'{' {
                depth += 1;
            } else if b[i] == b'}' {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

fn find_matching_delim(src: &str, mask: &[bool], open_idx: usize, open_ch: u8, close_ch: u8) -> Option<usize> {
    let b = src.as_bytes();
    let mut depth = 0usize;
    let mut i = open_idx;
    while i < b.len() {
        if mask[i] {
            if b[i] == open_ch {
                depth += 1;
            } else if b[i] == close_ch {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

fn parse_use_state_line(line: &str) -> Option<(String, String, String, String)> {
    let t = line.trim();
    let rest = t.strip_prefix("const ").or_else(|| t.strip_prefix("let "))?.trim_start();
    if !rest.starts_with('[') {
        return None;
    }
    let close_br = rest.find(']')?;
    let pair = &rest[1..close_br];
    let (v_raw, s_raw) = pair.split_once(',')?;
    let var_name = ident_at(v_raw.trim())?.to_string();
    let setter_name = ident_at(s_raw.trim())?.to_string();
    let after_eq = rest[close_br + 1..].trim_start().strip_prefix('=')?.trim_start();
    let after_us = after_eq.strip_prefix("useState")?.trim_start();
    let (explicit_ty, after_gen) = if after_us.starts_with('<') {
        let mask = code_mask(after_us);
        let end_gt = find_matching_delim(after_us, &mask, 0, b'<', b'>')?;
        let ty = after_us[1..end_gt].trim().to_string();
        (Some(ty), after_us[end_gt + 1..].trim_start())
    } else {
        (None, after_us)
    };
    if !after_gen.starts_with('(') {
        return None;
    }
    let mask = code_mask(after_gen);
    let end_par = find_matching_delim(after_gen, &mask, 0, b'(', b')')?;
    let init = after_gen[1..end_par].trim();
    let init_expr = if init.is_empty() { "null".to_string() } else { init.to_string() };
    let ty = explicit_ty.unwrap_or_else(|| infer_ts_type(&init_expr));
    Some((var_name, setter_name, ty, init_expr))
}

fn infer_ts_type(expr: &str) -> String {
    let t = expr.trim();
    if (t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')) {
        return "string".into();
    }
    if t == "true" || t == "false" {
        return "boolean".into();
    }
    let num_candidate = t.strip_prefix('-').unwrap_or(t);
    if !num_candidate.is_empty() && num_candidate.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return "number".into();
    }
    if t == "[]" {
        return "any[]".into();
    }
    if t.starts_with('[') && t.ends_with(']') {
        let first = t[1..t.len() - 1].split(',').next().unwrap_or("").trim();
        let inner = infer_ts_type(first);
        return format!("{inner}[]");
    }
    "any".into()
}

fn lift_component_block(fn_name: &str, header: &str, body: &str) -> String {
    let mut hooks = Vec::new();
    let mut pos = 0usize;
    for line in body.split_inclusive('\n') {
        let next = pos + line.len();
        if let Some((var_name, setter_name, ty, init_expr)) = parse_use_state_line(line) {
            hooks.push(HookVar {
                var_name,
                setter_name,
                ty,
                init_expr,
                decl_line_range: (pos, next),
            });
        }
        pos = next;
    }
    if hooks.is_empty() {
        return format!("{header}{body}");
    }

    let obj_name = format!("{fn_name}State");
    let mut obj_def = format!("object {obj_name} {{\n");
    for h in &hooks {
        let _ = writeln!(obj_def, "  {}: {} = {}", h.var_name, h.ty, h.init_expr);
        let _ = writeln!(obj_def, "  {}(v: {}) {{ this.{} = v }}", h.setter_name, h.ty, h.var_name);
    }
    obj_def.push_str("}\n\n");

    // Strip the `useState` declaration lines.
    let mut clean_body = String::with_capacity(body.len());
    let mut c = 0usize;
    for h in &hooks {
        clean_body.push_str(&body[c..h.decl_line_range.0]);
        c = h.decl_line_range.1;
    }
    clean_body.push_str(&body[c..]);

    // Rewrite `setter(...)` calls, handling functional updaters `x => expr` cleanly.
    let mut after_setters = clean_body;
    for h in &hooks {
        after_setters = rewrite_setter_calls(&after_setters, &h.setter_name, &h.var_name);
    }

    // Rewrite free reads of lifted state variables -> `state.<varName>`.
    let hook_names: HashSet<String> = hooks.iter().map(|h| h.var_name.clone()).collect();
    let rewritten_body = rewrite_state_reads(&after_setters, &hook_names);

    format!("{obj_def}{header}\n  const state = {obj_name}.get(\"main\")\n{}", rewritten_body.trim_start_matches('\n'))
}

fn rewrite_setter_calls(src: &str, setter: &str, var_name: &str) -> String {
    let mask = code_mask(src);
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len() + 32);
    let mut cursor = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        let prev_ok = i == 0 || !is_ident_char(b[i - 1]);
        if mask[i] && src.is_char_boundary(i) && prev_ok && b[i..].starts_with(setter.as_bytes()) {
            let after_name = i + setter.len();
            let mut k = after_name;
            while k < b.len() && b[k] == b' ' {
                k += 1;
            }
            if k < b.len() && b[k] == b'(' && mask[k] && (i == 0 || b[i - 1] != b'.') {
                if let Some(close_p) = find_matching_delim(src, &mask, k, b'(', b')') {
                    out.push_str(&src[cursor..i]);
                    let arg = src[k + 1..close_p].trim();
                    let next_arg = if let Some((param, expr)) = parse_single_arrow_fn(arg) {
                        let repl = format!("state.{var_name}");
                        rewrite_free_ident(expr, &param, &repl)
                    } else {
                        arg.to_string()
                    };
                    let _ = write!(out, "state.{setter}({next_arg})");
                    cursor = close_p + 1;
                    i = cursor;
                    continue;
                }
            }
        }
        i += 1;
    }
    out.push_str(&src[cursor..]);
    out
}

fn parse_single_arrow_fn(s: &str) -> Option<(String, &str)> {
    let mask = code_mask(s);
    let b = s.as_bytes();
    let mut paren = 0usize;
    let mut i = 0usize;
    while i + 1 < b.len() {
        if mask[i] {
            if b[i] == b'(' {
                paren += 1;
            } else if b[i] == b')' {
                paren = paren.saturating_sub(1);
            } else if paren == 0 && b[i] == b'=' && b[i + 1] == b'>' {
                let lhs = s[..i].trim();
                let inner = lhs.strip_prefix('(').and_then(|x| x.strip_suffix(')')).unwrap_or(lhs).trim();
                let param_name = inner.split(':').next()?.trim();
                if ident_at(param_name) == Some(param_name) {
                    let rhs = s[i + 2..].trim();
                    return Some((param_name.to_string(), rhs));
                }
                return None;
            }
        }
        i += 1;
    }
    None
}

fn rewrite_free_ident(src: &str, ident: &str, replacement: &str) -> String {
    let mut set = HashSet::new();
    set.insert(ident.to_string());
    let mask = code_mask(src);
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len() + 16);
    let mut i = 0usize;
    while i < b.len() {
        if mask[i] && (i == 0 || !is_ident_char(b[i - 1])) {
            if let Some(tok) = ident_at(&src[i..]) {
                let end = i + tok.len();
                let after_dot = i > 0 && b[i - 1] == b'.';
                if !after_dot && set.contains(tok) {
                    out.push_str(replacement);
                    i = end;
                    continue;
                }
                out.push_str(tok);
                i = end;
                continue;
            }
        }
        let ch = src[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn rewrite_state_reads(src: &str, vars: &HashSet<String>) -> String {
    let mask = code_mask(src);
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len() + 64);
    let mut i = 0usize;
    while i < b.len() {
        if !mask[i] {
            let ch = src[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        if i == 0 || !is_ident_char(b[i - 1]) {
            if let Some(tok) = ident_at(&src[i..]) {
                let end = i + tok.len();
                if vars.contains(tok) {
                    let prev = prev_non_ws(src, i);
                    let next = next_non_ws(src, end);
                    let after_dot = prev == Some('.');
                    // JSX attribute names like `count={x}` have `=` (single) immediately after.
                    let is_jsx_attr = next == Some('=') && !src[end..].trim_start().starts_with("==") && !src[end..].trim_start().starts_with("=>");
                    // Arrow param shadowing like `count => ...` or `(count) => ...`
                    let is_arrow_param = src[end..].trim_start().starts_with("=>")
                        || (next == Some(')') && src[end..].find(')').is_some_and(|p| src[end + p + 1..].trim_start().starts_with("=>")));
                    // Object key literal `{ count: 1 }` vs JS shorthand `{ count }` (exclude JSX `{count}` / `attr={count}`).
                    let is_obj_key = matches!(prev, Some('{') | Some(',')) && next == Some(':');
                    let is_shorthand = if prev == Some(',') && matches!(next, Some(',') | Some('}')) {
                        true
                    } else if prev == Some('{') && next == Some('}') {
                        let brace_pos = src[..i].rfind('{').unwrap_or(0);
                        let close_pos = end + src[end..].find('}').unwrap_or(0);
                        let before = prev_non_ws(src, brace_pos);
                        let after_close = next_non_ws(src, close_pos + 1);
                        after_close != Some('<')
                            && ((ends_with_kw(&src[..brace_pos], &["return"])
                                && after_close.map_or(true, |c| matches!(c, ';' | '}' | ')' | ',')))
                                || (matches!(before, Some('[' | ','))
                                    && matches!(after_close, Some(']' | ',' | ')' | ';' | '}')))
                                || (before == Some('(')
                                    && matches!(after_close, Some(')' | ','))
                                    && !matches!(
                                        prev_non_ws(src, src[..brace_pos].rfind('(').unwrap_or(0)),
                                        Some('>')
                                    ))
                                || (before == Some(':')
                                    && matches!(after_close, Some(',' | '}' | ')' | ';'))
                                    && is_after_obj_colon(&src[..brace_pos])))
                    } else {
                        false
                    };
                    if after_dot || is_jsx_attr || is_arrow_param || is_obj_key {
                        out.push_str(tok);
                    } else if is_shorthand {
                        let _ = write!(out, "{tok}: state.{tok}");
                    } else {
                        let _ = write!(out, "state.{tok}");
                    }
                    i = end;
                    continue;
                }
                out.push_str(tok);
                i = end;
                continue;
            }
        }
        let ch = src[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn prev_non_ws(s: &str, idx: usize) -> Option<char> {
    s[..idx].chars().rev().find(|c| !c.is_whitespace())
}

fn next_non_ws(s: &str, idx: usize) -> Option<char> {
    s[idx..].chars().find(|c| !c.is_whitespace())
}

fn ends_with_kw(s: &str, kws: &[&str]) -> bool {
    let t = s.trim_end();
    for kw in kws {
        if let Some(prefix) = t.strip_suffix(kw) {
            if prefix.is_empty() || !is_ident_char(prefix.as_bytes()[prefix.len() - 1]) {
                return true;
            }
        }
    }
    false
}

fn is_after_obj_colon(before_brace: &str) -> bool {
    let Some(before_colon) = before_brace.trim_end().strip_suffix(':') else {
        return false;
    };
    let trimmed = before_colon.trim_end();
    let b = trimmed.as_bytes();
    let mut k = b.len();
    if k == 0 || !is_ident_char(b[k - 1]) {
        return false;
    }
    while k > 0 && is_ident_char(b[k - 1]) {
        k -= 1;
    }
    matches!(prev_non_ws(trimmed, k), Some('{' | ','))
}

fn translate_jsx(src: &str, file: &str, diags: &mut Vec<MigrationDiagnostic>) -> String {
    let mask = code_mask(src);
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len() + 128);
    let mut stack: Vec<(String, String)> = Vec::new();
    let mut i = 0usize;

    while i < b.len() {
        if !mask[i] {
            let ch = src[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        if b[i] == b'<' {
            // Fragment `<>` / `</>`
            if i + 1 < b.len() && b[i + 1] == b'>' {
                stack.push(("".into(), "Column".into()));
                out.push_str("<Column>");
                i += 2;
                continue;
            }
            if i + 2 < b.len() && b[i + 1] == b'/' && b[i + 2] == b'>' {
                if let Some((orig, tj)) = stack.last() {
                    if orig.is_empty() {
                        out.push_str("</");
                        out.push_str(tj);
                        out.push('>');
                        stack.pop();
                        i += 3;
                        continue;
                    }
                }
            }
            // Closing HTML tag `</tag>`
            if i + 1 < b.len() && b[i + 1] == b'/' {
                let after = i + 2;
                if let Some(tag) = ident_at(&src[after..]) {
                    let close_gt = src[after + tag.len()..].trim_start();
                    if close_gt.starts_with('>') {
                        let gt_abs = after + tag.len() + (src[after + tag.len()..].len() - close_gt.len()) + 1;
                        if let Some(pos) = stack.iter().rposition(|(orig, _)| orig == tag) {
                            let (_, tj_tag) = stack.remove(pos);
                            let _ = write!(out, "</{tj_tag}>");
                            i = gt_abs;
                            continue;
                        }
                    }
                }
            }
            // Opening HTML tag `<tag ...>`
            let prev = prev_non_ws(src, i);
            let can_be_jsx = prev.map_or(true, |c| !c.is_ascii_alphanumeric() && !matches!(c, '_' | '$' | ')' | ']'))
                || ends_with_kw(&src[..i], &["return", "else", "yield", "await"]);
            if can_be_jsx {
                if let Some(tag) = ident_at(&src[i + 1..]) {
                    if is_html_tag(tag) {
                        let after_tag = i + 1 + tag.len();
                        let next_ch = b.get(after_tag).copied();
                        if next_ch.is_some_and(|c| c.is_ascii_whitespace() || c == b'>' || c == b'/') {
                            if let Some((attrs, self_closing, tag_end)) = parse_jsx_open_tag(src, &mask, after_tag) {
                                let line_no = src[..i].bytes().filter(|&c| c == b'\n').count() + 1;
                                let (tj_tag, rendered_attrs, void_tag) =
                                    convert_html_element(tag, &attrs, line_no, file, diags);
                                let close_self = self_closing || void_tag;
                                out.push('<');
                                out.push_str(&tj_tag);
                                if !rendered_attrs.is_empty() {
                                    out.push(' ');
                                    out.push_str(&rendered_attrs.join(" "));
                                }
                                if close_self {
                                    out.push_str(" />");
                                } else {
                                    out.push('>');
                                    stack.push((tag.to_string(), tj_tag));
                                }
                                i = tag_end;
                                continue;
                            }
                        }
                    }
                }
            }
        }
        let ch = src[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn is_html_tag(t: &str) -> bool {
    matches!(
        t,
        "div" | "section" | "main" | "article" | "header" | "footer" | "nav" | "aside" | "form"
            | "ul" | "ol" | "li" | "p" | "span" | "label" | "small" | "strong"
            | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
            | "button" | "input" | "textarea" | "img" | "a"
    )
}

#[derive(Clone)]
enum AttrVal {
    Bool,
    Quoted(String),
    Braced(String),
}

type OpenTag = (Vec<(String, AttrVal)>, bool, usize);

fn parse_jsx_open_tag(src: &str, mask: &[bool], start: usize) -> Option<OpenTag> {
    let b = src.as_bytes();
    let mut attrs = Vec::new();
    let mut i = start;
    while i < b.len() {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            return None;
        }
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'>' {
            return Some((attrs, true, i + 2));
        }
        if b[i] == b'>' {
            return Some((attrs, false, i + 1));
        }
        // Spread props `{...props}` or attr names with `-` (`aria-label`)
        if b[i] == b'{' {
            let end_b = find_matching_brace(src, mask, i)?;
            i = end_b + 1;
            continue;
        }
        let attr_start = i;
        while i < b.len() && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'-' | b'_' | b':')) {
            i += 1;
        }
        if i == attr_start {
            return None;
        }
        let name = src[attr_start..i].to_string();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= b.len() {
                return None;
            }
            if b[i] == b'"' || b[i] == b'\'' {
                let q = b[i];
                let q_start = i + 1;
                i += 1;
                while i < b.len() {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        i += 2;
                    } else if b[i] == q {
                        break;
                    } else {
                        i += 1;
                    }
                }
                let val = src[q_start..i].to_string();
                if i < b.len() {
                    i += 1;
                }
                attrs.push((name, AttrVal::Quoted(val)));
            } else if b[i] == b'{' {
                let end_b = find_matching_brace(src, mask, i)?;
                let val = src[i + 1..end_b].trim().to_string();
                i = end_b + 1;
                attrs.push((name, AttrVal::Braced(val)));
            } else {
                return None;
            }
        } else {
            attrs.push((name, AttrVal::Bool));
        }
    }
    None
}

fn convert_html_element(
    tag: &str,
    attrs: &[(String, AttrVal)],
    line_no: usize,
    file: &str,
    diags: &mut Vec<MigrationDiagnostic>,
) -> (String, Vec<String>, bool) {
    let mut input_type: Option<String> = None;
    let mut has_row_flex = false;
    let mut out_attrs: Vec<String> = Vec::new();
    let mut extra_props: BTreeMap<String, String> = BTreeMap::new();

    for (k, v) in attrs {
        if k == "type" {
            if let AttrVal::Quoted(s) = v {
                input_type = Some(s.clone());
            }
        }
        if k == "className" || k == "class" {
            if let AttrVal::Quoted(cls) = v {
                for tok in cls.split_whitespace() {
                    match tok {
                        "flex-row" | "row" => has_row_flex = true,
                        "font-bold" => { extra_props.insert("weight".into(), "\"bold\"".into()); }
                        "p-2" => { extra_props.insert("padding".into(), "8".into()); }
                        "p-4" => { extra_props.insert("padding".into(), "16".into()); }
                        "p-6" => { extra_props.insert("padding".into(), "24".into()); }
                        "gap-2" => { extra_props.insert("gap".into(), "8".into()); }
                        "gap-4" => { extra_props.insert("gap".into(), "16".into()); }
                        "rounded" | "rounded-md" | "rounded-lg" => { extra_props.insert("radius".into(), "8".into()); }
                        _ => {}
                    }
                }
            }
        }
        if k == "style" {
            if let AttrVal::Braced(st) = v {
                parse_inline_style(st, &mut extra_props, &mut has_row_flex);
            }
        }
    }

    let is_checkbox = tag == "input" && input_type.as_deref() == Some("checkbox");
    let (tj_tag, is_void) = match tag {
        "div" | "section" | "main" | "article" | "header" | "footer" | "nav" | "aside" | "form" | "ul" | "ol" | "li" => {
            (if has_row_flex { "Row" } else { "Column" }.to_string(), false)
        }
        "h1" => { extra_props.entry("size".into()).or_insert("24".into()); extra_props.entry("weight".into()).or_insert("\"bold\"".into()); ("Text".into(), false) }
        "h2" => { extra_props.entry("size".into()).or_insert("20".into()); extra_props.entry("weight".into()).or_insert("\"bold\"".into()); ("Text".into(), false) }
        "h3" => { extra_props.entry("size".into()).or_insert("18".into()); extra_props.entry("weight".into()).or_insert("\"bold\"".into()); ("Text".into(), false) }
        "h4" | "h5" | "h6" => { extra_props.entry("size".into()).or_insert("16".into()); extra_props.entry("weight".into()).or_insert("\"bold\"".into()); ("Text".into(), false) }
        "strong" => { extra_props.entry("weight".into()).or_insert("\"bold\"".into()); ("Text".into(), false) }
        "p" | "span" | "label" | "small" => ("Text".into(), false),
        "button" => ("Button".into(), false),
        "input" if is_checkbox => ("Toggle".into(), true),
        "input" => {
            if let Some(ref ty) = input_type {
                match ty.as_str() {
                    "password" => { extra_props.insert("secure".into(), "true".into()); }
                    "number" | "date" | "time" | "datetime-local" => {
                        let mapped = if ty == "datetime-local" { "datetime" } else { ty.as_str() };
                        extra_props.insert("type".into(), format!("\"{mapped}\""));
                        diags.push(MigrationDiagnostic {
                            file: file.into(),
                            line: line_no,
                            severity: "info",
                            kind: "input-typed",
                            message: format!("`<input type=\"{ty}\">` emits `number | null` directly in TinyJump"),
                            hint: "Typed `<Input>` handlers receive a number directly (or `null` when empty), no `e.target.value` needed.".into(),
                        });
                    }
                    _ => {}
                }
            }
            ("Input".into(), true)
        }
        "textarea" => {
            extra_props.insert("multiline".into(), "true".into());
            ("Input".into(), true)
        }
        "img" => ("Image".into(), true),
        "a" => {
            diags.push(MigrationDiagnostic {
                file: file.into(),
                line: line_no,
                severity: "manual",
                kind: "jsx-anchor",
                message: "`<a>` anchor converted to `<Text>` (`href` removed)".into(),
                hint: "Replace navigation links with `<Tabs>` or button `onPress` state transitions.".into(),
            });
            ("Text".into(), false)
        }
        _ => (tag.to_string(), false),
    };

    for (k, v) in attrs {
        match k.as_str() {
            "className" | "class" => {
                diags.push(MigrationDiagnostic {
                    file: file.into(),
                    line: line_no,
                    severity: "info",
                    kind: "jsx-classname",
                    message: "Stripped CSS `className` attribute".into(),
                    hint: "Mapped basic layout classes where recognized; style remaining appearance with TinyJump props.".into(),
                });
            }
            "style" | "htmlFor" | "name" | "autoFocus" => {}
            "type" if tag == "button" || tag == "input" => {}
            "href" if tag == "a" => {}
            "onSubmit" if tag == "form" => {
                diags.push(MigrationDiagnostic {
                    file: file.into(),
                    line: line_no,
                    severity: "manual",
                    kind: "form-onSubmit",
                    message: "Removed `<form onSubmit>` from container `<Column>`".into(),
                    hint: "Wire submission on the `<Button onPress={...}>` or use `Form` / `FormInput` from `tj:ui`.".into(),
                });
            }
            "checked" if is_checkbox => {
                out_attrs.push(format_attr("value", v));
            }
            "alt" if tag == "img" => {
                out_attrs.push(format_attr("label", v));
            }
            "aria-label" => {
                out_attrs.push(format_attr("label", v));
            }
            "onClick" => {
                let cleaned = unwrap_event_handler(v, false);
                out_attrs.push(format!("onPress={{{cleaned}}}"));
            }
            "onChange" if tag == "input" || tag == "textarea" => {
                let cleaned = unwrap_event_handler(v, is_checkbox);
                out_attrs.push(format!("onChange={{{cleaned}}}"));
            }
            _ => {
                out_attrs.push(format_attr(k, v));
            }
        }
    }
    for (pk, pv) in extra_props {
        let already = out_attrs.iter().any(|a| a.starts_with(&format!("{pk}=")));
        if !already {
            if pv.starts_with('"') && pv.ends_with('"') {
                out_attrs.push(format!("{pk}={pv}"));
            } else {
                out_attrs.push(format!("{pk}={{{pv}}}"));
            }
        }
    }
    (tj_tag, out_attrs, is_void)
}

fn format_attr(name: &str, val: &AttrVal) -> String {
    match val {
        AttrVal::Bool => format!("{name}={{true}}"),
        AttrVal::Quoted(s) => format!("{name}=\"{s}\""),
        AttrVal::Braced(s) => format!("{name}={{{s}}}"),
    }
}

fn parse_inline_style(raw: &str, props: &mut BTreeMap<String, String>, has_row_flex: &mut bool) {
    let inner = raw.trim().strip_prefix('{').and_then(|s| s.strip_suffix('}')).unwrap_or(raw).trim();
    for part in inner.split(',') {
        let Some((k_raw, v_raw)) = part.split_once(':') else { continue };
        let k = k_raw.trim().trim_matches(['"', '\'']);
        let v = v_raw.trim();
        match k {
            "flexDirection" if v.contains("row") => *has_row_flex = true,
            "padding" | "margin" | "gap" | "width" | "height" => {
                if v.bytes().all(|b| b.is_ascii_digit()) {
                    props.insert(k.to_string(), v.to_string());
                }
            }
            "borderRadius" if v.bytes().all(|b| b.is_ascii_digit()) => {
                props.insert("radius".into(), v.to_string());
            }
            "fontSize" if v.bytes().all(|b| b.is_ascii_digit()) => {
                props.insert("size".into(), v.to_string());
            }
            "fontWeight" if v == "\"bold\"" || v == "'bold'" || v == "700" => {
                props.insert("weight".into(), "\"bold\"".into());
            }
            "color" | "background" | "backgroundColor" => {
                let target_key = if k == "backgroundColor" { "background" } else { k };
                let cleaned = v.trim_matches('\'');
                if cleaned.starts_with('"') || (v.starts_with('\'') && v.ends_with('\'')) {
                    let s = v.trim_matches(['"', '\'']);
                    props.insert(target_key.into(), format!("\"{s}\""));
                }
            }
            _ => {}
        }
    }
}

fn unwrap_event_handler(val: &AttrVal, is_checkbox: bool) -> String {
    let raw = match val {
        AttrVal::Braced(s) => s.as_str(),
        AttrVal::Quoted(s) => return format!("\"{s}\""),
        AttrVal::Bool => return "() => {}".into(),
    };
    let mut expr = raw
        .replace("e.preventDefault();", "")
        .replace("e.preventDefault()", "")
        .replace("event.preventDefault();", "")
        .replace("event.preventDefault()", "");

    if is_checkbox && (expr.contains(".target.checked") || expr.contains(".currentTarget.checked")) {
        if let Some((param, body)) = parse_single_arrow_fn(&expr) {
            let b = body
                .replace(&format!("{param}.target.checked"), "checked")
                .replace(&format!("{param}.currentTarget.checked"), "checked");
            return format!("checked => {b}");
        }
    }
    if expr.contains(".target.value") || expr.contains(".currentTarget.value") {
        if let Some((param, body)) = parse_single_arrow_fn(&expr) {
            let b = body
                .replace(&format!("{param}.target.value"), "v")
                .replace(&format!("{param}.currentTarget.value"), "v");
            return format!("v => {b}");
        }
    }
    if let Some((param, body)) = parse_single_arrow_fn(&expr) {
        if !body.contains(&param) {
            return format!("() => {}", body.trim());
        }
    }
    expr = expr.trim().to_string();
    expr
}

fn trim_excess_blank_lines(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut blanks = 0usize;
    for line in src.lines() {
        if line.trim().is_empty() {
            blanks += 1;
            if blanks <= 1 && !out.is_empty() {
                out.push('\n');
            }
        } else {
            blanks = 0;
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

// =========================================================================
// `tj migrate compare` — behaviour, API, live URL & screenshot verification
// =========================================================================

struct CompareMeasure;

impl Measure for CompareMeasure {
    fn measure(&mut self, _node: u32, _max_width: Option<f32>) -> (f32, f32) {
        (80.0, 24.0)
    }
}

#[derive(Default)]
struct ScreenInventory {
    texts: BTreeSet<String>,
    buttons: BTreeSet<String>,
    inputs: BTreeSet<String>,
}

fn boot_and_extract_inventory(source: &str, width: f32, height: f32) -> Result<ScreenInventory, String> {
    let program = tj_core::load(source).map_err(|errs| errs.iter().map(|d| d.message.as_str()).collect::<Vec<_>>().join("; "))?;
    let opts = Options {
        now: 1_800_000_000_000.0,
        seed: 1,
        render: true,
        server: true,
        explain: true,
        max_objects: 100_000,
        max_object_bytes: 0,
        remote_classes: Vec::new(),
    };
    let (mut rt, _out) = Runtime::boot(program, Box::new(MemStore::default()), opts).map_err(|e| format!("boot failed: {e}"))?;
    rt.set_measure(Box::new(CompareMeasure));
    let _ = rt.handle(1_800_000_000_000.0, Input::Entropy { bytes: "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20".into() });
    let _ = rt.handle(1_800_000_000_000.0, Input::Layout { width, height, rtl: false });
    let reply_out = rt.handle(1_800_000_000_000.0, Input::Inspect { reply: Some(1) });
    for o in reply_out {
        if let Output::Reply { ok, error, .. } = o {
            if let Some(err) = error {
                return Err(err);
            }
            let snap = ok.unwrap_or(Json::Null);
            let mut inv = ScreenInventory::default();
            collect_inventory_nodes(snap["tree"].as_array().map_or(&[], Vec::as_slice), &mut inv);
            return Ok(inv);
        }
    }
    Err("inspect did not return tree".into())
}

fn collect_inventory_nodes(nodes: &[Json], inv: &mut ScreenInventory) {
    for n in nodes {
        let kind = n["kind"].as_str().unwrap_or("");
        let props = &n["props"];
        let text = props["text"].as_str().unwrap_or("").trim();
        let label = props["label"].as_str().unwrap_or("").trim();
        let ph = props["placeholder"].as_str().unwrap_or("").trim();
        if !text.is_empty() {
            inv.texts.insert(text.to_string());
        }
        if !label.is_empty() {
            inv.texts.insert(label.to_string());
        }
        match kind {
            "Button" => {
                let b = if !text.is_empty() { text } else { label };
                if !b.is_empty() {
                    inv.buttons.insert(b.to_string());
                }
            }
            "Input" | "Toggle" => {
                if !label.is_empty() {
                    inv.inputs.insert(label.to_string());
                }
                if !ph.is_empty() {
                    inv.inputs.insert(ph.to_string());
                }
                if !text.is_empty() {
                    inv.inputs.insert(text.to_string());
                }
            }
            _ => {}
        }
        if let Some(children) = n["children"].as_array() {
            collect_inventory_nodes(children, inv);
        }
    }
}

struct CompareOpts {
    migrated: PathBuf,
    traces: Vec<PathBuf>,
    old_snapshot: Option<PathBuf>,
    old_url: Option<String>,
    old_screenshot: Option<PathBuf>,
    new_screenshot: Option<PathBuf>,
    threshold: f64,
    diff_out: Option<PathBuf>,
    width: f32,
    height: f32,
    as_json: bool,
}

fn run_compare(args: &[String]) -> Result<(), String> {
    let usage = "usage: tj migrate compare <migrated.tj> [--trace <file.trace.jsonl>]... [--old-snapshot <file.json>]\n  [--old-url <http://...>] [--old-screenshot <orig.png> --new-screenshot <migrated.png> [--threshold <pct>] [--diff <out.png>]]\n  [--width <n>] [--height <n>] [--json]";
    let mut migrated: Option<PathBuf> = None;
    let mut traces = Vec::new();
    let mut old_snapshot: Option<PathBuf> = None;
    let mut old_url: Option<String> = None;
    let mut old_screenshot: Option<PathBuf> = None;
    let mut new_screenshot: Option<PathBuf> = None;
    let mut threshold = DEFAULT_THRESHOLD_PCT;
    let mut diff_out: Option<PathBuf> = None;
    let mut width = 390.0f32;
    let mut height = 844.0f32;
    let mut as_json = false;

    let mut it = args.iter().peekable();
    if let Some(first) = it.next_if(|a| !a.starts_with("--")) {
        migrated = Some(PathBuf::from(first));
    }
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--trace" => traces.push(PathBuf::from(value()?)),
            "--old-snapshot" => old_snapshot = Some(PathBuf::from(value()?)),
            "--old-url" => old_url = Some(value()?.clone()),
            "--old-screenshot" => old_screenshot = Some(PathBuf::from(value()?)),
            "--new-screenshot" => new_screenshot = Some(PathBuf::from(value()?)),
            "--threshold" => {
                let pct: f64 = value()?.parse().map_err(|_| "--threshold must be in 0..=100")?;
                if !pct.is_finite() || !(0.0..=100.0).contains(&pct) {
                    return Err("--threshold must be in 0..=100".into());
                }
                threshold = pct;
            }
            "--diff" => diff_out = Some(PathBuf::from(value()?)),
            "--width" => {
                let w: f32 = value()?.parse().map_err(|_| "--width must be a positive number")?;
                if !w.is_finite() || w <= 0.0 {
                    return Err("--width must be positive".into());
                }
                width = w;
            }
            "--height" => {
                let h: f32 = value()?.parse().map_err(|_| "--height must be a positive number")?;
                if !h.is_finite() || h <= 0.0 {
                    return Err("--height must be positive".into());
                }
                height = h;
            }
            "--json" => as_json = true,
            "--help" | "-h" => {
                println!("{usage}");
                return Ok(());
            }
            other if !other.starts_with("--") && migrated.is_none() => migrated = Some(PathBuf::from(other)),
            other => return Err(format!("unknown option {other}\n\n{usage}")),
        }
    }
    if old_screenshot.is_some() != new_screenshot.is_some() {
        return Err("--old-screenshot and --new-screenshot must be provided together".into());
    }
    let opts = CompareOpts {
        migrated: migrated.ok_or_else(|| format!("`tj migrate compare` needs `<migrated.tj>`\n\n{usage}"))?,
        traces,
        old_snapshot,
        old_url,
        old_screenshot,
        new_screenshot,
        threshold,
        diff_out,
        width,
        height,
        as_json,
    };

    let rep = compare_impl(&opts)?;
    if opts.as_json {
        println!("{}", serde_json::to_string_pretty(&rep).expect("json"));
    } else {
        println!("{}", format_compare_human(&rep));
    }
    if rep["passed"].as_bool() == Some(true) {
        Ok(())
    } else {
        Err("migrate compare failed: parity or layout checks did not pass".into())
    }
}

fn compare_impl(opts: &CompareOpts) -> Result<Json, String> {
    let app_dir = opts.migrated.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let loaded = app::read(&opts.migrated)?;
    let inv = boot_and_extract_inventory(&loaded.source, opts.width, opts.height)?;

    let layout_findings = test_runner::layout_findings_multi(&loaded.source, &[("compare", opts.width, opts.height, false)])?;
    let layout_errors: Vec<String> = layout_findings
        .into_iter()
        .flat_map(|(_, fs)| fs.into_iter())
        .filter(|f| f.severity == review::Severity::Error)
        .map(|f| format!("[{}] {}: {}", f.check, f.node, f.message))
        .collect();

    let mut passed = layout_errors.is_empty();
    let mut trace_reports = Vec::new();
    for tpath in &opts.traces {
        let ttext = read_bounded_utf8(tpath, MAX_FILE_BYTES)?;
        let res = test_runner::run_trace(app_dir, &loaded.source, &tpath.display().to_string(), &ttext, HashMap::new());
        if !res.passed() {
            passed = false;
        }
        trace_reports.push(res.json());
    }

    let snapshot_report = if let Some(ref spath) = opts.old_snapshot {
        let raw = read_bounded_utf8(spath, MAX_SNAP_BYTES)?;
        let snap: Json = serde_json::from_str(&raw).map_err(|e| format!("invalid snapshot {}: {e}", spath.display()))?;
        let text_cmp = compare_sets(&snap["texts"], &inv.texts, true);
        let btn_cmp = compare_sets(&snap["buttons"], &inv.buttons, false);
        let inp_cmp = compare_sets(&snap["inputs"], &inv.inputs, false);
        let mut api_error: Option<String> = None;
        let api_ok = if let Some(calls) = snap["api"].as_array().filter(|a| !a.is_empty()) {
            let mut trace_buf = String::from("{\"do\":\"boot\",\"server\":true,\"render\":false}\n");
            for c in calls {
                let mut step = json!({
                    "do": "call",
                    "object": c["object"],
                    "method": c["method"],
                    "args": c.get("args").cloned().unwrap_or_else(|| json!([])),
                });
                if let Some(exp) = c.get("expect") {
                    step.as_object_mut().unwrap().insert("expect".into(), exp.clone());
                }
                if let Some(exp_c) = c.get("expectContains") {
                    step.as_object_mut().unwrap().insert("expectContains".into(), exp_c.clone());
                }
                if let Some(exp_e) = c.get("expectError") {
                    step.as_object_mut().unwrap().insert("expectError".into(), exp_e.clone());
                }
                let _ = writeln!(trace_buf, "{step}");
            }
            let res = test_runner::run_trace(app_dir, &loaded.source, "snapshot-api", &trace_buf, HashMap::new());
            if !res.passed() {
                api_error = res.error;
                false
            } else {
                true
            }
        } else {
            true
        };
        if text_cmp["missing"].as_array().is_some_and(|a| !a.is_empty())
            || btn_cmp["missing"].as_array().is_some_and(|a| !a.is_empty())
            || inp_cmp["missing"].as_array().is_some_and(|a| !a.is_empty())
            || !api_ok
        {
            passed = false;
        }
        Some(json!({
            "texts": text_cmp,
            "buttons": btn_cmp,
            "inputs": inp_cmp,
            "apiPassed": api_ok,
            "apiError": api_error,
        }))
    } else {
        None
    };

    let url_report = if let Some(ref url) = opts.old_url {
        let fetched = fetch_old_url_texts(url)?;
        let expected_json = Json::Array(fetched.iter().cloned().map(Json::String).collect());
        let text_cmp = compare_sets(&expected_json, &inv.texts, true);
        if text_cmp["missing"].as_array().is_some_and(|a| !a.is_empty()) {
            passed = false;
        }
        Some(json!({ "url": url, "extractedCount": fetched.len(), "texts": text_cmp }))
    } else {
        None
    };

    let visual_report = if let (Some(ref old_p), Some(ref new_p)) = (&opts.old_screenshot, &opts.new_screenshot) {
        let old_img = decode_png(&fs::read(old_p).map_err(|e| format!("cannot read {}: {e}", old_p.display()))?)?;
        let new_img = decode_png(&fs::read(new_p).map_err(|e| format!("cannot read {}: {e}", new_p.display()))?)?;
        let build_diff = opts.diff_out.is_some();
        let diff = compare_png(&old_img, &new_img, crate::golden::DEFAULT_CHANNEL_TOLERANCE, build_diff);
        if let (Some(ref dp), Some(ref img)) = (&opts.diff_out, &diff.diff_image) {
            if let Some(p) = dp.parent().filter(|p| !p.as_os_str().is_empty()) {
                fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
            }
            fs::write(dp, encode_png(img)?).map_err(|e| format!("cannot write {}: {e}", dp.display()))?;
        }
        let ok = diff.passed(opts.threshold);
        if !ok {
            passed = false;
        }
        Some(json!({
            "passed": ok,
            "diffPercent": diff.diff_percent,
            "thresholdPercent": opts.threshold,
            "summary": diff.summary(opts.threshold),
            "diffImage": opts.diff_out.as_ref().map(|p| p.display().to_string()),
        }))
    } else {
        None
    };

    Ok(json!({
        "passed": passed,
        "migrated": opts.migrated.display().to_string(),
        "viewport": { "w": opts.width, "h": opts.height },
        "extracted": {
            "texts": inv.texts.iter().collect::<Vec<_>>(),
            "buttons": inv.buttons.iter().collect::<Vec<_>>(),
            "inputs": inv.inputs.iter().collect::<Vec<_>>(),
        },
        "layoutErrors": layout_errors,
        "traces": trace_reports,
        "snapshot": snapshot_report,
        "oldUrl": url_report,
        "visual": visual_report,
    }))
}

fn compare_sets(expected_json: &Json, actual: &BTreeSet<String>, substring_ok: bool) -> Json {
    let want: Vec<String> = expected_json
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Json::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let mut matched = Vec::new();
    let mut missing = Vec::new();
    for item in &want {
        let lower_item = item.to_ascii_lowercase();
        let found = actual.iter().any(|a| {
            let lower_a = a.to_ascii_lowercase();
            if substring_ok {
                lower_a.contains(&lower_item) || lower_item == lower_a
            } else {
                lower_a == lower_item
            }
        });
        if found {
            matched.push(item.clone());
        } else {
            missing.push(item.clone());
        }
    }
    let extra: Vec<String> = actual
        .iter()
        .filter(|a| {
            let la = a.to_ascii_lowercase();
            !want.iter().any(|w| {
                let lw = w.to_ascii_lowercase();
                if substring_ok { la.contains(&lw) || lw == la } else { la == lw }
            })
        })
        .cloned()
        .collect();
    let parity = if want.is_empty() { 100.0 } else { (matched.len() as f64 / want.len() as f64) * 100.0 };
    json!({
        "matched": matched,
        "missing": missing,
        "extra": extra,
        "parityPct": (parity * 10.0).round() / 10.0,
    })
}

fn fetch_old_url_texts(url: &str) -> Result<Vec<String>, String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!("--old-url {url:?} must start with http:// or https://"));
    }
    let after_scheme = url.split_once("://").map(|x| x.1).unwrap_or("");
    let host_port = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    if host_port.is_empty() {
        return Err(format!("invalid --old-url {url:?}"));
    }
    let host_only = if host_port.starts_with('[') {
        host_port.split(']').next().unwrap_or("").trim_start_matches('[')
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    let port: u16 = if host_port.contains(':') && !host_port.ends_with(']') {
        host_port.rsplit(':').next().and_then(|p| p.parse().ok()).unwrap_or(if url.starts_with("https://") { 443 } else { 80 })
    } else if url.starts_with("https://") {
        443
    } else {
        80
    };
    let is_loopback_name = matches!(host_only, "localhost" | "127.0.0.1" | "::1");
    let allow_private = is_loopback_name || std::env::var(PRIVATE_FETCH_ENV).is_ok_and(|v| v == "1");
    if !allow_private {
        let addrs: Vec<_> = (host_only, port)
            .to_socket_addrs()
            .map_err(|e| format!("cannot resolve {host_only}: {e}"))?
            .collect();
        if addrs.is_empty() || addrs.iter().any(|a| !is_public(a.ip()) && !a.ip().is_loopback()) {
            return Err(format!("refusing private/link-local address for {url:?} (set {PRIVATE_FETCH_ENV}=1 to allow)"));
        }
    }

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .redirects(0)
        .build();
    let resp = agent.get(url).call().map_err(|e| format!("cannot fetch {url}: {e}"))?;
    let is_json = resp.content_type().contains("json");
    let mut bytes = Vec::new();
    resp.into_reader()
        .take(MAX_URL_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read {url}: {e}"))?;
    if bytes.len() > MAX_URL_BYTES {
        return Err(format!("{url} response exceeds {} MiB limit", MAX_URL_BYTES >> 20));
    }
    let body = String::from_utf8(bytes).map_err(|_| format!("{url} response is not UTF-8"))?;
    if is_json {
        if let Ok(v) = serde_json::from_str::<Json>(&body) {
            let mut out = Vec::new();
            collect_json_strings(&v, &mut out);
            return Ok(out);
        }
    }
    Ok(extract_html_text_tokens(&body))
}

fn collect_json_strings(v: &Json, out: &mut Vec<String>) {
    match v {
        Json::String(s) => {
            let t = s.trim();
            if !t.is_empty() && !out.contains(&t.to_string()) {
                out.push(t.to_string());
            }
        }
        Json::Array(arr) => arr.iter().for_each(|x| collect_json_strings(x, out)),
        Json::Object(map) => map.values().for_each(|x| collect_json_strings(x, out)),
        _ => {}
    }
}

fn extract_html_text_tokens(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut stripped = String::with_capacity(html.len());
    let mut i = 0usize;
    while i < html.len() {
        if lower[i..].starts_with("<script") {
            if let Some(end) = lower[i..].find("</script>") {
                i += end + 9;
                continue;
            }
        }
        if lower[i..].starts_with("<style") {
            if let Some(end) = lower[i..].find("</style>") {
                i += end + 8;
                continue;
            }
        }
        if html.as_bytes()[i] == b'<' {
            if let Some(gt) = html[i..].find('>') {
                stripped.push('\n');
                i += gt + 1;
                continue;
            }
        }
        let ch = html[i..].chars().next().unwrap();
        stripped.push(ch);
        i += ch.len_utf8();
    }
    let decoded = stripped
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    let mut out = Vec::new();
    for line in decoded.lines() {
        let t = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

fn format_compare_human(rep: &Json) -> String {
    let mut s = format!(
        "migrate compare {}: {}\n",
        rep["migrated"].as_str().unwrap_or(""),
        if rep["passed"].as_bool() == Some(true) { "PASSED" } else { "FAILED" }
    );
    if let Some(errs) = rep["layoutErrors"].as_array().filter(|a| !a.is_empty()) {
        for e in errs {
            let _ = writeln!(s, "  layout error: {}", e.as_str().unwrap_or(""));
        }
    }
    if let Some(snap) = rep["snapshot"].as_object() {
        let _ = writeln!(
            s,
            "  snapshot parity: texts {}%, buttons {}%, inputs {}%, apiPassed={}",
            snap["texts"]["parityPct"], snap["buttons"]["parityPct"], snap["inputs"]["parityPct"], snap["apiPassed"]
        );
    }
    if let Some(u) = rep["oldUrl"].as_object() {
        let _ = writeln!(s, "  old-url parity ({}): {}%", u["url"].as_str().unwrap_or(""), u["texts"]["parityPct"]);
    }
    if let Some(v) = rep["visual"].as_object() {
        let _ = writeln!(s, "  visual: {}", v["summary"].as_str().unwrap_or(""));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::thread;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tj-migrate-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn translates_react_counter_tsx_compiles_and_runs() {
        let react_src = r#"
import React, { useState } from "react";
import "./Counter.css";

export default function App() {
  const [count, setCount] = useState<number>(0);
  return (
    <div className="p-4 flex-row gap-2">
      <h1>Counter App — Café</h1>
      <span style={{ fontSize: 18 }}>Count: {count}</span>
      <button type="button" onClick={() => setCount(c => c + 1)}>Increment</button>
    </div>
  );
}
"#;
        let (tj_code, diags) = translate_source(react_src, "Counter.tsx");
        assert!(diags.iter().all(|d| d.severity != "manual"), "unexpected manual diagnostics: {diags:?}");
        assert!(tj_code.contains("object AppState"), "{tj_code}");
        assert!(tj_code.contains("count: number = 0"), "{tj_code}");
        assert!(tj_code.contains("const state = AppState.get(\"main\")"), "{tj_code}");
        assert!(tj_code.contains("state.setCount(state.count + 1)"), "{tj_code}");
        assert!(tj_code.contains("<Row gap={8} padding={16}>"), "{tj_code}");
        assert!(tj_code.contains("<Button onPress={() => state.setCount(state.count + 1)}>Increment</Button>"), "{tj_code}");

        let prog = tj_core::compile(&tj_code).unwrap_or_else(|e| panic!("compiled failed: {e:?}\n{tj_code}"));
        let trace = r#"
{"do": "boot", "render": true}
{"do": "assertScreen", "contains": ["Counter App — Café", "Count: 0", "Increment"]}
{"do": "ui", "target": "Increment", "event": "onPress"}
{"do": "assertScreen", "contains": ["Count: 1"]}
"#;
        let res = test_runner::run_trace(Path::new("."), &tj_code, "counter.trace.jsonl", trace, HashMap::new());
        assert!(res.passed(), "migrated Counter trace failed: {:?}\n{tj_code}", res.error);
        drop(prog);
    }

    #[test]
    fn translates_todo_form_inputs_checkbox_and_dialect() {
        let tsx = r#"
import { useState } from "react";

export default function TodoForm(): JSX.Element {
  var title = "Tasks";
  const [text, setText] = useState("");
  const [done, setDone] = useState(false);
  const check = text == undefined ? "empty" : text;
  return (
    <form onSubmit={(e) => { e.preventDefault(); setText(""); }}>
      <h2>{title}</h2>
      <input type="text" value={text} placeholder="New task" onChange={(e) => setText(e.target.value)} />
      <input type="checkbox" checked={done} onChange={(e) => setDone(e.target.checked)} aria-label="Mark done" />
      <span>{check != "empty" ? text : "Nothing"}</span>
      <button type="submit" onClick={() => setText("")}>Add</button>
    </form>
  );
}
"#;
        let (tj_code, diags) = translate_source(&format!("\u{feff}{tsx}"), "TodoForm.tsx");
        assert!(tj_code.contains("let title = \"Tasks\""), "{tj_code}");
        assert!(tj_code.contains("state.text === null"), "{tj_code}");
        assert!(tj_code.contains("check !== \"empty\""), "{tj_code}");
        assert!(tj_code.contains("<Input value={state.text} placeholder=\"New task\" onChange={v => state.setText(v)} />"), "{tj_code}");
        assert!(tj_code.contains("<Toggle value={state.done} onChange={checked => state.setDone(checked)} label=\"Mark done\""), "{tj_code}");
        assert!(diags.iter().any(|d| d.kind == "form-onSubmit"));

        // Wrap with App() and verify tj_core compiles the output cleanly.
        let full = format!("{tj_code}\nfunction App() {{ return <TodoForm /> }}\n");
        tj_core::compile(&full).unwrap_or_else(|e| panic!("TodoForm compile error: {e:?}\n{full}"));
    }

    #[test]
    fn reports_unsupported_patterns_and_ignores_strings_and_comments() {
        let tsx = r#"
import axios from "axios";
import { Helper } from "./helper.ts";

export function Broken() {
  // undefined useEffect localStorage inside comment must be ignored!
  const msg = "class switch useEffect window.localStorage undefined";
  const [items, setItems] = useState<string[]>([]);
  useEffect(() => {
    setInterval(() => { console.log("tick"); }, 1000);
  }, []);
  const saved = localStorage.getItem("k");
  return <div><span>{msg}</span><Helper items={items} /></div>;
}
"#;
        let (tj_code, diags) = translate_source(tsx, "Broken.tsx");
        assert!(tj_code.contains("import { Helper } from \"./helper\""), "{tj_code}");
        assert!(tj_code.contains("const msg = \"class switch useEffect window.localStorage undefined\""), "string must not be mangled:\n{tj_code}");
        assert!(diags.iter().any(|d| d.kind == "import-pkg" && d.severity == "manual"));
        assert!(diags.iter().any(|d| d.kind == "hook-useEffect" && d.hint.contains("@every")));
        assert!(diags.iter().any(|d| d.kind == "web-storage" && d.severity == "manual"));
        assert!(!diags.iter().any(|d| d.line == 6), "comment line 6 must not emit false-positive diagnostic: {diags:?}");
    }

    #[test]
    fn migrates_directory_skipping_node_modules_and_tests() {
        let dir = tmp("dir");
        let app_dir = dir.join("my-react");
        let src_dir = app_dir.join("src");
        let nm_dir = app_dir.join("node_modules/pkg");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&nm_dir).unwrap();

        fs::write(nm_dir.join("bad.ts"), "export class Bad {}").unwrap();
        fs::write(src_dir.join("App.test.tsx"), "class IgnoredTest {}").unwrap();
        fs::write(src_dir.join("utils.ts"), "export function greet(who: string): string { return `Hello ${who}` }\n").unwrap();
        fs::write(
            src_dir.join("App.tsx"),
            r#"
import { useState } from "react";
import { greet } from "./utils.ts";

export default function App() {
  const [name, setName] = useState("World");
  return <div><h1>{greet(name)}</h1><button onClick={() => setName("TinyJump")}>Switch</button></div>;
}
"#,
        )
        .unwrap();

        let out_dir = dir.join("migrated-app");
        let outcome = migrate_path(&app_dir, Some(&out_dir)).unwrap();
        assert_eq!(outcome.json["fileCount"], json!(2));
        assert!(outcome.all_compile, "directory migration should compile cleanly:\n{}", outcome.summary);
        assert!(out_dir.join("src/App.tj").is_file());
        assert!(out_dir.join("src/utils.tj").is_file());
        assert!(!out_dir.join("node_modules/pkg/bad.tj").exists());
        assert!(!out_dir.join("src/App.test.tj").exists());

        // Overwrite protection refuses same input/output path.
        assert!(migrate_path(&src_dir.join("App.tsx"), Some(&src_dir.join("App.tsx"))).is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compares_traces_snapshots_old_url_and_visual_pngs() {
        let dir = tmp("compare");
        let tj_file = dir.join("app.tj");
        let (tj_code, _) = translate_source(
            r#"
import { useState } from "react";
export default function App() {
  const [n, setN] = useState(0);
  return <div><h1>Parity Title</h1><span>Total: {n}</span><button onClick={() => setN(n + 1)}>Add</button></div>;
}
"#,
            "App.tsx",
        );
        fs::write(&tj_file, &tj_code).unwrap();

        let trace_file = dir.join("parity.trace.jsonl");
        fs::write(
            &trace_file,
            "{\"do\":\"boot\",\"render\":true}\n{\"do\":\"assertScreen\",\"contains\":[\"Parity Title\",\"Total: 0\"]}\n{\"do\":\"ui\",\"target\":\"Add\",\"event\":\"onPress\"}\n{\"do\":\"assertScreen\",\"contains\":[\"Total: 1\"]}\n",
        )
        .unwrap();

        let snap_pass = dir.join("snap-ok.json");
        fs::write(
            &snap_pass,
            r#"{
              "texts": ["Parity Title", "Total: 0"],
              "buttons": ["Add"],
              "api": [{ "object": "AppState/main", "method": "setN", "args": [5] }]
            }"#,
        )
        .unwrap();

        // Loopback old-url mock server serving HTML matching old page texts.
        let srv = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = srv.server_addr().to_ip().unwrap().port();
        let (done_tx, done_rx) = channel();
        thread::spawn(move || {
            if let Ok(Some(req)) = srv.recv_timeout(Duration::from_secs(5)) {
                let html = "<!doctype html><html><script>ignored()</script><body><h1>Parity Title</h1><p>Total: 0</p><button>Add</button></body></html>";
                let _ = req.respond(tiny_http::Response::from_string(html));
            }
            let _ = done_tx.send(());
        });

        let old_png = dir.join("old.png");
        let new_png = dir.join("new.png");
        let diff_png = dir.join("diff.png");
        let img = crate::golden::Image { width: 2, height: 2, pixels: vec![255; 16] };
        fs::write(&old_png, encode_png(&img).unwrap()).unwrap();
        fs::write(&new_png, encode_png(&img).unwrap()).unwrap();

        let ok_rep = compare_impl(&CompareOpts {
            migrated: tj_file.clone(),
            traces: vec![trace_file.clone()],
            old_snapshot: Some(snap_pass),
            old_url: Some(format!("http://127.0.0.1:{port}/")),
            old_screenshot: Some(old_png.clone()),
            new_screenshot: Some(new_png.clone()),
            threshold: 0.1,
            diff_out: Some(diff_png.clone()),
            width: 390.0,
            height: 844.0,
            as_json: false,
        })
        .unwrap();
        let _ = done_rx.recv_timeout(Duration::from_secs(2));
        assert_eq!(ok_rep["passed"], json!(true), "{ok_rep}");
        assert_eq!(ok_rep["snapshot"]["texts"]["parityPct"], json!(100.0));
        assert_eq!(ok_rep["oldUrl"]["texts"]["parityPct"], json!(100.0));
        assert_eq!(ok_rep["visual"]["passed"], json!(true));
        assert!(diff_png.is_file());

        // Missing snapshot item & differing PNG fail cleanly.
        let snap_fail = dir.join("snap-bad.json");
        fs::write(&snap_fail, r#"{ "texts": ["Missing Text"], "buttons": ["Gone"] }"#).unwrap();
        let bad_img = crate::golden::Image { width: 2, height: 2, pixels: vec![0; 16] };
        fs::write(&new_png, encode_png(&bad_img).unwrap()).unwrap();
        let bad_rep = compare_impl(&CompareOpts {
            migrated: tj_file,
            traces: vec![],
            old_snapshot: Some(snap_fail),
            old_url: None,
            old_screenshot: Some(old_png),
            new_screenshot: Some(new_png),
            threshold: 0.1,
            diff_out: Some(diff_png),
            width: 390.0,
            height: 844.0,
            as_json: false,
        })
        .unwrap();
        assert_eq!(bad_rep["passed"], json!(false));
        assert_eq!(bad_rep["snapshot"]["texts"]["missing"], json!(["Missing Text"]));
        assert_eq!(bad_rep["visual"]["passed"], json!(false));

        let _ = fs::remove_dir_all(&dir);
    }
}

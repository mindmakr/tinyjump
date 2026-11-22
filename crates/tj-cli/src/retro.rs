//! `tj retro`: actionable post-run & health report for an app (static dead-code and access
//! checks, multi-viewport LTR/RTL layout sweep, benchmark/budget headroom, live server
//! telemetry via `/_tj/manifest` + `/_tj/inspect`, and local storage/key hygiene).
//!
//! Every finding carries an action tag: `fix`, `optimize`, `prune`, or `rotate`.

use crate::{access, app, audit, auth, bench, manifest, review, sealed, test_runner};
use serde_json::{json, Map, Value as Json};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tj_core::modules;
use tj_core::syntax::ast::{ArrayEntry, Expr, ExprKind, Item, JsxChild, JsxElement, RecordEntry, Stmt, TplPiece};
use tj_core::syntax::parse_range;

const SLOW_P95_MS: f64 = 200.0;
const HEAVY_STEPS_PER_CALL: u64 = 50_000;
const OBJECT_BYTES_WARN_PCT: f64 = 0.70;
const BUDGET_WARN_PCT: f64 = 0.80;
const KEY_ROTATE_DAYS: u64 = 90;

const VIEWPORTS: &[(&str, f32, f32, bool)] = &[
    ("phone-360 LTR", 360.0, 800.0, false),
    ("phone-360 RTL", 360.0, 800.0, true),
    ("tablet-768 LTR", 768.0, 1024.0, false),
    ("tablet-768 RTL", 768.0, 1024.0, true),
    ("desktop-1280 LTR", 1280.0, 800.0, false),
    ("desktop-1280 RTL", 1280.0, 800.0, true),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetroFinding {
    pub tag: &'static str,
    pub area: &'static str,
    pub subject: String,
    pub message: String,
    pub hint: String,
}

impl RetroFinding {
    pub fn json(&self) -> Json {
        json!({
            "tag": self.tag,
            "area": self.area,
            "subject": self.subject,
            "message": self.message,
            "hint": self.hint,
        })
    }
}

pub struct RetroReport {
    pub app: String,
    pub app_sha256: String,
    pub findings: Vec<RetroFinding>,
    pub bench: Option<Json>,
    pub live_checked: bool,
    pub data_checked: bool,
}

impl RetroReport {
    pub fn json(&self) -> Json {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for f in &self.findings {
            *counts.entry(f.tag).or_insert(0) += 1;
        }
        json!({
            "app": self.app,
            "appSha256": self.app_sha256,
            "liveChecked": self.live_checked,
            "dataChecked": self.data_checked,
            "summary": counts,
            "bench": self.bench,
            "findings": self.findings.iter().map(RetroFinding::json).collect::<Vec<_>>(),
        })
    }

    pub fn markdown(&self) -> String {
        let short = &self.app_sha256[..self.app_sha256.len().min(12)];
        let mut out = format!("# Retro: {} (`{short}`)\n\n", self.app);
        if self.findings.is_empty() {
            out.push_str("No actionable findings (`fix`, `optimize`, `prune`, `rotate`).\n");
            return out;
        }
        for tag in ["fix", "optimize", "prune", "rotate"] {
            let group: Vec<&RetroFinding> = self.findings.iter().filter(|f| f.tag == tag).collect();
            if group.is_empty() {
                continue;
            }
            let _ = writeln!(out, "## [{tag}] ({})\n", group.len());
            for f in group {
                let _ = writeln!(out, "- **{}** (`{}`): {}", f.subject, f.area, f.message);
                if !f.hint.is_empty() {
                    let _ = writeln!(out, "  - *Action:* {}", f.hint);
                }
            }
            out.push('\n');
        }
        out
    }
}

/// CLI entry point: `tj retro <app.tj> [--target <url>] [--data <dir>] [--json]`.
pub fn run(args: &[String]) -> Result<(), String> {
    let usage = "usage: tj retro <app.tj> [--target <url>] [--data <dir>] [--json]";
    let mut file: Option<PathBuf> = None;
    let mut target: Option<String> = None;
    let mut data: Option<PathBuf> = None;
    let mut as_json = false;
    let mut it = args.iter().peekable();
    if let Some(first) = it.next_if(|a| !a.starts_with("--")) {
        file = Some(PathBuf::from(first));
    }
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--target" => target = Some(value()?.clone()),
            "--data" => data = Some(PathBuf::from(value()?)),
            "--json" => as_json = true,
            other => return Err(format!("unknown option {other}\n\n{usage}")),
        }
    }
    let file = file.ok_or_else(|| format!("`tj retro` needs an app file\n\n{usage}"))?;
    let report = analyze(&file, target.as_deref(), data.as_deref())?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&report.json()).expect("JSON serializes"));
    } else {
        print!("{}", report.markdown());
    }
    Ok(())
}

/// Runs static, layout, benchmark/budget, optional live-server, and optional data-dir checks.
pub fn analyze(file: &Path, target: Option<&str>, data: Option<&Path>) -> Result<RetroReport, String> {
    let name = file.display().to_string();
    let source = app::read(file)?.source;
    let program = tj_core::compile(&source).map_err(|errs| errs.iter().map(|d| d.render(&name, &source)).collect::<Vec<_>>().join("\n\n"))?;
    let manifest = manifest::Manifest::build(&source, &program, &[]);
    let app_sha256 = manifest.json["appSha256"].as_str().unwrap_or("").to_string();
    let mut findings = Vec::new();

    // 1. Access & privacy warnings (`fix`).
    for w in access::warnings(&source) {
        findings.push(RetroFinding {
            tag: "fix",
            area: "access",
            subject: name.clone(),
            message: w,
            hint: "review the object's @read / @allow / @pii rules (`tj check --access --privacy`)".into(),
        });
    }

    // 2. Dead private methods (`prune`).
    check_dead_methods(&source, &mut findings);

    // 3. Multi-viewport LTR & RTL layout sweep (`fix`).
    if program.app.is_some() {
        check_layouts(&source, &mut findings);
    }

    // 4. Benchmark & budget headroom (`optimize`).
    let bench_json = match bench::bench(&name, &source) {
        Ok(b) => {
            check_budget_headroom(file, &b, &mut findings);
            Some(b.to_json())
        }
        Err(e) => {
            findings.push(RetroFinding {
                tag: "fix",
                area: "runtime",
                subject: name.clone(),
                message: format!("benchmark run failed: {e}"),
                hint: "fix the boot/tick runtime error before deploying".into(),
            });
            None
        }
    };

    // 5. Optional live server telemetry (`/_tj/manifest` + `/_tj/inspect`, `ops:observe`).
    let mut live_checked = false;
    if let Some(url) = target {
        let base = url.trim_end_matches('/');
        let live_manifest = fetch_ops_json(&format!("{base}/_tj/manifest"))?;
        let live_inspect = fetch_ops_json(&format!("{base}/_tj/inspect"))?;
        analyze_live(&live_manifest, &live_inspect, &mut findings);
        live_checked = true;
    }

    // 6. Local data & key hygiene.
    let data_dir = data.map(Path::to_path_buf).or_else(|| {
        let default = file.parent().unwrap_or(Path::new(".")).join(".tj-data");
        default.exists().then_some(default)
    });
    let mut data_checked = false;
    if let Some(dir) = data_dir.as_deref() {
        let has_pii = program.objects.iter().any(|o| !o.pii.is_empty());
        check_data_hygiene(dir, has_pii, &mut findings);
        data_checked = true;
    }

    Ok(RetroReport { app: name, app_sha256, findings, bench: bench_json, live_checked, data_checked })
}

/// Flags `private` object methods (not `@every` or `@invariant`) never referenced in the bundle.
pub(crate) fn check_dead_methods(source: &str, out: &mut Vec<RetroFinding>) {
    let mut private_methods: Vec<(String, String)> = Vec::new();
    let mut refs: HashSet<String> = HashSet::new();
    for f in modules::files(source) {
        let Ok(parsed) = parse_range(source, f.start, f.end) else { continue };
        for item in &parsed.items {
            match item {
                Item::Object(o) => {
                    for fl in &o.fields {
                        if let Some(e) = &fl.init {
                            collect_expr_refs(e, &mut refs);
                        }
                    }
                    for m in &o.methods {
                        let special = m.decorators.iter().any(|d| matches!(d.name.as_str(), "every" | "invariant"));
                        if m.private && !special {
                            private_methods.push((o.name.clone(), m.func.name.clone()));
                        }
                        collect_stmts_refs(&m.func.body, &mut refs);
                    }
                }
                Item::Function(fn_decl) => collect_stmts_refs(&fn_decl.body, &mut refs),
                Item::Let(d) => {
                    if let Some(e) = &d.init {
                        collect_expr_refs(e, &mut refs);
                    }
                }
                Item::TypeAlias { .. } => {}
            }
        }
    }
    for (cls, method) in private_methods {
        if !refs.contains(&method) {
            out.push(RetroFinding {
                tag: "prune",
                area: "dead-code",
                subject: format!("{cls}.{method}"),
                message: format!("private method {cls}.{method} is never called or scheduled"),
                hint: format!("delete unused private method `{method}` to keep the bundle small"),
            });
        }
    }
}

fn collect_stmts_refs(stmts: &[Stmt], refs: &mut HashSet<String>) {
    for s in stmts {
        match s {
            Stmt::Let(d) => {
                if let Some(e) = &d.init {
                    collect_expr_refs(e, refs);
                }
            }
            Stmt::Function(f) => collect_stmts_refs(&f.body, refs),
            Stmt::Expr(e) | Stmt::Throw(e, _) => collect_expr_refs(e, refs),
            Stmt::Return(Some(e), _) => collect_expr_refs(e, refs),
            Stmt::Return(None, _) | Stmt::Break(_) | Stmt::Continue(_) => {}
            Stmt::If { cond, then, els } => {
                collect_expr_refs(cond, refs);
                collect_stmts_refs(then, refs);
                if let Some(els) = els {
                    collect_stmts_refs(els, refs);
                }
            }
            Stmt::While { cond, body } => {
                collect_expr_refs(cond, refs);
                collect_stmts_refs(body, refs);
            }
            Stmt::ForOf { iter, body, .. } => {
                collect_expr_refs(iter, refs);
                collect_stmts_refs(body, refs);
            }
            Stmt::For { init, cond, update, body } => {
                if let Some(i) = init {
                    collect_stmts_refs(std::slice::from_ref(i.as_ref()), refs);
                }
                if let Some(c) = cond {
                    collect_expr_refs(c, refs);
                }
                if let Some(u) = update {
                    collect_expr_refs(u, refs);
                }
                collect_stmts_refs(body, refs);
            }
            Stmt::Block(b) => collect_stmts_refs(b, refs),
            Stmt::Try { body, handler, .. } => {
                collect_stmts_refs(body, refs);
                collect_stmts_refs(handler, refs);
            }
        }
    }
}

fn collect_expr_refs(e: &Expr, refs: &mut HashSet<String>) {
    match &e.kind {
        ExprKind::Member { obj, name, .. } => {
            refs.insert(name.clone());
            collect_expr_refs(obj, refs);
        }
        ExprKind::Str(s) => {
            // `this.schedule(ms, "methodName", ...)` / `invoke(..., "methodName", ...)`.
            refs.insert(s.clone());
        }
        ExprKind::Call { callee, args, .. } => {
            collect_expr_refs(callee, refs);
            for a in args {
                collect_expr_refs(a, refs);
            }
        }
        ExprKind::Index { obj, index, .. } => {
            collect_expr_refs(obj, refs);
            collect_expr_refs(index, refs);
        }
        ExprKind::Binary(_, l, r) | ExprKind::Logical(_, l, r) => {
            collect_expr_refs(l, refs);
            collect_expr_refs(r, refs);
        }
        ExprKind::Assign { target, value, .. } => {
            collect_expr_refs(target, refs);
            collect_expr_refs(value, refs);
        }
        ExprKind::Unary(_, x) | ExprKind::Await(x) | ExprKind::Update { target: x, .. } => collect_expr_refs(x, refs),
        ExprKind::Cond(c, a, b) => {
            collect_expr_refs(c, refs);
            collect_expr_refs(a, refs);
            collect_expr_refs(b, refs);
        }
        ExprKind::Array(xs) => {
            for x in xs {
                match x {
                    ArrayEntry::Item(ex) | ArrayEntry::Spread(ex) => collect_expr_refs(ex, refs),
                }
            }
        }
        ExprKind::Record(fs) => {
            for entry in fs {
                match entry {
                    RecordEntry::Field(_, v) | RecordEntry::Spread(v) => collect_expr_refs(v, refs),
                    RecordEntry::Computed(k, v) => {
                        collect_expr_refs(k, refs);
                        collect_expr_refs(v, refs);
                    }
                }
            }
        }
        ExprKind::Template(parts) => {
            for p in parts {
                if let TplPiece::Expr(ex) = p {
                    collect_expr_refs(ex, refs);
                }
            }
        }
        ExprKind::Arrow(f) => collect_stmts_refs(&f.body, refs),
        ExprKind::Jsx(el) => collect_jsx_refs(el, refs),
        ExprKind::Num(_) | ExprKind::Bool(_) | ExprKind::Null | ExprKind::Ident(_) | ExprKind::This => {}
    }
}

fn collect_jsx_refs(el: &JsxElement, refs: &mut HashSet<String>) {
    for (_, v) in &el.attrs {
        collect_expr_refs(v, refs);
    }
    for ch in &el.children {
        match ch {
            JsxChild::Expr(ex) => collect_expr_refs(ex, refs),
            JsxChild::Element(sub) => collect_jsx_refs(sub, refs),
            JsxChild::Text(_) => {}
        }
    }
}

fn check_layouts(source: &str, out: &mut Vec<RetroFinding>) {
    // Group identical (check, node, message) findings across viewports so reports stay concise.
    let mut grouped: BTreeMap<(String, String), (review::Severity, Vec<&'static str>)> = BTreeMap::new();
    for &(label, w, h, rtl) in VIEWPORTS {
        match test_runner::layout_findings(source, w, h, rtl) {
            Ok(list) => {
                for f in list {
                    let key = (format!("{} {}", f.check, f.node), f.message);
                    let entry = grouped.entry(key).or_insert((f.severity, Vec::new()));
                    if !entry.1.contains(&label) {
                        entry.1.push(label);
                    }
                }
            }
            Err(e) => {
                out.push(RetroFinding {
                    tag: "fix",
                    area: "layout",
                    subject: label.into(),
                    message: format!("layout sweep failed: {e}"),
                    hint: "ensure the app boots cleanly for UI layout".into(),
                });
                break;
            }
        }
    }
    for ((subject, message), (sev, vps)) in grouped {
        let level = if sev == review::Severity::Error { "error" } else { "warning" };
        out.push(RetroFinding {
            tag: "fix",
            area: "layout",
            subject,
            message: format!("[{level}] {message} (viewports: {})", vps.join(", ")),
            hint: "fix layout/accessibility/contrast issue and re-verify with `tj test` `assertLayout` or `tj review`".into(),
        });
    }
}

fn check_budget_headroom(file: &Path, b: &bench::Bench, out: &mut Vec<RetroFinding>) {
    let budget_path = Path::new("scripts/budget.json");
    let Ok(text) = std::fs::read_to_string(budget_path) else { return };
    let Ok(budget) = serde_json::from_str::<Json>(&text) else { return };
    let norm = file.to_string_lossy().replace('\\', "/");
    let apps = budget["apps"].as_object();
    if let Some((_, lim)) = apps.and_then(|m| m.iter().find(|(k, _)| norm.ends_with(k.as_str()))) {
        for (metric, actual) in [
            ("vm_steps", b.vm_steps as f64),
            ("patch_ops", b.patch_ops as f64),
            ("renders", b.renders as f64),
            ("boot_ms", b.boot_ms),
            ("tick_ms", b.tick_ms),
        ] {
            if let Some(cap) = lim[metric].as_f64().filter(|&c| c > 0.0) {
                let pct = actual / cap;
                if pct >= BUDGET_WARN_PCT {
                    out.push(RetroFinding {
                        tag: "optimize",
                        area: "budget",
                        subject: format!("{norm} ({metric})"),
                        message: format!("{metric} is at {:.0}% of budget ({actual} / {cap})", pct * 100.0),
                        hint: "trim render/tick work before hitting the scripts/budget.json ceiling".into(),
                    });
                }
            }
        }
    }
}

/// Analyzes live `/_tj/manifest` + `/_tj/inspect` JSON snapshots (also unit-testable offline).
pub(crate) fn analyze_live(manifest: &Json, inspect: &Json, out: &mut Vec<RetroFinding>) {
    let empty = Map::new();
    let health = &manifest["health"];
    if health["autoRollbacks"].as_u64().unwrap_or(0) > 0 {
        out.push(RetroFinding {
            tag: "fix",
            area: "runtime",
            subject: "auto-rollback".into(),
            message: format!("server triggered {} automatic rollback(s) since boot", health["autoRollbacks"]),
            hint: "inspect audit logs and server errors for the rejected release before re-deploying".into(),
        });
    }
    if health["auditDropped"].as_u64().unwrap_or(0) > 0 {
        out.push(RetroFinding {
            tag: "fix",
            area: "security",
            subject: "audit".into(),
            message: format!("{} audit log entries were dropped", health["auditDropped"]),
            hint: "check disk space and audit writer health on the server".into(),
        });
    }

    let counted = inspect["stats"]["methods"].as_object().unwrap_or(&empty);
    let timed = inspect["server"]["methods"].as_object().unwrap_or(&empty);
    let names: BTreeSet<&str> = counted.keys().chain(timed.keys()).map(String::as_str).collect();
    for name in names {
        let c = counted.get(name);
        let t = timed.get(name);
        let calls = c.and_then(|v| v["calls"].as_u64()).unwrap_or(0);
        let errors = c.and_then(|v| v["errors"].as_u64()).unwrap_or(0);
        let refused = c.and_then(|v| v["refused"].as_u64()).unwrap_or(0);
        let steps = c.and_then(|v| v["steps"].as_u64()).unwrap_or(0);
        let failed = t.and_then(|v| v["failed"].as_u64()).unwrap_or(0);
        let p95 = t.and_then(|v| v["p95Ms"].as_f64()).unwrap_or(0.0);

        if errors > 0 || failed > 0 {
            out.push(RetroFinding {
                tag: "fix",
                area: "runtime",
                subject: name.into(),
                message: format!("{name} recorded {errors} core error(s) / {failed} failed HTTP call(s)"),
                hint: "add a deterministic .trace.jsonl regression test and fix the failing path".into(),
            });
        }
        if refused > 0 {
            out.push(RetroFinding {
                tag: "fix",
                area: "access",
                subject: name.into(),
                message: format!("{name} had {refused} access refusal(s) (403)"),
                hint: "check whether UI is hiding controls for unauthorized roles or a client has stale credentials".into(),
            });
        }
        let steps_per_call = steps.checked_div(calls).unwrap_or(0);
        if p95 >= SLOW_P95_MS || steps_per_call >= HEAVY_STEPS_PER_CALL {
            out.push(RetroFinding {
                tag: "optimize",
                area: "runtime",
                subject: name.into(),
                message: format!("{name} is hot (p95 {p95} ms, {steps_per_call} steps/call across {calls} calls)"),
                hint: "reduce per-call scans or split large object collections".into(),
            });
        }
    }

    // `@exclusive` contention spikes.
    for (class, wait) in inspect["stats"]["waiting"].as_object().unwrap_or(&empty) {
        let n = wait.as_u64().unwrap_or(0);
        if n > 0 {
            out.push(RetroFinding {
                tag: "optimize",
                area: "runtime",
                subject: class.clone(),
                message: format!("{n} call(s) currently waiting on @exclusive {class} lock(s)"),
                hint: "keep slow `await fetch` calls outside `@exclusive` locks or shard per-entity objects".into(),
            });
        }
    }

    // Objects nearing `maxObjectBytes` cap (>= 70%).
    let max_bytes_cap = health["maxObjectBytes"].as_u64().unwrap_or(0);
    if max_bytes_cap > 0 {
        for (class, sz) in inspect["stats"]["maxBytes"].as_object().unwrap_or(&empty) {
            let bytes = sz.as_u64().unwrap_or(0);
            let ratio = bytes as f64 / max_bytes_cap as f64;
            if ratio >= OBJECT_BYTES_WARN_PCT {
                out.push(RetroFinding {
                    tag: "optimize",
                    area: "storage",
                    subject: class.clone(),
                    message: format!("{class} largest stored record is {bytes} bytes ({:.0}% of {max_bytes_cap}-byte cap)", ratio * 100.0),
                    hint: "prune bounded lists, add @retain, or split per-item objects before hitting 413".into(),
                });
            }
        }
    }
}

fn check_data_hygiene(data: &Path, has_pii: bool, out: &mut Vec<RetroFinding>) {
    if data.join("audit").exists() {
        if let Err(e) = audit::verify(data, None) {
            out.push(RetroFinding {
                tag: "fix",
                area: "security",
                subject: data.join("audit").display().to_string(),
                message: format!("audit log verification failed: {e}"),
                hint: "investigate log tampering or truncated files (`tj audit verify --data <dir>`)".into(),
            });
        }
    }
    let audit_key = audit::key_path(data);
    if audit_key.exists() && !audit::kept_apart(&audit_key, data) {
        out.push(RetroFinding {
            tag: "rotate",
            area: "security",
            subject: audit_key.display().to_string(),
            message: "audit signing key sits inside the data directory".into(),
            hint: "move the audit key outside `<data>` and set `TJ_AUDIT_KEY_FILE`".into(),
        });
    }
    if has_pii && std::env::var_os(sealed::KEY_FILE_ENV).filter(|v| !v.is_empty()).is_none() {
        out.push(RetroFinding {
            tag: "fix",
            area: "security",
            subject: data.display().to_string(),
            message: "app declares @pii fields but TJ_KEY_FILE is not set".into(),
            hint: "set `TJ_KEY_FILE` outside `<data>` and run `tj store rekey --data <dir>` with the server stopped".into(),
        });
    }
    if let Some(keys_path) = std::env::var_os(crate::identity::KEYS_ENV).filter(|v| !v.is_empty()) {
        let path = Path::new(&keys_path);
        if let Ok(age) = std::fs::metadata(path).and_then(|m| m.modified()).and_then(|t| SystemTime::now().duration_since(t).map_err(|_| std::io::Error::other("clock"))) {
            if age > Duration::from_secs(KEY_ROTATE_DAYS * 86_400) {
                out.push(RetroFinding {
                    tag: "rotate",
                    area: "security",
                    subject: path.display().to_string(),
                    message: format!("API keys file has not been rotated in {} days", age.as_secs() / 86_400),
                    hint: "issue replacement keys with `tj key` and remove old hashes from TJ_KEYS".into(),
                });
            }
        }
    }
}

fn fetch_ops_json(url: &str) -> Result<Json, String> {
    let mut req = ureq::get(url);
    if let Some(h) = auth::client_header() {
        req = req.set("Authorization", &h);
    }
    let resp = match req.call() {
        Ok(r) | Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(format!("cannot reach {url}: {e}")),
    };
    let text = resp.into_string().map_err(|e| e.to_string())?;
    let body: Json = serde_json::from_str(&text).map_err(|e| format!("bad reply from {url}: {e}"))?;
    if let Some(e) = body["error"].as_str() {
        return Err(format!("{url}: {e}"));
    }
    Ok(body["ok"].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dead_methods_layout_sweep_and_live_telemetry_produce_actionable_tags() {
        let src = r#"
@server @read("anyone") object Box {
  n: number = 0
  @allow("anyone") bump() { this.used(); this.schedule(100, "scheduled") }
  private used() { this.n += 1 }
  private scheduled() { this.n += 1 }
  private orphan() { this.n += 99 }
  @every("10s") private tick() {}
  @invariant ok(): boolean { return this.n >= 0 }
}
function App() {
  return <Column padding={16}>
    <Button onPress={() => {}}></Button>
  </Column>
}
"#;
        let mut findings = Vec::new();
        check_dead_methods(src, &mut findings);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].tag, "prune");
        assert_eq!(findings[0].subject, "Box.orphan");

        // Multi-viewport layout sweep catches the unlabelled button across all 6 viewports!
        check_layouts(src, &mut findings);
        let layout_f = findings.iter().find(|f| f.area == "layout").expect("layout finding");
        assert_eq!(layout_f.tag, "fix");
        assert!(layout_f.message.contains("phone-360 LTR") && layout_f.message.contains("desktop-1280 RTL"), "{}", layout_f.message);

        // Live telemetry flags failing methods, slow methods, exclusive waits, byte-cap >= 70%, and auto-rollbacks.
        let manifest = json!({
            "health": {
                "maxObjectBytes": 1000,
                "autoRollbacks": 1,
                "auditDropped": 2,
            }
        });
        let inspect = json!({
            "stats": {
                "methods": {
                    "Box.bump": { "calls": 10, "errors": 2, "refused": 1, "steps": 600000 }
                },
                "waiting": { "Box": 3 },
                "maxBytes": { "Box": 800 }
            },
            "server": {
                "methods": {
                    "Box.bump": { "count": 10, "failed": 2, "p95Ms": 350.0 }
                }
            }
        });
        let mut live = Vec::new();
        analyze_live(&manifest, &inspect, &mut live);
        let areas_tags: Vec<(&str, &str)> = live.iter().map(|f| (f.tag, f.area)).collect();
        assert!(areas_tags.contains(&("fix", "runtime")), "{live:?}");
        assert!(areas_tags.contains(&("fix", "security")), "{live:?}");
        assert!(areas_tags.contains(&("fix", "access")), "{live:?}");
        assert!(areas_tags.contains(&("optimize", "runtime")), "{live:?}");
        assert!(areas_tags.contains(&("optimize", "storage")), "{live:?}");
    }
}

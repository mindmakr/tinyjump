//! `tj`: the tinyjump command line.

mod access;
mod app;
mod audit;
mod auth;
mod bench;
mod build;
mod bytes;
mod compat;
mod compliance;
mod engine;
mod http;
mod identity;
mod leases;
mod mailbox;
mod manifest;
mod plugin_server;
mod plugins;
mod privacy;
mod registry;
mod remotes;
mod retro;
mod stats;
mod mcp;
mod review;
mod sealed;
mod store;
mod test_runner;
mod webhooks;

use engine::Engine;

/// The language reference (docs/language.md), for agents: `tj docs` and the MCP `language` tool.
pub const LANGUAGE: &str = include_str!("../../../docs/language.md");
use http::{Dev, Web};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

const USAGE: &str = "\
tj: tinyjump

USAGE:
  tj docs                        print the language reference: load it before writing app code
  tj check <app.tj> [--manifest [--json]] [--against <old.tj> [--allow-breaking]] [--access] [--privacy] [--compliance <pack.json>] [--min-trust community|verified|official]
                                 compile and report errors; --manifest prints the canonical app contract
                                 (hash, objects, rules, personal data, plugins, warnings; JSON with --json),
                                 --against compares schemas, outbox signatures and rules against an older app,
                                 --access prints who may read and call what, --privacy the personal data
                                 the app holds and for what (with --record: GDPR article 30 Markdown),
                                 --compliance checks the app against a compliance pack, and --min-trust
                                 enforces locked registry package trust marks (`tj.lock`)
  tj add <name> [--registry <url-or-dir>] [--dir <app-dir>] [--min-trust community|verified|official]
                                 install a package from the registry into `.tj-packages/` and pin its
                                 commit and SHA-256 in `tj.lock` after verifying declared permissions
  tj registry check <manifest.json> [--index <dir>] [--source <file>] [--local]
                                 validate a registry manifest (schema, license, typosquats, commit/hash pin,
                                 static permission audit)
  tj bench <app.tj> [--json]     time compiling, booting and a minute of running; count VM steps and patches
  tj bench --budget <file>       fail if apps or builds are slower or bigger than the budget (scripts/budget.sh)
  tj build <app.tj> [--out <path>] [--web | --linux | --android | --ios] [--wasm <file>] [--runner <file>] [--assemble] [--json]
                                 compile AOT bytecode (.tjb) or package a static web bundle, a standalone
                                 executable with embedded bytecode, or Android/iOS AOT asset bundles
  tj test  <app.tj> [trace...]   run deterministic .trace.jsonl tests in memory (controllable clock,
                                 fake plugins/HTTP, access & @pii checks, Taffy layout review, restarts)
  tj serve <app.tj> [options]    run the backend: objects, timers, HTTP API, realtime events
  tj dev   <app.tj> [options]    serve + the web host with live reload
  tj review [--target <url>]     check a running app for layout, accessibility and contrast problems
  tj stats  [--target <url>]     a running app's methods: calls, errors, refusals, VM steps and latency
  tj privacy export|erase <person id> [--target <url>]
                                 everything a running server holds about a person, or erase it
  tj lease list|acquire|release  view, acquire or release operational work leases on a running server
  tj reload [<app.tj>] [--target <url>] [--trace <file.trace.jsonl>]... [--allow-breaking] [--watch-secs <n>] [--error-threshold <n>]
                                 shadow-boot, validate invariants/smoke traces and live-reload a running server
  tj rollback [--target <url>]   roll back a running server to its previous known-good bundle
  tj retro <app.tj> [--target <url>] [--data <dir>] [--json]
                                 actionable health & retro report ([fix], [optimize], [prune], [rotate]):
                                 dead private methods, access warnings, 6-viewport LTR/RTL layout sweep,
                                 benchmark headroom, live telemetry and audit/key hygiene
  tj mcp    [--target <url>] [--record <trace.jsonl>]
                                 MCP server (stdio) so coding agents can inspect, drive, record and test an app
  tj plugin ai | fake <file.json>
                                 run a plugin that ships with tinyjump as an MCP server (tj serve starts them)
  tj key <name> --keys <file> [--role <r>]... [--ops observe|test|maintain]
                                 make an API key (printed once; only its hash is stored in the file)
  tj store rekey --data <dir>    with the server stopped: encrypt a folder's plain records, and re-wrap
                                 sealed ones under the newest key in TJ_KEY_FILE
  tj audit verify --data <dir> [--checkpoints <copy>]
                                 check a server's audit log; with a copy of its checkpoints kept
                                 elsewhere, also catch a history rewritten by someone with the key

OPTIONS:
  --port <n>      port to listen on (default 4700)
  --host <addr>   address to bind (default 127.0.0.1; use 0.0.0.0 to expose)
  --data <dir>    where objects are stored (default: .tj-data next to the app)
  --wasm <file>   web runtime for `dev` (default: the workspace's `cargo web` output)
  --target <url>  app automation endpoint (default http://127.0.0.1:4700; Android/iOS debug: :4701)
  --plugin <name=fake:file.json>
                  run plugin <name> as a scripted fake (tests, demos without keys)
  --compliance <pack.json>
                  `serve`: refuse to start while a control of the pack is not met
  --min-trust <community|verified|official>
                  `check` / `serve` / `add`: refuse locked packages below this trust mark or flagged
  --web           `serve`: also serve the web app, with a sign-in bar for API keys
  --as <name:role,role>
                  `dev` only: requests without credentials act as this user (dev:<name>)

ENVIRONMENT (tj serve):
  TJ_ISSUER       sign-in provider (OpenID Connect issuer URL); TJ_AUDIENCE: the app's client id
  TJ_ROLES_CLAIM  where tokens carry roles, e.g. realm_access.roles (none by default)
  TJ_KEYS         file of API key hashes (see `tj key`, including `--ops observe|test|maintain`)
  TJ_ADMIN_KEY    full-access admin key for /_tj/* (24+ chars); prefer scoped `--ops` keys in TJ_KEYS
  TJ_OPS_KEY      client key sent by `tj review`, `tj stats`, `tj mcp`, `tj lease`, `tj privacy`,
                  `tj reload`, `tj rollback` and `tj retro` (takes precedence over TJ_ADMIN_KEY)
  TJ_MAX_OBJECTS  objects remote requests may load at once (default 100000)
  TJ_MAX_OBJECT_BYTES
                  maximum serialized JSON bytes per stored object (default 262144; 0 disables;
                  growth past the cap returns 413 while shrinking/erasing is always allowed)
  TJ_RATE_LIMIT   per-caller `/api/*` requests per minute on `tj serve` (default 600; 0 disables)
  TJ_TRUST_PROXY  1 behind a proxy: per-caller limits use X-Forwarded-For
  TJ_FETCH_ALLOW  comma-separated outbound fetch host allowlist (exact `api.example.com` or
                  strict subdomain `*.example.com`, enforced on redirects too)
  TJ_ALLOW_PRIVATE_FETCH
                  1 lets fetch reach loopback, private and link-local addresses (blocked on
                  tj serve, so app URLs and agent prompts cannot read internal services)
  TJ_AI_MAX_CALLS_HOUR / TJ_PLUGIN_MAX_CALLS_HOUR
                  hourly call budget for `tj:ai` or all server plugins (overridden per plugin
                  by `maxCallsPerHour` in `plugins.json`)
  TJ_TLS_TERMINATED
                  1 when a proxy in front terminates TLS. Without it, a server on an address
                  other machines can reach refuses requests that carry credentials
  TJ_KEY_FILE     encrypts stored objects: a file only the server can read, made on first use.
                  Back it up; add a line `k2 <64 hex>` and run `tj store rekey` to change keys
  TJ_AUDIT_KEY_FILE
                  the audit log's key (made on first start; default: audit.key in the data
                  folder). Keep it apart from the data, and back it up
  TJ_AUDIT_DAYS   days of audit log kept (default 365)
  TJ_REGISTRY     registry base URL or local directory for `tj add` (default: official index URL)
";

struct Args {
    command: String,
    file: Option<PathBuf>,
    port: u16,
    host: String,
    data: Option<PathBuf>,
    wasm: Option<PathBuf>,
    target: String,
    budget: Option<PathBuf>,
    json: bool,
    manifest: bool,
    against: Option<PathBuf>,
    allow_breaking: bool,
    access: bool,
    privacy: bool,
    record: bool,
    record_file: Option<PathBuf>,
    compliance: Option<PathBuf>,
    web: bool,
    as_user: Option<String>,
    keys: Option<PathBuf>,
    roles: Vec<String>,
    ops: Option<auth::OpsScope>,
    plugins: Vec<String>,
    checkpoints: Option<PathBuf>,
    min_trust: Option<registry::Trust>,
}

impl Args {
    fn file(&self) -> Result<&Path, String> {
        self.file.as_deref().ok_or_else(|| format!("`tj {}` needs an app file", self.command))
    }
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1).peekable();
    let command = it.next().ok_or("missing command")?;
    let file = it.next_if(|a| !a.starts_with("--")).map(PathBuf::from);
    let mut args = Args { command, file, port: 4700, host: "127.0.0.1".into(), data: None, wasm: None, target: "http://127.0.0.1:4700".into(), budget: None, json: false, manifest: false, against: None, allow_breaking: false, access: false, privacy: false, record: false, record_file: None, compliance: None, web: false, as_user: None, keys: None, roles: Vec::new(), ops: None, plugins: Vec::new(), checkpoints: None, min_trust: None };
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--port" => args.port = value()?.parse().map_err(|_| "--port must be a number")?,
            "--host" => args.host = value()?,
            "--data" => args.data = Some(PathBuf::from(value()?)),
            "--wasm" => args.wasm = Some(PathBuf::from(value()?)),
            "--target" => args.target = value()?,
            "--budget" => args.budget = Some(PathBuf::from(value()?)),
            "--json" => args.json = true,
            "--manifest" => args.manifest = true,
            "--against" => args.against = Some(PathBuf::from(value()?)),
            "--allow-breaking" => args.allow_breaking = true,
            "--access" => args.access = true,
            "--privacy" => args.privacy = true,
            "--record" => {
                if args.command == "mcp" {
                    args.record_file = Some(PathBuf::from(value()?));
                } else {
                    args.record = true;
                }
            }
            "--compliance" => args.compliance = Some(PathBuf::from(value()?)),
            "--web" => args.web = true,
            "--as" => args.as_user = Some(value()?),
            "--keys" => args.keys = Some(PathBuf::from(value()?)),
            "--role" => args.roles.push(value()?),
            "--ops" => args.ops = Some(auth::OpsScope::parse(&value()?)?),
            "--plugin" => args.plugins.push(value()?),
            "--checkpoints" => args.checkpoints = Some(PathBuf::from(value()?)),
            "--min-trust" => args.min_trust = Some(registry::Trust::parse(&value()?)?),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(args)
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().collect();
    // Standalone executable built with `tj build --linux`: boot the embedded AOT bytecode directly.
    match build::embedded_app() {
        Ok(Some(bytecode)) => {
            return match serve_embedded(&bytecode, &raw[1..]) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            };
        }
        Ok(None) => {}
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    }
    // `tj`, `tj --help`, `tj help` and `--help` after any command print the usage.
    if raw.iter().skip(1).any(|a| matches!(a.as_str(), "--version" | "-V" | "version")) {
        println!("tj {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if raw.len() < 2 || raw.iter().skip(1).any(|a| matches!(a.as_str(), "--help" | "-h" | "help")) {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if raw.get(1).map(String::as_str) == Some("privacy") {
        return match privacy::run(&raw[2..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }
    if raw.get(1).map(String::as_str) == Some("lease") {
        return match leases::run(&raw[2..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }
    if raw.get(1).map(String::as_str) == Some("build") {
        return match build::run(&raw[2..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }
    if raw.get(1).map(String::as_str) == Some("test") {
        return match test_runner::run(&raw[2..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }
    if raw.get(1).map(String::as_str) == Some("reload") {
        return match reload_cmd(&raw[2..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }
    if raw.get(1).map(String::as_str) == Some("rollback") {
        return match rollback_cmd(&raw[2..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }
    if raw.get(1).map(String::as_str) == Some("retro") {
        return match retro::run(&raw[2..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }
    if raw.get(1).map(String::as_str) == Some("add") {
        return match registry::run_add(&raw[2..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }
    if raw.get(1).map(String::as_str) == Some("registry") {
        return match registry::run_registry(&raw[2..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }
    if raw.get(1).map(String::as_str) == Some("plugin") {
        let Some(name) = raw.get(2) else {
            eprintln!("error: tj plugin needs a name (ai, fake)\n\n{USAGE}");
            return ExitCode::from(2);
        };
        return match plugin_server::run(name, raw.get(3).map(Path::new)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let result = match args.command.as_str() {
        "docs" => {
            print!("{LANGUAGE}");
            Ok(())
        }
        "check" => args.file().and_then(|f| check(f, args.manifest, args.json, args.against.as_deref(), args.allow_breaking, args.access, args.privacy, args.record, args.compliance.as_deref(), args.min_trust)),
        "key" => new_key(&args),
        "bench" => run_bench(&args),
        "serve" => serve(&args, false),
        "dev" => serve(&args, true),
        "review" => review(&args.target),
        "stats" => inspect(&args.target).map(|s| print!("{}", stats::report(&s))),
        "audit" => audit_verify(&args),
        "store" => store_rekey(&args),
        "mcp" => mcp::run(&args.target, args.record_file),
        other => Err(format!("unknown command {other}\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// Prints the review of a running app; fails if it found errors, so CI can gate on it.
fn review(target: &str) -> Result<(), String> {
    let snapshot = inspect(target)?;
    let findings = review::review(&snapshot);
    print!("{}", review::report(&snapshot, &findings));
    if findings.iter().any(|f| f.severity == review::Severity::Error) {
        return Err("review found errors".into());
    }
    Ok(())
}

/// A running app's snapshot, from its automation endpoint.
fn inspect(target: &str) -> Result<serde_json::Value, String> {
    let url = format!("{}/_tj/inspect", target.trim_end_matches('/'));
    let mut req = ureq::get(&url);
    if let Some(h) = auth::client_header() {
        req = req.set("Authorization", &h);
    }
    let body = match req.call() {
        Ok(r) | Err(ureq::Error::Status(_, r)) => r.into_string().map_err(|e| e.to_string())?,
        Err(e) => return Err(format!("cannot reach {url}: {e}")),
    };
    let reply: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("bad reply from {url}: {e}"))?;
    if let Some(e) = reply["error"].as_str() {
        return Err(e.to_string());
    }
    Ok(reply["ok"].clone())
}

fn run_bench(args: &Args) -> Result<(), String> {
    if let Some(budget) = &args.budget {
        return bench::check_budget(budget);
    }
    let file = args.file()?;
    let name = file.display().to_string();
    let b = bench::bench(&name, &read_source(file)?)?;
    if args.json {
        println!("{}", b.to_json());
    } else {
        print!("{}", b.report(&name));
    }
    Ok(())
}

/// The app as one source: the file and every file it imports.
fn read_source(file: &Path) -> Result<String, String> {
    app::read(file).map(|a| a.source)
}

#[allow(clippy::too_many_arguments)]
fn check(file: &Path, show_manifest: bool, as_json: bool, against: Option<&Path>, allow_breaking: bool, access: bool, privacy: bool, record: bool, pack: Option<&Path>, min_trust: Option<registry::Trust>) -> Result<(), String> {
    if as_json && show_manifest && against.is_some() {
        return Err("pass either --manifest --json or --against <old.tj> --json, not both".into());
    }
    let src = read_source(file)?;
    let name = file.display().to_string();
    let app_dir = file.parent().unwrap_or(Path::new("."));
    match tj_core::compile(&src) {
        Ok(program) => {
            if show_manifest && as_json {
                let (_, warnings) = registry::check_lockfile(app_dir, min_trust)?;
                for w in warnings {
                    eprintln!("warning: {w}");
                }
                let configured = plugins::load(file, &[]).map(|p| p.names().into_iter().map(str::to_string).collect::<Vec<_>>()).unwrap_or_default();
                let refs: Vec<&str> = configured.iter().map(String::as_str).collect();
                let m = manifest::Manifest::build(&src, &program, &refs);
                println!("{}", serde_json::to_string_pretty(&m.json).expect("JSON serializes"));
                return Ok(());
            }
            if let Some(old_file) = against {
                let findings = if old_file.extension().and_then(|e| e.to_str()) == Some("json") {
                    let raw = std::fs::read_to_string(old_file).map_err(|e| format!("cannot read {}: {e}", old_file.display()))?;
                    let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| format!("{} is not valid JSON: {e}", old_file.display()))?;
                    let old_snap = compat::Snapshot::from_manifest_json(v)?;
                    let new_snap = compat::Snapshot::build(&src, &program);
                    compat::check_snapshots(&old_snap, &new_snap)
                } else {
                    let old_src = read_source(old_file)?;
                    let old_name = old_file.display().to_string();
                    let old_prog = tj_core::compile(&old_src).map_err(|errs| errs.iter().map(|d| d.render(&old_name, &old_src)).collect::<Vec<_>>().join("\n\n"))?;
                    compat::check(&old_src, &old_prog, &src, &program)
                };
                if as_json {
                    let mut failures = Vec::new();
                    match registry::check_lockfile(app_dir, min_trust) {
                        Ok((_, warnings)) => {
                            for w in warnings {
                                eprintln!("warning: {w}");
                            }
                        }
                        Err(e) => failures.push(e),
                    }
                    let j = serde_json::json!({
                        "compatible": !compat::has_breaking(&findings),
                        "findings": findings.iter().map(compat::Finding::json).collect::<Vec<_>>(),
                    });
                    println!("{}", serde_json::to_string_pretty(&j).expect("JSON serializes"));
                    if !allow_breaking && compat::has_breaking(&findings) {
                        failures.push("breaking upgrade changes found (above); pass --allow-breaking if intentional".into());
                    }
                    return if failures.is_empty() { Ok(()) } else { Err(failures.join("\n")) };
                }
                println!("ok: {name}");
                print!("{}", compat::report(&findings));
                let mut failures = Vec::new();
                if !allow_breaking && compat::has_breaking(&findings) {
                    failures.push("breaking upgrade changes found (above); pass --allow-breaking if intentional".into());
                }
                if let Err(e) = print_check_reports(file, app_dir, &src, &program, show_manifest, access, privacy, record, pack, min_trust) {
                    failures.push(e);
                }
                return if failures.is_empty() { Ok(()) } else { Err(failures.join("\n")) };
            }
            println!("ok: {name}");
            print_check_reports(file, app_dir, &src, &program, show_manifest, access, privacy, record, pack, min_trust)
        }
        Err(errs) => Err(errs.iter().map(|d| d.render(&name, &src)).collect::<Vec<_>>().join("\n\n")),
    }
}

#[allow(clippy::too_many_arguments)]
fn print_check_reports(file: &Path, app_dir: &Path, src: &str, program: &tj_core::vm::program::Program, show_manifest: bool, access: bool, privacy: bool, record: bool, pack: Option<&Path>, min_trust: Option<registry::Trust>) -> Result<(), String> {
    let mut failures = Vec::new();
    match registry::check_lockfile(app_dir, min_trust) {
        Ok((lines, warnings)) => {
            if !lines.is_empty() {
                println!("Packages ({}):", registry::LOCKFILE_NAME);
                for line in lines {
                    println!("{line}");
                }
            }
            for w in warnings {
                eprintln!("warning: {w}");
            }
        }
        Err(e) => failures.push(e),
    }
    if show_manifest {
        let configured = plugins::load(file, &[]).map(|p| p.names().into_iter().map(str::to_string).collect::<Vec<_>>()).unwrap_or_default();
        let refs: Vec<&str> = configured.iter().map(String::as_str).collect();
        let m = manifest::Manifest::build(src, program, &refs);
        print!("{}", m.text());
    }
    if access {
        print!("{}", access::report(src));
    }
    let private_fetch = std::env::var(engine::PRIVATE_FETCH_ENV).is_ok_and(|v| v == "1");
    let fetch_allow = engine::fetch_allow_from_env()?;
    let remotes = match remotes::Remotes::load(file, program, false, private_fetch, &fetch_allow) {
        Ok(r) => r,
        Err(e) => {
            failures.push(e);
            remotes::Remotes::default()
        }
    };
    let remote_classes = remotes.classes();
    let remote_dests = remotes.destinations();
    let flow_errs = tj_core::check_remote_flows(src, &remote_dests);
    if !flow_errs.is_empty() {
        let name = file.display().to_string();
        for d in &flow_errs {
            failures.push(d.render(&name, src));
        }
    }
    let webhooks = match webhooks::Webhooks::load(file, program, &remote_classes, false, private_fetch, &fetch_allow) {
        Ok(w) => w,
        Err(e) => {
            failures.push(e);
            webhooks::Webhooks::default()
        }
    };
    let src_facts = tj_core::facts_with_remotes(src, &remote_dests);
    let wh_transfers = webhooks.pii_transfers(program, &src_facts.reveals);
    if privacy && record {
        print!("{}", privacy::record_with_transfers(src, program, &remote_dests, &wh_transfers));
    } else if privacy {
        print!("{}", privacy::report(program));
    }
    if let Some(pack) = pack {
        let pack = compliance::Pack::load(pack)?;
        let (report, ok) = pack.report(&pack.evaluate_with_transfers(src, program, None, &remote_dests, &wh_transfers));
        print!("{report}");
        if !ok {
            failures.push("some controls are not met (above)".into());
        }
    }
    if failures.is_empty() { Ok(()) } else { Err(failures.join("\n")) }
}

fn store_rekey(args: &Args) -> Result<(), String> {
    let usage = "usage: tj store rekey --data <dir>  (with TJ_KEY_FILE set, and the server stopped)";
    if args.file.as_deref() != Some(Path::new("rekey")) {
        return Err(usage.into());
    }
    print!("{}", sealed::rekey(args.data.as_deref().ok_or(usage)?)?);
    Ok(())
}

fn audit_verify(args: &Args) -> Result<(), String> {
    let usage = "usage: tj audit verify --data <dir> [--checkpoints <copy of checkpoints.jsonl>]";
    if args.file.as_deref() != Some(Path::new("verify")) {
        return Err(usage.into());
    }
    let data = args.data.as_deref().ok_or(usage)?;
    print!("{}", audit::verify(data, args.checkpoints.as_deref())?);
    Ok(())
}

fn new_key(args: &Args) -> Result<(), String> {
    let name = args.file.as_ref().map(|p| p.display().to_string()).ok_or("`tj key` needs a name: tj key <name> --keys keys.json [--role staff] [--ops observe|test|maintain]")?;
    let file = args.keys.as_deref().ok_or("`tj key` needs --keys <file>, the file TJ_KEYS will name")?;
    let key = identity::new_key(&name, &args.roles, args.ops, file)?;
    let ops = args.ops.map(|s| format!(" (ops:{})", s.as_str())).unwrap_or_default();
    eprintln!("Added key:{name}{ops} to {}. The key, shown only now:", file.display());
    println!("{key}");
    Ok(())
}

fn workspace_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn serve(args: &Args, dev_mode: bool) -> Result<(), String> {
    let file = args.file()?;
    let app_dir = file.parent().unwrap_or(Path::new("."));
    let (_, lock_warnings) = registry::check_lockfile(app_dir, args.min_trust)?;
    for w in lock_warnings {
        eprintln!("warning: {w}");
    }
    let source = read_source(file)?;
    let data = args.data.clone().unwrap_or_else(|| file.parent().unwrap_or(Path::new(".")).join(".tj-data"));
    let loopback = http::is_loopback(&args.host);
    if args.as_user.is_some() && !(dev_mode && loopback) {
        return Err("--as is for `tj dev` on this machine only (with --host 127.0.0.1)".into());
    }
    let identity = identity::Identity::new(args.as_user.as_deref())?;
    // In dev mode the browser runs the UI; the server keeps the backend objects. Dev also
    // explains refusals, which a real server must not.
    let plugins = Arc::new(plugins::load(file, &args.plugins)?);
    plugins.start_all()?;
    // Checked before anything starts: a folder that mixes plain and sealed records, or a key
    // file that cannot be read, stops the server here.
    let (_, storage) = sealed::open_store(&data, &[])?;
    let is_bytecode = source.starts_with(tj_core::vm::bytecode::BUNDLE_PREFIX);
    if is_bytecode && args.compliance.is_some() {
        return Err("--compliance requires .tj source for static privacy-flow analysis; run `tj check --compliance` before `tj build`".into());
    }
    let program = tj_core::load(&source).map_err(|e| e.iter().map(|d| d.render(&file.display().to_string(), &source)).collect::<Vec<_>>().join("\n\n"))?;
    let private_fetch = dev_mode || std::env::var(engine::PRIVATE_FETCH_ENV).is_ok_and(|v| v == "1");
    let fetch_allow = engine::fetch_allow_from_env()?;
    let remotes = Arc::new(remotes::Remotes::load(file, &program, true, private_fetch, &fetch_allow)?);
    let remote_classes = remotes.classes();
    let remote_dests = remotes.destinations();
    if is_bytecode && !remote_dests.is_empty() && program.objects.iter().any(|o| !o.pii.is_empty()) {
        return Err("serving a precompiled .tjb with both @pii and remotes.json is refused because static remote-flow checks need .tj source; run `tj serve <app.tj>`".into());
    }
    let flow_errs = if is_bytecode { Vec::new() } else { tj_core::check_remote_flows(&source, &remote_dests) };
    if !flow_errs.is_empty() {
        return Err(flow_errs.iter().map(|d| d.render(&file.display().to_string(), &source)).collect::<Vec<_>>().join("\n\n"));
    }
    let webhooks = Arc::new(webhooks::Webhooks::load(file, &program, &remote_classes, true, private_fetch, &fetch_allow)?);
    let compliance_gate = if let Some(pack) = &args.compliance {
        let pack = compliance::Pack::load(pack)?;
        let key_file = std::env::var_os(sealed::KEY_FILE_ENV).filter(|v| !v.is_empty()).map(PathBuf::from);
        let server = compliance::Server {
            encrypted: key_file.is_some(),
            encryption_key_apart: key_file.as_deref().is_some_and(|k| audit::kept_apart(k, &data)),
            audit_key_apart: audit::kept_apart(&audit::key_path(&data), &data),
            tls: !http::credentials_need_tls(dev_mode, &args.host, std::env::var(http::TLS_TERMINATED_ENV).ok().as_deref()),
        };
        let facts = tj_core::facts_with_remotes(&source, &remote_dests);
        let wh_transfers = webhooks.pii_transfers(&program, &facts.reveals);
        let (report, ok) = pack.report(&pack.evaluate_with_transfers(&source, &program, Some(&server), &remote_dests, &wh_transfers));
        if !ok {
            return Err(format!("{report}\nnot starting: some controls of the compliance pack are not met (above)"));
        }
        eprintln!("compliance: {} ({}): every checked control is met", pack.title, pack.names.join(" + "));
        Some((pack, server))
    } else {
        None
    };
    let (audit, audit_key) = audit::Audit::start(&data)?;
    let engine = Engine::start_with_integrations(
        source,
        data.clone(),
        false,
        dev_mode,
        plugins.clone(),
        audit.clone(),
        compliance_gate,
        remotes.clone(),
        webhooks.clone(),
    )?;
    let addr = format!("{}:{}", args.host, args.port);
    let automation = if dev_mode { auth::Automation::Open } else { auth::Automation::for_serve()? };
    // The web app: always under `tj dev`, and under `tj serve` with --web.
    let web = if dev_mode || args.web {
        let wasm = args.wasm.clone().unwrap_or_else(|| workspace_dir().join("target/wasm32-unknown-unknown/small/tj_wasm.wasm"));
        if !wasm.exists() {
            return Err(format!("web runtime not found at {}\nbuild it with: cargo web", wasm.display()));
        }
        Some(Arc::new(Web { source: file.to_path_buf(), web_dir: workspace_dir().join("hosts/web"), wasm }))
    } else {
        None
    };
    let dev = dev_mode.then(|| {
        let dev = Arc::new(Dev::default());
        watch(file.to_path_buf(), engine.clone(), dev.clone());
        dev
    });
    let leases = leases::Leases::open(&data);
    let names = plugins.names();
    let plugins = if names.is_empty() { "none".to_string() } else { names.join(", ") };
    eprintln!("tinyjump {} on http://{addr}  (data: {}; automation: {}; callers: {}; plugins: {plugins})", if dev_mode { "dev" } else { "server" }, data.display(), automation.describe(identity.has_keys()), identity.describe());
    let tls = http::credentials_need_tls(dev_mode, &args.host, std::env::var(http::TLS_TERMINATED_ENV).ok().as_deref());
    eprintln!("storage: {storage}\naudit log: {} (key: {audit_key})", data.join("audit").display());
    if tls {
        eprintln!("credentials: refused over plain HTTP on {}; behind a TLS proxy, set {}=1", args.host, http::TLS_TERMINATED_ENV);
    } else if !dev_mode && std::env::var(http::TLS_TERMINATED_ENV).is_ok_and(|v| v == "1") && std::env::var("TJ_TRUST_PROXY").ok().as_deref() != Some("1") {
        eprintln!("note: {}=1 is set without TJ_TRUST_PROXY=1; per-IP rate and stream limits will see the proxy address", http::TLS_TERMINATED_ENV);
    }
    http::serve(&addr, http::Front::new(engine, web, dev, automation, identity, audit, leases, tls).with_app_file(file.to_path_buf()).with_webhooks(webhooks))
}

fn serve_embedded(bytecode: &[u8], raw_args: &[String]) -> Result<(), String> {
    let mut port: u16 = 4700;
    let mut host = "127.0.0.1".to_string();
    let mut data: Option<PathBuf> = None;
    let mut info = false;
    let mut it = raw_args.iter();
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--info" => info = true,
            "--port" => port = value()?.parse().map_err(|_| "--port must be a number")?,
            "--host" => host = value()?.clone(),
            "--data" => data = Some(PathBuf::from(value()?)),
            "--help" | "-h" => {
                println!("Standalone tinyjump app (embedded AOT bytecode)\n\nOPTIONS:\n  --port <n>      port to listen on (default 4700)\n  --host <addr>   address to bind (default 127.0.0.1)\n  --data <dir>    where objects are stored (default: .tj-data)\n  --info          print embedded app metadata and exit");
                return Ok(());
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    let program = tj_core::vm::program::Program::from_bytes(bytecode)?;
    if info {
        let objects: Vec<&str> = program.objects.iter().map(|o| &*o.name).collect();
        println!("embedded AOT app: {} bytes bytecode, objects: [{}], ui: {}", bytecode.len(), objects.join(", "), program.app.is_some());
        return Ok(());
    }
    let source = tj_core::vm::bytecode::bundle_from_bytes(bytecode);
    let data = data.unwrap_or_else(|| PathBuf::from(".tj-data"));
    let identity = identity::Identity::new(None)?;
    let (_, storage) = sealed::open_store(&data, &[])?;
    let (audit, audit_key) = audit::Audit::start(&data)?;
    let plugins = Arc::new(plugins::Plugins::default());
    let remotes = Arc::new(remotes::Remotes::default());
    let webhooks = Arc::new(webhooks::Webhooks::default());
    let engine = Engine::start_with_integrations(source, data.clone(), false, false, plugins, audit.clone(), None, remotes, webhooks.clone())?;
    let addr = format!("{host}:{port}");
    let automation = auth::Automation::for_serve()?;
    let leases = leases::Leases::open(&data);
    let tls = http::credentials_need_tls(false, &host, std::env::var(http::TLS_TERMINATED_ENV).ok().as_deref());
    eprintln!("tinyjump standalone server on http://{addr} (data: {}; storage: {storage}; audit: {audit_key})", data.display());
    http::serve(&addr, http::Front::new(engine, None, None, automation, identity, audit, leases, tls).with_webhooks(webhooks))
}

fn reload_cmd(args: &[String]) -> Result<(), String> {
    let usage = "usage: tj reload [<app.tj>] [--target <url>] [--trace <file.trace.jsonl>]... [--allow-breaking] [--watch-secs <n>] [--error-threshold <n>]";
    let mut file: Option<PathBuf> = None;
    let mut target = "http://127.0.0.1:4700".to_string();
    let mut traces: Vec<PathBuf> = Vec::new();
    let mut allow_breaking = false;
    let mut watch_secs: Option<u64> = None;
    let mut error_threshold: Option<u64> = None;
    let mut it = args.iter().peekable();
    if let Some(first) = it.next_if(|a| !a.starts_with("--")) {
        file = Some(PathBuf::from(first));
    }
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--target" => target = value()?.clone(),
            "--trace" => traces.push(PathBuf::from(value()?)),
            "--allow-breaking" => allow_breaking = true,
            "--watch-secs" => watch_secs = Some(value()?.parse().map_err(|_| "--watch-secs must be a number")?),
            "--error-threshold" => error_threshold = Some(value()?.parse().map_err(|_| "--error-threshold must be a number")?),
            other => return Err(format!("unknown option {other}\n\n{usage}")),
        }
    }
    let mut body = serde_json::Map::new();
    if let Some(p) = &file {
        body.insert("source".into(), serde_json::json!(read_source(p)?));
    }
    if allow_breaking {
        body.insert("allowBreaking".into(), serde_json::json!(true));
    }
    if !traces.is_empty() {
        let list: Vec<serde_json::Value> = traces
            .iter()
            .map(|p| std::fs::read_to_string(p).map(serde_json::Value::String).map_err(|e| format!("cannot read {}: {e}", p.display())))
            .collect::<Result<_, _>>()?;
        body.insert("smokeTraces".into(), serde_json::Value::Array(list));
    }
    if let Some(w) = watch_secs {
        body.insert("watchSecs".into(), serde_json::json!(w));
    }
    if let Some(t) = error_threshold {
        body.insert("errorThreshold".into(), serde_json::json!(t));
    }
    let ok = post_ops(&target, "/_tj/reload", serde_json::Value::Object(body))?;
    println!("{}", serde_json::to_string_pretty(&ok).expect("JSON serializes"));
    Ok(())
}

fn rollback_cmd(args: &[String]) -> Result<(), String> {
    let usage = "usage: tj rollback [--target <url>]";
    let mut target = "http://127.0.0.1:4700".to_string();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--target" => target = it.next().ok_or("--target needs a value")?.clone(),
            other => return Err(format!("unknown option {other}\n\n{usage}")),
        }
    }
    let ok = post_ops(&target, "/_tj/rollback", serde_json::json!({}))?;
    println!("{}", serde_json::to_string_pretty(&ok).expect("JSON serializes"));
    Ok(())
}

fn post_ops(target: &str, path: &str, body: serde_json::Value) -> Result<serde_json::Value, String> {
    let base = target.trim_end_matches('/');
    let mut req = ureq::post(&format!("{base}{path}")).set("Content-Type", "application/json");
    if let Some(h) = auth::client_header() {
        req = req.set("Authorization", &h);
    }
    let resp = req.send_string(&body.to_string());
    let text = match resp {
        Ok(r) | Err(ureq::Error::Status(_, r)) => r.into_string().map_err(|e| e.to_string())?,
        Err(e) => return Err(format!("cannot reach {base}: {e}")),
    };
    let reply: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("bad reply from {base}: {e}"))?;
    if let Some(e) = reply["error"].as_str() {
        return Err(e.to_string());
    }
    Ok(reply["ok"].clone())
}

/// Polls the app's files; on a change, reloads the backend and tells browsers to reload.
fn watch(file: PathBuf, engine: Engine, dev: Arc<Dev>) {
    let stamps = |files: &[PathBuf]| -> Vec<Option<SystemTime>> { files.iter().map(|f| std::fs::metadata(f).and_then(|m| m.modified()).ok()).collect() };
    let spawned = thread::Builder::new().name("tj-watch".into()).spawn(move || {
        let initial = app::read(&file).ok();
        let mut prev_snap = initial.as_ref().and_then(|a| tj_core::compile(&a.source).ok().map(|p| compat::Snapshot::build(&a.source, &p)));
        let mut files = initial.map(|a| a.files).unwrap_or_else(|| vec![file.clone()]);
        let mut last = stamps(&files);
        loop {
            thread::sleep(Duration::from_millis(300));
            if stamps(&files) == last {
                continue;
            }
            match app::read(&file) {
                Ok(app) => {
                    match tj_core::compile(&app.source) {
                        Ok(new_prog) => {
                            let new_snap = compat::Snapshot::build(&app.source, &new_prog);
                            // Non-blocking upgrade check during `tj dev` iteration (`R-003.1`).
                            if let Some(old_snap) = &prev_snap {
                                let findings = compat::check_snapshots(old_snap, &new_snap);
                                if !findings.is_empty() {
                                    eprint!("[tj] upgrade notice:\n{}", compat::report(&findings));
                                }
                            }
                            prev_snap = Some(new_snap);
                            engine.reload(app.source);
                        }
                        Err(errs) => eprintln!("{}", errs.iter().map(|d| d.render(&file.display().to_string(), &app.source)).collect::<Vec<_>>().join("\n\n")),
                    }
                    files = app.files; // imports may have changed
                }
                Err(e) => eprintln!("[tj] {e}"),
            }
            last = stamps(&files);
            // Browsers reload either way: they show compile errors in their overlay.
            dev.set_inline_reloaded(false);
            dev.notify_reload();
        }
    });
    if let Err(e) = spawned {
        eprintln!("[tj] live reload is off: {e}");
    }
}

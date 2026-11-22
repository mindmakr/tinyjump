//! `tj`: the tinyjump command line.

mod access;
mod app;
mod audit;
mod auth;
mod bench;
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
mod stats;
mod mcp;
mod review;
mod sealed;
mod store;
mod test_runner;

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
  tj check <app.tj> [--manifest [--json]] [--against <old.tj> [--allow-breaking]] [--access] [--privacy] [--compliance <pack.json>]
                                 compile and report errors; --manifest prints the canonical app contract
                                 (hash, objects, rules, personal data, plugins, warnings; JSON with --json),
                                 --against compares schemas, outbox signatures and rules against an older app,
                                 --access prints who may read and call what, --privacy the personal data
                                 the app holds and for what (with --record: GDPR article 30 Markdown),
                                 and --compliance checks the app against a compliance pack
  tj bench <app.tj> [--json]     time compiling, booting and a minute of running; count VM steps and patches
  tj bench --budget <file>       fail if apps or builds are slower or bigger than the budget (scripts/budget.sh)
  tj test  <app.tj> [trace...]   run deterministic .trace.jsonl tests in memory (controllable clock,
                                 fake plugins/HTTP, access & @pii checks, Taffy layout review, restarts)
  tj serve <app.tj> [options]    run the backend: objects, timers, HTTP API, realtime events
  tj dev   <app.tj> [options]    serve + the web host with live reload
  tj review [--target <url>]     check a running app for layout, accessibility and contrast problems
  tj stats  [--target <url>]     a running app's methods: calls, errors, refusals, VM steps and latency
  tj privacy export|erase <person id> [--target <url>]
                                 everything a running server holds about a person, or erase it
  tj lease list|acquire|release  view, acquire or release operational work leases on a running server
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
  --web           `serve`: also serve the web app, with a sign-in bar for API keys
  --as <name:role,role>
                  `dev` only: requests without credentials act as this user (dev:<name>)

ENVIRONMENT (tj serve):
  TJ_ISSUER       sign-in provider (OpenID Connect issuer URL); TJ_AUDIENCE: the app's client id
  TJ_ROLES_CLAIM  where tokens carry roles, e.g. realm_access.roles (none by default)
  TJ_KEYS         file of API key hashes (see `tj key`, including `--ops observe|test|maintain`)
  TJ_ADMIN_KEY    full-access admin key for /_tj/* (24+ chars); prefer scoped `--ops` keys in TJ_KEYS
  TJ_OPS_KEY      client key sent by `tj review`, `tj stats`, `tj mcp`, `tj lease`, `tj privacy`
                  (takes precedence over TJ_ADMIN_KEY when set)
  TJ_MAX_OBJECTS  objects remote requests may load at once (default 100000)
  TJ_TRUST_PROXY  1 behind a proxy: per-caller limits use X-Forwarded-For
  TJ_ALLOW_PRIVATE_FETCH
                  1 lets fetch reach loopback, private and link-local addresses (blocked on
                  tj serve, so app URLs and agent prompts cannot read internal services)
  TJ_TLS_TERMINATED
                  1 when a proxy in front terminates TLS. Without it, a server on an address
                  other machines can reach refuses requests that carry credentials
  TJ_KEY_FILE     encrypts stored objects: a file only the server can read, made on first use.
                  Back it up; add a line `k2 <64 hex>` and run `tj store rekey` to change keys
  TJ_AUDIT_KEY_FILE
                  the audit log's key (made on first start; default: audit.key in the data
                  folder). Keep it apart from the data, and back it up
  TJ_AUDIT_DAYS   days of audit log kept (default 365)
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
    let mut args = Args { command, file, port: 4700, host: "127.0.0.1".into(), data: None, wasm: None, target: "http://127.0.0.1:4700".into(), budget: None, json: false, manifest: false, against: None, allow_breaking: false, access: false, privacy: false, record: false, record_file: None, compliance: None, web: false, as_user: None, keys: None, roles: Vec::new(), ops: None, plugins: Vec::new(), checkpoints: None };
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
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(args)
}

fn main() -> ExitCode {
    // `tj plugin <name> [file]` speaks MCP on stdio; it has its own arguments.
    let raw: Vec<String> = std::env::args().collect();
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
    if raw.get(1).map(String::as_str) == Some("test") {
        return match test_runner::run(&raw[2..]) {
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
        "check" => args.file().and_then(|f| check(f, args.manifest, args.json, args.against.as_deref(), args.allow_breaking, args.access, args.privacy, args.record, args.compliance.as_deref())),
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
fn check(file: &Path, show_manifest: bool, as_json: bool, against: Option<&Path>, allow_breaking: bool, access: bool, privacy: bool, record: bool, pack: Option<&Path>) -> Result<(), String> {
    let src = read_source(file)?;
    let name = file.display().to_string();
    match tj_core::compile(&src) {
        Ok(program) => {
            if let Some(old_file) = against {
                let old_src = read_source(old_file)?;
                let old_name = old_file.display().to_string();
                let old_prog = tj_core::compile(&old_src).map_err(|errs| errs.iter().map(|d| d.render(&old_name, &old_src)).collect::<Vec<_>>().join("\n\n"))?;
                let findings = compat::check(&old_src, &old_prog, &src, &program);
                if as_json {
                    let j = serde_json::json!({
                        "compatible": !compat::has_breaking(&findings),
                        "findings": findings.iter().map(compat::Finding::json).collect::<Vec<_>>(),
                    });
                    println!("{}", serde_json::to_string_pretty(&j).expect("JSON serializes"));
                } else {
                    println!("ok: {name}");
                    print!("{}", compat::report(&findings));
                }
                if !allow_breaking && compat::has_breaking(&findings) {
                    return Err("breaking upgrade changes found (above); pass --allow-breaking if intentional".into());
                }
                return Ok(());
            }
            if show_manifest && as_json {
                let configured = plugins::load(file, &[]).map(|p| p.names().into_iter().map(str::to_string).collect::<Vec<_>>()).unwrap_or_default();
                let refs: Vec<&str> = configured.iter().map(String::as_str).collect();
                let m = manifest::Manifest::build(&src, &program, &refs);
                println!("{}", serde_json::to_string_pretty(&m.json).expect("JSON serializes"));
                return Ok(());
            }
            println!("ok: {name}");
            if show_manifest {
                let configured = plugins::load(file, &[]).map(|p| p.names().into_iter().map(str::to_string).collect::<Vec<_>>()).unwrap_or_default();
                let refs: Vec<&str> = configured.iter().map(String::as_str).collect();
                let m = manifest::Manifest::build(&src, &program, &refs);
                print!("{}", m.text());
            }
            if access {
                print!("{}", access::report(&src));
            }
            if privacy && record {
                print!("{}", privacy::record(&src, &program));
            } else if privacy {
                print!("{}", privacy::report(&program));
            }
            if let Some(pack) = pack {
                let pack = compliance::Pack::load(pack)?;
                let (report, ok) = pack.report(&pack.evaluate(&src, &program, None));
                print!("{report}");
                if !ok {
                    return Err("some controls are not met (above)".into());
                }
            }
            Ok(())
        }
        Err(errs) => Err(errs.iter().map(|d| d.render(&name, &src)).collect::<Vec<_>>().join("\n\n")),
    }
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
    if let Some(pack) = &args.compliance {
        let pack = compliance::Pack::load(pack)?;
        let program = tj_core::compile(&source).map_err(|e| e.iter().map(|d| d.render(&file.display().to_string(), &source)).collect::<Vec<_>>().join("

"))?;
        let key_file = std::env::var_os(sealed::KEY_FILE_ENV).filter(|v| !v.is_empty()).map(PathBuf::from);
        let server = compliance::Server {
            encrypted: key_file.is_some(),
            encryption_key_apart: key_file.as_deref().is_some_and(|k| audit::kept_apart(k, &data)),
            audit_key_apart: audit::kept_apart(&audit::key_path(&data), &data),
            tls: !http::credentials_need_tls(dev_mode, &args.host, std::env::var(http::TLS_TERMINATED_ENV).ok().as_deref()),
        };
        let (report, ok) = pack.report(&pack.evaluate(&source, &program, Some(&server)));
        if !ok {
            return Err(format!("{report}
not starting: some controls of the compliance pack are not met (above)"));
        }
        eprintln!("compliance: {} ({}): every checked control is met", pack.title, pack.names.join(" + "));
    }
    let (audit, audit_key) = audit::Audit::start(&data)?;
    let engine = Engine::start(source, data.clone(), false, dev_mode, plugins.clone(), audit.clone())?;
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
    }
    http::serve(&addr, http::Front::new(engine, web, dev, automation, identity, audit, leases, tls))
}

/// Polls the app's files; on a change, reloads the backend and tells browsers to reload.
fn watch(file: PathBuf, engine: Engine, dev: Arc<Dev>) {
    let stamps = |files: &[PathBuf]| -> Vec<Option<SystemTime>> { files.iter().map(|f| std::fs::metadata(f).and_then(|m| m.modified()).ok()).collect() };
    let spawned = thread::Builder::new().name("tj-watch".into()).spawn(move || {
        let initial = app::read(&file).ok();
        let mut prev_src = initial.as_ref().map(|a| a.source.clone());
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
                            // Non-blocking upgrade check during `tj dev` iteration (`R-003.1`).
                            if let Some(old_src) = &prev_src {
                                if let Ok(old_prog) = tj_core::compile(old_src) {
                                    let findings = compat::check(old_src, &old_prog, &app.source, &new_prog);
                                    if !findings.is_empty() {
                                        eprint!("[tj] upgrade notice:\n{}", compat::report(&findings));
                                    }
                                }
                            }
                            prev_src = Some(app.source.clone());
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
            dev.notify_reload();
        }
    });
    if let Err(e) = spawned {
        eprintln!("[tj] live reload is off: {e}");
    }
}

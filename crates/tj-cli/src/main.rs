//! `tj`: the tinyjump command line.

mod engine;
mod http;
mod mcp;
mod review;
mod store;

use engine::Engine;
use http::Dev;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

const USAGE: &str = "\
tj: tinyjump

USAGE:
  tj check <app.tj>              compile and report errors
  tj serve <app.tj> [options]    run the backend: objects, timers, HTTP API, realtime events
  tj dev   <app.tj> [options]    serve + the web host with live reload
  tj review [--target <url>]     check a running app for layout, accessibility and contrast problems
  tj mcp    [--target <url>]     MCP server (stdio) so coding agents can inspect, drive and test an app

OPTIONS:
  --port <n>      port to listen on (default 4700)
  --host <addr>   address to bind (default 127.0.0.1; use 0.0.0.0 to expose)
  --data <dir>    where objects are stored (default: .tj-data next to the app)
  --wasm <file>   web runtime for `dev` (default: the workspace's `cargo web` output)
  --target <url>  app automation endpoint (default http://127.0.0.1:4700; Android/iOS debug: :4701)
";

struct Args {
    command: String,
    file: Option<PathBuf>,
    port: u16,
    host: String,
    data: Option<PathBuf>,
    wasm: Option<PathBuf>,
    target: String,
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
    let mut args = Args { command, file, port: 4700, host: "127.0.0.1".into(), data: None, wasm: None, target: "http://127.0.0.1:4700".into() };
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--port" => args.port = value()?.parse().map_err(|_| "--port must be a number")?,
            "--host" => args.host = value()?,
            "--data" => args.data = Some(PathBuf::from(value()?)),
            "--wasm" => args.wasm = Some(PathBuf::from(value()?)),
            "--target" => args.target = value()?,
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(args)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let result = match args.command.as_str() {
        "check" => args.file().and_then(check),
        "serve" => serve(&args, false),
        "dev" => serve(&args, true),
        "review" => review(&args.target),
        "mcp" => mcp::run(&args.target),
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
    let url = format!("{}/_tj/inspect", target.trim_end_matches('/'));
    let body = ureq::get(&url).call().map_err(|e| format!("cannot reach {url}: {e}"))?.into_string().map_err(|e| e.to_string())?;
    let reply: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("bad reply from {url}: {e}"))?;
    let snapshot = &reply["ok"];
    let findings = review::review(snapshot);
    print!("{}", review::report(snapshot, &findings));
    if findings.iter().any(|f| f.severity == review::Severity::Error) {
        return Err("review found errors".into());
    }
    Ok(())
}

fn read_source(file: &Path) -> Result<String, String> {
    std::fs::read_to_string(file).map_err(|e| format!("cannot read {}: {e}", file.display()))
}

fn check(file: &Path) -> Result<(), String> {
    let src = read_source(file)?;
    let name = file.display().to_string();
    match tj_core::compile(&src) {
        Ok(_) => {
            println!("ok: {name}");
            Ok(())
        }
        Err(errs) => Err(errs.iter().map(|d| d.render(&name, &src)).collect::<Vec<_>>().join("\n\n")),
    }
}

fn workspace_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn serve(args: &Args, dev_mode: bool) -> Result<(), String> {
    let file = args.file()?;
    let source = read_source(file)?;
    let data = args.data.clone().unwrap_or_else(|| file.parent().unwrap_or(Path::new(".")).join(".tj-data"));
    // In dev mode the browser runs the UI; the server keeps the backend objects.
    let engine = Engine::start(source, data.clone(), false)?;
    let addr = format!("{}:{}", args.host, args.port);
    let dev = if dev_mode {
        let wasm = args.wasm.clone().unwrap_or_else(|| workspace_dir().join("target/wasm32-unknown-unknown/small/tj_wasm.wasm"));
        if !wasm.exists() {
            return Err(format!("web runtime not found at {}\nbuild it with: cargo web", wasm.display()));
        }
        let dev = Arc::new(Dev { source: file.to_path_buf(), web_dir: workspace_dir().join("hosts/web"), wasm, reload: Mutex::new(Vec::new()) });
        watch(file.to_path_buf(), engine.clone(), dev.clone());
        Some(dev)
    } else {
        None
    };
    eprintln!("tinyjump {} on http://{addr}  (data: {})", if dev_mode { "dev" } else { "server" }, data.display());
    http::serve(&addr, engine, dev)
}

/// Polls the app file; on change, reloads the backend and tells browsers to reload.
fn watch(file: PathBuf, engine: Engine, dev: Arc<Dev>) {
    let modified = |f: &Path| std::fs::metadata(f).and_then(|m| m.modified()).ok();
    let spawned = thread::Builder::new().name("tj-watch".into()).spawn(move || {
        let mut last: Option<SystemTime> = modified(&file);
        loop {
            thread::sleep(Duration::from_millis(300));
            let now = modified(&file);
            if now == last {
                continue;
            }
            last = now;
            match std::fs::read_to_string(&file) {
                Ok(src) => match tj_core::compile(&src) {
                    Ok(_) => engine.reload(src),
                    Err(errs) => eprintln!("{}", errs.iter().map(|d| d.render(&file.display().to_string(), &src)).collect::<Vec<_>>().join("\n\n")),
                },
                Err(e) => eprintln!("[tj] cannot read {}: {e}", file.display()),
            }
            // Browsers reload either way: they show compile errors in their overlay.
            dev.notify_reload();
        }
    });
    if let Err(e) = spawned {
        eprintln!("[tj] live reload is off: {e}");
    }
}

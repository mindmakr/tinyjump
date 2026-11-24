//! `tj build`: ahead-of-time bytecode compilation (`.tjb`) and turnkey target packaging
//! (`--web`, `--linux`, `--android`, `--ios`, `--oci`).
//!
//! Devices and standalone servers boot `.tjb` bytecode directly via `Program::from_bytes` /
//! `Program::from_bundle`, skipping the lexer, parser, type-checker and compiler at startup.

use crate::{app, device_perms, oci, registry, remotes, webhooks};
use serde_json::{json, Value as Json};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use tj_core::crypto::sha256;
use tj_core::vm::bytecode;

/// Fixed 48-byte trailer appended to `--linux` standalone executables:
/// `[payload_len: u64 LE][sha256(payload): 32 bytes][b"TJAPP001"]`.
const TRAILER_MAGIC: &[u8; 8] = b"TJAPP001";
const TRAILER_LEN: u64 = 48;
const MAX_EMBEDDED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_VERSION_CODE: u32 = 2_100_000_000;
const ALLOWED_ANDROID_ABIS: &[&str] = &["armeabi-v7a", "arm64-v8a", "x86", "x86_64"];
const RESERVED_PKG_WORDS: &[&str] = &[
    "abstract", "assert", "boolean", "break", "byte", "case", "catch", "char", "class", "const",
    "continue", "default", "do", "double", "else", "enum", "extends", "false", "final", "finally",
    "float", "for", "fun", "goto", "if", "implements", "import", "in", "instanceof", "int",
    "interface", "is", "long", "native", "new", "null", "object", "package", "private", "protected",
    "public", "return", "short", "static", "strictfp", "super", "switch", "synchronized", "this",
    "throw", "throws", "transient", "true", "try", "val", "var", "void", "volatile", "when", "while",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Bytecode,
    Web,
    Linux,
    Android,
    Ios,
    Oci,
}

impl Target {
    fn as_str(self) -> &'static str {
        match self {
            Target::Bytecode => "bytecode",
            Target::Web => "web",
            Target::Linux => "linux",
            Target::Android => "android",
            Target::Ios => "ios",
            Target::Oci => "oci",
        }
    }
}

#[derive(Default)]
struct BuildOpts<'a> {
    out: Option<&'a Path>,
    wasm: Option<&'a Path>,
    server_url: Option<&'a str>,
    runner: Option<&'a Path>,
    min_trust: Option<registry::Trust>,
    app_id: Option<&'a str>,
    title: Option<&'a str>,
    version: Option<&'a str>,
    version_code: Option<u32>,
    abis: Option<Vec<String>>,
    port: Option<u16>,
    arch: Option<oci::OciArch>,
    no_web: bool,
    aot: bool,
    release: bool,
    assemble: bool,
}

pub fn run(args: &[String]) -> Result<(), String> {
    let usage = "usage: tj build <app.tj> [--out <path>] [--web | --linux | --android | --ios | --oci]\n  [--app-id <id>] [--title <name>] [--version <x.y.z>] [--version-code <n>]\n  [--server <url>] [--aot] [--release] [--assemble] [--abis <csv>]\n  [--port <n>] [--arch <amd64|arm64|arm>] [--no-web]\n  [--wasm <file>] [--runner <file>] [--min-trust <level>] [--json]";
    let mut file: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut target = Target::Bytecode;
    let mut target_set = false;
    let mut wasm: Option<PathBuf> = None;
    let mut server_url: Option<String> = None;
    let mut runner: Option<PathBuf> = None;
    let mut min_trust: Option<registry::Trust> = None;
    let mut app_id: Option<String> = None;
    let mut title: Option<String> = None;
    let mut version: Option<String> = None;
    let mut version_code: Option<u32> = None;
    let mut abis: Option<Vec<String>> = None;
    let mut port: Option<u16> = None;
    let mut arch: Option<oci::OciArch> = None;
    let mut no_web = false;
    let mut aot = false;
    let mut release = false;
    let mut assemble = false;
    let mut json_out = false;

    let mut it = args.iter().peekable();
    if let Some(first) = it.next_if(|a| !a.starts_with("--")) {
        file = Some(PathBuf::from(first));
    }
    let mut set_target = |t: Target| -> Result<(), String> {
        if target_set && target != t {
            return Err(format!("choose only one build target (--web, --linux, --android, --ios or --oci)\n\n{usage}"));
        }
        target = t;
        target_set = true;
        Ok(())
    };
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--out" => out = Some(PathBuf::from(value()?)),
            "--web" => set_target(Target::Web)?,
            "--linux" => set_target(Target::Linux)?,
            "--android" => set_target(Target::Android)?,
            "--ios" => set_target(Target::Ios)?,
            "--oci" => set_target(Target::Oci)?,
            "--wasm" => wasm = Some(PathBuf::from(value()?)),
            "--server" => server_url = Some(value()?.clone()),
            "--runner" => runner = Some(PathBuf::from(value()?)),
            "--min-trust" => min_trust = Some(registry::Trust::parse(value()?)?),
            "--app-id" => app_id = Some(value()?.clone()),
            "--title" => title = Some(value()?.clone()),
            "--version" => version = Some(value()?.clone()),
            "--version-code" => {
                let raw = value()?;
                let n: u32 = raw.parse().map_err(|_| format!("--version-code {raw:?} must be an integer in 1..={MAX_VERSION_CODE}"))?;
                if n == 0 || n > MAX_VERSION_CODE {
                    return Err(format!("--version-code must be in 1..={MAX_VERSION_CODE}"));
                }
                version_code = Some(n);
            }
            "--abis" => abis = Some(parse_abis(value()?)?),
            "--port" => {
                let raw = value()?;
                let n: u16 = raw.parse().map_err(|_| format!("--port {raw:?} must be in 1..=65535"))?;
                if n == 0 {
                    return Err("--port must be in 1..=65535".into());
                }
                port = Some(n);
            }
            "--arch" => arch = Some(oci::OciArch::parse(value()?)?),
            "--no-web" => no_web = true,
            "--aot" => aot = true,
            "--release" => release = true,
            "--assemble" => assemble = true,
            "--json" => json_out = true,
            other => return Err(format!("unknown option {other}\n\n{usage}")),
        }
    }
    let entry = file.ok_or_else(|| format!("`tj build` needs an app file\n\n{usage}"))?;
    let opts = BuildOpts {
        out: out.as_deref(),
        wasm: wasm.as_deref(),
        server_url: server_url.as_deref(),
        runner: runner.as_deref(),
        min_trust,
        app_id: app_id.as_deref(),
        title: title.as_deref(),
        version: version.as_deref(),
        version_code,
        abis,
        port,
        arch,
        no_web,
        aot,
        release,
        assemble,
    };
    let report = build_app(&entry, target, &opts)?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&report).expect("json"));
    } else {
        println!(
            "built {} ({}) -> {} ({} bytes bytecode, sha256 {})",
            entry.display(),
            target.as_str(),
            report["output"].as_str().unwrap_or(""),
            report["bytecodeBytes"].as_u64().unwrap_or(0),
            &report["sha256"].as_str().unwrap_or("")[..12],
        );
        if let Some(art) = report["artifact"].as_str() {
            println!("  artifact: {art}");
        }
        if let Some(notes) = report["notes"].as_array() {
            for n in notes {
                if let Some(s) = n.as_str() {
                    println!("  note: {s}");
                }
            }
        }
    }
    Ok(())
}

fn hex_bytes(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        use std::fmt::Write;
        let _ = write!(s, "{x:02x}");
    }
    s
}

fn app_name(entry: &Path) -> String {
    let stem = entry.file_stem().and_then(|s| s.to_str()).unwrap_or("app");
    if stem == "app" {
        if let Some(parent) = entry.parent().and_then(|p| p.file_name()).and_then(|s| s.to_str()) {
            if !parent.is_empty() && parent != "." {
                return parent.to_string();
            }
        }
    }
    stem.to_string()
}

fn sanitize_android_segment(raw: &str) -> String {
    let mut s: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect();
    while s.starts_with('_') {
        s.remove(0);
    }
    if s.is_empty() || !s.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        s.insert_str(0, "app");
    }
    if RESERVED_PKG_WORDS.contains(&s.as_str()) {
        s.push_str("_app");
    }
    s
}

fn sanitize_ios_segment(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_hyphen = true;
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_hyphen = false;
        } else if !prev_hyphen {
            out.push('-');
            prev_hyphen = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("app");
    }
    out
}

fn default_app_id(name: &str, target: Target) -> String {
    let seg = match target {
        Target::Ios => sanitize_ios_segment(name),
        _ => sanitize_android_segment(name),
    };
    format!("com.mindmakr.tinyjump.{seg}")
}

fn validate_app_id(id: &str, target: Target) -> Result<(), String> {
    let segs: Vec<&str> = id.split('.').collect();
    if segs.len() < 2 || id.len() > 128 {
        return Err(format!("invalid --app-id {id:?}: use reverse-DNS format with at least two segments (e.g. com.example.myapp)"));
    }
    if target == Target::Ios {
        for seg in segs {
            let valid = !seg.is_empty()
                && seg.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
                && seg.chars().last().is_some_and(|c| c.is_ascii_alphanumeric())
                && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
            if !valid {
                return Err(format!("invalid iOS --app-id segment {seg:?} in {id:?}: CFBundleIdentifier allows only [A-Za-z0-9-] (no underscores)"));
            }
        }
        return Ok(());
    }
    for seg in segs {
        let valid = !seg.is_empty()
            && seg.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return Err(format!("invalid --app-id segment {seg:?} in {id:?}: segments must start with a letter and use [A-Za-z0-9_]"));
        }
        let lower = seg.to_ascii_lowercase();
        if RESERVED_PKG_WORDS.contains(&lower.as_str()) {
            return Err(format!("invalid --app-id segment {seg:?}: Java/Kotlin reserved keyword"));
        }
    }
    Ok(())
}

fn validate_version(v: &str) -> Result<(u32, u32, u32), String> {
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() != 3 || !parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) {
        return Err(format!("invalid --version {v:?}: expected MAJOR.MINOR.PATCH with digits only (e.g. 1.0.0)"));
    }
    let maj: u32 = parts[0].parse().map_err(|_| format!("invalid --version {v:?}"))?;
    let min: u32 = parts[1].parse().map_err(|_| format!("invalid --version {v:?}"))?;
    let pat: u32 = parts[2].parse().map_err(|_| format!("invalid --version {v:?}"))?;
    Ok((maj, min, pat))
}

fn default_version_code(maj: u32, min: u32, pat: u32) -> u32 {
    maj.saturating_mul(10000)
        .saturating_add(min.saturating_mul(100))
        .saturating_add(pat)
        .clamp(1, MAX_VERSION_CODE)
}

fn validate_title(t: &str) -> Result<(), String> {
    if t.trim().is_empty() || t.len() > 80 || t.chars().any(char::is_control) {
        return Err(format!("invalid --title {t:?}: must be 1..=80 printable characters"));
    }
    Ok(())
}

fn validate_server_url(url: &str, target: Target, release: bool) -> Result<(), String> {
    if url.is_empty() {
        return if target == Target::Web { Ok(()) } else { Err("--server cannot be empty for mobile targets".into()) };
    }
    if url.chars().any(char::is_control)
        || url.contains('"')
        || url.contains('\'')
        || url.contains('\\')
        || url.contains('<')
        || url.contains('>')
        || url.contains(char::is_whitespace)
    {
        return Err(format!("invalid --server URL {url:?}: contains forbidden characters"));
    }
    if url.starts_with("https://") {
        return Ok(());
    }
    if url.starts_with("http://") {
        if release && matches!(target, Target::Android | Target::Ios) {
            return Err(format!("--release mobile builds disable cleartext HTTP; use https:// instead of {url:?}"));
        }
        return Ok(());
    }
    Err(format!("invalid --server URL {url:?}: must start with https:// or http://"))
}

fn parse_abis(csv: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for raw in csv.split(',') {
        let abi = raw.trim();
        if abi.is_empty() {
            continue;
        }
        if !ALLOWED_ANDROID_ABIS.contains(&abi) {
            return Err(format!("unknown Android ABI {abi:?}; expected subset of {}", ALLOWED_ANDROID_ABIS.join(", ")));
        }
        if !out.iter().any(|x| x == abi) {
            out.push(abi.to_string());
        }
    }
    if out.is_empty() {
        return Err("--abis must list at least one Android ABI".into());
    }
    Ok(out)
}

fn pascal_exec_name(raw: &str) -> String {
    let mut out = String::new();
    let mut cap = true;
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            if cap {
                out.push(c.to_ascii_uppercase());
                cap = false;
            } else {
                out.push(c);
            }
        } else {
            cap = true;
        }
    }
    if out.is_empty() || !out.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        out.insert_str(0, "App");
    }
    out
}

fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Escapes text for Android `res/values/strings.xml` so AAPT2 treats leading `@`/`?`,
/// quotes, backslashes and XML entities as literal characters (and never triggers manifest placeholders).
fn escape_android_res_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    if s.starts_with('@') || s.starts_with('?') {
        out.push('\\');
    }
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

fn escape_c_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

fn escape_kotlin_string(s: &str) -> String {
    escape_c_string(s).replace('$', "\\$")
}

fn escape_shell_single(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn workspace_dir() -> PathBuf {
    if let Ok(home) = std::env::var("TJ_HOME") {
        let p = PathBuf::from(home);
        if p.join("hosts").is_dir() {
            return fs::canonicalize(&p).unwrap_or(p);
        }
    }
    let fallback = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    fs::canonicalize(&fallback).unwrap_or(fallback)
}

fn find_web_file(rel: &str) -> Option<PathBuf> {
    let joined = format!("hosts/web/{rel}");
    let candidates = [workspace_dir().join(&joined), PathBuf::from(&joined)];
    candidates.into_iter().find(|p| p.exists())
}

fn find_web_js() -> Option<PathBuf> {
    find_web_file("tinyjump.js")
}

fn find_web_host_index() -> Option<PathBuf> {
    find_web_file("index.html")
}

fn find_web_device_plugins_js() -> Option<PathBuf> {
    find_web_file("plugins/device.js")
}

fn find_web_webrtc_js() -> Option<PathBuf> {
    find_web_file("plugins/webrtc.js")
}

fn find_web_whiteboard_js() -> Option<PathBuf> {
    find_web_file("plugins/whiteboard.js")
}

fn find_web_gfx_js() -> Option<PathBuf> {
    find_web_file("plugins/gfx.js")
}

/// Resolves the WASM runtime for `--web`. Returns `(path, used_aot, stale_aot_note)`.
fn find_wasm(explicit: Option<&Path>, require_aot: bool) -> (Option<PathBuf>, bool, Option<String>) {
    if let Some(p) = explicit {
        return (p.exists().then(|| p.to_path_buf()), require_aot, None);
    }
    let aot = [
        workspace_dir().join("target/aot/wasm32-unknown-unknown/small/tj_wasm.wasm"),
        PathBuf::from("target/aot/wasm32-unknown-unknown/small/tj_wasm.wasm"),
    ]
    .into_iter()
    .find(|p| p.exists());
    if require_aot {
        return (aot, true, None);
    }
    let full = [
        workspace_dir().join("target/wasm32-unknown-unknown/small/tj_wasm.wasm"),
        PathBuf::from("target/wasm32-unknown-unknown/small/tj_wasm.wasm"),
    ]
    .into_iter()
    .find(|p| p.exists());
    match (aot, full) {
        (Some(a), Some(f)) => {
            let a_time = fs::metadata(&a).and_then(|m| m.modified()).ok();
            let f_time = fs::metadata(&f).and_then(|m| m.modified()).ok();
            if let (Some(at), Some(ft)) = (a_time, f_time) {
                if ft > at {
                    return (
                        Some(f),
                        false,
                        Some("bundled newer full WASM (existing AOT WASM is older; run `cargo web-aot` to refresh it)".into()),
                    );
                }
            }
            (Some(a), true, None)
        }
        (Some(a), None) => (Some(a), true, None),
        (None, Some(f)) => (Some(f), false, None),
        (None, None) => (None, false, None),
    }
}

fn find_java_home() -> Option<PathBuf> {
    if let Ok(jh) = std::env::var("JAVA_HOME") {
        let p = PathBuf::from(jh);
        if p.is_dir() {
            return Some(p);
        }
    }
    let candidates: &[&str] = if cfg!(windows) {
        &[
            r"C:\Program Files\Android\Android Studio\jbr",
            r"C:\Program Files\Android\Android Studio\jre",
        ]
    } else if cfg!(target_os = "macos") {
        &["/Applications/Android Studio.app/Contents/jbr/Contents/Home"]
    } else {
        &["/opt/android-studio/jbr", "/usr/lib/jvm/default-java"]
    };
    candidates.iter().map(PathBuf::from).find(|p| p.is_dir())
}

fn build_app(entry: &Path, target: Target, opts: &BuildOpts<'_>) -> Result<Json, String> {
    if opts.abis.is_some() && target != Target::Android {
        return Err("--abis only applies to --android".into());
    }
    if (opts.port.is_some() || opts.arch.is_some() || opts.no_web) && target != Target::Oci {
        return Err("--port, --arch, and --no-web only apply to --oci".into());
    }
    if opts.assemble && !matches!(target, Target::Android | Target::Ios | Target::Oci) {
        return Err("--assemble only applies to --android, --ios, and --oci targets".into());
    }

    let app_dir = entry.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let (_, warnings) = registry::check_lockfile(app_dir, opts.min_trust)?;
    for w in warnings {
        eprintln!("warning: {w}");
    }
    let loaded = app::read(entry)?;
    if loaded.source.starts_with(bytecode::BUNDLE_PREFIX) {
        return Err("`tj build` expects a .tj source file, not a precompiled .tjb bundle".into());
    }
    let program = tj_core::compile(&loaded.source).map_err(|errs| {
        errs.iter().map(|d| d.render(&entry.display().to_string(), &loaded.source)).collect::<Vec<_>>().join("\n\n")
    })?;
    // Enforce remote privacy-flow and webhook validation if remotes.json / webhooks.json sit next to the app.
    let private_fetch = std::env::var(crate::engine::PRIVATE_FETCH_ENV).is_ok_and(|v| v == "1");
    let fetch_allow = crate::engine::fetch_allow_from_env()?;
    let rem = remotes::Remotes::load(entry, &program, false, private_fetch, &fetch_allow)?;
    let rem_classes = rem.classes();
    let rem_dests = rem.destinations();
    let flow_errs = tj_core::check_remote_flows(&loaded.source, &rem_dests);
    if !flow_errs.is_empty() {
        return Err(flow_errs.iter().map(|d| d.render(&entry.display().to_string(), &loaded.source)).collect::<Vec<_>>().join("\n\n"));
    }
    let _ = webhooks::Webhooks::load(entry, &program, &rem_classes, false, private_fetch, &fetch_allow)?;

    let bytes = program.to_bytes()?;
    let bundle = bytecode::bundle_from_bytes(&bytes);
    let digest = sha256(&bytes);
    let sha_hex = hex_bytes(&digest);
    let name = app_name(entry);
    let title = opts.title.unwrap_or(&name);
    if opts.title.is_some() || matches!(target, Target::Web | Target::Android | Target::Ios | Target::Oci) {
        validate_title(title)?;
    }
    let version = opts.version.unwrap_or("0.1.0");
    let (maj, min, pat) = validate_version(version)?;
    let version_code = opts.version_code.unwrap_or_else(|| default_version_code(maj, min, pat));
    let app_id = match opts.app_id {
        Some(id) => {
            validate_app_id(id, target)?;
            id.to_string()
        }
        None => default_app_id(&name, target),
    };
    if let Some(url) = opts.server_url {
        validate_server_url(url, target, opts.release)?;
    }

    let objects: Vec<&str> = program.objects.iter().map(|o| &*o.name).collect();
    let has_ui = program.app.is_some();
    let has_server_objects = program.objects.iter().any(|o| o.server);
    let dev_perms = device_perms::load_app_device_permissions(app_dir)?;
    let dev_perm_names: Vec<&str> = dev_perms.keys().map(String::as_str).collect();
    let mut notes: Vec<String> = Vec::new();
    let mut artifact_path: Option<PathBuf> = None;
    let mut oci_meta: Option<Json> = None;

    let output_path = match target {
        Target::Bytecode => {
            let dest = opts.out.map(PathBuf::from).unwrap_or_else(|| entry.with_extension("tjb"));
            if let Some(p) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
                fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
            }
            fs::write(&dest, &bytes).map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
            dest
        }
        Target::Web => {
            let dest = opts.out.map(PathBuf::from).unwrap_or_else(|| app_dir.join("dist").join("web"));
            fs::create_dir_all(&dest).map_err(|e| format!("cannot create {}: {e}", dest.display()))?;
            fs::write(dest.join("app.tjb"), &bundle).map_err(|e| format!("cannot write app.tjb: {e}"))?;
            if let Some(js) = find_web_js() {
                fs::copy(&js, dest.join("tinyjump.js")).map_err(|e| format!("cannot copy {}: {e}", js.display()))?;
            } else {
                notes.push("tinyjump.js not found in workspace; copy hosts/web/tinyjump.js into this folder".into());
            }
            let (found_wasm, used_aot_wasm, stale_note) = find_wasm(opts.wasm, opts.aot);
            if let Some(note) = stale_note {
                notes.push(note);
            }
            if let Some(w) = found_wasm {
                fs::copy(&w, dest.join("tj_wasm.wasm")).map_err(|e| format!("cannot copy {}: {e}", w.display()))?;
                if used_aot_wasm && opts.wasm.is_none() {
                    notes.push(format!("bundled compiler-free AOT WASM ({})", w.display()));
                }
            } else if let Some(explicit) = opts.wasm {
                return Err(format!("wasm file not found at {}", explicit.display()));
            } else if opts.aot {
                return Err("compiler-free AOT WASM not found; run `cargo web-aot` first (or pass --wasm <file>)".into());
            } else {
                notes.push("tj_wasm.wasm not found; run `cargo web` (or `cargo web-aot` for 36% smaller AOT WASM) and re-run `tj build --web`".into());
            }
            let host_plugins = device_perms::bundled_host_plugins(&dev_perms);
            let stale_plugins_js = dest.join("device-plugins.js");
            if !host_plugins.is_empty() {
                if let Some(pjs) = find_web_device_plugins_js() {
                    fs::copy(&pjs, &stale_plugins_js).map_err(|e| format!("cannot copy {}: {e}", pjs.display()))?;
                } else {
                    notes.push("hosts/web/plugins/device.js not found in workspace; copy it as device-plugins.js into this folder".into());
                }
            } else if stale_plugins_js.exists() {
                let _ = fs::remove_file(&stale_plugins_js);
            }
            let has_video_call = device_perms::uses_video_call(&program, &dev_perms);
            let stale_webrtc_js = dest.join("webrtc-plugin.js");
            if has_video_call {
                if let Some(wjs) = find_web_webrtc_js() {
                    fs::copy(&wjs, &stale_webrtc_js).map_err(|e| format!("cannot copy {}: {e}", wjs.display()))?;
                } else {
                    notes.push("hosts/web/plugins/webrtc.js not found in workspace; copy it as webrtc-plugin.js into this folder".into());
                }
            } else if stale_webrtc_js.exists() {
                let _ = fs::remove_file(&stale_webrtc_js);
            }
            let has_whiteboard = device_perms::uses_whiteboard(&program);
            let stale_whiteboard_js = dest.join("whiteboard-plugin.js");
            if has_whiteboard {
                if let Some(wbjs) = find_web_whiteboard_js() {
                    fs::copy(&wbjs, &stale_whiteboard_js).map_err(|e| format!("cannot copy {}: {e}", wbjs.display()))?;
                } else {
                    notes.push("hosts/web/plugins/whiteboard.js not found in workspace; copy it as whiteboard-plugin.js into this folder".into());
                }
            } else if stale_whiteboard_js.exists() {
                let _ = fs::remove_file(&stale_whiteboard_js);
            }
            let has_frame = program.has_frame();
            let has_gfx = program.uses_scene() || has_frame;
            let stale_gfx_js = dest.join("gfx-plugin.js");
            if has_gfx {
                if let Some(gjs) = find_web_gfx_js() {
                    fs::copy(&gjs, &stale_gfx_js).map_err(|e| format!("cannot copy {}: {e}", gjs.display()))?;
                } else {
                    notes.push("hosts/web/plugins/gfx.js not found in workspace; copy it as gfx-plugin.js into this folder".into());
                }
            } else if stale_gfx_js.exists() {
                let _ = fs::remove_file(&stale_gfx_js);
            }
            if has_server_objects && opts.server_url.is_none() {
                notes.push("this app has @server objects; pass --server <url> (or '' for same-origin) so the web host knows where `tj serve` runs".into());
            }
            let html = web_index_html(title, opts.server_url, &host_plugins, has_video_call, has_whiteboard, has_gfx, has_frame);
            fs::write(dest.join("index.html"), html).map_err(|e| format!("cannot write index.html: {e}"))?;
            let manifest = json!({
                "target": "web",
                "app": name,
                "title": title,
                "version": version,
                "aot": used_aot_wasm,
                "sha256": sha_hex,
                "bytecodeBytes": bytes.len(),
                "bundleBytes": bundle.len(),
                "objects": objects,
                "hasUi": has_ui,
                "devicePermissions": dev_perm_names,
            });
            fs::write(dest.join("manifest.json"), serde_json::to_string_pretty(&manifest).expect("json"))
                .map_err(|e| format!("cannot write manifest.json: {e}"))?;
            dest
        }
        Target::Linux => {
            let runner_path = resolve_runner(opts.runner)?;
            let runner_ext = runner_path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let default_name = if runner_ext.eq_ignore_ascii_case("exe") { format!("{name}.exe") } else { name.clone() };
            let dest = opts.out.map(PathBuf::from).unwrap_or_else(|| app_dir.join("dist").join(default_name));
            package_standalone(&runner_path, &bytes, &dest)?;
            if cfg!(windows) && opts.runner.is_none() {
                notes.push("packaged using the current Windows tj binary; pass --runner <path-to-linux-tj> to cross-package a Linux ELF".into());
            }
            note_standalone_sidecars(app_dir, &mut notes);
            dest
        }
        Target::Oci => {
            let runner_path = resolve_runner(opts.runner)?;
            let exe_bytes = assemble_standalone_bytes(&runner_path, &bytes)?;
            let detected_arch = oci::detect_elf_arch(&exe_bytes);
            if detected_arch.is_none() {
                notes.push("packaged using non-ELF host runner; pass --runner <path-to-static-linux-tj> for a runnable Linux OCI image".into());
            } else if let (Some(explicit), Some(elf_arch)) = (opts.arch, detected_arch) {
                if explicit != elf_arch {
                    notes.push(format!("--arch {} overrides runner ELF arch {}; ensure the binary matches your target runtime", explicit.as_str(), elf_arch.as_str()));
                }
            }
            if let Some(interp) = oci::detect_elf_dynamic_interp(&exe_bytes) {
                notes.push(format!("runner ELF is dynamically linked ({interp}); FROM scratch images require a static Linux binary (e.g. musl)"));
            }
            let arch = opts.arch.or(detected_arch).unwrap_or_else(oci::host_default_arch);
            let port = opts.port.unwrap_or(4700);
            let bin_name = oci::sanitize_bin_name(&name);
            let web_owned = collect_oci_web_assets(&program, &dev_perms, opts, has_ui, &mut notes)?;
            let web_refs: Vec<(&str, &[u8])> = web_owned.iter().map(|(k, v)| (k.as_str(), v.as_slice())).collect();
            let out_is_tar = opts.out.is_some_and(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("tar")));

            let (output_dir_or_tar, layout_dir, archive_target, containerfile_path) = if out_is_tar {
                let tar_path = opts.out.expect("checked").to_path_buf();
                (tar_path.clone(), None, Some(tar_path), None)
            } else {
                let dist = opts.out.map(PathBuf::from).unwrap_or_else(|| app_dir.join("dist").join("oci"));
                fs::create_dir_all(&dist).map_err(|e| format!("cannot create {}: {e}", dist.display()))?;
                let layout = dist.join("layout");
                let tar_out = opts.assemble.then(|| dist.join(format!("{bin_name}-oci.tar")));
                let cf = dist.join("Containerfile");
                (dist, Some(layout), tar_out, Some(cf))
            };

            let oci_res = oci::build_oci_image(&oci::OciOptions {
                layout_dir: layout_dir.as_deref(),
                archive_path: archive_target.as_deref(),
                app_name: &name,
                title,
                version,
                app_sha256: &sha_hex,
                bytecode_bytes: bytes.len(),
                has_ui,
                port,
                arch,
                executable_bytes: &exe_bytes,
                web_assets: &web_refs,
            })?;

            if let Some(ref cf_path) = containerfile_path {
                let cf = oci::render_containerfile(&oci_res.bin_name, title, version, &sha_hex, port, !oci_res.web_files.is_empty());
                fs::write(cf_path, cf).map_err(|e| format!("cannot write {}: {e}", cf_path.display()))?;
            }
            artifact_path = oci_res.archive_path.clone();
            note_standalone_sidecars(app_dir, &mut notes);

            let meta = json!({
                "binary": oci_res.bin_name,
                "arch": arch.as_str(),
                "platform": arch.platform_str(),
                "port": port,
                "imageDigest": oci_res.image_digest,
                "configDigest": oci_res.config_digest,
                "layerDigest": oci_res.layer_digest,
                "diffId": oci_res.diff_id,
                "executableBytes": exe_bytes.len(),
                "layerTarBytes": oci_res.layer_tar_bytes,
                "layerGzBytes": oci_res.layer_gz_bytes,
                "webFiles": oci_res.web_files,
                "containerfile": containerfile_path.as_ref().map(|p| p.display().to_string()),
            });
            if !out_is_tar {
                let mf = json!({
                    "target": "oci",
                    "app": name,
                    "title": title,
                    "version": version,
                    "sha256": sha_hex,
                    "bytecodeBytes": bytes.len(),
                    "bundleBytes": bundle.len(),
                    "objects": objects,
                    "hasUi": has_ui,
                    "artifact": artifact_path.as_ref().map(|p| p.display().to_string()),
                    "oci": meta,
                });
                fs::write(output_dir_or_tar.join("manifest.json"), serde_json::to_string_pretty(&mf).expect("json"))
                    .map_err(|e| format!("cannot write manifest.json: {e}"))?;
            }
            oci_meta = Some(meta);
            output_dir_or_tar
        }
        Target::Android => {
            let dest = opts.out.map(PathBuf::from).unwrap_or_else(|| app_dir.join("dist").join("android"));
            let abis = opts
                .abis
                .clone()
                .unwrap_or_else(|| ALLOWED_ANDROID_ABIS.iter().map(|s| s.to_string()).collect());
            if has_server_objects && opts.server_url.is_none() {
                notes.push("this app has @server objects; pass --server <https://...> to bake the backend URL into MainActivity".into());
            }
            generate_android_project(
                &dest,
                &name,
                &app_id,
                title,
                version,
                version_code,
                &bundle,
                opts.server_url,
                opts.aot,
                &abis,
                &dev_perms,
                &mut notes,
            )?;
            let apk = if opts.assemble {
                Some(assemble_android(&dest, &name, opts.release, opts.aot, &abis, &mut notes)?)
            } else {
                None
            };
            if let Some(ref p) = apk {
                artifact_path = Some(p.clone());
            }
            let manifest = json!({
                "target": "android",
                "app": name,
                "appId": app_id,
                "title": title,
                "version": version,
                "versionCode": version_code,
                "aot": opts.aot,
                "abis": abis,
                "minSdk": 21,
                "devicePermissions": dev_perm_names,
                "sha256": sha_hex,
                "bytecodeBytes": bytes.len(),
                "bundleBytes": bundle.len(),
                "objects": objects,
                "hasUi": has_ui,
                "artifact": apk.as_ref().map(|p| p.display().to_string()),
            });
            fs::write(dest.join("manifest.json"), serde_json::to_string_pretty(&manifest).expect("json"))
                .map_err(|e| format!("cannot write manifest.json: {e}"))?;
            dest
        }
        Target::Ios => {
            let dest = opts.out.map(PathBuf::from).unwrap_or_else(|| app_dir.join("dist").join("ios"));
            let exec_name = pascal_exec_name(&name);
            if has_server_objects && opts.server_url.is_none() {
                notes.push("this app has @server objects; pass --server <https://...> to bake the backend URL into AppDelegate".into());
            }
            generate_ios_project(
                &dest,
                &exec_name,
                &app_id,
                title,
                version,
                version_code,
                &bundle,
                opts.server_url,
                opts.aot,
                opts.release,
                &dev_perms,
                &mut notes,
            )?;
            let app_bundle = if opts.assemble {
                Some(assemble_ios(&dest, &exec_name, opts.release, &mut notes)?)
            } else {
                None
            };
            if let Some(ref p) = app_bundle {
                artifact_path = Some(p.clone());
            }
            let manifest = json!({
                "target": "ios",
                "app": name,
                "appId": app_id,
                "title": title,
                "executable": exec_name,
                "version": version,
                "versionCode": version_code,
                "aot": opts.aot,
                "minIos": "13.0",
                "devicePermissions": dev_perm_names,
                "sha256": sha_hex,
                "bytecodeBytes": bytes.len(),
                "bundleBytes": bundle.len(),
                "objects": objects,
                "hasUi": has_ui,
                "artifact": app_bundle.as_ref().map(|p| p.display().to_string()),
            });
            fs::write(dest.join("manifest.json"), serde_json::to_string_pretty(&manifest).expect("json"))
                .map_err(|e| format!("cannot write manifest.json: {e}"))?;
            dest
        }
    };

    let mut out = json!({
        "target": target.as_str(),
        "app": name,
        "appId": app_id,
        "title": title,
        "version": version,
        "output": output_path.display().to_string(),
        "artifact": artifact_path.map(|p| p.display().to_string()),
        "sha256": sha_hex,
        "bytecodeBytes": bytes.len(),
        "bundleBytes": bundle.len(),
        "objects": objects,
        "hasUi": has_ui,
        "notes": notes,
    });
    if let Some(meta) = oci_meta {
        out.as_object_mut().expect("json object").insert("oci".into(), meta);
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn generate_android_project(
    dest: &Path,
    name: &str,
    app_id: &str,
    title: &str,
    version: &str,
    version_code: u32,
    bundle: &str,
    server_url: Option<&str>,
    aot: bool,
    abis: &[String],
    dev_perms: &BTreeMap<String, device_perms::DevicePermission>,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    let assets = dest.join("assets");
    fs::create_dir_all(&assets).map_err(|e| format!("cannot create {}: {e}", assets.display()))?;
    fs::write(assets.join("app.tjb"), bundle).map_err(|e| format!("cannot write assets/app.tjb: {e}"))?;

    let ws = workspace_dir();
    let ws_android = ws.join("hosts/android");
    let lib_dir = ws_android.join("tinyjump");
    let lib_dir_canon = fs::canonicalize(&lib_dir).unwrap_or_else(|_| lib_dir.clone());
    // Strip Windows UNC prefix (`\\?\`) so Gradle's file(...) accepts the path cleanly.
    let lib_path_str = strip_win_unc(&lib_dir_canon);
    if !lib_dir.is_dir() {
        notes.push("hosts/android/tinyjump not found; set TJ_HOME to your tinyjump checkout before running Gradle".into());
    }

    let settings = format!(
        "pluginManagement {{\n    repositories {{\n        google()\n        mavenCentral()\n        gradlePluginPortal()\n    }}\n}}\n\ndependencyResolutionManagement {{\n    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)\n    repositories {{\n        google()\n        mavenCentral()\n    }}\n}}\n\nrootProject.name = \"{}\"\ninclude(\":app\", \":tinyjump\")\nproject(\":tinyjump\").projectDir = file(\"{}\")\n",
        escape_kotlin_string(name),
        escape_kotlin_string(&lib_path_str),
    );
    fs::write(dest.join("settings.gradle.kts"), settings).map_err(|e| format!("cannot write settings.gradle.kts: {e}"))?;

    // Redirect `:tinyjump` buildDirectory into this generated project so builds stay isolated
    // and never mutate the shared tinyjump repo checkout.
    fs::write(
        dest.join("build.gradle.kts"),
        "plugins {\n    id(\"com.android.application\") version \"8.13.2\" apply false\n    id(\"com.android.library\") version \"8.13.2\" apply false\n}\n\nproject(\":tinyjump\") {\n    layout.buildDirectory.set(rootProject.layout.buildDirectory.dir(\"tinyjump\"))\n}\n",
    )
    .map_err(|e| format!("cannot write build.gradle.kts: {e}"))?;

    let props = format!(
        "org.gradle.jvmargs=-Xmx2g -Dfile.encoding=UTF-8\nandroid.useAndroidX=false\nandroid.nonTransitiveRClass=true\ntjAot={}\ntjAbis={}\n",
        if aot { "true" } else { "false" },
        abis.join(","),
    );
    fs::write(dest.join("gradle.properties"), props).map_err(|e| format!("cannot write gradle.properties: {e}"))?;

    // Copy local.properties (sdk.dir) if present in hosts/android, else derive from non-empty ANDROID_HOME.
    let ws_local = ws_android.join("local.properties");
    if ws_local.is_file() {
        fs::copy(&ws_local, dest.join("local.properties"))
            .map_err(|e| format!("cannot copy {}: {e}", ws_local.display()))?;
    } else if let Ok(sdk) = std::env::var("ANDROID_HOME").or_else(|_| std::env::var("ANDROID_SDK_ROOT")) {
        let trimmed = sdk.trim();
        if !trimmed.is_empty() {
            let escaped = trimmed.replace('\\', "\\\\").replace(':', "\\:");
            fs::write(dest.join("local.properties"), format!("sdk.dir={escaped}\n"))
                .map_err(|e| format!("cannot write local.properties: {e}"))?;
        }
    }

    // Copy Gradle wrapper from hosts/android so `./gradlew :app:assembleDebug` works standalone.
    for rel in ["gradlew", "gradlew.bat", "gradle/wrapper/gradle-wrapper.jar", "gradle/wrapper/gradle-wrapper.properties"] {
        let src = ws_android.join(rel);
        if src.is_file() {
            let dst = dest.join(rel);
            if let Some(p) = dst.parent() {
                fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
            }
            fs::copy(&src, &dst).map_err(|e| format!("cannot copy {}: {e}", src.display()))?;
            #[cfg(unix)]
            if rel == "gradlew" {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&dst, fs::Permissions::from_mode(0o755))
                    .map_err(|e| format!("cannot mark {} executable: {e}", dst.display()))?;
            }
        }
    }

    let app_mod = dest.join("app");
    // Clean previous generated Java sources so changing `--app-id` in the same `--out` never leaves stale classes.
    let java_root = app_mod.join("src/main/java");
    if java_root.exists() {
        fs::remove_dir_all(&java_root).map_err(|e| format!("cannot clean {}: {e}", java_root.display()))?;
    }
    let pkg_dir = java_root.join(app_id.replace('.', "/"));
    fs::create_dir_all(&pkg_dir).map_err(|e| format!("cannot create {}: {e}", pkg_dir.display()))?;

    let app_gradle = format!(
        "plugins {{\n    id(\"com.android.application\")\n}}\n\nandroid {{\n    namespace = \"{app_id}\"\n    compileSdk = 35\n\n    defaultConfig {{\n        applicationId = \"{app_id}\"\n        minSdk = 21\n        targetSdk = 35\n        versionCode = {version_code}\n        versionName = \"{version}\"\n    }}\n\n    signingConfigs {{\n        val ksPath = System.getenv(\"TJ_ANDROID_KEYSTORE\")\n        if (!ksPath.isNullOrBlank()) {{\n            create(\"release\") {{\n                storeFile = file(ksPath)\n                storePassword = System.getenv(\"TJ_ANDROID_KEYSTORE_PASS\") ?: \"\"\n                keyAlias = System.getenv(\"TJ_ANDROID_KEY_ALIAS\") ?: \"\"\n                keyPassword = System.getenv(\"TJ_ANDROID_KEY_PASS\") ?: \"\"\n            }}\n        }}\n    }}\n\n    buildTypes {{\n        getByName(\"debug\") {{ manifestPlaceholders[\"cleartext\"] = \"true\" }}\n        getByName(\"release\") {{\n            manifestPlaceholders[\"cleartext\"] = \"false\"\n            signingConfigs.findByName(\"release\")?.let {{ signingConfig = it }}\n        }}\n    }}\n\n    compileOptions {{\n        sourceCompatibility = JavaVersion.VERSION_11\n        targetCompatibility = JavaVersion.VERSION_11\n    }}\n\n    sourceSets[\"main\"].assets.srcDir(rootDir.resolve(\"assets\"))\n}}\n\ndependencies {{\n    implementation(project(\":tinyjump\"))\n}}\n"
    );
    fs::write(app_mod.join("build.gradle.kts"), app_gradle).map_err(|e| format!("cannot write app/build.gradle.kts: {e}"))?;

    let res_values = app_mod.join("src/main/res/values");
    fs::create_dir_all(&res_values).map_err(|e| format!("cannot create {}: {e}", res_values.display()))?;
    let strings_xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<resources>\n    <string name=\"app_name\">{}</string>\n</resources>\n",
        escape_android_res_string(title),
    );
    fs::write(res_values.join("strings.xml"), strings_xml).map_err(|e| format!("cannot write strings.xml: {e}"))?;

    let perm_xml = device_perms::android_manifest_xml(dev_perms);
    let manifest_xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<manifest xmlns:android=\"http://schemas.android.com/apk/res/android\">\n{perm_xml}    <application\n        android:allowBackup=\"false\"\n        android:supportsRtl=\"true\"\n        android:usesCleartextTraffic=\"${{cleartext}}\"\n        android:label=\"@string/app_name\"\n        android:theme=\"@android:style/Theme.Material.Light.NoActionBar\">\n        <activity\n            android:name=\".MainActivity\"\n            android:exported=\"true\"\n            android:windowSoftInputMode=\"adjustResize\">\n            <intent-filter>\n                <action android:name=\"android.intent.action.MAIN\" />\n                <category android:name=\"android.intent.category.LAUNCHER\" />\n            </intent-filter>\n        </activity>\n    </application>\n</manifest>\n"
    );
    fs::write(app_mod.join("src/main/AndroidManifest.xml"), manifest_xml)
        .map_err(|e| format!("cannot write AndroidManifest.xml: {e}"))?;

    let host_plugins = device_perms::bundled_host_plugins(dev_perms);
    let has_host_plugins = !host_plugins.is_empty();
    if has_host_plugins {
        let plugin_src = ws_android.join("plugins/DevicePlugins.java");
        if plugin_src.is_file() {
            let plugin_dir = java_root.join("com/mindmakr/tinyjump/plugins");
            fs::create_dir_all(&plugin_dir).map_err(|e| format!("cannot create {}: {e}", plugin_dir.display()))?;
            fs::copy(&plugin_src, plugin_dir.join("DevicePlugins.java"))
                .map_err(|e| format!("cannot copy {}: {e}", plugin_src.display()))?;
        } else {
            notes.push("hosts/android/plugins/DevicePlugins.java not found in workspace; set TJ_HOME to your tinyjump checkout".into());
        }
    }

    let default_server_expr = match server_url {
        Some(u) => format!("\"{}\"", escape_c_string(u)),
        None => "null".to_string(),
    };
    let main_java = if has_host_plugins {
        let caps_list: String = host_plugins.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(", ");
        format!(
            "package {app_id};\n\nimport android.app.Activity;\nimport android.content.Intent;\nimport android.content.pm.ApplicationInfo;\nimport android.os.Bundle;\nimport android.view.View;\nimport android.widget.FrameLayout;\nimport com.mindmakr.tinyjump.TinyJump;\nimport com.mindmakr.tinyjump.plugins.DevicePlugins;\nimport java.io.ByteArrayOutputStream;\nimport java.io.IOException;\nimport java.io.InputStream;\nimport java.util.Arrays;\nimport java.util.HashSet;\n\npublic final class MainActivity extends Activity {{\n    private static final String DEFAULT_SERVER = {default_server_expr};\n    private static final int AUTOMATION_PORT = 4701;\n    private TinyJump app;\n\n    @Override\n    protected void onCreate(Bundle state) {{\n        super.onCreate(state);\n        FrameLayout root = new FrameLayout(this);\n        root.setFitsSystemWindows(true);\n        boolean debuggable = (getApplicationInfo().flags & ApplicationInfo.FLAG_DEBUGGABLE) != 0;\n        if (debuggable) {{\n            String dir = getIntent().getStringExtra(\"dir\");\n            if (\"rtl\".equals(dir)) root.setLayoutDirection(View.LAYOUT_DIRECTION_RTL);\n            if (\"ltr\".equals(dir)) root.setLayoutDirection(View.LAYOUT_DIRECTION_LTR);\n        }}\n        setContentView(root);\n        DevicePlugins.install(this, new HashSet<>(Arrays.asList({caps_list})));\n        String source = loadBundle();\n        String server = DEFAULT_SERVER;\n        if (debuggable) {{\n            String overrideServer = getIntent().getStringExtra(\"server\");\n            if (overrideServer != null) server = overrideServer;\n        }}\n        app = TinyJump.start(this, root, source, server);\n        if (debuggable) app.enableAutomation(AUTOMATION_PORT);\n    }}\n\n    @Override\n    protected void onActivityResult(int requestCode, int resultCode, Intent data) {{\n        if (DevicePlugins.onActivityResult(this, requestCode, resultCode, data)) return;\n        super.onActivityResult(requestCode, resultCode, data);\n    }}\n\n    @Override\n    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {{\n        if (DevicePlugins.onRequestPermissionsResult(this, requestCode, permissions, grantResults)) return;\n        super.onRequestPermissionsResult(requestCode, permissions, grantResults);\n    }}\n\n    @Override\n    protected void onDestroy() {{\n        DevicePlugins.uninstall(this);\n        if (app != null) app.stop();\n        super.onDestroy();\n    }}\n\n    private String loadBundle() {{\n        try (InputStream in = getAssets().open(\"app.tjb\")) {{\n            ByteArrayOutputStream out = new ByteArrayOutputStream();\n            byte[] buf = new byte[8192];\n            for (int n; (n = in.read(buf)) > 0; ) out.write(buf, 0, n);\n            return out.toString(\"UTF-8\");\n        }} catch (IOException e) {{\n            throw new IllegalStateException(\"missing assets/app.tjb\", e);\n        }}\n    }}\n}}\n"
        )
    } else {
        format!(
            "package {app_id};\n\nimport android.app.Activity;\nimport android.content.pm.ApplicationInfo;\nimport android.os.Bundle;\nimport android.view.View;\nimport android.widget.FrameLayout;\nimport com.mindmakr.tinyjump.TinyJump;\nimport java.io.ByteArrayOutputStream;\nimport java.io.IOException;\nimport java.io.InputStream;\n\npublic final class MainActivity extends Activity {{\n    private static final String DEFAULT_SERVER = {default_server_expr};\n    private static final int AUTOMATION_PORT = 4701;\n    private TinyJump app;\n\n    @Override\n    protected void onCreate(Bundle state) {{\n        super.onCreate(state);\n        FrameLayout root = new FrameLayout(this);\n        root.setFitsSystemWindows(true);\n        boolean debuggable = (getApplicationInfo().flags & ApplicationInfo.FLAG_DEBUGGABLE) != 0;\n        if (debuggable) {{\n            String dir = getIntent().getStringExtra(\"dir\");\n            if (\"rtl\".equals(dir)) root.setLayoutDirection(View.LAYOUT_DIRECTION_RTL);\n            if (\"ltr\".equals(dir)) root.setLayoutDirection(View.LAYOUT_DIRECTION_LTR);\n        }}\n        setContentView(root);\n        String source = loadBundle();\n        String server = DEFAULT_SERVER;\n        if (debuggable) {{\n            String overrideServer = getIntent().getStringExtra(\"server\");\n            if (overrideServer != null) server = overrideServer;\n        }}\n        app = TinyJump.start(this, root, source, server);\n        if (debuggable) app.enableAutomation(AUTOMATION_PORT);\n    }}\n\n    @Override\n    protected void onDestroy() {{\n        if (app != null) app.stop();\n        super.onDestroy();\n    }}\n\n    private String loadBundle() {{\n        try (InputStream in = getAssets().open(\"app.tjb\")) {{\n            ByteArrayOutputStream out = new ByteArrayOutputStream();\n            byte[] buf = new byte[8192];\n            for (int n; (n = in.read(buf)) > 0; ) out.write(buf, 0, n);\n            return out.toString(\"UTF-8\");\n        }} catch (IOException e) {{\n            throw new IllegalStateException(\"missing assets/app.tjb\", e);\n        }}\n    }}\n}}\n"
        )
    };
    fs::write(pkg_dir.join("MainActivity.java"), main_java).map_err(|e| format!("cannot write MainActivity.java: {e}"))?;

    fs::write(
        dest.join("README.txt"),
        format!(
            "Turnkey Android project for {title} ({app_id}, v{version}).\n\
             assets/app.tjb is a UTF-8 AOT bytecode bundle (//# tjb:1:<base64>).\n\
             Build a debug APK:\n  ./gradlew :app:assembleDebug\n\
             Build a release APK (set TJ_ANDROID_KEYSTORE, TJ_ANDROID_KEYSTORE_PASS, TJ_ANDROID_KEY_ALIAS, TJ_ANDROID_KEY_PASS to sign):\n  ./gradlew :app:assembleRelease\n\
             Or run `tj build <app.tj> --android --assemble`.\n"
        ),
    )
    .map_err(|e| format!("cannot write README.txt: {e}"))?;
    Ok(())
}

fn assemble_android(
    dest: &Path,
    name: &str,
    release: bool,
    aot: bool,
    abis: &[String],
    notes: &mut Vec<String>,
) -> Result<PathBuf, String> {
    let gradlew = if cfg!(windows) { dest.join("gradlew.bat") } else { dest.join("gradlew") };
    if !gradlew.is_file() {
        return Err(format!(
            "Gradle wrapper not found at {}; ensure TJ_HOME points to a tinyjump checkout with hosts/android/gradlew",
            gradlew.display()
        ));
    }
    // Canonicalize TJ_ANDROID_KEYSTORE relative to caller's CWD and validate signing env vars up front.
    let mut signed_release = false;
    let mut keystore_canon: Option<PathBuf> = None;
    if release {
        if let Ok(ks) = std::env::var("TJ_ANDROID_KEYSTORE") {
            let trimmed = ks.trim();
            if !trimmed.is_empty() {
                let resolved = fs::canonicalize(trimmed)
                    .map_err(|e| format!("TJ_ANDROID_KEYSTORE ({trimmed}) cannot be opened: {e}"))?;
                for var in ["TJ_ANDROID_KEYSTORE_PASS", "TJ_ANDROID_KEY_ALIAS", "TJ_ANDROID_KEY_PASS"] {
                    if std::env::var(var).map_or(true, |v| v.is_empty()) {
                        return Err(format!("TJ_ANDROID_KEYSTORE is set but {var} is missing or empty"));
                    }
                }
                keystore_canon = Some(resolved);
                signed_release = true;
            }
        }
    }

    let variant = if release { "release" } else { "debug" };
    let apk_dir = dest.join("app/build/outputs/apk").join(variant);
    if apk_dir.exists() {
        let _ = fs::remove_dir_all(&apk_dir);
    }

    let task = if release { ":app:assembleRelease" } else { ":app:assembleDebug" };
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", "gradlew.bat"]);
        c
    } else {
        let abs_gradlew = fs::canonicalize(&gradlew).unwrap_or(gradlew.clone());
        Command::new(abs_gradlew)
    };
    cmd.current_dir(dest)
        .arg(task)
        .arg(format!("-PtjAot={}", if aot { "true" } else { "false" }))
        .arg(format!("-PtjAbis={}", abis.join(",")));
    if let Some(ks) = keystore_canon {
        cmd.env("TJ_ANDROID_KEYSTORE", strip_win_unc(&ks));
    }
    if std::env::var_os("JAVA_HOME").is_none() {
        if let Some(jh) = find_java_home() {
            cmd.env("JAVA_HOME", jh);
        }
    }
    let status = cmd
        .status()
        .map_err(|e| format!("failed to launch Gradle ({}): {e}. Ensure Java 11+ (JAVA_HOME) and Android SDK/NDK are installed", gradlew.display()))?;
    if !status.success() {
        return Err(format!("Gradle {task} failed with status {status}"));
    }
    let (expected_in, out_suffix) = if !release {
        ("app-debug.apk", ".apk")
    } else if signed_release {
        ("app-release.apk", "-release.apk")
    } else {
        notes.push("release APK is unsigned (set TJ_ANDROID_KEYSTORE, TJ_ANDROID_KEYSTORE_PASS, TJ_ANDROID_KEY_ALIAS, TJ_ANDROID_KEY_PASS to sign before installing)".into());
        ("app-release-unsigned.apk", "-release-unsigned.apk")
    };
    let built = apk_dir.join(expected_in);
    if !built.is_file() {
        return Err(format!("Gradle succeeded but expected APK was not found at {}", built.display()));
    }
    let out_apk = dest.join(format!("{name}{out_suffix}"));
    fs::copy(&built, &out_apk).map_err(|e| format!("cannot copy built APK to {}: {e}", out_apk.display()))?;
    Ok(out_apk)
}

#[allow(clippy::too_many_arguments)]
fn generate_ios_project(
    dest: &Path,
    exec_name: &str,
    app_id: &str,
    title: &str,
    version: &str,
    version_code: u32,
    bundle: &str,
    server_url: Option<&str>,
    aot: bool,
    release: bool,
    dev_perms: &BTreeMap<String, device_perms::DevicePermission>,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    let resources = dest.join("Resources");
    fs::create_dir_all(&resources).map_err(|e| format!("cannot create {}: {e}", resources.display()))?;
    fs::write(resources.join("app.tjb"), bundle).map_err(|e| format!("cannot write Resources/app.tjb: {e}"))?;

    let ats = if release {
        String::new()
    } else {
        "    <key>NSAppTransportSecurity</key>\n    <dict>\n        <key>NSAllowsLocalNetworking</key>\n        <true/>\n    </dict>\n".to_string()
    };
    let ios_perms_xml = device_perms::ios_plist_xml(dev_perms, title, escape_xml);
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n    <key>CFBundleIdentifier</key>\n    <string>{}</string>\n    <key>CFBundleName</key>\n    <string>{}</string>\n    <key>CFBundleDisplayName</key>\n    <string>{}</string>\n    <key>CFBundleExecutable</key>\n    <string>{}</string>\n    <key>CFBundlePackageType</key>\n    <string>APPL</string>\n    <key>CFBundleShortVersionString</key>\n    <string>{}</string>\n    <key>CFBundleVersion</key>\n    <string>{}</string>\n    <key>MinimumOSVersion</key>\n    <string>13.0</string>\n    <key>UIDeviceFamily</key>\n    <array>\n        <integer>1</integer>\n        <integer>2</integer>\n    </array>\n    <key>UILaunchScreen</key>\n    <dict/>\n{}{}</dict>\n</plist>\n",
        escape_xml(app_id),
        escape_xml(exec_name),
        escape_xml(title),
        escape_xml(exec_name),
        escape_xml(version),
        version_code,
        ats,
        ios_perms_xml,
    );
    fs::write(dest.join("Info.plist"), plist).map_err(|e| format!("cannot write Info.plist: {e}"))?;

    let ws = workspace_dir();
    if !ws.join("hosts/ios/Sources/TinyJump").is_dir() {
        notes.push("hosts/ios/Sources/TinyJump not found; set TJ_HOME to your tinyjump checkout before running build.sh".into());
    }
    let host_plugins = device_perms::bundled_host_plugins(dev_perms);
    let has_host_plugins = !host_plugins.is_empty();
    let dst_ios_plugins = dest.join("DevicePlugins.swift");
    if has_host_plugins {
        let src_ios_plugins = ws.join("hosts/ios/Plugins/DevicePlugins.swift");
        if src_ios_plugins.is_file() {
            fs::copy(&src_ios_plugins, &dst_ios_plugins)
                .map_err(|e| format!("cannot copy {}: {e}", src_ios_plugins.display()))?;
        } else {
            notes.push("hosts/ios/Plugins/DevicePlugins.swift not found in workspace; set TJ_HOME to your tinyjump checkout".into());
        }
    } else if dst_ios_plugins.exists() {
        let _ = fs::remove_file(&dst_ios_plugins);
    }

    let default_server_swift = match server_url {
        Some(u) => format!("\"{}\"", escape_c_string(u)),
        None => "nil".to_string(),
    };
    let ios_plugin_register = if has_host_plugins {
        let caps_list: String = host_plugins.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(", ");
        format!("        DevicePlugins.register(presenter: controller, capabilities: Set([{caps_list}]))\n")
    } else {
        String::new()
    };
    let ios_plugin_terminate = if has_host_plugins {
        "\n    func applicationWillTerminate(_ application: UIApplication) {\n        DevicePlugins.unregister()\n        app?.stop()\n    }\n"
    } else {
        ""
    };
    let app_delegate = format!(
        "import UIKit\n\n@main\nfinal class AppDelegate: UIResponder, UIApplicationDelegate {{\n    var window: UIWindow?\n    private var app: TinyJump?\n    private static let defaultServer: String? = {default_server_swift}\n\n    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {{\n        let controller = UIViewController()\n        controller.view.backgroundColor = .systemBackground\n        let window = UIWindow(frame: UIScreen.main.bounds)\n        window.rootViewController = controller\n        window.makeKeyAndVisible()\n        self.window = window\n\n{ios_plugin_register}        var server = AppDelegate.defaultServer\n        #if DEBUG\n        let defaults = UserDefaults.standard\n        switch defaults.string(forKey: \"dir\") {{\n        case \"rtl\": controller.view.semanticContentAttribute = .forceRightToLeft\n        case \"ltr\": controller.view.semanticContentAttribute = .forceLeftToRight\n        default: break\n        }}\n        if let overrideServer = defaults.string(forKey: \"server\") {{ server = overrideServer }}\n        #endif\n\n        let url = Bundle.main.url(forResource: \"app\", withExtension: \"tjb\", subdirectory: \"Resources\")\n            ?? Bundle.main.url(forResource: \"app\", withExtension: \"tjb\")\n        guard let bundleUrl = url, let source = try? String(contentsOf: bundleUrl, encoding: .utf8) else {{\n            fatalError(\"missing Resources/app.tjb\")\n        }}\n        do {{\n            app = try TinyJump.start(root: controller.view, source: source, server: server)\n        }} catch {{\n            fatalError(\"\\(error)\")\n        }}\n        #if DEBUG\n        app?.enableAutomation(port: 4701)\n        #endif\n        return true\n    }}{ios_plugin_terminate}}}\n"
    );
    fs::write(dest.join("AppDelegate.swift"), app_delegate).map_err(|e| format!("cannot write AppDelegate.swift: {e}"))?;
    let aot_block = if aot {
        "CARGO_ARGS+=(--no-default-features)\nTARGET_SUB=target/aot"
    } else {
        "TARGET_SUB=target"
    };
    let swift_debug = if release { "" } else { "SWIFT_ARGS+=(-D DEBUG)\n" };
    let extra_swift = if has_host_plugins { " \"$PWD/DevicePlugins.swift\"" } else { "" };
    let ws_default = escape_shell_single(&strip_win_unc(&ws));
    let build_sh = format!(
        "#!/usr/bin/env bash\n\
         # Builds {exec_name}.app on macOS with Xcode (no Xcode project needed).\n\
         # Usage: ./build.sh [--sim | --device]\n\
         set -euo pipefail\n\
         if [[ -n \"${{TJ_HOME:-}}\" ]]; then\n\
           TJ_HOME=\"$(cd \"$TJ_HOME\" && pwd)\"\n\
         else\n\
           TJ_HOME={ws_default}\n\
         fi\n\
         cd \"$(dirname \"$0\")\"\n\
         MODE=\"${{1:---sim}}\"\n\
         case \"$MODE\" in\n\
           --device)\n\
             SDK=iphoneos\n\
             RUST_TARGET=aarch64-apple-ios\n\
             SWIFT_TARGET=arm64-apple-ios13.0\n\
             ;;\n\
           --sim)\n\
             SDK=iphonesimulator\n\
             case \"$(uname -m)\" in\n\
               arm64) RUST_TARGET=aarch64-apple-ios-sim; SWIFT_TARGET=arm64-apple-ios13.0-simulator ;;\n\
               *) RUST_TARGET=x86_64-apple-ios; SWIFT_TARGET=x86_64-apple-ios13.0-simulator ;;\n\
             esac\n\
             ;;\n\
           *)\n\
             echo \"usage: ./build.sh [--sim | --device]\" >&2\n\
             exit 1\n\
             ;;\n\
         esac\n\
         rustup target add \"$RUST_TARGET\" >/dev/null\n\
         CARGO_ARGS=(build -p tj-ffi --profile small --target \"$RUST_TARGET\")\n\
         {aot_block}\n\
         CARGO_ARGS+=(--target-dir \"$TJ_HOME/$TARGET_SUB\")\n\
         (cd \"$TJ_HOME\" && cargo \"${{CARGO_ARGS[@]}}\")\n\
         APP=\"$PWD/{exec_name}.app\"\n\
         rm -rf \"$APP\"\n\
         mkdir -p \"$APP\"\n\
         SWIFT_ARGS=(-O -parse-as-library -target \"$SWIFT_TARGET\")\n\
         {swift_debug}\
         xcrun --sdk \"$SDK\" swiftc \"${{SWIFT_ARGS[@]}}\" \\\n\
           -import-objc-header \"$TJ_HOME/crates/tj-ffi/include/tinyjump.h\" \\\n\
           -L \"$TJ_HOME/$TARGET_SUB/$RUST_TARGET/small\" -ltj_ffi \\\n\
           \"$TJ_HOME\"/hosts/ios/Sources/TinyJump/*.swift \"$PWD/AppDelegate.swift\"{extra_swift} \\\n\
           -o \"$APP/{exec_name}\"\n\
         cp \"$PWD/Resources/app.tjb\" \"$APP/app.tjb\"\n\
         cp \"$PWD/Info.plist\" \"$APP/Info.plist\"\n\
         SIGN_ID=\"${{TJ_IOS_CODESIGN_IDENTITY:--}}\"\n\
         if [[ \"$MODE\" == \"--device\" && \"$SIGN_ID\" == \"-\" ]]; then\n\
           echo \"warning: ad-hoc signing device build; set TJ_IOS_CODESIGN_IDENTITY for real iOS devices\" >&2\n\
         fi\n\
         codesign --force --sign \"$SIGN_ID\" \"$APP\"\n\
         echo \"built $APP\"\n"
    );
    let build_sh_path = dest.join("build.sh");
    fs::write(&build_sh_path, build_sh).map_err(|e| format!("cannot write build.sh: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&build_sh_path, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot mark {} executable: {e}", build_sh_path.display()))?;
    }

    fs::write(
        dest.join("README.txt"),
        format!(
            "Turnkey iOS project for {title} ({app_id}, v{version}).\n\
             Resources/app.tjb is a UTF-8 AOT bytecode bundle (//# tjb:1:<base64>).\n\
             Build {exec_name}.app on macOS with Xcode:\n  ./build.sh           # iOS Simulator\n  ./build.sh --device  # arm64 iOS device\n\
             Or run `tj build <app.tj> --ios --assemble` on macOS.\n"
        ),
    )
    .map_err(|e| format!("cannot write README.txt: {e}"))?;
    Ok(())
}

fn assemble_ios(dest: &Path, exec_name: &str, release: bool, notes: &mut Vec<String>) -> Result<PathBuf, String> {
    if !cfg!(target_os = "macos") {
        return Err(format!(
            "--ios --assemble requires macOS with Xcode; project staged in {} — run ./build.sh on macOS",
            dest.display()
        ));
    }
    let mode = if release { "--device" } else { "--sim" };
    if release && std::env::var("TJ_IOS_CODESIGN_IDENTITY").map_or(true, |v| v.trim().is_empty()) {
        notes.push("iOS device release build is ad-hoc signed; set TJ_IOS_CODESIGN_IDENTITY to sign for real devices".into());
    }
    let mut cmd = Command::new("bash");
    cmd.args(["build.sh", mode]).current_dir(dest);
    cmd.env("TJ_HOME", strip_win_unc(&workspace_dir()));
    let status = cmd.status().map_err(|e| format!("failed to run build.sh: {e}"))?;
    if !status.success() {
        return Err(format!("iOS build.sh {mode} failed with status {status}"));
    }
    let app_bundle = dest.join(format!("{exec_name}.app"));
    if !app_bundle.join(exec_name).is_file() {
        return Err(format!("iOS build.sh succeeded but executable missing at {}", app_bundle.join(exec_name).display()));
    }
    Ok(app_bundle)
}

fn strip_win_unc(p: &Path) -> String {
    let s = p.display().to_string();
    s.strip_prefix(r"\\?\").unwrap_or(&s).replace('\\', "/")
}

fn web_index_html(title: &str, server: Option<&str>, host_plugins: &[&str], video_call: bool, whiteboard: bool, gfx: bool, has_frame: bool) -> String {
    let escaped = escape_xml(title);
    let server_opt = match server {
        Some(url) => {
            let js = serde_json::to_string(url)
                .expect("json string")
                .replace('<', "\\u003c")
                .replace('>', "\\u003e")
                .replace('&', "\\u0026");
            format!(", server: {js}")
        }
        None => String::new(),
    };
    let (plugin_import, plugin_opt) = if host_plugins.is_empty() {
        (String::new(), String::new())
    } else {
        let caps = serde_json::to_string(host_plugins).expect("json array");
        (
            "import { createDevicePlugins } from './device-plugins.js';\n".to_string(),
            format!(", plugins: createDevicePlugins({caps})"),
        )
    };
    let mut comp_imports = String::new();
    if video_call {
        comp_imports.push_str("import { createVideoCallComponents } from './webrtc-plugin.js';\n");
    }
    if whiteboard {
        comp_imports.push_str("import { createWhiteboardComponents } from './whiteboard-plugin.js';\n");
    }
    let comp_opt = match (video_call, whiteboard) {
        (true, true) => ", components: Object.assign({}, createVideoCallComponents(), createWhiteboardComponents())".to_string(),
        (true, false) => ", components: createVideoCallComponents()".to_string(),
        (false, true) => ", components: createWhiteboardComponents()".to_string(),
        (false, false) => String::new(),
    };
    let (gfx_import, gfx_opt) = if gfx {
        (
            "import { createGfxPlugin } from './gfx-plugin.js';\n".to_string(),
            format!(", gfx: createGfxPlugin({{ animate: {has_frame} }})"),
        )
    } else {
        (String::new(), String::new())
    };
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n<title>{escaped}</title>\n<style>html,body{{margin:0;padding:0;min-height:100vh;font-family:system-ui,-apple-system,sans-serif}}body{{display:flex;flex-direction:column}}#app{{flex:1;display:flex;flex-direction:column}}</style>\n</head>\n<body>\n<div id=\"app\"></div>\n<script type=\"module\">\nimport {{ start }} from './tinyjump.js';\n{plugin_import}{comp_imports}{gfx_import}fetch('./app.tjb').then((r) => {{ if (!r.ok) throw new Error('failed to load app.tjb: HTTP ' + r.status); return r.text(); }}).then((source) => start({{ wasm: './tj_wasm.wasm', source, root: document.getElementById('app'){server_opt}{plugin_opt}{comp_opt}{gfx_opt} }}));\n</script>\n</body>\n</html>\n"
    )
}

fn resolve_runner(explicit: Option<&Path>) -> Result<PathBuf, String> {
    match explicit {
        Some(p) => Ok(p.to_path_buf()),
        None => std::env::current_exe().map_err(|e| format!("cannot locate current executable: {e}")),
    }
}

fn note_standalone_sidecars(app_dir: &Path, notes: &mut Vec<String>) {
    for sidecar in ["plugins.json", "remotes.json", "webhooks.json"] {
        if app_dir.join(sidecar).exists() {
            notes.push(format!("{sidecar} is not loaded by standalone binaries yet; deploy with `tj serve <app.tj>` when using {sidecar}"));
        }
    }
}

fn collect_oci_web_assets(
    program: &tj_core::vm::program::Program,
    dev_perms: &BTreeMap<String, device_perms::DevicePermission>,
    opts: &BuildOpts<'_>,
    has_ui: bool,
    notes: &mut Vec<String>,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    if !has_ui || opts.no_web {
        return Ok(Vec::new());
    }
    let (found_wasm, used_aot_wasm, stale_note) = find_wasm(opts.wasm, opts.aot);
    if let Some(note) = stale_note {
        notes.push(note);
    }
    if let Some(explicit) = opts.wasm {
        if found_wasm.is_none() {
            return Err(format!("wasm file not found at {}", explicit.display()));
        }
    }
    if opts.aot && !used_aot_wasm && opts.wasm.is_none() {
        return Err("compiler-free AOT WASM not found; run `cargo web-aot` first (or pass --wasm <file>)".into());
    }
    let (Some(html), Some(js), Some(wasm_path)) = (find_web_host_index(), find_web_js(), found_wasm) else {
        notes.push("skipping /web/ inside OCI image (build WASM with `cargo web` / `cargo web-aot`, or pass --no-web)".into());
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (rel, path) in [("index.html", html), ("tinyjump.js", js), ("tinyjump.wasm", wasm_path)] {
        let data = fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        out.push((rel.to_string(), data));
    }
    let host_plugins = device_perms::bundled_host_plugins(dev_perms);
    let optional: [(&str, bool, Option<PathBuf>); 4] = [
        ("plugins/device.js", !host_plugins.is_empty(), find_web_device_plugins_js()),
        ("plugins/webrtc.js", device_perms::uses_video_call(program, dev_perms), find_web_webrtc_js()),
        ("plugins/whiteboard.js", device_perms::uses_whiteboard(program), find_web_whiteboard_js()),
        ("plugins/gfx.js", program.uses_scene() || program.has_frame(), find_web_gfx_js()),
    ];
    for (rel, needed, found) in optional {
        if needed {
            if let Some(p) = found {
                let data = fs::read(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
                out.push((rel.to_string(), data));
            } else {
                notes.push(format!("hosts/web/{rel} not found in workspace; skipped from /web/"));
            }
        }
    }
    if !dev_perms.is_empty() {
        let mut dev_map = serde_json::Map::new();
        for (k, perm) in dev_perms {
            let val = match &perm.reason {
                Some(r) => serde_json::Value::String(r.clone()),
                None => serde_json::Value::Bool(true),
            };
            dev_map.insert(k.clone(), val);
        }
        let doc = json!({ "device": dev_map });
        out.push(("permissions.json".to_string(), serde_json::to_vec_pretty(&doc).expect("json")));
    }
    Ok(out)
}

/// Builds a standalone executable image (`runner_base || bytecode || 48-byte SHA-256 trailer`)
/// in memory, stripping any prior verified embedded payload from `runner`.
pub fn assemble_standalone_bytes(runner: &Path, bytecode: &[u8]) -> Result<Vec<u8>, String> {
    if (bytecode.len() as u64) > MAX_EMBEDDED_BYTES {
        return Err(format!("bytecode payload ({} bytes) exceeds 64 MiB limit", bytecode.len()));
    }
    let mut base = fs::read(runner).map_err(|e| format!("cannot read runner {}: {e}", runner.display()))?;
    // If runner itself carries an embedded app, verify its SHA-256 before stripping it.
    if let Some((payload_start, expected)) = find_trailer(&base)? {
        let payload_end = base.len() - TRAILER_LEN as usize;
        if sha256(&base[payload_start..payload_end]) != expected {
            return Err("runner carries a corrupt embedded app trailer (SHA-256 mismatch)".into());
        }
        base.truncate(payload_start);
    }
    let digest = sha256(bytecode);
    let mut out = Vec::with_capacity(base.len() + bytecode.len() + TRAILER_LEN as usize);
    out.extend_from_slice(&base);
    out.extend_from_slice(bytecode);
    out.extend_from_slice(&(bytecode.len() as u64).to_le_bytes());
    out.extend_from_slice(&digest);
    out.extend_from_slice(TRAILER_MAGIC);
    Ok(out)
}

/// Appends a compiled `.tjb` payload + 48-byte SHA-256 trailer to a copy of `runner`.
/// Any existing verified embedded trailer on `runner` is stripped first.
pub fn package_standalone(runner: &Path, bytecode: &[u8], out: &Path) -> Result<(), String> {
    let image = assemble_standalone_bytes(runner, bytecode)?;
    if let Some(p) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
    }
    let mut f = fs::File::create(out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    f.write_all(&image).map_err(|e| format!("write standalone binary failed: {e}"))?;
    f.sync_all().map_err(|e| format!("sync failed: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(out, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot mark {} executable: {e}", out.display()))?;
    }
    Ok(())
}

fn find_trailer(bytes: &[u8]) -> Result<Option<(usize, [u8; 32])>, String> {
    if (bytes.len() as u64) < TRAILER_LEN {
        return Ok(None);
    }
    let t_start = bytes.len() - TRAILER_LEN as usize;
    if &bytes[bytes.len() - 8..] != TRAILER_MAGIC {
        return Ok(None);
    }
    let len = u64::from_le_bytes(bytes[t_start..t_start + 8].try_into().unwrap());
    if len > MAX_EMBEDDED_BYTES || len > t_start as u64 {
        return Err("corrupt embedded app trailer (invalid payload length)".into());
    }
    let mut expected = [0u8; 32];
    expected.copy_from_slice(&bytes[t_start + 8..t_start + 40]);
    Ok(Some((t_start - len as usize, expected)))
}

/// Extracts and SHA-256-verifies an embedded `.tjb` payload from an executable, if present.
pub fn extract_embedded(exe: &Path) -> Result<Option<Vec<u8>>, String> {
    let mut f = fs::File::open(exe).map_err(|e| format!("cannot open {}: {e}", exe.display()))?;
    let total = f.metadata().map_err(|e| format!("stat failed: {e}"))?.len();
    if total < TRAILER_LEN {
        return Ok(None);
    }
    f.seek(SeekFrom::Start(total - TRAILER_LEN)).map_err(|e| format!("seek failed: {e}"))?;
    let mut trailer = [0u8; TRAILER_LEN as usize];
    f.read_exact(&mut trailer).map_err(|e| format!("read trailer failed: {e}"))?;
    if &trailer[40..48] != TRAILER_MAGIC {
        return Ok(None);
    }
    let len = u64::from_le_bytes(trailer[0..8].try_into().unwrap());
    let body_end = total - TRAILER_LEN;
    if len > MAX_EMBEDDED_BYTES || len > body_end {
        return Err("corrupt embedded app trailer (invalid payload length)".into());
    }
    let mut expected = [0u8; 32];
    expected.copy_from_slice(&trailer[8..40]);
    f.seek(SeekFrom::Start(body_end - len)).map_err(|e| format!("seek payload failed: {e}"))?;
    let mut payload = vec![0u8; len as usize];
    f.read_exact(&mut payload).map_err(|e| format!("read payload failed: {e}"))?;
    if sha256(&payload) != expected {
        return Err("embedded app payload failed SHA-256 integrity check".into());
    }
    Ok(Some(payload))
}

/// Checks if the currently running `tj` binary has an embedded AOT `.tjb` app appended.
pub fn embedded_app() -> Result<Option<Vec<u8>>, String> {
    let Ok(exe) = std::env::current_exe() else { return Ok(None) };
    extract_embedded(&exe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat;
    use crate::manifest::Manifest;
    use tj_core::vm::program::Program;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tj-build-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn builds_bytecode_web_mobile_and_verified_standalone_executables() {
        let dir = tmp("all");
        let app_file = dir.join("app.tj");
        fs::write(
            &app_file,
            r#"
@server @read("anyone") object Notes {
  items: string[] = []
  @allow("anyone") add(t: string): number { this.items.push(t); return this.items.length }
}
function App() {
  const n = Notes.get("main")
  return <Column><Text>{n.items.length}</Text></Column>
}
"#,
        )
        .unwrap();

        // 1. Default bytecode (.tjb) target + binary read-back via app::read.
        let tjb_path = dir.join("out.tjb");
        let rep = build_app(&app_file, Target::Bytecode, &BuildOpts { out: Some(&tjb_path), ..Default::default() }).unwrap();
        assert!(tjb_path.exists());
        assert_eq!(rep["target"], json!("bytecode"));
        let raw = fs::read(&tjb_path).unwrap();
        let prog = Program::from_bytes(&raw).unwrap();
        assert_eq!(prog.objects.len(), 1);
        let read_back = app::read(&tjb_path).unwrap();
        assert!(read_back.source.starts_with(bytecode::BUNDLE_PREFIX));
        assert!(tj_core::load(&read_back.source).is_ok());
        assert!(tj_core::compile(&read_back.source).is_err());

        // Manifest::build and Snapshot::build preserve objects/methods for AOT .tjb bundles.
        let aot_manifest = Manifest::build(&read_back.source, &prog, &[]);
        assert_eq!(aot_manifest.json["aot"], json!(true));
        let objs = aot_manifest.json["objects"].as_array().unwrap();
        assert_eq!(objs.len(), 1);
        assert_eq!(objs[0]["name"], json!("Notes"));
        let snap1 = compat::Snapshot::build(&read_back.source, &prog);
        let prog_empty = tj_core::compile("function App() { return <Column /> }").unwrap();
        let snap2 = compat::Snapshot::build(&prog_empty.to_bundle().unwrap(), &prog_empty);
        assert!(compat::has_breaking(&compat::check_snapshots(&snap1, &snap2)));

        // 2. Web target writes app.tjb, index.html, tinyjump.js, manifest.json.
        let web_dir = dir.join("web");
        build_app(
            &app_file,
            Target::Web,
            &BuildOpts { out: Some(&web_dir), server_url: Some("https://api.example.com"), title: Some("My Notes"), ..Default::default() },
        )
        .unwrap();
        let html = fs::read_to_string(web_dir.join("index.html")).unwrap();
        assert!(html.contains("flex-direction:column") && html.contains("r.ok") && html.contains("https://api.example.com"));
        assert!(html.contains("<title>My Notes</title>"));
        assert!(web_dir.join("tinyjump.js").exists());
        assert!(web_dir.join("manifest.json").exists());
        let web_bundle = fs::read_to_string(web_dir.join("app.tjb")).unwrap();
        assert!(Program::from_bundle(&web_bundle).unwrap().is_ok());

        // 3. Turnkey Android project: Gradle files, strings.xml (safe against placeholders/@), AndroidManifest, MainActivity, device permissions.
        fs::write(
            dir.join("permissions.json"),
            r#"{ "device": { "camera": "Scan QR & receipts", "notifications": true } }"#,
        )
        .unwrap();
        let and_dir = dir.join("android");
        build_app(
            &app_file,
            Target::Android,
            &BuildOpts {
                out: Some(&and_dir),
                app_id: Some("com.example.notes_app"),
                title: Some("@Notes & ${foo}'s"),
                version: Some("1.2.3"),
                server_url: Some("https://api.example.com"),
                abis: Some(vec!["arm64-v8a".into()]),
                aot: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(Program::from_bundle(&fs::read_to_string(and_dir.join("assets/app.tjb")).unwrap()).unwrap().is_ok());
        let and_manifest = fs::read_to_string(and_dir.join("app/src/main/AndroidManifest.xml")).unwrap();
        assert!(and_manifest.contains("android:label=\"@string/app_name\""));
        assert!(and_manifest.contains("android.permission.CAMERA"));
        assert!(and_manifest.contains("android.permission.POST_NOTIFICATIONS"));
        let strings_xml = fs::read_to_string(and_dir.join("app/src/main/res/values/strings.xml")).unwrap();
        assert!(strings_xml.contains("\\@Notes &amp; ${foo}\\'s"), "{strings_xml}");
        let root_gradle = fs::read_to_string(and_dir.join("build.gradle.kts")).unwrap();
        assert!(root_gradle.contains("layout.buildDirectory.set(rootProject.layout.buildDirectory.dir(\"tinyjump\"))"));
        let and_gradle = fs::read_to_string(and_dir.join("app/build.gradle.kts")).unwrap();
        assert!(and_gradle.contains("applicationId = \"com.example.notes_app\""));
        assert!(and_gradle.contains("versionName = \"1.2.3\""));
        assert!(and_gradle.contains("versionCode = 10203"));
        let main_java = fs::read_to_string(and_dir.join("app/src/main/java/com/example/notes_app/MainActivity.java")).unwrap();
        assert!(main_java.contains("package com.example.notes_app;"));
        // Rebuilding into the same dir with a new --app-id cleans the stale MainActivity.java.
        build_app(
            &app_file,
            Target::Android,
            &BuildOpts { out: Some(&and_dir), app_id: Some("com.example.other"), ..Default::default() },
        )
        .unwrap();
        assert!(!and_dir.join("app/src/main/java/com/example/notes_app/MainActivity.java").exists());
        assert!(and_dir.join("app/src/main/java/com/example/other/MainActivity.java").exists());

        // 4. Turnkey iOS project: hyphenated CFBundleIdentifier, Info.plist, AppDelegate.swift, build.sh.
        let ios_dir = dir.join("ios");
        build_app(
            &app_file,
            Target::Ios,
            &BuildOpts {
                out: Some(&ios_dir),
                app_id: Some("com.example.my-notes"),
                title: Some("Notes & Tasks"),
                version: Some("2.0.1"),
                server_url: Some("https://api.example.com"),
                aot: true,
                release: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(Program::from_bundle(&fs::read_to_string(ios_dir.join("Resources/app.tjb")).unwrap()).unwrap().is_ok());
        let plist = fs::read_to_string(ios_dir.join("Info.plist")).unwrap();
        assert!(plist.contains("<string>com.example.my-notes</string>"));
        assert!(plist.contains("<string>Notes &amp; Tasks</string>"));
        assert!(plist.contains("<string>2.0.1</string>"));
        assert!(plist.contains("<key>NSCameraUsageDescription</key>"));
        assert!(plist.contains("<string>Scan QR &amp; receipts</string>"));
        assert!(!plist.contains("NSAllowsLocalNetworking"), "release Info.plist must omit local cleartext exception");
        let swift = fs::read_to_string(ios_dir.join("AppDelegate.swift")).unwrap();
        assert!(swift.contains("defaultServer: String? = \"https://api.example.com\""));
        let build_sh = fs::read_to_string(ios_dir.join("build.sh")).unwrap();
        assert!(build_sh.contains("--no-default-features") && build_sh.contains("--target-dir \"$TJ_HOME/$TARGET_SUB\"") && !build_sh.contains("-D DEBUG"));

        // 5. Standalone executable packaging: append, verify SHA-256, detect tampering, re-package cleanly.
        let fake_runner = dir.join("fake-runner");
        fs::write(&fake_runner, b"ELF-MOCK-RUNNER-BINARY").unwrap();
        let exe_out = dir.join("my-app");
        build_app(&app_file, Target::Linux, &BuildOpts { out: Some(&exe_out), runner: Some(&fake_runner), ..Default::default() }).unwrap();
        let extracted = extract_embedded(&exe_out).unwrap().expect("embedded app");
        assert_eq!(extracted, raw);
        assert!(Program::from_bytes(&extracted).is_ok());

        let exe_out2 = dir.join("my-app-2");
        package_standalone(&exe_out, &raw, &exe_out2).unwrap();
        assert_eq!(fs::metadata(&exe_out2).unwrap().len(), fs::metadata(&exe_out).unwrap().len());

        let mut tampered = fs::read(&exe_out).unwrap();
        let idx = b"ELF-MOCK-RUNNER-BINARY".len() + 2;
        tampered[idx] ^= 0xff;
        let bad_exe = dir.join("tampered");
        fs::write(&bad_exe, tampered).unwrap();
        assert!(extract_embedded(&bad_exe).unwrap_err().contains("SHA-256"));
        assert!(package_standalone(&bad_exe, &raw, &dir.join("repack-bad")).unwrap_err().contains("SHA-256"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validates_packaging_identifiers_versions_abis_and_server_urls() {
        // Android package IDs vs iOS CFBundleIdentifier rules.
        assert!(validate_app_id("com.example.booking", Target::Android).is_ok());
        assert!(validate_app_id("com.example.my_app", Target::Android).is_ok());
        assert!(validate_app_id("single", Target::Android).is_err());
        assert!(validate_app_id("com.example.class", Target::Android).is_err());
        assert!(validate_app_id("com.example.bad-hyphen", Target::Android).is_err());
        assert!(validate_app_id("com.123.app", Target::Android).is_err());

        assert!(validate_app_id("com.example.my-app", Target::Ios).is_ok());
        assert!(validate_app_id("com.example.my_app", Target::Ios).is_err());
        assert_eq!(default_app_id("durable-cart", Target::Ios), "com.mindmakr.tinyjump.durable-cart");
        assert_eq!(default_app_id("durable-cart", Target::Android), "com.mindmakr.tinyjump.durable_cart");

        // Strict ASCII digits for version; Play Store / i32 versionCode cap.
        assert_eq!(validate_version("1.2.3").unwrap(), (1, 2, 3));
        assert!(validate_version("+1.2.3").is_err());
        assert!(validate_version("1.2").is_err());
        assert!(validate_version("1.2.x").is_err());
        assert_eq!(default_version_code(999999, 0, 0), MAX_VERSION_CODE);

        assert_eq!(parse_abis("arm64-v8a, armeabi-v7a, arm64-v8a").unwrap(), vec!["arm64-v8a", "armeabi-v7a"]);
        assert!(parse_abis("mips64").is_err());

        // Server URL: blocks <script> breakout and cleartext http:// on --release mobile.
        assert!(validate_server_url("https://api.example.com", Target::Android, true).is_ok());
        assert!(validate_server_url("http://10.0.2.2:4700", Target::Android, false).is_ok());
        assert!(validate_server_url("http://10.0.2.2:4700", Target::Android, true).is_err());
        assert!(validate_server_url("http://127.0.0.1:4700", Target::Ios, true).is_err());
        assert!(validate_server_url("", Target::Web, true).is_ok());
        assert!(validate_server_url("", Target::Android, false).is_err());
        assert!(validate_server_url("https://api.example.com/</script><script>alert(1)</script>", Target::Web, false).is_err());
        assert!(validate_server_url("https://api.example.com\"; evil()", Target::Web, false).is_err());
        assert!(validate_server_url("javascript:alert(1)", Target::Web, false).is_err());

        assert_eq!(sanitize_android_segment("durable-cart"), "durable_cart");
        assert_eq!(sanitize_android_segment("class"), "class_app");
        assert_eq!(pascal_exec_name("durable-cart"), "DurableCart");
    }

    #[test]
    fn bundles_opt_in_host_device_plugins_only_when_declared() {
        let dir = tmp("opt-in-plugins");
        let app_file = dir.join("app.tj");
        fs::write(&app_file, "function App() { return <Column><Text>hi</Text></Column> }\n").unwrap();

        // 1. Without device permissions or 3D scene: 0 extra plugin files or hooks on Web, Android, iOS.
        let web_plain = dir.join("web-plain");
        build_app(&app_file, Target::Web, &BuildOpts { out: Some(&web_plain), ..Default::default() }).unwrap();
        assert!(!web_plain.join("device-plugins.js").exists());
        assert!(!web_plain.join("gfx-plugin.js").exists());
        let plain_html = fs::read_to_string(web_plain.join("index.html")).unwrap();
        assert!(!plain_html.contains("device-plugins.js"));
        assert!(!plain_html.contains("gfx-plugin.js"));

        // 1b. App using `<Scene>` bundles `gfx-plugin.js` only when opted in, and cleans stale copies when removed.
        let scene_file = dir.join("scene.tj");
        fs::write(&scene_file, "function App() { return <Scene><Mesh geometry=\"box\" /></Scene> }\n").unwrap();
        let web_3d = dir.join("web-3d");
        build_app(&scene_file, Target::Web, &BuildOpts { out: Some(&web_3d), ..Default::default() }).unwrap();
        assert!(web_3d.join("gfx-plugin.js").exists());
        let html_3d = fs::read_to_string(web_3d.join("index.html")).unwrap();
        assert!(html_3d.contains("import { createGfxPlugin } from './gfx-plugin.js';"));
        assert!(html_3d.contains("gfx: createGfxPlugin({ animate: false })"));
        // Rebuilding plain app into the same output dir cleans stale `gfx-plugin.js`.
        build_app(&app_file, Target::Web, &BuildOpts { out: Some(&web_3d), ..Default::default() }).unwrap();
        assert!(!web_3d.join("gfx-plugin.js").exists());
        assert!(!fs::read_to_string(web_3d.join("index.html")).unwrap().contains("gfx-plugin.js"));

        let and_plain = dir.join("and-plain");
        build_app(&app_file, Target::Android, &BuildOpts { out: Some(&and_plain), app_id: Some("com.example.plain"), ..Default::default() }).unwrap();
        assert!(!and_plain.join("app/src/main/java/com/mindmakr/tinyjump/plugins/DevicePlugins.java").exists());
        assert!(!fs::read_to_string(and_plain.join("app/src/main/java/com/example/plain/MainActivity.java")).unwrap().contains("DevicePlugins"));

        let ios_plain = dir.join("ios-plain");
        build_app(&app_file, Target::Ios, &BuildOpts { out: Some(&ios_plain), app_id: Some("com.example.plain"), ..Default::default() }).unwrap();
        assert!(!ios_plain.join("DevicePlugins.swift").exists());
        assert!(!fs::read_to_string(ios_plain.join("AppDelegate.swift")).unwrap().contains("DevicePlugins"));
        assert!(!fs::read_to_string(ios_plain.join("build.sh")).unwrap().contains("DevicePlugins.swift"));

        // 2. With permissions.json declaring files, notifications, location, camera: opt-in plugins are wired across all 3 platforms.
        fs::write(
            dir.join("permissions.json"),
            r#"{ "device": { "files": "Pick documents", "notifications": "Remind users", "location": "Find nearby stores", "camera": "Capture receipt" } }"#,
        )
        .unwrap();

        let web_dev = dir.join("web-dev");
        build_app(&app_file, Target::Web, &BuildOpts { out: Some(&web_dev), ..Default::default() }).unwrap();
        assert!(web_dev.join("device-plugins.js").exists());
        let html = fs::read_to_string(web_dev.join("index.html")).unwrap();
        assert!(html.contains("import { createDevicePlugins } from './device-plugins.js';"));
        assert!(html.contains("createDevicePlugins([\"files\",\"notifications\",\"location\",\"camera\"])"));

        let and_dev = dir.join("and-dev");
        build_app(&app_file, Target::Android, &BuildOpts { out: Some(&and_dev), app_id: Some("com.example.devcaps"), ..Default::default() }).unwrap();
        assert!(and_dev.join("app/src/main/java/com/mindmakr/tinyjump/plugins/DevicePlugins.java").exists());
        let main_java = fs::read_to_string(and_dev.join("app/src/main/java/com/example/devcaps/MainActivity.java")).unwrap();
        assert!(main_java.contains("DevicePlugins.install(this, new HashSet<>(Arrays.asList(\"files\", \"notifications\", \"location\", \"camera\")))"));
        assert!(main_java.contains("DevicePlugins.onActivityResult("));
        assert!(main_java.contains("DevicePlugins.onRequestPermissionsResult("));
        assert!(main_java.contains("DevicePlugins.uninstall(this)"));

        let ios_dev = dir.join("ios-dev");
        build_app(&app_file, Target::Ios, &BuildOpts { out: Some(&ios_dev), app_id: Some("com.example.devcaps"), ..Default::default() }).unwrap();
        assert!(ios_dev.join("DevicePlugins.swift").exists());
        let swift = fs::read_to_string(ios_dev.join("AppDelegate.swift")).unwrap();
        assert!(swift.contains("DevicePlugins.register(presenter: controller, capabilities: Set([\"files\", \"notifications\", \"location\", \"camera\"]))"));
        assert!(fs::read_to_string(ios_dev.join("build.sh")).unwrap().contains("\"$PWD/DevicePlugins.swift\""));

        // 3. Rebuilding in-place after removing permissions.json cleans stale plugin files and hooks.
        fs::remove_file(dir.join("permissions.json")).unwrap();
        build_app(&app_file, Target::Web, &BuildOpts { out: Some(&web_dev), ..Default::default() }).unwrap();
        assert!(!web_dev.join("device-plugins.js").exists());
        assert!(!fs::read_to_string(web_dev.join("index.html")).unwrap().contains("device-plugins.js"));

        build_app(&app_file, Target::Android, &BuildOpts { out: Some(&and_dev), app_id: Some("com.example.devcaps"), ..Default::default() }).unwrap();
        assert!(!and_dev.join("app/src/main/java/com/mindmakr/tinyjump/plugins/DevicePlugins.java").exists());
        assert!(!fs::read_to_string(and_dev.join("app/src/main/java/com/example/devcaps/MainActivity.java")).unwrap().contains("DevicePlugins"));

        build_app(&app_file, Target::Ios, &BuildOpts { out: Some(&ios_dev), app_id: Some("com.example.devcaps"), ..Default::default() }).unwrap();
        assert!(!ios_dev.join("DevicePlugins.swift").exists());
        assert!(!fs::read_to_string(ios_dev.join("AppDelegate.swift")).unwrap().contains("DevicePlugins"));
        assert!(!fs::read_to_string(ios_dev.join("build.sh")).unwrap().contains("DevicePlugins.swift"));

        // 4. WebRTC VideoCall + Whiteboard components are bundled ONLY when used.
        assert!(!web_dev.join("webrtc-plugin.js").exists());
        assert!(!web_dev.join("whiteboard-plugin.js").exists());
        fs::write(&app_file, "function App() { return <Column><Native name=\"VideoCall\" active={false} /><Native name=\"Whiteboard\" readOnly={false} /></Column> }\n").unwrap();
        fs::write(dir.join("permissions.json"), r#"{ "device": { "camera": "Video", "microphone": "Audio" } }"#).unwrap();
        build_app(&app_file, Target::Web, &BuildOpts { out: Some(&web_dev), ..Default::default() }).unwrap();
        assert!(web_dev.join("webrtc-plugin.js").exists());
        assert!(web_dev.join("whiteboard-plugin.js").exists());
        let combined_html = fs::read_to_string(web_dev.join("index.html")).unwrap();
        assert!(combined_html.contains("import { createVideoCallComponents } from './webrtc-plugin.js';"));
        assert!(combined_html.contains("import { createWhiteboardComponents } from './whiteboard-plugin.js';"));
        assert!(combined_html.contains("components: Object.assign({}, createVideoCallComponents(), createWhiteboardComponents())"));

        // Rebuilding with VideoCall only removes stale whiteboard-plugin.js and uses direct helper.
        fs::write(&app_file, "function App() { return <Column><Native name=\"VideoCall\" active={false} /></Column> }\n").unwrap();
        build_app(&app_file, Target::Web, &BuildOpts { out: Some(&web_dev), ..Default::default() }).unwrap();
        assert!(web_dev.join("webrtc-plugin.js").exists());
        assert!(!web_dev.join("whiteboard-plugin.js").exists());
        assert!(fs::read_to_string(web_dev.join("index.html")).unwrap().contains("components: createVideoCallComponents()"));

        // Rebuilding without VideoCall or Whiteboard cleans up both plugin files.
        fs::write(&app_file, "function App() { return <Column><Text>hi</Text></Column> }\n").unwrap();
        build_app(&app_file, Target::Web, &BuildOpts { out: Some(&web_dev), ..Default::default() }).unwrap();
        assert!(!web_dev.join("webrtc-plugin.js").exists());
        assert!(!web_dev.join("whiteboard-plugin.js").exists());
        let clean_html = fs::read_to_string(web_dev.join("index.html")).unwrap();
        assert!(!clean_html.contains("webrtc-plugin.js") && !clean_html.contains("whiteboard-plugin.js"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn builds_oci_layout_tar_and_validates_flags() {
        let dir = tmp("oci");
        let app_file = dir.join("app.tj");
        fs::write(
            &app_file,
            r#"
@server @read("anyone") object Room {
  @allow("anyone") ping(): string { return "ok" }
}
function App() {
  return <Column><Native name="VideoCall" active={false} /><Text>hi</Text></Column>
}
"#,
        )
        .unwrap();

        let mut elf_arm64 = vec![0u8; 64];
        elf_arm64[0..4].copy_from_slice(b"\x7fELF");
        elf_arm64[5] = 1;
        elf_arm64[18..20].copy_from_slice(&0x00B7u16.to_le_bytes());
        let runner = dir.join("runner-arm64");
        fs::write(&runner, &elf_arm64).unwrap();
        fs::write(
            dir.join("permissions.json"),
            r#"{ "device": { "camera": "Video", "microphone": "Audio" } }"#,
        )
        .unwrap();
        let wasm = dir.join("tinyjump.wasm");
        fs::write(&wasm, b"\x00asm\x01\x00\x00\x00").unwrap();

        // Flag validation across targets.
        assert!(build_app(&app_file, Target::Linux, &BuildOpts { port: Some(8080), ..Default::default() }).is_err());
        assert!(build_app(&app_file, Target::Web, &BuildOpts { arch: Some(oci::OciArch::Arm64), ..Default::default() }).is_err());
        assert!(build_app(&app_file, Target::Android, &BuildOpts { no_web: true, ..Default::default() }).is_err());

        // Full OCI layout + assembled tar with /web/ and auto-detected arm64 arch.
        let oci_dir = dir.join("out-oci");
        let rep = build_app(
            &app_file,
            Target::Oci,
            &BuildOpts {
                out: Some(&oci_dir),
                runner: Some(&runner),
                wasm: Some(&wasm),
                port: Some(8080),
                version: Some("1.0.0"),
                title: Some("Call Room"),
                assemble: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(rep["target"], json!("oci"));
        assert_eq!(rep["oci"]["arch"], json!("arm64"));
        assert_eq!(rep["oci"]["platform"], json!("linux/arm64"));
        assert_eq!(rep["oci"]["port"], json!(8080));
        let web_files = rep["oci"]["webFiles"].as_array().unwrap();
        assert!(web_files.iter().any(|v| v == "index.html"));
        assert!(web_files.iter().any(|v| v == "tinyjump.wasm"));
        assert!(web_files.iter().any(|v| v == "plugins/webrtc.js"));
        assert!(web_files.iter().any(|v| v == "permissions.json"));
        assert!(!web_files.iter().any(|v| v == "plugins/whiteboard.js"));

        let verified = oci::verify_oci_layout(&oci_dir.join("layout")).unwrap();
        assert_eq!(verified["architecture"], json!("arm64"));
        let tar_path = PathBuf::from(rep["artifact"].as_str().unwrap());
        assert!(tar_path.is_file());
        let cf = fs::read_to_string(oci_dir.join("Containerfile")).unwrap();
        assert!(
            cf.contains("USER 65532:65532")
                && cf.contains("EXPOSE 8080")
                && cf.contains("TJ_DEFAULT_PORT=8080")
                && !cf.contains("\"--port\"")
                && cf.contains("\"--web-dir\", \"/web\"")
        );

        // Reject invalid auto-derived title with control chars, and warn on dynamically linked ELF / arch mismatch.
        let mut dyn_elf = vec![0u8; 256];
        dyn_elf[0..4].copy_from_slice(b"\x7fELF");
        dyn_elf[4] = 2;
        dyn_elf[5] = 1;
        dyn_elf[18..20].copy_from_slice(&0x003Eu16.to_le_bytes());
        dyn_elf[32..40].copy_from_slice(&64u64.to_le_bytes());
        dyn_elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        dyn_elf[56..58].copy_from_slice(&1u16.to_le_bytes());
        dyn_elf[64..68].copy_from_slice(&3u32.to_le_bytes());
        dyn_elf[72..80].copy_from_slice(&160u64.to_le_bytes());
        dyn_elf[96..104].copy_from_slice(&28u64.to_le_bytes());
        dyn_elf[160..188].copy_from_slice(b"/lib64/ld-linux-x86-64.so.2\0");
        let dyn_runner = dir.join("dyn-runner");
        fs::write(&dyn_runner, &dyn_elf).unwrap();

        // Direct .tar output + --no-web omits /web/ cleanly; mismatched --arch and dynamic ELF emit clear notes.
        let tar_only = dir.join("headless.tar");
        let rep2 = build_app(
            &app_file,
            Target::Oci,
            &BuildOpts {
                out: Some(&tar_only),
                runner: Some(&dyn_runner),
                arch: Some(oci::OciArch::Arm),
                no_web: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(tar_only.is_file());
        assert_eq!(rep2["oci"]["arch"], json!("arm"));
        assert_eq!(rep2["oci"]["webFiles"], json!([]));
        let rep2_notes = rep2["notes"].as_array().unwrap();
        assert!(rep2_notes.iter().any(|n| n.as_str().is_some_and(|s| s.contains("overrides runner ELF arch"))));
        assert!(rep2_notes.iter().any(|n| n.as_str().is_some_and(|s| s.contains("dynamically linked"))));
        let outer = oci::parse_tar_entries(&fs::read(&tar_only).unwrap()).unwrap();
        assert!(outer.iter().any(|(n, _)| n == "oci-layout"));

        let _ = fs::remove_dir_all(&dir);
    }
}

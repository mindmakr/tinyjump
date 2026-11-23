//! `tj build`: ahead-of-time bytecode compilation (`.tjb`) and target packaging
//! (`--web`, `--linux`, `--android`, `--ios`).
//!
//! Devices and standalone servers boot `.tjb` bytecode directly via `Program::from_bytes` /
//! `Program::from_bundle`, skipping the lexer, parser, type-checker and compiler at startup.

use crate::{app, registry, remotes};
use serde_json::{json, Value as Json};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tj_core::crypto::sha256;
use tj_core::vm::bytecode;

/// Fixed 48-byte trailer appended to `--linux` standalone executables:
/// `[payload_len: u64 LE][sha256(payload): 32 bytes][b"TJAPP001"]`.
const TRAILER_MAGIC: &[u8; 8] = b"TJAPP001";
const TRAILER_LEN: u64 = 48;
const MAX_EMBEDDED_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Bytecode,
    Web,
    Linux,
    Android,
    Ios,
}

impl Target {
    fn as_str(self) -> &'static str {
        match self {
            Target::Bytecode => "bytecode",
            Target::Web => "web",
            Target::Linux => "linux",
            Target::Android => "android",
            Target::Ios => "ios",
        }
    }
}

pub fn run(args: &[String]) -> Result<(), String> {
    let usage = "usage: tj build <app.tj> [--out <path>] [--web | --linux | --android | --ios] [--wasm <file>] [--runner <file>] [--assemble] [--min-trust <level>] [--json]";
    let mut file: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut target = Target::Bytecode;
    let mut target_set = false;
    let mut wasm: Option<PathBuf> = None;
    let mut runner: Option<PathBuf> = None;
    let mut assemble = false;
    let mut min_trust: Option<registry::Trust> = None;
    let mut json_out = false;

    let mut it = args.iter().peekable();
    if let Some(first) = it.next_if(|a| !a.starts_with("--")) {
        file = Some(PathBuf::from(first));
    }
    let mut set_target = |t: Target| -> Result<(), String> {
        if target_set && target != t {
            return Err(format!("choose only one build target (--web, --linux, --android or --ios)\n\n{usage}"));
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
            "--wasm" => wasm = Some(PathBuf::from(value()?)),
            "--runner" => runner = Some(PathBuf::from(value()?)),
            "--assemble" => assemble = true,
            "--min-trust" => min_trust = Some(registry::Trust::parse(value()?)?),
            "--json" => json_out = true,
            other => return Err(format!("unknown option {other}\n\n{usage}")),
        }
    }
    let entry = file.ok_or_else(|| format!("`tj build` needs an app file\n\n{usage}"))?;
    let report = build_app(&entry, target, out.as_deref(), wasm.as_deref(), runner.as_deref(), assemble, min_trust)?;
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

fn workspace_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn find_web_js() -> Option<PathBuf> {
    let candidates = [workspace_dir().join("hosts/web/tinyjump.js"), PathBuf::from("hosts/web/tinyjump.js")];
    candidates.into_iter().find(|p| p.exists())
}

fn find_wasm(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return p.exists().then(|| p.to_path_buf());
    }
    let candidates = [
        workspace_dir().join("target/wasm32-unknown-unknown/small/tj_wasm.wasm"),
        PathBuf::from("target/wasm32-unknown-unknown/small/tj_wasm.wasm"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

fn build_app(
    entry: &Path,
    target: Target,
    out: Option<&Path>,
    wasm: Option<&Path>,
    runner: Option<&Path>,
    assemble: bool,
    min_trust: Option<registry::Trust>,
) -> Result<Json, String> {
    let app_dir = entry.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let (_, warnings) = registry::check_lockfile(app_dir, min_trust)?;
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
    // Enforce remote privacy-flow checks if remotes.json sits next to the app.
    let fetch_allow = crate::engine::fetch_allow_from_env()?;
    let rem = remotes::Remotes::load(entry, &program, false, true, &fetch_allow)?;
    let rem_dests = rem.destinations();
    let flow_errs = tj_core::check_remote_flows(&loaded.source, &rem_dests);
    if !flow_errs.is_empty() {
        return Err(flow_errs.iter().map(|d| d.render(&entry.display().to_string(), &loaded.source)).collect::<Vec<_>>().join("\n\n"));
    }

    let bytes = program.to_bytes()?;
    let bundle = bytecode::bundle_from_bytes(&bytes);
    let digest = sha256(&bytes);
    let sha_hex = hex_bytes(&digest);
    let name = app_name(entry);
    let objects: Vec<&str> = program.objects.iter().map(|o| &*o.name).collect();
    let has_ui = program.app.is_some();
    let mut notes: Vec<String> = Vec::new();

    let output_path = match target {
        Target::Bytecode => {
            let dest = out.map(PathBuf::from).unwrap_or_else(|| entry.with_extension("tjb"));
            if let Some(p) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
                fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
            }
            fs::write(&dest, &bytes).map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
            dest
        }
        Target::Web => {
            let dest = out.map(PathBuf::from).unwrap_or_else(|| app_dir.join("dist").join("web"));
            fs::create_dir_all(&dest).map_err(|e| format!("cannot create {}: {e}", dest.display()))?;
            fs::write(dest.join("app.tjb"), &bundle).map_err(|e| format!("cannot write app.tjb: {e}"))?;
            if let Some(js) = find_web_js() {
                fs::copy(&js, dest.join("tinyjump.js")).map_err(|e| format!("cannot copy {}: {e}", js.display()))?;
            } else {
                notes.push("tinyjump.js not found in workspace; copy hosts/web/tinyjump.js into this folder".into());
            }
            if let Some(w) = find_wasm(wasm) {
                fs::copy(&w, dest.join("tj_wasm.wasm")).map_err(|e| format!("cannot copy {}: {e}", w.display()))?;
            } else if let Some(explicit) = wasm {
                return Err(format!("wasm file not found at {}", explicit.display()));
            } else {
                notes.push("tj_wasm.wasm not found; run `cargo web` and re-run `tj build --web` (or pass --wasm <file>)".into());
            }
            let html = web_index_html(&name);
            fs::write(dest.join("index.html"), html).map_err(|e| format!("cannot write index.html: {e}"))?;
            let manifest = json!({
                "target": "web",
                "app": name,
                "sha256": sha_hex,
                "bytecodeBytes": bytes.len(),
                "bundleBytes": bundle.len(),
                "objects": objects,
                "hasUi": has_ui,
            });
            fs::write(dest.join("manifest.json"), serde_json::to_string_pretty(&manifest).expect("json"))
                .map_err(|e| format!("cannot write manifest.json: {e}"))?;
            dest
        }
        Target::Linux => {
            let runner_path = match runner {
                Some(p) => p.to_path_buf(),
                None => std::env::current_exe().map_err(|e| format!("cannot locate current executable: {e}"))?,
            };
            let runner_ext = runner_path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let default_name = if runner_ext.eq_ignore_ascii_case("exe") { format!("{name}.exe") } else { name.clone() };
            let dest = out.map(PathBuf::from).unwrap_or_else(|| app_dir.join("dist").join(default_name));
            package_standalone(&runner_path, &bytes, &dest)?;
            if cfg!(windows) && runner.is_none() {
                notes.push("packaged using the current Windows tj binary; pass --runner <path-to-linux-tj> to cross-package a Linux ELF".into());
            }
            dest
        }
        Target::Android => {
            let dest = out.map(PathBuf::from).unwrap_or_else(|| app_dir.join("dist").join("android"));
            let assets = dest.join("assets");
            fs::create_dir_all(&assets).map_err(|e| format!("cannot create {}: {e}", assets.display()))?;
            fs::write(assets.join("app.tjb"), &bundle).map_err(|e| format!("cannot write assets/app.tjb: {e}"))?;
            let manifest = json!({
                "target": "android",
                "app": name,
                "minSdk": 21,
                "sha256": sha_hex,
                "bytecodeBytes": bytes.len(),
                "bundleBytes": bundle.len(),
                "objects": objects,
                "hasUi": has_ui,
            });
            fs::write(dest.join("manifest.json"), serde_json::to_string_pretty(&manifest).expect("json"))
                .map_err(|e| format!("cannot write manifest.json: {e}"))?;
            fs::write(
                dest.join("README.txt"),
                "Load assets/app.tjb as a UTF-8 string and pass it to TinyJump.start(context, root, source).\nThe runtime boots directly from AOT bytecode without running the compiler.\n",
            )
            .map_err(|e| format!("cannot write README.txt: {e}"))?;
            if assemble {
                assemble_android()?;
            }
            dest
        }
        Target::Ios => {
            let dest = out.map(PathBuf::from).unwrap_or_else(|| app_dir.join("dist").join("ios"));
            let resources = dest.join("Resources");
            fs::create_dir_all(&resources).map_err(|e| format!("cannot create {}: {e}", resources.display()))?;
            fs::write(resources.join("app.tjb"), &bundle).map_err(|e| format!("cannot write Resources/app.tjb: {e}"))?;
            let manifest = json!({
                "target": "ios",
                "app": name,
                "minIos": "13.0",
                "sha256": sha_hex,
                "bytecodeBytes": bytes.len(),
                "bundleBytes": bundle.len(),
                "objects": objects,
                "hasUi": has_ui,
            });
            fs::write(dest.join("manifest.json"), serde_json::to_string_pretty(&manifest).expect("json"))
                .map_err(|e| format!("cannot write manifest.json: {e}"))?;
            fs::write(
                dest.join("README.txt"),
                "Bundle Resources/app.tjb into your Xcode target and pass its UTF-8 contents to TinyJump.start(root:source:).\nThe runtime boots directly from AOT bytecode without running the compiler.\n",
            )
            .map_err(|e| format!("cannot write README.txt: {e}"))?;
            if assemble {
                assemble_ios()?;
            }
            dest
        }
    };

    Ok(json!({
        "target": target.as_str(),
        "app": name,
        "output": output_path.display().to_string(),
        "sha256": sha_hex,
        "bytecodeBytes": bytes.len(),
        "bundleBytes": bundle.len(),
        "objects": objects,
        "hasUi": has_ui,
        "notes": notes,
    }))
}

fn web_index_html(title: &str) -> String {
    let escaped: String = title
        .chars()
        .map(|c| match c {
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '&' => "&amp;".into(),
            '"' => "&quot;".into(),
            _ => c.to_string(),
        })
        .collect();
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n<title>{escaped}</title>\n<style>html,body,#app{{margin:0;padding:0;min-height:100vh;font-family:system-ui,-apple-system,sans-serif}}</style>\n</head>\n<body>\n<div id=\"app\"></div>\n<script type=\"module\">\nimport {{ start }} from './tinyjump.js';\nfetch('./app.tjb').then((r) => r.text()).then((source) => start({{ wasm: './tj_wasm.wasm', source, root: document.getElementById('app') }}));\n</script>\n</body>\n</html>\n"
    )
}

/// Appends a compiled `.tjb` payload + 48-byte SHA-256 trailer to a copy of `runner`.
/// Any existing embedded trailer on `runner` is stripped first.
pub fn package_standalone(runner: &Path, bytecode: &[u8], out: &Path) -> Result<(), String> {
    if (bytecode.len() as u64) > MAX_EMBEDDED_BYTES {
        return Err(format!("bytecode payload ({} bytes) exceeds 64 MiB limit", bytecode.len()));
    }
    let mut base = fs::read(runner).map_err(|e| format!("cannot read runner {}: {e}", runner.display()))?;
    // If runner itself already carries an embedded app, strip it cleanly before appending the new one.
    if let Some((payload_start, _)) = find_trailer(&base)? {
        base.truncate(payload_start);
    }
    let digest = sha256(bytecode);
    if let Some(p) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
    }
    let mut f = fs::File::create(out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    f.write_all(&base).map_err(|e| format!("write runner failed: {e}"))?;
    f.write_all(bytecode).map_err(|e| format!("write payload failed: {e}"))?;
    f.write_all(&(bytecode.len() as u64).to_le_bytes()).map_err(|e| format!("write trailer failed: {e}"))?;
    f.write_all(&digest).map_err(|e| format!("write trailer failed: {e}"))?;
    f.write_all(TRAILER_MAGIC).map_err(|e| format!("write trailer failed: {e}"))?;
    f.sync_all().map_err(|e| format!("sync failed: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(out, fs::Permissions::from_mode(0o755));
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

fn assemble_android() -> Result<(), String> {
    let android_dir = workspace_dir().join("hosts/android");
    let gradlew = if cfg!(windows) { android_dir.join("gradlew.bat") } else { android_dir.join("gradlew") };
    if !gradlew.exists() {
        return Err(format!("Android Gradle wrapper not found at {}; bundle was staged without assembling an APK", gradlew.display()));
    }
    if std::env::var_os("ANDROID_HOME").is_none() && std::env::var_os("ANDROID_SDK_ROOT").is_none() {
        return Err("ANDROID_HOME is not set; set ANDROID_HOME to assemble an Android APK".into());
    }
    let status = std::process::Command::new(&gradlew)
        .current_dir(&android_dir)
        .arg(":demo:assembleRelease")
        .status()
        .map_err(|e| format!("failed to run Gradle: {e}"))?;
    if !status.success() {
        return Err(format!("Gradle :demo:assembleRelease failed with status {status}"));
    }
    Ok(())
}

fn assemble_ios() -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err("--ios --assemble requires macOS with Xcode; Resources/app.tjb was staged for Xcode packaging".into());
    }
    let ios_dir = workspace_dir().join("hosts/ios");
    let status = std::process::Command::new("xcodebuild")
        .current_dir(&ios_dir)
        .arg("-version")
        .status()
        .map_err(|e| format!("xcodebuild not available: {e}"))?;
    if !status.success() {
        return Err("xcodebuild failed; ensure Xcode command-line tools are installed".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let rep = build_app(&app_file, Target::Bytecode, Some(&tjb_path), None, None, false, None).unwrap();
        assert!(tjb_path.exists());
        assert_eq!(rep["target"], json!("bytecode"));
        let raw = fs::read(&tjb_path).unwrap();
        let prog = Program::from_bytes(&raw).unwrap();
        assert_eq!(prog.objects.len(), 1);
        let read_back = app::read(&tjb_path).unwrap();
        assert!(read_back.source.starts_with(bytecode::BUNDLE_PREFIX));
        assert!(tj_core::load(&read_back.source).is_ok());

        // 2. Web target writes app.tjb, index.html, manifest.json (and copies tinyjump.js).
        let web_dir = dir.join("web");
        build_app(&app_file, Target::Web, Some(&web_dir), None, None, false, None).unwrap();
        assert!(web_dir.join("index.html").exists());
        assert!(web_dir.join("tinyjump.js").exists());
        assert!(web_dir.join("manifest.json").exists());
        let web_bundle = fs::read_to_string(web_dir.join("app.tjb")).unwrap();
        assert!(Program::from_bundle(&web_bundle).unwrap().is_ok());

        // 3. Android and iOS targets stage assets/app.tjb and Resources/app.tjb.
        let and_dir = dir.join("android");
        build_app(&app_file, Target::Android, Some(&and_dir), None, None, false, None).unwrap();
        assert!(Program::from_bundle(&fs::read_to_string(and_dir.join("assets/app.tjb")).unwrap()).unwrap().is_ok());
        let ios_dir = dir.join("ios");
        build_app(&app_file, Target::Ios, Some(&ios_dir), None, None, false, None).unwrap();
        assert!(Program::from_bundle(&fs::read_to_string(ios_dir.join("Resources/app.tjb")).unwrap()).unwrap().is_ok());

        // 4. Standalone executable packaging: append, verify SHA-256, detect tampering, re-package cleanly.
        let fake_runner = dir.join("fake-runner");
        fs::write(&fake_runner, b"ELF-MOCK-RUNNER-BINARY").unwrap();
        let exe_out = dir.join("my-app");
        build_app(&app_file, Target::Linux, Some(&exe_out), None, Some(&fake_runner), false, None).unwrap();
        let extracted = extract_embedded(&exe_out).unwrap().expect("embedded app");
        assert_eq!(extracted, raw);
        assert!(Program::from_bytes(&extracted).is_ok());

        // Re-packaging from an already-packaged executable strips the old trailer cleanly.
        let exe_out2 = dir.join("my-app-2");
        package_standalone(&exe_out, &raw, &exe_out2).unwrap();
        assert_eq!(fs::metadata(&exe_out2).unwrap().len(), fs::metadata(&exe_out).unwrap().len());

        // Flipping 1 byte in the payload fails SHA-256 verification.
        let mut tampered = fs::read(&exe_out).unwrap();
        let idx = b"ELF-MOCK-RUNNER-BINARY".len() + 2;
        tampered[idx] ^= 0xff;
        let bad_exe = dir.join("tampered");
        fs::write(&bad_exe, tampered).unwrap();
        assert!(extract_embedded(&bad_exe).unwrap_err().contains("SHA-256"));

        let _ = fs::remove_dir_all(&dir);
    }
}

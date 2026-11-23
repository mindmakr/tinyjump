//! Incoming HMAC-signed webhooks and outgoing durable, retried webhooks (`webhooks.json`).
//!
//!   webhooks.json, next to <app.tj>:
//!   {
//!     "incoming": {
//!       "stripe": {
//!         "secretEnv": "STRIPE_WEBHOOK_SECRET",
//!         "object": "Billing/main",
//!         "method": "onWebhook",
//!         "caller": "webhook:stripe",
//!         "roles": ["webhook"],
//!         "maxAgeSecs": 300
//!       }
//!     },
//!     "outgoing": {
//!       "fulfillment": {
//!         "url": "https://fulfillment.example.com/hooks/tinyjump",
//!         "secretEnv": "FULFILLMENT_WEBHOOK_SECRET",
//!         "objects": ["Order/*"],
//!         "events": ["changed"],
//!         "roles": ["webhook"],
//!         "maxRetries": 5
//!       }
//!     }
//!   }

use crate::audit::{Actor, Audit, Entry};
use crate::bytes::{hex, random};
use crate::remotes::{strict_agent, validate_http_url};
use crate::store::private_file;
use ring::hmac;
use serde_json::{json, Value as Json};
use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tj_core::runtime::protocol::Caller;
use tj_core::vm::program::Program;

const MIN_SECRET_LEN: usize = 16;
const DEFAULT_MAX_AGE_SECS: i64 = 300;
const MAX_ALLOWED_AGE_SECS: i64 = 3600;
const DEFAULT_MAX_RETRIES: u32 = 5;
const MAX_ALLOWED_RETRIES: u32 = 10;
const MAX_WEBHOOK_QUEUE: usize = 1000;
const OUTGOING_TIMEOUT: Duration = Duration::from_secs(15);
const RETRY_BASE: Duration = if cfg!(test) { Duration::from_millis(50) } else { Duration::from_secs(1) };
const RETRY_CAP: Duration = if cfg!(test) { Duration::from_millis(400) } else { Duration::from_secs(60) };
const QUEUE_FILE: &str = "webhooks-queue.json";

#[derive(Clone, Debug)]
pub struct IncomingWebhook {
    pub secret: Vec<u8>,
    pub object: String,
    pub method: String,
    pub caller: String,
    pub roles: Vec<String>,
    pub max_age_secs: i64,
}

#[derive(Clone, Debug)]
pub struct OutgoingWebhook {
    pub name: String,
    pub url: String,
    pub secret: Vec<u8>,
    pub objects: Vec<String>,
    pub on_changed: bool,
    pub on_emit: bool,
    pub caller: String,
    pub roles: Vec<String>,
    pub max_retries: u32,
}

impl OutgoingWebhook {
    pub fn matches(&self, object: &str, event: &str) -> bool {
        let ev_ok = match event {
            "changed" => self.on_changed,
            "emit" => self.on_emit,
            _ => false,
        };
        if !ev_ok {
            return false;
        }
        let class = object.split('/').next().unwrap_or("");
        self.objects.iter().any(|p| p == object || p.strip_suffix("/*").is_some_and(|c| c == class))
    }

    /// Always a remote caller (never `Caller::trusted()`), so `@read` and `@pii` field masking
    /// apply to every outgoing webhook payload before it leaves the runtime.
    pub fn reader(&self) -> Caller {
        Caller::Remote { id: self.caller.clone(), roles: self.roles.clone() }
    }
}

#[derive(Clone, Default, Debug)]
pub struct Webhooks {
    pub incoming: HashMap<String, IncomingWebhook>,
    pub outgoing: Vec<OutgoingWebhook>,
}

impl Webhooks {
    pub fn load(entry: &Path, program: &Program, require_env: bool, private_fetch: bool, fetch_allow: &[String]) -> Result<Webhooks, String> {
        let file = entry.parent().unwrap_or(Path::new("")).join("webhooks.json");
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Webhooks::default()),
            Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
        };
        Self::parse(&text, &file.display().to_string(), program, require_env, private_fetch, fetch_allow)
    }

    pub fn parse(text: &str, label: &str, program: &Program, require_env: bool, private_fetch: bool, fetch_allow: &[String]) -> Result<Webhooks, String> {
        let root: Json = serde_json::from_str(text).map_err(|e| format!("{label} is not valid JSON: {e}"))?;
        let obj = root.as_object().ok_or_else(|| format!("{label} must be a JSON object with \"incoming\" and/or \"outgoing\""))?;
        for k in obj.keys() {
            if !matches!(k.as_str(), "incoming" | "outgoing") {
                return Err(format!("{label}: unknown top-level section {k:?} (expected \"incoming\" or \"outgoing\")"));
            }
        }
        let mut incoming = HashMap::new();
        if let Some(inc) = obj.get("incoming") {
            let map = inc.as_object().ok_or_else(|| format!("{label}: \"incoming\" must be a JSON object"))?;
            for (name, v) in map {
                validate_name(name).map_err(|e| format!("{label}: incoming {name:?}: {e}"))?;
                let m = v.as_object().ok_or_else(|| format!("{label}: incoming {name:?} must be an object"))?;
                let secret = read_secret_env(m, &format!("{label}: incoming {name:?}"), require_env)?;
                let object = m
                    .get("object")
                    .and_then(Json::as_str)
                    .ok_or_else(|| format!("{label}: incoming {name:?} needs \"object\" (\"Class/id\")"))?;
                let (class, id) = object
                    .split_once('/')
                    .filter(|(c, i)| !c.is_empty() && !i.is_empty() && *i != "*" && !i.contains('/'))
                    .ok_or_else(|| format!("{label}: incoming {name:?} \"object\" must be \"Class/id\", got {object:?}"))?;
                let cid = program.object(class).ok_or_else(|| format!("{label}: incoming {name:?} names unknown class {class:?}"))?;
                let def = &program.objects[cid as usize];
                if !def.server {
                    return Err(format!("{label}: incoming {name:?} target {class:?} must be an @server object"));
                }
                let method = m
                    .get("method")
                    .and_then(Json::as_str)
                    .ok_or_else(|| format!("{label}: incoming {name:?} needs \"method\""))?;
                let mdef = def
                    .methods
                    .get(method)
                    .ok_or_else(|| format!("{label}: incoming {name:?} method {class}.{method} does not exist"))?;
                if mdef.private {
                    return Err(format!("{label}: incoming {name:?} method {class}.{method} is private"));
                }
                let caller = match m.get("caller") {
                    None | Some(Json::Null) => format!("webhook:{name}"),
                    Some(Json::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
                    _ => return Err(format!("{label}: incoming {name:?} \"caller\" must be a non-empty string")),
                };
                let roles = parse_roles(m.get("roles"), &format!("{label}: incoming {name:?}"))?;
                let max_age_secs = match m.get("maxAgeSecs") {
                    None | Some(Json::Null) => DEFAULT_MAX_AGE_SECS,
                    Some(v) => {
                        let n = v.as_i64().ok_or_else(|| format!("{label}: incoming {name:?} \"maxAgeSecs\" must be a positive integer"))?;
                        if !(1..=MAX_ALLOWED_AGE_SECS).contains(&n) {
                            return Err(format!("{label}: incoming {name:?} \"maxAgeSecs\" must be between 1 and {MAX_ALLOWED_AGE_SECS}"));
                        }
                        n
                    }
                };
                incoming.insert(
                    name.clone(),
                    IncomingWebhook {
                        secret,
                        object: format!("{class}/{id}"),
                        method: method.to_string(),
                        caller,
                        roles,
                        max_age_secs,
                    },
                );
            }
        }
        let mut outgoing = Vec::new();
        if let Some(out) = obj.get("outgoing") {
            let map = out.as_object().ok_or_else(|| format!("{label}: \"outgoing\" must be a JSON object"))?;
            for (name, v) in map {
                validate_name(name).map_err(|e| format!("{label}: outgoing {name:?}: {e}"))?;
                let m = v.as_object().ok_or_else(|| format!("{label}: outgoing {name:?} must be an object"))?;
                let secret = read_secret_env(m, &format!("{label}: outgoing {name:?}"), require_env)?;
                let raw_url = m
                    .get("url")
                    .and_then(Json::as_str)
                    .ok_or_else(|| format!("{label}: outgoing {name:?} needs \"url\""))?;
                let url = validate_http_url(raw_url, true, private_fetch, fetch_allow)
                    .map_err(|e| format!("{label}: outgoing {name:?} \"url\": {e}"))?;
                let objects = m
                    .get("objects")
                    .and_then(Json::as_array)
                    .filter(|a| !a.is_empty())
                    .ok_or_else(|| format!("{label}: outgoing {name:?} needs a non-empty \"objects\" array (e.g. [\"Order/*\"])"))?;
                let mut patterns = Vec::with_capacity(objects.len());
                for item in objects {
                    let s = item.as_str().ok_or_else(|| format!("{label}: outgoing {name:?} \"objects\" entries must be strings"))?;
                    let (class, id) = s
                        .split_once('/')
                        .filter(|(c, i)| !c.is_empty() && !i.is_empty() && !i.contains('/'))
                        .ok_or_else(|| format!("{label}: outgoing {name:?} pattern {s:?} must be \"Class/id\" or \"Class/*\""))?;
                    let cid = program.object(class).ok_or_else(|| format!("{label}: outgoing {name:?} names unknown class {class:?}"))?;
                    if !program.objects[cid as usize].server {
                        return Err(format!("{label}: outgoing {name:?} class {class:?} must be @server"));
                    }
                    patterns.push(format!("{class}/{id}"));
                }
                let (mut on_changed, mut on_emit) = (false, false);
                match m.get("events") {
                    None | Some(Json::Null) => on_changed = true,
                    Some(Json::Array(arr)) if !arr.is_empty() => {
                        for e in arr {
                            match e.as_str() {
                                Some("changed") => on_changed = true,
                                Some("emit") => on_emit = true,
                                other => return Err(format!("{label}: outgoing {name:?} unknown event {other:?} (use \"changed\" and/or \"emit\")")),
                            }
                        }
                    }
                    _ => return Err(format!("{label}: outgoing {name:?} \"events\" must be a non-empty array of \"changed\" / \"emit\"")),
                }
                let caller = match m.get("caller") {
                    None | Some(Json::Null) => format!("webhook:{name}"),
                    Some(Json::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
                    _ => return Err(format!("{label}: outgoing {name:?} \"caller\" must be a non-empty string")),
                };
                let roles = parse_roles(m.get("roles"), &format!("{label}: outgoing {name:?}"))?;
                let max_retries = match m.get("maxRetries") {
                    None | Some(Json::Null) => DEFAULT_MAX_RETRIES,
                    Some(v) => {
                        let n = v.as_u64().ok_or_else(|| format!("{label}: outgoing {name:?} \"maxRetries\" must be an integer 0..={MAX_ALLOWED_RETRIES}"))?;
                        if n > u64::from(MAX_ALLOWED_RETRIES) {
                            return Err(format!("{label}: outgoing {name:?} \"maxRetries\" must be <= {MAX_ALLOWED_RETRIES}"));
                        }
                        n as u32
                    }
                };
                outgoing.push(OutgoingWebhook {
                    name: name.clone(),
                    url,
                    secret,
                    objects: patterns,
                    on_changed,
                    on_emit,
                    caller,
                    roles,
                    max_retries,
                });
            }
        }
        Ok(Webhooks { incoming, outgoing })
    }

    pub fn verify_incoming(&self, name: &str, timestamp: Option<&str>, signature: Option<&str>, raw_body: &[u8], now_secs: i64) -> Result<&IncomingWebhook, (u16, String)> {
        let Some(hook) = self.incoming.get(name) else {
            return Err((404, "unknown webhook".into()));
        };
        let Some(ts_str) = timestamp.map(str::trim).filter(|s| !s.is_empty()) else {
            return Err((401, "missing X-TinyJump-Timestamp header".into()));
        };
        let ts: i64 = ts_str.parse().map_err(|_| (401, "invalid X-TinyJump-Timestamp header".into()))?;
        if (now_secs - ts).abs() > hook.max_age_secs {
            return Err((401, "webhook timestamp outside allowed replay window".into()));
        }
        let Some(sig_hdr) = signature.map(str::trim) else {
            return Err((401, "missing X-TinyJump-Signature header".into()));
        };
        let Some(hex_sig) = sig_hdr.strip_prefix("v1=") else {
            return Err((401, "X-TinyJump-Signature must use the \"v1=<hex>\" scheme".into()));
        };
        let Some(tag) = decode_hex(hex_sig).filter(|b| b.len() == 32) else {
            return Err((401, "malformed X-TinyJump-Signature".into()));
        };
        let mut msg = Vec::with_capacity(ts_str.len() + 1 + raw_body.len());
        msg.extend_from_slice(ts_str.as_bytes());
        msg.push(b'.');
        msg.extend_from_slice(raw_body);
        let key = hmac::Key::new(hmac::HMAC_SHA256, &hook.secret);
        hmac::verify(&key, &msg, &tag).map_err(|_| (401, "invalid webhook signature".into()))?;
        Ok(hook)
    }
}

/// Computes `X-TinyJump-Signature` (`v1=<hex(HMAC-SHA256(secret, "{timestamp}.{body}"))>`).
pub fn sign_payload(secret: &[u8], timestamp: i64, raw_body: &[u8]) -> String {
    let ts = timestamp.to_string();
    let mut msg = Vec::with_capacity(ts.len() + 1 + raw_body.len());
    msg.extend_from_slice(ts.as_bytes());
    msg.push(b'.');
    msg.extend_from_slice(raw_body);
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    let tag = hmac::sign(&key, &msg);
    format!("v1={}", hex(tag.as_ref()))
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err("name must be 1-64 ASCII letters, digits, '-' or '_'".into());
    }
    Ok(())
}

fn parse_roles(v: Option<&Json>, ctx: &str) -> Result<Vec<String>, String> {
    match v {
        None | Some(Json::Null) => Ok(vec!["webhook".into()]),
        Some(Json::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                let s = item.as_str().filter(|s| !s.is_empty()).ok_or_else(|| format!("{ctx} \"roles\" entries must be non-empty strings"))?;
                out.push(s.to_string());
            }
            Ok(out)
        }
        _ => Err(format!("{ctx} \"roles\" must be an array of strings")),
    }
}

fn read_secret_env(m: &serde_json::Map<String, Json>, ctx: &str, require_env: bool) -> Result<Vec<u8>, String> {
    if m.contains_key("secret") {
        return Err(format!("{ctx} must not put a raw \"secret\" in webhooks.json; use \"secretEnv\" naming an environment variable"));
    }
    let var = m
        .get("secretEnv")
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{ctx} needs \"secretEnv\" naming an environment variable"))?;
    if !require_env {
        return Ok(Vec::new());
    }
    let val = std::env::var(var).ok().filter(|s| !s.is_empty()).ok_or_else(|| format!("{ctx} requires environment variable {var}"))?;
    if val.len() < MIN_SECRET_LEN {
        return Err(format!("{ctx}: environment variable {var} must be at least {MIN_SECRET_LEN} bytes"));
    }
    Ok(val.into_bytes())
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len() / 2);
    for pair in b.chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

pub fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

#[derive(Clone)]
struct PendingDelivery {
    id: String,
    target: String,
    body: String,
    attempts: u32,
    next_at: Instant,
}

impl PendingDelivery {
    fn to_json(&self) -> Json {
        json!({
            "id": self.id,
            "target": self.target,
            "body": self.body,
            "attempts": self.attempts,
        })
    }

    fn from_json(v: &Json) -> Option<PendingDelivery> {
        Some(PendingDelivery {
            id: v.get("id")?.as_str()?.to_string(),
            target: v.get("target")?.as_str()?.to_string(),
            body: v.get("body")?.as_str()?.to_string(),
            attempts: v.get("attempts").and_then(Json::as_u64).unwrap_or(0) as u32,
            next_at: Instant::now(),
        })
    }
}

struct QueueState {
    items: VecDeque<PendingDelivery>,
    queue_path: PathBuf,
}

impl QueueState {
    fn save(&self) {
        let arr: Vec<Json> = self.items.iter().map(PendingDelivery::to_json).collect();
        let text = serde_json::to_string(&Json::Array(arr)).unwrap_or_else(|_| "[]".into());
        let tmp = self.queue_path.with_extension("json.tmp");
        if let Some(parent) = self.queue_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = private_file().write(true).create(true).truncate(true).open(&tmp) {
            if f.write_all(text.as_bytes()).is_ok() && f.sync_all().is_ok() {
                let _ = std::fs::rename(&tmp, &self.queue_path);
            }
        }
    }
}

/// Durable background worker that delivers outgoing webhooks with HMAC-SHA256 signatures
/// and bounded exponential backoff retries across restarts.
pub struct WebhookSender {
    targets: Arc<Vec<OutgoingWebhook>>,
    state: Arc<(Mutex<QueueState>, Condvar)>,
    shutdown: Arc<AtomicBool>,
}

impl WebhookSender {
    pub fn start(
        targets: Vec<OutgoingWebhook>,
        data_dir: &Path,
        audit: Audit,
        private_fetch: bool,
        fetch_allow: Arc<Vec<String>>,
    ) -> WebhookSender {
        let queue_path = data_dir.join(QUEUE_FILE);
        let mut items = VecDeque::new();
        if !targets.is_empty() {
            if let Ok(text) = std::fs::read_to_string(&queue_path) {
                if let Ok(Json::Array(arr)) = serde_json::from_str::<Json>(&text) {
                    for v in &arr {
                        if let Some(d) = PendingDelivery::from_json(v) {
                            if targets.iter().any(|t| t.name == d.target) && items.len() < MAX_WEBHOOK_QUEUE {
                                items.push_back(d);
                            }
                        }
                    }
                }
            }
        }
        let targets = Arc::new(targets);
        let state = Arc::new((Mutex::new(QueueState { items, queue_path }), Condvar::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        if !targets.is_empty() {
            let t = targets.clone();
            let s = state.clone();
            let sd = shutdown.clone();
            let _ = thread::Builder::new().name("tj-webhooks".into()).spawn(move || {
                run_sender(t, s, sd, audit, private_fetch, fetch_allow);
            });
        }
        WebhookSender { targets, state, shutdown }
    }

    pub fn targets(&self) -> &[OutgoingWebhook] {
        &self.targets
    }

    pub fn enqueue(&self, target: &str, payload: &Json) {
        if self.targets.is_empty() {
            return;
        }
        let id = match random::<16>() {
            Ok(b) => format!("whd_{}", hex(&b)),
            Err(_) => format!("whd_{}", now_secs()),
        };
        let body = serde_json::to_string(payload).unwrap_or_else(|_| "{}".into());
        let (lock, cv) = &*self.state;
        if let Ok(mut q) = lock.lock() {
            if q.items.len() >= MAX_WEBHOOK_QUEUE {
                eprintln!("[tj] outgoing webhook queue full ({MAX_WEBHOOK_QUEUE}); dropping oldest delivery");
                q.items.pop_front();
            }
            q.items.push_back(PendingDelivery { id, target: target.to_string(), body, attempts: 0, next_at: Instant::now() });
            q.save();
            cv.notify_one();
        }
    }
}

impl Drop for WebhookSender {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.state.1.notify_all();
    }
}

fn run_sender(
    targets: Arc<Vec<OutgoingWebhook>>,
    state: Arc<(Mutex<QueueState>, Condvar)>,
    shutdown: Arc<AtomicBool>,
    audit: Audit,
    private_fetch: bool,
    fetch_allow: Arc<Vec<String>>,
) {
    let agent = strict_agent(OUTGOING_TIMEOUT, private_fetch, fetch_allow);
    let (lock, cv) = &*state;
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        let next = {
            let Ok(mut q) = lock.lock() else { return };
            let now = Instant::now();
            // Per-target FIFO: only the head delivery for each target may be attempted, so
            // retries preserve causal order while another target can still make progress.
            let mut seen_targets = std::collections::HashSet::new();
            let mut ready_pos = None;
            let mut earliest_wait: Option<Duration> = None;
            for (idx, d) in q.items.iter().enumerate() {
                if !seen_targets.insert(d.target.as_str()) {
                    continue;
                }
                if d.next_at <= now {
                    ready_pos = Some(idx);
                    break;
                }
                let w = d.next_at.saturating_duration_since(now);
                earliest_wait = Some(earliest_wait.map_or(w, |cur| cur.min(w)));
            }
            if let Some(pos) = ready_pos {
                q.items.remove(pos)
            } else {
                let wait = earliest_wait.unwrap_or(Duration::from_secs(5));
                let _ = cv.wait_timeout(q, wait.min(Duration::from_millis(500)));
                continue;
            }
        };
        let Some(mut d) = next else { continue };
        let Some(target) = targets.iter().find(|t| t.name == d.target) else {
            if let Ok(q) = lock.lock() {
                q.save();
            }
            continue;
        };
        let ts = now_secs();
        let sig = sign_payload(&target.secret, ts, d.body.as_bytes());
        let resp = agent
            .post(&target.url)
            .set("Content-Type", "application/json")
            .set("X-TinyJump-Delivery", &d.id)
            .set("X-TinyJump-Timestamp", &ts.to_string())
            .set("X-TinyJump-Signature", &sig)
            .send_string(&d.body);
        let (done, status_num, retry) = match resp {
            Ok(r) => {
                let s = r.status();
                (true, u64::from(s), false)
            }
            Err(ureq::Error::Status(s, _)) => {
                let retryable = s >= 500 || s == 408 || s == 429;
                (!retryable, u64::from(s), retryable)
            }
            Err(ureq::Error::Transport(t)) => {
                use std::error::Error as _;
                let policy = t
                    .source()
                    .and_then(|s| s.downcast_ref::<std::io::Error>())
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied);
                (policy, 0, !policy)
            }
        };
        if done {
            let ok = (200..300).contains(&status_num);
            audit.record(
                Entry::new("webhook_out")
                    .who(Actor::System)
                    .text("webhook", &target.name)
                    .num("status", status_num)
                    .ok(ok),
            );
            if let Ok(q) = lock.lock() {
                q.save();
            }
        } else if retry && d.attempts < target.max_retries {
            let delay = (RETRY_BASE * (1u32 << d.attempts.min(10))).min(RETRY_CAP);
            d.attempts += 1;
            d.next_at = Instant::now() + delay;
            if let Ok(mut q) = lock.lock() {
                q.items.push_front(d);
                q.save();
            }
        } else {
            audit.record(
                Entry::new("webhook_out")
                    .who(Actor::System)
                    .text("webhook", &target.name)
                    .num("status", status_num)
                    .ok(false),
            );
            if let Ok(q) = lock.lock() {
                q.save();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_webhooks_json_and_verifies_hmac_and_replay_window() {
        let prog = tj_core::compile(
            r#"
@server @read("anyone") object Billing {
  last: string = ""
  @allow("webhook") onWebhook(body: any) { this.last = body.id }
  private secret() {}
}
object Local { x: number = 0 }
"#,
        )
        .unwrap();
        // Raw secret rejected.
        let err = Webhooks::parse(
            r#"{"incoming":{"s":{"secret":"raw","object":"Billing/main","method":"onWebhook"}}}"#,
            "webhooks.json",
            &prog,
            false,
            false,
            &[],
        )
        .unwrap_err();
        assert!(err.contains("secretEnv"), "{err}");
        // Private method rejected.
        let err = Webhooks::parse(
            r#"{"incoming":{"s":{"secretEnv":"S_SEC","object":"Billing/main","method":"secret"}}}"#,
            "webhooks.json",
            &prog,
            false,
            false,
            &[],
        )
        .unwrap_err();
        assert!(err.contains("private"), "{err}");
        // Plain HTTP non-loopback outgoing rejected.
        let err = Webhooks::parse(
            r#"{"outgoing":{"f":{"url":"http://hooks.example.com/in","secretEnv":"S_SEC","objects":["Billing/*"]}}}"#,
            "webhooks.json",
            &prog,
            false,
            false,
            &[],
        )
        .unwrap_err();
        assert!(err.contains("https://"), "{err}");

        // Direct HMAC verification & replay window.
        let secret = b"super-secret-webhook-key-32-bytes";
        let mut wh = Webhooks::default();
        wh.incoming.insert(
            "stripe".into(),
            IncomingWebhook {
                secret: secret.to_vec(),
                object: "Billing/main".into(),
                method: "onWebhook".into(),
                caller: "webhook:stripe".into(),
                roles: vec!["webhook".into()],
                max_age_secs: 300,
            },
        );
        let body = br#"{"id":"evt_1"}"#;
        let ts = 1_700_000_000i64;
        let sig = sign_payload(secret, ts, body);
        assert!(wh.verify_incoming("stripe", Some(&ts.to_string()), Some(&sig), body, ts + 10).is_ok());
        // Tampered body fails.
        assert_eq!(wh.verify_incoming("stripe", Some(&ts.to_string()), Some(&sig), br#"{"id":"evt_2"}"#, ts + 10).unwrap_err().0, 401);
        // Expired timestamp fails (replay window).
        assert_eq!(wh.verify_incoming("stripe", Some(&ts.to_string()), Some(&sig), body, ts + 301).unwrap_err().0, 401);
        // Unknown webhook returns 404.
        assert_eq!(wh.verify_incoming("nope", Some(&ts.to_string()), Some(&sig), body, ts).unwrap_err().0, 404);
    }
}

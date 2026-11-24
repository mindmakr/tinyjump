//! HTTP front door. Every public object method is an API endpoint, object changes
//! stream as server-sent events, and the automation surface is under `/_tj/`. Every API
//! request carries a caller, checked against the app's rules (docs/access-control.md).
//!
//!   POST /api/{Class}/{id}/{method}   body (application/json): array of arguments
//!                                     -> {"ok": result} | {"error": msg}; an Idempotency-Key
//!                                     header makes repeats return the first result
//!   GET  /api/{Class}/{id}            -> {"ok": state}
//!   GET  /api/events?object=Class/id&since=N
//!                                     -> SSE stream of `emit` and `changed`; it starts with
//!                                        the listed objects changed after N (or after the
//!                                        `Last-Event-ID` header), then streams live
//!   POST /api/events/ticket           body: {"objects": [...]} -> {"ok": {"ticket": t}}, for
//!                                     browsers, whose EventSource cannot send Authorization:
//!                                     then GET /api/events?ticket=t&since=N
//!   GET  /_tj/manifest                -> canonical app contract + live health & leases (`observe`)
//!   GET  /_tj/metrics                 -> OpenMetrics counters (`observe`)
//!   GET  /_tj/inspect                 -> semantic tree + `@pii`-masked object states (`observe`)
//!   GET  /_tj/leases                  -> active work leases (`observe`)
//!   POST /_tj/leases                  -> acquire or renew a work lease (`observe`/`test`/`maintain`)
//!   DELETE /_tj/leases/{id}           -> release own lease, or any lease with `maintain`
//!   POST /_tj/input                   -> body: one protocol Input (`test` on dev; `maintain` on serve)
//!   POST /_tj/privacy                 -> export or erase a person's data (`maintain`)
//!   POST /_tj/reload                  -> shadow-boot, verify & swap live app bundle (`maintain`)
//!   POST /_tj/rollback                -> roll back to the previous known-good bundle (`maintain`)
//!   (`/_tj/*` needs a scoped `--ops` key in `TJ_KEYS` or `TJ_ADMIN_KEY` on `tj serve`; see auth.rs)
//!
//! The audit log (audit.rs) gets every API call, every refusal, and every operational request
//! that reads objects or changes state (`inspect`, `input`, `privacy`, `leases`). Refusals
//! and operational security events wait until they are on disk, and one that cannot be
//! written is refused.
//!
//! In dev mode it also serves the web host, the WASM runtime and the app source,
//! plus `/_tj/reload` (SSE) for live reload, and answers only loopback `Host`s.

use crate::audit::{Actor, Audit, Entry};
use crate::auth::{Automation, OpsAccess, OpsScope};
use crate::engine::{CallFailure, Engine, Failure, ReloadRequest};
use crate::bytes::{hex, random};
use crate::identity::{sha256_hex, Identity};
use crate::leases::Leases;
use crate::mailbox::{Event, Mailbox};
use crate::webhooks::{now_secs, Webhooks};
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use tiny_http::{Header, Method, Request, Response, Server};
use tj_core::runtime::protocol::Caller;
use tj_core::Input;

/// An API request, for the audit log if it is refused: what was asked, of which class and
/// method. Object ids are left out, as anyone can make them up.
type Asked<'a> = (&'static str, Option<&'a str>, Option<&'a str>);

const MAX_BODY: u64 = 1 << 20;
const MAX_CONNECTIONS: usize = 512;
const SSE_KEEPALIVE: Duration = Duration::from_secs(15);
/// How often an open stream re-checks its credential (expiry, revoked keys).
const RECHECK_EVERY: Duration = Duration::from_secs(60);
/// Open streams per caller, or per IP address for anonymous callers.
const STREAMS_PER_CALLER: usize = 8;
const TICKET_LIFETIME: Duration = Duration::from_secs(60);
const MAX_TICKETS: usize = 1000;
/// Per-caller HTTP `/api/*` rate limit (`TJ_RATE_LIMIT`, requests per minute; default 600 on
/// `tj serve`, 0 / off on `tj dev`; 0 disables).
pub const RATE_LIMIT_ENV: &str = "TJ_RATE_LIMIT";
const DEFAULT_RATE_LIMIT: u32 = 600;
const RATE_WINDOW: Duration = Duration::from_secs(60);
const MAX_RATE_KEYS: usize = 10_000;

struct Bucket {
    tokens: f64,
    last: Instant,
    audited_at: Option<Instant>,
}

/// Constant-memory (`O(1)` per key) token-bucket HTTP rate limiter with bounded key map.
struct RateLimiter {
    limit: u32,
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl RateLimiter {
    fn new(limit: u32) -> RateLimiter {
        RateLimiter { limit, buckets: Mutex::default() }
    }

    fn refill(b: &mut Bucket, cap: f64, now: Instant) {
        let elapsed = now.saturating_duration_since(b.last).as_secs_f64();
        let rate = cap / RATE_WINDOW.as_secs_f64();
        b.tokens = (b.tokens + elapsed * rate).min(cap);
        b.last = now;
    }

    /// Cheap check whether `key` is already exhausted (short-circuits forged-token floods from
    /// an IP before running expensive JWT signature verification).
    fn is_exhausted(&self, key: &str, now: Instant) -> Option<u64> {
        if self.limit == 0 {
            return None;
        }
        let Ok(mut map) = self.buckets.lock() else { return None };
        let b = map.get_mut(key)?;
        let cap = self.limit as f64;
        Self::refill(b, cap, now);
        if b.tokens < 1.0 {
            let rate = cap / RATE_WINDOW.as_secs_f64();
            Some(((1.0 - b.tokens) / rate).ceil().max(1.0) as u64)
        } else {
            None
        }
    }

    /// Returns `Ok(())` if allowed, or `Err((retry_after_secs, audit_now))` if over limit.
    /// `audit_now` is true at most once per caller per `RATE_WINDOW` to prevent audit flooding.
    fn check(&self, key: &str, now: Instant) -> Result<(), (u64, bool)> {
        if self.limit == 0 {
            return Ok(());
        }
        let cap = self.limit as f64;
        let rate = cap / RATE_WINDOW.as_secs_f64();
        let Ok(mut map) = self.buckets.lock() else { return Ok(()) };
        if map.len() >= MAX_RATE_KEYS && !map.contains_key(key) {
            map.retain(|_, b| {
                Self::refill(b, cap, now);
                b.tokens < cap - 0.001
            });
            if map.len() >= MAX_RATE_KEYS {
                let is_authed = !key.starts_with("ip:") && !key.starts_with("bad:");
                let victim = if is_authed {
                    map.iter()
                        .filter(|(k, _)| k.starts_with("ip:") || k.starts_with("bad:"))
                        .min_by_key(|(_, b)| b.last)
                        .or_else(|| map.iter().min_by_key(|(_, b)| b.last))
                        .map(|(k, _)| k.clone())
                } else {
                    None
                };
                if let Some(v) = victim {
                    map.remove(&v);
                } else {
                    return Err((1, false));
                }
            }
        }
        let b = map.entry(key.to_string()).or_insert(Bucket { tokens: cap, last: now, audited_at: None });
        Self::refill(b, cap, now);
        if b.tokens < 1.0 {
            let wait = ((1.0 - b.tokens) / rate).ceil().max(1.0) as u64;
            let audit_now = b.audited_at.map_or(true, |t| now.saturating_duration_since(t) >= RATE_WINDOW);
            if audit_now {
                b.audited_at = Some(now);
            }
            return Err((wait, audit_now));
        }
        b.tokens -= 1.0;
        Ok(())
    }
}

type Body = Response<std::io::Cursor<Vec<u8>>>;

/// The web host's files: served by `tj dev`, and by `tj serve --web`.
pub struct Web {
    pub source: PathBuf,
    pub web_dir: PathBuf,
    pub wasm: PathBuf,
}

/// What only `tj dev` has: the open browser tabs, told to reload when the app changes and
/// asked to answer `/_tj/inspect` and `/_tj/input`, so agents drive the real web screen.
#[derive(Default)]
pub struct Dev {
    tabs: Mutex<Vec<Arc<Mailbox>>>,
    asks: Mutex<HashMap<u64, Sender<Result<Json, String>>>>,
    next_ask: AtomicU64,
    inline_reloaded: AtomicBool,
}

/// How long a browser tab has to answer an agent. An open tab answers in milliseconds; one
/// that closed is only noticed when this runs out.
const TAB_ANSWER: Duration = if cfg!(test) { Duration::from_millis(200) } else { Duration::from_secs(3) };

impl Dev {
    pub fn notify_reload(&self) {
        if let Ok(mut tabs) = self.tabs.lock() {
            tabs.retain(|s| s.push(Event::new(String::new(), None, "event: reload\ndata: {}\n\n".into())));
        }
    }

    pub fn set_inline_reloaded(&self, v: bool) {
        self.inline_reloaded.store(v, Ordering::Relaxed);
    }

    /// Asks the newest open tab to `inspect` or take an `input`; `None` when no tab is open,
    /// and the server answers itself. A tab that does not answer is dropped, and the question
    /// goes to the next open tab, or to the server.
    fn ask(&self, kind: &str, input: &Json) -> Option<Result<Json, Failure>> {
        let id = self.next_ask.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = channel();
        self.asks.lock().ok()?.insert(id, tx);
        let text = format!("event: ask\ndata: {}\n\n", json!({ "id": id, "kind": kind, "input": input }));
        let tab = {
            let mut tabs = self.tabs.lock().ok()?;
            loop {
                match tabs.last() {
                    Some(tab) if tab.push(Event::new(String::new(), None, text.clone())) => break Some(tab.clone()),
                    Some(_) => drop(tabs.pop()),
                    None => break None,
                }
            }
        };
        let answer = tab.as_ref().map(|_| rx.recv_timeout(TAB_ANSWER));
        if let Ok(mut asks) = self.asks.lock() {
            asks.remove(&id);
        }
        Some(match answer? {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err((400, e)),
            Err(_) => {
                self.forget_tab(tab.as_ref().expect("asked a tab"));
                return self.ask(kind, input);
            }
        })
    }

    /// A tab that closed (its stream ended) or stopped answering.
    fn forget_tab(&self, tab: &Arc<Mailbox>) {
        tab.close();
        if let Ok(mut tabs) = self.tabs.lock() {
            tabs.retain(|t| !Arc::ptr_eq(t, tab));
        }
    }

    fn answer(&self, id: u64, r: Result<Json, String>) {
        if let Some(tx) = self.asks.lock().ok().and_then(|mut a| a.remove(&id)) {
            let _ = tx.send(r);
        }
    }
}

/// A one-time stream ticket, stored by the hash of its value.
struct Ticket {
    caller: Caller,
    objects: Vec<String>,
    credential: Option<String>,
    expires: Instant,
}

/// Everything a request handler needs.
pub struct Front {
    pub engine: Engine,
    pub web: Option<Arc<Web>>,
    pub dev: Option<Arc<Dev>>,
    pub automation: Automation,
    pub identity: Identity,
    pub audit: Audit,
    pub leases: Leases,
    /// Configured entry `.tj` file, reloaded when `POST /_tj/reload` omits `"source"`.
    pub app_file: Option<PathBuf>,
    /// Incoming HMAC-signed webhook endpoints (`webhooks.json`).
    pub webhooks: Arc<Webhooks>,
    /// Refuse requests that carry credentials, as this server only speaks plain HTTP and
    /// nobody said a proxy in front of it terminates TLS (see [`credentials_need_tls`]).
    pub credentials_need_tls: bool,
    /// Behind a proxy: take the client address from `X-Forwarded-For` (`TJ_TRUST_PROXY=1`).
    pub trust_proxy: bool,
    streams: Mutex<HashMap<String, usize>>,
    tickets: Mutex<HashMap<String, Ticket>>,
    rate: RateLimiter,
}

impl Front {
    #[allow(clippy::too_many_arguments)]
    pub fn new(engine: Engine, web: Option<Arc<Web>>, dev: Option<Arc<Dev>>, automation: Automation, identity: Identity, audit: Audit, leases: Leases, credentials_need_tls: bool) -> Front {
        let trust_proxy = std::env::var("TJ_TRUST_PROXY").is_ok_and(|v| v == "1");
        let default_rate = if dev.is_some() { 0 } else { DEFAULT_RATE_LIMIT };
        let limit = std::env::var(RATE_LIMIT_ENV).ok().and_then(|v| v.parse().ok()).unwrap_or(default_rate);
        Front { engine, web, dev, automation, identity, audit, leases, app_file: None, webhooks: Arc::default(), credentials_need_tls, trust_proxy, streams: Mutex::default(), tickets: Mutex::default(), rate: RateLimiter::new(limit) }
    }

    pub fn with_app_file(mut self, path: PathBuf) -> Front {
        self.app_file = Some(path);
        self
    }

    pub fn with_webhooks(mut self, webhooks: Arc<Webhooks>) -> Front {
        self.webhooks = webhooks;
        self
    }

    #[cfg(test)]
    pub fn with_rate_limit(mut self, limit: u32) -> Front {
        self.rate = RateLimiter::new(limit);
        self
    }
}

pub const TLS_TERMINATED_ENV: &str = "TJ_TLS_TERMINATED";

/// Whether `tj serve` must refuse credentials: it speaks plain HTTP, so on an address other
/// machines can reach, a token or key would cross the network readable, unless a proxy in
/// front terminates TLS (`TJ_TLS_TERMINATED=1`, which is declared, not verified). `tj dev`
/// is for development and is exempt.
pub fn credentials_need_tls(dev: bool, host: &str, tls_terminated: Option<&str>) -> bool {
    !dev && !is_loopback(host) && tls_terminated != Some("1")
}

/// Whether a bind address is reachable from this machine only.
pub fn is_loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

pub fn serve(addr: &str, front: Front) -> Result<(), String> {
    let server = Server::http(addr).map_err(|e| format!("cannot listen on {addr}: {e}"))?;
    let active = Arc::new(AtomicUsize::new(0));
    let front = Arc::new(front);
    for req in server.incoming_requests() {
        if active.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
            let _ = req.respond(json_response(503, &json!({ "error": "busy" })));
            continue;
        }
        active.fetch_add(1, Ordering::Relaxed);
        let (front, counter) = (front.clone(), active.clone());
        let spawned = thread::Builder::new().name("tj-http".into()).spawn(move || {
            handle(req, &front);
            counter.fetch_sub(1, Ordering::Relaxed);
        });
        if spawned.is_err() {
            active.fetch_sub(1, Ordering::Relaxed);
        }
    }
    Ok(())
}

fn json_response(status: u16, body: &Json) -> Body {
    Response::from_data(body.to_string()).with_status_code(status).with_header(header("Content-Type", "application/json"))
}

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).expect("static header is valid")
}

fn result_response(r: Result<Json, Failure>) -> Body {
    match r {
        Ok(v) => json_response(200, &json!({ "ok": v })),
        Err((status, e)) => json_response(status, &json!({ "error": e })),
    }
}

fn call_response(r: Result<Json, CallFailure>) -> Body {
    match r {
        Ok(v) => json_response(200, &json!({ "ok": v })),
        Err((status, e, Some(field))) => json_response(status, &json!({ "error": e, "field": field })),
        Err((status, e, None)) => json_response(status, &json!({ "error": e })),
    }
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            out.push(u8::from_str_radix(std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A JSON body. Requests that change things must say `Content-Type: application/json`,
/// which a cross-site form cannot send without the browser asking first.
fn read_json(req: &mut Request) -> Result<Json, Failure> {
    if !header_value(req, "Content-Type").is_some_and(|t| t.trim_start().to_ascii_lowercase().starts_with("application/json")) {
        return Err((415, "send the body as JSON, with `Content-Type: application/json`".into()));
    }
    let mut body = String::new();
    req.as_reader().take(MAX_BODY + 1).read_to_string(&mut body).map_err(|e| (400, format!("cannot read body: {e}")))?;
    if body.len() as u64 > MAX_BODY {
        return Err((413, "body too large".into()));
    }
    if body.trim().is_empty() {
        return Ok(Json::Null);
    }
    serde_json::from_str(&body).map_err(|e| (400, format!("body is not JSON: {e}")))
}

/// Raw JSON request bytes (capped at `MAX_BODY`) for HMAC-signed incoming webhooks, where the
/// signature must be verified over the exact wire bytes before parsing.
fn read_raw_json_bytes(req: &mut Request) -> Result<Vec<u8>, Failure> {
    if !header_value(req, "Content-Type").is_some_and(|t| t.trim_start().to_ascii_lowercase().starts_with("application/json")) {
        return Err((415, "send the body as JSON, with `Content-Type: application/json`".into()));
    }
    let mut body = Vec::new();
    req.as_reader().take(MAX_BODY + 1).read_to_end(&mut body).map_err(|e| (400, format!("cannot read body: {e}")))?;
    if body.len() as u64 > MAX_BODY {
        return Err((413, "body too large".into()));
    }
    Ok(body)
}

/// Handles `POST /webhooks/{name}`: verifies `X-TinyJump-Timestamp` + `X-TinyJump-Signature`
/// (constant-time HMAC-SHA256 over `"{timestamp}.{raw_body}"`), enforces replay window and rate
/// limits, and dispatches the payload to the configured `@server` object method as the webhook's
/// named caller under `@allow` rules.
fn handle_webhook(req: &mut Request, front: &Front, name: &str, started: Instant) -> Body {
    let ip = client_ip(req, front.trust_proxy);
    let bad_key = format!("bad:{ip}");
    // Charge `bad:{ip}` ONLY on failed requests (404 / malformed headers / 413 / 415 / 401),
    // returning 429 once exhausted and throttling `audit.security` writes — never blocking
    // valid HMAC-signed webhooks behind a shared NAT or reverse proxy (`RW-2`).
    let charge_bad = |status: u16, msg: String| -> Body {
        if front.rate.limit > 0 {
            if let Err((retry_after, audit_now)) = front.rate.check(&bad_key, started) {
                if audit_now {
                    front.audit.record(
                        Entry::new("refused")
                            .who(Actor::Invalid)
                            .text("request", "rate limit")
                            .text("webhook", name)
                            .num("status", 429),
                    );
                }
                return json_response(429, &json!({ "error": "rate limit exceeded; slow down and try again shortly" }))
                    .with_header(header("Retry-After", &retry_after.to_string()));
            }
        }
        if status == 401 {
            let _ = front.audit.security(
                Entry::new("refused")
                    .who(Actor::Invalid)
                    .text("request", "webhook")
                    .text("webhook", name)
                    .num("status", u64::from(status)),
            );
        }
        json_response(status, &json!({ "error": msg }))
    };
    // Validate webhook name, timestamp replay window, and signature hex syntax BEFORE reading
    // up to 1 MiB of body bytes (`RW-2`).
    let ts = header_value(req, "X-TinyJump-Timestamp");
    let sig = header_value(req, "X-TinyJump-Signature");
    let (hook, ts_str, tag) = match front.webhooks.parse_incoming_headers(name, ts.as_deref(), sig.as_deref(), now_secs()) {
        Ok(v) => v,
        Err((status, msg)) => return charge_bad(status, msg),
    };
    let raw = match read_raw_json_bytes(req) {
        Ok(b) => b,
        Err((s, e)) => return charge_bad(s, e),
    };
    let hook = match Webhooks::verify_incoming_body(hook, ts_str, &tag, &raw) {
        Ok(h) => h,
        Err((status, msg)) => return charge_bad(status, msg),
    };
    if front.rate.limit > 0 {
        let wh_key = format!("wh:{name}");
        if let Err((retry_after, audit_now)) = front.rate.check(&wh_key, started) {
            if audit_now {
                front.audit.record(
                    Entry::new("refused")
                        .who(Actor::id(Some(&hook.caller)))
                        .text("request", "rate limit")
                        .text("webhook", name)
                        .num("status", 429),
                );
            }
            return json_response(429, &json!({ "error": "rate limit exceeded; slow down and try again shortly" }))
                .with_header(header("Retry-After", &retry_after.to_string()));
        }
    }
    let body_json: Json = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => return json_response(400, &json!({ "error": format!("body is not JSON: {e}") })),
    };
    let delivery = header_value(req, "X-TinyJump-Delivery").or_else(|| header_value(req, "Idempotency-Key")).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let call_id = match delivery {
        Some(id) => {
            let scoped = format!("wh:{name}:{id}");
            if scoped.len() > 128 {
                return json_response(400, &json!({ "error": "X-TinyJump-Delivery header is too long" }));
            }
            Some(scoped)
        }
        None => None,
    };
    let caller = Caller::Remote { id: hook.caller.clone(), roles: hook.roles.clone() };
    let res = front.engine.request_with_field(Input::Call {
        object: hook.object.clone(),
        method: hook.method.clone(),
        args: vec![body_json],
        reply: None,
        call: call_id,
        caller: Some(caller),
    });
    if let Err((status @ (401 | 403), _, _)) = &res {
        let _ = front.audit.security(
            Entry::new("refused")
                .who(Actor::id(Some(&hook.caller)))
                .text("request", "webhook")
                .text("webhook", name)
                .num("status", u64::from(*status)),
        );
    }
    call_response(res)
}

/// `tj dev` answers only requests addressed to `localhost` or to an IP address, so a web page
/// cannot reach it through a DNS name of its own that points at this machine (DNS
/// rebinding). Devices and emulators still connect by address (e.g. 10.0.2.2).
fn trusted_host(req: &Request) -> bool {
    let Some(host) = header_value(req, "Host") else { return false };
    let name = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => host.split(':').next().unwrap_or(""),
    };
    name == "localhost" || name.parse::<std::net::IpAddr>().is_ok()
}

fn handle(mut req: Request, front: &Front) {
    let started = Instant::now();
    let path = req.url().split('?').next().unwrap_or("/").to_string();
    let segs: Option<Vec<String>> = path.trim_start_matches('/').split('/').map(percent_decode).collect();
    let Some(segs) = segs else {
        let _ = req.respond(json_response(400, &json!({ "error": "bad path encoding" })));
        return;
    };
    let segs: Vec<&str> = segs.iter().map(String::as_str).collect();
    let authorization = header_value(&req, "Authorization");
    let admin = segs.first() == Some(&"_tj");
    let mut ops_access: Option<OpsAccess> = None;
    let early = if front.dev.is_some() && !trusted_host(&req) {
        Some(("host", 403, "tj dev answers only localhost and IP addresses, not other host names".to_string()))
    } else if front.credentials_need_tls && (authorization.is_some() || segs.first() == Some(&"webhooks")) {
        Some(("plain http", 403, format!("credentials and webhooks are refused over plain HTTP, where anyone on the way can read them; put this server behind a proxy that terminates TLS and set {TLS_TERMINATED_ENV}=1")))
    } else if admin {
        match front.automation.authenticate(authorization.as_deref(), &front.identity) {
            Ok(access) => {
                ops_access = Some(access);
                None
            }
            Err((status, e)) => Some(("admin", status, e)),
        }
    } else {
        None
    };
    if let Some((asked, status, e)) = early {
        refused(front, authorization.as_deref(), None, (asked, None, None), status);
        let _ = req.respond(json_response(status, &json!({ "error": e })));
        front.engine.stats().request(started.elapsed(), false);
        return;
    }
    // Per-caller HTTP rate cap on `/api/*` (`TJ_RATE_LIMIT`, default 600/min on `tj serve`).
    // Unfiltered `GET /api/events` uses ops/admin credentials and is exempt like `/_tj/*`.
    // Anonymous callers and IPs flooding invalid non-`tjk_` tokens (OIDC JWTs) are limited
    // before expensive JWT verification; `tjk_` API keys are a single SHA-256 and are verified
    // first so valid keys behind a shared NAT/proxy are never blocked by `bad:{ip}`.
    let mut api_verified: Option<Result<crate::identity::Verified, Failure>> = None;
    // Exempt unfiltered `GET /api/events` ONLY when query params have no `object`/`ticket`
    // AND the caller actually authenticates with `ops:maintain` or `TJ_ADMIN_KEY` (cheap SHA-256
    // / admin key check, never OIDC crypto) so unauthenticated floods still hit `RateLimiter`.
    let unfiltered_ops_events = *req.method() == Method::Get
        && segs.as_slice() == ["api", "events"]
        && query_values(req.url(), "object").is_ok_and(|v| v.is_empty())
        && query_values(req.url(), "ticket").is_ok_and(|v| v.is_empty())
        && front
            .automation
            .authenticate(authorization.as_deref(), &front.identity)
            .and_then(|o| o.require(OpsScope::Maintain, front.dev.is_some(), "unfiltered /api/events"))
            .is_ok();
    if segs.first() == Some(&"api") && !unfiltered_ops_events && front.rate.limit > 0 {
        let respond_429 = |req: Request, retry_after: u64, who: Option<Actor>| {
            if let Some(who) = who {
                front.audit.record(Entry::new("refused").who(who).text("request", "rate limit").num("status", 429));
            }
            let resp = json_response(429, &json!({ "error": "rate limit exceeded; slow down and try again shortly" }))
                .with_header(header("Retry-After", &retry_after.to_string()));
            let _ = req.respond(resp);
            front.engine.stats().request(started.elapsed(), false);
        };
        if authorization.is_none() {
            let ip_key = format!("ip:{}", client_ip(&req, front.trust_proxy));
            if let Err((retry_after, audit_now)) = front.rate.check(&ip_key, started) {
                respond_429(req, retry_after, audit_now.then_some(Actor::Anonymous));
                return;
            }
        } else {
            let bad_key = format!("bad:{}", client_ip(&req, front.trust_proxy));
            let is_api_key = authorization
                .as_deref()
                .and_then(|h| h.strip_prefix("Bearer ").map(str::trim))
                .is_some_and(|t| t.starts_with(crate::identity::KEY_PREFIX));
            if !is_api_key {
                if let Some(retry_after) = front.rate.is_exhausted(&bad_key, started) {
                    respond_429(req, retry_after, None);
                    return;
                }
            }
            let verified = front.identity.verify(authorization.as_deref());
            let (rate_key, actor) = match &verified {
                Ok(v) => match &v.caller {
                    Caller::Remote { id, .. } if !id.is_empty() => (id.clone(), Actor::of(&v.caller)),
                    _ => (format!("ip:{}", client_ip(&req, front.trust_proxy)), Actor::Anonymous),
                },
                Err(_) => (bad_key, Actor::Invalid),
            };
            if let Err((retry_after, audit_now)) = front.rate.check(&rate_key, started) {
                respond_429(req, retry_after, audit_now.then_some(actor));
                return;
            }
            api_verified = Some(verified);
        }
    }
    let method = req.method().clone();
    let engine = &front.engine;
    let dev_mode = front.dev.is_some();
    let mut asked: Option<Asked> = None;
    let mut take_verified = || api_verified.take().unwrap_or_else(|| front.identity.verify(authorization.as_deref()));
    let response = match (&method, segs.as_slice()) {
        (Method::Post, ["webhooks", name]) => handle_webhook(&mut req, front, name, started),
        (Method::Get, ["api", "events"]) => return stream(req, front, authorization, api_verified),
        (Method::Post, ["api", "events", "ticket"]) => {
            asked = Some(("ticket", None, None));
            result_response(ticket(&mut req, front, authorization.as_deref(), api_verified))
        }
        (Method::Get, ["api", class, id]) => {
            asked = Some(("get", Some(class), None));
            let object = format!("{class}/{id}");
            result_response(take_verified().and_then(|v| engine.request(Input::Get { object, reply: None, caller: Some(v.caller) })))
        }
        (Method::Post, ["api", class, id, name]) => {
            asked = Some(("call", Some(class), Some(name)));
            let object = format!("{class}/{id}");
            // With an Idempotency-Key, a repeated request returns the first result without running again.
            let call = header_value(&req, "Idempotency-Key");
            let r = read_json(&mut req).and_then(|body| match body {
                Json::Null => Ok(Vec::new()),
                Json::Array(args) => Ok(args),
                _ => Err((400, "body must be a JSON array of arguments".into())),
            });
            // The engine writes the call to the audit log as it answers it; refusals are
            // written below, as security events.
            call_response(r.map_err(|(s, m)| (s, m, None)).and_then(|args| {
                let caller = take_verified().map_err(|(s, m)| (s, m, None))?.caller;
                engine.request_with_field(Input::Call { object, method: name.to_string(), args, reply: None, call, caller: Some(caller) })
            }))
        }
        // Canonical App Manifest + live health & active leases (`observe` scope).
        (Method::Get, ["_tj", "manifest"]) => {
            asked = Some(("manifest", None, None));
            let ops = ops_access.as_ref().expect("authenticated");
            result_response(ops.require(OpsScope::Observe, dev_mode, "/_tj/manifest").and_then(|()| {
                let leases: Vec<Json> = front.leases.list().into_iter().map(|l| l.json()).collect();
                engine.manifest(dev_mode, front.audit.dropped(), leases)
            }))
        }
        // A server's inspect also has its own counters: requests, latency, realtime fan-out.
        // Under `tj dev` the screen is in the browser tab, which answers when it is open; the
        // server's objects, which it holds for real, come from the server either way.
        (Method::Get, ["_tj", "inspect"]) => {
            asked = Some(("inspect", None, None));
            let ops = ops_access.as_ref().expect("authenticated");
            result_response(ops.require(OpsScope::Observe, dev_mode, "/_tj/inspect").and_then(|()| {
                front.audit.security(Entry::new("admin").who(ops.actor()).text("request", "inspect"))?;
                let server = engine.request(Input::Inspect { reply: None })?;
                match front.dev.as_ref().and_then(|d| d.ask("inspect", &Json::Null)) {
                    Some(screen) => screen.map(|mut s| {
                        s["serverObjects"] = server["objects"].clone();
                        s
                    }),
                    None => Ok(server),
                }
            }).map(|mut s| {
                s["server"] = engine.stats().json();
                s
            }))
        }
        // Active work leases: who is currently observing, testing or maintaining the app.
        (Method::Get, ["_tj", "leases"]) => {
            asked = Some(("leases", None, None));
            let ops = ops_access.as_ref().expect("authenticated");
            result_response(ops.require(OpsScope::Observe, dev_mode, "/_tj/leases").map(|()| {
                let leases: Vec<Json> = front.leases.list().into_iter().map(|l| l.json()).collect();
                json!({ "leases": leases })
            }))
        }
        (Method::Post, ["_tj", "leases"]) => {
            asked = Some(("lease acquire", None, None));
            let ops = ops_access.as_ref().expect("authenticated");
            result_response(read_json(&mut req).and_then(|j| {
                let role = match j.get("role").and_then(Json::as_str) {
                    Some(r) => OpsScope::parse(r).map_err(|e| (400, e))?,
                    None => match ops {
                        OpsAccess::Admin => OpsScope::Maintain,
                        OpsAccess::Scoped { scope, .. } => *scope,
                    },
                };
                ops.require_lease_role(role)?;
                let scope = j.get("scope").and_then(Json::as_str).unwrap_or("app").to_string();
                let exclusive = j.get("exclusive").and_then(Json::as_bool);
                let ttl_secs = j.get("ttlSecs").and_then(Json::as_u64);
                let note = j.get("note").and_then(Json::as_str).unwrap_or("").to_string();
                let holder = match (ops, j.get("holder").and_then(Json::as_str).filter(|s| !s.is_empty())) {
                    (OpsAccess::Admin, Some(h)) => h.to_string(),
                    _ => ops.holder(),
                };
                let lease = front.leases.acquire(holder.clone(), role, scope.clone(), exclusive, ttl_secs, note)?;
                if let Err(e) = front.audit.security(Entry::new("lease").who(ops.actor()).text("request", "acquire").text("role", role.as_str()).text("scope", &scope)) {
                    let _ = front.leases.release(&lease.id, &holder, true);
                    return Err(e);
                }
                Ok(lease.json())
            }))
        }
        (Method::Delete, ["_tj", "leases", id]) => {
            asked = Some(("lease release", None, None));
            let ops = ops_access.as_ref().expect("authenticated");
            result_response(ops.require(OpsScope::Observe, dev_mode, "/_tj/leases").and_then(|()| {
                let released = front.leases.release(id, &ops.holder(), ops.can_force_release())?;
                front.audit.security(Entry::new("lease").who(ops.actor()).text("request", "release").text("scope", &released.scope))?;
                Ok(json!({ "released": released.id }))
            }))
        }
        // Who the credentials (or their absence) say the caller is: the web host signs in
        // with it. Outside `/_tj/`, so a person's own key is enough.
        (Method::Get, ["api", "whoami"]) => result_response({
            asked = Some(("whoami", None, None));
            take_verified()
        }.map(|v| match v.caller {
            Caller::Remote { id, roles } => json!({ "id": id, "roles": roles, "dev": front.dev.is_some(), "devicePlugins": engine.device_plugins(), "videoCall": engine.has_video_call(), "whiteboard": engine.has_whiteboard(), "gfx": engine.has_gfx(), "hasFrame": engine.has_frame() }),
            Caller::Trusted(_) => json!({ "id": "", "roles": [], "dev": front.dev.is_some(), "devicePlugins": engine.device_plugins(), "videoCall": engine.has_video_call(), "whiteboard": engine.has_whiteboard(), "gfx": engine.has_gfx(), "hasFrame": engine.has_frame() }),
        })),
        // Prometheus and Grafana: the same statistics, without the tree and object states.
        (Method::Get, ["_tj", "metrics"]) => {
            asked = Some(("metrics", None, None));
            let ops = ops_access.as_ref().expect("authenticated");
            match ops.require(OpsScope::Observe, dev_mode, "/_tj/metrics").and_then(|()| engine.core_stats()) {
                Ok(core) => Response::from_data(engine.stats().openmetrics(&core).into_bytes()).with_header(header("Content-Type", "application/openmetrics-text; version=1.0.0; charset=utf-8")),
                Err((status, e)) => json_response(status, &json!({ "error": e })),
            }
        }
        // A person's data, or erasing it (docs/privacy.md); written to the audit log first.
        (Method::Post, ["_tj", "privacy"]) => {
            asked = Some(("privacy", None, None));
            let ops = ops_access.as_ref().expect("authenticated");
            result_response(ops.require(OpsScope::Maintain, dev_mode, "/_tj/privacy").and_then(|()| read_json(&mut req)).and_then(|j| {
                let (Some(action @ ("export" | "erase")), Some(subject)) = (j["action"].as_str(), j["subject"].as_str().filter(|s| !s.is_empty())) else {
                    return Err((400, "send {\"action\": \"export\" or \"erase\", \"subject\": \"<person id>\"}".into()));
                };
                if action == "erase" {
                    check_lease(&front.leases, ops, "app")?;
                }
                front.audit.security(Entry::new("admin").who(ops.actor()).text("request", &format!("privacy {action}")).person("subject", subject))?;
                engine.privacy(subject.to_string(), action == "erase")
            }))
        }
        (Method::Post, ["_tj", "input"]) => {
            asked = Some(("input", None, None));
            let ops = ops_access.as_ref().expect("authenticated");
            result_response(ops.require(OpsScope::Test, dev_mode, "/_tj/input").and_then(|()| read_json(&mut req)).and_then(|j| {
                let mut input = serde_json::from_value::<Input>(j.clone()).map_err(|e| (400, format!("not a protocol input: {e}")))?;
                if !matches!(input, Input::Get { .. } | Input::Inspect { .. }) {
                    let scope = match &input {
                        Input::Call { object, .. } => object.as_str(),
                        _ => "app",
                    };
                    check_lease(&front.leases, ops, scope)?;
                }
                front.audit.security(admin_input(ops.actor(), &input))?;
                // Under `tj dev`, what a person does on the screen goes to the open tab. Calls and
                // reads stay here, where the server's objects live.
                if matches!(input, Input::Ui { .. } | Input::Layout { .. } | Input::Locale { .. }) {
                    if let Some(r) = front.dev.as_ref().and_then(|d| d.ask("input", &j)) {
                        return r;
                    }
                }
                // Automation always runs as `Caller::trusted()` (never honour a body-supplied
                // `caller` identity that could bypass `@pii` masking or impersonate a user).
                if let Input::Call { caller, .. } | Input::Get { caller, .. } = &mut input {
                    *caller = Some(Caller::trusted());
                }
                engine.request(input)
            }))
        }
        // Shadow-boot, compatibility-check and swap a new app bundle (`maintain` only).
        (Method::Post, ["_tj", "reload"]) => {
            asked = Some(("reload", None, None));
            let ops = ops_access.as_ref().expect("authenticated");
            result_response(ops.require(OpsScope::Maintain, dev_mode, "/_tj/reload").and_then(|()| check_lease(&front.leases, ops, "app")).and_then(|()| read_json(&mut req)).and_then(|j| {
                if !(j.is_object() || j.is_null()) {
                    return Err((400, "body must be a JSON object".into()));
                }
                let source = match j.get("source") {
                    None | Some(Json::Null) => match &front.app_file {
                        Some(p) => crate::app::read(p).map(|a| a.source).map_err(|e| (500, format!("cannot read {}: {e}", p.display())))?,
                        None => return Err((400, "send {\"source\": \"<app bundle>\"}".into())),
                    },
                    Some(Json::String(s)) if !s.trim().is_empty() => s.clone(),
                    Some(Json::String(_)) => return Err((400, "\"source\" cannot be empty".into())),
                    Some(_) => return Err((400, "\"source\" must be a string".into())),
                };
                let allow_breaking = match j.get("allowBreaking") {
                    None | Some(Json::Null) => false,
                    Some(Json::Bool(b)) => *b,
                    Some(_) => return Err((400, "\"allowBreaking\" must be true or false".into())),
                };
                let smoke_traces: Vec<String> = match j.get("smokeTraces") {
                    None | Some(Json::Null) => Vec::new(),
                    Some(Json::Array(arr)) => arr
                        .iter()
                        .map(|v| v.as_str().map(str::to_string).ok_or_else(|| (400, "smokeTraces entries must be JSONL strings".to_string())))
                        .collect::<Result<_, _>>()?,
                    Some(_) => return Err((400, "smokeTraces must be an array of JSONL strings".into())),
                };
                let watch_secs = match j.get("watchSecs") {
                    None | Some(Json::Null) => 30,
                    Some(v) => v.as_u64().ok_or_else(|| (400, "\"watchSecs\" must be a non-negative integer".to_string()))?,
                };
                let error_threshold = match j.get("errorThreshold") {
                    None | Some(Json::Null) => 3,
                    Some(v) => v.as_u64().ok_or_else(|| (400, "\"errorThreshold\" must be a non-negative integer".to_string()))?,
                };
                // Pre-compile (or validate AOT bytecode) on the HTTP thread so bad bundles fail fast (400) without stalling `tj-runtime`.
                tj_core::load(&source).map_err(|errs| (400, errs.iter().map(|d| d.render("app.tj", &source)).collect::<Vec<_>>().join("\n\n")))?;
                let res = engine.safe_reload(ReloadRequest {
                    source,
                    allow_breaking,
                    smoke_traces,
                    watch_secs,
                    error_threshold,
                    actor: ops.actor(),
                })?;
                if let Some(dev) = &front.dev {
                    dev.set_inline_reloaded(true);
                    dev.notify_reload();
                }
                Ok(res)
            }))
        }
        // Roll back to the previous known-good bundle (`maintain` only).
        (Method::Post, ["_tj", "rollback"]) => {
            asked = Some(("rollback", None, None));
            let ops = ops_access.as_ref().expect("authenticated");
            result_response(ops.require(OpsScope::Maintain, dev_mode, "/_tj/rollback").and_then(|()| check_lease(&front.leases, ops, "app")).and_then(|()| read_json(&mut req).map(|_| ())).and_then(|()| {
                let res = engine.rollback(ops.actor())?;
                if let Some(dev) = &front.dev {
                    dev.set_inline_reloaded(true);
                    dev.notify_reload();
                }
                Ok(res)
            }))
        }
        // A tab's answer to an agent's question (dev only; JSON only, so another site's form
        // cannot post one).
        (Method::Post, ["_tj", "answer", id]) if front.dev.is_some() => result_response(read_json(&mut req).and_then(|j| {
            let id = id.parse().map_err(|_| (400, "bad answer id".to_string()))?;
            let r = match j["error"].as_str() {
                Some(e) => Err(e.to_string()),
                None => Ok(j["ok"].clone()),
            };
            front.dev.as_ref().expect("checked").answer(id, r);
            Ok(json!(null))
        })),
        (Method::Get, ["_tj", "reload"]) if front.dev.is_some() => {
            let dev = front.dev.as_ref().expect("checked");
            let events = Arc::new(Mailbox::new(Some(Vec::new()), None));
            if let Ok(mut tabs) = dev.tabs.lock() {
                tabs.push(events.clone());
            }
            sse(req, &events, || true);
            dev.forget_tab(&events);
            return;
        }
        (Method::Get, rest) if front.web.is_some() => {
            let web = front.web.as_deref().expect("checked");
            match rest {
                [""] | ["index.html"] => file(web.web_dir.join("index.html"), "text/html; charset=utf-8"),
                ["tinyjump.js"] => file(web.web_dir.join("tinyjump.js"), "text/javascript; charset=utf-8"),
                ["device-plugins.js"] => {
                    if engine.device_plugins().is_empty() {
                        Response::from_data("export function createDevicePlugins(){return{};}\n")
                            .with_header(header("Content-Type", "text/javascript; charset=utf-8"))
                            .with_header(header("Cache-Control", "no-store"))
                    } else {
                        file(web.web_dir.join("plugins/device.js"), "text/javascript; charset=utf-8")
                    }
                }
                ["webrtc-plugin.js"] => {
                    if !engine.has_video_call() {
                        Response::from_data("export function createVideoCallComponents(){return{};}\n")
                            .with_header(header("Content-Type", "text/javascript; charset=utf-8"))
                            .with_header(header("Cache-Control", "no-store"))
                    } else {
                        file(web.web_dir.join("plugins/webrtc.js"), "text/javascript; charset=utf-8")
                    }
                }
                ["whiteboard-plugin.js"] => {
                    if !engine.has_whiteboard() {
                        Response::from_data("export function createWhiteboardComponents(){return{};}\n")
                            .with_header(header("Content-Type", "text/javascript; charset=utf-8"))
                            .with_header(header("Cache-Control", "no-store"))
                    } else {
                        file(web.web_dir.join("plugins/whiteboard.js"), "text/javascript; charset=utf-8")
                    }
                }
                ["gfx-plugin.js"] => {
                    if !engine.has_gfx() {
                        Response::from_data("export function createGfxPlugin(){return null;}\n")
                            .with_header(header("Content-Type", "text/javascript; charset=utf-8"))
                            .with_header(header("Cache-Control", "no-store"))
                    } else {
                        file(web.web_dir.join("plugins/gfx.js"), "text/javascript; charset=utf-8")
                    }
                }
                ["tinyjump.wasm"] => file(web.wasm.clone(), "application/wasm"),
                // The whole app as one bundle: the entry and the files it imports (or the live
                // reloaded bundle on `tj serve --web`).
                ["app.tj"] => {
                    let body = if front.dev.as_ref().is_some_and(|d| !d.inline_reloaded.load(Ordering::Relaxed)) {
                        crate::app::read(&web.source).map(|app| app.source)
                    } else {
                        Ok(engine.current_source())
                    };
                    match body {
                        Ok(src) => Response::from_data(src).with_header(header("Content-Type", "text/plain; charset=utf-8")).with_header(header("Cache-Control", "no-store")),
                        Err(e) => json_response(404, &json!({ "error": e })),
                    }
                }
                _ => json_response(404, &json!({ "error": "not found" })),
            }
        }
        _ => json_response(404, &json!({ "error": "not found" })),
    };
    let status = response.status_code().0;
    if let Some(asked) = asked.filter(|_| matches!(status, 401 | 403)) {
        refused(front, authorization.as_deref(), ops_access.as_ref(), asked, status);
    }
    let _ = req.respond(response);
    engine.stats().request(started.elapsed(), status < 400);
}

fn check_lease(leases: &Leases, ops: &OpsAccess, scope: &str) -> Result<(), Failure> {
    match ops {
        OpsAccess::Admin => Ok(()),
        OpsAccess::Scoped { .. } => leases.check_exclusive(&ops.holder(), scope),
    }
}

/// A refusal, written to the audit log before the answer goes out: who (re-checked here, so
/// only refusals pay for it), what was asked, and the status. Never credentials, bodies or
/// arguments. The request is refused either way, so a log that cannot be written changes
/// nothing; the writer reports that itself.
fn refused(front: &Front, authorization: Option<&str>, ops: Option<&OpsAccess>, (asked, class, method): Asked, status: u16) {
    let who = match ops {
        Some(o) => o.actor(),
        None => match front.identity.verify(authorization) {
            Ok(v) => Actor::of(&v.caller),
            Err(_) => authorization
                .and_then(|h| h.strip_prefix("Bearer ").map(str::trim))
                .filter(|t| t.starts_with(crate::identity::KEY_PREFIX))
                .and_then(|t| front.identity.verify_ops_key(t).ok().flatten())
                .map_or(Actor::Invalid, |(name, _)| Actor::Ops(name)),
        },
    };
    let mut e = Entry::new("refused").who(who).text("request", asked).num("status", status.into());
    if let Some(class) = class {
        e = e.text("class", class);
    }
    if let Some(method) = method {
        e = e.text("method", method);
    }
    let _ = front.audit.security(e);
}

/// An operational input, for the audit log: what it does and to which object, never its values.
fn admin_input(who: Actor, input: &Input) -> Entry {
    let e = Entry::new("admin").who(who);
    match input {
        Input::Call { object, method, .. } => e.text("request", "call").object(object).text("method", method),
        Input::Get { object, .. } => e.text("request", "get").object(object),
        Input::Ui { event, .. } => e.text("request", "ui").text("event", event),
        _ => e.text("request", "input"),
    }
}

fn header_value(req: &Request, name: &'static str) -> Option<String> {
    req.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.as_str().to_string())
}

fn file(path: PathBuf, content_type: &str) -> Body {
    match std::fs::read(&path) {
        Ok(bytes) => Response::from_data(bytes).with_header(header("Content-Type", content_type)).with_header(header("Cache-Control", "no-store")),
        Err(e) => json_response(404, &json!({ "error": format!("{}: {e}", path.display()) })),
    }
}

/// All values of a query parameter, percent-decoded; a value that does not decode is an
/// error, never silently dropped (a dropped `object=` must not widen a stream).
fn query_values(url: &str, name: &str) -> Result<Vec<String>, Failure> {
    let Some((_, query)) = url.split_once('?') else { return Ok(Vec::new()) };
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .filter(|(k, _)| *k == name)
        .map(|(_, v)| percent_decode(&v.replace('+', " ")).ok_or_else(|| (400, format!("bad {name}= value"))))
        .collect()
}

/// The classes of refused objects, for the audit log. Object ids are never written: they can
/// be anything a caller makes up, and a person's object id is their id.
fn classes(objects: &[String]) -> String {
    let mut classes: Vec<&str> = objects.iter().map(|o| o.split_once('/').map_or("?", |(c, _)| c)).collect();
    classes.sort_unstable();
    classes.dedup();
    classes.join(" ")
}

/// `POST /api/events/ticket`: a one-time ticket for a stream of the listed objects, bound to
/// the caller and those objects, stored only by its hash.
fn ticket(req: &mut Request, front: &Front, authorization: Option<&str>, verified: Option<Result<crate::identity::Verified, Failure>>) -> Result<Json, Failure> {
    let body = read_json(req)?;
    let objects: Vec<String> = body["objects"].as_array().map(|a| a.iter().filter_map(|o| o.as_str().map(str::to_string)).collect()).unwrap_or_default();
    if objects.is_empty() {
        return Err((400, "list the objects to stream: {\"objects\": [\"Class/id\", ...]}".into()));
    }
    let verified = verified.unwrap_or_else(|| front.identity.verify(authorization))?;
    // The stream checks again when it opens, and names any it refuses.
    let readable = front.engine.may_read(objects.clone(), verified.caller.clone())?;
    if readable.len() < objects.len() {
        let who = match &verified.caller {
            Caller::Remote { id, .. } => Actor::id(Some(id)),
            Caller::Trusted(_) => Actor::Admin,
        };
        let refused: Vec<String> = objects.iter().filter(|o| !readable.contains(o)).cloned().collect();
        let _ = front.audit.security(Entry::new("refused").who(who).text("request", "ticket").text("classes", &classes(&refused)).num("status", 403));
    }
    let value = hex(&random::<32>().map_err(|e| (500, e))?);
    let mut tickets = front.tickets.lock().map_err(|_| (500, "tickets unavailable".to_string()))?;
    tickets.retain(|_, t| t.expires > Instant::now());
    if tickets.len() >= MAX_TICKETS {
        return Err((503, "too many pending stream tickets; try again shortly".into()));
    }
    let credential = authorization.map(str::to_string);
    tickets.insert(sha256_hex(&value), Ticket { caller: verified.caller, objects, credential, expires: Instant::now() + TICKET_LIFETIME });
    Ok(json!({ "ticket": value }))
}

enum StreamAuth {
    User(String),
    Ops(String),
}

/// An admitted stream: its events, its slot in the per-caller cap, and the credential to
/// re-check while it is open.
struct Opened {
    events: Arc<Mailbox>,
    slot: Option<String>,
    auth: Option<StreamAuth>,
}

/// `GET /api/events`: checks who may stream what, caps streams per caller, then streams.
fn stream(req: Request, front: &Front, authorization: Option<String>, mut api_verified: Option<Result<crate::identity::Verified, Failure>>) {
    let mut ops_access: Option<OpsAccess> = None;
    let opened = (|| -> Result<Opened, Failure> {
        let since_header = header_value(&req, "Last-Event-ID");
        let since = since_header.into_iter().chain(query_values(req.url(), "since")?).find_map(|s| s.trim().parse().ok()).unwrap_or(0);
        let ticket = query_values(req.url(), "ticket")?.into_iter().next();
        let (caller, objects, auth) = match ticket {
            Some(value) => {
                let t = front.tickets.lock().ok().and_then(|mut m| m.remove(&sha256_hex(&value))).filter(|t| t.expires > Instant::now());
                let t = t.ok_or((401, "this stream ticket is unknown, used or expired; get a new one".to_string()))?;
                (Some(t.caller), t.objects, t.credential.map(StreamAuth::User))
            }
            None => {
                let objects = query_values(req.url(), "object")?;
                if objects.is_empty() {
                    // Every object, unfiltered, requires `maintain` or the admin key.
                    let ops = front.automation.authenticate(authorization.as_deref(), &front.identity).map_err(|_| (403, "list the objects to stream: ?object=Class/id".to_string()))?;
                    ops_access = Some(ops.clone());
                    ops.require(OpsScope::Maintain, front.dev.is_some(), "unfiltered /api/events").map_err(|_| (403, "list the objects to stream: ?object=Class/id".to_string()))?;
                    front.audit.security(Entry::new("admin").who(ops.actor()).text("request", "stream"))?;
                    let auth = matches!(ops, OpsAccess::Scoped { .. }).then(|| authorization.clone()).flatten().map(StreamAuth::Ops);
                    (None, objects, auth)
                } else {
                    let v = api_verified.take().unwrap_or_else(|| front.identity.verify(authorization.as_deref()))?;
                    (Some(v.caller), objects, authorization.clone().map(StreamAuth::User))
                }
            }
        };
        let slot = match &caller {
            Some(Caller::Remote { id, .. }) if !id.is_empty() => Some(id.clone()),
            Some(_) => Some(format!("ip:{}", client_ip(&req, front.trust_proxy))),
            None => None,
        };
        if let Some(slot) = &slot {
            let mut open = front.streams.lock().map_err(|_| (500, "streams unavailable".to_string()))?;
            let n = open.entry(slot.clone()).or_insert(0);
            if *n >= STREAMS_PER_CALLER {
                return Err((429, format!("at most {STREAMS_PER_CALLER} open streams per caller")));
            }
            *n += 1;
        }
        let caller_id = match &caller {
            Some(Caller::Remote { id, .. }) => Some(id.clone()),
            _ => None,
        };
        match front.engine.subscribe(objects, since, caller) {
            Ok((events, refused)) => {
                // Partly refused is still a refusal, written to the audit log.
                if !refused.is_empty() {
                    let who = caller_id.as_deref().map_or(Actor::Anonymous, |id| Actor::id(Some(id)));
                    let _ = front.audit.security(Entry::new("refused").who(who).text("request", "stream").text("classes", &classes(&refused)).num("status", 403));
                }
                let keep_auth = match auth {
                    Some(StreamAuth::User(c)) => caller_id.is_some().then_some(StreamAuth::User(c)),
                    other => other,
                };
                Ok(Opened { events, slot, auth: keep_auth })
            }
            Err(e) => {
                release(front, slot.as_deref());
                Err(e)
            }
        }
    })();
    match opened {
        Ok(Opened { events, slot, auth }) => {
            // Re-check the credential while the stream is open: expired tokens and revoked
            // keys (including scoped `ops:maintain` keys) must not keep receiving changes.
            let first = match &auth {
                Some(StreamAuth::User(c)) => Some(front.identity.verify(Some(c)).ok().map(|v| (v.caller, v.expires))),
                _ => None,
            };
            let still_valid = || match (&auth, &first) {
                (Some(StreamAuth::User(c)), Some(Some((caller, expires)))) => {
                    expires.map_or(true, |t| SystemTime::now() < t) && front.identity.verify(Some(c)).is_ok_and(|v| v.caller == *caller)
                }
                (Some(StreamAuth::User(_)), _) => false,
                (Some(StreamAuth::Ops(c)), _) => front
                    .automation
                    .authenticate(Some(c), &front.identity)
                    .and_then(|o| o.require(OpsScope::Maintain, front.dev.is_some(), "unfiltered /api/events"))
                    .is_ok(),
                (None, _) => true,
            };
            sse(req, &events, still_valid);
            release(front, slot.as_deref());
        }
        Err((status, e)) => {
            if matches!(status, 401 | 403) {
                refused(front, authorization.as_deref(), ops_access.as_ref(), ("stream", None, None), status);
            }
            let _ = req.respond(json_response(status, &json!({ "error": e })));
        }
    }
}

fn release(front: &Front, slot: Option<&str>) {
    if let (Some(slot), Ok(mut open)) = (slot, front.streams.lock()) {
        if let Some(n) = open.get_mut(slot) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                open.remove(slot);
            }
        }
    }
}

/// Client IP address: behind a single trusted reverse proxy (`TJ_TRUST_PROXY=1`), uses the
/// rightmost valid IP in the **last** `X-Forwarded-For` header (the entry appended by the proxy,
/// not an attacker-controlled leftmost value or earlier duplicate header), falling back to peer IP.
fn client_ip(req: &Request, trust_proxy: bool) -> String {
    if trust_proxy {
        if let Some(ip) = req
            .headers()
            .iter()
            .rev()
            .find(|h| h.field.equiv("X-Forwarded-For"))
            .and_then(|h| {
                h.value
                    .as_str()
                    .rsplit(',')
                    .map(str::trim)
                    .find(|s| !s.is_empty())
                    .and_then(|s| s.parse::<std::net::IpAddr>().ok())
            })
        {
            return ip.to_string();
        }
    }
    req.remote_addr().map_or_else(|| "unknown".into(), |a| a.ip().to_string())
}

/// Streams server-sent events from `events` until the client goes away or `still_valid`
/// says its credential no longer is (checked every minute).
fn sse(req: Request, events: &Mailbox, still_valid: impl Fn() -> bool) {
    let mut w = req.into_writer();
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n";
    let mut ok = w.write_all(head.as_bytes()).and_then(|()| w.flush()).is_ok();
    events.connected(true);
    let mut checked = Instant::now();
    while ok {
        let batch = events.take(SSE_KEEPALIVE);
        if events.is_closed() {
            break;
        }
        if checked.elapsed() >= RECHECK_EVERY {
            checked = Instant::now();
            if !still_valid() {
                break;
            }
        }
        let chunk: String = if batch.is_empty() { ":\n\n".to_string() } else { batch.iter().map(|e| e.text.as_str()).collect() };
        ok = w.write_all(chunk.as_bytes()).and_then(|()| w.flush()).is_ok();
        if ok {
            events.delivered(&batch);
        }
    }
    events.connected(false);
    events.close();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_reach_the_open_tab_and_the_server_answers_when_none_is_open() {
        let dev = Arc::new(Dev::default());
        assert!(dev.ask("inspect", &Json::Null).is_none(), "no tab: the server answers");
        let tab = Arc::new(Mailbox::new(Some(Vec::new()), None));
        dev.tabs.lock().unwrap().push(tab.clone());
        // The tab reads the question and posts its answer.
        let answering = {
            let dev = dev.clone();
            thread::spawn(move || {
                let asked = tab.take(Duration::from_secs(5));
                let text = &asked[0].text;
                let data: Json = serde_json::from_str(text.split("data: ").nth(1).unwrap().trim()).unwrap();
                assert_eq!(data["kind"], json!("input"));
                dev.answer(data["id"].as_u64().unwrap(), Ok(json!({ "pressed": data["input"]["target"]["id"] })));
            })
        };
        let r = dev.ask("input", &json!({ "type": "ui", "target": { "id": "add" }, "event": "onPress" }));
        answering.join().unwrap();
        assert_eq!(r, Some(Ok(json!({ "pressed": "add" }))));

        // A tab that stops answering (closed, reloading) is dropped, and the server answers.
        assert!(dev.ask("inspect", &Json::Null).is_none());
        assert!(dev.tabs.lock().unwrap().is_empty());
    }

    #[test]
    fn credentials_need_tls_on_reachable_servers_only() {
        assert!(credentials_need_tls(false, "0.0.0.0", None));
        assert!(credentials_need_tls(false, "10.1.2.3", Some("yes")), "only 1 says so");
        assert!(!credentials_need_tls(false, "0.0.0.0", Some("1")));
        assert!(!credentials_need_tls(false, "127.0.0.1", None));
        assert!(!credentials_need_tls(true, "0.0.0.0", None), "tj dev is for development");
    }

    #[test]
    fn bad_object_values_are_errors_not_dropped() {
        assert_eq!(query_values("/api/events?object=Room%2Flobby&object=A/b", "object").unwrap(), ["Room/lobby", "A/b"]);
        assert_eq!(query_values("/api/events?object=%zz", "object").unwrap_err().0, 400);
        assert!(query_values("/api/events", "object").unwrap().is_empty());
    }

    #[test]
    fn scoped_ops_keys_manifest_and_leases_work_end_to_end() {
        let dir = std::env::temp_dir().join(format!("tj-http-ops-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data = dir.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let keys_file = dir.join("keys.json");
        let obs = crate::identity::new_key("critic", &[], Some(OpsScope::Observe), &keys_file).unwrap();
        let tst = crate::identity::new_key("tester", &[], Some(OpsScope::Test), &keys_file).unwrap();
        let mnt = crate::identity::new_key("maint", &[], Some(OpsScope::Maintain), &keys_file).unwrap();
        let mnt2 = crate::identity::new_key("maint2", &[], Some(OpsScope::Maintain), &keys_file).unwrap();
        let ana = crate::identity::new_key("ana", &[], None, &keys_file).unwrap();

        let source = r#"
@server @read("owner") object Account {
  @pii("contact", "account") email: string = ""
  @allow("owner") set(e: string) { this.email = e }
}
"#
        .to_string();
        let (audit, _) = Audit::start(&data).unwrap();
        let engine = Engine::start(source, data.clone(), false, false, Arc::default(), audit.clone()).unwrap();
        let identity = Identity::from_key_file(&keys_file).unwrap();
        let front = Arc::new(Front::new(engine, None, None, Automation::Off, identity, audit, Leases::open(&data), false));

        let server = Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let srv_front = front.clone();
        thread::spawn(move || {
            for req in server.incoming_requests() {
                handle(req, &srv_front);
            }
        });

        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(5)).build();
        let call = |method: &str, path: &str, token: Option<&str>, body: Option<Json>| -> (u16, Json) {
            let mut req = agent.request(method, &format!("{base}{path}"));
            if let Some(t) = token {
                req = req.set("Authorization", &format!("Bearer {t}"));
            }
            let resp = match body {
                Some(j) => req.set("Content-Type", "application/json").send_string(&j.to_string()),
                None => req.call(),
            };
            let (status, r) = match resp {
                Ok(r) => (r.status(), r),
                Err(ureq::Error::Status(s, r)) => (s, r),
                Err(e) => panic!("request {method} {path} failed: {e}"),
            };
            (status, serde_json::from_str(&r.into_string().unwrap()).unwrap())
        };

        // Ana saves PII via the normal app API; `--ops` keys are refused on `/api/*` (L1-1).
        assert_eq!(call("POST", "/api/Account/key:ana/set", Some(&ana), Some(json!(["ana@example.com"]))).0, 200);
        assert_eq!(call("POST", "/api/Account/key:critic/set", Some(&obs), Some(json!(["bad@example.com"]))).0, 403);
        assert_eq!(call("POST", "/api/Account/key:tester/set", Some(&tst), Some(json!(["bad@example.com"]))).0, 403);

        // Unfiltered `/api/events` refuses `ops:observe` (L1-2).
        assert_eq!(call("GET", "/api/events", Some(&obs), None).0, 403);

        // Plain user key & unauthenticated requests are refused on /_tj/*.
        assert_eq!(call("GET", "/_tj/manifest", None, None).0, 401);
        assert_eq!(call("GET", "/_tj/manifest", Some(&ana), None).0, 403);

        // `ops:observe` reads manifest, inspect (with @pii masked), metrics and leases; refused on input & privacy.
        let (st, m) = call("GET", "/_tj/manifest", Some(&obs), None);
        assert_eq!(st, 200);
        assert_eq!(m["ok"]["objects"][0]["name"], json!("Account"));
        assert_eq!(m["ok"]["health"]["mode"], json!("serve"));
        assert_eq!(m["ok"]["health"]["objectsLoaded"], json!(1));

        let (st, ins) = call("GET", "/_tj/inspect", Some(&obs), None);
        assert_eq!(st, 200);
        let acct = ins["ok"]["objects"].as_array().unwrap().iter().find(|o| o["object"] == "Account/key:ana").expect("Account loaded");
        assert_eq!(acct["state"]["email"], json!("•••"), "PII is masked for ops:observe");

        assert_eq!(call("POST", "/_tj/input", Some(&obs), Some(json!({ "type": "tick" }))).0, 403);
        assert_eq!(call("POST", "/_tj/privacy", Some(&obs), Some(json!({ "action": "export", "subject": "key:ana" }))).0, 403);

        // `ops:test` is refused on live `tj serve` /_tj/input and /_tj/privacy.
        assert_eq!(call("POST", "/_tj/input", Some(&tst), Some(json!({ "type": "tick" }))).0, 403);
        assert_eq!(call("POST", "/_tj/privacy", Some(&tst), Some(json!({ "action": "export", "subject": "key:ana" }))).0, 403);

        // Leases: observer acquires observe lease, cannot acquire maintain lease; maintainer acquires exclusive lease and conflicts with admin.
        let (st, l_obs) = call("POST", "/_tj/leases", Some(&obs), Some(json!({ "role": "observe", "scope": "app", "note": "review" })));
        assert_eq!(st, 200);
        assert_eq!(l_obs["ok"]["holder"], json!("ops:critic"));
        assert_eq!(call("POST", "/_tj/leases", Some(&obs), Some(json!({ "role": "maintain", "scope": "app" }))).0, 403);

        let (st, l_mnt) = call("POST", "/_tj/leases", Some(&mnt), Some(json!({ "role": "maintain", "scope": "Account", "note": "migration" })));
        assert_eq!(st, 200);
        let mnt_id = l_mnt["ok"]["id"].as_str().unwrap().to_string();
        // Observer cannot release maintainer's lease; maintainer can release observer's lease.
        assert_eq!(call("DELETE", &format!("/_tj/leases/{mnt_id}"), Some(&obs), None).0, 403);
        let obs_id = l_obs["ok"]["id"].as_str().unwrap().to_string();
        assert_eq!(call("DELETE", &format!("/_tj/leases/{obs_id}"), Some(&mnt), None).0, 200);

        // Live manifest includes the active lease.
        let (st, m2) = call("GET", "/_tj/manifest", Some(&obs), None);
        assert_eq!(st, 200);
        assert_eq!(m2["ok"]["health"]["activeLeases"], json!(1));
        assert_eq!(m2["ok"]["leases"][0]["holder"], json!("ops:maint"));

        // `ops:maintain` holder can run privacy & /_tj/input; competing `ops:maint2` gets 409 on mutating calls while lease is held, while read-only diagnostics stay open (L1-3).
        assert_eq!(call("POST", "/_tj/input", Some(&mnt2), Some(json!({ "type": "tick" }))).0, 409);
        assert_eq!(call("POST", "/_tj/privacy", Some(&mnt2), Some(json!({ "action": "erase", "subject": "key:ana" }))).0, 409);
        assert_eq!(call("POST", "/_tj/input", Some(&mnt2), Some(json!({ "type": "inspect" }))).0, 200);
        assert_eq!(call("POST", "/_tj/privacy", Some(&mnt2), Some(json!({ "action": "export", "subject": "key:ana" }))).0, 200);
        let (st, exp) = call("POST", "/_tj/privacy", Some(&mnt), Some(json!({ "action": "export", "subject": "key:ana" })));
        assert_eq!(st, 200);
        assert_eq!(exp["ok"]["Account/key:ana"]["email"], json!("ana@example.com"));
        assert_eq!(call("POST", "/_tj/input", Some(&mnt), Some(json!({ "type": "tick" }))).0, 200);
        // Once the lease is released, `ops:maint2` can proceed.
        assert_eq!(call("DELETE", &format!("/_tj/leases/{mnt_id}"), Some(&mnt), None).0, 200);
        assert_eq!(call("POST", "/_tj/input", Some(&mnt2), Some(json!({ "type": "tick" }))).0, 200);

        // Unfiltered `/api/events` accepts `ops:maintain` and writes an admin stream audit entry (L1-2).
        let sse_resp = agent
            .get(&format!("{base}/api/events"))
            .set("Authorization", &format!("Bearer {mnt}"))
            .call()
            .expect("ops:maintain opens unfiltered stream");
        assert_eq!(sse_resp.status(), 200);
        drop(sse_resp);

        // Audit log records scoped operational actors by name (`ops:critic`, `ops:tester`, `ops:maint`).
        let entries: String = std::fs::read_dir(data.join("audit"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl") && !e.path().ends_with("checkpoints.jsonl"))
            .map(|e| std::fs::read_to_string(e.path()).unwrap())
            .collect();
        assert!(entries.contains("\"who\":\"ops:critic\""), "{entries}");
        assert!(entries.contains("\"who\":\"ops:tester\""), "{entries}");
        assert!(entries.contains("\"who\":\"ops:maint\""), "{entries}");
        assert!(entries.contains("\"kind\":\"lease\""), "{entries}");
        assert!(entries.contains("\"kind\":\"admin\"") && entries.contains("\"request\":\"stream\""), "{entries}");
        assert!(entries.contains("\"kind\":\"refused\"") && entries.contains("\"who\":\"ops:critic\"") && entries.contains("\"request\":\"stream\""), "{entries}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shadow_boot_reload_rollback_and_auto_rollback_work_end_to_end() {
        let dir = std::env::temp_dir().join(format!("tj-http-reload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data = dir.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let keys_file = dir.join("keys.json");
        let obs = crate::identity::new_key("critic", &[], Some(OpsScope::Observe), &keys_file).unwrap();
        let mnt = crate::identity::new_key("maint", &[], Some(OpsScope::Maintain), &keys_file).unwrap();
        let mnt2 = crate::identity::new_key("maint2", &[], Some(OpsScope::Maintain), &keys_file).unwrap();

        let v1 = r#"
@server @read("anyone") object Counter {
  n: number = 0
  @allow("anyone") set(v: number): number { this.n = v; return this.n }
  @allow("anyone") version(): string { return "v1" }
}
"#
        .to_string();

        let (audit, _) = Audit::start(&data).unwrap();
        let engine = Engine::start(v1.clone(), data.clone(), false, false, Arc::default(), audit.clone()).unwrap();
        let identity = Identity::from_key_file(&keys_file).unwrap();
        let front = Arc::new(Front::new(engine, None, None, Automation::Off, identity, audit, Leases::open(&data), false));

        let server = Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let srv_front = front.clone();
        thread::spawn(move || {
            for req in server.incoming_requests() {
                handle(req, &srv_front);
            }
        });

        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(5)).build();
        let call = |method: &str, path: &str, token: Option<&str>, body: Option<Json>| -> (u16, Json) {
            let mut req = agent.request(method, &format!("{base}{path}"));
            if let Some(t) = token {
                req = req.set("Authorization", &format!("Bearer {t}"));
            }
            let resp = match body {
                Some(j) => req.set("Content-Type", "application/json").send_string(&j.to_string()),
                None => req.call(),
            };
            let (status, r) = match resp {
                Ok(r) => (r.status(), r),
                Err(ureq::Error::Status(s, r)) => (s, r),
                Err(e) => panic!("request {method} {path} failed: {e}"),
            };
            (status, serde_json::from_str(&r.into_string().unwrap()).unwrap())
        };

        // Store negative value in v1 so we can test shadow-boot @invariant rejection.
        assert_eq!(call("POST", "/api/Counter/c1/set", None, Some(json!([-5]))).0, 200);
        assert_eq!(call("POST", "/api/Counter/c1/version", None, Some(json!([]))), (200, json!({ "ok": "v1" })));

        // `ops:observe` is refused on reload/rollback; rollback before any reload returns 409.
        assert_eq!(call("POST", "/_tj/reload", Some(&obs), Some(json!({ "source": v1 }))).0, 403);
        assert_eq!(call("POST", "/_tj/rollback", Some(&obs), Some(json!({}))).0, 403);
        assert_eq!(call("POST", "/_tj/rollback", Some(&mnt), Some(json!({}))).0, 409);

        // Competing exclusive lease blocks reload (409).
        let (st, lease) = call("POST", "/_tj/leases", Some(&mnt2), Some(json!({ "role": "maintain", "scope": "app" })));
        assert_eq!(st, 200);
        assert_eq!(call("POST", "/_tj/reload", Some(&mnt), Some(json!({ "source": v1 }))).0, 409);
        assert_eq!(call("DELETE", &format!("/_tj/leases/{}", lease["ok"]["id"].as_str().unwrap()), Some(&mnt2), None).0, 200);

        // 1. Breaking upgrade (removing `n`) is refused with 409.
        let breaking = r#"
@server @read("anyone") object Counter {
  count: number = 0
  @allow("anyone") set(v: number): number { this.count = v; return this.count }
  @allow("anyone") version(): string { return "bad" }
}
"#;
        assert_eq!(call("POST", "/_tj/reload", Some(&mnt), Some(json!({ "source": breaking }))).0, 409);

        // 2. Shadow boot rejects an upgrade whose new `@invariant` is violated by stored `Counter/c1` (n = -5).
        let bad_inv = r#"
@server @read("anyone") object Counter {
  n: number = 0
  @invariant nonNeg(): boolean { return this.n >= 0 }
  @allow("anyone") set(v: number): number { this.n = v; return this.n }
  @allow("anyone") version(): string { return "inv" }
}
"#;
        let (st, body) = call("POST", "/_tj/reload", Some(&mnt), Some(json!({ "source": bad_inv })));
        assert_eq!(st, 422, "{body}");
        assert!(body["error"].as_str().unwrap().contains("Counter/c1"), "{body}");
        // Still running v1!
        assert_eq!(call("POST", "/api/Counter/c1/version", None, Some(json!([]))), (200, json!({ "ok": "v1" })));

        // Fix Counter/c1 to 10 so invariant passes.
        assert_eq!(call("POST", "/api/Counter/c1/set", None, Some(json!([10]))).0, 200);

        // 3. Failing smoke trace rejects reload with 422 and NEVER mutates disk;
        //    smoke traces cannot read server files via `"plugin"` `"file"` (C-037.1);
        //    wrong-typed reload fields return 400 (C-037.10); rollback requires JSON CSRF barrier (C-037.8).
        assert_eq!(call("POST", "/_tj/reload", Some(&mnt), Some(json!({ "source": 123 }))).0, 400);
        assert_eq!(call("POST", "/_tj/reload", Some(&mnt), Some(json!({ "source": v1, "watchSecs": -1 }))).0, 400);
        let file_read_smoke = "{\"do\": \"plugin\", \"name\": \"ai\", \"file\": \"keys.json\"}";
        let (st, file_err) = call("POST", "/_tj/reload", Some(&mnt), Some(json!({ "source": v1, "smokeTraces": [file_read_smoke] })));
        assert_eq!(st, 422, "{file_err}");
        assert!(file_err["error"].as_str().unwrap().contains("cannot read server files"), "{file_err}");
        let bad_ct = agent.post(&format!("{base}/_tj/rollback")).set("Authorization", &format!("Bearer {mnt}")).set("Content-Type", "application/x-www-form-urlencoded").send_string("x=1");
        assert!(matches!(bad_ct, Err(ureq::Error::Status(415, _))), "non-JSON rollback must be refused");

        let v2 = r#"
@server @read("anyone") object Counter {
  n: number = 0
  @invariant nonNeg(): boolean { return this.n >= 0 }
  @allow("anyone") set(v: number): number { this.n = v; return this.n }
  @allow("anyone") version(): string { return "v2" }
}
"#;
        let bad_smoke = "{\"do\": \"call\", \"object\": \"Counter/c1\", \"method\": \"set\", \"args\": [999], \"as\": \"anon\"}\n{\"do\": \"call\", \"object\": \"Counter/c1\", \"method\": \"version\", \"as\": \"anon\", \"expect\": \"wrong\"}";
        assert_eq!(call("POST", "/_tj/reload", Some(&mnt), Some(json!({ "source": v2, "smokeTraces": [bad_smoke] }))).0, 422);
        // Disk still holds n = 10, not 999!
        assert_eq!(call("GET", "/api/Counter/c1", None, None), (200, json!({ "ok": { "n": 10 } })));

        // C-037.4: tightening `@read("anyone")` -> `@read("staff")` closes existing anonymous SSE subscribers!
        let (anon_sub, _) = front.engine.subscribe(vec!["Counter/c1".into()], 0, Some(Caller::Remote { id: String::new(), roles: Vec::new() })).unwrap();
        assert!(!anon_sub.is_closed());
        let tightened = r#"
@server @read("staff") object Counter {
  n: number = 0
  @allow("staff") set(v: number): number { this.n = v; return this.n }
  @allow("staff") version(): string { return "tight" }
}
"#;
        assert_eq!(call("POST", "/_tj/reload", Some(&mnt), Some(json!({ "source": tightened, "watchSecs": 0 }))).0, 200);
        assert!(anon_sub.is_closed(), "anonymous SSE subscriber must be closed when @read tightens");
        // Roll back to v1 before testing v2 + auto-rollback.
        assert_eq!(call("POST", "/_tj/rollback", Some(&mnt), Some(json!({}))).0, 200);

        // 4. Valid upgrade + passing smoke trace (mutating overlay to 777) swaps cleanly without touching disk!
        let good_smoke = "{\"do\": \"get\", \"object\": \"Counter/c1\", \"as\": \"anon\", \"expect\": {\"n\": 10}}\n{\"do\": \"call\", \"object\": \"Counter/c1\", \"method\": \"set\", \"args\": [777], \"as\": \"anon\", \"expect\": 777}\n{\"do\": \"call\", \"object\": \"Counter/c1\", \"method\": \"version\", \"as\": \"anon\", \"expect\": \"v2\"}";
        let (st, ok) = call("POST", "/_tj/reload", Some(&mnt), Some(json!({ "source": v2, "smokeTraces": [good_smoke], "watchSecs": 30, "errorThreshold": 2 })));
        assert_eq!(st, 200, "{ok}");
        assert_eq!(ok["ok"]["smokeTracesPassed"], json!(1));
        assert_eq!(call("POST", "/api/Counter/c1/version", None, Some(json!([]))), (200, json!({ "ok": "v2" })));
        assert_eq!(call("GET", "/api/Counter/c1", None, None), (200, json!({ "ok": { "n": 10 } })), "smoke trace overlay writes must never reach live disk");

        // Live manifest reflects reload count & active watch window.
        let (st, m) = call("GET", "/_tj/manifest", Some(&obs), None);
        assert_eq!(st, 200);
        assert_eq!(m["ok"]["health"]["reloads"], json!(2));
        assert_eq!(m["ok"]["health"]["watching"], json!(true));

        // 5. Manual rollback restores v1 cleanly.
        let (st, rb) = call("POST", "/_tj/rollback", Some(&mnt), Some(json!({})));
        assert_eq!(st, 200, "{rb}");
        assert_eq!(call("POST", "/api/Counter/c1/version", None, Some(json!([]))), (200, json!({ "ok": "v1" })));

        // 6. Health-gated auto-rollback: deploy v3 with a buggy alarm; trigger 2 Output::Errors -> auto-rolls back to v1!
        let v3_buggy = r#"
@server @read("anyone") object Counter {
  n: number = 0
  @allow("anyone") set(v: number): number { this.n = v; return this.n }
  @allow("anyone") version(): string { return "v3-buggy" }
  @allow("anyone") armBug() { this.schedule(0, "crash") }
  private crash() { throw "boom after reload" }
}
"#;
        let (st, _) = call("POST", "/_tj/reload", Some(&mnt), Some(json!({ "source": v3_buggy, "watchSecs": 30, "errorThreshold": 2 })));
        assert_eq!(st, 200);
        assert_eq!(call("POST", "/api/Counter/c1/version", None, Some(json!([]))), (200, json!({ "ok": "v3-buggy" })));
        // Fire 2 crashing alarms via /api/Counter/c1/armBug + /_tj/input tick.
        assert_eq!(call("POST", "/api/Counter/c1/armBug", None, Some(json!([]))).0, 200);
        assert_eq!(call("POST", "/_tj/input", Some(&mnt), Some(json!({ "type": "tick" }))).0, 200);
        assert_eq!(call("POST", "/api/Counter/c1/armBug", None, Some(json!([]))).0, 200);
        assert_eq!(call("POST", "/_tj/input", Some(&mnt), Some(json!({ "type": "tick" }))).0, 200);
        // Auto-rollback restored v1, AND a subsequent manual `POST /_tj/rollback` returns 409
        // rather than re-promoting the broken `v3-buggy` bundle (P3B-2)!
        assert_eq!(call("POST", "/api/Counter/c1/version", None, Some(json!([]))), (200, json!({ "ok": "v1" })));
        assert_eq!(call("POST", "/_tj/rollback", Some(&mnt), Some(json!({}))).0, 409);
        let (st, m_after) = call("GET", "/_tj/manifest", Some(&obs), None);
        assert_eq!(st, 200);
        assert_eq!(m_after["ok"]["health"]["autoRollbacks"], json!(1));
        assert_eq!(m_after["ok"]["health"]["watching"], json!(false));

        // Audit log recorded reload, rollback, and auto-rollback.
        let entries: String = std::fs::read_dir(data.join("audit"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl") && !e.path().ends_with("checkpoints.jsonl"))
            .map(|e| std::fs::read_to_string(e.path()).unwrap())
            .collect();
        assert!(entries.contains("\"request\":\"reload\""), "{entries}");
        assert!(entries.contains("\"request\":\"rollback\""), "{entries}");
        assert!(entries.contains("\"request\":\"auto-rollback\""), "{entries}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rate_limit_caps_api_calls_returns_retry_after_and_audits_429() {
        let dir = std::env::temp_dir().join(format!("tj-http-rate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data = dir.join("data");
        let src = r#"
@server @read("anyone") object Ping {
  n: number = 0
  @allow("anyone") hit(): number { this.n += 1; return this.n }
}
"#;
        let (audit, _) = Audit::start(&data).unwrap();
        let engine = Engine::start(src.into(), data.clone(), false, false, Arc::default(), audit.clone()).unwrap();
        let leases = Leases::open(&data);
        let mut front = Front::new(engine, None, None, Automation::Open, Identity::anonymous(), audit, leases, false).with_rate_limit(2);
        front.trust_proxy = true;
        let front = Arc::new(front);
        let server = Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let f = front.clone();
        thread::spawn(move || {
            for req in server.incoming_requests() {
                handle(req, &f);
            }
        });

        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(5)).build();
        let url = format!("http://{addr}/api/Ping/p1/hit");
        // C-039.2: Spoofing different leftmost `X-Forwarded-For` entries cannot bypass the limit
        // because `client_ip` reads the proxy-appended rightmost valid IP (`203.0.113.9`).
        assert_eq!(agent.post(&url).set("Content-Type", "application/json").set("X-Forwarded-For", "1.1.1.1, 203.0.113.9").send_string("[]").unwrap().status(), 200);
        assert_eq!(agent.post(&url).set("Content-Type", "application/json").set("X-Forwarded-For", "2.2.2.2, 203.0.113.9").send_string("[]").unwrap().status(), 200);
        // 3rd and 4th requests within the window return 429 with `Retry-After`.
        for fake in ["3.3.3.3, 203.0.113.9", "4.4.4.4, 203.0.113.9"] {
            match agent.post(&url).set("Content-Type", "application/json").set("X-Forwarded-For", fake).send_string("[]") {
                Err(ureq::Error::Status(429, resp)) => {
                    assert!(resp.header("Retry-After").and_then(|s| s.parse::<u64>().ok()).is_some_and(|n| n >= 1));
                }
                other => panic!("expected 429, got {other:?}"),
            }
        }
        // `/_tj/*` is exempt from the `/api/*` caller rate limit so operators can still inspect!
        assert_eq!(agent.get(&format!("http://{addr}/_tj/manifest")).call().unwrap().status(), 200);

        // Wait briefly for the non-blocking audit writer to flush; verify at most ONE 429 entry
        // was recorded in the window (C-039.3: no audit-fsync flood).
        let read_audit = || -> String {
            std::fs::read_dir(data.join("audit"))
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl") && !e.path().ends_with("checkpoints.jsonl"))
                .map(|e| std::fs::read_to_string(e.path()).unwrap())
                .collect()
        };
        let mut entries = String::new();
        for _ in 0..30 {
            entries = read_audit();
            if entries.contains("\"request\":\"rate limit\"") {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(entries.matches("\"request\":\"rate limit\"").count(), 1, "expected exactly 1 deduplicated 429 audit entry: {entries}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn track1_holistic_security_privacy_and_autorollback_regressions() {
        let dir = std::env::temp_dir().join(format!("tj-http-t1-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let data = dir.join("data");
        let keys_file = dir.join("keys.json");
        let obs = crate::identity::new_key("obs", &[], Some(OpsScope::Observe), &keys_file).unwrap();
        let tst = crate::identity::new_key("tst", &[], Some(OpsScope::Test), &keys_file).unwrap();
        let mnt = crate::identity::new_key("mnt", &[], Some(OpsScope::Maintain), &keys_file).unwrap();
        let ana_key = crate::identity::new_key("ana", &[], None, &keys_file).unwrap();

        let v1 = r#"
@server @read("owner", "staff") object Account {
  @pii("contact", "account") email: string = ""
  @allow("owner") setEmail(e: string): string { this.email = e; return "saved" }
  @allow("anyone") ping(): string { return "v1" }
}
"#;
        let (audit, _) = Audit::start(&data).unwrap();
        let engine = Engine::start(v1.into(), data.clone(), false, false, Arc::default(), audit.clone()).unwrap();
        let leases = Leases::open(&data);
        let identity = Identity::from_key_file(&keys_file).unwrap();
        let mut front = Front::new(engine, None, None, Automation::Off, identity, audit, leases, false).with_rate_limit(2);
        front.trust_proxy = true;
        let front = Arc::new(front);
        let server = Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let f = front.clone();
        thread::spawn(move || {
            for req in server.incoming_requests() {
                handle(req, &f);
            }
        });

        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(5)).build();
        let base = format!("http://{addr}");
        let call = |method: &str, path: &str, bearer: Option<&str>, body: Option<Json>| -> (u16, Json) {
            let mut r = agent.request(method, &format!("{base}{path}"));
            if let Some(k) = bearer {
                r = r.set("Authorization", &format!("Bearer {k}"));
            }
            let resp = match body {
                Some(b) => r.set("Content-Type", "application/json").send_string(&b.to_string()),
                None => r.call(),
            };
            let (st, resp) = match resp {
                Ok(r) => (r.status(), r),
                Err(ureq::Error::Status(s, r)) => (s, r),
                Err(e) => panic!("transport error: {e}"),
            };
            let text = resp.into_string().unwrap();
            (st, serde_json::from_str(&text).unwrap_or(Json::String(text)))
        };

        // Seed sensitive PII for `key:ana`.
        assert_eq!(call("POST", "/api/Account/key:ana/setEmail", Some(&ana_key), Some(json!(["secret@example.com"]))).0, 200);

        // T1-1: `ops:observe` and `ops:test` MUST be refused (403) when asking for `exclusive: true`.
        assert_eq!(call("POST", "/_tj/leases", Some(&obs), Some(json!({ "role": "observe", "scope": "app", "exclusive": true }))).0, 403);
        assert_eq!(call("POST", "/_tj/leases", Some(&tst), Some(json!({ "role": "test", "scope": "app", "exclusive": true }))).0, 403);

        // T1-2(A): `POST /_tj/input` ignores body-supplied `"caller"` so `@pii` stays masked `"•••"`.
        let (st, got) = call("POST", "/_tj/input", Some(&mnt), Some(json!({
            "type": "get",
            "object": "Account/key:ana",
            "caller": { "id": "key:ana", "roles": [] }
        })));
        assert_eq!(st, 200, "{got}");
        assert_eq!(got["ok"]["email"], json!("•••"), "body-supplied caller must never unmask @pii: {got}");

        // T1-2(B): `POST /_tj/reload` smoke traces block `"privacy"` steps, block person-id `"get"`,
        // and redact mismatch payloads so raw `@pii` cannot leak in `422`.
        let priv_smoke = "{\"do\": \"privacy\", \"action\": \"export\", \"subject\": \"key:ana\", \"expect\": \"\"}";
        let (st, err) = call("POST", "/_tj/reload", Some(&mnt), Some(json!({ "source": v1, "smokeTraces": [priv_smoke] })));
        assert_eq!(st, 422);
        assert!(!err.to_string().contains("secret@example.com"), "PII leaked in privacy smoke error: {err}");

        let get_smoke = "{\"do\": \"get\", \"object\": \"Account/key:ana\", \"as\": \"key:ana\", \"expect\": {}}";
        let (st, err) = call("POST", "/_tj/reload", Some(&mnt), Some(json!({ "source": v1, "smokeTraces": [get_smoke] })));
        assert_eq!(st, 422);
        assert!(!err.to_string().contains("secret@example.com"), "PII leaked in get smoke error: {err}");

        // T1-4: exhausting `bad:{ip}` with forged JWTs from an IP does NOT lock out a valid `tjk_` API key.
        for _ in 0..3 {
            let _ = call("POST", "/api/Account/key:ana/ping", Some("forged.jwt.token"), Some(json!([])));
        }
        assert_eq!(call("POST", "/api/Account/key:ana/ping", Some(&ana_key), Some(json!([]))), (200, json!({ "ok": "v1" })));

        // T1-R1: unauthenticated `GET /api/events` and percent-encoded `?%6fbject=...` MUST still
        // hit `RateLimiter` (429) rather than bypassing it into fsynced `audit.security` refusals.
        let mut hit_429_events = false;
        for _ in 0..4 {
            let r = agent
                .get(&format!("{base}/api/events"))
                .set("X-Forwarded-For", "198.51.100.55")
                .call();
            if matches!(r, Err(ureq::Error::Status(429, _))) {
                hit_429_events = true;
                break;
            }
        }
        assert!(hit_429_events, "unauthenticated GET /api/events must be rate-limited");
        let mut hit_429_pct = false;
        for _ in 0..4 {
            let r = agent
                .get(&format!("{base}/api/events?%6fbject=Account/key:ana"))
                .set("X-Forwarded-For", "198.51.100.56")
                .call();
            if matches!(r, Err(ureq::Error::Status(429, _))) {
                hit_429_pct = true;
                break;
            }
        }
        assert!(hit_429_pct, "percent-encoded ?%6fbject= must be rate-limited");
        // Forged JWT on `GET /api/events` also hits `RateLimiter` (429).
        let mut hit_429_jwt = false;
        for _ in 0..4 {
            let r = agent
                .get(&format!("{base}/api/events"))
                .set("Authorization", "Bearer forged.jwt.events")
                .set("X-Forwarded-For", "198.51.100.57")
                .call();
            if matches!(r, Err(ureq::Error::Status(429, _))) {
                hit_429_jwt = true;
                break;
            }
        }
        assert!(hit_429_jwt, "forged JWT on GET /api/events must be rate-limited");

        // T1-R2: caller-input errors (`Account.get("a/b")`, `JSON.parse("{")`) return 400 (NOT 500)
        // and MUST NOT trigger auto-rollback during the watch window!
        let v2_input_err = r#"
@server @read("owner", "staff") object Account {
  @pii("contact", "account") email: string = ""
  @allow("owner") setEmail(e: string): string { this.email = e; return "saved" }
  @allow("anyone") ping(): string { return "v2-ok" }
  @allow("anyone") badLookup(id: string): string { return Account.get(id).id }
  @allow("anyone") badJson(s: string): any { return JSON.parse(s) }
}
"#;
        assert_eq!(call("POST", "/_tj/reload", Some(&mnt), Some(json!({ "source": v2_input_err, "watchSecs": 30, "errorThreshold": 2 }))).0, 200);
        for (ip, path, arg) in [
            ("198.51.100.60", "/api/Account/a/badLookup", "a/b"),
            ("198.51.100.61", "/api/Account/a/badJson", "{"),
            ("198.51.100.62", "/api/Account/a/badLookup", ""),
        ] {
            let resp = agent
                .post(&format!("{base}{path}"))
                .set("Content-Type", "application/json")
                .set("X-Forwarded-For", ip)
                .send_string(&json!([arg]).to_string());
            assert!(matches!(resp, Err(ureq::Error::Status(400, _))), "caller-input error on {path} must return 400: {resp:?}");
        }
        // Still running `v2-ok` — bad caller inputs did NOT force an auto-rollback!
        let still_v2 = agent
            .post(&format!("{base}/api/Account/a/ping"))
            .set("Content-Type", "application/json")
            .set("X-Forwarded-For", "198.51.100.63")
            .send_string("[]")
            .unwrap();
        assert_eq!(still_v2.into_string().unwrap(), "{\"ok\":\"v2-ok\"}");

        // T1-5: uncaught HTTP VM fault (`cannot read 'x' of null` -> 500) triggers auto-rollback during watch window!
        let v2_crash = r#"
@server @read("owner", "staff") object Account {
  @pii("contact", "account") email: string = ""
  @allow("owner") setEmail(e: string): string { this.email = e; return "saved" }
  @allow("anyone") ping(): string {
    const x: any = null
    return x.boom
  }
}
"#;
        assert_eq!(call("POST", "/_tj/reload", Some(&mnt), Some(json!({ "source": v2_crash, "allowBreaking": true, "watchSecs": 30, "errorThreshold": 2 }))).0, 200);
        // Trigger 2 HTTP 500 crashes from distinct anonymous XFF IPs (to stay under rate limit = 2).
        for ip in ["198.51.100.1", "198.51.100.2"] {
            let resp = agent
                .post(&format!("{base}/api/Account/a/ping"))
                .set("Content-Type", "application/json")
                .set("X-Forwarded-For", ip)
                .send_string("[]");
            assert!(matches!(resp, Err(ureq::Error::Status(500, _))), "uncaught VM fault must return 500: {resp:?}");
        }
        // Auto-rollback restored v2-ok AND cleared stale method stats (T1-8)!
        let resp = agent
            .post(&format!("{base}/api/Account/a/ping"))
            .set("Content-Type", "application/json")
            .set("X-Forwarded-For", "198.51.100.3")
            .send_string("[]")
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.into_string().unwrap(), "{\"ok\":\"v2-ok\"}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn server_to_server_replicas_and_webhooks_work_end_to_end() {
        let dir = std::env::temp_dir().join(format!("tj-s2s-webhooks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let dir_a = dir.join("server-a");
        let dir_b = dir.join("server-b");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();

        // 1. Server A (Inventory): owns Stock, protected by a real API key with role "service".
        let keys_a = dir_a.join("keys.json");
        let svc_key = crate::identity::new_key("shop-svc", &["service".into()], None, &keys_a).unwrap();
        let src_a = r#"
@server @read("service") object Stock {
  count: number = 0
  @allow("service") seed(n: number): number { this.count = n; return this.count }
  @allow("service") reserve(n: number): number {
    check(this.count >= n, "count", "out of stock")
    this.count -= n
    return this.count
  }
}
"#
        .to_string();
        let audit_a = Audit::start(&dir_a).unwrap().0;
        let eng_a = Engine::start(src_a, dir_a.clone(), false, true, Arc::default(), audit_a.clone()).unwrap();
        let id_a = Identity::from_key_file(&keys_a).unwrap();
        let srv_a = Server::http("127.0.0.1:0").unwrap();
        let port_a = srv_a.server_addr().to_ip().unwrap().port();
        let front_a = Arc::new(Front::new(eng_a, None, None, Automation::Open, id_a, audit_a, Leases::open(&dir_a), false));
        thread::spawn(move || {
            for req in srv_a.incoming_requests() {
                let f = front_a.clone();
                thread::spawn(move || handle(req, &f));
            }
        });
        let base_a = format!("http://127.0.0.1:{port_a}");

        // 2. Outgoing webhook mock receiver: rejects FIRST delivery with 503 (testing retries!),
        // accepts subsequent deliveries with 200 and records headers + body!
        let wh_secret = b"webhook-e2e-shared-secret-32-bytes";
        let wh_srv = Server::http("127.0.0.1:0").unwrap();
        let wh_port = wh_srv.server_addr().to_ip().unwrap().port();
        let received = Arc::new(Mutex::new(Vec::new()));
        let recv_ref = received.clone();
        thread::spawn(move || {
            let mut first = true;
            for mut req in wh_srv.incoming_requests() {
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                let delivery = header_value(&req, "X-TinyJump-Delivery").unwrap_or_default();
                let ts = header_value(&req, "X-TinyJump-Timestamp").unwrap_or_default();
                let sig = header_value(&req, "X-TinyJump-Signature").unwrap_or_default();
                recv_ref.lock().unwrap().push((delivery, ts, sig, body));
                if first {
                    first = false;
                    let _ = req.respond(Response::from_string("try again").with_status_code(503));
                } else {
                    let _ = req.respond(Response::from_string("ok").with_status_code(200));
                }
            }
        });

        // 3. Server B (Shop): treats `Stock` as a remote replica on Server A, has incoming
        // HMAC-signed webhook `stripe` and outgoing retried webhook `fulfillment`.
        let src_b = r#"
@server @read("service") object Stock {
  count: number = 0
  @allow("service") reserve(n: number): number { return 0 }
}
@server @read("anyone") object Order {
  @pii("contact", "orders") customerEmail: string = ""
  bought: number = 0
  webhooks: number = 0
  lastEvent: string = ""
  @allow("anyone") async buy(sku: string, n: number, email: string): number {
    this.customerEmail = email
    const left = await Stock.get(sku).reserve(n)
    this.bought += n
    this.emit("placed", { sku, n })
    return left
  }
  @allow("anyone") stockLeft(sku: string): number {
    return Stock.get(sku).count
  }
  @allow("webhook") onStripe(evt: any): number {
    this.webhooks += 1
    this.lastEvent = evt.id
    return this.webhooks
  }
}
"#
        .to_string();
        let remotes_b = Arc::new(crate::remotes::Remotes::from_targets(vec![(
            "Stock".into(),
            crate::remotes::RemoteTarget {
                url: base_a.clone(),
                key_env: Some("TEST_STOCK_KEY".into()),
                api_key: Some(svc_key.clone()),
            },
        )]));
        let outgoing_b = vec![crate::webhooks::OutgoingWebhook {
            name: "fulfillment".into(),
            url: format!("http://127.0.0.1:{wh_port}/hook"),
            secret: wh_secret.to_vec(),
            objects: vec!["Order/*".into()],
            on_changed: true,
            on_emit: false,
            caller: "webhook:fulfillment".into(),
            roles: vec!["webhook".into()],
            max_retries: 3,
        }];
        let mut wh_cfg = crate::webhooks::Webhooks { incoming: HashMap::new(), outgoing: outgoing_b.clone() };
        wh_cfg.incoming.insert(
            "stripe".into(),
            crate::webhooks::IncomingWebhook {
                secret: wh_secret.to_vec(),
                object: "Order/o1".into(),
                method: "onStripe".into(),
                caller: "webhook:stripe".into(),
                roles: vec!["webhook".into()],
                max_age_secs: 300,
            },
        );
        let wh_arc = Arc::new(wh_cfg);
        let audit_b = Audit::start(&dir_b).unwrap().0;
        let eng_b = Engine::start_with_integrations(
            src_b,
            dir_b.clone(),
            false,
            true,
            Arc::default(),
            audit_b.clone(),
            None,
            remotes_b,
            wh_arc.clone(),
            std::collections::BTreeMap::new(),
        )
        .unwrap();
        let srv_b = Server::http("127.0.0.1:0").unwrap();
        let port_b = srv_b.server_addr().to_ip().unwrap().port();
        let front_b = Arc::new(
            Front::new(eng_b.clone(), None, None, Automation::Open, Identity::anonymous(), audit_b.clone(), Leases::open(&dir_b), false)
                .with_webhooks(wh_arc.clone()),
        );
        let srv_rl = Server::http("127.0.0.1:0").unwrap();
        let port_rl = srv_rl.server_addr().to_ip().unwrap().port();
        let front_rl = Arc::new(
            Front::new(eng_b, None, None, Automation::Open, Identity::anonymous(), audit_b, Leases::open(&dir_b), false)
                .with_webhooks(wh_arc)
                .with_rate_limit(2),
        );
        thread::spawn(move || {
            for req in srv_rl.incoming_requests() {
                let f = front_rl.clone();
                thread::spawn(move || handle(req, &f));
            }
        });
        let base_rl = format!("http://127.0.0.1:{port_rl}");
        thread::spawn(move || {
            for req in srv_b.incoming_requests() {
                let f = front_b.clone();
                thread::spawn(move || handle(req, &f));
            }
        });
        let base_b = format!("http://127.0.0.1:{port_b}");
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(5)).build();

        // Seed Stock/sku-1 = 5 on Server A using the real service API key.
        let seed_resp = agent
            .post(&format!("{base_a}/api/Stock/sku-1/seed"))
            .set("Content-Type", "application/json")
            .set("Authorization", &format!("Bearer {svc_key}"))
            .send_string("[5]")
            .unwrap();
        assert_eq!(seed_resp.status(), 200);

        // Call Order/o1.buy("sku-1", 2, "ana@example.com") on Server B -> calls Server A with
        // Bearer key + Idempotency-Key -> returns 3 remaining!
        let buy_resp = agent
            .post(&format!("{base_b}/api/Order/o1/buy"))
            .set("Content-Type", "application/json")
            .send_string(r#"["sku-1", 2, "ana@example.com"]"#)
            .unwrap();
        assert_eq!(buy_resp.status(), 200);
        assert_eq!(buy_resp.into_string().unwrap(), r#"{"ok":3}"#);

        // Wait for Server B's RemoteHub SSE stream to sync Stock/sku-1.count == 3!
        let mut synced = false;
        for _ in 0..40 {
            let r = agent
                .post(&format!("{base_b}/api/Order/o1/stockLeft"))
                .set("Content-Type", "application/json")
                .send_string(r#"["sku-1"]"#)
                .unwrap()
                .into_string()
                .unwrap();
            if r == r#"{"ok":3}"# {
                synced = true;
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(synced, "Server B replica must sync Stock/sku-1 = 3 via SSE from Server A");

        // Over-reserving preserves `field: "count"` refusal from Server A!
        let over = agent
            .post(&format!("{base_b}/api/Order/o1/buy"))
            .set("Content-Type", "application/json")
            .send_string(r#"["sku-1", 99, "ana@example.com"]"#);
        let Err(ureq::Error::Status(400, resp)) = over else { panic!("expected 400 out of stock, got {over:?}") };
        let err_json: Json = serde_json::from_str(&resp.into_string().unwrap()).unwrap();
        assert_eq!(err_json["field"], json!("count"), "{err_json}");

        // Outgoing webhook: wait for retry (attempt 1 -> 503, attempt 2 -> 200) and final bought=2
        // delivery; verify per-target FIFO order, HMAC signature, same X-TinyJump-Delivery id
        // across retries, AND @pii customerEmail masked!
        let mut delivered = Vec::new();
        for _ in 0..40 {
            let snap = received.lock().unwrap().clone();
            if snap.len() >= 3 {
                delivered = snap;
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(delivered.len() >= 3, "expected 503 retry + 200 delivery + bought=2 change, got {delivered:?}");
        assert_eq!(delivered[0].0, delivered[1].0, "retried webhook must keep same X-TinyJump-Delivery in FIFO order");
        let (del_id, ts_str, sig, body_str) = delivered.last().unwrap();
        assert!(del_id.starts_with("whd_"), "{del_id}");
        let ts_val: i64 = ts_str.parse().unwrap();
        assert_eq!(*sig, crate::webhooks::sign_payload(wh_secret, ts_val, body_str.as_bytes()));
        let payload: Json = serde_json::from_str(body_str).unwrap();
        assert_eq!(payload["event"], json!("changed"));
        assert_eq!(payload["object"], json!("Order/o1"));
        assert_eq!(payload["state"]["bought"], json!(2));
        assert_eq!(payload["state"]["customerEmail"], json!("•••"), "@pii must be masked in outgoing webhook: {payload}");

        // Incoming webhook: bad signature -> 401; stale timestamp -> 401; exhausting `bad:{ip}`
        // returns 429 to further invalid webhooks while a valid HMAC-signed webhook from the SAME
        // IP still succeeds with 200 (`RW-2`, shared-proxy/NAT safety)!
        let wh_body = r#"{"id":"evt_paid_1"}"#;
        let now = crate::webhooks::now_secs();
        let bad_sig = agent
            .post(&format!("{base_rl}/webhooks/stripe"))
            .set("Content-Type", "application/json")
            .set("X-TinyJump-Timestamp", &now.to_string())
            .set("X-TinyJump-Signature", "v1=0000000000000000000000000000000000000000000000000000000000000000")
            .send_string(wh_body);
        assert!(matches!(bad_sig, Err(ureq::Error::Status(401, _))), "{bad_sig:?}");

        let stale_ts = now - 600;
        let stale_sig = crate::webhooks::sign_payload(wh_secret, stale_ts, wh_body.as_bytes());
        let stale = agent
            .post(&format!("{base_rl}/webhooks/stripe"))
            .set("Content-Type", "application/json")
            .set("X-TinyJump-Timestamp", &stale_ts.to_string())
            .set("X-TinyJump-Signature", &stale_sig)
            .send_string(wh_body);
        assert!(matches!(stale, Err(ureq::Error::Status(401, _))), "{stale:?}");

        // 3rd invalid request from same IP hits `bad:{ip}` rate limit (429)!
        let bad_3 = agent
            .post(&format!("{base_rl}/webhooks/stripe"))
            .set("Content-Type", "application/json")
            .set("X-TinyJump-Timestamp", &now.to_string())
            .set("X-TinyJump-Signature", "v1=0000000000000000000000000000000000000000000000000000000000000000")
            .send_string(wh_body);
        assert!(matches!(bad_3, Err(ureq::Error::Status(429, _))), "exhausted bad:{{ip}} must return 429: {bad_3:?}");

        // Valid HMAC-signed webhook from the SAME IP STILL succeeds with 200 (never locked out by `bad:{ip}`)!
        let good_sig = crate::webhooks::sign_payload(wh_secret, now, wh_body.as_bytes());
        let ok1 = agent
            .post(&format!("{base_rl}/webhooks/stripe"))
            .set("Content-Type", "application/json")
            .set("X-TinyJump-Timestamp", &now.to_string())
            .set("X-TinyJump-Signature", &good_sig)
            .set("X-TinyJump-Delivery", "del-99")
            .send_string(wh_body)
            .unwrap();
        assert_eq!(ok1.status(), 200);
        assert_eq!(ok1.into_string().unwrap(), r#"{"ok":1}"#);

        // Replay exact same X-TinyJump-Delivery -> idempotent cached result (still 1, NOT 2)!
        let ok2 = agent
            .post(&format!("{base_rl}/webhooks/stripe"))
            .set("Content-Type", "application/json")
            .set("X-TinyJump-Timestamp", &now.to_string())
            .set("X-TinyJump-Signature", &good_sig)
            .set("X-TinyJump-Delivery", "del-99")
            .send_string(wh_body)
            .unwrap();
        assert_eq!(ok2.status(), 200);
        assert_eq!(ok2.into_string().unwrap(), r#"{"ok":1}"#);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn serves_opt_in_device_and_webrtc_plugins_or_stubs() {
        let dir = std::env::temp_dir().join(format!("tj-http-devplugins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let d1 = dir.join("plain");
        let d2 = dir.join("call");
        std::fs::create_dir_all(&d1).unwrap();
        std::fs::create_dir_all(&d2).unwrap();
        let web_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../hosts/web");
        let wasm = dir.join("dummy.wasm");
        std::fs::write(&wasm, b"").unwrap();
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(5)).build();

        // 1. Plain app: /api/whoami reports empty devicePlugins + videoCall=false; stubs served.
        let src_plain = "function App() { return <Column><Text>hi</Text></Column> }\n".to_string();
        let audit1 = Audit::start(&d1).unwrap().0;
        let eng1 = Engine::start(src_plain, d1.clone(), false, true, Arc::default(), audit1.clone()).unwrap();
        let srv1 = Server::http("127.0.0.1:0").unwrap();
        let port1 = srv1.server_addr().to_ip().unwrap().port();
        let web1 = Some(Arc::new(Web { source: d1.join("app.tj"), web_dir: web_dir.clone(), wasm: wasm.clone() }));
        let f1 = Arc::new(Front::new(eng1, web1, None, Automation::Open, Identity::anonymous(), audit1, Leases::open(&d1), false));
        thread::spawn(move || {
            for req in srv1.incoming_requests() {
                handle(req, &f1);
            }
        });
        let who1: Json = serde_json::from_str(&agent.get(&format!("http://127.0.0.1:{port1}/api/whoami")).call().unwrap().into_string().unwrap()).unwrap();
        assert_eq!(who1["ok"]["devicePlugins"], json!([]));
        assert_eq!(who1["ok"]["videoCall"], json!(false));
        assert_eq!(who1["ok"]["whiteboard"], json!(false));
        assert_eq!(who1["ok"]["gfx"], json!(false));
        let dev_stub = agent.get(&format!("http://127.0.0.1:{port1}/device-plugins.js")).call().unwrap().into_string().unwrap();
        assert!(dev_stub.len() < 80 && dev_stub.contains("createDevicePlugins"));
        let rtc_stub = agent.get(&format!("http://127.0.0.1:{port1}/webrtc-plugin.js")).call().unwrap().into_string().unwrap();
        assert!(rtc_stub.len() < 80 && rtc_stub.contains("createVideoCallComponents"));
        let wb_stub = agent.get(&format!("http://127.0.0.1:{port1}/whiteboard-plugin.js")).call().unwrap().into_string().unwrap();
        assert!(wb_stub.len() < 80 && wb_stub.contains("createWhiteboardComponents"));
        let gfx_stub = agent.get(&format!("http://127.0.0.1:{port1}/gfx-plugin.js")).call().unwrap().into_string().unwrap();
        assert!(gfx_stub.len() < 80 && gfx_stub.contains("createGfxPlugin"));

        // 2. VideoCall + Whiteboard + 3D Scene app with camera+microphone permissions: full plugin files served.
        let src_call = "function App() { return <Column><Native name=\"VideoCall\" active={false} /><Native name=\"Whiteboard\" readOnly={false} /><Scene><Mesh geometry=\"box\" /></Scene></Column> }\n".to_string();
        let mut perms = std::collections::BTreeMap::new();
        perms.insert("camera".into(), crate::device_perms::DevicePermission { reason: None, sources: vec!["permissions.json".into()] });
        perms.insert("microphone".into(), crate::device_perms::DevicePermission { reason: None, sources: vec!["permissions.json".into()] });
        let audit2 = Audit::start(&d2).unwrap().0;
        let eng2 = Engine::start_with_integrations(
            src_call,
            d2.clone(),
            false,
            true,
            Arc::default(),
            audit2.clone(),
            None,
            Arc::default(),
            Arc::default(),
            perms,
        )
        .unwrap();
        let srv2 = Server::http("127.0.0.1:0").unwrap();
        let port2 = srv2.server_addr().to_ip().unwrap().port();
        let web2 = Some(Arc::new(Web { source: d2.join("app.tj"), web_dir, wasm }));
        let f2 = Arc::new(Front::new(eng2, web2, None, Automation::Open, Identity::anonymous(), audit2, Leases::open(&d2), false));
        thread::spawn(move || {
            for req in srv2.incoming_requests() {
                handle(req, &f2);
            }
        });
        let who2: Json = serde_json::from_str(&agent.get(&format!("http://127.0.0.1:{port2}/api/whoami")).call().unwrap().into_string().unwrap()).unwrap();
        assert_eq!(who2["ok"]["devicePlugins"], json!(["camera"]));
        assert_eq!(who2["ok"]["videoCall"], json!(true));
        assert_eq!(who2["ok"]["whiteboard"], json!(true));
        assert_eq!(who2["ok"]["gfx"], json!(true));
        let dev_full = agent.get(&format!("http://127.0.0.1:{port2}/device-plugins.js")).call().unwrap().into_string().unwrap();
        assert!(dev_full.len() > 2000 && dev_full.contains("handleCamera"));
        let rtc_full = agent.get(&format!("http://127.0.0.1:{port2}/webrtc-plugin.js")).call().unwrap().into_string().unwrap();
        assert!(rtc_full.len() > 2000 && rtc_full.contains("RTCPeerConnection"));
        let wb_full = agent.get(&format!("http://127.0.0.1:{port2}/whiteboard-plugin.js")).call().unwrap().into_string().unwrap();
        assert!(wb_full.len() > 2000 && wb_full.contains("createWhiteboardComponent"));
        let gfx_full = agent.get(&format!("http://127.0.0.1:{port2}/gfx-plugin.js")).call().unwrap().into_string().unwrap();
        assert!(gfx_full.len() > 2000 && gfx_full.contains("createGfxPlugin"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sse_topic_prefix_streams_matching_objects_only_and_enforces_rules() {
        use std::io::{BufRead, BufReader};

        let dir = std::env::temp_dir().join(format!("tj-http-topic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = r#"
@server @read("anyone") object Note {
  text: string = ""
  @read("staff") internal: string = ""
  @allow("anyone") set(text: string, internal: string) {
    this.text = text
    this.internal = internal
  }
}
@server @read("owner") object Secret {
  @pii("other", "ops") body: string = ""
}
"#.to_string();
        let audit = Audit::start(&dir).unwrap().0;
        let eng = Engine::start(src, dir.clone(), false, true, Arc::default(), audit.clone()).unwrap();
        let srv = Server::http("127.0.0.1:0").unwrap();
        let port = srv.server_addr().to_ip().unwrap().port();
        let front = Arc::new(Front::new(eng, None, None, Automation::Open, Identity::anonymous(), audit, Leases::open(&dir), false));
        thread::spawn(move || {
            for req in srv.incoming_requests() {
                let f = front.clone();
                thread::spawn(move || handle(req, &f));
            }
        });
        let base = format!("http://127.0.0.1:{port}");
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(5)).build();

        // Seed studio:1 and other:1 before subscribing.
        agent.post(&format!("{base}/api/Note/studio:1/set")).set("Content-Type", "application/json").send_string(r#"["s1","sec1"]"#).unwrap();
        agent.post(&format!("{base}/api/Note/other:1/set")).set("Content-Type", "application/json").send_string(r#"["o1","sec2"]"#).unwrap();

        // GET on wildcard returns 400; unauthorized wildcard returns 403.
        let get_wild = agent.get(&format!("{base}/api/Note/studio:*")).call();
        assert!(matches!(get_wild, Err(ureq::Error::Status(400, _))));
        let get_secret = agent.get(&format!("{base}/api/Secret/user:ana*")).call();
        assert!(matches!(get_secret, Err(ureq::Error::Status(403, _))));
        let sse_secret = agent.get(&format!("{base}/api/events?object=Secret/user:ana*")).call().unwrap();
        let mut sreader = BufReader::new(sse_secret.into_reader());
        let mut refused_ev = String::new();
        for _ in 0..2 {
            sreader.read_line(&mut refused_ev).unwrap();
        }
        assert!(refused_ev.contains("event: refused") && refused_ev.contains("Secret/user:ana*"), "{refused_ev}");
        drop(sreader);

        // Subscribe to `Note/studio:*`: receives initial studio:1 (masked) and live studio:2, never other:1/other:2.
        let resp = agent.get(&format!("{base}/api/events?object=Note/studio:*&since=0")).call().unwrap();
        let mut reader = BufReader::new(resp.into_reader());
        let mut lines = Vec::new();
        let mut line = String::new();
        // Read initial catch-up event for Note/studio:1.
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            let trimmed = line.trim().to_string();
            lines.push(trimmed.clone());
            if trimmed.is_empty() && lines.iter().any(|l| l.contains("Note/studio:1")) {
                break;
            }
        }
        // Trigger other:2 and studio:2 while stream is open; wait until studio:2 arrives.
        agent.post(&format!("{base}/api/Note/other:2/set")).set("Content-Type", "application/json").send_string(r#"["o2","sec3"]"#).unwrap();
        agent.post(&format!("{base}/api/Note/studio:2/set")).set("Content-Type", "application/json").send_string(r#"["s2","sec4"]"#).unwrap();
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            let trimmed = line.trim().to_string();
            lines.push(trimmed.clone());
            if trimmed.is_empty() && lines.iter().any(|l| l.contains("Note/studio:2")) {
                break;
            }
        }
        let joined = lines.join("\n");
        assert!(joined.contains("Note/studio:1") && joined.contains("Note/studio:2"), "{joined}");
        assert!(!joined.contains("Note/other:1") && !joined.contains("Note/other:2"), "{joined}");
        assert!(!joined.contains("sec1") && !joined.contains("sec4"), "staff-only fields must stay masked: {joined}");

        drop(reader);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

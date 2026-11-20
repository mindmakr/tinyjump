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
//!   GET  /_tj/inspect                 -> semantic tree + object states
//!   POST /_tj/input                   body: one protocol Input
//!   (`/_tj/*` needs the admin key on `tj serve`; see auth.rs)
//!
//! The audit log (audit.rs) gets every API call, every refusal, and every admin request that
//! can read objects or change them (`inspect`, `input`). Refusals and admin requests wait
//! until they are on disk, and an admin request that cannot be written is refused.
//!
//! In dev mode it also serves the web host, the WASM runtime and the app source,
//! plus `/_tj/reload` (SSE) for live reload, and answers only loopback `Host`s.

use crate::audit::{Actor, Audit, Entry};
use crate::auth::Automation;
use crate::engine::{Engine, Failure};
use crate::bytes::{hex, random};
use crate::identity::{sha256_hex, Identity};
use crate::mailbox::{Event, Mailbox};
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
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

type Body = Response<std::io::Cursor<Vec<u8>>>;

/// Files served in dev mode.
pub struct Dev {
    pub source: PathBuf,
    pub web_dir: PathBuf,
    pub wasm: PathBuf,
    pub reload: Mutex<Vec<Arc<Mailbox>>>,
}

impl Dev {
    pub fn notify_reload(&self) {
        if let Ok(mut subs) = self.reload.lock() {
            subs.retain(|s| s.push(Event::new(String::new(), None, "event: reload\ndata: {}\n\n".into())));
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
    pub dev: Option<Arc<Dev>>,
    pub automation: Automation,
    pub identity: Identity,
    pub audit: Audit,
    /// Refuse requests that carry credentials, as this server only speaks plain HTTP and
    /// nobody said a proxy in front of it terminates TLS (see [`credentials_need_tls`]).
    pub credentials_need_tls: bool,
    /// Behind a proxy: take the client address from `X-Forwarded-For` (`TJ_TRUST_PROXY=1`).
    pub trust_proxy: bool,
    streams: Mutex<HashMap<String, usize>>,
    tickets: Mutex<HashMap<String, Ticket>>,
}

impl Front {
    pub fn new(engine: Engine, dev: Option<Arc<Dev>>, automation: Automation, identity: Identity, audit: Audit, credentials_need_tls: bool) -> Front {
        let trust_proxy = std::env::var("TJ_TRUST_PROXY").is_ok_and(|v| v == "1");
        Front { engine, dev, automation, identity, audit, credentials_need_tls, trust_proxy, streams: Mutex::default(), tickets: Mutex::default() }
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
    let early = if front.dev.is_some() && !trusted_host(&req) {
        Some(("host", 403, "tj dev answers only localhost and IP addresses, not other host names".to_string()))
    } else if front.credentials_need_tls && authorization.is_some() {
        Some(("plain http", 403, format!("credentials are refused over plain HTTP, where anyone on the way can read them; put this server behind a proxy that terminates TLS and set {TLS_TERMINATED_ENV}=1")))
    } else if admin {
        front.automation.check(authorization.as_deref()).err().map(|(status, e)| ("admin", status, e))
    } else {
        None
    };
    if let Some((asked, status, e)) = early {
        refused(front, authorization.as_deref(), (asked, None, None), status);
        let _ = req.respond(json_response(status, &json!({ "error": e })));
        front.engine.stats().request(started.elapsed(), false);
        return;
    }
    let method = req.method().clone();
    let engine = &front.engine;
    let mut asked: Option<Asked> = None;
    let response = match (&method, segs.as_slice()) {
        (Method::Get, ["api", "events"]) => return stream(req, front, authorization),
        (Method::Post, ["api", "events", "ticket"]) => {
            asked = Some(("ticket", None, None));
            result_response(ticket(&mut req, front, authorization.as_deref()))
        }
        (Method::Get, ["api", class, id]) => {
            asked = Some(("get", Some(class), None));
            let object = format!("{class}/{id}");
            result_response(front.identity.verify(authorization.as_deref()).and_then(|v| engine.request(Input::Get { object, reply: None, caller: Some(v.caller) })))
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
            let mut who = None;
            let r = r.and_then(|args| {
                let caller = front.identity.verify(authorization.as_deref())?.caller;
                who = Some(Actor::of(&caller));
                engine.request(Input::Call { object: object.clone(), method: name.to_string(), args, reply: None, call, caller: Some(caller) })
            });
            let status = r.as_ref().map_or_else(|(status, _)| *status, |_| 200);
            // Refusals are written below, as security events.
            if let Some(who) = who.filter(|_| !matches!(status, 401 | 403)) {
                front.audit.record(Entry::new("call").who(who).object(&object).text("method", name).num("status", status.into()).num("ms", started.elapsed().as_millis() as u64));
            }
            result_response(r)
        }
        // A server's inspect also has its own counters: requests, latency, realtime fan-out.
        (Method::Get, ["_tj", "inspect"]) => result_response(front.audit.security(Entry::new("admin").who(Actor::Admin).text("request", "inspect")).and_then(|()| engine.request(Input::Inspect { reply: None })).map(|mut s| {
            s["server"] = engine.stats().json();
            s
        })),
        // Prometheus and Grafana: the same statistics, without the tree and object states.
        (Method::Get, ["_tj", "metrics"]) => match engine.core_stats() {
            Ok(core) => Response::from_data(engine.stats().openmetrics(&core).into_bytes()).with_header(header("Content-Type", "application/openmetrics-text; version=1.0.0; charset=utf-8")),
            Err((status, e)) => json_response(status, &json!({ "error": e })),
        },
        (Method::Post, ["_tj", "input"]) => result_response(read_json(&mut req).and_then(|j| {
            let mut input = serde_json::from_value::<Input>(j).map_err(|e| (400, format!("not a protocol input: {e}")))?;
            // Automation is trusted (it already passed the admin key), so it says so explicitly.
            if let Input::Call { caller: c @ None, .. } | Input::Get { caller: c @ None, .. } = &mut input {
                *c = Some(Caller::trusted());
            }
            front.audit.security(admin_input(&input))?;
            engine.request(input)
        })),
        (Method::Get, rest) if front.dev.is_some() => {
            let dev = front.dev.as_deref().expect("checked");
            match rest {
                [""] | ["index.html"] => file(dev.web_dir.join("index.html"), "text/html; charset=utf-8"),
                ["tinyjump.js"] => file(dev.web_dir.join("tinyjump.js"), "text/javascript; charset=utf-8"),
                ["tinyjump.wasm"] => file(dev.wasm.clone(), "application/wasm"),
                // The whole app as one bundle: the entry and the files it imports.
                ["app.tj"] => match crate::app::read(&dev.source) {
                    Ok(app) => Response::from_data(app.source).with_header(header("Content-Type", "text/plain; charset=utf-8")).with_header(header("Cache-Control", "no-store")),
                    Err(e) => json_response(404, &json!({ "error": e })),
                },
                ["_tj", "reload"] => {
                    let events = Arc::new(Mailbox::new(Vec::new(), None));
                    if let Ok(mut subs) = dev.reload.lock() {
                        subs.push(events.clone());
                    }
                    return sse(req, &events, || true);
                }
                _ => json_response(404, &json!({ "error": "not found" })),
            }
        }
        _ => json_response(404, &json!({ "error": "not found" })),
    };
    let status = response.status_code().0;
    if let Some(asked) = asked.filter(|_| matches!(status, 401 | 403)) {
        refused(front, authorization.as_deref(), asked, status);
    }
    let _ = req.respond(response);
    engine.stats().request(started.elapsed(), status < 400);
}

/// A refusal, written to the audit log before the answer goes out: who (re-checked here, so
/// only refusals pay for it), what was asked, and the status. Never credentials, bodies or
/// arguments. The request is refused either way, so a log that cannot be written changes
/// nothing; the writer reports that itself.
fn refused(front: &Front, authorization: Option<&str>, (asked, class, method): Asked, status: u16) {
    let who = front.identity.verify(authorization).map_or(Actor::Invalid, |v| Actor::of(&v.caller));
    let mut e = Entry::new("refused").who(who).text("request", asked).num("status", status.into());
    if let Some(class) = class {
        e = e.text("class", class);
    }
    if let Some(method) = method {
        e = e.text("method", method);
    }
    let _ = front.audit.security(e);
}

/// An admin input, for the audit log: what it does and to which object, never its values.
fn admin_input(input: &Input) -> Entry {
    let e = Entry::new("admin").who(Actor::Admin);
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

/// `POST /api/events/ticket`: a one-time ticket for a stream of the listed objects, bound to
/// the caller and those objects, stored only by its hash.
fn ticket(req: &mut Request, front: &Front, authorization: Option<&str>) -> Result<Json, Failure> {
    let body = read_json(req)?;
    let objects: Vec<String> = body["objects"].as_array().map(|a| a.iter().filter_map(|o| o.as_str().map(str::to_string)).collect()).unwrap_or_default();
    if objects.is_empty() {
        return Err((400, "list the objects to stream: {\"objects\": [\"Class/id\", ...]}".into()));
    }
    let verified = front.identity.verify(authorization)?;
    front.engine.may_read(objects.clone(), verified.caller.clone())?;
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

/// An admitted stream: its events, its slot in the per-caller cap, and the credential to
/// re-check while it is open.
struct Opened {
    events: Arc<Mailbox>,
    slot: Option<String>,
    credential: Option<String>,
}

/// `GET /api/events`: checks who may stream what, caps streams per caller, then streams.
fn stream(req: Request, front: &Front, authorization: Option<String>) {
    let opened = (|| -> Result<Opened, Failure> {
        let since_header = header_value(&req, "Last-Event-ID");
        let since = since_header.into_iter().chain(query_values(req.url(), "since")?).find_map(|s| s.trim().parse().ok()).unwrap_or(0);
        let ticket = query_values(req.url(), "ticket")?.into_iter().next();
        let (caller, objects, credential) = match ticket {
            Some(value) => {
                let t = front.tickets.lock().ok().and_then(|mut m| m.remove(&sha256_hex(&value))).filter(|t| t.expires > Instant::now());
                let t = t.ok_or((401, "this stream ticket is unknown, used or expired; get a new one".to_string()))?;
                (Some(t.caller), t.objects, t.credential)
            }
            None => {
                let objects = query_values(req.url(), "object")?;
                if objects.is_empty() {
                    // Every object, unfiltered, is for the admin only.
                    front.automation.check(authorization.as_deref()).map_err(|_| (403, "list the objects to stream: ?object=Class/id".to_string()))?;
                    (None, objects, None)
                } else {
                    (Some(front.identity.verify(authorization.as_deref())?.caller), objects, authorization.clone())
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
            Ok(events) => Ok(Opened { events, slot, credential: credential.filter(|_| caller_id.is_some()) }),
            Err(e) => {
                release(front, slot.as_deref());
                Err(e)
            }
        }
    })();
    match opened {
        Ok(Opened { events, slot, credential }) => {
            // Re-check the credential while the stream is open: expired tokens and revoked
            // keys must not keep receiving changes.
            let first = credential.as_deref().map(|c| front.identity.verify(Some(c)).ok().map(|v| (v.caller, v.expires)));
            let still_valid = || match (&credential, &first) {
                (Some(c), Some(Some((caller, expires)))) => {
                    expires.map_or(true, |t| SystemTime::now() < t) && front.identity.verify(Some(c)).is_ok_and(|v| v.caller == *caller)
                }
                (Some(_), _) => false,
                (None, _) => true,
            };
            sse(req, &events, still_valid);
            release(front, slot.as_deref());
        }
        Err((status, e)) => {
            if matches!(status, 401 | 403) {
                refused(front, authorization.as_deref(), ("stream", None, None), status);
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

fn client_ip(req: &Request, trust_proxy: bool) -> String {
    if trust_proxy {
        if let Some(first) = header_value(req, "X-Forwarded-For").and_then(|v| v.split(',').next().map(|s| s.trim().to_string())) {
            return first;
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
}

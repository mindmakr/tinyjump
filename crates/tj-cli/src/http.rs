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
use crate::engine::{CallFailure, Engine, Failure};
use crate::bytes::{hex, random};
use crate::identity::{sha256_hex, Identity};
use crate::leases::Leases;
use crate::mailbox::{Event, Mailbox};
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
    /// Refuse requests that carry credentials, as this server only speaks plain HTTP and
    /// nobody said a proxy in front of it terminates TLS (see [`credentials_need_tls`]).
    pub credentials_need_tls: bool,
    /// Behind a proxy: take the client address from `X-Forwarded-For` (`TJ_TRUST_PROXY=1`).
    pub trust_proxy: bool,
    streams: Mutex<HashMap<String, usize>>,
    tickets: Mutex<HashMap<String, Ticket>>,
}

impl Front {
    #[allow(clippy::too_many_arguments)]
    pub fn new(engine: Engine, web: Option<Arc<Web>>, dev: Option<Arc<Dev>>, automation: Automation, identity: Identity, audit: Audit, leases: Leases, credentials_need_tls: bool) -> Front {
        let trust_proxy = std::env::var("TJ_TRUST_PROXY").is_ok_and(|v| v == "1");
        Front { engine, web, dev, automation, identity, audit, leases, credentials_need_tls, trust_proxy, streams: Mutex::default(), tickets: Mutex::default() }
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
    } else if front.credentials_need_tls && authorization.is_some() {
        Some(("plain http", 403, format!("credentials are refused over plain HTTP, where anyone on the way can read them; put this server behind a proxy that terminates TLS and set {TLS_TERMINATED_ENV}=1")))
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
    let method = req.method().clone();
    let engine = &front.engine;
    let dev_mode = front.dev.is_some();
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
            // The engine writes the call to the audit log as it answers it; refusals are
            // written below, as security events.
            call_response(r.map_err(|(s, m)| (s, m, None)).and_then(|args| {
                let caller = front.identity.verify(authorization.as_deref()).map_err(|(s, m)| (s, m, None))?.caller;
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
            front.identity.verify(authorization.as_deref())
        }.map(|v| match v.caller {
            Caller::Remote { id, roles } => json!({ "id": id, "roles": roles, "dev": front.dev.is_some() }),
            Caller::Trusted(_) => json!({ "id": "", "roles": [], "dev": front.dev.is_some() }),
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
                if matches!(input, Input::Ui { .. } | Input::Layout { .. }) {
                    if let Some(r) = front.dev.as_ref().and_then(|d| d.ask("input", &j)) {
                        return r;
                    }
                }
                // Automation is trusted (it already passed the scope check), so it says so explicitly.
                if let Input::Call { caller: c @ None, .. } | Input::Get { caller: c @ None, .. } = &mut input {
                    *c = Some(Caller::trusted());
                }
                engine.request(input)
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
                ["tinyjump.wasm"] => file(web.wasm.clone(), "application/wasm"),
                // The whole app as one bundle: the entry and the files it imports.
                ["app.tj"] => match crate::app::read(&web.source) {
                    Ok(app) => Response::from_data(app.source).with_header(header("Content-Type", "text/plain; charset=utf-8")).with_header(header("Cache-Control", "no-store")),
                    Err(e) => json_response(404, &json!({ "error": e })),
                },
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
fn ticket(req: &mut Request, front: &Front, authorization: Option<&str>) -> Result<Json, Failure> {
    let body = read_json(req)?;
    let objects: Vec<String> = body["objects"].as_array().map(|a| a.iter().filter_map(|o| o.as_str().map(str::to_string)).collect()).unwrap_or_default();
    if objects.is_empty() {
        return Err((400, "list the objects to stream: {\"objects\": [\"Class/id\", ...]}".into()));
    }
    let verified = front.identity.verify(authorization)?;
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
fn stream(req: Request, front: &Front, authorization: Option<String>) {
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
                    (Some(front.identity.verify(authorization.as_deref())?.caller), objects, authorization.clone().map(StreamAuth::User))
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
}

//! HTTP front door. Every public object method is an API endpoint, object changes
//! stream as server-sent events, and the automation surface is under `/_tj/`.
//!
//!   POST /api/{Class}/{id}/{method}   body: JSON array of arguments -> {"ok": result} | {"error": msg}
//!                                     an Idempotency-Key header makes repeats return the first result
//!   GET  /api/{Class}/{id}            -> {"ok": state}
//!   GET  /api/events?object=Class/id&since=N
//!                                     -> SSE stream of `emit` and `changed`; it starts with
//!                                        the listed objects changed after N (or after the
//!                                        `Last-Event-ID` header), then streams live
//!   GET  /_tj/inspect                 -> semantic tree + object states
//!   POST /_tj/input                   body: one protocol Input
//!   (`/_tj/*` needs the admin key on `tj serve`; see auth.rs)
//!
//! In dev mode it also serves the web host, the WASM runtime and the app source,
//! plus `/_tj/reload` (SSE) for live reload.

use crate::auth::Automation;
use crate::engine::Engine;
use crate::mailbox::{Event, Mailbox};
use serde_json::{json, Value as Json};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Request, Response, Server};
use tj_core::Input;

const MAX_BODY: u64 = 1 << 20;
const MAX_CONNECTIONS: usize = 512;
const SSE_KEEPALIVE: Duration = Duration::from_secs(15);

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
            subs.retain(|s| s.push(Event::new(String::new(), false, "event: reload\ndata: {}\n\n".into())));
        }
    }
}

pub fn serve(addr: &str, engine: Engine, dev: Option<Arc<Dev>>, automation: Automation) -> Result<(), String> {
    let server = Server::http(addr).map_err(|e| format!("cannot listen on {addr}: {e}"))?;
    let active = Arc::new(AtomicUsize::new(0));
    let automation = Arc::new(automation);
    for req in server.incoming_requests() {
        if active.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
            let _ = req.respond(json_response(503, &json!({ "error": "busy" })));
            continue;
        }
        active.fetch_add(1, Ordering::Relaxed);
        let (engine, dev, counter, automation) = (engine.clone(), dev.clone(), active.clone(), automation.clone());
        let spawned = thread::Builder::new().name("tj-http".into()).spawn(move || {
            handle(req, &engine, dev.as_deref(), &automation);
            counter.fetch_sub(1, Ordering::Relaxed);
        });
        if spawned.is_err() {
            active.fetch_sub(1, Ordering::Relaxed);
        }
    }
    Ok(())
}

fn json_response(status: u16, body: &Json) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_data(body.to_string()).with_status_code(status).with_header(header("Content-Type", "application/json"))
}

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).expect("static header is valid")
}

fn result_response(r: Result<Json, String>) -> Response<std::io::Cursor<Vec<u8>>> {
    match r {
        Ok(v) => json_response(200, &json!({ "ok": v })),
        Err(e) => json_response(400, &json!({ "error": e })),
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

fn read_json(req: &mut Request) -> Result<Json, String> {
    let mut body = String::new();
    req.as_reader().take(MAX_BODY + 1).read_to_string(&mut body).map_err(|e| format!("cannot read body: {e}"))?;
    if body.len() as u64 > MAX_BODY {
        return Err("body too large".into());
    }
    if body.trim().is_empty() {
        return Ok(Json::Null);
    }
    serde_json::from_str(&body).map_err(|e| format!("body is not JSON: {e}"))
}

fn handle(mut req: Request, engine: &Engine, dev: Option<&Dev>, automation: &Automation) {
    let started = Instant::now();
    let path = req.url().split('?').next().unwrap_or("/").to_string();
    let segs: Option<Vec<String>> = path.trim_start_matches('/').split('/').map(percent_decode).collect();
    let Some(segs) = segs else {
        let _ = req.respond(json_response(400, &json!({ "error": "bad path encoding" })));
        return;
    };
    let segs: Vec<&str> = segs.iter().map(String::as_str).collect();
    if segs.first() == Some(&"_tj") {
        if let Err((status, e)) = automation.check(header_value(&req, "Authorization").as_deref()) {
            let _ = req.respond(json_response(status, &json!({ "error": e })));
            engine.stats().request(started.elapsed(), false);
            return;
        }
    }
    let method = req.method().clone();
    let response = match (&method, segs.as_slice()) {
        (Method::Get, ["api", "events"]) => {
            let only = query_values(req.url(), "object");
            // A browser's EventSource reconnects with the id of the last event it saw.
            let resume = header_value(&req, "Last-Event-ID");
            let since = resume.into_iter().chain(query_values(req.url(), "since")).find_map(|s| s.trim().parse().ok()).unwrap_or(0);
            return sse(req, &engine.subscribe(only, since));
        }
        (Method::Get, ["api", class, id]) => result_response(engine.request(Input::Get { object: format!("{class}/{id}"), reply: None })),
        (Method::Post, ["api", class, id, name]) => match (read_json(&mut req), header_value(&req, "Idempotency-Key")) {
            // With an Idempotency-Key, a repeated request returns the first result without running again.
            (Ok(Json::Null), call) => result_response(engine.request(Input::Call { object: format!("{class}/{id}"), method: name.to_string(), args: Vec::new(), reply: None, call })),
            (Ok(Json::Array(args)), call) => result_response(engine.request(Input::Call { object: format!("{class}/{id}"), method: name.to_string(), args, reply: None, call })),
            (Ok(_), _) => json_response(400, &json!({ "error": "body must be a JSON array of arguments" })),
            (Err(e), _) => json_response(400, &json!({ "error": e })),
        },
        // A server's inspect also has its own counters: requests, latency, realtime fan-out.
        (Method::Get, ["_tj", "inspect"]) => result_response(engine.request(Input::Inspect { reply: None }).map(|mut s| {
            s["server"] = engine.stats().json();
            s
        })),
        (Method::Post, ["_tj", "input"]) => match read_json(&mut req).and_then(|j| serde_json::from_value::<Input>(j).map_err(|e| format!("not a protocol input: {e}"))) {
            Ok(input) => result_response(engine.request(input)),
            Err(e) => json_response(400, &json!({ "error": e })),
        },
        (Method::Get, rest) if dev.is_some() => {
            let dev = dev.expect("checked");
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
                    return sse(req, &events);
                }
                _ => json_response(404, &json!({ "error": "not found" })),
            }
        }
        _ => json_response(404, &json!({ "error": "not found" })),
    };
    let ok = response.status_code().0 < 400;
    let _ = req.respond(response);
    engine.stats().request(started.elapsed(), ok);
}

fn header_value(req: &Request, name: &'static str) -> Option<String> {
    req.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.as_str().to_string())
}

fn file(path: PathBuf, content_type: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    match std::fs::read(&path) {
        Ok(bytes) => Response::from_data(bytes).with_header(header("Content-Type", content_type)).with_header(header("Cache-Control", "no-store")),
        Err(e) => json_response(404, &json!({ "error": format!("{}: {e}", path.display()) })),
    }
}

/// All values of a query parameter, percent-decoded.
fn query_values(url: &str, name: &str) -> Vec<String> {
    let Some((_, query)) = url.split_once('?') else { return Vec::new() };
    query.split('&').filter_map(|pair| pair.split_once('=')).filter(|(k, _)| *k == name).filter_map(|(_, v)| percent_decode(&v.replace('+', " "))).collect()
}

/// Streams server-sent events from `events` until the client goes away.
fn sse(req: Request, events: &Mailbox) {
    let mut w = req.into_writer();
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n";
    let mut ok = w.write_all(head.as_bytes()).and_then(|()| w.flush()).is_ok();
    events.connected(true);
    while ok {
        let batch = events.take(SSE_KEEPALIVE);
        let chunk: String = if batch.is_empty() { ":\n\n".to_string() } else { batch.iter().map(|e| e.text.as_str()).collect() };
        ok = w.write_all(chunk.as_bytes()).and_then(|()| w.flush()).is_ok();
        if ok {
            events.delivered(&batch);
        }
    }
    events.connected(false);
    events.close();
}

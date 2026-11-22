//! `tj mcp`: a Model Context Protocol server on stdio, so coding agents can inspect, drive,
//! screenshot and review a running app on any host that serves the automation API
//! (`tj serve`/`tj dev`, Android and iOS debug builds).

use crate::review;
use serde_json::{json, Value as Json};
use std::cell::RefCell;
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

const MAX_IMAGE: u64 = 20 << 20;

struct Recorder {
    file: Option<PathBuf>,
    steps: Vec<Json>,
}

pub fn run(target: &str, record_file: Option<PathBuf>) -> Result<(), String> {
    let client = Client {
        agent: ureq::AgentBuilder::new().timeout(Duration::from_secs(60)).build(),
        target: target.trim_end_matches('/').to_string(),
        auth: crate::auth::client_header(),
        recorder: RefCell::new(None),
    };
    if let Some(f) = record_file {
        let mut steps = Vec::new();
        if let Some((id, roles)) = client.whoami() {
            steps.push(json!({ "do": "as", "id": id, "roles": roles }));
        }
        *client.recorder.borrow_mut() = Some(Recorder { file: Some(f), steps });
    }
    let mut stdout = std::io::stdout();
    for line in std::io::stdin().lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        // Some shells prefix piped input with a byte-order mark.
        let line = line.trim_start_matches('\u{feff}');
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Json>(line) {
            Err(e) => json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": e.to_string() } }),
            Ok(msg) => {
                // Notifications (no id) need no answer.
                let Some(id) = msg.get("id").cloned() else { continue };
                match client.handle(msg["method"].as_str().unwrap_or(""), &msg["params"]) {
                    Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                    Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
                }
            }
        };
        writeln!(stdout, "{reply}").and_then(|()| stdout.flush()).map_err(|e| e.to_string())?;
    }
    // Flush any active --record trace on clean EOF.
    let _ = client.flush_recorder();
    Ok(())
}

fn tools() -> Json {
    let none = json!({ "type": "object", "properties": {} });
    json!([
        { "name": "language", "description": "The tinyjump language reference: everything that differs from TypeScript and React, the UI components and the built-ins. Load it before writing or changing app code.", "inputSchema": none },
        { "name": "manifest", "description": "The app's canonical contract (code hash, files, objects, fields, access rules, personal data, plugins, warnings) plus live server health and active work leases.", "inputSchema": none },
        { "name": "inspect", "description": "The app's screen as an outline ([node] kind \"text\" #id @x,y wxh on:events), plus object state and work counters.", "inputSchema": none },
        { "name": "act", "description": "Fire a UI event like a person would: press a button, type into an input, flip a toggle.",
          "inputSchema": { "type": "object", "required": ["target", "event"], "properties": {
            "target": { "type": "string", "description": "#id, the node's visible text, or its node number" },
            "event": { "type": "string", "description": "onPress, onChange or onSubmit" },
            "value": { "description": "the typed text for onChange/onSubmit, or true/false for a Toggle" } } } },
        { "name": "call", "description": "Call a public method of an object, e.g. object \"Todos/main\", method \"add\".",
          "inputSchema": { "type": "object", "required": ["object", "method"], "properties": {
            "object": { "type": "string", "description": "Class/id" }, "method": { "type": "string" }, "args": { "type": "array" } } } },
        { "name": "get", "description": "Read an object's state.",
          "inputSchema": { "type": "object", "required": ["object"], "properties": { "object": { "type": "string", "description": "Class/id" } } } },
        { "name": "screenshot", "description": "Capture the screen as an image (iOS and Android hosts).", "inputSchema": none },
        { "name": "review", "description": "Check the screen for layout (overflow, overlap), accessibility, touch-target and contrast problems; reports work counters for performance tracking.", "inputSchema": none },
        { "name": "input", "description": "Send any protocol input (advanced), e.g. {\"type\":\"tick\"}. See docs/host-protocol.md.",
          "inputSchema": { "type": "object", "required": ["input"], "properties": { "input": { "type": "object" } } } },
        { "name": "record_trace", "description": "Record subsequent act, call, get, review and tick steps as a deterministic .trace.jsonl file for `tj test`.",
          "inputSchema": { "type": "object", "required": ["action"], "properties": {
            "action": { "type": "string", "description": "start, stop or status" },
            "file": { "type": "string", "description": "optional output path for action=start (e.g. repro.trace.jsonl)" } } } }
    ])
}

/// `#add` → by id, `12` → by node number, anything else → by visible text.
fn target(s: &str) -> Json {
    if let Some(id) = s.strip_prefix('#') {
        json!({ "id": id })
    } else if let Ok(node) = s.parse::<u64>() {
        json!({ "node": node })
    } else {
        json!({ "text": s })
    }
}

struct Client {
    agent: ureq::Agent,
    target: String,
    /// `Authorization` for a `tj serve` target, from `TJ_OPS_KEY` or `TJ_ADMIN_KEY`.
    auth: Option<String>,
    recorder: RefCell<Option<Recorder>>,
}

impl Client {
    fn handle(&self, method: &str, params: &Json) -> Result<Json, (i64, String)> {
        match method {
            "initialize" => Ok(json!({
                "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "tinyjump", "version": env!("CARGO_PKG_VERSION") },
                "instructions": format!("Drives the tinyjump app at {}. Before writing app code, load `language` (the language reference). Start with `inspect`, act with `act`, check with `review` and `screenshot`.", self.target),
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => {
                let args = &params["arguments"];
                let content = match self.tool(params["name"].as_str().unwrap_or(""), args) {
                    Ok(content) => return Ok(json!({ "content": content })),
                    Err(e) => json!([{ "type": "text", "text": e }]),
                };
                Ok(json!({ "content": content, "isError": true }))
            }
            _ => Err((-32601, format!("method not found: {method}"))),
        }
    }

    fn tool(&self, name: &str, a: &Json) -> Result<Json, String> {
        let text = |s: String| Ok(json!([{ "type": "text", "text": s }]));
        match name {
            "language" => text(crate::LANGUAGE.to_string()),
            "manifest" => text(serde_json::to_string_pretty(&self.manifest()?).unwrap_or_default()),
            "inspect" => {
                let s = self.inspect()?;
                let objects = serde_json::to_string_pretty(&s["objects"]).unwrap_or_default();
                // Under `tj dev`, the screen's objects come from the tab and the server's own
                // from the server.
                let server = match s.get("serverObjects") {
                    Some(o) => format!("\nserver objects:\n{}", serde_json::to_string_pretty(o).unwrap_or_default()),
                    None => String::new(),
                };
                text(format!("{}\nobjects:\n{objects}{server}\nmetrics: {}", review::outline(&s["tree"]), s["metrics"]))
            }
            "review" => {
                let s = self.inspect()?;
                let w = s["viewport"]["w"].as_f64().unwrap_or(390.0);
                let h = s["viewport"]["h"].as_f64().unwrap_or(844.0);
                self.record_step(json!({ "do": "assertLayout", "width": w, "height": h, "review": true }));
                text(review::report(&s, &review::review(&s)))
            }
            "act" => {
                let t = a["target"].as_str().unwrap_or("");
                let val = a.get("value").cloned().unwrap_or(Json::Null);
                let input = json!({ "type": "ui", "target": target(t), "event": a["event"], "value": val });
                let r = self.input(&input);
                let mut rec = json!({ "do": "ui", "target": t, "event": a["event"] });
                if !val.is_null() {
                    rec["value"] = val;
                }
                if let Err(e) = &r {
                    rec["expectError"] = json!(e);
                }
                self.record_step(rec);
                r?;
                let s = self.inspect()?;
                text(format!("done; the screen is now:\n{}", review::outline(&s["tree"])))
            }
            "call" => {
                let args = a.get("args").cloned().unwrap_or(json!([]));
                let input = json!({ "type": "call", "object": a["object"], "method": a["method"], "args": args });
                let r = self.input(&input);
                let mut rec = json!({ "do": "call", "object": a["object"], "method": a["method"], "args": args, "as": "trusted" });
                match &r {
                    Ok(v) => rec["expect"] = v.clone(),
                    Err(e) => rec["expectError"] = json!(e),
                }
                self.record_step(rec);
                text(r?.to_string())
            }
            "get" => {
                let r = self.input(&json!({ "type": "get", "object": a["object"] }));
                let mut rec = json!({ "do": "get", "object": a["object"], "as": "trusted" });
                match &r {
                    Ok(v) => rec["expect"] = v.clone(),
                    Err(e) => rec["expectError"] = json!(e),
                }
                self.record_step(rec);
                text(serde_json::to_string_pretty(&r?).unwrap_or_default())
            }
            "input" => {
                let inp = &a["input"];
                let r = self.input(inp);
                if inp["type"].as_str() == Some("tick") {
                    let mut step = json!({ "do": "tick" });
                    if let Some(ms) = inp.get("advanceMs").and_then(Json::as_f64) {
                        step["advanceMs"] = json!(ms);
                    } else if let Some(adv) = inp.get("advance").and_then(Json::as_str) {
                        step["advance"] = json!(adv);
                    } else {
                        step["advanceMs"] = json!(1000);
                    }
                    self.record_step(step);
                }
                text(r?.to_string())
            }
            "record_trace" => text(self.record_trace(a)?),
            "screenshot" => {
                let png = self.screenshot()?;
                Ok(json!([{ "type": "image", "data": crate::bytes::base64(&png), "mimeType": "image/png" }]))
            }
            _ => Err(format!("unknown tool {name}")),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.target)
    }

    fn unreachable(&self, e: impl std::fmt::Display) -> String {
        format!("cannot reach the app at {}: {e}. Is it running with automation on (tj dev, or a debug build with adb forward / the iOS Simulator)?", self.target)
    }

    /// The `ok` of an automation reply, or its `error`.
    fn result(&self, resp: Result<ureq::Response, ureq::Error>) -> Result<Json, String> {
        let resp = match resp {
            Ok(r) | Err(ureq::Error::Status(_, r)) => r,
            Err(e) => return Err(self.unreachable(e)),
        };
        let body: Json = serde_json::from_str(&resp.into_string().map_err(|e| e.to_string())?).map_err(|e| format!("bad reply: {e}"))?;
        match body.get("error").and_then(Json::as_str) {
            Some(e) => Err(e.to_string()),
            None => Ok(body["ok"].clone()),
        }
    }

    /// A request to the app's automation endpoint `path`.
    fn request(&self, method: &str, path: &str) -> ureq::Request {
        let req = self.agent.request(method, &self.url(path));
        match &self.auth {
            Some(h) => req.set("Authorization", h),
            None => req,
        }
    }

    fn inspect(&self) -> Result<Json, String> {
        self.result(self.request("GET", "/_tj/inspect").call())
    }

    fn manifest(&self) -> Result<Json, String> {
        self.result(self.request("GET", "/_tj/manifest").call())
    }

    fn record_step(&self, step: Json) {
        if let Some(rec) = self.recorder.borrow_mut().as_mut() {
            rec.steps.push(step);
        }
    }

    fn flush_recorder(&self) -> Result<Option<(Option<PathBuf>, String)>, String> {
        let Some(rec) = self.recorder.borrow_mut().take() else { return Ok(None) };
        let body = rec.steps.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("\n") + if rec.steps.is_empty() { "" } else { "\n" };
        if let Some(path) = &rec.file {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            }
            std::fs::write(path, &body).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        }
        Ok(Some((rec.file, body)))
    }

    fn whoami(&self) -> Option<(String, Vec<String>)> {
        let resp = self.agent.get(&self.url("/api/whoami")).call().ok()?;
        let body: Json = serde_json::from_str(&resp.into_string().ok()?).ok()?;
        let ok = body.get("ok")?;
        let id = ok["id"].as_str().filter(|s| !s.is_empty())?.to_string();
        let roles = ok["roles"].as_array().map(|a| a.iter().filter_map(|r| r.as_str().map(str::to_string)).collect()).unwrap_or_default();
        Some((id, roles))
    }

    fn record_trace(&self, a: &Json) -> Result<String, String> {
        match a["action"].as_str().unwrap_or("") {
            "start" => {
                let file = a["file"].as_str().filter(|s| !s.is_empty()).map(PathBuf::from);
                let mut steps = Vec::new();
                if let Some((id, roles)) = self.whoami() {
                    steps.push(json!({ "do": "as", "id": id, "roles": roles }));
                }
                *self.recorder.borrow_mut() = Some(Recorder { file: file.clone(), steps });
                match file {
                    Some(p) => Ok(format!("recording trace to {}", p.display())),
                    None => Ok("recording trace in memory (call record_trace stop to retrieve)".into()),
                }
            }
            "stop" => match self.flush_recorder()? {
                Some((Some(path), body)) => Ok(format!("saved {} step(s) to {}\n{body}", body.lines().count(), path.display())),
                Some((None, body)) => Ok(format!("recorded {} step(s):\n{body}", body.lines().count())),
                None => Err("no trace recording is active; call record_trace with action \"start\" first".into()),
            },
            "status" => match self.recorder.borrow().as_ref() {
                Some(rec) => Ok(format!("recording ({} step(s){})", rec.steps.len(), rec.file.as_ref().map(|p| format!(", file: {}", p.display())).unwrap_or_default())),
                None => Ok("not recording".into()),
            },
            other => Err(format!("record_trace action must be start, stop or status, got {other:?}")),
        }
    }

    fn input(&self, input: &Json) -> Result<Json, String> {
        self.result(self.request("POST", "/_tj/input").set("Content-Type", "application/json").send_string(&input.to_string()))
    }

    fn screenshot(&self) -> Result<Vec<u8>, String> {
        let resp = match self.request("GET", "/_tj/screenshot").call() {
            Ok(r) => r,
            Err(ureq::Error::Status(404, _)) => return Err("this host cannot take screenshots yet".into()),
            Err(e) => return Err(self.unreachable(e)),
        };
        let mut png = Vec::new();
        resp.into_reader().take(MAX_IMAGE).read_to_end(&mut png).map_err(|e| e.to_string())?;
        Ok(png)
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_parse() {
        assert_eq!(target("#add"), json!({ "id": "add" }));
        assert_eq!(target("12"), json!({ "node": 12 }));
        assert_eq!(target("Clear done"), json!({ "text": "Clear done" }));
    }

    #[test]
    fn speaks_mcp_without_a_running_app() {
        let c = Client { agent: ureq::AgentBuilder::new().timeout(Duration::from_millis(200)).build(), target: "http://127.0.0.1:9".into(), auth: None, recorder: RefCell::new(None) };
        let init = c.handle("initialize", &json!({ "protocolVersion": "2025-06-18" })).unwrap();
        assert_eq!(init["capabilities"]["tools"], json!({}));
        let names: Vec<String> = c.handle("tools/list", &json!({})).unwrap()["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, ["language", "manifest", "inspect", "act", "call", "get", "screenshot", "review", "input", "record_trace"]);
        // The language reference works without an app: agents load it before writing code.
        let docs = c.handle("tools/call", &json!({ "name": "language", "arguments": {} })).unwrap();
        assert!(docs["content"][0]["text"].as_str().unwrap().starts_with("# The tinyjump language"));
        let call = c.handle("tools/call", &json!({ "name": "inspect", "arguments": {} })).unwrap();
        assert_eq!(call["isError"], json!(true));
        assert!(call["content"][0]["text"].as_str().unwrap().contains("cannot reach the app"));
        assert_eq!(c.handle("nope", &json!({})).unwrap_err().0, -32601);

        // Trace recording writes valid JSONL lines and saves to disk on stop.
        let dir = std::env::temp_dir().join(format!("tj-mcp-rec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let out_file = dir.join("repro.trace.jsonl");
        let start = c.handle("tools/call", &json!({ "name": "record_trace", "arguments": { "action": "start", "file": out_file } })).unwrap();
        assert!(start["content"][0]["text"].as_str().unwrap().contains("recording trace"));
        c.record_step(json!({ "do": "ui", "target": "#add", "event": "onPress" }));
        c.record_step(json!({ "do": "call", "object": "Todos/main", "method": "add", "args": [], "expect": null }));
        let stop = c.handle("tools/call", &json!({ "name": "record_trace", "arguments": { "action": "stop" } })).unwrap();
        assert!(stop["content"][0]["text"].as_str().unwrap().contains("saved 2 step(s)"));
        let saved = std::fs::read_to_string(&out_file).unwrap();
        assert_eq!(saved.lines().count(), 2);
        assert!(saved.contains("\"do\":\"ui\"") && saved.contains("\"do\":\"call\""), "{saved}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_trace_captures_whoami_identity_and_tick_advance() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                let body = if req.url() == "/api/whoami" {
                    r#"{"ok":{"id":"dev:ana","roles":["staff"],"dev":true}}"#
                } else {
                    r#"{"ok":null}"#
                };
                let _ = req.respond(tiny_http::Response::from_string(body));
            }
        });
        let c = Client {
            agent: ureq::AgentBuilder::new().timeout(Duration::from_secs(2)).build(),
            target: format!("http://127.0.0.1:{port}"),
            auth: None,
            recorder: RefCell::new(None),
        };
        c.handle("tools/call", &json!({ "name": "record_trace", "arguments": { "action": "start" } })).unwrap();
        c.handle("tools/call", &json!({ "name": "input", "arguments": { "input": { "type": "tick", "advanceMs": 5000 } } })).unwrap();
        let stop = c.handle("tools/call", &json!({ "name": "record_trace", "arguments": { "action": "stop" } })).unwrap();
        let text = stop["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"do\":\"as\"") && text.contains("\"dev:ana\"") && text.contains("\"staff\""), "{text}");
        assert!(text.contains("\"do\":\"tick\"") && text.contains("\"advanceMs\":5000"), "{text}");
    }
}

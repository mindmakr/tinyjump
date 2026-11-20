//! `tj mcp`: a Model Context Protocol server on stdio, so coding agents can inspect, drive,
//! screenshot and review a running app on any host that serves the automation API
//! (`tj serve`/`tj dev`, Android and iOS debug builds).

use crate::review;
use serde_json::{json, Value as Json};
use std::io::{BufRead, Read, Write};
use std::time::Duration;

const MAX_IMAGE: u64 = 20 << 20;

pub fn run(target: &str) -> Result<(), String> {
    let client = Client { agent: ureq::AgentBuilder::new().timeout(Duration::from_secs(60)).build(), target: target.trim_end_matches('/').to_string(), auth: crate::auth::client_header() };
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
    Ok(())
}

fn tools() -> Json {
    let none = json!({ "type": "object", "properties": {} });
    json!([
        { "name": "language", "description": "The tinyjump language reference: everything that differs from TypeScript and React, the UI components and the built-ins. Load it before writing or changing app code.", "inputSchema": none },
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
          "inputSchema": { "type": "object", "required": ["input"], "properties": { "input": { "type": "object" } } } }
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
    /// `Authorization` for a `tj serve` target, from `TJ_ADMIN_KEY`.
    auth: Option<String>,
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
            "inspect" => {
                let s = self.inspect()?;
                let objects = serde_json::to_string_pretty(&s["objects"]).unwrap_or_default();
                text(format!("{}\nobjects:\n{objects}\nmetrics: {}", review::outline(&s["tree"]), s["metrics"]))
            }
            "review" => {
                let s = self.inspect()?;
                text(review::report(&s, &review::review(&s)))
            }
            "act" => {
                let input = json!({ "type": "ui", "target": target(a["target"].as_str().unwrap_or("")), "event": a["event"], "value": a.get("value").cloned().unwrap_or(Json::Null) });
                self.input(&input)?;
                let s = self.inspect()?;
                text(format!("done; the screen is now:\n{}", review::outline(&s["tree"])))
            }
            "call" => {
                let input = json!({ "type": "call", "object": a["object"], "method": a["method"], "args": a.get("args").cloned().unwrap_or(json!([])) });
                text(self.input(&input)?.to_string())
            }
            "get" => text(serde_json::to_string_pretty(&self.input(&json!({ "type": "get", "object": a["object"] }))?).unwrap_or_default()),
            "input" => text(self.input(&a["input"])?.to_string()),
            "screenshot" => {
                let png = self.screenshot()?;
                Ok(json!([{ "type": "image", "data": base64(&png), "mimeType": "image/png" }]))
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

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        for (input, expected) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64(input.as_bytes()), expected);
        }
    }

    #[test]
    fn targets_parse() {
        assert_eq!(target("#add"), json!({ "id": "add" }));
        assert_eq!(target("12"), json!({ "node": 12 }));
        assert_eq!(target("Clear done"), json!({ "text": "Clear done" }));
    }

    #[test]
    fn speaks_mcp_without_a_running_app() {
        let c = Client { agent: ureq::AgentBuilder::new().timeout(Duration::from_millis(200)).build(), target: "http://127.0.0.1:9".into(), auth: None };
        let init = c.handle("initialize", &json!({ "protocolVersion": "2025-06-18" })).unwrap();
        assert_eq!(init["capabilities"]["tools"], json!({}));
        let names: Vec<String> = c.handle("tools/list", &json!({})).unwrap()["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, ["language", "inspect", "act", "call", "get", "screenshot", "review", "input"]);
        // The language reference works without an app: agents load it before writing code.
        let docs = c.handle("tools/call", &json!({ "name": "language", "arguments": {} })).unwrap();
        assert!(docs["content"][0]["text"].as_str().unwrap().starts_with("# The tinyjump language"));
        let call = c.handle("tools/call", &json!({ "name": "inspect", "arguments": {} })).unwrap();
        assert_eq!(call["isError"], json!(true));
        assert!(call["content"][0]["text"].as_str().unwrap().contains("cannot reach the app"));
        assert_eq!(c.handle("nope", &json!({})).unwrap_err().0, -32601);
    }
}

//! Plugins that ship with tinyjump, as MCP servers on stdio (docs/plugins.md):
//!
//! - `tj plugin ai`: the `complete` tool, one turn of Claude (Anthropic Messages API, raw
//!   HTTP: Rust has no official SDK). `ANTHROPIC_API_KEY` is required; `TJ_AI_MODEL`
//!   (default `claude-opus-5-5`) and `TJ_AI_EFFORT` (default `medium`) are optional.
//! - `tj plugin fake <file.json>`: scripted answers, `{"tool": [answer, ...]}`, given in
//!   order and then repeated from the start, for tests and demos without a key.

use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

const API: &str = "https://api.anthropic.com/v1/messages";
const MODEL: &str = "claude-opus-5-5";
const MAX_TOKENS: u32 = 16000;

trait Tools {
    fn list(&self) -> Json;
    fn call(&self, tool: &str, args: &Json) -> Result<Json, String>;
}

/// Serves `name` (`ai` or `fake`) on stdin/stdout until stdin closes.
pub fn run(name: &str, file: Option<&Path>) -> Result<(), String> {
    let tools: Box<dyn Tools> = match name {
        "ai" => Box::new(Ai),
        "fake" => Box::new(Fake::load(file.ok_or("`tj plugin fake` needs a file of answers")?)?),
        other => return Err(format!("no built-in plugin {other}; built in: ai, fake")),
    };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        let Ok(msg) = serde_json::from_str::<Json>(&line) else { continue };
        let Some(id) = msg.get("id").cloned() else { continue }; // notifications need no answer
        let result = match msg["method"].as_str().unwrap_or("") {
            "initialize" => json!({
                "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": format!("tinyjump-{name}"), "version": env!("CARGO_PKG_VERSION") },
            }),
            "tools/list" => json!({ "tools": tools.list() }),
            "tools/call" => match tools.call(msg["params"]["name"].as_str().unwrap_or(""), &msg["params"]["arguments"]) {
                Ok(v) if v.is_object() => json!({ "content": [{ "type": "text", "text": v.to_string() }], "structuredContent": v }),
                Ok(v) => json!({ "content": [{ "type": "text", "text": v.to_string() }] }),
                Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
            },
            "ping" => json!({}),
            other => {
                writeln!(out, "{}", json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("method not found: {other}") } })).map_err(|e| e.to_string())?;
                out.flush().map_err(|e| e.to_string())?;
                continue;
            }
        };
        writeln!(out, "{}", json!({ "jsonrpc": "2.0", "id": id, "result": result })).map_err(|e| e.to_string())?;
        out.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

struct Ai;

impl Tools for Ai {
    fn list(&self) -> Json {
        json!([{
            "name": "complete",
            "description": "One model turn: system prompt, messages and tools in; text, tool calls and the stop reason out.",
            "inputSchema": { "type": "object", "properties": { "system": { "type": "string" }, "messages": { "type": "array" }, "tools": { "type": "array" } }, "required": ["messages"] },
        }])
    }

    fn call(&self, tool: &str, args: &Json) -> Result<Json, String> {
        if tool != "complete" {
            return Err(format!("the ai plugin has no tool {tool}"));
        }
        let key = std::env::var("ANTHROPIC_API_KEY").ok().filter(|k| !k.is_empty()).ok_or("ANTHROPIC_API_KEY is not set for the ai plugin; pass it in plugins.json \"env\"")?;
        let model = std::env::var("TJ_AI_MODEL").ok().filter(|m| !m.is_empty()).unwrap_or_else(|| MODEL.to_string());
        let request = request(&model, args);
        let resp = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(300))
            .build()
            .post(API)
            .set("content-type", "application/json")
            .set("x-api-key", &key)
            .set("anthropic-version", "2026-10-21")
            .set("anthropic-beta", "server-side-fallback-2026-07-01")
            .send_string(&request.to_string());
        let body: Json = match resp {
            Ok(r) => serde_json::from_str(&r.into_string().map_err(|e| e.to_string())?).map_err(|e| format!("unreadable answer from the model: {e}"))?,
            Err(ureq::Error::Status(status, r)) => {
                let detail: Json = serde_json::from_str(&r.into_string().unwrap_or_default()).unwrap_or_default();
                return Err(format!("the model API answered {status}: {}", detail["error"]["message"].as_str().unwrap_or("no details")));
            }
            Err(e) => return Err(format!("cannot reach the model API: {e}")),
        };
        Ok(reply(&body))
    }
}

/// The Messages API request for one `complete` call.
fn request(model: &str, args: &Json) -> Json {
    let messages: Vec<Json> = args["messages"].as_array().into_iter().flatten().map(message).collect();
    let tools: Vec<Json> = args["tools"].as_array().into_iter().flatten().map(|t| json!({ "name": t["name"], "description": t["description"], "input_schema": t["input"] })).collect();
    let effort = std::env::var("TJ_AI_EFFORT").ok().filter(|e| !e.is_empty()).unwrap_or_else(|| "medium".into());
    let mut req = json!({ "model": model, "max_tokens": MAX_TOKENS, "messages": messages, "output_config": { "effort": effort } });
    if let Some(system) = args["system"].as_str().filter(|s| !s.is_empty()) {
        req["system"] = json!(system);
    }
    if !tools.is_empty() {
        req["tools"] = json!(tools);
    }
    // On a refusal, these models can hand the turn to a fallback model on the server side.
    if ["claude-opus-5", "claude-fable-5", "claude-sonnet-5-5"].iter().any(|m| model.starts_with(m)) {
        req["fallbacks"] = json!("default");
    }
    req
}

/// A tinyjump message as an API message. Assistant turns go back exactly as the model sent
/// them (`raw`), which current models require.
fn message(m: &Json) -> Json {
    if m["role"] == "assistant" {
        return json!({ "role": "assistant", "content": m["raw"].clone() });
    }
    let mut content: Vec<Json> = m["results"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| json!({ "type": "tool_result", "tool_use_id": r["id"], "content": r["output"].as_str().unwrap_or(""), "is_error": r["error"].as_bool().unwrap_or(false) }))
        .collect();
    if let Some(text) = m["text"].as_str().filter(|t| !t.is_empty()) {
        content.push(json!({ "type": "text", "text": text }));
    }
    json!({ "role": "user", "content": content })
}

/// The model's answer as a tinyjump `Reply`.
fn reply(body: &Json) -> Json {
    let blocks = body["content"].as_array().cloned().unwrap_or_default();
    let text: String = blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("");
    let calls: Vec<Json> = blocks.iter().filter(|b| b["type"] == "tool_use").map(|b| json!({ "id": b["id"], "name": b["name"], "input": b["input"] })).collect();
    json!({ "text": text, "calls": calls, "stop": body["stop_reason"].as_str().unwrap_or("end_turn"), "raw": blocks })
}

struct Fake {
    answers: HashMap<String, Vec<Json>>,
    next: Mutex<HashMap<String, usize>>,
}

impl Fake {
    fn load(file: &Path) -> Result<Fake, String> {
        let text = std::fs::read_to_string(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        let all: HashMap<String, Vec<Json>> = serde_json::from_str(&text).map_err(|e| format!("{} must be {{\"tool\": [answer, ...]}}: {e}", file.display()))?;
        Ok(Fake { answers: all, next: Mutex::default() })
    }
}

impl Tools for Fake {
    fn list(&self) -> Json {
        Json::Array(self.answers.keys().map(|n| json!({ "name": n, "description": "scripted", "inputSchema": { "type": "object" } })).collect())
    }

    fn call(&self, tool: &str, _args: &Json) -> Result<Json, String> {
        let list = self.answers.get(tool).filter(|l| !l.is_empty()).ok_or(format!("the fake has no answers for {tool}"))?;
        let mut next = self.next.lock().map_err(|_| "fake lock poisoned")?;
        let i = next.entry(tool.to_string()).or_insert(0);
        let answer = list[*i % list.len()].clone();
        *i += 1;
        Ok(answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_replies_map_to_the_messages_api() {
        let args = json!({
            "system": "Be brief.",
            "messages": [
                { "role": "user", "text": "Add tea" },
                { "role": "assistant", "raw": [{ "type": "tool_use", "id": "t1", "name": "add", "input": { "sku": "tea" } }] },
                { "role": "user", "results": [{ "id": "t1", "output": "added" }] },
            ],
            "tools": [{ "name": "add", "description": "Add a product", "input": { "type": "object" } }],
        });
        let req = request("claude-opus-5-5", &args);
        assert_eq!(req["model"], "claude-opus-5-5");
        assert_eq!(req["messages"][1]["content"][0]["type"], "tool_use", "assistant turns go back unchanged");
        assert_eq!(req["messages"][2]["content"][0], json!({ "type": "tool_result", "tool_use_id": "t1", "content": "added", "is_error": false }));
        assert_eq!(req["tools"][0]["input_schema"], json!({ "type": "object" }));
        assert_eq!(req["fallbacks"], "default");
        assert!(request("claude-haiku-5-5", &args).get("fallbacks").is_none(), "models without server-side fallback do not get it");

        let r = reply(&json!({ "content": [{ "type": "thinking", "thinking": "" }, { "type": "text", "text": "Done." }, { "type": "tool_use", "id": "t2", "name": "remove", "input": {} }], "stop_reason": "tool_use" }));
        assert_eq!((r["text"].clone(), r["stop"].clone(), r["calls"][0]["name"].clone()), (json!("Done."), json!("tool_use"), json!("remove")));
        assert_eq!(r["raw"].as_array().map(Vec::len), Some(3), "raw keeps every block, thinking included");
    }
}

//! Plugins that ship with tinyjump, as MCP servers on stdio (docs/plugins.md):
//!
//! - `tj plugin ai`: the `complete` tool, one turn of an external or local LLM (raw HTTP,
//!   0 bytes added to `tj-core`):
//!   - `TJ_AI_PROVIDER=anthropic` (default): Anthropic Messages API (`ANTHROPIC_API_KEY`,
//!     `TJ_AI_MODEL` default `claude-opus-5-5`, `TJ_AI_EFFORT` default `medium`).
//!   - `TJ_AI_PROVIDER=openai`: OpenAI-compatible `/chat/completions` (OpenAI, OpenRouter,
//!     Groq, Together, vLLM, LM Studio, `llama.cpp` server). Uses `TJ_AI_BASE_URL` (default
//!     `https://api.openai.com/v1`), `TJ_AI_API_KEY` or `OPENAI_API_KEY` (optional on
//!     loopback), and `TJ_AI_MODEL`.
//!   - `TJ_AI_PROVIDER=ollama`: local Ollama shortcut (`http://127.0.0.1:11434/v1`,
//!     `TJ_AI_MODEL` default `llama3.2`, no API key required).
//!   - `TJ_AI_PROVIDER=gemini`: Google Gemini OpenAI-compatible endpoint (`GEMINI_API_KEY`
//!     or `TJ_AI_API_KEY`, `TJ_AI_MODEL` default `gemini-2.5-flash`).
//! - `tj plugin fake <file.json>`: scripted answers, `{"tool": [answer, ...]}`, given in
//!   order and then repeated from the start, for tests and demos without a key.

use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

const ANTHROPIC_API: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_MODEL: &str = "claude-opus-5-5";
const OPENAI_BASE: &str = "https://api.openai.com/v1";
const OPENAI_MODEL: &str = "gpt-4o-mini";
const OLLAMA_BASE: &str = "http://127.0.0.1:11434/v1";
const OLLAMA_MODEL: &str = "llama3.2";
const GEMINI_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/openai";
const GEMINI_MODEL: &str = "gemini-2.5-flash";
const MAX_TOKENS: u32 = 16000;
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provider {
    Anthropic,
    OpenAiCompat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AiConfig {
    provider: Provider,
    endpoint: String,
    model: String,
    api_key: Option<String>,
    effort: String,
    max_tokens: Option<u32>,
}

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
        let cfg = resolve_config(|k| std::env::var(k).ok())?;
        complete_with(&cfg, args)
    }
}

fn non_empty(env: &impl Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    env(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn resolve_config(env: impl Fn(&str) -> Option<String>) -> Result<AiConfig, String> {
    let base_url = non_empty(&env, "TJ_AI_BASE_URL");
    let raw_provider = match non_empty(&env, "TJ_AI_PROVIDER") {
        Some(p) => p.to_ascii_lowercase(),
        None if base_url.is_some() => "openai".into(),
        None if non_empty(&env, "ANTHROPIC_API_KEY").is_some() => "anthropic".into(),
        None if non_empty(&env, "OPENAI_API_KEY").is_some() => "openai".into(),
        None if non_empty(&env, "GEMINI_API_KEY").is_some() => "gemini".into(),
        None => "anthropic".into(),
    };
    let effort = non_empty(&env, "TJ_AI_EFFORT").unwrap_or_else(|| "medium".into());
    let max_tokens = match non_empty(&env, "TJ_AI_MAX_TOKENS") {
        Some(raw) => {
            let n: u32 = raw.parse().map_err(|_| format!("TJ_AI_MAX_TOKENS must be a positive integer, got {raw:?}"))?;
            if n == 0 {
                return Err("TJ_AI_MAX_TOKENS must be greater than 0".into());
            }
            Some(n)
        }
        None => None,
    };

    match raw_provider.as_str() {
        "anthropic" => {
            let endpoint = match base_url {
                Some(b) => anthropic_messages_url(&b),
                None => ANTHROPIC_API.to_string(),
            };
            let is_loopback = validate_endpoint_url(&endpoint)?;
            let api_key = non_empty(&env, "ANTHROPIC_API_KEY").or_else(|| non_empty(&env, "TJ_AI_API_KEY"));
            if api_key.is_none() && !is_loopback {
                return Err("ANTHROPIC_API_KEY is not set for the ai plugin; pass it in plugins.json \"env\" (or set TJ_AI_PROVIDER=ollama for a local LLM, or openai/gemini with TJ_AI_API_KEY)".into());
            }
            let model = non_empty(&env, "TJ_AI_MODEL").unwrap_or_else(|| ANTHROPIC_MODEL.to_string());
            Ok(AiConfig { provider: Provider::Anthropic, endpoint, model, api_key, effort, max_tokens })
        }
        "openai" | "openai-compatible" | "local" => {
            let base = base_url.unwrap_or_else(|| OPENAI_BASE.to_string());
            let endpoint = chat_completions_url(&base);
            let is_loopback = validate_endpoint_url(&endpoint)?;
            let api_key = non_empty(&env, "TJ_AI_API_KEY").or_else(|| non_empty(&env, "OPENAI_API_KEY"));
            if api_key.is_none() && !is_loopback {
                return Err("OPENAI_API_KEY (or TJ_AI_API_KEY) is not set for the ai plugin; pass it in plugins.json \"env\" (local loopback servers like http://127.0.0.1:11434/v1 do not need a key)".into());
            }
            let model = non_empty(&env, "TJ_AI_MODEL").unwrap_or_else(|| OPENAI_MODEL.to_string());
            Ok(AiConfig { provider: Provider::OpenAiCompat, endpoint, model, api_key, effort, max_tokens })
        }
        "ollama" => {
            let base = base_url.unwrap_or_else(|| OLLAMA_BASE.to_string());
            let endpoint = chat_completions_url(&base);
            let _ = validate_endpoint_url(&endpoint)?;
            let api_key = non_empty(&env, "TJ_AI_API_KEY").or_else(|| non_empty(&env, "OLLAMA_API_KEY"));
            let model = non_empty(&env, "TJ_AI_MODEL").unwrap_or_else(|| OLLAMA_MODEL.to_string());
            Ok(AiConfig { provider: Provider::OpenAiCompat, endpoint, model, api_key, effort, max_tokens })
        }
        "gemini" => {
            let base = base_url.unwrap_or_else(|| GEMINI_BASE.to_string());
            let endpoint = chat_completions_url(&base);
            let is_loopback = validate_endpoint_url(&endpoint)?;
            let api_key = non_empty(&env, "TJ_AI_API_KEY").or_else(|| non_empty(&env, "GEMINI_API_KEY"));
            if api_key.is_none() && !is_loopback {
                return Err("GEMINI_API_KEY (or TJ_AI_API_KEY) is not set for the ai plugin; pass it in plugins.json \"env\"".into());
            }
            let model = non_empty(&env, "TJ_AI_MODEL").unwrap_or_else(|| GEMINI_MODEL.to_string());
            Ok(AiConfig { provider: Provider::OpenAiCompat, endpoint, model, api_key, effort, max_tokens })
        }
        other => Err(format!("unknown TJ_AI_PROVIDER {other:?}; supported: anthropic, openai, ollama, gemini")),
    }
}

fn anthropic_messages_url(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/messages") {
        trimmed.to_string()
    } else if trimmed.ends_with("/v1") {
        format!("{trimmed}/messages")
    } else {
        format!("{trimmed}/v1/messages")
    }
}

fn chat_completions_url(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/chat/completions")
    }
}

/// Validates an LLM endpoint URL and returns `true` if its host is loopback.
/// Plain `http://` is allowed only for loopback hosts (`localhost`, `127.0.0.1`, `[::1]`)
/// so local Ollama / `llama.cpp` / LM Studio servers work while remote endpoints always use TLS.
fn validate_endpoint_url(url: &str) -> Result<bool, String> {
    if url.is_empty() || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("invalid TJ_AI_BASE_URL {url:?}"));
    }
    let (is_http, rest) = if let Some(r) = url.strip_prefix("https://") {
        (false, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (true, r)
    } else {
        return Err(format!("TJ_AI_BASE_URL must start with https:// (or http:// for localhost), got {url:?}"));
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    if host.is_empty() {
        return Err(format!("TJ_AI_BASE_URL is missing a host: {url:?}"));
    }
    let loopback = matches!(host.to_ascii_lowercase().as_str(), "localhost" | "127.0.0.1" | "::1");
    if is_http && !loopback {
        return Err(format!("plain http:// is only allowed for loopback local LLMs (localhost, 127.0.0.1, [::1]); use https:// for {host}"));
    }
    Ok(loopback)
}

fn read_capped(resp: ureq::Response) -> Result<String, String> {
    let mut buf = Vec::new();
    resp.into_reader()
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("failed reading model response: {e}"))?;
    if buf.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(format!("model response exceeded {MAX_RESPONSE_BYTES} byte cap"));
    }
    String::from_utf8(buf).map_err(|e| format!("model response was not valid UTF-8: {e}"))
}

fn complete_with(cfg: &AiConfig, args: &Json) -> Result<Json, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(300))
        .redirects(0)
        .build();
    let payload = match cfg.provider {
        Provider::Anthropic => anthropic_request(&cfg.model, &cfg.effort, cfg.max_tokens.unwrap_or(MAX_TOKENS), args),
        Provider::OpenAiCompat => openai_request(&cfg.model, cfg.max_tokens, args),
    };
    let mut req = agent.post(&cfg.endpoint).set("content-type", "application/json");
    match cfg.provider {
        Provider::Anthropic => {
            if let Some(key) = &cfg.api_key {
                req = req.set("x-api-key", key);
            }
            req = req
                .set("anthropic-version", "2026-10-21")
                .set("anthropic-beta", "server-side-fallback-2026-07-01");
        }
        Provider::OpenAiCompat => {
            if let Some(key) = &cfg.api_key {
                req = req.set("authorization", &format!("Bearer {key}"));
            }
        }
    }
    let resp = req.send_string(&payload.to_string());
    let body: Json = match resp {
        Ok(r) => {
            let text = read_capped(r)?;
            serde_json::from_str(&text).map_err(|e| format!("unreadable answer from the model: {e}"))?
        }
        Err(ureq::Error::Status(status, r)) => {
            let text = read_capped(r).unwrap_or_default();
            let detail: Json = serde_json::from_str(&text).unwrap_or_default();
            let msg = detail["error"]["message"]
                .as_str()
                .or_else(|| detail["error"].as_str())
                .or_else(|| detail["message"].as_str())
                .unwrap_or("no details");
            return Err(format!("the model API answered {status}: {msg}"));
        }
        Err(e) => return Err(format!("cannot reach the model API ({}): {e}", cfg.endpoint)),
    };
    match cfg.provider {
        Provider::Anthropic => Ok(anthropic_reply(&body)),
        Provider::OpenAiCompat => openai_reply(&body),
    }
}

/// The Anthropic Messages API request for one `complete` call.
fn anthropic_request(model: &str, effort: &str, max_tokens: u32, args: &Json) -> Json {
    let messages: Vec<Json> = args["messages"].as_array().into_iter().flatten().map(anthropic_message).collect();
    let tools: Vec<Json> = args["tools"].as_array().into_iter().flatten().map(|t| json!({ "name": t["name"], "description": t["description"], "input_schema": t["input"] })).collect();
    let mut req = json!({ "model": model, "max_tokens": max_tokens, "messages": messages, "output_config": { "effort": effort } });
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

/// A tinyjump message as an Anthropic API message. Assistant turns go back exactly as the
/// model sent them (`raw`), which current models require.
fn anthropic_message(m: &Json) -> Json {
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

/// The Anthropic model's answer as a tinyjump `Reply`.
fn anthropic_reply(body: &Json) -> Json {
    let blocks = body["content"].as_array().cloned().unwrap_or_default();
    let text: String = blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("");
    let calls: Vec<Json> = blocks.iter().filter(|b| b["type"] == "tool_use").map(|b| json!({ "id": b["id"], "name": b["name"], "input": b["input"] })).collect();
    json!({ "text": text, "calls": calls, "stop": body["stop_reason"].as_str().unwrap_or("end_turn"), "raw": blocks })
}

/// The OpenAI-compatible Chat Completions request (OpenAI, Ollama, llama.cpp, LM Studio,
/// vLLM, Gemini OpenAI-compat, OpenRouter, Groq, Together).
fn openai_request(model: &str, max_tokens: Option<u32>, args: &Json) -> Json {
    let mut messages = Vec::new();
    if let Some(system) = args["system"].as_str().filter(|s| !s.is_empty()) {
        messages.push(json!({ "role": "system", "content": system }));
    }
    for m in args["messages"].as_array().into_iter().flatten() {
        if m["role"] == "assistant" {
            messages.push(openai_assistant_message(m));
            continue;
        }
        for r in m["results"].as_array().into_iter().flatten() {
            let raw_out = r["output"].as_str().unwrap_or("");
            let content = if r["error"].as_bool().unwrap_or(false) && !raw_out.starts_with("Error:") {
                format!("Error: {raw_out}")
            } else {
                raw_out.to_string()
            };
            messages.push(json!({
                "role": "tool",
                "tool_call_id": r["id"].as_str().unwrap_or(""),
                "content": content,
            }));
        }
        if let Some(text) = m["text"].as_str().filter(|t| !t.is_empty()) {
            messages.push(json!({ "role": "user", "content": text }));
        }
    }
    let tools: Vec<Json> = args["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t["name"],
                    "description": t["description"],
                    "parameters": t["input"],
                }
            })
        })
        .collect();
    let mut req = json!({ "model": model, "messages": messages });
    if let Some(max) = max_tokens {
        req["max_tokens"] = json!(max);
    }
    if !tools.is_empty() {
        req["tools"] = json!(tools);
    }
    req
}

fn openai_assistant_message(m: &Json) -> Json {
    if let Some(obj) = m["raw"].as_object() {
        if obj.get("role").and_then(Json::as_str) == Some("assistant") {
            return m["raw"].clone();
        }
    }
    // Fallback if replaying a conversation whose prior turn came from another provider format.
    if let Some(blocks) = m["raw"].as_array() {
        let text: String = blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("");
        let tool_calls: Vec<Json> = blocks
            .iter()
            .filter(|b| b["type"] == "tool_use")
            .map(|b| {
                json!({
                    "id": b["id"],
                    "type": "function",
                    "function": { "name": b["name"], "arguments": b["input"].to_string() }
                })
            })
            .collect();
        let mut out = json!({ "role": "assistant", "content": if text.is_empty() && !tool_calls.is_empty() { Json::Null } else { json!(text) } });
        if !tool_calls.is_empty() {
            out["tool_calls"] = json!(tool_calls);
        }
        return out;
    }
    json!({ "role": "assistant", "content": m["text"].as_str().unwrap_or("") })
}

/// Converts an OpenAI-compatible Chat Completions response into a tinyjump `Reply`.
fn openai_reply(body: &Json) -> Result<Json, String> {
    let choice = body["choices"]
        .as_array()
        .and_then(|c| c.first())
        .ok_or_else(|| "model API returned no choices".to_string())?;
    let msg = &choice["message"];
    let text = match &msg["content"] {
        Json::String(s) => s.clone(),
        Json::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(""),
        _ => String::new(),
    };
    let mut calls = Vec::new();
    for (idx, tc) in msg["tool_calls"].as_array().into_iter().flatten().enumerate() {
        let id = tc["id"].as_str().map(str::to_string).unwrap_or_else(|| format!("call_{idx}"));
        let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
        let input = match &tc["function"]["arguments"] {
            Json::String(s) if s.trim().is_empty() => json!({}),
            Json::String(s) => serde_json::from_str(s).unwrap_or_else(|_| json!({ "_raw": s })),
            Json::Object(_) => tc["function"]["arguments"].clone(),
            _ => json!({}),
        };
        calls.push(json!({ "id": id, "name": name, "input": input }));
    }
    let stop = match choice["finish_reason"].as_str().unwrap_or("stop") {
        "tool_calls" | "function_call" => "tool_use",
        "length" => "max_tokens",
        "content_filter" => "refusal",
        _ => "end_turn",
    };
    let mut raw = msg.clone();
    if let Some(obj) = raw.as_object_mut() {
        obj.entry("role".to_string()).or_insert_with(|| json!("assistant"));
    }
    Ok(json!({ "text": text, "calls": calls, "stop": stop, "raw": raw }))
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
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::thread;

    fn env_from<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| pairs.iter().find(|(key, _)| *key == k).map(|(_, v)| (*v).to_string())
    }

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
        let req = anthropic_request("claude-opus-5-5", "medium", MAX_TOKENS, &args);
        assert_eq!(req["model"], "claude-opus-5-5");
        assert_eq!(req["messages"][1]["content"][0]["type"], "tool_use", "assistant turns go back unchanged");
        assert_eq!(req["messages"][2]["content"][0], json!({ "type": "tool_result", "tool_use_id": "t1", "content": "added", "is_error": false }));
        assert_eq!(req["tools"][0]["input_schema"], json!({ "type": "object" }));
        assert_eq!(req["fallbacks"], "default");
        assert!(anthropic_request("claude-haiku-5-5", "medium", MAX_TOKENS, &args).get("fallbacks").is_none(), "models without server-side fallback do not get it");

        let r = anthropic_reply(&json!({ "content": [{ "type": "thinking", "thinking": "" }, { "type": "text", "text": "Done." }, { "type": "tool_use", "id": "t2", "name": "remove", "input": {} }], "stop_reason": "tool_use" }));
        assert_eq!((r["text"].clone(), r["stop"].clone(), r["calls"][0]["name"].clone()), (json!("Done."), json!("tool_use"), json!("remove")));
        assert_eq!(r["raw"].as_array().map(Vec::len), Some(3), "raw keeps every block, thinking included");
    }

    #[test]
    fn provider_resolution_and_url_security_rules() {
        let ollama = resolve_config(env_from(&[("TJ_AI_PROVIDER", "ollama")])).unwrap();
        assert_eq!(ollama.provider, Provider::OpenAiCompat);
        assert_eq!(ollama.endpoint, "http://127.0.0.1:11434/v1/chat/completions");
        assert_eq!(ollama.model, "llama3.2");
        assert_eq!(ollama.api_key, None, "local Ollama needs no API key");

        let local_llama = resolve_config(env_from(&[("TJ_AI_BASE_URL", "http://localhost:8080/v1"), ("TJ_AI_MODEL", "qwen2.5")])).unwrap();
        assert_eq!(local_llama.provider, Provider::OpenAiCompat);
        assert_eq!(local_llama.endpoint, "http://localhost:8080/v1/chat/completions");
        assert_eq!(local_llama.model, "qwen2.5");
        assert_eq!(local_llama.api_key, None);

        let gemini = resolve_config(env_from(&[("GEMINI_API_KEY", "g-test")])).unwrap();
        assert_eq!(gemini.provider, Provider::OpenAiCompat);
        assert_eq!(gemini.endpoint, "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions");
        assert_eq!(gemini.model, "gemini-2.5-flash");
        assert_eq!(gemini.api_key.as_deref(), Some("g-test"));

        let openai = resolve_config(env_from(&[("OPENAI_API_KEY", "sk-test"), ("TJ_AI_MODEL", "gpt-4.1-mini")])).unwrap();
        assert_eq!(openai.provider, Provider::OpenAiCompat);
        assert_eq!(openai.endpoint, "https://api.openai.com/v1/chat/completions");
        assert_eq!(openai.model, "gpt-4.1-mini");

        let remote_http = resolve_config(env_from(&[("TJ_AI_BASE_URL", "http://192.168.1.50:11434/v1"), ("TJ_AI_API_KEY", "secret")])).unwrap_err();
        assert!(remote_http.contains("plain http:// is only allowed for loopback"), "got: {remote_http}");

        let missing_remote_key = resolve_config(env_from(&[("TJ_AI_PROVIDER", "openai")])).unwrap_err();
        assert!(missing_remote_key.contains("OPENAI_API_KEY"), "got: {missing_remote_key}");

        let bad_provider = resolve_config(env_from(&[("TJ_AI_PROVIDER", "unknown")])).unwrap_err();
        assert!(bad_provider.contains("anthropic, openai, ollama, gemini"), "got: {bad_provider}");
    }

    #[test]
    fn openai_request_and_reply_support_tools_and_raw_replay() {
        let prev_raw = json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{ "id": "call_1", "type": "function", "function": { "name": "add", "arguments": "{\"sku\":\"tea\"}" } }]
        });
        let args = json!({
            "system": "Shop assistant.",
            "messages": [
                { "role": "user", "text": "Two teas" },
                { "role": "assistant", "raw": prev_raw },
                { "role": "user", "results": [{ "id": "call_1", "output": "out of stock", "error": true }] },
            ],
            "tools": [{ "name": "add", "description": "Add sku", "input": { "type": "object", "properties": { "sku": { "type": "string" } } } }],
        });
        let req = openai_request("llama3.2", Some(512), &args);
        assert_eq!(req["model"], "llama3.2");
        assert_eq!(req["max_tokens"], 512);
        assert_eq!(req["messages"][0], json!({ "role": "system", "content": "Shop assistant." }));
        assert_eq!(req["messages"][2], prev_raw, "OpenAI assistant turn replays verbatim");
        assert_eq!(req["messages"][3], json!({ "role": "tool", "tool_call_id": "call_1", "content": "Error: out of stock" }));
        assert_eq!(req["tools"][0]["type"], "function");
        assert_eq!(req["tools"][0]["function"]["name"], "add");

        // Handles both stringified JSON arguments (OpenAI) and object arguments (some local runners).
        let rep = openai_reply(&json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": "Adding tea now.",
                    "tool_calls": [
                        { "id": "c1", "type": "function", "function": { "name": "add", "arguments": "{\"sku\":\"tea\"}" } },
                        { "id": "c2", "type": "function", "function": { "name": "add", "arguments": { "sku": "mug" } } }
                    ]
                }
            }]
        }))
        .unwrap();
        assert_eq!(rep["text"], "Adding tea now.");
        assert_eq!(rep["stop"], "tool_use");
        assert_eq!(rep["calls"][0], json!({ "id": "c1", "name": "add", "input": { "sku": "tea" } }));
        assert_eq!(rep["calls"][1], json!({ "id": "c2", "name": "add", "input": { "sku": "mug" } }));
        assert_eq!(rep["raw"]["role"], "assistant");
    }

    #[test]
    fn loopback_local_llm_end_to_end_call_without_api_key() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut req_line = String::new();
            reader.read_line(&mut req_line).unwrap();
            assert!(req_line.starts_with("POST /v1/chat/completions "), "unexpected request line: {req_line}");
            let mut content_len = 0usize;
            let mut saw_auth = false;
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                if h == "\r\n" || h.is_empty() {
                    break;
                }
                let lower = h.to_ascii_lowercase();
                if lower.starts_with("authorization:") {
                    saw_auth = true;
                }
                if let Some(v) = lower.strip_prefix("content-length:") {
                    content_len = v.trim().parse().unwrap_or(0);
                }
            }
            assert!(!saw_auth, "local loopback request without key should not send Authorization header");
            let mut body_bytes = vec![0u8; content_len];
            reader.read_exact(&mut body_bytes).unwrap();
            let req_json: Json = serde_json::from_slice(&body_bytes).unwrap();
            assert_eq!(req_json["model"], "llama3.2");

            let resp_body = json!({
                "choices": [{
                    "finish_reason": "tool_calls",
                    "message": {
                        "role": "assistant",
                        "content": "",
                        "tool_calls": [{ "id": "local_1", "type": "function", "function": { "name": "add", "arguments": "{\"sku\":\"tea\"}" } }]
                    }
                }]
            })
            .to_string();
            let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", resp_body.len(), resp_body);
            stream.write_all(resp.as_bytes()).unwrap();
        });

        let cfg = resolve_config(env_from(&[
            ("TJ_AI_PROVIDER", "ollama"),
            ("TJ_AI_BASE_URL", &format!("http://{addr}/v1")),
        ]))
        .unwrap();
        let rep = complete_with(&cfg, &json!({
            "system": "Be brief",
            "messages": [{ "role": "user", "text": "Add tea" }],
            "tools": [{ "name": "add", "description": "Add", "input": { "type": "object" } }],
        }))
        .unwrap();
        server.join().unwrap();
        assert_eq!(rep["stop"], "tool_use");
        assert_eq!(rep["calls"][0]["name"], "add");
        assert_eq!(rep["calls"][0]["input"]["sku"], "tea");
    }
}

//! Server plugins (docs/plugins.md): each is a separate process speaking MCP over stdio. A
//! plugin call from the app (`plugin(name, method, args)`) becomes `tools/call` with
//! `name = method`. Plugins are isolated: a crash restarts the process (at most once a
//! second), a slow answer times out, and a plugin sees only the environment variables
//! `plugins.json` passes to it.
//!
//!   plugins.json, next to the app:
//!   { "ai": { "command": ["tj", "plugin", "ai"], "env": ["ANTHROPIC_API_KEY"] } }

use serde_json::{json, Value as Json};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const CALL_TIMEOUT: Duration = Duration::from_secs(60);
const RESTART_AFTER: Duration = Duration::from_secs(1);
const BUDGET_WINDOW: Duration = Duration::from_secs(3600);
pub const AI_BUDGET_ENV: &str = "TJ_AI_MAX_CALLS_HOUR";
pub const PLUGIN_BUDGET_ENV: &str = "TJ_PLUGIN_MAX_CALLS_HOUR";
/// Variables every plugin gets, so it can start at all; nothing else leaks from the server.
const BASE_ENV: &[&str] = &["PATH", "SYSTEMROOT", "SystemRoot", "TEMP", "TMP", "HOME"];

#[derive(Clone)]
struct Config {
    command: Vec<String>,
    env: Vec<String>,
    max_calls_per_hour: Option<usize>,
}

type Waiting = Arc<Mutex<HashMap<u64, Sender<Json>>>>;

/// A running plugin process.
struct Process {
    child: Child,
    stdin: ChildStdin,
    waiting: Waiting,
    next_id: u64,
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

struct Slot {
    config: Config,
    process: Option<Process>,
    started: Option<Instant>,
    calls: VecDeque<Instant>,
}

#[derive(Default)]
pub struct Plugins {
    slots: HashMap<String, Mutex<Slot>>,
}

/// The plugins of an app: `plugins.json` next to its entry file, with `--plugin` overrides
/// (`name=fake:file.json` runs the scripted fake instead).
pub fn load(entry: &Path, overrides: &[String]) -> Result<Plugins, String> {
    let dir = entry.parent().unwrap_or(Path::new(""));
    let file = dir.join("plugins.json");
    let mut configs: HashMap<String, Config> = HashMap::new();
    if let Ok(text) = std::fs::read_to_string(&file) {
        let all: Json = serde_json::from_str(&text).map_err(|e| format!("{} is not JSON: {e}", file.display()))?;
        for (name, c) in all.as_object().ok_or(format!("{} must be an object of plugins", file.display()))? {
            let strings = |k: &str| c[k].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect::<Vec<_>>()).unwrap_or_default();
            let command = strings("command");
            if command.is_empty() {
                return Err(format!("plugin {name} in {} needs a \"command\"", file.display()));
            }
            let max_calls_per_hour = match c.get("maxCallsPerHour") {
                None | Some(Json::Null) => default_budget(name),
                Some(v) => {
                    let n = v.as_u64().ok_or_else(|| format!("plugin {name} \"maxCallsPerHour\" must be a non-negative integer"))? as usize;
                    (n > 0).then_some(n)
                }
            };
            configs.insert(name.clone(), Config { command, env: strings("env"), max_calls_per_hour });
        }
    }
    for o in overrides {
        let (name, spec) = o.split_once('=').ok_or(format!("--plugin takes name=fake:file.json, got {o}"))?;
        let fake = spec.strip_prefix("fake:").ok_or(format!("--plugin {name}= supports fake:<file.json>"))?;
        let path: PathBuf = fake.into();
        let max_calls_per_hour = configs.get(name).and_then(|c| c.max_calls_per_hour).or_else(|| default_budget(name));
        configs.insert(name.to_string(), Config { command: vec!["tj".into(), "plugin".into(), "fake".into(), path.display().to_string()], env: Vec::new(), max_calls_per_hour });
    }
    let slots = configs.into_iter().map(|(n, config)| (n, Mutex::new(Slot { config, process: None, started: None, calls: VecDeque::new() }))).collect();
    Ok(Plugins { slots })
}

fn default_budget(name: &str) -> Option<usize> {
    let specific = (name == "ai").then(|| std::env::var(AI_BUDGET_ENV).ok()).flatten();
    let fallback = std::env::var(PLUGIN_BUDGET_ENV).ok();
    specific.or(fallback).and_then(|v| v.parse::<usize>().ok()).filter(|&n| n > 0)
}

impl Plugins {
    pub fn names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.slots.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }

    /// Starts every plugin and checks it answers, so a broken one fails at startup.
    pub fn start_all(&self) -> Result<(), String> {
        for (name, slot) in &self.slots {
            let mut s = slot.lock().map_err(|_| "plugin lock poisoned")?;
            s.ensure(name).map_err(|e| format!("plugin {name} did not start: {e}"))?;
        }
        Ok(())
    }

    /// Calls `method` of plugin `name`; its result, or an error message for the app.
    pub fn call(&self, name: &str, method: &str, args: Json) -> Result<Json, String> {
        let slot = self.slots.get(name).ok_or_else(|| format!("no plugin \"{name}\" is configured; add it to plugins.json next to the app"))?;
        let (rx, id, waiting) = {
            let mut s = slot.lock().map_err(|_| "plugin lock poisoned")?;
            s.check_budget(name, Instant::now())?;
            s.ensure(name)?;
            let p = s.process.as_mut().expect("ensured");
            p.next_id += 1;
            let id = p.next_id;
            let (tx, rx) = channel();
            p.waiting.lock().map_err(|_| "plugin lock poisoned")?.insert(id, tx);
            let msg = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": method, "arguments": args } });
            if writeln!(p.stdin, "{msg}").and_then(|()| p.stdin.flush()).is_err() {
                s.process = None; // it died; the next call restarts it
                return Err(format!("plugin {name} stopped; it will be restarted"));
            }
            (rx, id, p.waiting.clone())
        };
        let answer = rx.recv_timeout(CALL_TIMEOUT);
        if let Ok(mut w) = waiting.lock() {
            w.remove(&id);
        }
        match answer {
            Ok(reply) => result(name, reply),
            Err(_) => Err(format!("plugin {name} did not answer {method} within {}s", CALL_TIMEOUT.as_secs())),
        }
    }
}

/// An MCP `tools/call` answer as an app value: `structuredContent`, else the text (as JSON
/// when it parses); `isError` and JSON-RPC errors become errors.
fn result(name: &str, reply: Json) -> Result<Json, String> {
    if let Some(e) = reply.get("error") {
        return Err(format!("plugin {name}: {}", e["message"].as_str().unwrap_or("error")));
    }
    let r = &reply["result"];
    let text = r["content"].as_array().map(|c| c.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("")).unwrap_or_default();
    if r["isError"].as_bool() == Some(true) {
        return Err(if text.is_empty() { format!("plugin {name} failed") } else { text });
    }
    if let Some(s) = r.get("structuredContent") {
        return Ok(s.clone());
    }
    Ok(serde_json::from_str(&text).unwrap_or(Json::String(text)))
}

impl Slot {
    fn check_budget(&mut self, name: &str, now: Instant) -> Result<(), String> {
        let Some(limit) = self.config.max_calls_per_hour.filter(|&l| l > 0) else {
            return Ok(());
        };
        while self.calls.front().is_some_and(|&t| now.saturating_duration_since(t) >= BUDGET_WINDOW) {
            self.calls.pop_front();
        }
        if self.calls.len() >= limit {
            return Err(format!("plugin {name} reached its hourly call budget ({limit}/hour); try again later or raise maxCallsPerHour"));
        }
        self.calls.push_back(now);
        Ok(())
    }

    /// Starts the process if it is not running (not more than once a second).
    fn ensure(&mut self, name: &str) -> Result<(), String> {
        if let Some(p) = &mut self.process {
            if matches!(p.child.try_wait(), Ok(None)) {
                return Ok(());
            }
            self.process = None;
        }
        if self.started.is_some_and(|t| t.elapsed() < RESTART_AFTER) {
            return Err(format!("plugin {name} is restarting; try again shortly"));
        }
        self.started = Some(Instant::now());
        let p = spawn(&self.config)?;
        self.process = Some(p);
        let p = self.process.as_mut().expect("just set");
        // The MCP handshake; tools/list proves the plugin works.
        let init = json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "tinyjump", "version": env!("CARGO_PKG_VERSION") } } });
        let (tx, rx) = channel();
        p.waiting.lock().map_err(|_| "plugin lock poisoned")?.insert(0, tx);
        let sent = writeln!(p.stdin, "{init}").and_then(|()| writeln!(p.stdin, "{}", json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))).and_then(|()| p.stdin.flush());
        let ok = sent.is_ok() && rx.recv_timeout(Duration::from_secs(10)).is_ok_and(|r| r.get("result").is_some());
        if !ok {
            self.process = None;
            return Err(format!("plugin {name} did not complete the MCP handshake"));
        }
        Ok(())
    }
}

fn spawn(config: &Config) -> Result<Process, String> {
    // "tj" means this very binary, so plugins that ship with tinyjump need no PATH setup.
    let program = match config.command[0].as_str() {
        "tj" => std::env::current_exe().map_err(|e| e.to_string())?,
        other => PathBuf::from(other),
    };
    let mut cmd = Command::new(&program);
    cmd.args(&config.command[1..]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).env_clear();
    for var in BASE_ENV.iter().map(|v| v.to_string()).chain(config.env.iter().cloned()) {
        if let Ok(v) = std::env::var(&var) {
            cmd.env(&var, v);
        }
    }
    let mut child = cmd.spawn().map_err(|e| format!("cannot run {}: {e}", program.display()))?;
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let waiting: Waiting = Arc::default();
    let w = waiting.clone();
    thread::Builder::new()
        .name("tj-plugin".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Ok(msg) = serde_json::from_str::<Json>(&line) else { continue };
                let Some(id) = msg["id"].as_u64() else { continue };
                if let Some(tx) = w.lock().ok().and_then(|mut m| m.remove(&id)) {
                    let _ = tx.send(msg);
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(Process { child, stdin, waiting, next_id: 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_become_values_or_errors() {
        assert_eq!(result("ai", json!({ "result": { "structuredContent": { "a": 1 }, "content": [] } })), Ok(json!({ "a": 1 })));
        assert_eq!(result("ai", json!({ "result": { "content": [{ "type": "text", "text": "{\"b\":2}" }] } })), Ok(json!({ "b": 2 })));
        assert_eq!(result("ai", json!({ "result": { "content": [{ "type": "text", "text": "plain" }] } })), Ok(json!("plain")));
        assert_eq!(result("ai", json!({ "result": { "isError": true, "content": [{ "type": "text", "text": "no key" }] } })), Err("no key".into()));
        assert!(result("ai", json!({ "error": { "message": "bad" } })).unwrap_err().contains("bad"));
    }

    #[test]
    fn hourly_call_budget_blocks_excess_calls_and_recovers_after_window() {
        let mut slot = Slot {
            config: Config { command: vec!["tj".into()], env: Vec::new(), max_calls_per_hour: Some(2) },
            process: None,
            started: None,
            calls: VecDeque::new(),
        };
        let t0 = Instant::now();
        assert!(slot.check_budget("ai", t0).is_ok());
        assert!(slot.check_budget("ai", t0 + Duration::from_secs(10)).is_ok());
        let err = slot.check_budget("ai", t0 + Duration::from_secs(20)).unwrap_err();
        assert!(err.contains("hourly call budget (2/hour)"), "{err}");
        assert!(slot.check_budget("ai", t0 + Duration::from_secs(3601)).is_ok());

        let dir = std::env::temp_dir().join(format!("tj-plugins-budget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let entry = dir.join("app.tj");
        std::fs::write(&entry, "").unwrap();
        std::fs::write(dir.join("plugins.json"), r#"{ "ai": { "command": ["tj", "plugin", "ai"], "maxCallsPerHour": 5 } }"#).unwrap();
        let loaded = load(&entry, &[]).expect("valid plugins.json");
        assert_eq!(loaded.slots["ai"].lock().unwrap().config.max_calls_per_hour, Some(5));
        std::fs::write(dir.join("plugins.json"), r#"{ "ai": { "command": ["tj", "plugin", "ai"], "maxCallsPerHour": "five" } }"#).unwrap();
        assert!(load(&entry, &[]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! `@server` object sync: HTTP method calls with `Idempotency-Key` + SSE change stream.

use serde_json::{json, Value as Json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub type WakeFn = Arc<dyn Fn(Json) + Send + Sync>;

pub struct SyncClient {
    server: Option<String>,
    token: Arc<Mutex<Option<String>>>,
    user: Option<String>,
    roles: Vec<String>,
    replicas: BTreeMap<String, u64>,
    cursor: u64,
    dirty: bool,
    gen: Arc<AtomicU64>,
    wake: WakeFn,
}

impl SyncClient {
    pub fn new(server: Option<String>, wake: WakeFn) -> Self {
        let server = server.map(|s| s.trim_end_matches('/').to_string()).filter(|s| !s.is_empty());
        Self {
            server,
            token: Arc::new(Mutex::new(None)),
            user: None,
            roles: Vec::new(),
            replicas: BTreeMap::new(),
            cursor: 0,
            dirty: false,
            gen: Arc::new(AtomicU64::new(0)),
            wake,
        }
    }

    pub fn set_token(&mut self, new_token: Option<String>, new_user: Option<String>, new_roles: Vec<String>) -> bool {
        if let Ok(mut t) = self.token.lock() {
            *t = new_token;
        }
        let changed = self.user != new_user || self.roles != new_roles;
        self.user = new_user;
        self.roles = new_roles;
        if changed {
            self.replicas.clear();
            self.cursor = 0;
            self.dirty = false;
            self.gen.fetch_add(1, Ordering::SeqCst);
        } else if !self.replicas.is_empty() {
            self.connect();
        }
        changed
    }

    pub fn subscribe(&mut self, key: String, seq: u64) {
        if self.server.is_none() || self.replicas.contains_key(&key) {
            return;
        }
        if self.replicas.keys().any(|pat| covers(pat, &key)) {
            return;
        }
        if key.ends_with('*') {
            self.replicas.retain(|k, _| !covers(&key, k));
        }
        self.replicas.insert(key, seq);
        self.cursor = self.replicas.values().copied().min().unwrap_or(0);
        self.dirty = true;
    }

    pub fn flush_subscriptions(&mut self) {
        if self.dirty {
            self.dirty = false;
            self.connect();
        }
    }

    pub fn record_seq(&mut self, key: &str, seq: u64) {
        self.cursor = self.cursor.max(seq);
        for (entry, have) in &mut self.replicas {
            if entry == key || covers(entry, key) {
                *have = (*have).max(seq);
            }
        }
    }

    pub fn stop(&self) {
        self.gen.fetch_add(1, Ordering::SeqCst);
    }

    pub fn call(&self, id: i64, effect: Json) {
        let Some(base) = self.server.clone() else {
            let object = effect["object"].as_str().unwrap_or("");
            (self.wake)(json!({ "type": "effectDone", "id": id, "error": format!("no server configured for {object}") }));
            return;
        };
        let token = self.token.lock().ok().and_then(|g| g.clone());
        let wake = self.wake.clone();
        thread::spawn(move || {
            let object = effect["object"].as_str().unwrap_or("");
            let method = effect["method"].as_str().unwrap_or("");
            let path: String = object.split('/').map(percent_encode).collect::<Vec<_>>().join("/");
            let url = format!("{base}/api/{path}/{}", percent_encode(method));
            let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(30)).build();
            let mut req = agent.post(&url).set("Content-Type", "application/json");
            if let Some(call_id) = effect["call"].as_str() {
                req = req.set("Idempotency-Key", call_id);
            }
            if let Some(ref t) = token {
                req = req.set("Authorization", &format!("Bearer {t}"));
            }
            let args = effect.get("args").cloned().unwrap_or_else(|| json!([]));
            let done = match req.send_string(&args.to_string()) {
                Ok(resp) => match resp.into_string().map_err(|e| e.to_string()).and_then(|s| serde_json::from_str::<Json>(&s).map_err(|e| e.to_string())) {
                    Ok(body) => {
                        if let Some(err) = body["error"].as_str() {
                            let mut m = json!({ "type": "effectDone", "id": id, "error": err });
                            if let Some(f) = body["field"].as_str() {
                                m["field"] = json!(f);
                            }
                            m
                        } else {
                            json!({ "type": "effectDone", "id": id, "ok": body.get("ok").cloned().unwrap_or(Json::Null) })
                        }
                    }
                    Err(e) => json!({ "type": "effectDone", "id": id, "error": format!("bad reply: {e}"), "unreachable": true }),
                },
                Err(ureq::Error::Status(status, resp)) => {
                    let retryable = status >= 502 || status == 401 || status == 429;
                    if !retryable {
                        if let Ok(text) = resp.into_string() {
                            if let Ok(body) = serde_json::from_str::<Json>(&text) {
                                if let Some(err) = body["error"].as_str() {
                                    let mut m = json!({ "type": "effectDone", "id": id, "error": err });
                                    if let Some(f) = body["field"].as_str() {
                                        m["field"] = json!(f);
                                    }
                                    (wake)(m);
                                    return;
                                }
                            }
                        }
                    }
                    json!({ "type": "effectDone", "id": id, "error": format!("HTTP {status}"), "unreachable": true })
                }
                Err(e) => json!({ "type": "effectDone", "id": id, "error": format!("{e}"), "unreachable": true }),
            };
            (wake)(done);
        });
    }

    pub fn http_effect(&self, id: i64, effect: Json) {
        let wake = self.wake.clone();
        thread::spawn(move || {
            let url = effect["url"].as_str().unwrap_or("");
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                (wake)(json!({ "type": "effectDone", "id": id, "error": "only http(s) URLs can be fetched" }));
                return;
            }
            let method = effect["method"].as_str().unwrap_or("GET");
            let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(30)).build();
            let mut req = agent.request(method, url);
            if let Some(headers) = effect["headers"].as_object() {
                for (k, v) in headers {
                    if let Some(s) = v.as_str() {
                        req = req.set(k, s);
                    }
                }
            }
            let resp = if let Some(body) = effect["body"].as_str() {
                req.send_string(body)
            } else {
                req.call()
            };
            let done = match resp {
                Ok(r) => finish_http(id, r),
                Err(ureq::Error::Status(_, r)) => finish_http(id, r),
                Err(e) => json!({ "type": "effectDone", "id": id, "error": format!("{e}") }),
            };
            (wake)(done);
        });
    }

    fn connect(&self) {
        let Some(base) = self.server.clone() else { return };
        let my_gen = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        let gen = self.gen.clone();
        let token = self.token.clone();
        let wake = self.wake.clone();
        let objects: Vec<String> = self.replicas.keys().cloned().collect();
        let mut cursor = self.cursor;
        if objects.is_empty() {
            return;
        }
        thread::spawn(move || {
            let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(10)).timeout_read(Duration::from_secs(60)).build();
            while gen.load(Ordering::SeqCst) == my_gen {
                let query = objects.iter().map(|o| format!("object={}", percent_encode(o))).collect::<Vec<_>>().join("&");
                let url = format!("{base}/api/events?{query}&since={cursor}");
                let mut req = agent.get(&url);
                if let Ok(Some(ref t)) = token.lock().map(|g| g.clone()) {
                    req = req.set("Authorization", &format!("Bearer {t}"));
                }
                match req.call() {
                    Ok(resp) if resp.status() == 200 => {
                        (wake)(json!({ "type": "online" }));
                        let mut reader = BufReader::new(resp.into_reader());
                        let mut event = String::new();
                        let mut data = String::new();
                        let mut line = String::new();
                        while gen.load(Ordering::SeqCst) == my_gen {
                            line.clear();
                            match reader.read_line(&mut line) {
                                Ok(0) | Err(_) => break,
                                Ok(_) => {
                                    let trimmed = line.trim_end_matches(['\r', '\n']);
                                    if trimmed.is_empty() {
                                        if event == "changed" && !data.is_empty() {
                                            if let Ok(d) = serde_json::from_str::<Json>(&data) {
                                                if let (Some(key), Some(seq)) = (d["object"].as_str(), d["seq"].as_u64()) {
                                                    cursor = cursor.max(seq);
                                                    (wake)(json!({
                                                        "type": "sync",
                                                        "object": key,
                                                        "state": d.get("state").cloned().unwrap_or(Json::Null),
                                                        "seq": seq
                                                    }));
                                                }
                                            }
                                        }
                                        event.clear();
                                        data.clear();
                                    } else if let Some(v) = trimmed.strip_prefix("event:") {
                                        event = v.strip_prefix(' ').unwrap_or(v).to_string();
                                    } else if let Some(v) = trimmed.strip_prefix("data:") {
                                        if !data.is_empty() {
                                            data.push('\n');
                                        }
                                        data.push_str(v.strip_prefix(' ').unwrap_or(v));
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
                if gen.load(Ordering::SeqCst) != my_gen {
                    break;
                }
                thread::sleep(Duration::from_secs(2));
            }
        });
    }
}

fn finish_http(id: i64, r: ureq::Response) -> Json {
    let status = r.status();
    let mut headers = serde_json::Map::new();
    for name in r.headers_names() {
        if let Some(val) = r.header(&name) {
            headers.insert(name.to_ascii_lowercase(), json!(val));
        }
    }
    let body = r.into_string().unwrap_or_default();
    json!({ "type": "effectDone", "id": id, "ok": { "status": status, "headers": headers, "body": body } })
}

pub fn sign_in_async(server: String, key: String, done: impl FnOnce(String, Vec<String>) + Send + 'static) {
    thread::spawn(move || {
        let url = format!("{}/api/whoami", server.trim_end_matches('/'));
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(10)).build();
        if let Ok(resp) = agent.get(&url).set("Authorization", &format!("Bearer {key}")).call() {
            if let Ok(text) = resp.into_string() {
                if let Ok(body) = serde_json::from_str::<Json>(&text) {
                    if let Some(id) = body["ok"]["id"].as_str() {
                        let roles = body["ok"]["roles"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                            .unwrap_or_default();
                        done(id.to_string(), roles);
                        return;
                    }
                }
            }
        }
        eprintln!("tinyjump-linux: sign-in failed");
    });
}

fn covers(pattern: &str, key: &str) -> bool {
    pattern
        .strip_suffix('*')
        .is_some_and(|pref| pref.contains('/') && key.starts_with(pref))
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

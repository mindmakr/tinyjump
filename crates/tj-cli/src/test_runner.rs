//! `tj test`: deterministic, in-memory trace runner over `tj-core` (zero ports, zero browser,
//! controllable clock, scripted fake plugins and HTTP mocks, Taffy flexbox layout + review,
//! and restart replay over the same in-memory store).
//!
//! Each `.trace.jsonl` file contains one JSON object per line (blank lines and `#`/`//`
//! comment lines are ignored). Every step has a `"do"` field:
//!
//!   boot          { "do": "boot", "now"?, "seed"?, "server"?, "render"?, "restart"? }
//!   as            { "do": "as", "id"?, "roles"?, "trusted"?, "caller"? }
//!   plugin        { "do": "plugin", "name", "answers" | "file" }
//!   http          { "do": "http", "match", "status"?, "body"?, "headers"?, "error"? }
//!   call          { "do": "call", "object", "method", "args"?, "as"?, "idempotencyKey"?,
//!                   "expect"?, "expectContains"?, "expectError"?, "expectField"?, "expectStatus"? }
//!   get           { "do": "get", "object", "as"?, "expect"?, "expectContains"?,
//!                   "expectError"?, "expectStatus"? }
//!   ui            { "do": "ui", "target", "event", "value"?, "expectError"? }
//!   tick          { "do": "tick", "advanceMs" | "advance" }
//!   privacy       { "do": "privacy", "action": "export"|"erase", "subject",
//!                   "expect"?, "expectContains"?, "expectError"? }
//!   assertScreen  { "do": "assertScreen", "contains"?, "notContains"? }
//!   assertLayout  { "do": "assertLayout", "width"?, "height"?, "rtl"?, "review"?, "maxFindings"? }
//!   assertMetrics { "do": "assertMetrics", "maxVmSteps"?, "maxRenders"?, "maxPatchOps"?,
//!                   "maxNodes"?, "maxObjects"? }
//!   assertInspect { "do": "assertInspect", "object", "expect"?, "expectContains"? }

use crate::review::{self, Severity};
use serde_json::{json, Map, Value as Json};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use tj_core::runtime::protocol::{Caller, Effect};
use tj_core::ui::layout::{Frame, Measure};
use tj_core::vm::program::Program;
use tj_core::{compile, Input, MemStore, Options, Output, PatchOp, Runtime, Store, Target};

const DEFAULT_START_MS: f64 = 1_800_000_000_000.0;
const DEFAULT_SEED: u64 = 1;
const DEFAULT_ENTROPY: &str = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
const MAX_EFFECT_PUMPS: usize = 256;
const MAX_TICK_STEPS: usize = 10_000;

type CallReply = Result<Json, (u16, String, Option<String>)>;

/// In-memory (or overlay) store shared across simulated restarts within one trace.
#[derive(Clone)]
struct SharedStore(Rc<RefCell<Box<dyn Store>>>);

impl Default for SharedStore {
    fn default() -> Self {
        SharedStore(Rc::new(RefCell::new(Box::new(MemStore::default()))))
    }
}

impl SharedStore {
    fn from_store(store: Box<dyn Store>) -> Self {
        SharedStore(Rc::new(RefCell::new(store)))
    }
}

impl Store for SharedStore {
    fn load(&mut self, key: &str) -> Result<Option<String>, String> {
        self.0.borrow_mut().load(key)
    }
    fn save(&mut self, key: &str, value: &str) -> Result<(), String> {
        self.0.borrow_mut().save(key, value)
    }
    fn keys(&mut self, prefix: &str) -> Result<Vec<String>, String> {
        self.0.borrow_mut().keys(prefix)
    }
    fn remove(&mut self, key: &str) -> Result<(), String> {
        self.0.borrow_mut().remove(key)
    }
    fn forget(&mut self, key: &str) -> Result<(), String> {
        self.0.borrow_mut().forget(key)
    }
    fn is_erased(&mut self, key: &str) -> Result<bool, String> {
        self.0.borrow_mut().is_erased(key)
    }
}

/// Deterministic widget size estimator for Taffy flexbox layout during `assertLayout`.
type NodeTable = Rc<RefCell<HashMap<u32, (String, Map<String, Json>)>>>;

struct TestMeasure(NodeTable);

impl Measure for TestMeasure {
    fn measure(&mut self, node: u32, max_width: Option<f32>) -> (f32, f32) {
        let map = self.0.borrow();
        let Some((kind, props)) = map.get(&node) else {
            return (0.0, 0.0);
        };
        let str_prop = |k: &str| props.get(k).and_then(Json::as_str).unwrap_or("");
        let num_prop = |k: &str| props.get(k).and_then(Json::as_f64).map(|v| v as f32);
        let font = num_prop("size").unwrap_or(15.0);
        let char_w = (font * 0.58).max(6.0);
        let line_h = (font * 1.35).ceil();
        let pad = num_prop("padding").unwrap_or(0.0);
        let px = num_prop("px").unwrap_or(pad);
        let py = num_prop("py").unwrap_or(pad);
        match kind.as_str() {
            "Text" | "Time" => {
                let text = match (kind.as_str(), str_prop("text")) {
                    ("Time", "") => "2026-11-22 14:30",
                    (_, t) => t,
                };
                let raw_w = text.chars().count() as f32 * char_w;
                let inner_max = max_width.map(|mw| (mw - px * 2.0).max(char_w));
                let (w, lines) = match inner_max {
                    Some(mw) if raw_w > mw => (mw, (raw_w / mw).ceil().max(1.0)),
                    _ => (raw_w, 1.0),
                };
                ((w + px * 2.0).ceil(), (lines * line_h + py * 2.0).ceil())
            }
            "Button" => {
                let text = str_prop("text");
                let w = (text.chars().count() as f32 * char_w + 32.0).max(44.0);
                let w = max_width.map_or(w, |mw| w.min(mw));
                let h = (line_h + 20.0).max(44.0);
                (w.ceil(), h.ceil())
            }
            "Input" => {
                let w = max_width.unwrap_or(240.0);
                (w.ceil(), 44.0)
            }
            "Toggle" => {
                let text = str_prop("text");
                let w = (text.chars().count() as f32 * char_w + 64.0).max(44.0);
                let w = max_width.map_or(w, |mw| w.min(mw));
                (w.ceil(), 44.0)
            }
            "Image" => (num_prop("width").unwrap_or(120.0).ceil(), num_prop("height").unwrap_or(120.0).ceil()),
            _ => (0.0, 0.0),
        }
    }
}

/// Scripted in-memory plugin answers (`{"tool": [answer, ...]}`), matching `tj plugin fake`.
#[derive(Clone, Default)]
pub struct FakePlugin {
    answers: HashMap<String, Vec<Json>>,
    next: HashMap<String, usize>,
}

impl FakePlugin {
    fn from_json(v: &Json) -> Result<FakePlugin, String> {
        let obj = v.as_object().ok_or("plugin answers must be a JSON object: {\"tool\": [answer, ...]}")?;
        let mut answers = HashMap::new();
        for (tool, list) in obj {
            let arr = list.as_array().ok_or_else(|| format!("plugin answers for {tool:?} must be an array"))?;
            if arr.is_empty() {
                return Err(format!("plugin answers for {tool:?} cannot be empty"));
            }
            answers.insert(tool.clone(), arr.clone());
        }
        Ok(FakePlugin { answers, next: HashMap::new() })
    }

    fn call(&mut self, method: &str) -> Result<Json, String> {
        let list = self.answers.get(method).ok_or_else(|| format!("fake plugin has no scripted answer for {method:?}"))?;
        let i = self.next.entry(method.to_string()).or_insert(0);
        let ans = list[*i % list.len()].clone();
        *i += 1;
        if let Some(err) = ans.get("error").and_then(Json::as_str).filter(|_| ans.as_object().is_some_and(|m| m.len() == 1)) {
            return Err(err.to_string());
        }
        Ok(ans)
    }
}

/// Scripted HTTP response for `fetch(...)` inside a trace.
#[derive(Clone)]
struct HttpMock {
    pattern: String,
    status: u16,
    headers: Json,
    body: String,
    error: Option<String>,
}

impl HttpMock {
    fn matches(&self, url: &str) -> bool {
        self.pattern == "*" || url == self.pattern || url.starts_with(&self.pattern)
    }
}

#[derive(Debug, Clone)]
pub struct TraceResult {
    pub name: String,
    pub steps: usize,
    pub error: Option<String>,
    pub vm_steps: u64,
    pub renders: u64,
    pub patch_ops: u64,
}

impl TraceResult {
    pub fn passed(&self) -> bool {
        self.error.is_none()
    }

    pub fn json(&self) -> Json {
        json!({
            "name": self.name,
            "passed": self.passed(),
            "steps": self.steps,
            "error": self.error,
            "metrics": { "vmSteps": self.vm_steps, "renders": self.renders, "patchOps": self.patch_ops },
        })
    }
}

/// Mutable state for one `.trace.jsonl` run.
struct Harness {
    app_dir: Option<PathBuf>,
    source: String,
    store: SharedStore,
    seeded_store: bool,
    booted_once: bool,
    nodes: NodeTable,
    frames: HashMap<u32, Frame>,
    rt: Option<Runtime>,
    now: f64,
    seed: u64,
    server: bool,
    render: bool,
    caller: Caller,
    next_reply: u64,
    plugins: HashMap<String, FakePlugin>,
    http_mocks: Vec<HttpMock>,
    last_errors: Vec<String>,
}

impl Harness {
    fn new(app_dir: PathBuf, source: String, cli_plugins: HashMap<String, FakePlugin>) -> Harness {
        Self::with_store(Some(app_dir), source, cli_plugins, SharedStore::default(), false, DEFAULT_START_MS)
    }

    fn with_store(app_dir: Option<PathBuf>, source: String, cli_plugins: HashMap<String, FakePlugin>, store: SharedStore, seeded_store: bool, initial_now: f64) -> Harness {
        Harness {
            app_dir,
            source,
            store,
            seeded_store,
            booted_once: false,
            nodes: Rc::new(RefCell::new(HashMap::new())),
            frames: HashMap::new(),
            rt: None,
            now: initial_now,
            seed: DEFAULT_SEED,
            server: true,
            render: true,
            caller: Caller::trusted(),
            next_reply: 0,
            plugins: cli_plugins,
            http_mocks: Vec::new(),
            last_errors: Vec::new(),
        }
    }

    fn ensure_booted(&mut self) -> Result<(), String> {
        if self.rt.is_none() {
            self.boot(false)?;
        }
        Ok(())
    }

    fn boot(&mut self, restart: bool) -> Result<(), String> {
        if !restart && (!self.seeded_store || self.booted_once) {
            self.store = SharedStore::default();
        }
        self.booted_once = true;
        self.nodes.borrow_mut().clear();
        self.frames.clear();
        let program: Program = compile(&self.source).map_err(|e| format!("compile failed: {}", e.iter().map(|d| d.message.as_str()).collect::<Vec<_>>().join("; ")))?;
        let opts = Options { now: self.now, seed: self.seed, render: self.render, server: self.server, explain: true, max_objects: 100_000, max_object_bytes: 0 };
        let (mut rt, mut out) = Runtime::boot(program, Box::new(self.store.clone()), opts).map_err(|e| format!("boot failed: {e}"))?;
        rt.set_measure(Box::new(TestMeasure(self.nodes.clone())));
        out.extend(rt.handle(self.now, Input::Entropy { bytes: DEFAULT_ENTROPY.into() }));
        if let Caller::Remote { id, roles } = &self.caller {
            if !id.is_empty() || !roles.is_empty() {
                out.extend(rt.handle(self.now, Input::Identity { user: (!id.is_empty()).then(|| id.clone()), roles: roles.clone() }));
            }
        }
        self.rt = Some(rt);
        let mut replies = HashMap::new();
        self.pump(out, &mut replies)?;
        if !self.last_errors.is_empty() {
            return Err(format!("boot error: {}", std::mem::take(&mut self.last_errors).join("; ")));
        }
        Ok(())
    }

    fn send_and_wait(&mut self, mut input: Input) -> Result<CallReply, String> {
        self.ensure_booted()?;
        self.next_reply += 1;
        let id = self.next_reply;
        match &mut input {
            Input::Call { reply, .. } | Input::Get { reply, .. } | Input::Ui { reply, .. } | Input::Inspect { reply } => *reply = Some(id),
            _ => {}
        }
        let out = self.rt.as_mut().expect("booted").handle(self.now, input);
        let mut replies = HashMap::new();
        self.pump(out, &mut replies)?;
        replies.remove(&id).ok_or_else(|| "runtime did not reply (unresolved async effect?)".to_string())
    }

    /// Processes runtime outputs, performing scripted plugin, HTTP and `tj:privacy` effects
    /// in-memory until no new effects are pending.
    fn pump(&mut self, mut out: Vec<Output>, replies: &mut HashMap<u64, CallReply>) -> Result<(), String> {
        let mut rounds = 0;
        while !out.is_empty() {
            rounds += 1;
            if rounds > MAX_EFFECT_PUMPS {
                return Err(format!("exceeded {MAX_EFFECT_PUMPS} effect rounds (possible infinite async loop)"));
            }
            let mut next_inputs = Vec::new();
            for o in std::mem::take(&mut out) {
                match o {
                    Output::Reply { reply, ok, error, field, status } => {
                        let r = match error {
                            Some(e) => Err((status.unwrap_or(400), e, field)),
                            None => Ok(ok.unwrap_or(Json::Null)),
                        };
                        replies.insert(reply, r);
                    }
                    Output::Patch { ops } => self.apply_patch(&ops),
                    Output::Frames { frames } => {
                        for f in frames {
                            self.frames.insert(f.id, f);
                        }
                    }
                    Output::Error { message, .. } => self.last_errors.push(message),
                    Output::Effect { id, effect } => {
                        let done = self.perform_effect(id, effect);
                        next_inputs.push(done);
                    }
                    _ => {}
                }
            }
            for input in next_inputs {
                out.extend(self.rt.as_mut().expect("booted").handle(self.now, input));
            }
        }
        Ok(())
    }

    fn perform_effect(&mut self, id: u32, effect: Effect) -> Input {
        let result = match effect {
            Effect::Plugin { plugin, method, args } if plugin == "privacy" => self.privacy_effect(id, &method, &args),
            Effect::Plugin { plugin, method, .. } => match self.plugins.get_mut(&plugin) {
                Some(fake) => fake.call(&method),
                None => Err(format!("no scripted fake for plugin {plugin:?}; add {{\"do\": \"plugin\", \"name\": \"{plugin}\", \"answers\": ...}} or pass --plugin {plugin}=fake:<file>")),
            },
            Effect::Http { url, .. } => match self.http_mocks.iter().rev().find(|m| m.matches(&url)) {
                Some(mock) => match &mock.error {
                    Some(e) => Err(e.clone()),
                    None => Ok(json!({ "status": mock.status, "headers": mock.headers, "body": mock.body })),
                },
                None => Err(format!("no scripted HTTP mock for {url:?}; add {{\"do\": \"http\", \"match\": ..., \"status\": 200, \"body\": ...}}")),
            },
            Effect::Call { .. } => Err("device-to-server call effect in single-runtime test; boot with \"server\": true".into()),
        };
        let (ok, error) = match result {
            Ok(v) => (Some(v), None),
            Err(e) => (None, Some(e)),
        };
        Input::EffectDone { id, ok, error, field: None, unreachable: false }
    }

    fn privacy_effect(&mut self, id: u32, method: &str, args: &Json) -> Result<Json, String> {
        let person = args["id"].as_str().filter(|s| !s.is_empty()).ok_or("give the id of the person")?;
        let by = args["caller"]["id"].as_str().unwrap_or_default();
        let roles: Vec<&str> = args["caller"]["roles"].as_array().map(|a| a.iter().filter_map(Json::as_str).collect()).unwrap_or_default();
        if !(by == person || roles.contains(&"privacy") || roles.contains(&"system")) {
            return Err("only the person themselves, the privacy role or the server's own code may export or erase someone's data".into());
        }
        let rt = self.rt.as_mut().expect("booted");
        match method {
            "export" => rt.export_subject(person),
            "erase" => {
                let e = rt.erase_subject(person, Some(id)).map_err(|(_, why)| why)?;
                let held: Vec<Json> = e.held.iter().map(|(object, until)| json!({ "object": object, "until": until })).collect();
                Ok(json!({ "erased": e.erased, "held": held }))
            }
            other => Err(format!("tj:privacy has no {other}")),
        }
    }

    fn apply_patch(&mut self, ops: &[PatchOp]) {
        let mut nodes = self.nodes.borrow_mut();
        for op in ops {
            match op {
                PatchOp::Create { id, kind, props, .. } => {
                    nodes.insert(*id, (kind.clone(), props.clone()));
                }
                PatchOp::Update { id, props, .. } => {
                    if let Some((_, map)) = nodes.get_mut(id) {
                        for (k, v) in props {
                            if v.is_null() {
                                map.remove(k);
                            } else {
                                map.insert(k.clone(), v.clone());
                            }
                        }
                    }
                }
                PatchOp::Remove { id } => {
                    nodes.remove(id);
                    self.frames.remove(id);
                }
                PatchOp::Children { .. } => {}
            }
        }
    }

    fn inspect_snapshot(&mut self) -> Result<Json, String> {
        match self.send_and_wait(Input::Inspect { reply: None })? {
            Ok(v) => Ok(v),
            Err((_, e, _)) => Err(format!("inspect failed: {e}")),
        }
    }

    fn step(&mut self, s: &Json) -> Result<(), String> {
        let action = s["do"].as_str().ok_or("every trace step must have a \"do\" string")?;
        self.last_errors.clear();
        let res = self.run_step(action, s);
        if res.is_ok() && !self.last_errors.is_empty() {
            return Err(format!("{action} produced app error: {}", std::mem::take(&mut self.last_errors).join("; ")));
        }
        res
    }

    fn run_step(&mut self, action: &str, s: &Json) -> Result<(), String> {
        match action {
            "boot" => {
                if let Some(now) = s["now"].as_f64() {
                    self.now = now;
                }
                if let Some(seed) = s["seed"].as_u64() {
                    self.seed = seed;
                }
                if let Some(server) = s["server"].as_bool() {
                    self.server = server;
                }
                if let Some(render) = s["render"].as_bool() {
                    self.render = render;
                }
                let restart = s["restart"].as_bool().unwrap_or(false);
                self.boot(restart)
            }
            "as" => {
                self.caller = parse_as_step(s)?;
                if self.rt.is_some() {
                    let (user, roles) = match &self.caller {
                        Caller::Remote { id, roles } => ((!id.is_empty()).then(|| id.clone()), roles.clone()),
                        Caller::Trusted(_) => (None, Vec::new()),
                    };
                    let out = self.rt.as_mut().expect("booted").handle(self.now, Input::Identity { user, roles });
                    let mut replies = HashMap::new();
                    self.pump(out, &mut replies)?;
                }
                Ok(())
            }
            "plugin" => {
                let name = s["name"].as_str().ok_or("\"plugin\" step needs \"name\"")?;
                let fake = if let Some(answers) = s.get("answers") {
                    FakePlugin::from_json(answers)?
                } else if let Some(file) = s["file"].as_str() {
                    let Some(app_dir) = &self.app_dir else {
                        return Err("smoke traces cannot read server files; use inline \"answers\" instead".into());
                    };
                    let path = app_dir.join(file);
                    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read plugin fake {}: {e}", path.display()))?;
                    let v: Json = serde_json::from_str(&text).map_err(|e| format!("{} is not JSON: {e}", path.display()))?;
                    FakePlugin::from_json(&v)?
                } else {
                    return Err("\"plugin\" step needs \"answers\" or \"file\"".into());
                };
                self.plugins.insert(name.to_string(), fake);
                Ok(())
            }
            "http" => {
                let pattern = s["match"].as_str().unwrap_or("*").to_string();
                let status = s["status"].as_u64().unwrap_or(200) as u16;
                let headers = s.get("headers").cloned().unwrap_or_else(|| json!({}));
                let body = match s.get("body") {
                    Some(Json::String(b)) => b.clone(),
                    Some(other) if !other.is_null() => other.to_string(),
                    _ => String::new(),
                };
                let error = s["error"].as_str().map(str::to_string);
                self.http_mocks.push(HttpMock { pattern, status, headers, body, error });
                Ok(())
            }
            "call" => {
                let object = s["object"].as_str().ok_or("\"call\" needs \"object\"")?.to_string();
                let method = s["method"].as_str().ok_or("\"call\" needs \"method\"")?.to_string();
                let args = s.get("args").and_then(Json::as_array).cloned().unwrap_or_default();
                let call = s["idempotencyKey"].as_str().map(str::to_string);
                let caller = match s.get("as") {
                    Some(v) => parse_caller(v)?,
                    None => self.caller.clone(),
                };
                let res = self.send_and_wait(Input::Call { object: object.clone(), method: method.clone(), args, reply: None, call, caller: Some(caller) })?;
                check_reply(&format!("call {object}.{method}"), res, s)
            }
            "get" => {
                let object = s["object"].as_str().ok_or("\"get\" needs \"object\"")?.to_string();
                let caller = match s.get("as") {
                    Some(v) => parse_caller(v)?,
                    None => self.caller.clone(),
                };
                let res = self.send_and_wait(Input::Get { object: object.clone(), reply: None, caller: Some(caller) })?;
                check_reply(&format!("get {object}"), res, s)
            }
            "ui" => {
                let target = parse_target(s.get("target").ok_or("\"ui\" needs \"target\"")?)?;
                let event = s["event"].as_str().ok_or("\"ui\" needs \"event\"")?.to_string();
                let value = s.get("value").cloned().unwrap_or(Json::Null);
                self.last_errors.clear();
                let res = self.send_and_wait(Input::Ui { target, event: event.clone(), value, reply: None })?;
                if let Some(want) = s["expectError"].as_str() {
                    match res {
                        Err((_, msg, _)) if msg.contains(want) => {
                            self.last_errors.clear();
                            return Ok(());
                        }
                        Err((_, msg, _)) => return Err(format!("ui {event}: expected error containing {want:?}, got {msg:?}")),
                        Ok(_) if self.last_errors.iter().any(|e| e.contains(want)) => {
                            self.last_errors.clear();
                            return Ok(());
                        }
                        Ok(_) => return Err(format!("ui {event}: expected error containing {want:?}, but succeeded")),
                    }
                }
                if let Err((_, msg, _)) = res {
                    return Err(format!("ui {event} failed: {msg}"));
                }
                if !self.last_errors.is_empty() {
                    return Err(format!("ui {event} produced app error: {}", std::mem::take(&mut self.last_errors).join("; ")));
                }
                Ok(())
            }
            "tick" => {
                self.ensure_booted()?;
                let delta = match (s["advanceMs"].as_f64(), s["advance"].as_str()) {
                    (Some(ms), _) => ms,
                    (None, Some(d)) => parse_duration_ms(d)?,
                    (None, None) => 0.0,
                };
                if delta < 0.0 {
                    return Err("tick advance cannot be negative".into());
                }
                let target = self.now + delta;
                self.last_errors.clear();
                let mut steps = 0;
                // Step through every timer/alarm due up to `target` in exact chronological order.
                while let Some(next) = self.rt.as_ref().expect("booted").next_wake() {
                    if next > target {
                        break;
                    }
                    steps += 1;
                    if steps > MAX_TICK_STEPS {
                        return Err(format!("tick fired more than {MAX_TICK_STEPS} timers advancing to {target}"));
                    }
                    self.now = next.max(self.now);
                    let out = self.rt.as_mut().expect("booted").handle(self.now, Input::Tick);
                    let mut replies = HashMap::new();
                    self.pump(out, &mut replies)?;
                }
                self.now = target;
                let out = self.rt.as_mut().expect("booted").handle(self.now, Input::Tick);
                let mut replies = HashMap::new();
                self.pump(out, &mut replies)?;
                if !self.last_errors.is_empty() {
                    return Err(format!("tick produced app error: {}", std::mem::take(&mut self.last_errors).join("; ")));
                }
                Ok(())
            }
            "privacy" => {
                self.ensure_booted()?;
                let op = s["action"].as_str().ok_or("\"privacy\" needs \"action\": \"export\" or \"erase\"")?;
                let subject = s["subject"].as_str().filter(|x| !x.is_empty()).ok_or("\"privacy\" needs \"subject\"")?;
                let res = match op {
                    "export" => self.rt.as_mut().expect("booted").export_subject(subject).map_err(|e| (503, e, None)),
                    "erase" => match self.rt.as_mut().expect("booted").erase_subject(subject, None) {
                        Ok(e) => {
                            let held: Vec<Json> = e.held.iter().map(|(object, until)| json!({ "object": object, "until": until })).collect();
                            let mut replies = HashMap::new();
                            self.pump(e.outputs, &mut replies)?;
                            Ok(json!({ "erased": e.erased, "held": held }))
                        }
                        Err((st, msg)) => Err((st, msg, None)),
                    },
                    other => return Err(format!("unknown privacy action {other:?}; use \"export\" or \"erase\"")),
                };
                check_reply(&format!("privacy {op} {subject}"), res, s)
            }
            "assertScreen" => {
                let snap = self.inspect_snapshot()?;
                let outline = review::outline(&snap["tree"]);
                for want in s["contains"].as_array().into_iter().flatten().filter_map(Json::as_str) {
                    if !outline.contains(want) {
                        return Err(format!("screen did not contain {want:?}\nscreen:\n{outline}"));
                    }
                }
                for forbid in s["notContains"].as_array().into_iter().flatten().filter_map(Json::as_str) {
                    if outline.contains(forbid) {
                        return Err(format!("screen unexpectedly contained {forbid:?}\nscreen:\n{outline}"));
                    }
                }
                Ok(())
            }
            "assertLayout" => {
                self.ensure_booted()?;
                let width = s["width"].as_f64().unwrap_or(390.0) as f32;
                let height = s["height"].as_f64().unwrap_or(844.0) as f32;
                let rtl = s["rtl"].as_bool().unwrap_or(false);
                let out = self.rt.as_mut().expect("booted").handle(self.now, Input::Layout { width, height, rtl });
                let mut replies = HashMap::new();
                self.pump(out, &mut replies)?;
                let mut snap = self.inspect_snapshot()?;
                snap["viewport"] = json!({ "w": width, "h": height });
                attach_bounds(&mut snap["tree"], &self.frames, 0.0, 0.0);
                if s["review"].as_bool().unwrap_or(true) {
                    let findings = review::review(&snap);
                    if let Some(max) = s["maxFindings"].as_u64() {
                        if findings.len() as u64 > max {
                            return Err(format!("layout review had {} finding(s) (max {max}):\n{}", findings.len(), review::report(&snap, &findings)));
                        }
                    } else if findings.iter().any(|f| f.severity == Severity::Error) {
                        return Err(format!("layout review failed:\n{}", review::report(&snap, &findings)));
                    }
                }
                Ok(())
            }
            "assertMetrics" => {
                let snap = self.inspect_snapshot()?;
                let m = &snap["metrics"];
                for (limit_key, metric_key) in [
                    ("maxVmSteps", "vmSteps"),
                    ("maxRenders", "renders"),
                    ("maxPatchOps", "patchOps"),
                    ("maxNodes", "nodes"),
                    ("maxObjects", "objects"),
                ] {
                    if let Some(limit) = s[limit_key].as_u64() {
                        let actual = m[metric_key].as_u64().unwrap_or(0);
                        if actual > limit {
                            return Err(format!("metric {metric_key} was {actual}, exceeding {limit_key} {limit}"));
                        }
                    }
                }
                Ok(())
            }
            "assertInspect" => {
                let object = s["object"].as_str().ok_or("\"assertInspect\" needs \"object\"")?;
                let snap = self.inspect_snapshot()?;
                let found = snap["objects"]
                    .as_array()
                    .and_then(|arr| arr.iter().find(|o| o["object"].as_str() == Some(object)))
                    .map(|o| o["state"].clone())
                    .ok_or_else(|| format!("inspect did not contain loaded object {object:?}"))?;
                check_value(&format!("inspect {object}"), &found, s)
            }
            other => Err(format!("unknown trace action {other:?}; expected boot, as, plugin, http, call, get, ui, tick, privacy, assertScreen, assertLayout, assertMetrics or assertInspect")),
        }
    }
}

/// Walks the inspect tree and attaches absolute screen `bounds` computed from Taffy's
/// parent-relative `Frame`s, so `review::review` checks real overflow, overlap and touch sizes.
fn attach_bounds(nodes: &mut Json, frames: &HashMap<u32, Frame>, parent_x: f32, parent_y: f32) {
    let Some(list) = nodes.as_array_mut() else { return };
    for node in list {
        let id = node["id"].as_u64().map(|n| n as u32);
        let (x, y) = match id.and_then(|i| frames.get(&i)) {
            Some(f) => {
                let ax = parent_x + f.x;
                let ay = parent_y + f.y;
                node["bounds"] = json!({ "x": ax, "y": ay, "w": f.w, "h": f.h });
                node["visible"] = json!(f.w > 0.0 && f.h > 0.0);
                (ax, ay)
            }
            None => (parent_x, parent_y),
        };
        if let Some(children) = node.get_mut("children") {
            attach_bounds(children, frames, x, y);
        }
    }
}

fn check_reply(what: &str, res: CallReply, step: &Json) -> Result<(), String> {
    let wants_err = step.get("expectError").is_some() || step.get("expectField").is_some() || step.get("expectStatus").is_some();
    match (res, wants_err) {
        (Ok(val), false) => check_value(what, &val, step),
        (Ok(val), true) => Err(format!("{what}: expected error, but succeeded with {val}")),
        (Err((status, msg, _)), false) => Err(format!("{what}: unexpected error (status {status}): {msg}")),
        (Err((status, msg, field)), true) => {
            if let Some(want_status) = step["expectStatus"].as_u64() {
                if u64::from(status) != want_status {
                    return Err(format!("{what}: expected status {want_status}, got {status} ({msg})"));
                }
            }
            if let Some(want_err) = step["expectError"].as_str() {
                if !msg.contains(want_err) {
                    return Err(format!("{what}: expected error containing {want_err:?}, got {msg:?}"));
                }
            }
            if let Some(want_field) = step["expectField"].as_str() {
                if field.as_deref() != Some(want_field) {
                    return Err(format!("{what}: expected field {want_field:?}, got {field:?} (error: {msg:?})"));
                }
            }
            Ok(())
        }
    }
}

fn check_value(what: &str, actual: &Json, step: &Json) -> Result<(), String> {
    if let Some(want) = step.get("expect") {
        if actual != want {
            return Err(format!("{what}: expected {want}, got {actual}"));
        }
    }
    if let Some(want) = step.get("expectContains") {
        if !json_contains(actual, want) {
            return Err(format!("{what}: expected value containing {want}, got {actual}"));
        }
    }
    Ok(())
}

/// Recursive partial match: every key in `want` object must exist and match in `actual`.
fn json_contains(actual: &Json, want: &Json) -> bool {
    match (actual, want) {
        (Json::Object(a), Json::Object(w)) => w.iter().all(|(k, v)| a.get(k).is_some_and(|av| json_contains(av, v))),
        (Json::Array(a), Json::Array(w)) => w.iter().all(|wv| a.iter().any(|av| json_contains(av, wv))),
        _ => actual == want,
    }
}

fn parse_as_step(s: &Json) -> Result<Caller, String> {
    if s["trusted"].as_bool() == Some(true) {
        return Ok(Caller::trusted());
    }
    if let Some(c) = s.get("caller") {
        return parse_caller(c);
    }
    let id = s["id"].as_str().unwrap_or("").to_string();
    let roles = s["roles"].as_array().map(|a| a.iter().filter_map(|r| r.as_str().map(str::to_string)).collect()).unwrap_or_default();
    Ok(Caller::Remote { id, roles })
}

/// Parses `"trusted"`, `"anon"`, `"user:ana"`, `"key:sam:staff,admin"`, or `{"id", "roles"}`.
fn parse_caller(v: &Json) -> Result<Caller, String> {
    match v {
        Json::String(s) => match s.as_str() {
            "trusted" | "system" => Ok(Caller::trusted()),
            "anon" | "anonymous" | "" => Ok(Caller::Remote { id: String::new(), roles: Vec::new() }),
            spec => {
                // Allow `user:ana` (no roles) or `key:sam:staff,admin` (third colon lists roles).
                let mut parts = spec.splitn(3, ':');
                let (p1, p2, p3) = (parts.next().unwrap_or(""), parts.next(), parts.next());
                match (p2, p3) {
                    (Some(name), Some(roles)) => Ok(Caller::Remote {
                        id: format!("{p1}:{name}"),
                        roles: roles.split(',').filter(|r| !r.is_empty()).map(str::to_string).collect(),
                    }),
                    (Some(name), None) => Ok(Caller::Remote { id: format!("{p1}:{name}"), roles: Vec::new() }),
                    (None, _) => Ok(Caller::Remote { id: p1.to_string(), roles: Vec::new() }),
                }
            }
        },
        Json::Object(_) => parse_as_step(v),
        _ => Err(format!("invalid caller {v}; use \"trusted\", \"anon\", \"user:ana\", \"key:sam:staff\" or {{\"id\", \"roles\"}}")),
    }
}

fn parse_target(v: &Json) -> Result<Target, String> {
    match v {
        Json::String(s) => {
            if let Some(id) = s.strip_prefix('#') {
                Ok(Target::Id { id: id.to_string() })
            } else if let Ok(node) = s.parse::<u32>() {
                Ok(Target::Node { node })
            } else {
                Ok(Target::Text { text: s.clone() })
            }
        }
        Json::Number(n) => Ok(Target::Node { node: n.as_u64().ok_or("node target must be a positive integer")? as u32 }),
        Json::Object(_) => serde_json::from_value(v.clone()).map_err(|e| format!("bad ui target: {e}")),
        _ => Err(format!("ui target must be \"#id\", visible text, node number or target object, got {v}")),
    }
}

fn parse_duration_ms(s: &str) -> Result<f64, String> {
    let s = s.trim();
    for (suffix, factor) in [("ms", 1.0), ("s", 1_000.0), ("m", 60_000.0), ("h", 3_600_000.0), ("d", 86_400_000.0), ("y", 365.0 * 86_400_000.0)] {
        if let Some(num) = s.strip_suffix(suffix) {
            let n: f64 = num.trim().parse().map_err(|_| format!("invalid duration {s:?}"))?;
            return Ok(n * factor);
        }
    }
    Err(format!("duration {s:?} must end with ms, s, m, h, d or y"))
}

/// Runs one `.trace.jsonl` text against `source`.
pub fn run_trace(app_dir: &Path, source: &str, name: &str, trace_text: &str, cli_plugins: HashMap<String, FakePlugin>) -> TraceResult {
    let h = Harness::new(app_dir.to_path_buf(), source.to_string(), cli_plugins);
    execute_trace(h, name, trace_text)
}

/// Boots `source` once in memory and runs Taffy layout + `review::review` across every
/// viewport in `viewports`, reusing `TestMeasure` and `attach_bounds` (used by `tj retro`).
pub fn layout_findings_multi(source: &str, viewports: &[(&'static str, f32, f32, bool)]) -> Result<Vec<(&'static str, Vec<review::Finding>)>, String> {
    let mut h = Harness::new(PathBuf::from("."), source.to_string(), HashMap::new());
    h.boot(false)?;
    let mut results = Vec::with_capacity(viewports.len());
    for &(label, width, height, rtl) in viewports {
        let out = h.rt.as_mut().expect("booted").handle(h.now, Input::Layout { width, height, rtl });
        let mut replies = HashMap::new();
        h.pump(out, &mut replies)?;
        let mut snap = h.inspect_snapshot()?;
        snap["viewport"] = json!({ "w": width, "h": height });
        attach_bounds(&mut snap["tree"], &h.frames, 0.0, 0.0);
        results.push((label, review::review(&snap)));
    }
    Ok(results)
}

/// Runs a smoke trace against a caller-supplied store (e.g. a copy-on-write `OverlayStore`
/// over a live server's data directory during shadow boot). Disables filesystem reads from
/// `"plugin"` `"file"` steps and starts the clock at `initial_now` (live `now_ms()`).
pub fn run_trace_on_store(source: &str, name: &str, trace_text: &str, cli_plugins: HashMap<String, FakePlugin>, store: Box<dyn Store>, initial_now: f64) -> TraceResult {
    let h = Harness::with_store(None, source.to_string(), cli_plugins, SharedStore::from_store(store), true, initial_now);
    execute_trace(h, name, trace_text)
}

fn execute_trace(mut h: Harness, name: &str, trace_text: &str) -> TraceResult {
    let mut executed = 0;
    let mut error = None;
    for (idx, raw) in trace_text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let step: Json = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                error = Some(format!("{name}:{line_no}: invalid JSON: {e}"));
                break;
            }
        };
        executed += 1;
        if let Err(e) = h.step(&step) {
            error = Some(format!("{name}:{line_no}: {e}"));
            break;
        }
    }
    let (mut vm_steps, mut renders, mut patch_ops) = (0, 0, 0);
    if h.rt.is_some() {
        if let Ok(snap) = h.inspect_snapshot() {
            vm_steps = snap["metrics"]["vmSteps"].as_u64().unwrap_or(0);
            renders = snap["metrics"]["renders"].as_u64().unwrap_or(0);
            patch_ops = snap["metrics"]["patchOps"].as_u64().unwrap_or(0);
        }
    }
    TraceResult { name: name.to_string(), steps: executed, error, vm_steps, renders, patch_ops }
}

/// Finds `*.trace.jsonl` next to the app and in `<app_dir>/tests/`.
fn discover_traces(app_file: &Path) -> Vec<PathBuf> {
    let dir = app_file.parent().unwrap_or(Path::new("."));
    let mut found = Vec::new();
    for folder in [dir.to_path_buf(), dir.join("tests")] {
        if let Ok(entries) = std::fs::read_dir(&folder) {
            for e in entries.flatten() {
                let p = e.path();
                if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(".trace.jsonl")) {
                    found.push(p);
                }
            }
        }
    }
    found.sort();
    found
}

fn parse_cli_plugins(overrides: &[String]) -> Result<HashMap<String, FakePlugin>, String> {
    let mut out = HashMap::new();
    for o in overrides {
        let (name, spec) = o.split_once('=').ok_or_else(|| format!("--plugin takes name=fake:file.json, got {o}"))?;
        let path = spec.strip_prefix("fake:").ok_or_else(|| format!("--plugin {name}= supports fake:<file.json>"))?;
        let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
        let v: Json = serde_json::from_str(&text).map_err(|e| format!("{path} is not JSON: {e}"))?;
        out.insert(name.to_string(), FakePlugin::from_json(&v)?);
    }
    Ok(out)
}

/// CLI entry point: `tj test <app.tj> [trace.trace.jsonl ...] [--json] [--plugin name=fake:file.json]`.
pub fn run(args: &[String]) -> Result<(), String> {
    let usage = "usage: tj test <app.tj> [trace.trace.jsonl ...] [--json] [--plugin name=fake:file.json]";
    let mut app_file: Option<PathBuf> = None;
    let mut traces: Vec<PathBuf> = Vec::new();
    let mut as_json = false;
    let mut plugin_overrides: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => as_json = true,
            "--plugin" => plugin_overrides.push(it.next().ok_or("--plugin needs name=fake:file.json")?.clone()),
            other if other.starts_with("--") => return Err(format!("unknown option {other}\n\n{usage}")),
            path => {
                if app_file.is_none() {
                    app_file = Some(PathBuf::from(path));
                } else {
                    traces.push(PathBuf::from(path));
                }
            }
        }
    }
    let app_file = app_file.ok_or_else(|| format!("`tj test` needs an app file\n\n{usage}"))?;
    let source = crate::app::read(&app_file)?.source;
    // Fail early with full rendered diagnostics if the app does not compile.
    if let Err(errs) = compile(&source) {
        let name = app_file.display().to_string();
        return Err(errs.iter().map(|d| d.render(&name, &source)).collect::<Vec<_>>().join("\n\n"));
    }
    if traces.is_empty() {
        traces = discover_traces(&app_file);
        if traces.is_empty() {
            return Err(format!("no *.trace.jsonl files found next to {} (or in its tests/ folder); pass trace files explicitly or record one with `tj mcp` record_trace", app_file.display()));
        }
    }
    let cli_plugins = parse_cli_plugins(&plugin_overrides)?;
    let app_dir = app_file.parent().unwrap_or(Path::new("."));
    let mut results = Vec::new();
    for path in &traces {
        let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let name = path.display().to_string();
        results.push(run_trace(app_dir, &source, &name, &text, cli_plugins.clone()));
    }
    let passed = results.iter().filter(|r| r.passed()).count();
    let failed = results.len() - passed;
    if as_json {
        let report = json!({ "passed": passed, "failed": failed, "traces": results.iter().map(TraceResult::json).collect::<Vec<_>>() });
        println!("{}", serde_json::to_string_pretty(&report).expect("JSON serializes"));
    } else {
        for r in &results {
            match &r.error {
                None => println!("ok   {}  ({} steps, {} vmSteps, {} renders)", r.name, r.steps, r.vm_steps, r.renders),
                Some(e) => println!("FAIL {}\n  {e}", r.name),
            }
        }
        println!("\n{passed} passed, {failed} failed");
    }
    if failed > 0 {
        return Err(format!("{failed} trace(s) failed"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn runs_todo_and_booking_example_traces() {
        for (app, trace) in [
            ("examples/todo/app.tj", "examples/todo/tests/basic.trace.jsonl"),
            ("examples/booking/app.tj", "examples/booking/tests/booking.trace.jsonl"),
        ] {
            let app_path = workspace().join(app);
            let trace_path = workspace().join(trace);
            let source = crate::app::read(&app_path).expect("reads app").source;
            let text = std::fs::read_to_string(&trace_path).expect("reads trace");
            let r = run_trace(app_path.parent().unwrap(), &source, trace, &text, HashMap::new());
            assert!(r.passed(), "{trace} failed: {:?}", r.error);
            assert!(r.steps > 0 && r.vm_steps > 0);
        }
    }

    #[test]
    fn supports_fake_plugins_http_mocks_layout_errors_and_metric_ceilings() {
        let src = r#"
import { complete } from "tj:ai"

@server @read("anyone") object Bot {
  reply: string = ""
  @allow("anyone") async ping(url: string): string {
    const res = await fetch(url)
    const ai = await complete("say hi", [{ role: "user", text: "hi" }], [])
    this.reply = `${res.status}:${ai.text}`
    return this.reply
  }
}

function App() {
  return <Column padding={16}>
    <Button id="ok" onPress={() => {}}>Press me</Button>
  </Column>
}
"#;
        let trace = r#"
# Scripted HTTP and plugin answers run deterministically in memory
{"do": "plugin", "name": "ai", "answers": {"complete": [{"text": "hello", "calls": [], "stop": "end_turn", "raw": {}}]}}
{"do": "http", "match": "https://api.example.com/ok", "status": 200, "body": "ok"}
{"do": "call", "object": "Bot/main", "method": "ping", "args": ["https://api.example.com/ok"], "as": "anon", "expect": "200:hello"}
{"do": "assertLayout", "width": 390, "height": 844, "review": true}
{"do": "assertMetrics", "maxVmSteps": 50000}
"#;
        let r = run_trace(Path::new("."), src, "inline.trace.jsonl", trace, HashMap::new());
        assert!(r.passed(), "unexpected failure: {:?}", r.error);

        // Exceeding a vmSteps ceiling fails clearly.
        let tight = r#"{"do": "assertMetrics", "maxVmSteps": 1}"#;
        let fail = run_trace(Path::new("."), src, "tight.trace.jsonl", tight, HashMap::new());
        assert!(fail.error.as_deref().is_some_and(|e| e.contains("exceeding maxVmSteps")), "{fail:?}");

        // Layout review catches an unlabelled button (a11y Error) in pure memory!
        let bad_ui = r#"function App() { return <Column><Button onPress={() => {}}></Button></Column> }"#;
        let layout_check = r#"{"do": "assertLayout", "width": 390, "height": 844, "review": true}"#;
        let bad = run_trace(Path::new("."), bad_ui, "bad_ui.trace.jsonl", layout_check, HashMap::new());
        assert!(bad.error.as_deref().is_some_and(|e| e.contains("layout review failed")), "{bad:?}");
    }

    #[test]
    fn catches_output_errors_on_non_ui_steps_and_measures_time_nodes() {
        // L2-1: Output::Error emitted during a `call` step (e.g. a due alarm that throws when
        // another call arrives at the same timestamp) must fail the step, never be dropped.
        let err_src = r#"
@server @read("anyone") object Clock {
  n: number = 0
  @allow("anyone") arm() { this.schedule(0, "boom") }
  @allow("anyone") ping(): number { this.n += 1; return this.n }
  private boom() { throw "alarm exploded" }
}
"#;
        let err_trace = r#"
{"do": "boot", "now": 1000, "render": false}
{"do": "call", "object": "Clock/c", "method": "arm", "as": "anon"}
{"do": "call", "object": "Clock/c", "method": "ping", "as": "anon", "expect": 1}
"#;
        let r = run_trace(Path::new("."), err_src, "err.trace.jsonl", err_trace, HashMap::new());
        assert!(!r.passed(), "Output::Error from due alarm during `call` must fail the trace");
        assert!(r.error.unwrap().contains("alarm exploded"));

        // L2-2: <Time> receives non-zero layout bounds in `assertLayout`.
        let time_src = r#"
function App() {
  return <Column padding={16}>
    <Time id="slot-time" value={1800000000000} />
  </Column>
}
"#;
        let mut h = Harness::new(PathBuf::from("."), time_src.into(), HashMap::new());
        h.step(&json!({ "do": "boot", "now": 1800000000000.0 })).unwrap();
        h.step(&json!({ "do": "assertLayout", "width": 390, "height": 844, "review": true })).unwrap();
        let time_id = h.nodes.borrow().iter().find(|(_, (k, _))| k == "Time").map(|(id, _)| *id).expect("Time node exists");
        let frame = h.frames.get(&time_id).expect("Time node laid out");
        assert!(frame.w > 0.0 && frame.h > 0.0, "Time node must have non-zero bounds: {frame:?}");

        // C-036.2: a pre-boot `{"do": "as", ...}` step (as recorded by `tj mcp record_trace`)
        // applies `Input::Identity` on auto-boot AND across restarts so UI renders/handlers see `caller`.
        let ui_id_src = r#"
function App() {
  return <Column padding={16}>
    <Text id="who">{caller.id}:{caller.roles.join(",")}</Text>
  </Column>
}
"#;
        let pre_boot_as = r#"
{"do": "as", "id": "dev:ana", "roles": ["staff"]}
{"do": "boot", "server": false}
{"do": "assertScreen", "contains": ["dev:ana:staff"]}
{"do": "boot", "restart": true}
{"do": "assertScreen", "contains": ["dev:ana:staff"]}
"#;
        let r_as = run_trace(Path::new("."), ui_id_src, "pre_boot_as.trace.jsonl", pre_boot_as, HashMap::new());
        assert!(r_as.passed(), "pre-boot `as` identity must apply on boot and restart: {:?}", r_as.error);
    }
}

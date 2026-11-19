//! Owns the runtime on its own thread and carries out its outputs:
//! replies, HTTP effects (worker pool), realtime broadcasts, logs and timers.

use crate::store::FileStore;
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tj_core::runtime::protocol::Effect;
use tj_core::{compile, Input, Options, Output, Runtime};

const EFFECT_WORKERS: usize = 4;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

type Reply = Sender<Result<Json, String>>;

/// A realtime event: the object it concerns and its server-sent-event text.
pub type Event = (String, String);

enum Msg {
    Input(Input, Option<Reply>),
    EffectDone(u32, Result<Json, String>),
    Subscribe { events: Sender<Event>, objects: Vec<String>, since: u64 },
    Reload(String),
}

#[derive(Clone)]
pub struct Engine {
    tx: Sender<Msg>,
}

pub fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

/// Compiles and boots `source`, rendering compile errors for a terminal.
fn boot(source: &str, data: &Path, render: bool) -> Result<(Runtime, Vec<Output>), String> {
    let program = compile(source).map_err(|errs| errs.iter().map(|d| d.render("app.tj", source)).collect::<Vec<_>>().join("\n\n"))?;
    let store = FileStore::open(data).map_err(|e| format!("cannot open data dir {}: {e}", data.display()))?;
    let seed = now_ms() as u64;
    Runtime::boot(program, Box::new(store), Options { now: now_ms(), seed, render, server: true }).map_err(|e| format!("boot failed: {e}"))
}

impl Engine {
    /// Starts the runtime thread; fails if the app does not compile or boot.
    pub fn start(source: String, data: PathBuf, render: bool) -> Result<Engine, String> {
        let (tx, rx) = channel();
        let (ready_tx, ready_rx) = channel();
        let back = tx.clone();
        thread::Builder::new()
            .name("tj-runtime".into())
            .spawn(move || match boot(&source, &data, render) {
                Ok((rt, out)) => {
                    let _ = ready_tx.send(Ok(()));
                    run(rt, out, rx, back, data, render);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| e.to_string())?;
        ready_rx.recv().map_err(|_| "runtime thread died".to_string())??;
        Ok(Engine { tx })
    }

    /// Sends an input and waits for its reply, if the input kind has one.
    pub fn request(&self, input: Input) -> Result<Json, String> {
        if !has_reply(&input) {
            self.tx.send(Msg::Input(input, None)).map_err(|_| "runtime stopped")?;
            return Ok(Json::Null);
        }
        let (t, r) = channel();
        self.tx.send(Msg::Input(input, Some(t))).map_err(|_| "runtime stopped")?;
        r.recv_timeout(REQUEST_TIMEOUT).map_err(|e| match e {
            RecvTimeoutError::Timeout => "timed out waiting for the app".to_string(),
            RecvTimeoutError::Disconnected => "the app restarted before answering".to_string(),
        })?
    }

    /// Realtime stream of `emit` and `changed` events. It starts with the current state of
    /// each of `objects` that changed after change number `since`, so a client that
    /// reconnects resumes where it left off instead of refetching.
    pub fn subscribe(&self, objects: Vec<String>, since: u64) -> Receiver<Event> {
        let (t, r) = channel();
        let _ = self.tx.send(Msg::Subscribe { events: t, objects, since });
        r
    }

    /// Re-boots with new source. Objects keep their stored state.
    pub fn reload(&self, source: String) {
        let _ = self.tx.send(Msg::Reload(source));
    }
}

fn has_reply(input: &Input) -> bool {
    matches!(input, Input::Call { .. } | Input::Get { .. } | Input::Ui { .. } | Input::Inspect { .. })
}

fn set_reply(input: &mut Input, id: u64) {
    match input {
        Input::Call { reply, .. } | Input::Get { reply, .. } | Input::Ui { reply, .. } | Input::Inspect { reply, .. } => *reply = Some(id),
        Input::EffectDone { .. } | Input::Sync { .. } | Input::Layout { .. } | Input::Tick => {}
    }
}

struct Dispatch {
    replies: HashMap<u64, Reply>,
    next_reply: u64,
    subscribers: Vec<Sender<Event>>,
    effects: Sender<(u32, Effect)>,
}

fn run(mut rt: Runtime, boot_out: Vec<Output>, rx: Receiver<Msg>, back: Sender<Msg>, data: PathBuf, render: bool) {
    let mut d = Dispatch { replies: HashMap::new(), next_reply: 0, subscribers: Vec::new(), effects: effect_pool(back) };
    d.outputs(boot_out);
    loop {
        let msg = match rt.next_wake() {
            Some(at) => rx.recv_timeout(Duration::from_millis((at - now_ms()).max(0.0) as u64)),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let out = match msg {
            Ok(Msg::Input(mut input, reply)) => {
                if let Some(r) = reply {
                    d.next_reply += 1;
                    d.replies.insert(d.next_reply, r);
                    set_reply(&mut input, d.next_reply);
                }
                rt.handle(now_ms(), input)
            }
            Ok(Msg::EffectDone(id, result)) => {
                let (ok, error) = match result {
                    Ok(v) => (Some(v), None),
                    Err(e) => (None, Some(e)),
                };
                rt.handle(now_ms(), Input::EffectDone { id, ok, error })
            }
            Ok(Msg::Subscribe { events, objects, since }) => {
                // Catch-up and live changes come from this one thread, so the client sees every
                // change once, in order.
                for o in rt.changed_since(&objects, since) {
                    if let Output::Changed { object, state, seq } = o {
                        let _ = events.send((object.clone(), changed_event(&object, &state, seq)));
                    }
                }
                d.subscribers.push(events);
                continue;
            }
            Ok(Msg::Reload(source)) => match boot(&source, &data, render) {
                Ok((new_rt, out)) => {
                    rt = new_rt;
                    // Requests waiting on the old runtime get a "restarted" error.
                    d.replies.clear();
                    eprintln!("[tj] reloaded");
                    out
                }
                Err(e) => {
                    eprintln!("[tj] reload failed, keeping the running version:\n{e}");
                    continue;
                }
            },
            Err(RecvTimeoutError::Timeout) => rt.handle(now_ms(), Input::Tick),
            Err(RecvTimeoutError::Disconnected) => return,
        };
        d.outputs(out);
    }
}

impl Dispatch {
    fn outputs(&mut self, out: Vec<Output>) {
        for o in out {
            match o {
                Output::Reply { reply, ok, error } => {
                    if let Some(r) = self.replies.remove(&reply) {
                        let _ = r.send(match error {
                            Some(e) => Err(e),
                            None => Ok(ok.unwrap_or(Json::Null)),
                        });
                    }
                }
                Output::Effect { id, effect } => {
                    let _ = self.effects.send((id, effect));
                }
                Output::Emit { object, name, data } => {
                    let msg = format!("event: emit\ndata: {}\n\n", json!({ "object": object, "name": name, "data": data }));
                    self.broadcast(&object, msg);
                }
                Output::Changed { object, state, seq } => {
                    let msg = changed_event(&object, &state, seq);
                    self.broadcast(&object, msg);
                }
                Output::Log { level, message } => eprintln!("[app] {level}: {message}"),
                Output::Error { message, trace } => {
                    eprintln!("[app] error: {message}");
                    for t in trace {
                        eprintln!("    {t}");
                    }
                }
                // The server owns its objects, so it never subscribes to replicas or draws UI.
                Output::Patch { .. } | Output::Frames { .. } | Output::Wake { .. } | Output::Subscribe { .. } => {}
            }
        }
    }

    fn broadcast(&mut self, object: &str, msg: String) {
        self.subscribers.retain(|s| s.send((object.to_string(), msg.clone())).is_ok());
    }
}

/// A `changed` server-sent event. Its id is the change number, so a browser's EventSource
/// resumes from it by itself (`Last-Event-ID`).
fn changed_event(object: &str, state: &Json, seq: u64) -> String {
    format!("id: {seq}\nevent: changed\ndata: {}\n\n", json!({ "object": object, "state": state, "seq": seq }))
}

/// A small pool of threads that perform effects and post the results back.
fn effect_pool(back: Sender<Msg>) -> Sender<(u32, Effect)> {
    let (tx, rx) = channel::<(u32, Effect)>();
    let rx = Arc::new(Mutex::new(rx));
    let agent = ureq::AgentBuilder::new().timeout(HTTP_TIMEOUT).build();
    for i in 0..EFFECT_WORKERS {
        let (rx, back, agent) = (rx.clone(), back.clone(), agent.clone());
        let spawned = thread::Builder::new().name(format!("tj-effects-{i}")).spawn(move || loop {
            let job = rx.lock().map_err(|_| ()).and_then(|r| r.recv().map_err(|_| ()));
            let Ok((id, effect)) = job else { return };
            if back.send(Msg::EffectDone(id, perform(&agent, effect))).is_err() {
                return;
            }
        });
        if let Err(e) = spawned {
            eprintln!("[tj] could not start an effect worker: {e}");
        }
    }
    tx
}

fn perform(agent: &ureq::Agent, effect: Effect) -> Result<Json, String> {
    let Effect::Http { url, method, headers, body } = effect else {
        return Err("server objects live here; there is no server to call".into());
    };
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!("only http(s) URLs can be fetched: {url}"));
    }
    let mut req = agent.request(&method, &url);
    if let Json::Object(h) = &headers {
        for (k, v) in h {
            if let Some(v) = v.as_str() {
                req = req.set(k, v);
            }
        }
    }
    let resp = match body {
        Some(b) => req.send_string(&b),
        None => req.call(),
    };
    let resp = match resp {
        Ok(r) | Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(e.to_string()),
    };
    let status = resp.status();
    let headers: serde_json::Map<String, Json> = resp.headers_names().into_iter().filter_map(|n| resp.header(&n).map(|v| (n.clone(), Json::String(v.to_string())))).collect();
    let body = resp.into_string().map_err(|e| e.to_string())?;
    Ok(json!({ "status": status, "headers": headers, "body": body }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The next `changed` event on a stream, with its change number.
    fn next_change(events: &Receiver<Event>) -> (u64, String) {
        loop {
            let (_, msg) = events.recv_timeout(Duration::from_secs(5)).expect("a change");
            if let Some(rest) = msg.strip_prefix("id: ") {
                let seq = rest.split('\n').next().and_then(|s| s.parse().ok()).expect("a numeric id");
                return (seq, msg);
            }
        }
    }

    #[test]
    fn streams_resume_from_the_clients_change_number() {
        let data = std::env::temp_dir().join(format!("tj-engine-test-{}", std::process::id()));
        let source = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/chat/app.tj")).expect("chat example");
        let engine = Engine::start(source, data.clone(), false).expect("boots");
        let post = |text: &str| engine.request(Input::Call { object: "Room/lobby".into(), method: "post".into(), args: vec![json!("ana"), json!(text)], reply: None });
        post("one").expect("posted");

        // A new client catches up on the current state first.
        let (seq, msg) = next_change(&engine.subscribe(vec!["Room/lobby".into()], 0));
        assert!(msg.contains("\"one\""), "{msg}");

        // A client that already has that change gets nothing old, only what happens next.
        let resumed = engine.subscribe(vec!["Room/lobby".into()], seq);
        post("two").expect("posted");
        let (next, msg) = next_change(&resumed);
        assert!(next > seq && msg.contains("\"two\""), "{msg}");
        let _ = std::fs::remove_dir_all(&data);
    }
}

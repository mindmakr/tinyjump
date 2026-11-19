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

enum Msg {
    Input(Input, Option<Reply>),
    EffectDone(u32, Result<Json, String>),
    Subscribe(Sender<String>),
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
    Runtime::boot(program, Box::new(store), Options { now: now_ms(), seed, render }).map_err(|e| format!("boot failed: {e}"))
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

    /// Realtime stream of server-sent-event messages (`emit` and `changed`).
    pub fn subscribe(&self) -> Receiver<String> {
        let (t, r) = channel();
        let _ = self.tx.send(Msg::Subscribe(t));
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
        Input::EffectDone { .. } | Input::Tick => {}
    }
}

struct Dispatch {
    replies: HashMap<u64, Reply>,
    next_reply: u64,
    subscribers: Vec<Sender<String>>,
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
            Ok(Msg::Subscribe(s)) => {
                d.subscribers.push(s);
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
                Output::Emit { object, name, data } => self.broadcast("emit", &json!({ "object": object, "name": name, "data": data })),
                Output::Changed { object, state } => self.broadcast("changed", &json!({ "object": object, "state": state })),
                Output::Log { level, message } => eprintln!("[app] {level}: {message}"),
                Output::Error { message, trace } => {
                    eprintln!("[app] error: {message}");
                    for t in trace {
                        eprintln!("    {t}");
                    }
                }
                Output::Patch { .. } | Output::Wake { .. } => {}
            }
        }
    }

    fn broadcast(&mut self, event: &str, data: &Json) {
        let msg = format!("event: {event}\ndata: {data}\n\n");
        self.subscribers.retain(|s| s.send(msg.clone()).is_ok());
    }
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
    let Effect::Http { url, method, headers, body } = effect;
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

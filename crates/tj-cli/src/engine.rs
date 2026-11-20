//! Owns the runtime on its own thread and carries out its outputs:
//! replies, HTTP effects (worker pool), realtime broadcasts, logs, timers and the audit log.

use crate::audit::{Actor, Audit, Entry};
use crate::identity::sha256_hex;
use crate::mailbox::{Event, Mailbox};
use crate::plugins::Plugins;
use crate::stats::Stats;
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tj_core::runtime::protocol::{Caller, Effect};
use tj_core::{compile, Input, Options, Output, Runtime};

const EFFECT_WORKERS: usize = 4;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// `1` lets `tj serve` fetch private, loopback and link-local addresses.
pub const PRIVATE_FETCH_ENV: &str = "TJ_ALLOW_PRIVATE_FETCH";

/// A refused or failed request: its HTTP status and message.
pub type Failure = (u16, String);

type Reply = Sender<Result<Json, Failure>>;

/// Remote requests may load at most this many objects at once (`TJ_MAX_OBJECTS`).
const MAX_OBJECTS: usize = 100_000;

enum Msg {
    /// An input, who waits for its reply, and when it was sent (for latency).
    Input(Input, Option<Reply>, Instant),
    EffectDone(u32, Result<Json, String>),
    /// `caller: None` is the admin: every object, unchecked.
    Subscribe { events: Arc<Mailbox>, objects: Vec<String>, since: u64, caller: Option<Caller>, done: Sender<Result<(), Failure>> },
    /// Whether `caller` may read every one of `objects` (for stream tickets).
    MayRead { objects: Vec<String>, caller: Caller, done: Sender<Result<(), Failure>> },
    /// The core's statistics (`Runtime::stats`).
    Stats(Sender<Json>),
    /// Export (`erase: false`) or erase everything held about a person.
    Privacy { subject: String, erase: bool, done: Sender<Result<Json, Failure>> },
    Reload(String),
}

#[derive(Clone)]
pub struct Engine {
    tx: Sender<Msg>,
    stats: Arc<Stats>,
}

pub fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

/// Compiles and boots `source`, rendering compile errors for a terminal. `explain` says why
/// requests are refused (`tj dev` only).
fn boot(source: &str, data: &Path, render: bool, explain: bool) -> Result<(Runtime, Vec<Output>), String> {
    let program = compile(source).map_err(|errs| errs.iter().map(|d| d.render("app.tj", source)).collect::<Vec<_>>().join("\n\n"))?;
    let (store, _) = crate::sealed::open_store(data)?;
    let seed = crate::bytes::random::<8>()?;
    let max_objects = std::env::var("TJ_MAX_OBJECTS").ok().and_then(|v| v.parse().ok()).unwrap_or(MAX_OBJECTS);
    let opts = Options { now: now_ms(), seed: u64::from_le_bytes(seed), render, server: true, explain, max_objects };
    let (mut rt, mut out) = Runtime::boot(program, store, opts).map_err(|e| format!("boot failed: {e}"))?;
    // Secure random bytes for `crypto.randomId()` and ids, as every host sends them.
    out.extend(rt.handle(now_ms(), Input::Entropy { bytes: crate::bytes::hex(&crate::bytes::random::<32>()?) }));
    Ok((rt, out))
}

impl Engine {
    /// Starts the runtime thread; fails if the app does not compile or boot.
    pub fn start(source: String, data: PathBuf, render: bool, explain: bool, plugins: Arc<Plugins>, audit: Audit) -> Result<Engine, String> {
        let (tx, rx) = channel();
        let (ready_tx, ready_rx) = channel();
        let back = tx.clone();
        let stats = Arc::new(Stats::default());
        let measured = stats.clone();
        thread::Builder::new()
            .name("tj-runtime".into())
            .spawn(move || match boot(&source, &data, render, explain) {
                Ok((rt, out)) => {
                    let _ = ready_tx.send(Ok(()));
                    audit.record(Entry::new("start").text("app", &sha256_hex(&source)));
                    run(rt, out, rx, back, data, render, explain, plugins, measured, audit);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| e.to_string())?;
        ready_rx.recv().map_err(|_| "runtime thread died".to_string())??;
        Ok(Engine { tx, stats })
    }

    /// Sends an input and waits for its reply, if the input kind has one.
    pub fn request(&self, input: Input) -> Result<Json, Failure> {
        let stopped = || (503, "runtime stopped".to_string());
        if !has_reply(&input) {
            self.tx.send(Msg::Input(input, None, Instant::now())).map_err(|_| stopped())?;
            return Ok(Json::Null);
        }
        let (t, r) = channel();
        self.tx.send(Msg::Input(input, Some(t), Instant::now())).map_err(|_| stopped())?;
        Self::wait(&r)?
    }

    /// The core's statistics: calls, errors, refusals and VM steps per method, and objects
    /// loaded per class. Cheaper than `inspect`, for metrics scrapers.
    pub fn core_stats(&self) -> Result<Json, Failure> {
        let (done, r) = channel();
        self.tx.send(Msg::Stats(done)).map_err(|_| (503, "runtime stopped".to_string()))?;
        Self::wait(&r)
    }

    fn wait<T>(r: &Receiver<T>) -> Result<T, Failure> {
        r.recv_timeout(REQUEST_TIMEOUT).map_err(|e| match e {
            RecvTimeoutError::Timeout => (503, "timed out waiting for the app".to_string()),
            RecvTimeoutError::Disconnected => (503, "the app restarted before answering".to_string()),
        })
    }

    /// Realtime stream of `emit` and `changed` events about `objects`, which `caller` must be
    /// allowed to read (`None`: the admin, and every object if `objects` is empty). It starts
    /// with the current state of each of `objects` that changed after change number `since`,
    /// so a client that reconnects resumes where it left off instead of refetching.
    pub fn subscribe(&self, objects: Vec<String>, since: u64, caller: Option<Caller>) -> Result<Arc<Mailbox>, Failure> {
        let reader = match &caller {
            Some(Caller::Remote { id, .. }) => Some(id.clone()),
            _ => None,
        };
        let events = Arc::new(Mailbox::new(objects.clone(), Some(self.stats.clone())).read_by(reader));
        let (done, r) = channel();
        self.tx.send(Msg::Subscribe { events: events.clone(), objects, since, caller, done }).map_err(|_| (503, "runtime stopped".to_string()))?;
        Self::wait(&r)??;
        Ok(events)
    }

    /// Whether `caller` may read every one of `objects`.
    pub fn may_read(&self, objects: Vec<String>, caller: Caller) -> Result<(), Failure> {
        let (done, r) = channel();
        self.tx.send(Msg::MayRead { objects, caller, done }).map_err(|_| (503, "runtime stopped".to_string()))?;
        Self::wait(&r)?
    }

    /// Everything held about `subject` (`erase: false`), or erases it and forgets their audit
    /// pseudonym (docs/privacy.md).
    pub fn privacy(&self, subject: String, erase: bool) -> Result<Json, Failure> {
        let (done, r) = channel();
        self.tx.send(Msg::Privacy { subject, erase, done }).map_err(|_| (503, "runtime stopped".to_string()))?;
        Self::wait(&r)?
    }

    /// The server's work counters.
    pub fn stats(&self) -> &Stats {
        &self.stats
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
        Input::EffectDone { .. } | Input::Sync { .. } | Input::Layout { .. } | Input::Online | Input::Identity { .. } | Input::Entropy { .. } | Input::Tick => {}
    }
}

struct Dispatch {
    replies: HashMap<u64, Waiter>,
    next_reply: u64,
    subscribers: Vec<Arc<Mailbox>>,
    effects: Sender<(u32, Effect)>,
    stats: Arc<Stats>,
    audit: Audit,
}

/// Who waits for a reply; for calls, the method and when the call was sent; for reads of
/// personal data, what to write to the audit log once it was read.
struct Waiter {
    reply: Reply,
    timed: Option<(String, Instant)>,
    read: Option<(String, Caller)>,
}

/// Whether `caller` may read every one of `objects`, checked on the runtime thread.
fn may_read(rt: &Runtime, objects: &[String], caller: &Caller) -> Result<(), Failure> {
    objects.iter().try_for_each(|o| rt.may_read(o, caller).map_err(|why| (403, why)))
}

#[allow(clippy::too_many_arguments)]
fn run(mut rt: Runtime, boot_out: Vec<Output>, rx: Receiver<Msg>, back: Sender<Msg>, data: PathBuf, render: bool, explain: bool, plugins: Arc<Plugins>, stats: Arc<Stats>, audit: Audit) {
    // `tj dev` (which explains refusals) reaches local services; a server only public ones.
    let private_fetch = explain || std::env::var(PRIVATE_FETCH_ENV).is_ok_and(|v| v == "1");
    let effects = effect_pool(back, plugins, stats.clone(), audit.clone(), private_fetch);
    let mut d = Dispatch { replies: HashMap::new(), next_reply: 0, subscribers: Vec::new(), effects, stats, audit };
    d.outputs(&rt, boot_out);
    loop {
        let msg = match rt.next_wake() {
            Some(at) => rx.recv_timeout(Duration::from_millis((at - now_ms()).max(0.0) as u64)),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let out = match msg {
            Ok(Msg::Input(mut input, reply, sent)) => {
                if let Some(reply) = reply {
                    // Only methods that exist are timed, so request paths cannot grow the table.
                    let timed = match &input {
                        Input::Call { object, method, .. } => rt.method_name(object, method).map(|name| (name, sent)),
                        _ => None,
                    };
                    // The admin's reads are audited where they arrive; a person's, here.
                    let read = match &input {
                        Input::Get { object, caller: Some(c @ Caller::Remote { .. }), .. } if rt.holds_personal_data(object) => Some((object.clone(), c.clone())),
                        _ => None,
                    };
                    d.next_reply += 1;
                    d.replies.insert(d.next_reply, Waiter { reply, timed, read });
                    set_reply(&mut input, d.next_reply);
                }
                rt.handle(now_ms(), input)
            }
            Ok(Msg::Stats(done)) => {
                let _ = done.send(rt.stats());
                continue;
            }
            Ok(Msg::Privacy { subject, erase, done }) => {
                if !erase {
                    let _ = done.send(rt.export_subject(&subject).map_err(|e| (503, e)));
                    continue;
                }
                match rt.erase_subject(&subject) {
                    Ok(e) => {
                        d.audit.forget(&subject);
                        let held: Vec<Json> = e.held.iter().map(|(object, until)| json!({ "object": object, "until": until })).collect();
                        let _ = done.send(Ok(json!({ "erased": e.erased, "held": held })));
                        e.outputs
                    }
                    Err(e) => {
                        let _ = done.send(Err(e));
                        continue;
                    }
                }
            }
            Ok(Msg::EffectDone(id, result)) => {
                let (ok, error) = match result {
                    Ok(v) => (Some(v), None),
                    Err(e) => (None, Some(e)),
                };
                rt.handle(now_ms(), Input::EffectDone { id, ok, error, unreachable: false })
            }
            Ok(Msg::MayRead { objects, caller, done }) => {
                let _ = done.send(may_read(&rt, &objects, &caller));
                continue;
            }
            Ok(Msg::Subscribe { events, objects, since, caller, done }) => {
                if let Some(Err(refused)) = caller.as_ref().map(|c| may_read(&rt, &objects, c)) {
                    let _ = done.send(Err(refused));
                    continue;
                }
                let _ = done.send(Ok(()));
                if let Some(c) = caller.filter(|c| matches!(c, Caller::Remote { .. })) {
                    for object in objects.iter().filter(|o| rt.holds_personal_data(o)) {
                        d.audit.record(Entry::new("stream").who(Actor::of(&c)).object(object));
                    }
                }
                // Catch-up and live changes come from this one thread, so the client sees every
                // change once, in order.
                for o in rt.changed_since(&objects, since) {
                    if let Output::Changed { object, state, seq } = o {
                        let state = if events.owns(&object) { None } else { rt.masked(&object, &state) }.unwrap_or(state);
                        events.push(changed_event(&object, &state, seq));
                    }
                }
                d.subscribers.push(events);
                continue;
            }
            Ok(Msg::Reload(source)) => match boot(&source, &data, render, explain) {
                Ok((new_rt, out)) => {
                    rt = new_rt;
                    d.audit.record(Entry::new("start").text("app", &sha256_hex(&source)));
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
        d.outputs(&rt, out);
    }
}

impl Dispatch {
    fn outputs(&mut self, rt: &Runtime, out: Vec<Output>) {
        for o in out {
            match o {
                Output::Reply { reply, ok, error, status } => {
                    if let Some(w) = self.replies.remove(&reply) {
                        if let Some((name, sent)) = &w.timed {
                            self.stats.method(name, sent.elapsed(), error.is_none());
                        }
                        if let Some((object, caller)) = w.read.filter(|_| error.is_none()) {
                            self.audit.record(Entry::new("read").who(Actor::of(&caller)).object(&object));
                        }
                        let _ = w.reply.send(match error {
                            Some(e) => Err((status.unwrap_or(400), e)),
                            None => Ok(ok.unwrap_or(Json::Null)),
                        });
                    }
                }
                Output::Effect { id, effect } => {
                    let _ = self.effects.send((id, effect));
                }
                Output::Emit { object, name, data } => {
                    let text = format!("event: emit\ndata: {}\n\n", json!({ "object": object, "name": name, "data": data }));
                    // A slow client gets one `read` per object, not one per request.
                    let merge = (name == "read").then_some("read");
                    self.broadcast(Event::new(object, merge, text));
                }
                Output::Changed { object, state, seq } => {
                    let full = changed_event(&object, &state, seq);
                    match rt.masked(&object, &state) {
                        // Personal data goes to its owner only.
                        Some(masked) => {
                            let masked = changed_event(&object, &masked, seq);
                            self.subscribers.retain(|s| s.push(if s.owns(&object) { full.clone() } else { masked.clone() }));
                        }
                        None => self.broadcast(full),
                    }
                }
                Output::Log { level, message } => eprintln!("[app] {level}: {message}"),
                Output::Erased { object, reason } => self.audit.record(Entry::new("erased").object(&object).text("reason", &reason)),
                Output::Ran { object, method, by, caller, ok, ms } => {
                    let kind = match by.as_str() {
                        "timer" => "timer",
                        "alarm" => "alarm",
                        _ => "invoke",
                    };
                    self.audit.record(Entry::new(kind).who(Actor::id(caller.as_deref())).object(&object).text("method", &method).ok(ok).num("ms", ms));
                }
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

    /// Hands an event to every client; clients that have gone are forgotten.
    fn broadcast(&mut self, e: Event) {
        self.subscribers.retain(|s| s.push(e.clone()));
    }
}

/// A `changed` server-sent event. Its id is the change number, so a browser's EventSource
/// resumes from it by itself (`Last-Event-ID`).
fn changed_event(object: &str, state: &Json, seq: u64) -> Event {
    let text = format!("id: {seq}\nevent: changed\ndata: {}\n\n", json!({ "object": object, "state": state, "seq": seq }));
    Event::new(object.to_string(), Some("state"), text)
}

/// A small pool of threads that perform effects and post the results back.
fn effect_pool(back: Sender<Msg>, plugins: Arc<Plugins>, stats: Arc<Stats>, audit: Audit, private_fetch: bool) -> Sender<(u32, Effect)> {
    let (tx, rx) = channel::<(u32, Effect)>();
    let rx = Arc::new(Mutex::new(rx));
    let mut agent = ureq::AgentBuilder::new().timeout(HTTP_TIMEOUT);
    if !private_fetch {
        // Checked where names become addresses, so redirects and names that point inside are
        // caught too.
        agent = agent.resolver(public_only);
    }
    let agent = agent.build();
    for i in 0..EFFECT_WORKERS {
        let (rx, back, agent, plugins, stats, audit) = (rx.clone(), back.clone(), agent.clone(), plugins.clone(), stats.clone(), audit.clone());
        let spawned = thread::Builder::new().name(format!("tj-effects-{i}")).spawn(move || loop {
            let job = rx.lock().map_err(|_| ()).and_then(|r| r.recv().map_err(|_| ()));
            let Ok((id, effect)) = job else { return };
            let kind = match &effect {
                Effect::Http { .. } => "http".to_string(),
                Effect::Plugin { plugin, .. } => format!("plugin.{plugin}"),
                Effect::Call { .. } => "call".to_string(),
            };
            // Plugin calls are audited; fetches are not (URLs can carry secrets).
            let plugin_call = match &effect {
                Effect::Plugin { plugin, method, .. } => Some((plugin.clone(), method.clone())),
                _ => None,
            };
            let started = Instant::now();
            let result = perform(&agent, &plugins, effect);
            stats.effect(&kind, started.elapsed(), result.is_ok());
            if let Some((plugin, method)) = plugin_call {
                audit.record(Entry::new("plugin").text("plugin", &plugin).text("method", &method).ok(result.is_ok()).num("ms", started.elapsed().as_millis() as u64));
            }
            if back.send(Msg::EffectDone(id, result)).is_err() {
                return;
            }
        });
        if let Err(e) = spawned {
            eprintln!("[tj] could not start an effect worker: {e}");
        }
    }
    tx
}

/// The public addresses of `netloc` (`host:port`): a server's `fetch` must not reach this
/// machine, its private network or the cloud metadata service (169.254.169.254), where an
/// app's URL argument or an agent's prompt could otherwise read internal data.
fn public_only(netloc: &str) -> std::io::Result<Vec<std::net::SocketAddr>> {
    use std::net::ToSocketAddrs;
    let public: Vec<_> = netloc.to_socket_addrs()?.filter(|a| is_public(a.ip())).collect();
    if public.is_empty() {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("{netloc} is a loopback, private or link-local address; a server fetches only public ones unless {PRIVATE_FETCH_ENV}=1")));
    }
    Ok(public)
}

fn is_public(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v) => {
            let [a, b, ..] = v.octets();
            // 100.64.0.0/10 is carrier-grade NAT, and 0.0.0.0/8 means this network.
            !(v.is_loopback() || v.is_private() || v.is_link_local() || v.is_unspecified() || v.is_broadcast() || v.is_multicast() || a == 0 || (a == 100 && b & 0xc0 == 64))
        }
        IpAddr::V6(v) => {
            let first = v.segments()[0];
            // fc00::/7 is unique local, fe80::/10 link-local; mapped IPv4 follows the IPv4 rules.
            let local = v.is_loopback() || v.is_unspecified() || v.is_multicast() || first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80;
            !local && v.to_ipv4_mapped().map_or(true, |m| is_public(IpAddr::V4(m)))
        }
    }
}

fn perform(agent: &ureq::Agent, plugins: &Plugins, effect: Effect) -> Result<Json, String> {
    let (url, method, headers, body) = match effect {
        Effect::Http { url, method, headers, body } => (url, method, headers, body),
        Effect::Plugin { plugin, method, args } => return plugins.call(&plugin, &method, args),
        Effect::Call { .. } => return Err("server objects live here; there is no server to call".into()),
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
    fn next_change(events: &Mailbox) -> (u64, String) {
        for _ in 0..50 {
            for e in events.take(Duration::from_millis(100)) {
                if let Some(rest) = e.text.strip_prefix("id: ") {
                    let seq = rest.split('\n').next().and_then(|s| s.parse().ok()).expect("a numeric id");
                    return (seq, e.text);
                }
            }
        }
        panic!("no change within 5s")
    }

    #[test]
    fn streams_resume_from_the_clients_change_number() {
        let data = std::env::temp_dir().join(format!("tj-engine-test-{}", std::process::id()));
        let source = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/chat/app.tj")).expect("chat example");
        let audit = Audit::start(&data).expect("audit log").0;
        let engine = Engine::start(source, data.clone(), false, false, Arc::default(), audit).expect("boots");
        let post = |text: &str| engine.request(Input::Call { object: "Room/lobby".into(), method: "post".into(), args: vec![json!("ana"), json!(text)], reply: None, call: None, caller: Some(Caller::trusted()) });
        post("one").expect("posted");

        // A new client catches up on the current state first.
        let (seq, msg) = next_change(&engine.subscribe(vec!["Room/lobby".into()], 0, None).expect("admin may stream"));
        assert!(msg.contains("\"one\""), "{msg}");

        // A client that already has that change gets nothing old, only what happens next.
        let resumed = engine.subscribe(vec!["Room/lobby".into()], seq, None).expect("admin may stream");
        post("two").expect("posted");
        let (next, msg) = next_change(&resumed);
        assert!(next > seq && msg.contains("\"two\""), "{msg}");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn servers_fetch_only_public_addresses() {
        for inside in ["127.0.0.1:80", "[::1]:80", "10.1.2.3:443", "192.168.1.1:80", "172.16.0.9:80", "169.254.169.254:80", "100.64.0.1:80", "0.0.0.0:80", "[fd00::1]:80", "[fe80::1]:80", "[::ffff:127.0.0.1]:80", "localhost:80"] {
            assert!(public_only(inside).is_err(), "{inside}");
        }
        assert_eq!(public_only("8.8.8.8:53").unwrap().len(), 1);
        assert!(public_only("[2606:4700::1111]:443").is_ok());
    }

    #[test]
    fn calls_are_timed_by_method_and_counted_by_the_core() {
        let data = std::env::temp_dir().join(format!("tj-engine-stats-{}", std::process::id()));
        let source = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/chat/app.tj")).expect("chat example");
        let audit = Audit::start(&data).expect("audit log").0;
        let engine = Engine::start(source, data.clone(), false, false, Arc::default(), audit).expect("boots");
        let call = |method: &str| engine.request(Input::Call { object: "Room/lobby".into(), method: method.into(), args: vec![json!("ana"), json!("hi")], reply: None, call: None, caller: Some(Caller::trusted()) });
        call("post").expect("posted");
        call("post").expect("posted");
        assert!(call("nope").is_err());

        let timed = engine.stats().json()["methods"].clone();
        assert_eq!(timed["Room.post"]["count"], json!(2), "{timed}");
        assert_eq!(timed.as_object().map(|m| m.len()), Some(1), "unknown methods are not timed: {timed}");
        let core = engine.core_stats().expect("stats");
        assert_eq!(core["methods"]["Room.post"]["calls"], json!(2), "{core}");
        assert!(engine.stats().openmetrics(&core).contains("tj_method_duration_seconds_count{class=\"Room\",method=\"post\"} 2"));
        let _ = std::fs::remove_dir_all(&data);
    }
}

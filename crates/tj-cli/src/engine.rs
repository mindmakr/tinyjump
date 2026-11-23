//! Owns the runtime on its own thread and carries out its outputs:
//! replies, HTTP effects (worker pool), realtime broadcasts, logs, timers and the audit log.

use crate::audit::{Actor, Audit, Entry};
use crate::compat::{self, Snapshot};
use crate::compliance::{Pack, Server as ComplianceServer};
use crate::identity::sha256_hex;
use crate::mailbox::{Event, Mailbox};
use crate::manifest::Manifest;
use crate::plugins::Plugins;
use crate::remotes::{strict_agent, RemoteHub, Remotes, SyncSink};
use crate::stats::Stats;
use crate::store::OverlayStore;
use crate::test_runner;
use crate::webhooks::{WebhookSender, Webhooks};
use serde_json::{json, Value as Json};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tj_core::runtime::protocol::{Caller, Effect};
use tj_core::{Input, Options, Output, Runtime};

const EFFECT_WORKERS: usize = 4;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_SMOKE_TRACES: usize = 16;
const MAX_WATCH_SECS: u64 = 3600;
/// `1` lets `tj serve` fetch private, loopback and link-local addresses.
pub const PRIVATE_FETCH_ENV: &str = "TJ_ALLOW_PRIVATE_FETCH";
/// Optional comma-separated outbound fetch host allowlist (e.g. `api.stripe.com,*.example.com`).
pub const FETCH_ALLOW_ENV: &str = "TJ_FETCH_ALLOW";
/// Per-object serialized byte cap (`TJ_MAX_OBJECT_BYTES`, default 256 KB; 0 disables).
pub const MAX_OBJECT_BYTES_ENV: &str = "TJ_MAX_OBJECT_BYTES";
pub const DEFAULT_MAX_OBJECT_BYTES: usize = 256 * 1024;

/// Compliance pack + server posture checked at startup and on every live reload.
pub type ComplianceGate = (Pack, ComplianceServer);

/// Options for a validated shadow-boot reload (`POST /_tj/reload`).
pub struct ReloadRequest {
    pub source: String,
    pub allow_breaking: bool,
    pub smoke_traces: Vec<String>,
    pub watch_secs: u64,
    pub error_threshold: u64,
    pub actor: Actor,
}

/// A refused or failed request: its HTTP status and message.
pub type Failure = (u16, String);
/// A call failure that may also carry the field name from `check(ok, field, message)`.
pub type CallFailure = (u16, String, Option<String>);

type Reply = Sender<Result<Json, CallFailure>>;

/// Remote requests may load at most this many objects at once (`TJ_MAX_OBJECTS`).
const MAX_OBJECTS: usize = 100_000;

enum Msg {
    /// An input, who waits for its reply, and when it was sent (for latency).
    Input(Input, Option<Reply>, Instant),
    /// `(generation, effect_id, result)`: mismatched generations from a pre-reload runtime are dropped.
    /// Error tuple carries `(message, field, unreachable)` so transient transport failures return 503
    /// without caching a permanent 400 `CallRecord`, and remote `check(ok, field, msg)` preserves `field`.
    EffectDone(u64, u32, Result<Json, (String, Option<String>, bool)>),
    /// `caller: None` is the admin: every object, unchecked.
    Subscribe { events: Arc<Mailbox>, objects: Vec<String>, since: u64, caller: Option<Caller>, done: Sender<Result<(), Failure>> },
    /// Which of `objects` `caller` may read (for stream tickets).
    MayRead { objects: Vec<String>, caller: Caller, done: Sender<Result<Vec<String>, Failure>> },
    /// The core's statistics (`Runtime::stats`).
    Stats(Sender<Json>),
    /// Export (`erase: false`) or erase everything held about a person.
    Privacy { subject: String, erase: bool, done: Sender<Result<Json, Failure>> },
    /// Fast file-watcher reload used by `tj dev` (optionally updating resolved device permissions).
    Reload(String, Option<std::collections::BTreeMap<String, crate::device_perms::DevicePermission>>),
    /// Shadow-booted, compatibility- and invariant-checked live reload (`POST /_tj/reload`).
    SafeReload { req: ReloadRequest, done: Sender<Result<Json, Failure>> },
    /// Roll back to the previous known-good bundle (`POST /_tj/rollback`).
    Rollback { actor: Actor, done: Sender<Result<Json, Failure>> },
}

#[derive(Clone)]
pub struct Engine {
    tx: Sender<Msg>,
    stats: Arc<Stats>,
    manifest: Arc<Mutex<Manifest>>,
    source: Arc<Mutex<String>>,
    max_objects: usize,
    max_object_bytes: usize,
    fetch_allowlist: bool,
    device_plugins: Arc<Mutex<Vec<&'static str>>>,
    video_call: Arc<AtomicBool>,
    gfx: Arc<AtomicBool>,
    has_frame: Arc<AtomicBool>,
}

pub fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

pub(crate) fn max_object_bytes_from_env() -> usize {
    std::env::var(MAX_OBJECT_BYTES_ENV).ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_MAX_OBJECT_BYTES)
}

pub(crate) fn parse_fetch_allow(spec: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for raw in spec.split(',') {
        let mut s = raw.trim().to_ascii_lowercase();
        if s.is_empty() {
            continue;
        }
        if let Some(inner) = s.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
            s = inner.to_string();
        }
        let s = s.trim_end_matches('.').to_string();
        let invalid_chars = s.is_empty()
            || s.contains("://")
            || s.contains('/')
            || s.contains('?')
            || s.contains('#')
            || s.chars().any(|c| c.is_whitespace() || c.is_control())
            || !s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'*'));
        // Reject `host:port` (unless the whole entry is a valid bare IPv6 address).
        let has_port = s.contains(':') && s.parse::<std::net::Ipv6Addr>().is_err();
        let bad_wild = s.starts_with('*') && !s.strip_prefix("*.").is_some_and(|d| !d.is_empty() && !d.contains('*'));
        if invalid_chars || has_port || bad_wild || (!s.starts_with("*.") && s.contains('*')) {
            return Err(format!("{FETCH_ALLOW_ENV}: invalid host entry {raw:?} (use bare hostnames like `api.example.com`, `*.example.com`, or an IP address)"));
        }
        out.push(s);
    }
    if out.is_empty() {
        return Err(format!("{FETCH_ALLOW_ENV} is set but contains no hosts (unset it to allow any public host)"));
    }
    Ok(out)
}

pub(crate) fn fetch_allow_from_env() -> Result<Vec<String>, String> {
    match std::env::var(FETCH_ALLOW_ENV) {
        Ok(s) if !s.is_empty() => parse_fetch_allow(&s),
        _ => Ok(Vec::new()),
    }
}

struct Booted {
    rt: Runtime,
    out: Vec<Output>,
    manifest: Manifest,
    snap: Snapshot,
    max_objects: usize,
    max_object_bytes: usize,
    video_call: bool,
    gfx: bool,
    has_frame: bool,
}

/// Compiles and boots `source`, rendering compile errors for a terminal. `explain` says why
/// requests are refused (`tj dev` only).
fn boot(
    source: &str,
    data: &Path,
    render: bool,
    explain: bool,
    configured_plugins: &[&str],
    remote_classes: &[String],
    device_perms: &std::collections::BTreeMap<String, crate::device_perms::DevicePermission>,
) -> Result<Booted, String> {
    let program = tj_core::load(source).map_err(|errs| errs.iter().map(|d| d.render("app.tj", source)).collect::<Vec<_>>().join("\n\n"))?;
    let video_call = crate::device_perms::uses_video_call(&program, device_perms);
    let has_frame = program.has_frame();
    let gfx = program.uses_scene() || has_frame;
    let mut manifest = Manifest::build(source, &program, configured_plugins);
    manifest.with_device_permissions(device_perms);
    let snap = Snapshot::build(source, &program);
    // Objects that belong to a person get keys of their own, so erasing them reaches backups.
    // Remote replicas are owned by another server and skipped by local erasure (C-050.13).
    let personal: Vec<String> = program
        .objects
        .iter()
        .filter(|d| d.belongs_to_a_person() && !remote_classes.iter().any(|r| r == &*d.name))
        .map(|d| d.name.to_string())
        .collect();
    let (store, _) = crate::sealed::open_store(data, &personal)?;
    let seed = crate::bytes::random::<8>()?;
    let max_objects = std::env::var("TJ_MAX_OBJECTS").ok().and_then(|v| v.parse().ok()).unwrap_or(MAX_OBJECTS);
    let max_object_bytes = max_object_bytes_from_env();
    let opts = Options {
        now: now_ms(),
        seed: u64::from_le_bytes(seed),
        render,
        server: true,
        explain,
        max_objects,
        max_object_bytes,
        remote_classes: remote_classes.to_vec(),
    };
    let (mut rt, mut out) = Runtime::boot(program, store, opts).map_err(|e| format!("boot failed: {e}"))?;
    // Secure random bytes for `crypto.randomId()` and ids, as every host sends them.
    out.extend(rt.handle(now_ms(), Input::Entropy { bytes: crate::bytes::hex(&crate::bytes::random::<32>()?) }));
    Ok(Booted { rt, out, manifest, snap, max_objects, max_object_bytes, video_call, gfx, has_frame })
}

struct EngineSyncSink(Sender<Msg>);

impl SyncSink for EngineSyncSink {
    fn on_sync(&self, object: String, state: Json, seq: u64) {
        let _ = self.0.send(Msg::Input(Input::Sync { object, state, seq: Some(seq) }, None, Instant::now()));
    }
    fn on_online(&self) {
        let _ = self.0.send(Msg::Input(Input::Online, None, Instant::now()));
    }
}

impl Engine {
    /// Starts the runtime thread; fails if the app does not compile or boot.
    #[cfg(test)]
    pub fn start(source: String, data: PathBuf, render: bool, explain: bool, plugins: Arc<Plugins>, audit: Audit) -> Result<Engine, String> {
        Self::start_with_integrations(source, data, render, explain, plugins, audit, None, Arc::default(), Arc::default(), std::collections::BTreeMap::new())
    }

    /// Starts the runtime thread with compliance, server-to-server remotes, webhooks, and device permissions.
    #[allow(clippy::too_many_arguments)]
    pub fn start_with_integrations(
        source: String,
        data: PathBuf,
        render: bool,
        explain: bool,
        plugins: Arc<Plugins>,
        audit: Audit,
        compliance: Option<ComplianceGate>,
        remotes: Arc<Remotes>,
        webhooks: Arc<Webhooks>,
        device_perms: std::collections::BTreeMap<String, crate::device_perms::DevicePermission>,
    ) -> Result<Engine, String> {
        let (tx, rx) = channel();
        let (ready_tx, ready_rx) = channel();
        let back = tx.clone();
        let stats = Arc::new(Stats::default());
        let measured = stats.clone();
        let plugin_names: Vec<String> = plugins.names().into_iter().map(str::to_string).collect();
        let shared_slot: Arc<Mutex<Option<Arc<Mutex<Manifest>>>>> = Arc::new(Mutex::new(None));
        let shared_ref = shared_slot.clone();
        let live_source = Arc::new(Mutex::new(source.clone()));
        let live_source_ref = live_source.clone();
        let fetch_allow = fetch_allow_from_env()?;
        let fetch_allowlist = !fetch_allow.is_empty();
        let device_plugins = Arc::new(Mutex::new(crate::device_perms::bundled_host_plugins(&device_perms)));
        let device_plugins_ref = device_plugins.clone();
        let video_call = Arc::new(AtomicBool::new(false));
        let video_call_ref = video_call.clone();
        let gfx = Arc::new(AtomicBool::new(false));
        let gfx_ref = gfx.clone();
        let has_frame = Arc::new(AtomicBool::new(false));
        let has_frame_ref = has_frame.clone();
        let remote_classes = remotes.classes();
        let private_fetch = explain || std::env::var(PRIVATE_FETCH_ENV).is_ok_and(|v| v == "1");
        let fetch_allow_arc = Arc::new(fetch_allow);
        let webhook_sender = WebhookSender::start(webhooks.outgoing.clone(), &data, audit.clone(), private_fetch, fetch_allow_arc.clone())?;
        thread::Builder::new()
            .name("tj-runtime".into())
            .spawn(move || {
                let refs: Vec<&str> = plugin_names.iter().map(String::as_str).collect();
                match boot(&source, &data, render, explain, &refs, &remote_classes, &device_perms) {
                    Ok(Booted { rt, out, manifest, snap, max_objects, max_object_bytes, video_call: has_vc, gfx: has_gfx, has_frame: hf }) => {
                        video_call_ref.store(has_vc, Ordering::Relaxed);
                        gfx_ref.store(has_gfx, Ordering::Relaxed);
                        has_frame_ref.store(hf, Ordering::Relaxed);
                        let shared = Arc::new(Mutex::new(manifest));
                        if let Ok(mut r) = shared_ref.lock() {
                            *r = Some(shared.clone());
                        }
                        let _ = ready_tx.send(Ok((max_objects, max_object_bytes)));
                        audit.record(Entry::new("start").text("app", &sha256_hex(&source)));
                        run(rt, out, rx, back, data, render, explain, plugins, measured, audit, shared, live_source_ref, source, snap, compliance, fetch_allow_arc, remotes, webhooks, webhook_sender, device_perms, device_plugins_ref, video_call_ref, gfx_ref, has_frame_ref);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let (max_objects, max_object_bytes) = ready_rx.recv().map_err(|_| "runtime thread died".to_string())??;
        let manifest = shared_slot
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .unwrap_or_else(|| Arc::new(Mutex::new(Manifest { json: Json::Null })));
        Ok(Engine { tx, stats, manifest, source: live_source, max_objects, max_object_bytes, fetch_allowlist, device_plugins, video_call, gfx, has_frame })
    }

    /// Opt-in host device plugins (`files`, `notifications`, `location`, `camera`) enabled by the app's permissions.
    pub fn device_plugins(&self) -> Vec<&'static str> {
        self.device_plugins.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Whether the app opts into the WebRTC `<Native name="VideoCall" />` component.
    pub fn has_video_call(&self) -> bool {
        self.video_call.load(Ordering::Relaxed)
    }

    /// Whether the app opts into the 3D `<Scene>` renderer or `@frame` animation loop.
    pub fn has_gfx(&self) -> bool {
        self.gfx.load(Ordering::Relaxed)
    }

    /// Whether the app declares any `@frame` animation hook.
    pub fn has_frame(&self) -> bool {
        self.has_frame.load(Ordering::Relaxed)
    }

    /// Sends an input and waits for its reply, if the input kind has one.
    pub fn request(&self, input: Input) -> Result<Json, Failure> {
        self.request_with_field(input).map_err(|(status, msg, _)| (status, msg))
    }

    /// Like [`Engine::request`], keeping any `field` from `check(ok, field, message)`.
    pub fn request_with_field(&self, input: Input) -> Result<Json, CallFailure> {
        let stopped = || (503, "runtime stopped".to_string(), None);
        if !has_reply(&input) {
            self.tx.send(Msg::Input(input, None, Instant::now())).map_err(|_| stopped())?;
            return Ok(Json::Null);
        }
        let (t, r) = channel();
        self.tx.send(Msg::Input(input, Some(t), Instant::now())).map_err(|_| stopped())?;
        Self::wait(&r).map_err(|(s, m)| (s, m, None))?
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
    /// Also returns the listed objects the caller may not read, for the audit log.
    pub fn subscribe(&self, objects: Vec<String>, since: u64, caller: Option<Caller>) -> Result<(Arc<Mailbox>, Vec<String>), Failure> {
        let reader = caller.clone().filter(|c| matches!(c, Caller::Remote { .. }));
        // Only what the caller may read goes into the mailbox, so no change to anything else
        // can reach it. Objects refused are named, so the device stops asking for them.
        let readable = match &caller {
            Some(c) => self.may_read(objects.clone(), c.clone())?,
            None => objects.clone(),
        };
        // The admin's stream with no list is every object; any other is exactly what it may read.
        let only = (caller.is_some() || !objects.is_empty()).then(|| readable.clone());
        let events = Arc::new(Mailbox::new(only, Some(self.stats.clone())).read_by(reader));
        let refused: Vec<String> = objects.iter().filter(|o| !readable.contains(o)).cloned().collect();
        if !refused.is_empty() {
            events.push(Event::new(String::new(), None, format!("event: refused\ndata: {}\n\n", json!({ "objects": refused }))));
        }
        let (done, r) = channel();
        self.tx.send(Msg::Subscribe { events: events.clone(), objects: readable, since, caller, done }).map_err(|_| (503, "runtime stopped".to_string()))?;
        Self::wait(&r)??;
        Ok((events, refused))
    }

    /// The `objects` that `caller` may read (see [`may_read`]).
    pub fn may_read(&self, objects: Vec<String>, caller: Caller) -> Result<Vec<String>, Failure> {
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

    /// The app's canonical manifest enriched with live health counters and active leases.
    pub fn manifest(&self, dev: bool, audit_dropped: u64, leases: Vec<Json>) -> Result<Json, Failure> {
        let core = self.core_stats()?;
        let health = self.stats.health(&core, dev, self.max_objects, self.max_object_bytes, self.fetch_allowlist, audit_dropped, leases.len());
        let manifest = self.manifest.lock().map_err(|_| (500, "manifest unavailable".to_string()))?;
        Ok(manifest.with_live(health, Json::Array(leases)))
    }

    /// Re-boots with new source (`tj dev` file watcher). Objects keep their stored state.
    pub fn reload(&self, source: String) {
        let _ = self.tx.send(Msg::Reload(source, None));
    }

    /// Re-boots with new source and refreshed device permissions (`tj dev` file watcher).
    pub fn reload_with_device_perms(
        &self,
        source: String,
        device_perms: std::collections::BTreeMap<String, crate::device_perms::DevicePermission>,
    ) {
        let _ = self.tx.send(Msg::Reload(source, Some(device_perms)));
    }

    /// Validated shadow-boot live reload (`POST /_tj/reload`): checks compatibility, boots on a
    /// copy-on-write overlay, validates `@invariant`s and smoke traces, drains in-flight calls,
    /// swaps cleanly and arms a health-gated auto-rollback watch window.
    pub fn safe_reload(&self, req: ReloadRequest) -> Result<Json, Failure> {
        let (done, r) = channel();
        self.tx.send(Msg::SafeReload { req, done }).map_err(|_| (503, "runtime stopped".to_string()))?;
        Self::wait(&r)?
    }

    /// Rolls back to the previous known-good app bundle (`POST /_tj/rollback`).
    pub fn rollback(&self, actor: Actor) -> Result<Json, Failure> {
        let (done, r) = channel();
        self.tx.send(Msg::Rollback { actor, done }).map_err(|_| (503, "runtime stopped".to_string()))?;
        Self::wait(&r)?
    }

    /// The currently running app bundle source (for `tj serve --web` `/app.tj`).
    pub fn current_source(&self) -> String {
        self.source.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

fn has_reply(input: &Input) -> bool {
    matches!(input, Input::Call { .. } | Input::Get { .. } | Input::Ui { .. } | Input::Inspect { .. })
}

fn set_reply(input: &mut Input, id: u64) {
    match input {
        Input::Call { reply, .. } | Input::Get { reply, .. } | Input::Ui { reply, .. } | Input::Inspect { reply, .. } => *reply = Some(id),
        Input::EffectDone { .. } | Input::Sync { .. } | Input::Layout { .. } | Input::Online | Input::Identity { .. } | Input::Entropy { .. } | Input::Locale { .. } | Input::Frame { .. } | Input::Tick => {}
    }
}

struct Dispatch {
    replies: HashMap<u64, Waiter>,
    next_reply: u64,
    subscribers: Vec<Arc<Mailbox>>,
    effects: Sender<(u64, u32, Effect)>,
    stats: Arc<Stats>,
    audit: Audit,
    /// `tj:privacy` calls to carry out on the runtime thread.
    privacy: Vec<(u32, String, Json)>,
    /// People erased in this turn, forgotten by the audit log after its entries are written.
    forget_after: Vec<String>,
    /// Uncaught `Output::Error` and `503` replies emitted in the current turn (for auto-rollback).
    turn_errors: u64,
    /// Monotonic runtime generation: incremented on every swap so stale `EffectDone`s are ignored.
    generation: u64,
    /// Persistent SSE subscriptions to remote tinyjump servers (`remotes.json`).
    remote_hub: Option<RemoteHub>,
    /// Durable outgoing webhook delivery worker (`webhooks.json`).
    webhooks: WebhookSender,
}

/// Post-reload health watch window: if uncaught errors or 503s reach `threshold` before `until`,
/// the runtime automatically restores `prev_source`.
struct WatchWindow {
    until: Instant,
    errors: u64,
    threshold: u64,
    prev_source: String,
    prev_sha: String,
}

/// Validates `req.source` offline against `current_snap`, `compliance`, and a copy-on-write
/// overlay over `data` (checking every stored object's `@invariant` and running `smoke_traces`).
#[allow(clippy::too_many_arguments)]
fn shadow_validate(
    source: &str,
    allow_breaking: bool,
    smoke_traces: &[String],
    current_snap: &Snapshot,
    data: &Path,
    explain: bool,
    compliance: Option<&ComplianceGate>,
    remotes: &Remotes,
    webhooks: &Webhooks,
) -> Result<(Vec<compat::Finding>, Snapshot), Failure> {
    if smoke_traces.len() > MAX_SMOKE_TRACES {
        return Err((400, format!("at most {MAX_SMOKE_TRACES} smoke traces per reload")));
    }
    let is_bytecode = source.starts_with(tj_core::vm::bytecode::BUNDLE_PREFIX);
    let program = tj_core::load(source)
        .map_err(|errs| (400, errs.iter().map(|d| d.render("app.tj", source)).collect::<Vec<_>>().join("\n\n")))?;
    let remote_classes = remotes.classes();
    let remote_dests = remotes.destinations();
    if is_bytecode && compliance.is_some() {
        return Err((400, "reloading a precompiled .tjb bundle under --compliance is refused; send .tj source for static compliance/privacy-flow checks".into()));
    }
    if is_bytecode && !remote_dests.is_empty() && program.objects.iter().any(|o| !o.pii.is_empty()) {
        return Err((400, "reloading a precompiled .tjb bundle with both @pii and remotes.json is refused; send .tj source for static remote-flow checks".into()));
    }
    let flow_errs = if is_bytecode { Vec::new() } else { tj_core::check_remote_flows(source, &remote_dests) };
    if !flow_errs.is_empty() {
        return Err((400, flow_errs.iter().map(|d| d.render("app.tj", source)).collect::<Vec<_>>().join("\n\n")));
    }
    webhooks.validate_against(&program, &remote_classes).map_err(|e| (409, e))?;
    if let Some((pack, server)) = compliance {
        let facts = tj_core::facts_with_remotes(source, &remote_dests);
        let wh_transfers = webhooks.pii_transfers(&program, &facts.reveals);
        let (report, ok) = pack.report(&pack.evaluate_with_transfers(source, &program, Some(server), &remote_dests, &wh_transfers));
        if !ok {
            return Err((409, format!("compliance pack {} not met:\n{report}", pack.title)));
        }
    }
    let new_snap = Snapshot::build(source, &program);
    let findings = compat::check_snapshots(current_snap, &new_snap);
    if !allow_breaking && compat::has_breaking(&findings) {
        return Err((409, format!("breaking upgrade changes found; pass allowBreaking: true if intentional\n{}", compat::report(&findings))));
    }
    // Shadow boot against a copy-on-write overlay over the real data dir (never mutates disk).
    let personal: Vec<String> = program
        .objects
        .iter()
        .filter(|d| d.belongs_to_a_person() && !remote_classes.iter().any(|r| r == &*d.name))
        .map(|d| d.name.to_string())
        .collect();
    let (base, _) = crate::sealed::open_store(data, &personal).map_err(|e| (503, e))?;
    let overlay = OverlayStore::new(base);
    let seed = crate::bytes::random::<8>().map_err(|e| (500, e))?;
    let max_objects = std::env::var("TJ_MAX_OBJECTS").ok().and_then(|v| v.parse().ok()).unwrap_or(MAX_OBJECTS);
    let max_object_bytes = max_object_bytes_from_env();
    let opts = Options {
        now: now_ms(),
        seed: u64::from_le_bytes(seed),
        render: false,
        server: true,
        explain,
        max_objects,
        max_object_bytes,
        remote_classes: remote_classes.to_vec(),
    };
    let (mut shadow_rt, boot_out) = Runtime::boot(program, Box::new(overlay), opts).map_err(|e| (422, format!("shadow boot failed: {e}")))?;
    let entropy = crate::bytes::hex(&crate::bytes::random::<32>().map_err(|e| (500, e))?);
    let entropy_out = shadow_rt.handle(now_ms(), Input::Entropy { bytes: entropy });
    for o in boot_out.into_iter().chain(entropy_out) {
        if let Output::Error { message, .. } = o {
            return Err((422, format!("shadow boot produced an error: {message}")));
        }
    }
    shadow_rt.check_invariants().map_err(|e| (422, format!("shadow boot invariant check failed: {e}")))?;
    for (idx, trace_text) in smoke_traces.iter().enumerate() {
        let (base, _) = crate::sealed::open_store(data, &personal).map_err(|e| (503, e))?;
        let overlay = OverlayStore::new(base);
        let res = test_runner::run_trace_on_store(source, &format!("smoke-{}", idx + 1), trace_text, HashMap::new(), Box::new(overlay), now_ms(), max_object_bytes);
        if !res.passed() {
            return Err((422, format!("shadow smoke trace #{} failed: {}", idx + 1, res.error.unwrap_or_default())));
        }
    }
    Ok((findings, new_snap))
}

/// Briefly drains in-flight `d.replies` (up to `DRAIN_TIMEOUT`) while buffering arriving
/// non-effect messages so no requests are dropped across a live reload or rollback.
fn drain_inflight(rt: &mut Runtime, d: &mut Dispatch, rx: &Receiver<Msg>, pending: &mut VecDeque<Msg>) {
    let deadline = Instant::now() + DRAIN_TIMEOUT;
    while !d.replies.is_empty() {
        let Some(rem) = deadline.checked_duration_since(Instant::now()) else { break };
        match rx.recv_timeout(rem) {
            Ok(Msg::EffectDone(gen, id, result)) => {
                if gen != d.generation {
                    continue;
                }
                let (ok, error, field, unreachable) = match result {
                    Ok(v) => (Some(v), None, None, false),
                    Err((e, f, u)) => (None, Some(e), f, u),
                };
                let out = rt.handle(now_ms(), Input::EffectDone { id, ok, error, field, unreachable });
                d.outputs(rt, out);
                privacy_effects(rt, d);
            }
            Ok(other) => pending.push_back(other),
            Err(_) => break,
        }
    }
}

/// Who waits for a reply; for calls, the method and when the call was sent; for reads of
/// personal data, what to write to the audit log once it was read; for a person's calls,
/// the audit entry written when it is answered.
struct Waiter {
    reply: Reply,
    timed: Option<(String, Instant)>,
    read: Option<(String, Caller)>,
    call: Option<(String, String, Caller, Instant)>,
}

/// Whether `caller` may read every one of `objects`, checked on the runtime thread.
/// The `objects` that `caller` may read. Refusals are not a failure: a device still listing
/// objects from before its user changed gets a stream of what it may read (perhaps nothing)
/// that names the rest, so it stops asking instead of retrying a 403 for ever.
fn may_read(rt: &Runtime, objects: &[String], caller: &Caller) -> Result<Vec<String>, Failure> {
    Ok(objects.iter().filter(|o| rt.may_read(o, caller).is_ok()).cloned().collect())
}

#[allow(clippy::too_many_arguments)]
fn run(
    mut rt: Runtime,
    boot_out: Vec<Output>,
    rx: Receiver<Msg>,
    back: Sender<Msg>,
    data: PathBuf,
    render: bool,
    explain: bool,
    plugins: Arc<Plugins>,
    stats: Arc<Stats>,
    audit: Audit,
    manifest: Arc<Mutex<Manifest>>,
    live_source: Arc<Mutex<String>>,
    mut current_source: String,
    mut current_snap: Snapshot,
    compliance: Option<ComplianceGate>,
    fetch_allow: Arc<Vec<String>>,
    remotes: Arc<Remotes>,
    webhook_cfg: Arc<Webhooks>,
    webhooks: WebhookSender,
    mut device_perms: std::collections::BTreeMap<String, crate::device_perms::DevicePermission>,
    device_plugins: Arc<Mutex<Vec<&'static str>>>,
    video_call: Arc<AtomicBool>,
    gfx: Arc<AtomicBool>,
    has_frame: Arc<AtomicBool>,
) {
    // `tj dev` (which explains refusals) reaches local services; a server only public ones.
    let private_fetch = explain || std::env::var(PRIVATE_FETCH_ENV).is_ok_and(|v| v == "1");
    let plugin_names: Vec<String> = plugins.names().into_iter().map(str::to_string).collect();
    let remote_classes = remotes.classes();
    let remote_hub = (!remotes.is_empty()).then(|| RemoteHub::new(remotes.clone(), Arc::new(EngineSyncSink(back.clone())), private_fetch, fetch_allow.clone()));
    let effects = effect_pool(back, plugins, stats.clone(), audit.clone(), private_fetch, fetch_allow, remotes.clone());
    let mut d = Dispatch {
        replies: HashMap::new(),
        next_reply: 0,
        subscribers: Vec::new(),
        effects,
        stats,
        audit,
        privacy: Vec::new(),
        forget_after: Vec::new(),
        turn_errors: 0,
        generation: 1,
        remote_hub,
        webhooks,
    };
    let mut current_sha = sha256_hex(&current_source);
    let mut previous: Option<(String, String)> = None;
    let mut watch: Option<WatchWindow> = None;
    let mut pending: VecDeque<Msg> = VecDeque::new();
    d.outputs(&mut rt, boot_out);
    loop {
        let msg = if let Some(m) = pending.pop_front() {
            Ok(m)
        } else {
            match rt.next_wake() {
                Some(at) => rx.recv_timeout(Duration::from_millis((at - now_ms()).max(0.0) as u64)),
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            }
        };
        d.turn_errors = 0;
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
                    let call = match &input {
                        Input::Call { object, method, caller: Some(c @ Caller::Remote { .. }), .. } => Some((object.clone(), method.clone(), c.clone(), sent)),
                        _ => None,
                    };
                    d.next_reply += 1;
                    d.replies.insert(d.next_reply, Waiter { reply, timed, read, call });
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
                match rt.erase_subject(&subject, None) {
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
            Ok(Msg::EffectDone(gen, id, result)) => {
                if gen != d.generation {
                    continue;
                }
                let (ok, error, field, unreachable) = match result {
                    Ok(v) => (Some(v), None, None, false),
                    Err((e, f, u)) => (None, Some(e), f, u),
                };
                rt.handle(now_ms(), Input::EffectDone { id, ok, error, field, unreachable })
            }
            Ok(Msg::MayRead { objects, caller, done }) => {
                let _ = done.send(may_read(&rt, &objects, &caller));
                continue;
            }
            Ok(Msg::Subscribe { events, objects, since, caller, done }) => {
                // Checked again here, in turn with every change: `objects` were all readable
                // when `subscribe` asked, and a rule may have changed since.
                if let Some(Err(refused)) = caller.as_ref().map(|c| may_read(&rt, &objects, c).and_then(|ok| if ok.len() == objects.len() { Ok(ok) } else { Err((403, "access changed".into())) })) {
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
                        let hidden = rt.hidden_from(&object, &state, events.reader());
                        events.push(changed_event(&object, &rt.without(&object, &state, &hidden), seq));
                    }
                }
                d.subscribers.push(events);
                continue;
            }
            Ok(Msg::Reload(source, updated_perms)) => {
                let next_perms = updated_perms.as_ref().unwrap_or(&device_perms);
                let refs: Vec<&str> = plugin_names.iter().map(String::as_str).collect();
                match boot(&source, &data, render, explain, &refs, &remote_classes, next_perms) {
                    Ok(Booted { rt: new_rt, out, manifest: new_manifest, snap: new_snap, video_call: has_vc, gfx: has_gfx, has_frame: hf, .. }) => {
                        if let Some(p) = updated_perms {
                            if let Ok(mut dp) = device_plugins.lock() {
                                *dp = crate::device_perms::bundled_host_plugins(&p);
                            }
                            device_perms = p;
                        }
                        video_call.store(has_vc, Ordering::Relaxed);
                        gfx.store(has_gfx, Ordering::Relaxed);
                        has_frame.store(hf, Ordering::Relaxed);
                        let new_sha = sha256_hex(&source);
                        previous = Some((current_source, current_sha));
                        current_source = source.clone();
                        current_sha = new_sha.clone();
                        current_snap = new_snap;
                        if let Ok(mut s) = live_source.lock() {
                            *s = source;
                        }
                        rt = new_rt;
                        d.on_swap(&rt);
                        if let Ok(mut m) = manifest.lock() {
                            *m = new_manifest;
                        }
                        watch = None;
                        d.stats.set_watch_until(0);
                        d.stats.reset_methods();
                        d.stats.reloaded();
                        d.audit.record(Entry::new("start").text("app", &new_sha));
                        eprintln!("[tj] reloaded");
                        out
                    }
                    Err(e) => {
                        eprintln!("[tj] reload failed, keeping the running version:\n{e}");
                        continue;
                    }
                }
            }
            Ok(Msg::SafeReload { req, done }) => {
                let new_sha = sha256_hex(&req.source);
                // Audit BEFORE draining or shadow_validate decrypts live data (fail-closed).
                if let Err(e) = d.audit.security(Entry::new("admin").who(req.actor).text("request", "reload").text("app", &new_sha)) {
                    let _ = done.send(Err(e));
                    continue;
                }
                // Drain in-flight effects BEFORE shadow_validate so all pending writes are on disk
                // and validated against the new bundle's `@invariant`s and smoke traces (T1-5).
                drain_inflight(&mut rt, &mut d, &rx, &mut pending);
                let (findings, new_snap) = match shadow_validate(&req.source, req.allow_breaking, &req.smoke_traces, &current_snap, &data, explain, compliance.as_ref(), &remotes, &webhook_cfg) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = done.send(Err(e));
                        continue;
                    }
                };
                d.turn_errors = 0;
                let refs: Vec<&str> = plugin_names.iter().map(String::as_str).collect();
                match boot(&req.source, &data, render, explain, &refs, &remote_classes, &device_perms) {
                    Ok(Booted { rt: new_rt, out, manifest: new_manifest, video_call: has_vc, gfx: has_gfx, has_frame: hf, .. }) => {
                        let prev_source = std::mem::replace(&mut current_source, req.source.clone());
                        let prev_sha = std::mem::replace(&mut current_sha, new_sha.clone());
                        previous = Some((prev_source.clone(), prev_sha.clone()));
                        current_snap = new_snap;
                        if let Ok(mut s) = live_source.lock() {
                            *s = req.source;
                        }
                        video_call.store(has_vc, Ordering::Relaxed);
                        gfx.store(has_gfx, Ordering::Relaxed);
                        has_frame.store(hf, Ordering::Relaxed);
                        rt = new_rt;
                        d.on_swap(&rt);
                        if let Ok(mut m) = manifest.lock() {
                            *m = new_manifest;
                        }
                        d.stats.reset_methods();
                        d.stats.reloaded();
                        let watch_secs = if req.error_threshold > 0 { req.watch_secs.min(MAX_WATCH_SECS) } else { 0 };
                        if watch_secs > 0 {
                            let until_ms = (now_ms() as u64).saturating_add(watch_secs * 1000);
                            d.stats.set_watch_until(until_ms);
                            watch = Some(WatchWindow {
                                until: Instant::now() + Duration::from_secs(watch_secs),
                                errors: 0,
                                threshold: req.error_threshold,
                                prev_source,
                                prev_sha: prev_sha.clone(),
                            });
                        } else {
                            watch = None;
                            d.stats.set_watch_until(0);
                        }
                        let warnings: Vec<Json> = findings.iter().map(compat::Finding::json).collect();
                        let _ = done.send(Ok(json!({
                            "appSha256": new_sha,
                            "previousSha256": prev_sha,
                            "findings": warnings,
                            "smokeTracesPassed": req.smoke_traces.len(),
                            "watchSecs": watch_secs,
                        })));
                        out
                    }
                    Err(e) => {
                        let _ = done.send(Err((503, format!("production swap boot failed; kept running version: {e}"))));
                        continue;
                    }
                }
            }
            Ok(Msg::Rollback { actor, done }) => {
                let Some((prev_source, prev_sha)) = previous.clone() else {
                    let _ = done.send(Err((409, "no previous app version is available to roll back to".into())));
                    continue;
                };
                if let Err(e) = d.audit.security(Entry::new("admin").who(actor).text("request", "rollback").text("app", &prev_sha)) {
                    let _ = done.send(Err(e));
                    continue;
                }
                drain_inflight(&mut rt, &mut d, &rx, &mut pending);
                d.turn_errors = 0;
                let refs: Vec<&str> = plugin_names.iter().map(String::as_str).collect();
                match boot(&prev_source, &data, render, explain, &refs, &remote_classes, &device_perms) {
                    Ok(Booted { rt: new_rt, out, manifest: new_manifest, snap: new_snap, video_call: has_vc, gfx: has_gfx, has_frame: hf, .. }) => {
                        let rolled_from_sha = std::mem::replace(&mut current_sha, prev_sha.clone());
                        current_source = prev_source.clone();
                        previous = None;
                        current_snap = new_snap;
                        if let Ok(mut s) = live_source.lock() {
                            *s = prev_source;
                        }
                        video_call.store(has_vc, Ordering::Relaxed);
                        gfx.store(has_gfx, Ordering::Relaxed);
                        has_frame.store(hf, Ordering::Relaxed);
                        rt = new_rt;
                        d.on_swap(&rt);
                        if let Ok(mut m) = manifest.lock() {
                            *m = new_manifest;
                        }
                        watch = None;
                        d.stats.set_watch_until(0);
                        d.stats.reset_methods();
                        let _ = done.send(Ok(json!({ "appSha256": prev_sha, "rolledBackFrom": rolled_from_sha })));
                        out
                    }
                    Err(e) => {
                        let _ = done.send(Err((503, format!("rollback boot failed: {e}"))));
                        continue;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => rt.handle(now_ms(), Input::Tick),
            Err(RecvTimeoutError::Disconnected) => return,
        };
        d.outputs(&mut rt, out);
        privacy_effects(&mut rt, &mut d);

        // Health-gated auto-rollback: if uncaught errors or 503s exceed the watch threshold
        // during the post-reload watch window, restore the previous known-good bundle.
        if let Some(w) = &mut watch {
            if Instant::now() >= w.until {
                watch = None;
                d.stats.set_watch_until(0);
            } else if d.turn_errors > 0 {
                w.errors += d.turn_errors;
                if w.errors >= w.threshold {
                    let w = watch.take().expect("checked");
                    d.stats.set_watch_until(0);
                    let refs: Vec<&str> = plugin_names.iter().map(String::as_str).collect();
                    match boot(&w.prev_source, &data, render, explain, &refs, &remote_classes, &device_perms) {
                        Ok(Booted { rt: prev_rt, out: boot_out, manifest: prev_manifest, snap: prev_snap, video_call: has_vc, gfx: has_gfx, has_frame: hf, .. }) => {
                            current_source = w.prev_source.clone();
                            current_sha = w.prev_sha.clone();
                            previous = None;
                            current_snap = prev_snap;
                            if let Ok(mut s) = live_source.lock() {
                                *s = w.prev_source;
                            }
                            video_call.store(has_vc, Ordering::Relaxed);
                            gfx.store(has_gfx, Ordering::Relaxed);
                            has_frame.store(hf, Ordering::Relaxed);
                            rt = prev_rt;
                            d.on_swap(&rt);
                            if let Ok(mut m) = manifest.lock() {
                                *m = prev_manifest;
                            }
                            d.stats.reset_methods();
                            d.stats.auto_rolled_back();
                            d.audit.record(Entry::new("admin").who(Actor::System).text("request", "auto-rollback").text("app", &w.prev_sha));
                            eprintln!("[tj] auto-rollback triggered ({} error(s) in watch window): restored {}", w.errors, &w.prev_sha[..w.prev_sha.len().min(12)]);
                            d.outputs(&mut rt, boot_out);
                            privacy_effects(&mut rt, &mut d);
                        }
                        Err(e) => eprintln!("[tj] auto-rollback boot failed: {e}"),
                    }
                }
            }
        }
    }
}

/// Carries out `tj:privacy` calls, as `tj privacy export|erase` does, and answers them. Then
/// the audit log forgets the people erased, after the entries of this turn.
fn privacy_effects(rt: &mut Runtime, d: &mut Dispatch) {
    while !d.privacy.is_empty() {
        for (id, method, args) in std::mem::take(&mut d.privacy) {
            let (ok, error) = match privacy_effect(rt, d, id, &method, &args) {
                Ok(v) => (Some(v), None),
                Err(e) => (None, Some(e)),
            };
            let out = rt.handle(now_ms(), Input::EffectDone { id, ok, error, field: None, unreachable: false });
            d.outputs(rt, out);
        }
    }
    // Not while a call of theirs is still running: what it does next is written under
    // their name, so they are forgotten after it ends.
    let running = |d: &Dispatch, rt: &Runtime, person: &str| rt.runs_for(person) || d.replies.values().any(|w| matches!(&w.call, Some((_, _, Caller::Remote { id, .. }, _)) if id == person));
    for person in std::mem::take(&mut d.forget_after) {
        if running(d, rt, &person) {
            d.forget_after.push(person);
        } else {
            d.audit.forget(&person);
        }
    }
}

/// One `tj:privacy` call: allowed for the person themselves, the `privacy` role and the
/// server's own code; audited.
fn privacy_effect(rt: &mut Runtime, d: &mut Dispatch, effect: u32, method: &str, args: &Json) -> Result<Json, String> {
    let person = args["id"].as_str().filter(|id| !id.is_empty()).ok_or("give the id of the person")?;
    let by = args["caller"]["id"].as_str().unwrap_or_default();
    let roles: Vec<&str> = args["caller"]["roles"].as_array().map(|r| r.iter().filter_map(Json::as_str).collect()).unwrap_or_default();
    let who = if roles.contains(&"system") { Actor::System } else { Actor::id(Some(by)) };
    if !((by == person) || roles.contains(&"privacy") || roles.contains(&"system")) {
        // A security event; without the person, since any id can be given.
        d.audit.record(Entry::new("refused").who(who).text("request", &format!("privacy {method}")).num("status", 403));
        return Err("only the person themselves, the privacy role or the server's own code may export or erase someone's data".into());
    }
    d.audit.record(Entry::new("privacy").who(who).text("request", method).person("subject", person));
    match method {
        "export" => rt.export_subject(person),
        "erase" => {
            let e = rt.erase_subject(person, Some(effect)).map_err(|(_, why)| why)?;
            d.outputs(rt, e.outputs);
            d.forget_after.push(person.to_string());
            let held: Vec<Json> = e.held.iter().map(|(object, until)| json!({ "object": object, "until": until })).collect();
            Ok(json!({ "erased": e.erased, "held": held }))
        }
        other => Err(format!("tj:privacy has no {other}")),
    }
}

impl Dispatch {
    /// Runs after every runtime swap: bumps `generation` so stale `EffectDone`s from the old
    /// runtime are ignored, clears uncompleted replies/privacy effects, and closes any SSE
    /// subscribers whose `@read` rules tightened under the new bundle.
    fn on_swap(&mut self, rt: &Runtime) {
        self.generation += 1;
        self.replies.clear();
        self.privacy.clear();
        self.turn_errors = 0;
        self.subscribers.retain(|s| {
            let ok = match (s.reader(), s.objects()) {
                (Some(caller), Some(objs)) => objs.iter().all(|o| rt.may_read(o, caller).is_ok()),
                (Some(_), None) => false,
                (None, _) => true,
            };
            if !ok {
                s.close();
            }
            ok
        });
    }

    fn outputs(&mut self, rt: &mut Runtime, out: Vec<Output>) {
        for o in out {
            match o {
                Output::Reply { reply, ok, error, field, status } => {
                    // Only internal VM/runtime faults (500) count toward auto-rollback; caller-triggerable
                    // `503` (max_objects / @exclusive wait), user `throw`/`check` (400), and `403` do not.
                    if status == Some(500) {
                        self.turn_errors += 1;
                    }
                    if let Some(w) = self.replies.remove(&reply) {
                        if let Some((name, sent)) = &w.timed {
                            self.stats.method(name, sent.elapsed(), error.is_none());
                        }
                        if let Some((object, caller)) = w.read.filter(|_| error.is_none()) {
                            self.audit.record(Entry::new("read").who(Actor::of(&caller)).object(&object));
                        }
                        // Written here, as it is answered, so a person erased by their own call
                        // is forgotten only after (refusals are written by the HTTP layer).
                        if let Some((object, method, caller, sent)) = w.call {
                            let status = if error.is_some() { status.unwrap_or(400) } else { 200 };
                            if !matches!(status, 401 | 403) {
                                self.audit.record(Entry::new("call").who(Actor::of(&caller)).object(&object).text("method", &method).num("status", status.into()).num("ms", sent.elapsed().as_millis() as u64));
                            }
                        }
                        let _ = w.reply.send(match error {
                            Some(e) => Err((status.unwrap_or(400), e, field)),
                            None => Ok(ok.unwrap_or(Json::Null)),
                        });
                    }
                }
                // tj:privacy acts on the runtime, so it runs on this thread (see `privacy_effects`).
                Output::Effect { id, effect: Effect::Plugin { plugin, method, args } } if plugin == "privacy" => self.privacy.push((id, method, args)),
                Output::Effect { id, effect } => {
                    let _ = self.effects.send((self.generation, id, effect));
                }
                Output::Emit { object, name, data } => {
                    if name != "read" {
                        for hook in self.webhooks.targets() {
                            if hook.matches(&object, "emit") && rt.may_read(&object, &hook.reader()).is_ok() {
                                self.webhooks.enqueue(&hook.name, &json!({ "event": "emit", "object": object, "name": name, "data": data }));
                            }
                        }
                    }
                    let text = format!("event: emit\ndata: {}\n\n", json!({ "object": object, "name": name, "data": data }));
                    // A slow client gets one `read` per object, not one per request.
                    let merge = (name == "read").then_some("read");
                    self.broadcast(Event::new(object, merge, text));
                }
                Output::Changed { object, state, seq } => {
                    for hook in self.webhooks.targets() {
                        if hook.matches(&object, "changed") {
                            let reader = hook.reader();
                            if rt.may_read(&object, &reader).is_ok() {
                                let hidden = rt.hidden_from(&object, &state, Some(&reader));
                                let masked = rt.without(&object, &state, &hidden);
                                self.webhooks.enqueue(&hook.name, &json!({ "event": "changed", "object": object, "seq": seq, "state": masked }));
                            }
                        }
                    }
                    // Each reader gets the fields their rules let them see: one event for
                    // each set of hidden fields, shared by every reader who has it.
                    let mut events: Vec<(Vec<usize>, Event)> = Vec::new();
                    self.subscribers.retain(|s| {
                        let hidden = rt.hidden_from(&object, &state, s.reader());
                        let e = match events.iter().find(|(h, _)| *h == hidden) {
                            Some((_, e)) => e.clone(),
                            None => {
                                let e = changed_event(&object, &rt.without(&object, &state, &hidden), seq);
                                events.push((hidden, e.clone()));
                                e
                            }
                        };
                        s.push(e)
                    });
                }
                Output::Log { level, message } => eprintln!("[app] {level}: {message}"),
                Output::Erased { object, reason, forget } => {
                    self.audit.record(Entry::new("erased").object(&object).text("reason", &reason));
                    if forget {
                        if let Some((_, person)) = object.split_once('/') {
                            self.audit.forget(person);
                        }
                    }
                }
                Output::Ran { object, method, by, caller, ok, ms } => {
                    let kind = match by.as_str() {
                        "timer" => "timer",
                        "alarm" => "alarm",
                        _ => "invoke",
                    };
                    self.audit.record(Entry::new(kind).who(Actor::id(caller.as_deref())).object(&object).text("method", &method).ok(ok).num("ms", ms));
                }
                Output::Error { message, trace } => {
                    self.turn_errors += 1;
                    eprintln!("[app] error: {message}");
                    for t in trace {
                        eprintln!("    {t}");
                    }
                }
                Output::Subscribe { object, seq } => {
                    if let Some(hub) = &self.remote_hub {
                        hub.subscribe(object, seq);
                    }
                }
                // Servers do not draw UI.
                Output::Patch { .. } | Output::Frames { .. } | Output::Wake { .. } => {}
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
fn effect_pool(
    back: Sender<Msg>,
    plugins: Arc<Plugins>,
    stats: Arc<Stats>,
    audit: Audit,
    private_fetch: bool,
    fetch_allow: Arc<Vec<String>>,
    remotes: Arc<Remotes>,
) -> Sender<(u64, u32, Effect)> {
    let (tx, rx) = channel::<(u64, u32, Effect)>();
    let rx = Arc::new(Mutex::new(rx));
    let mut agent = ureq::AgentBuilder::new().timeout(HTTP_TIMEOUT);
    if !private_fetch || !fetch_allow.is_empty() {
        // Checked where names become addresses, so redirects and names that point inside or
        // outside `TJ_FETCH_ALLOW` are caught too.
        let allow = fetch_allow.clone();
        agent = agent.resolver(move |netloc: &str| {
            if !allow.is_empty() && !netloc_allowed(netloc, &allow) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("outbound fetch host is not allowed by {FETCH_ALLOW_ENV}"),
                ));
            }
            if !private_fetch {
                public_only(netloc)
            } else {
                use std::net::ToSocketAddrs;
                Ok(netloc.to_socket_addrs()?.collect())
            }
        });
    }
    let agent = agent.build();
    let remote_agent = strict_agent(HTTP_TIMEOUT, private_fetch, fetch_allow.clone());
    for i in 0..EFFECT_WORKERS {
        let (rx, back, agent, remote_agent, plugins, stats, audit, fetch_allow, remotes) =
            (rx.clone(), back.clone(), agent.clone(), remote_agent.clone(), plugins.clone(), stats.clone(), audit.clone(), fetch_allow.clone(), remotes.clone());
        let spawned = thread::Builder::new().name(format!("tj-effects-{i}")).spawn(move || loop {
            let job = rx.lock().map_err(|_| ()).and_then(|r| r.recv().map_err(|_| ()));
            let Ok((gen, id, effect)) = job else { return };
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
            let result = perform(&agent, &remote_agent, &plugins, &fetch_allow, &remotes, effect);
            stats.effect(&kind, started.elapsed(), result.is_ok());
            if let Some((plugin, method)) = plugin_call {
                audit.record(Entry::new("plugin").text("plugin", &plugin).text("method", &method).ok(result.is_ok()).num("ms", started.elapsed().as_millis() as u64));
            }
            if back.send(Msg::EffectDone(gen, id, result)).is_err() {
                return;
            }
        });
        if let Err(e) = spawned {
            eprintln!("[tj] could not start an effect worker: {e}");
        }
    }
    tx
}

/// Checks whether a `host:port` netloc matches any entry in `TJ_FETCH_ALLOW`
/// (`example.com` exact match, or `*.example.com` strict subdomain match).
pub(crate) fn netloc_allowed(netloc: &str, allowed: &[String]) -> bool {
    let host = if let Some(rest) = netloc.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        netloc.split(':').next().unwrap_or("")
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    allowed.iter().any(|p| match p.strip_prefix("*.") {
        Some(suffix) => !suffix.is_empty() && host.len() > suffix.len() + 1 && host.ends_with(suffix) && host.as_bytes()[host.len() - suffix.len() - 1] == b'.',
        None => host == *p,
    })
}

fn url_host_allowed(url: &str, allowed: &[String]) -> bool {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).unwrap_or("");
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);
    netloc_allowed(hostport, allowed)
}

/// The public addresses of `netloc` (`host:port`): a server's `fetch` must not reach this
/// machine, its private network or the cloud metadata service (169.254.169.254), where an
/// app's URL argument or an agent's prompt could otherwise read internal data.
pub(crate) fn public_only(netloc: &str) -> std::io::Result<Vec<std::net::SocketAddr>> {
    use std::net::ToSocketAddrs;
    let public: Vec<_> = netloc.to_socket_addrs()?.filter(|a| is_public(a.ip())).collect();
    if public.is_empty() {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("{netloc} is a loopback, private or link-local address; a server fetches only public ones unless {PRIVATE_FETCH_ENV}=1")));
    }
    Ok(public)
}

pub(crate) fn public_or_loopback(netloc: &str) -> std::io::Result<Vec<std::net::SocketAddr>> {
    use std::net::ToSocketAddrs;
    let ok: Vec<_> = netloc.to_socket_addrs()?.filter(|a| is_public(a.ip()) || a.ip().is_loopback()).collect();
    if ok.is_empty() {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("{netloc} is a private or link-local address")));
    }
    Ok(ok)
}

pub(crate) fn is_public(ip: std::net::IpAddr) -> bool {
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

fn perform(
    agent: &ureq::Agent,
    remote_agent: &ureq::Agent,
    plugins: &Plugins,
    fetch_allow: &[String],
    remotes: &Remotes,
    effect: Effect,
) -> Result<Json, (String, Option<String>, bool)> {
    let (url, method, headers, body) = match effect {
        Effect::Http { url, method, headers, body } => (url, method, headers, body),
        Effect::Plugin { plugin, method, args } => return plugins.call(&plugin, &method, args).map_err(|e| (e, None, false)),
        Effect::Call { object, method, args, call } => return remotes.call(remote_agent, &object, &method, &args, &call),
    };
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(("only http and https addresses can be fetched".into(), None, false));
    }
    if !fetch_allow.is_empty() && !url_host_allowed(&url, fetch_allow) {
        return Err((format!("outbound fetch host is not allowed by {FETCH_ALLOW_ENV}"), None, false));
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
        // What went wrong, but not the address: app code often shows the error to people, and
        // an address can be private (a webhook with a token in it). Mark `unreachable = true`
        // so uncaught transport failures return 503 instead of caching a permanent 400 CallRecord.
        Err(ureq::Error::Transport(t)) => {
            use std::error::Error as _;
            let policy = t
                .source()
                .and_then(|s| s.downcast_ref::<std::io::Error>())
                .is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied);
            let deterministic = matches!(
                t.kind(),
                ureq::ErrorKind::InvalidUrl
                    | ureq::ErrorKind::UnknownScheme
                    | ureq::ErrorKind::TooManyRedirects
                    | ureq::ErrorKind::InsecureRequestHttpsOnly
                    | ureq::ErrorKind::InvalidProxyUrl
            );
            return Err((format!("the request failed: {}", t.kind()), None, !policy && !deterministic));
        }
    };
    let status = resp.status();
    let headers: serde_json::Map<String, Json> = resp.headers_names().into_iter().filter_map(|n| resp.header(&n).map(|v| (n.clone(), Json::String(v.to_string())))).collect();
    let body = resp.into_string().map_err(|e| (e.to_string(), None, true))?;
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
        let (seq, msg) = next_change(&engine.subscribe(vec!["Room/lobby".into()], 0, None).expect("admin may stream").0);
        assert!(msg.contains("\"one\""), "{msg}");

        // A client that already has that change gets nothing old, only what happens next.
        let resumed = engine.subscribe(vec!["Room/lobby".into()], seq, None).expect("admin may stream").0;
        post("two").expect("posted");
        let (next, msg) = next_change(&resumed);
        assert!(next > seq && msg.contains("\"two\""), "{msg}");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn a_stream_serves_what_the_caller_may_read_and_names_the_rest() {
        let data = std::env::temp_dir().join(format!("tj-engine-refused-{}", std::process::id()));
        let source = "@server @read(\"owner\") object Profile {
  name: string = \"\"
  @allow(\"owner\") set(n: string) { this.name = n }
}
".to_string();
        let audit = Audit::start(&data).expect("audit log").0;
        let engine = Engine::start(source, data.clone(), false, false, Arc::default(), audit).expect("boots");
        let who = |id: &str| Some(Caller::Remote { id: id.into(), roles: Vec::new() });
        let set = |id: &str, n: &str| engine.request(Input::Call { object: format!("Profile/{id}"), method: "set".into(), args: vec![json!(n)], reply: None, call: None, caller: who(id) }).expect("set");
        set("user:ana", "Ana");
        set("user:bo", "Bo");
        // A device still listing ana's profile from before bo signed in: bo gets his own, and
        // ana's is named as refused instead of the whole stream failing.
        let (events, refused) = engine.subscribe(vec!["Profile/user:ana".into(), "Profile/user:bo".into()], 0, who("user:bo")).expect("bo may stream his own");
        assert_eq!(refused, ["Profile/user:ana"], "named for the audit log");
        // Everything that arrives until `until` shows up (or two seconds pass).
        let read = |until: &str| {
            let (mut text, start) = (String::new(), std::time::Instant::now());
            while !text.contains(until) && start.elapsed() < Duration::from_secs(2) {
                text.extend(events.take(Duration::from_millis(200)).into_iter().map(|e| e.text));
            }
            text
        };
        let text = read("\"Bo\"");
        assert!(text.contains("event: refused") && text.contains("Profile/user:ana"), "{text}");
        assert!(text.contains("\"Bo\"") && !text.contains("\"Ana\""), "{text}");
        // Later changes to ana's profile never reach bo's stream.
        set("user:ana", "Ana again");
        set("user:bo", "Bo again");
        let later = read("Bo again");
        assert!(later.contains("Bo again") && !later.contains("Ana again"), "{later}");
        // Nothing readable: a stream that only names the refusals, and carries nothing else.
        let (events, refused) = engine.subscribe(vec!["Profile/user:ana".into()], 0, who("user:bo")).expect("opens");
        assert_eq!(refused, ["Profile/user:ana"]);
        set("user:ana", "Ana once more");
        let text: String = events.take(Duration::from_secs(1)).into_iter().map(|e| e.text).collect();
        assert!(text.contains("event: refused") && !text.contains("Ana once more"), "{text}");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn a_class_stream_carries_every_object_of_the_class_its_reader_may_read() {
        let data = std::env::temp_dir().join(format!("tj-engine-class-{}", std::process::id()));
        let source = "@server @read(\"anyone\") object Slot {
  title: string = \"\"
  @allow(\"anyone\") name(t: string) { this.title = t }
}
@server @read(\"owner\") object Profile {
  name: string = \"\"
}
".to_string();
        let audit = Audit::start(&data).expect("audit log").0;
        let engine = Engine::start(source, data.clone(), false, false, Arc::default(), audit).expect("boots");
        let anyone = || Some(Caller::Remote { id: String::new(), roles: Vec::new() });
        let name = |id: &str, t: &str| engine.request(Input::Call { object: format!("Slot/{id}"), method: "name".into(), args: vec![json!(t)], reply: None, call: None, caller: anyone() }).expect("named");
        name("s1", "Cut");
        let (events, refused) = engine.subscribe(vec!["Slot/*".into(), "Profile/*".into()], 0, anyone()).expect("opens");
        assert_eq!(refused, ["Profile/*"], "a class read only by its owners cannot be streamed whole");
        let read = |until: &str| {
            let (mut text, start) = (String::new(), std::time::Instant::now());
            while !text.contains(until) && start.elapsed() < Duration::from_secs(2) {
                text.extend(events.take(Duration::from_millis(200)).into_iter().map(|e| e.text));
            }
            text
        };
        assert!(read("Cut").contains("\"object\":\"Slot/s1\""), "it starts with the class's objects");
        name("s2", "Shave");
        assert!(read("Shave").contains("\"object\":\"Slot/s2\""), "and carries new ones");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn a_list_of_several_peoples_entries_streams_to_its_readers_and_masked_to_tools() {
        let data = std::env::temp_dir().join(format!("tj-engine-roster-{}", std::process::id()));
        let source = "@server @read(\"staff\") object Roster {
  @pii(\"contact\", \"booking\") @by(\"customer\") entries: { customer: string, name: string }[] = []
  @allow(\"signedIn\") add(name: string) { this.entries.push({ customer: caller.id, name }) }
}
".to_string();
        let audit = Audit::start(&data).expect("audit log").0;
        let engine = Engine::start(source, data.clone(), false, false, Arc::default(), audit).expect("boots");
        let (staff, _) = engine.subscribe(vec!["Roster/main".into()], 0, Some(Caller::Remote { id: "key:sam".into(), roles: vec!["staff".into()] })).expect("staff may read it");
        let (tool, _) = engine.subscribe(vec!["Roster/main".into()], 0, None).expect("the admin's stream");
        engine.request(Input::Call { object: "Roster/main".into(), method: "add".into(), args: vec![json!("Ana")], reply: None, call: None, caller: Some(Caller::Remote { id: "user:ana".into(), roles: Vec::new() }) }).expect("added");
        let read = |m: &Mailbox| {
            let (mut text, start) = (String::new(), std::time::Instant::now());
            while !text.contains("entries") && start.elapsed() < Duration::from_secs(2) {
                text.extend(m.take(Duration::from_millis(200)).into_iter().map(|e| e.text));
            }
            text
        };
        assert!(read(&staff).contains("\"Ana\""), "staff read the list");
        let masked = read(&tool);
        assert!(masked.contains("entries") && !masked.contains("Ana"), "{masked}");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn a_class_stream_shows_who_booked_only_to_staff_and_to_that_person() {
        let data = std::env::temp_dir().join(format!("tj-engine-fields-{}", std::process::id()));
        let source = "@server @read(\"anyone\") object Slot {
  @read(\"staff\", \"self\") customer: string = \"\"
  @allow(\"signedIn\") book() { this.customer = caller.id }
}
".to_string();
        let audit = Audit::start(&data).expect("audit log").0;
        let engine = Engine::start(source, data.clone(), false, false, Arc::default(), audit).expect("boots");
        let person = |id: &str, roles: &[&str]| Some(Caller::Remote { id: id.into(), roles: roles.iter().map(|r| r.to_string()).collect() });
        let stream = |caller| engine.subscribe(vec!["Slot/*".into()], 0, caller).expect("anyone may read slots").0;
        let (ana, bo, staff) = (stream(person("user:ana", &[])), stream(person("user:bo", &[])), stream(person("key:sam", &["staff"])));
        engine.request(Input::Call { object: "Slot/s1".into(), method: "book".into(), args: Vec::new(), reply: None, call: None, caller: person("user:ana", &[]) }).expect("booked");
        let read = |m: &Mailbox| {
            let (mut text, start) = (String::new(), std::time::Instant::now());
            while !text.contains("customer") && start.elapsed() < Duration::from_secs(2) {
                text.extend(m.take(Duration::from_millis(200)).into_iter().map(|e| e.text));
            }
            text
        };
        assert!(read(&ana).contains("user:ana"), "the person who booked sees it is theirs");
        assert!(read(&staff).contains("user:ana"), "staff see who booked");
        let other = read(&bo);
        assert!(other.contains("\"customer\":\"•••\"") && !other.contains("user:ana"), "{other}");
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
    fn a_failed_fetch_says_what_went_wrong_but_not_the_address() {
        // The address may be private (a webhook with a token in it), and apps show errors.
        let agent = ureq::AgentBuilder::new().resolver(public_only).timeout(Duration::from_secs(2)).build();
        let url = "http://127.0.0.1:9/hook?token=secret123";
        let (e, _, unreachable) = perform(&agent, &agent, &Plugins::default(), &[], &Remotes::default(), Effect::Http { url: url.into(), method: "GET".into(), headers: Json::Null, body: None }).unwrap_err();
        assert!(e.starts_with("the request failed") && !e.contains("secret123") && !e.contains("127.0.0.1"), "{e}");
        assert!(!unreachable, "public_only PermissionDenied policy refusal must not mark unreachable");

        // Real TCP connection refusal on an allowed loopback address marks `unreachable = true` (503 retriable).
        let open_agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(2)).build();
        let (e2, _, unreachable2) = perform(&open_agent, &open_agent, &Plugins::default(), &[], &Remotes::default(), Effect::Http { url: "http://127.0.0.1:1/".into(), method: "GET".into(), headers: Json::Null, body: None }).unwrap_err();
        assert!(e2.starts_with("the request failed"), "{e2}");
        assert!(unreachable2, "real transport failure must mark unreachable=true");

        // IK-4: deterministic URL parse errors (`InvalidUrl`) must NOT mark `unreachable = true`.
        let (e3, _, unreachable3) = perform(&open_agent, &open_agent, &Plugins::default(), &[], &Remotes::default(), Effect::Http { url: "https://[bad url".into(), method: "GET".into(), headers: Json::Null, body: None }).unwrap_err();
        assert!(e3.starts_with("the request failed"), "{e3}");
        assert!(!unreachable3, "InvalidUrl must not trigger infinite 503 retries");
    }

    #[test]
    fn a_person_can_delete_their_own_account_and_the_audit_log_forgets_them_after() {
        let data = std::env::temp_dir().join(format!("tj-engine-privacy-{}", std::process::id()));
        let source = r#"
import { erase, exportData } from "tj:privacy"

@server @read("owner") object Account {
  @pii("contact", "account") email: string = ""
  note: string = ""
  @allow("owner") set(e: string) { this.email = e }
  @allow("owner") async mine(): any { return await exportData(caller.id) }
  @allow("owner") async deleteMe(): string {
    await erase(caller.id)
    return this.email
  }
  @allow("signedIn") async eraseOther(id: string) { await erase(id) }
  @allow("owner") async deleteThenWait(): string {
    await erase(caller.id)
    try { await fetch("http://127.0.0.1:9/") } catch (e) { }
    return "done"
  }
}
"#
        .to_string();
        let (audit, _) = Audit::start(&data).expect("audit log");
        let engine = Engine::start(source, data.clone(), false, false, Arc::default(), audit).expect("boots");
        let as_user = |id: &str| Some(Caller::Remote { id: id.into(), roles: Vec::new() });
        let call = |object: &str, method: &str, args: Vec<Json>, who: &str| engine.request(Input::Call { object: object.into(), method: method.into(), args, reply: None, call: None, caller: as_user(who) });
        call("Account/user:ana", "set", vec![json!("ana@example.com")], "user:ana").expect("set");
        let mine = call("Account/user:ana", "mine", vec![], "user:ana").expect("export");
        assert_eq!(mine["Account/user:ana"]["email"], json!("ana@example.com"));

        // Someone else cannot; the person can, and their method sees the blank state.
        let refused = call("Account/user:bo", "eraseOther", vec![json!("user:ana")], "user:bo").unwrap_err();
        assert!(refused.1.contains("only the person themselves"), "{refused:?}");
        assert_eq!(call("Account/user:ana", "deleteMe", vec![], "user:ana"), Ok(json!("")));
        // A call that goes on after erasing its caller: they are forgotten once it ends.
        call("Account/user:cy", "set", vec![json!("cy@example.com")], "user:cy").expect("set");
        assert_eq!(call("Account/user:cy", "deleteThenWait", vec![], "user:cy"), Ok(json!("done")));

        // The audit log has the call, written before the person was forgotten: no pseudonym
        // of theirs is left behind (bo's remains).
        let log = || std::fs::read_dir(data.join("audit")).unwrap().filter_map(|e| e.ok()).filter(|e| e.path().extension().is_some_and(|x| x == "jsonl") && !e.path().ends_with("checkpoints.jsonl")).map(|e| std::fs::read_to_string(e.path()).unwrap()).collect::<String>();
        let mut tries = 0;
        while !log().contains("\"method\":\"deleteThenWait\"") && tries < 50 {
            std::thread::sleep(Duration::from_millis(50));
            tries += 1;
        }
        let entries = log();
        assert!(entries.contains("\"kind\":\"privacy\"") && entries.contains("\"method\":\"deleteMe\""), "{entries}");
        assert!(entries.contains("\"kind\":\"refused\"") && entries.contains("\"request\":\"privacy erase\""), "the refusal is a security event: {entries}");
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(std::fs::read_dir(data.join("subjects")).unwrap().count(), 1, "only bo's pseudonym is left");
        let _ = std::fs::remove_dir_all(&data);
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

    #[test]
    fn fetch_allowlist_matches_exact_and_strict_subdomains_and_blocks_disallowed() {
        let allow = parse_fetch_allow("api.example.com, *.partner.org, [2606:4700::1111]").unwrap();
        // Exact host match (case-insensitive, with port or trailing dot) and normalized IPv6 literal.
        assert!(netloc_allowed("api.example.com:443", &allow));
        assert!(netloc_allowed("API.EXAMPLE.COM.:80", &allow));
        assert!(netloc_allowed("[2606:4700::1111]:443", &allow));
        assert!(!netloc_allowed("sub.api.example.com:443", &allow));
        assert!(!netloc_allowed("evilapi.example.com:443", &allow));
        // Strict wildcard subdomain match: bare apex and suffix-spoofing rejected.
        assert!(netloc_allowed("hooks.partner.org:443", &allow));
        assert!(netloc_allowed("a.b.partner.org:443", &allow));
        assert!(!netloc_allowed("partner.org:443", &allow));
        assert!(!netloc_allowed("evilpartner.org:443", &allow));
        // Malformed / whitespace-only env entries fail startup instead of failing open (C-039.4).
        assert!(parse_fetch_allow(" , ").is_err());
        assert!(parse_fetch_allow("https://api.example.com").is_err());
        assert!(parse_fetch_allow("api.example.com:443").is_err());
        // URL parsing including userinfo stripping.
        assert!(url_host_allowed("https://api.example.com/v1/ok?x=1", &allow));
        assert!(url_host_allowed("https://user:pass@hooks.partner.org:8443/cb", &allow));
        assert!(!url_host_allowed("https://evil.com/steal", &allow));
        // `perform` refuses disallowed hosts without leaking the URL.
        let agent = ureq::AgentBuilder::new().build();
        let (err, _, unreachable) = perform(
            &agent,
            &agent,
            &Plugins::default(),
            &allow,
            &Remotes::default(),
            Effect::Http {
                url: "https://secret-token.evil.com/hook?key=TOPSECRET".into(),
                method: "GET".into(),
                headers: Json::Null,
                body: None,
            },
        )
        .unwrap_err();
        assert!(!unreachable, "allowlist refusal must not mark unreachable");
        assert!(err.contains(FETCH_ALLOW_ENV), "{err}");
        assert!(!err.contains("TOPSECRET") && !err.contains("evil.com"), "error must not leak URL: {err}");

        // C-039.4: end-to-end HTTP 302 redirect from an allowed host (127.0.0.1) to a disallowed
        // host is caught and blocked by `ureq`'s `.resolver(...)`.
        let srv = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = srv.server_addr().to_ip().unwrap().port();
        let handle = std::thread::spawn(move || {
            if let Ok(req) = srv.recv() {
                let loc = tiny_http::Header::from_bytes(&b"Location"[..], &b"http://disallowed.example.invalid/secret"[..]).unwrap();
                let _ = req.respond(tiny_http::Response::empty(302).with_header(loc));
            }
        });
        let local_allow = Arc::new(parse_fetch_allow("127.0.0.1").unwrap());
        let a_ref = local_allow.clone();
        let redirect_agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(5))
            .resolver(move |netloc: &str| {
                if !netloc_allowed(netloc, &a_ref) {
                    return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("blocked by {FETCH_ALLOW_ENV}")));
                }
                use std::net::ToSocketAddrs;
                Ok(netloc.to_socket_addrs()?.collect())
            })
            .build();
        let (redir_err, _, redir_unreachable) = perform(
            &redirect_agent,
            &redirect_agent,
            &Plugins::default(),
            &local_allow,
            &Remotes::default(),
            Effect::Http {
                url: format!("http://127.0.0.1:{port}/start"),
                method: "GET".into(),
                headers: Json::Null,
                body: None,
            },
        )
        .unwrap_err();
        assert!(!redir_unreachable, "redirect allowlist refusal must not mark unreachable");
        assert!(redir_err.contains("the request failed"), "redirect to disallowed host must fail: {redir_err}");
        assert!(!redir_err.contains("disallowed.example.invalid"), "redirect error must not leak URL: {redir_err}");
        let _ = handle.join();
    }
}

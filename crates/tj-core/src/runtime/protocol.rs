//! The host protocol. Every host speaks these messages, in Rust or as JSON.
//! Documented for host authors in `docs/host-protocol.md`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};

/// Who is making a `call` or `get` (docs/access-control.md). `"trusted"` is the host itself
/// (automation, a device's own UI); the rules do not apply to it. A server runtime refuses
/// a `call` or `get` without a caller, so a network path that forgets it fails closed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Caller {
    Trusted(Trust),
    Remote {
        id: String,
        #[serde(default)]
        roles: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trust {
    Trusted,
}

impl Caller {
    pub fn trusted() -> Caller {
        Caller::Trusted(Trust::Trusted)
    }
}

/// What a host sends to the runtime, always with the current time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Input {
    /// Call a public method on an object. `object` is `"Class/id"`. A call with a `call` id
    /// runs at most once: repeating it returns the first result (`Idempotency-Key` in HTTP).
    Call {
        object: String,
        method: String,
        #[serde(default)]
        args: Vec<Json>,
        #[serde(default)]
        reply: Option<u64>,
        #[serde(default)]
        call: Option<String>,
        #[serde(default)]
        caller: Option<Caller>,
    },
    /// Read an object's state.
    Get {
        object: String,
        #[serde(default)]
        reply: Option<u64>,
        #[serde(default)]
        caller: Option<Caller>,
    },
    /// A UI event from the user or from an automation client.
    Ui {
        target: Target,
        event: String,
        #[serde(default)]
        value: Json,
        #[serde(default)]
        reply: Option<u64>,
    },
    /// The result of an [`Output::Effect`]. `unreachable`: a server call never got an
    /// answer (offline, timed out, or the server is down or busy), so it is retried later.
    EffectDone {
        id: u32,
        #[serde(default)]
        ok: Option<Json>,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        unreachable: bool,
    },
    /// The server's state of an `@server` object this device subscribed to, with its change
    /// number. Older or repeated numbers are ignored, so late deliveries never go back in time.
    Sync {
        object: String,
        state: Json,
        #[serde(default)]
        seq: Option<u64>,
    },
    /// Native hosts: lay the screen out in this viewport (points/dp) after applying patches
    /// or when the screen size changes. Answered with [`Output::Frames`].
    /// `rtl` mirrors the layout for right-to-left locales (Arabic, Urdu, Hebrew, ...).
    Layout {
        width: f32,
        height: f32,
        #[serde(default)]
        rtl: bool,
    },
    /// The host reached the server again (its change stream connected): queued server calls
    /// are retried now instead of waiting out their backoff.
    Online,
    /// Devices: the signed-in user changed (`null` when signed out). Server calls queued by
    /// someone else are dropped, never sent as the new user, and replicas are cleared.
    Identity {
        #[serde(default)]
        user: Option<String>,
    },
    /// Secure random bytes from the host (hex, at least 32 bytes), sent once right after
    /// boot. They are mixed into the generator behind `crypto.randomId()` and device ids.
    Entropy { bytes: String },
    /// Time moved on; due timers run.
    Tick,
    /// The semantic tree and object states, for automation.
    Inspect {
        #[serde(default)]
        reply: Option<u64>,
    },
}

/// How automation addresses a UI node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Target {
    Node { node: u32 },
    Id { id: String },
    Text { text: String },
}

/// What the runtime asks of the host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Output {
    /// Apply these changes to the native view tree.
    Patch { ops: Vec<PatchOp> },
    /// Position these nodes (only the ones that moved), relative to their parent.
    Frames { frames: Vec<crate::ui::layout::Frame> },
    /// Perform an effect and answer with [`Input::EffectDone`].
    Effect { id: u32, effect: Effect },
    /// An object emitted an event (realtime channel `object`).
    Emit { object: String, name: String, data: Json },
    /// An object's state changed (already persisted). `seq` is its change number: it grows
    /// with every change this runtime makes, across restarts.
    Changed { object: String, state: Json, seq: u64 },
    /// Keep this device's replica of an `@server` object current: stream the server's changes
    /// after `seq` (the cached replica's change number, 0 if none) and send each as
    /// [`Input::Sync`].
    Subscribe { object: String, seq: u64 },
    Reply {
        reply: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ok: Option<Json>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// For an error that is about the request rather than the app, its HTTP status:
        /// 400 (no caller), 403 (not allowed), 422 (a call id reused for another call),
        /// 503 (too many objects loaded).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
    },
    Log { level: String, message: String },
    /// Servers only, for the audit log: a call nobody requested ended. `by` is `timer`,
    /// `alarm` or `invoke`; `caller` is who it ran as (none: the system). Never arguments,
    /// results or error text.
    Ran {
        object: String,
        method: String,
        by: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller: Option<String>,
        ok: bool,
        ms: u64,
    },
    /// An uncaught error in a UI handler, timer or render.
    Error { message: String, trace: Vec<String> },
    /// When to send the next [`Input::Tick`] (epoch milliseconds), if any timer is pending.
    Wake { at: Option<f64> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Effect {
    /// Answer with `{ status, headers, body }`.
    Http {
        url: String,
        method: String,
        headers: Json,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
    },
    /// Call a method of an `@server` object on the server, sending `call` as the
    /// `Idempotency-Key` header; answer with its result, or with `unreachable` if no answer came.
    Call { object: String, method: String, args: Vec<Json>, call: String },
    /// Call `method` of plugin `plugin` (docs/plugins.md); answer with its result. On a server,
    /// `tj serve` runs the plugin as an MCP server: `tools/call` with `name = method`.
    Plugin { plugin: String, method: String, args: Json },
}

/// One change to the native view tree. Node 0 is the host's root container.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum PatchOp {
    /// Create a detached node.
    Create { id: u32, kind: String, props: Map<String, Json>, events: Vec<String> },
    /// Change props (a `null` value removes the prop) and, if present, the handled events.
    Update {
        id: u32,
        props: Map<String, Json>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        events: Option<Vec<String>>,
    },
    /// Set the exact ordered children of a node.
    Children { id: u32, ids: Vec<u32> },
    /// Delete a node.
    Remove { id: u32 },
}

//! The host protocol. Every host speaks these messages, in Rust or as JSON.
//! Documented for host authors in `docs/host-protocol.md`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};

/// What a host sends to the runtime, always with the current time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Input {
    /// Call a public method on an object. `object` is `"Class/id"`.
    Call {
        object: String,
        method: String,
        #[serde(default)]
        args: Vec<Json>,
        #[serde(default)]
        reply: Option<u64>,
    },
    /// Read an object's state.
    Get {
        object: String,
        #[serde(default)]
        reply: Option<u64>,
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
    /// The result of an [`Output::Effect`].
    EffectDone {
        id: u32,
        #[serde(default)]
        ok: Option<Json>,
        #[serde(default)]
        error: Option<String>,
    },
    /// The server's latest state of an `@server` object this device subscribed to.
    Sync { object: String, state: Json },
    /// Native hosts: lay the screen out in this viewport (points/dp) after applying patches
    /// or when the screen size changes. Answered with [`Output::Frames`].
    /// `rtl` mirrors the layout for right-to-left locales (Arabic, Urdu, Hebrew, ...).
    Layout {
        width: f32,
        height: f32,
        #[serde(default)]
        rtl: bool,
    },
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
    /// An object's state changed (already persisted).
    Changed { object: String, state: Json },
    /// Keep this device's replica of an `@server` object current: fetch its state now and
    /// send [`Input::Sync`] whenever the server reports a change.
    Subscribe { object: String },
    Reply {
        reply: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ok: Option<Json>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Log { level: String, message: String },
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
    /// Call a method of an `@server` object on the server; answer with its result.
    Call { object: String, method: String, args: Vec<Json> },
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

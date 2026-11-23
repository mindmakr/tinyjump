//! The host protocol. Every host speaks these messages, in Rust or as JSON.
//! Documented for host authors in `docs/host-protocol.md`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};

/// Who is making a `call` or `get` (docs/access-control.md). `"trusted"` is the host itself
/// (automation, a device's own UI); the rules do not apply to it. A server runtime refuses
/// a `call` or `get` without a caller, so a network path that forgets it fails closed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged, try_from = "Json")]
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

/// What a host sends to the runtime, always with the current time. Read by hand
/// (`TryFrom<Json>` below) rather than by serde's generated code, which cost device builds
/// about 35 KB; written by serde.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", try_from = "Json")]
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
    /// The result of an [`Output::Effect`]. `unreachable`: a server `call` never got an
    /// answer (offline, timed out, or the server is down or busy) so it is retried later, or
    /// on a server, an `http` effect hit a transient transport failure (unwound as `503`
    /// without caching a `400` call record).
    EffectDone {
        id: u32,
        #[serde(default)]
        ok: Option<Json>,
        #[serde(default)]
        error: Option<String>,
        /// The field named by `check(ok, field, message)` on a server call refusal.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
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
    /// `roles` are the user's roles as the server verified them, for the UI to show or hide
    /// controls; the server checks every call itself.
    Identity {
        #[serde(default)]
        user: Option<String>,
        #[serde(default)]
        roles: Vec<String>,
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
#[serde(untagged, try_from = "Json")]
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
        /// The field named by `check(ok, field, message)`, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
        /// For an error that is about the request rather than the app, its HTTP status:
        /// 400 (no caller), 403 (not allowed), 422 (a call id reused for another call),
        /// 503 (too many objects loaded).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
    },
    Log { level: String, message: String },
    /// Servers only, for the audit log: retention erased an object (`reason`: `retain`, or
    /// `erase after hold` when a person's erasure waited for a legal hold to end).
    /// `forget`: nothing of the person is held any more, so the audit log forgets them.
    #[cfg(feature = "server")]
    Erased { object: String, reason: String, forget: bool },
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

/// Reads the fields of one JSON object, saying which field is wrong when one is.
struct Fields(Map<String, Json>);

impl Fields {
    fn take(&mut self, key: &str) -> Json {
        self.0.remove(key).unwrap_or(Json::Null)
    }

    fn string(&mut self, key: &str) -> Result<String, String> {
        self.opt_string(key)?.ok_or_else(|| format!("missing `{key}`"))
    }

    fn opt_string(&mut self, key: &str) -> Result<Option<String>, String> {
        match self.take(key) {
            Json::Null => Ok(None),
            Json::String(s) => Ok(Some(s)),
            _ => Err(format!("`{key}` must be a string")),
        }
    }

    fn opt_u64(&mut self, key: &str) -> Result<Option<u64>, String> {
        match self.take(key) {
            Json::Null => Ok(None),
            v => v.as_u64().map(Some).ok_or_else(|| format!("`{key}` must be a whole number")),
        }
    }

    fn number(&mut self, key: &str) -> Result<f64, String> {
        self.take(key).as_f64().ok_or_else(|| format!("`{key}` must be a number"))
    }

    fn flag(&mut self, key: &str) -> Result<bool, String> {
        match self.take(key) {
            Json::Null => Ok(false),
            Json::Bool(b) => Ok(b),
            _ => Err(format!("`{key}` must be true or false")),
        }
    }

    fn opt_json(&mut self, key: &str) -> Option<Json> {
        Some(self.take(key)).filter(|v| !v.is_null())
    }

    fn roles(&mut self) -> Result<Vec<String>, String> {
        match self.take("roles") {
            Json::Null => Ok(Vec::new()),
            Json::Array(a) => a.into_iter().map(|r| r.as_str().map(str::to_string).ok_or_else(|| "`roles` must be strings".to_string())).collect(),
            _ => Err("`roles` must be an array of strings".into()),
        }
    }

    fn caller(&mut self) -> Result<Option<Caller>, String> {
        match self.take("caller") {
            Json::Null => Ok(None),
            v => Caller::try_from(v).map(Some),
        }
    }
}

fn object(v: Json, what: &str) -> Result<Fields, String> {
    match v {
        Json::Object(m) => Ok(Fields(m)),
        _ => Err(format!("{what} must be a JSON object")),
    }
}

impl TryFrom<Json> for Input {
    type Error = String;

    fn try_from(v: Json) -> Result<Input, String> {
        let mut f = object(v, "an input")?;
        let kind = f.string("type")?;
        let at = |e: String| format!("{kind} input: {e}");
        let input = (|| -> Result<Input, String> {
            Ok(match kind.as_str() {
                "call" => {
                    let args = match f.take("args") {
                        Json::Null => Vec::new(),
                        Json::Array(a) => a,
                        _ => return Err("`args` must be an array".into()),
                    };
                    Input::Call { object: f.string("object")?, method: f.string("method")?, args, reply: f.opt_u64("reply")?, call: f.opt_string("call")?, caller: f.caller()? }
                }
                "get" => Input::Get { object: f.string("object")?, reply: f.opt_u64("reply")?, caller: f.caller()? },
                "ui" => Input::Ui { target: Target::try_from(f.take("target"))?, event: f.string("event")?, value: f.take("value"), reply: f.opt_u64("reply")? },
                "effectDone" => {
                    let id = f.opt_u64("id")?.and_then(|id| u32::try_from(id).ok()).ok_or("`id` must be an effect id")?;
                    Input::EffectDone { id, ok: f.opt_json("ok"), error: f.opt_string("error")?, field: f.opt_string("field")?, unreachable: f.flag("unreachable")? }
                }
                "sync" => Input::Sync { object: f.string("object")?, state: f.take("state"), seq: f.opt_u64("seq")? },
                "layout" => Input::Layout { width: f.number("width")? as f32, height: f.number("height")? as f32, rtl: f.flag("rtl")? },
                "online" => Input::Online,
                "identity" => Input::Identity { user: f.opt_string("user")?, roles: f.roles()? },
                "entropy" => Input::Entropy { bytes: f.string("bytes")? },
                "tick" => Input::Tick,
                "inspect" => Input::Inspect { reply: f.opt_u64("reply")? },
                other => return Err(format!("unknown type `{other}`")),
            })
        })();
        input.map_err(at)
    }
}

impl TryFrom<Json> for Target {
    type Error = String;

    fn try_from(v: Json) -> Result<Target, String> {
        let mut f = object(v, "`target`")?;
        if let Some(node) = f.opt_u64("node")? {
            return u32::try_from(node).map(|node| Target::Node { node }).map_err(|_| "`node` is too large".into());
        }
        if let Some(id) = f.opt_string("id")? {
            return Ok(Target::Id { id });
        }
        f.opt_string("text")?.map(|text| Target::Text { text }).ok_or_else(|| "`target` needs `node`, `id` or `text`".into())
    }
}

impl TryFrom<Json> for Caller {
    type Error = String;

    fn try_from(v: Json) -> Result<Caller, String> {
        if v == "trusted" {
            return Ok(Caller::trusted());
        }
        let mut f = object(v, "`caller`")?;
        let roles = f.roles()?;
        Ok(Caller::Remote { id: f.string("id")?, roles })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_input_reads_back_what_serde_writes() {
        let inputs = [
            Input::Call { object: "Cart/ana".into(), method: "add".into(), args: vec![json!("a"), json!(2)], reply: Some(7), call: Some("c-1".into()), caller: Some(Caller::Remote { id: "user:ana".into(), roles: vec!["staff".into()] }) },
            Input::Call { object: "A/b".into(), method: "m".into(), args: vec![], reply: None, call: None, caller: Some(Caller::trusted()) },
            Input::Get { object: "A/b".into(), reply: Some(1), caller: None },
            Input::Ui { target: Target::Node { node: 4 }, event: "onPress".into(), value: Json::Null, reply: None },
            Input::Ui { target: Target::Id { id: "add".into() }, event: "onChange".into(), value: json!("x"), reply: Some(2) },
            Input::Ui { target: Target::Text { text: "Save".into() }, event: "onPress".into(), value: Json::Null, reply: None },
            Input::EffectDone { id: 3, ok: Some(json!({ "status": 200 })), error: None, field: None, unreachable: false },
            Input::EffectDone { id: 4, ok: None, error: Some("offline".into()), field: None, unreachable: true },
            Input::EffectDone { id: 5, ok: None, error: Some("Give a title".into()), field: Some("new-title".into()), unreachable: false },
            Input::Sync { object: "A/b".into(), state: json!({ "n": 1 }), seq: Some(9) },
            Input::Layout { width: 390.0, height: 844.5, rtl: true },
            Input::Online,
            Input::Identity { user: Some("user:ana".into()), roles: vec!["staff".into()] },
            Input::Identity { user: None, roles: Vec::new() },
            Input::Entropy { bytes: "ab".repeat(32) },
            Input::Tick,
            Input::Inspect { reply: Some(5) },
        ];
        for input in inputs {
            let text = serde_json::to_string(&input).unwrap();
            assert_eq!(serde_json::from_str::<Input>(&text).unwrap(), input, "{text}");
        }
    }

    #[test]
    fn bad_inputs_say_what_is_wrong() {
        let err = |text: &str| serde_json::from_str::<Input>(text).unwrap_err().to_string();
        assert!(err(r#"{"type":"nope"}"#).contains("unknown type `nope`"));
        assert!(err(r#"{"type":"call","method":"m"}"#).contains("call input: missing `object`"));
        assert!(err(r#"{"type":"call","object":"A/b","method":"m","args":3}"#).contains("`args` must be an array"));
        assert!(err(r#"{"type":"ui","target":{},"event":"onPress"}"#).contains("`target` needs `node`, `id` or `text`"));
        assert!(err(r#"{"type":"get","object":"A/b","caller":{"roles":["x"]}}"#).contains("missing `id`"));
        assert!(err(r#"[1]"#).contains("an input must be a JSON object"));
    }
}

//! The JSON boundary shared by hosts written in other languages (WASM/JavaScript,
//! JNI/Java, Swift). Each call returns a status code and a JSON payload.

use crate::runtime::{Options, Runtime};
use crate::ui::layout::Measure;
use crate::{compile, Input, Store};
use serde_json::{json, Value as Json};

/// The payload is the outputs array.
pub const OK: i32 = 0;
/// The payload lists compile errors, or is `{ message }` for a bad input.
pub const INVALID: i32 = 1;
/// The payload is `{ message, trace }`.
pub const BOOT_FAILED: i32 = 2;

#[derive(Default)]
pub struct JsonHost {
    runtime: Option<Runtime>,
}

impl JsonHost {
    /// Compiles and boots `src`, replacing any running app. Native hosts pass a `measure` for layout.
    pub fn boot(&mut self, src: &str, store: Box<dyn Store>, measure: Option<Box<dyn Measure>>, now: f64, seed: u64, render: bool) -> (i32, String) {
        let program = match compile(src) {
            Ok(p) => p,
            Err(errs) => {
                let list: Vec<Json> = errs
                    .iter()
                    .map(|d| {
                        let (line, col) = crate::syntax::line_col(src, d.span.start as usize);
                        json!({ "message": d.message, "hint": d.hint, "line": line, "col": col, "text": d.render("app.tj", src) })
                    })
                    .collect();
                return (INVALID, Json::Array(list).to_string());
            }
        };
        // Foreign-language hosts are devices: `@server` objects are replicas.
        match Runtime::boot(program, store, Options { now, seed, render, server: false }) {
            Ok((mut rt, outputs)) => {
                if let Some(m) = measure {
                    rt.set_measure(m);
                }
                self.runtime = Some(rt);
                (OK, to_json(&outputs))
            }
            Err(e) => (BOOT_FAILED, json!({ "message": e.message, "trace": e.trace }).to_string()),
        }
    }

    /// Handles one JSON-encoded [`Input`].
    pub fn handle(&mut self, now: f64, input: &str) -> (i32, String) {
        let input: Input = match serde_json::from_str(input) {
            Ok(i) => i,
            Err(e) => return (INVALID, json!({ "message": format!("bad input: {e}") }).to_string()),
        };
        match self.runtime.as_mut() {
            Some(rt) => (OK, to_json(&rt.handle(now, input))),
            None => (INVALID, json!({ "message": "not booted" }).to_string()),
        }
    }
}

/// Parses the JSON array of keys a host store returns; anything else means no keys.
pub fn parse_keys(json: &str) -> Vec<String> {
    serde_json::from_str(json).unwrap_or_default()
}

fn to_json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("protocol types serialize")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemStore;

    #[test]
    fn boots_handles_and_reports_errors() {
        let mut h = JsonHost::default();
        assert_eq!(h.handle(0.0, r#"{"type":"tick"}"#).0, INVALID, "not booted yet");
        let (code, body) = h.boot("function f() { return x }", Box::new(MemStore::default()), None, 0.0, 1, false);
        assert_eq!(code, INVALID);
        assert!(body.contains("unknown name 'x'"));
        let (code, _) = h.boot("object A { n = 0\n inc() { this.n += 1; return this.n } }", Box::new(MemStore::default()), None, 0.0, 1, false);
        assert_eq!(code, OK);
        let (code, body) = h.handle(1.0, r#"{"type":"call","object":"A/a","method":"inc","reply":7}"#);
        assert_eq!(code, OK);
        assert!(body.contains(r#""reply":7,"ok":1"#), "{body}");
        assert_eq!(h.handle(1.0, "nope").0, INVALID);
    }
}

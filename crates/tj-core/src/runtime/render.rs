//! Renders `App` into the view tree and builds the automation snapshot.

use super::protocol::Output;
use super::Runtime;
use crate::ui::catalog::{self, Content};
use crate::ui::tree::VNode;
use crate::vm::value::{Elem, Value};
use serde_json::{json, Map, Value as Json};
use std::rc::Rc;

impl Runtime {
    pub(crate) fn render_app(&mut self) {
        let Some(slot) = self.program.app else { return };
        let app = self.globals[slot as usize].clone();
        self.read_clock = false;
        let result = self.fiber_for(&app, Vec::new()).map_err(|e| (e.message(), Vec::new())).and_then(|mut f| {
            f.caller = self.local();
            self.run_sync(f).map_err(|fail| (fail.message, fail.trace))
        });
        // A screen that shows the time is drawn again at the start of each minute.
        if self.read_clock && self.clock.is_none() {
            let at = (self.now / 60_000.0).floor() * 60_000.0 + 60_000.0;
            self.clock = Some(at);
            self.push_timer(at, super::TimerKind::Clock);
        }
        let nodes = result.and_then(|v| {
            let mut out = Vec::new();
            nodes(&v, &mut out).map(|()| out).map_err(|e| (e, Vec::new()))
        });
        match nodes {
            Ok(nodes) => {
                let ops = self.tree.update(nodes);
                self.metrics.renders += 1;
                self.metrics.patch_ops += ops.len() as u64;
                if !ops.is_empty() {
                    self.outputs.push(Output::Patch { ops });
                }
            }
            Err((message, trace)) => self.outputs.push(Output::Error { message: format!("render: {message}"), trace }),
        }
    }

    /// The semantic tree, object states, next wake time, work counters and statistics.
    pub fn inspect(&self) -> Json {
        let mut metrics = serde_json::to_value(self.metrics).expect("metrics serialize");
        metrics["nodes"] = json!(self.tree.node_count());
        metrics["objects"] = json!(self.instances.len());
        json!({ "tree": self.tree.to_json(), "objects": self.object_states(), "outbox": self.outbox_json(), "wake": self.next_wake(), "locale": &*self.locale, "metrics": metrics, "stats": self.stats() })
    }
}

/// Flattens a rendered value into nodes. Arrays flatten; `null` and booleans render nothing,
/// so `{cond && <X/>}` works; strings and numbers become `Text`.
fn nodes(v: &Value, out: &mut Vec<VNode>) -> Result<(), String> {
    match v {
        Value::Null | Value::Bool(_) => Ok(()),
        Value::Arr(items) => items.borrow().iter().try_for_each(|i| nodes(i, out)),
        Value::Elem(e) => {
            out.push(elem(e)?);
            Ok(())
        }
        Value::Str(_) | Value::Num(_) => {
            let mut props = Map::new();
            props.insert("text".into(), Json::String(v.display()));
            out.push(VNode { kind: Rc::from("Text"), key: None, props, handlers: Vec::new(), children: Vec::new() });
            Ok(())
        }
        other => Err(format!("cannot render a {}", other.type_name())),
    }
}

fn text(v: &Value, out: &mut String) -> Result<(), String> {
    match v {
        Value::Null | Value::Bool(_) => Ok(()),
        Value::Arr(items) => items.borrow().iter().try_for_each(|i| text(i, out)),
        Value::Elem(e) => Err(format!("<{}> cannot be inside text; put it next to the <Text>", e.kind)),
        other => {
            out.push_str(&other.display());
            Ok(())
        }
    }
}

fn elem(e: &Elem) -> Result<VNode, String> {
    let kind = catalog::kind(&e.kind).expect("the compiler only builds catalog elements");
    let is_native = kind.name == "Native";
    let mut native_kind: Option<Rc<str>> = None;
    let mut props = Map::new();
    let mut handlers = Vec::new();
    let mut key = None;
    for (name, v) in &e.props {
        if is_native && &**name == "name" {
            match v {
                Value::Str(s) if catalog::valid_native_name(s) => native_kind = Some(s.clone()),
                Value::Str(s) => return Err(format!("<Native> invalid component name '{s}'")),
                other => return Err(format!("<Native> name must be a string, got a {}", other.type_name())),
            }
        } else if &**name == "key" {
            key = Some(v.display());
        } else if kind.is_event(name) {
            match v {
                Value::Func(_) | Value::Native(_) => handlers.push((name.to_string(), v.clone())),
                Value::Null => {}
                other => return Err(format!("<{}> {name} must be a function, got a {}", e.kind, other.type_name())),
            }
        } else if !matches!(v, Value::Null) {
            props.insert(name.to_string(), v.to_json().map_err(|err| format!("<{}> {name}: {err}", e.kind))?);
        }
    }
    let out_kind = if is_native {
        native_kind.ok_or_else(|| "<Native> requires a 'name' prop".to_string())?
    } else {
        e.kind.clone()
    };
    let mut children = Vec::new();
    match kind.content {
        Content::Text => {
            let mut s = String::new();
            for c in &e.children {
                text(c, &mut s)?;
            }
            if !e.children.is_empty() {
                props.insert("text".into(), Json::String(s));
            }
        }
        Content::Nodes => {
            for c in &e.children {
                nodes(c, &mut children)?;
            }
            // A labelled group around one control without a label (a form field, `tj:ui`'s
            // Field): the label names the control, for screen readers and agents alike.
            let unnamed = |n: &VNode| matches!(&*n.kind, "Input" | "Toggle") && !n.props.contains_key("label");
            if !is_native && props.contains_key("label") && children.iter().filter(|c| unnamed(c)).count() == 1 {
                let label = props.remove("label").expect("checked");
                if let Some(control) = children.iter_mut().find(|c| unnamed(c)) {
                    control.props.insert("label".into(), label);
                }
            }
        }
        Content::None => {}
    }
    Ok(VNode { kind: out_kind, key, props, handlers, children })
}

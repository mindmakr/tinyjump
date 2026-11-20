//! The retained view tree and the diff that turns a fresh render into host patches.

use crate::runtime::protocol::{PatchOp, Target};
use crate::vm::value::Value;
use indexmap::IndexMap;
use serde_json::{json, Map, Value as Json};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

/// A freshly rendered node, before ids are assigned.
pub struct VNode {
    pub kind: Rc<str>,
    pub key: Option<String>,
    pub props: Map<String, Json>,
    pub handlers: Vec<(String, Value)>,
    pub children: Vec<VNode>,
}

/// A node as the host currently has it.
pub struct Node {
    pub id: u32,
    pub kind: Rc<str>,
    pub key: Option<String>,
    pub props: Map<String, Json>,
    pub events: Vec<String>,
    pub children: Vec<Node>,
}

#[derive(Default)]
pub struct Tree {
    roots: Vec<Node>,
    handlers: HashMap<(u32, String), Value>,
    next_id: u32,
}

impl Tree {
    /// Reconciles a new render against the current tree and returns the patches.
    pub fn update(&mut self, render: Vec<VNode>) -> Vec<PatchOp> {
        let mut ops = Vec::new();
        let mut handlers = HashMap::new();
        let old = std::mem::take(&mut self.roots);
        self.roots = self.children(0, old, render, &mut ops, &mut handlers);
        self.handlers = handlers;
        ops
    }

    pub fn handler(&self, node: u32, event: &str) -> Option<&Value> {
        self.handlers.get(&(node, event.to_string()))
    }

    /// Finds a node by number, by its `id` prop, or by its visible text or label.
    pub fn find(&self, target: &Target) -> Option<u32> {
        fn walk<'a>(nodes: &'a [Node], pred: &dyn Fn(&Node) -> bool) -> Option<&'a Node> {
            nodes.iter().find_map(|n| if pred(n) { Some(n) } else { walk(&n.children, pred) })
        }
        let prop_is = |n: &Node, key: &str, want: &str| n.props.get(key).and_then(Json::as_str) == Some(want);
        let found = match target {
            Target::Node { node } => walk(&self.roots, &|n| n.id == *node),
            Target::Id { id } => walk(&self.roots, &|n| prop_is(n, "id", id)),
            Target::Text { text } => walk(&self.roots, &|n| prop_is(n, "text", text) || prop_is(n, "label", text) || prop_is(n, "placeholder", text)),
        };
        found.map(|n| n.id)
    }

    /// The nodes the host currently shows, top level first.
    pub fn roots(&self) -> &[Node] {
        &self.roots
    }

    /// Number of nodes on screen.
    pub fn node_count(&self) -> usize {
        fn count(nodes: &[Node]) -> usize {
            nodes.iter().map(|n| 1 + count(&n.children)).sum()
        }
        count(&self.roots)
    }

    /// The semantic tree for automation.
    pub fn to_json(&self) -> Json {
        fn node(n: &Node) -> Json {
            json!({ "id": n.id, "kind": &*n.kind, "props": n.props, "events": n.events, "children": n.children.iter().map(node).collect::<Vec<_>>() })
        }
        Json::Array(self.roots.iter().map(node).collect())
    }

    fn children(&mut self, parent: u32, old: Vec<Node>, render: Vec<VNode>, ops: &mut Vec<PatchOp>, handlers: &mut HashMap<(u32, String), Value>) -> Vec<Node> {
        let old_ids: Vec<u32> = old.iter().map(|n| n.id).collect();
        let mut keyed: IndexMap<String, Node> = IndexMap::new();
        let mut unkeyed: VecDeque<Node> = VecDeque::new();
        for n in old {
            match n.key.clone() {
                Some(k) => {
                    keyed.insert(k, n);
                }
                None => unkeyed.push_back(n),
            }
        }
        let mut out = Vec::with_capacity(render.len());
        for v in render {
            let candidate = match &v.key {
                Some(k) => keyed.shift_remove(k),
                None => unkeyed.pop_front(),
            };
            let node = match candidate {
                Some(o) if o.kind == v.kind => self.patch(o, v, ops, handlers),
                Some(o) => {
                    remove(&o, ops);
                    self.create(v, ops, handlers)
                }
                None => self.create(v, ops, handlers),
            };
            out.push(node);
        }
        for o in keyed.values().chain(unkeyed.iter()) {
            remove(o, ops);
        }
        let ids: Vec<u32> = out.iter().map(|n| n.id).collect();
        if ids != old_ids {
            ops.push(PatchOp::Children { id: parent, ids });
        }
        out
    }

    fn create(&mut self, v: VNode, ops: &mut Vec<PatchOp>, handlers: &mut HashMap<(u32, String), Value>) -> Node {
        self.next_id += 1;
        let id = self.next_id;
        let events: Vec<String> = v.handlers.iter().map(|(e, _)| e.clone()).collect();
        ops.push(PatchOp::Create { id, kind: v.kind.to_string(), props: v.props.clone(), events: events.clone() });
        for (e, h) in v.handlers {
            handlers.insert((id, e), h);
        }
        let children = self.children(id, Vec::new(), v.children, ops, handlers);
        Node { id, kind: v.kind, key: v.key, props: v.props, events, children }
    }

    fn patch(&mut self, old: Node, v: VNode, ops: &mut Vec<PatchOp>, handlers: &mut HashMap<(u32, String), Value>) -> Node {
        let mut changed = Map::new();
        for (k, val) in &v.props {
            if old.props.get(k) != Some(val) {
                changed.insert(k.clone(), val.clone());
            }
        }
        for k in old.props.keys() {
            if !v.props.contains_key(k) {
                changed.insert(k.clone(), Json::Null);
            }
        }
        let events: Vec<String> = v.handlers.iter().map(|(e, _)| e.clone()).collect();
        let events_changed = events != old.events;
        if !changed.is_empty() || events_changed {
            ops.push(PatchOp::Update { id: old.id, props: changed, events: events_changed.then(|| events.clone()) });
        }
        for (e, h) in v.handlers {
            handlers.insert((old.id, e), h);
        }
        let children = self.children(old.id, old.children, v.children, ops, handlers);
        Node { id: old.id, kind: v.kind, key: v.key, props: v.props, events, children }
    }
}

fn remove(n: &Node, ops: &mut Vec<PatchOp>) {
    for c in &n.children {
        remove(c, ops);
    }
    ops.push(PatchOp::Remove { id: n.id });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(key: Option<&str>, t: &str) -> VNode {
        let mut props = Map::new();
        props.insert("text".into(), json!(t));
        VNode { kind: Rc::from("Text"), key: key.map(String::from), props, handlers: Vec::new(), children: Vec::new() }
    }

    #[test]
    fn creates_then_updates_minimally() {
        let mut t = Tree::default();
        let ops = t.update(vec![text(None, "a"), text(None, "b")]);
        assert_eq!(ops.len(), 3); // 2 creates + children of root
        let ops = t.update(vec![text(None, "a"), text(None, "c")]);
        assert_eq!(ops, vec![PatchOp::Update { id: 2, props: [("text".to_string(), json!("c"))].into_iter().collect(), events: None }]);
        assert!(t.update(vec![text(None, "a"), text(None, "c")]).is_empty());
    }

    #[test]
    fn keyed_reorder_and_removal() {
        let mut t = Tree::default();
        t.update(vec![text(Some("x"), "x"), text(Some("y"), "y"), text(Some("z"), "z")]);
        let ops = t.update(vec![text(Some("z"), "z"), text(Some("x"), "x")]);
        assert_eq!(ops, vec![PatchOp::Remove { id: 2 }, PatchOp::Children { id: 0, ids: vec![3, 1] }]);
        assert_eq!(t.find(&Target::Text { text: "z".into() }), Some(3));
    }
}

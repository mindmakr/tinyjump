//! The flexbox engine (Taffy). See the parent module for the rules.

use super::{Frame, Measure};
use crate::ui::tree::{Node, Tree};
use serde_json::{Map, Value as Json};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use taffy::prelude::*;
use taffy::{compute_leaf_layout, AlignContent, AlignItems, LayoutInput, LayoutOutput, Overflow, Point as TPoint};

struct Leaf {
    id: u32,
    /// Fingerprint of the props, so cached measurements are dropped when a widget changes.
    props: u64,
}

#[derive(Default)]
pub struct Layout {
    previous: HashMap<u32, Frame>,
    cache: HashMap<(u32, u64, i32), (f32, f32)>,
}

fn is_leaf(kind: &str) -> bool {
    matches!(kind, "Text" | "Button" | "Input" | "Toggle" | "Image")
}

fn num(props: &Map<String, Json>, key: &str) -> Option<f32> {
    props.get(key).and_then(Json::as_f64).map(|v| v as f32)
}

/// A number is points/dp; `"50%"` is a percentage; anything else is automatic.
fn dimension(props: &Map<String, Json>, key: &str) -> Dimension {
    match props.get(key) {
        Some(Json::Number(n)) => Dimension::length(n.as_f64().unwrap_or(0.0) as f32),
        Some(Json::String(s)) => s.strip_suffix('%').and_then(|p| p.trim().parse::<f32>().ok()).map_or(Dimension::auto(), |p| Dimension::percent(p / 100.0)),
        _ => Dimension::auto(),
    }
}

fn style(n: &Node) -> Style {
    let p = &n.props;
    let kind = &*n.kind;
    let mut s = Style { size: Size { width: dimension(p, "width"), height: dimension(p, "height") }, ..Style::default() };
    s.flex_direction = if kind == "Row" { FlexDirection::Row } else { FlexDirection::Column };
    s.align_items = match p.get("align").and_then(Json::as_str) {
        Some("start") => AlignItems::FLEX_START,
        Some("center") => AlignItems::CENTER,
        Some("end") => AlignItems::FLEX_END,
        Some("stretch") => AlignItems::STRETCH,
        _ if kind == "Row" => AlignItems::CENTER,
        _ => AlignItems::STRETCH,
    };
    s.justify_content = match p.get("justify").and_then(Json::as_str) {
        Some("center") => AlignContent::CENTER,
        Some("end") => AlignContent::FLEX_END,
        Some("between") => AlignContent::SPACE_BETWEEN,
        Some("around") => AlignContent::SPACE_AROUND,
        _ => AlignContent::FLEX_START,
    };
    if let Some(g) = num(p, "gap") {
        s.gap = Size { width: LengthPercentage::length(g), height: LengthPercentage::length(g) };
    }
    if let Some(m) = num(p, "margin") {
        let m = LengthPercentageAuto::length(m);
        s.margin = Rect { left: m, right: m, top: m, bottom: m };
    }
    // Leaves draw their own padding natively (it is part of their measured size).
    if let (Some(pad), false) = (num(p, "padding"), is_leaf(kind)) {
        let pad = LengthPercentage::length(pad);
        s.padding = Rect { left: pad, right: pad, top: pad, bottom: pad };
    }
    if let Some(g) = num(p, "grow") {
        s.flex_grow = g;
    }
    if kind == "Spacer" {
        s.flex_grow = s.flex_grow.max(1.0);
        s.flex_basis = Dimension::length(0.0);
    }
    if kind == "Scroll" {
        s.overflow = TPoint { x: Overflow::Visible, y: Overflow::Scroll };
        s.scrollbar_width = 0.0;
    }
    s
}

fn fingerprint(props: &Map<String, Json>) -> u64 {
    let mut h = DefaultHasher::new();
    serde_json::to_string(props).unwrap_or_default().hash(&mut h);
    h.finish()
}

impl Layout {
    /// Lays the tree out in a `width` x `height` viewport and returns the frames that changed.
    pub fn compute(&mut self, tree: &Tree, width: f32, height: f32, measure: &mut dyn Measure) -> Vec<Frame> {
        let mut t: TaffyTree<Leaf> = TaffyTree::new();
        let mut ids: Vec<(NodeId, u32, bool)> = Vec::new();
        let roots: Vec<NodeId> = tree.roots().iter().map(|n| build(&mut t, n, &mut ids)).collect();
        // Like the web host's root: a full-width column at least as tall as the screen.
        let root_style = Style {
            flex_direction: FlexDirection::Column,
            size: Size { width: Dimension::length(width), height: Dimension::auto() },
            min_size: Size { width: LengthPercentageAuto::auto(), height: LengthPercentageAuto::length(height) },
            ..Style::default()
        };
        let Ok(root) = t.new_with_children(root_style, &roots) else { return Vec::new() };

        let cache = &mut self.cache;
        let result = t.compute_layout_with_measure(root, Size { width: AvailableSpace::Definite(width), height: AvailableSpace::MaxContent }, |input: LayoutInput, _, leaf: Option<&mut Leaf>, style| -> LayoutOutput {
            compute_leaf_layout(input, style, |_, _| 0.0, |known, available| {
                let Some(leaf) = leaf.as_deref() else { return Size { width: known.width.unwrap_or(0.0), height: known.height.unwrap_or(0.0) } };
                if let (Some(w), Some(h)) = (known.width, known.height) {
                    return Size { width: w, height: h };
                }
                let max_width = known.width.or(match available.width {
                    AvailableSpace::Definite(w) => Some(w),
                    AvailableSpace::MinContent => Some(0.0),
                    AvailableSpace::MaxContent => None,
                });
                let key = (leaf.id, leaf.props, max_width.map_or(-1, |w| w.round() as i32));
                let (w, h) = *cache.entry(key).or_insert_with(|| measure.measure(leaf.id, max_width));
                Size { width: known.width.unwrap_or(w), height: known.height.unwrap_or(h) }
            })
        });
        if result.is_err() {
            return Vec::new();
        }

        let mut now = HashMap::with_capacity(ids.len());
        let mut changed = Vec::new();
        for (node, id, scroll) in ids {
            let Ok(l) = t.layout(node) else { continue };
            let content = scroll.then(|| {
                let children = t.children(node).unwrap_or_default();
                let bottom = children.iter().filter_map(|c| t.layout(*c).ok()).map(|c| c.location.y + c.size.height + c.margin.bottom).fold(0.0, f32::max);
                bottom + l.padding.bottom
            });
            let f = Frame { id, x: l.location.x, y: l.location.y, w: l.size.width, h: l.size.height, content };
            if self.previous.get(&id) != Some(&f) {
                changed.push(f.clone());
            }
            now.insert(id, f);
        }
        self.previous = now;
        // Drop measurements of widgets that are gone.
        self.cache.retain(|(id, _, _), _| self.previous.contains_key(id));
        changed
    }
}

fn build(t: &mut TaffyTree<Leaf>, n: &Node, ids: &mut Vec<(NodeId, u32, bool)>) -> NodeId {
    let s = style(n);
    let node = if is_leaf(&n.kind) {
        t.new_leaf_with_context(s, Leaf { id: n.id, props: fingerprint(&n.props) })
    } else {
        let children: Vec<NodeId> = n.children.iter().map(|c| build(t, c, ids)).collect();
        t.new_with_children(s, &children)
    }
    .expect("taffy accepts new nodes");
    ids.push((node, n.id, &*n.kind == "Scroll"));
    node
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tree::VNode;
    use serde_json::json;
    use std::rc::Rc;

    /// Every character is 8 wide and a line is 20 tall; text wraps at word boundaries.
    struct Fake(HashMap<u32, String>);

    impl Measure for Fake {
        fn measure(&mut self, node: u32, max_width: Option<f32>) -> (f32, f32) {
            let text = self.0.get(&node).cloned().unwrap_or_default();
            let full = text.chars().count() as f32 * 8.0;
            match max_width {
                Some(max) if full > max => {
                    let longest = text.split(' ').map(|w| w.chars().count()).max().unwrap_or(0) as f32 * 8.0;
                    let lines = (full / max.max(longest)).ceil();
                    (max.max(longest).min(full), lines * 20.0)
                }
                _ => (full, 20.0),
            }
        }
    }

    fn v(kind: &str, props: Json, children: Vec<VNode>) -> VNode {
        VNode { kind: Rc::from(kind), key: None, props: props.as_object().cloned().unwrap_or_default(), handlers: Vec::new(), children }
    }

    fn text(t: &str, props: Json) -> VNode {
        let mut p = props.as_object().cloned().unwrap_or_default();
        p.insert("text".into(), json!(t));
        v("Text", Json::Object(p), Vec::new())
    }

    /// Renders, lays out at 300x600 and returns frames by the order nodes were created.
    fn lay(render: Vec<VNode>) -> Vec<Frame> {
        let mut tree = Tree::default();
        tree.update(render);
        let mut texts = HashMap::new();
        fn collect(nodes: &[Node], out: &mut HashMap<u32, String>) {
            for n in nodes {
                if let Some(t) = n.props.get("text").and_then(Json::as_str) {
                    out.insert(n.id, t.to_string());
                }
                collect(&n.children, out);
            }
        }
        collect(tree.roots(), &mut texts);
        let mut frames = Layout::default().compute(&tree, 300.0, 600.0, &mut Fake(texts));
        frames.sort_by_key(|f| f.id);
        frames
    }

    fn rect(f: &Frame) -> [f32; 4] {
        [f.x, f.y, f.w, f.h]
    }

    #[test]
    fn columns_stack_and_stretch_with_padding_and_gap() {
        let f = lay(vec![v("Column", json!({ "padding": 10, "gap": 5 }), vec![text("one", json!({})), text("two", json!({}))])]);
        assert_eq!(rect(&f[0]), [0.0, 0.0, 300.0, 65.0], "full width, as tall as its content (like the web)");
        assert_eq!(rect(&f[1]), [10.0, 10.0, 280.0, 20.0]);
        assert_eq!(rect(&f[2]), [10.0, 35.0, 280.0, 20.0]);
    }

    #[test]
    fn grow_takes_the_free_space_in_a_row() {
        // name | message (grow) | time — the time ends at the right edge.
        let f = lay(vec![v("Column", json!({}), vec![v("Row", json!({ "gap": 10 }), vec![
            text("ana", json!({})),
            text("hello", json!({ "grow": 1 })),
            text("21:13", json!({})),
        ])])]);
        let (name, message, time) = (&f[2], &f[3], &f[4]);
        assert_eq!((name.x, name.w), (0.0, 24.0));
        assert_eq!(message.x, 34.0);
        assert_eq!(time.x + time.w, 300.0, "the last item reaches the right edge");
        assert_eq!(message.w, 300.0 - 24.0 - 40.0 - 20.0);
    }

    #[test]
    fn long_text_wraps_inside_a_row_instead_of_overflowing() {
        let long = "a long message that needs to wrap onto several lines of text";
        let f = lay(vec![v("Column", json!({}), vec![v("Row", json!({}), vec![text(long, json!({ "grow": 1 })), text("now", json!({}))])])]);
        let (message, time) = (&f[2], &f[3]);
        assert!(message.h > 20.0, "wrapped: {message:?}");
        assert_eq!(time.x + time.w, 300.0);
    }

    #[test]
    fn justify_between_spacer_and_sizes() {
        let f = lay(vec![v("Column", json!({}), vec![
            v("Row", json!({ "justify": "between" }), vec![text("left", json!({})), text("right", json!({}))]),
            v("Row", json!({}), vec![text("a", json!({})), v("Spacer", json!({}), vec![]), text("b", json!({}))]),
            v("Column", json!({ "width": "50%", "height": 40 }), vec![]),
        ])]);
        assert_eq!(f[3].x + f[3].w, 300.0, "between pushes the last item right");
        assert_eq!(f[7].x + f[7].w, 300.0, "a spacer pushes what follows to the end");
        assert_eq!([f[8].w, f[8].h], [150.0, 40.0]);
    }

    #[test]
    fn scroll_reports_its_content_height_and_only_changes_are_returned() {
        let items: Vec<VNode> = (0..10).map(|i| text(&format!("item {i}"), json!({}))).collect();
        let mut tree = Tree::default();
        tree.update(vec![v("Scroll", json!({ "height": 100 }), items)]);
        let mut layout = Layout::default();
        let mut fake = Fake(HashMap::new());
        let first = layout.compute(&tree, 300.0, 600.0, &mut fake);
        let scroll = first.iter().find(|f| f.content.is_some()).unwrap();
        assert_eq!((scroll.h, scroll.content), (100.0, Some(200.0)));
        assert!(layout.compute(&tree, 300.0, 600.0, &mut fake).is_empty(), "nothing moved");
        assert!(!layout.compute(&tree, 320.0, 600.0, &mut fake).is_empty(), "a resize moves things");
    }
}

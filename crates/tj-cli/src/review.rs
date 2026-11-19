//! Reviews a running app from its inspect snapshot (any host): layout, accessibility and
//! contrast findings, plus the runtime's work counters. Shared by `tj review` and `tj mcp`.

use serde_json::{Map, Value as Json};

/// Smallest comfortable touch target, in points/dp (Apple HIG; Material asks for 48).
const MIN_TOUCH: f64 = 44.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug)]
pub struct Finding {
    pub severity: Severity,
    pub check: &'static str,
    pub node: String,
    pub message: String,
}

struct Walk {
    viewport_w: Option<f64>,
    findings: Vec<Finding>,
}

/// All findings, most severe first.
pub fn review(snapshot: &Json) -> Vec<Finding> {
    let mut w = Walk { viewport_w: snapshot["viewport"]["w"].as_f64(), findings: Vec::new() };
    if let Some(tree) = snapshot["tree"].as_array() {
        w.nodes(tree, "#ffffff", false);
    }
    w.findings.sort_by_key(|f| f.severity);
    w.findings
}

fn props(n: &Json) -> &Map<String, Json> {
    static EMPTY: std::sync::OnceLock<Map<String, Json>> = std::sync::OnceLock::new();
    n["props"].as_object().unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}

fn prop<'a>(n: &'a Json, key: &str) -> Option<&'a str> {
    props(n).get(key).and_then(Json::as_str).filter(|s| !s.trim().is_empty())
}

/// `Button "Add" #add`, for messages.
pub fn describe(n: &Json) -> String {
    let mut s = n["kind"].as_str().unwrap_or("?").to_string();
    if let Some(t) = prop(n, "text").or_else(|| prop(n, "placeholder")) {
        let short: String = t.chars().take(30).collect();
        s.push_str(&format!(" {short:?}"));
    }
    if let Some(id) = prop(n, "id") {
        s.push_str(&format!(" #{id}"));
    }
    s
}

struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

fn bounds(n: &Json) -> Option<Rect> {
    let b = &n["bounds"];
    Some(Rect { x: b["x"].as_f64()?, y: b["y"].as_f64()?, w: b["w"].as_f64()?, h: b["h"].as_f64()? })
}

impl Walk {
    fn add(&mut self, severity: Severity, check: &'static str, n: &Json, message: String) {
        self.findings.push(Finding { severity, check, node: describe(n), message });
    }

    fn nodes(&mut self, list: &[Json], background: &str, in_scroll: bool) {
        for n in list {
            self.node(n, background, in_scroll);
        }
        self.overlaps(list);
    }

    fn node(&mut self, n: &Json, inherited_bg: &str, in_scroll: bool) {
        let kind = n["kind"].as_str().unwrap_or("");
        let background = prop(n, "background").unwrap_or(inherited_bg).to_string();
        let interactive = matches!(kind, "Button" | "Input" | "Toggle");

        // Accessibility: everything a person can act on must say what it is.
        let named = prop(n, "text").is_some() || prop(n, "label").is_some();
        match kind {
            "Button" | "Toggle" if !named => self.add(Severity::Error, "a11y", n, "has no text or label, so screen readers cannot name it".into()),
            "Input" if !named && prop(n, "placeholder").is_none() => self.add(Severity::Warning, "a11y", n, "has no label or placeholder".into()),
            "Image" if prop(n, "label").is_none() => self.add(Severity::Warning, "a11y", n, "has no label (alt text)".into()),
            _ => {}
        }

        if let Some(r) = bounds(n) {
            if interactive && n["visible"].as_bool() != Some(false) && (r.h < MIN_TOUCH || (kind == "Button" && r.w < MIN_TOUCH)) {
                self.add(Severity::Warning, "touch", n, format!("touch target is {:.0}x{:.0}; make it at least {MIN_TOUCH:.0}x{MIN_TOUCH:.0}", r.w, r.h));
            }
            if let Some(vw) = self.viewport_w {
                if r.x + r.w > vw + 1.0 || r.x < -1.0 {
                    self.add(Severity::Error, "overflow", n, format!("spans x {:.0}..{:.0} but the screen is {vw:.0} wide", r.x, r.x + r.w));
                }
            }
        }
        let has_text = prop(n, "text").is_some();
        if has_text && !in_scroll && n["visible"].as_bool() == Some(false) {
            self.add(Severity::Warning, "visible", n, "has text but is not visible on screen (clipped, hidden or off-screen)".into());
        }

        // Contrast of text against the nearest background (WCAG AA).
        if let (Some(fg), true) = (prop(n, "color"), matches!(kind, "Text" | "Toggle" | "Input")) {
            if let (Some(a), Some(b)) = (luminance(fg), luminance(&background)) {
                let ratio = (a.max(b) + 0.05) / (a.min(b) + 0.05);
                let size = props(n).get("size").and_then(Json::as_f64).unwrap_or(16.0);
                let large = size >= 24.0 || (size >= 18.66 && prop(n, "weight") == Some("bold"));
                let needed = if large { 3.0 } else { 4.5 };
                if ratio < needed {
                    self.add(Severity::Warning, "contrast", n, format!("text {fg} on {background} has contrast {ratio:.1}:1; needs {needed}:1"));
                }
            }
        }

        if let Some(children) = n["children"].as_array() {
            self.nodes(children, &background, in_scroll || kind == "Scroll");
        }
    }

    /// Siblings in a stack never overlap unless something is broken.
    fn overlaps(&mut self, list: &[Json]) {
        let shown: Vec<(&Json, Rect)> = list.iter().filter(|n| n["visible"].as_bool() != Some(false)).filter_map(|n| bounds(n).map(|r| (n, r))).filter(|(_, r)| r.w > 0.0 && r.h > 0.0).collect();
        for (i, (a, ra)) in shown.iter().enumerate() {
            for (b, rb) in &shown[i + 1..] {
                let w = (ra.x + ra.w).min(rb.x + rb.w) - ra.x.max(rb.x);
                let h = (ra.y + ra.h).min(rb.y + rb.h) - ra.y.max(rb.y);
                if w > 1.0 && h > 1.0 {
                    self.add(Severity::Warning, "overlap", a, format!("overlaps {} by {w:.0}x{h:.0}", describe(b)));
                }
            }
        }
    }
}

/// Relative luminance of a CSS color (#rgb, #rrggbb or a basic name).
fn luminance(color: &str) -> Option<f64> {
    let hex = match color.trim().to_lowercase().as_str() {
        "black" => "000000".to_string(),
        "white" => "ffffff".to_string(),
        "red" => "ff0000".to_string(),
        "green" => "008000".to_string(),
        "blue" => "0000ff".to_string(),
        "gray" | "grey" => "808080".to_string(),
        c => {
            let h = c.strip_prefix('#')?;
            match h.len() {
                3 => h.chars().flat_map(|ch| [ch, ch]).collect(),
                6 | 8 => h[..6].to_string(),
                _ => return None,
            }
        }
    };
    let channel = |i: usize| -> Option<f64> {
        let v = f64::from(u8::from_str_radix(&hex[i..i + 2], 16).ok()?) / 255.0;
        Some(if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) })
    };
    Some(0.2126 * channel(0)? + 0.7152 * channel(2)? + 0.0722 * channel(4)?)
}

/// An indented outline of the semantic tree: compact enough for an agent's context.
pub fn outline(tree: &Json) -> String {
    fn walk(nodes: &[Json], depth: usize, out: &mut String) {
        for n in nodes {
            out.push_str(&"  ".repeat(depth));
            out.push_str(&format!("[{}] {}", n["id"], describe(n)));
            if let Some(r) = bounds(n) {
                out.push_str(&format!(" @{:.0},{:.0} {:.0}x{:.0}", r.x, r.y, r.w, r.h));
            }
            if n["visible"].as_bool() == Some(false) {
                out.push_str(" hidden");
            }
            if let Some(events) = n["events"].as_array().filter(|e| !e.is_empty()) {
                let names: Vec<&str> = events.iter().filter_map(Json::as_str).collect();
                out.push_str(&format!(" on:{}", names.join(",")));
            }
            out.push('\n');
            walk(n["children"].as_array().map_or(&[], Vec::as_slice), depth + 1, out);
        }
    }
    let mut out = String::new();
    walk(tree.as_array().map_or(&[], Vec::as_slice), 0, &mut out);
    out
}

/// A human- and agent-readable report: findings first, then the work counters.
pub fn report(snapshot: &Json, findings: &[Finding]) -> String {
    let mut out = String::new();
    if findings.is_empty() {
        out.push_str("No findings.\n");
    }
    for f in findings {
        let level = if f.severity == Severity::Error { "error" } else { "warning" };
        out.push_str(&format!("{level} [{}] {}: {}\n", f.check, f.node, f.message));
    }
    // Runtime work counters, and on a server its own: requests, latency, realtime fan-out.
    for key in ["metrics", "server"] {
        if let Some(m) = snapshot[key].as_object() {
            let counters: Vec<String> = m.iter().map(|(k, v)| format!("{k}={v}")).collect();
            out.push_str(&format!("{key}: {}\n", counters.join(" ")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: u32, kind: &str, props: Json, bounds: [f64; 4], children: Vec<Json>) -> Json {
        json!({ "id": id, "kind": kind, "props": props, "events": [], "visible": true,
                "bounds": { "x": bounds[0], "y": bounds[1], "w": bounds[2], "h": bounds[3] }, "children": children })
    }

    fn checks(snapshot: &Json) -> Vec<&'static str> {
        review(snapshot).into_iter().map(|f| f.check).collect()
    }

    #[test]
    fn a_clean_screen_has_no_findings() {
        let tree = json!([node(1, "Column", json!({}), [0.0, 0.0, 390.0, 200.0], vec![
            node(2, "Text", json!({ "text": "Todos", "color": "#111" }), [16.0, 16.0, 100.0, 30.0], vec![]),
            node(3, "Button", json!({ "text": "Add" }), [16.0, 60.0, 80.0, 44.0], vec![]),
        ])]);
        assert!(review(&json!({ "tree": tree, "viewport": { "w": 390, "h": 800 } })).is_empty());
    }

    #[test]
    fn finds_overflow_overlap_touch_contrast_and_a11y() {
        let tree = json!([node(1, "Column", json!({}), [0.0, 0.0, 420.0, 300.0], vec![
            node(2, "Text", json!({ "text": "faint", "color": "#999" }), [0.0, 0.0, 100.0, 20.0], vec![]),
            node(3, "Text", json!({ "text": "on top" }), [0.0, 10.0, 100.0, 20.0], vec![]),
            node(4, "Button", json!({}), [0.0, 40.0, 30.0, 21.0], vec![]),
        ])]);
        let found = checks(&json!({ "tree": tree, "viewport": { "w": 390, "h": 800 } }));
        for expected in ["overflow", "overlap", "touch", "contrast", "a11y"] {
            assert!(found.contains(&expected), "missing {expected} in {found:?}");
        }
        assert_eq!(review(&json!({ "tree": tree, "viewport": { "w": 390 } }))[0].severity, Severity::Error, "errors sort first");
    }

    #[test]
    fn contrast_math_matches_wcag() {
        let white = luminance("#fff").unwrap();
        let black = luminance("black").unwrap();
        assert!(((white + 0.05) / (black + 0.05) - 21.0).abs() < 0.01);
    }
}

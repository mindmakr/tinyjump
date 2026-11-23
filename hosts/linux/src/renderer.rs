//! Native GTK 3 view tree renderer mirroring `crates/tj-core/src/ui/catalog.rs`.
//! Implements `tj_core::ui::layout::Measure`, applies patches and Taffy frames, handles
//! RTL mirroring, emits UI events back to `tj-core`, and renders Cairo PNG screenshots.

use crate::gtk3::{
    self, cstr, Gtk3, GtkAllocation, GtkCssProvider, GtkRequisition, GtkWidget, CAIRO_FORMAT_ARGB32, CAIRO_STATUS_SUCCESS,
    GTK_ICON_SIZE_MENU, GTK_ORIENTATION_HORIZONTAL, GTK_POLICY_AUTOMATIC, GTK_POLICY_NEVER,
    GTK_STYLE_PROVIDER_PRIORITY_APPLICATION, GTK_TEXT_DIR_LTR, GTK_TEXT_DIR_RTL, G_FALSE, G_TRUE, PANGO_WRAP_WORD_CHAR,
};
use serde_json::{json, Map, Value as Json};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CStr;
use std::os::raw::{c_int, c_uint, c_void};
use std::rc::Rc;
use tj_core::ui::layout::Measure;

pub type UiEventCallback = Rc<dyn Fn(u32, &str, Json)>;

pub struct Node {
    pub kind: String,
    pub widget: *mut GtkWidget,
    /// Where child widgets are placed (a `GtkFixed`), or None for leaf widgets.
    pub container: Option<*mut GtkWidget>,
    /// Auxiliary widget inside a composite node (e.g. `GtkLabel` inside `Toggle` or `Text`).
    pub label_widget: Option<*mut GtkWidget>,
    pub switch_widget: Option<*mut GtkWidget>,
    pub tab_buttons: Vec<(String, *mut GtkWidget)>,
    pub css: Option<*mut GtkCssProvider>,
    pub placed_in: Option<*mut GtkWidget>,
    pub frame: (i32, i32, i32, i32),
    pub props: Map<String, Json>,
    pub events: Vec<String>,
    pub children: Vec<u32>,
}

pub struct Renderer {
    gtk: &'static Gtk3,
    pub root: *mut GtkWidget,
    nodes: HashMap<u32, Node>,
    pub rtl: bool,
    pub locale: String,
    pub viewport_w: i32,
    pub viewport_h: i32,
    on_ui: UiEventCallback,
}

struct SignalCtx {
    node_id: u32,
    tag: String,
    on_ui: UiEventCallback,
    quiet_map: Rc<RefCell<HashMap<u32, bool>>>,
    events_map: Rc<RefCell<HashMap<u32, Vec<String>>>>,
    props_map: Rc<RefCell<HashMap<u32, Map<String, Json>>>>,
    scroll_extent_map: Rc<RefCell<HashMap<u32, f64>>>,
}

pub struct SharedRenderer {
    inner: Rc<RefCell<Renderer>>,
    quiet_map: Rc<RefCell<HashMap<u32, bool>>>,
    events_map: Rc<RefCell<HashMap<u32, Vec<String>>>>,
    props_map: Rc<RefCell<HashMap<u32, Map<String, Json>>>>,
    scroll_extent_map: Rc<RefCell<HashMap<u32, f64>>>,
}

impl SharedRenderer {
    pub fn new(
        gtk: &'static Gtk3,
        root: *mut GtkWidget,
        width: i32,
        height: i32,
        rtl_override: Option<bool>,
        on_ui: UiEventCallback,
    ) -> Self {
        let quiet_map = Rc::new(RefCell::new(HashMap::new()));
        let events_map = Rc::new(RefCell::new(HashMap::new()));
        let props_map = Rc::new(RefCell::new(HashMap::new()));
        let scroll_extent_map = Rc::new(RefCell::new(HashMap::new()));
        let locale = std::env::var("LANG")
            .ok()
            .and_then(|s| s.split('.').next().map(|t| t.replace('_', "-")))
            .filter(|s| !s.is_empty() && s != "C" && s != "POSIX")
            .unwrap_or_else(|| "en-US".into());
        let rtl = rtl_override.unwrap_or_else(|| is_rtl_locale(&locale));
        let mut nodes = HashMap::new();
        nodes.insert(
            0,
            Node {
                kind: "Root".into(),
                widget: root,
                container: Some(root),
                label_widget: None,
                switch_widget: None,
                tab_buttons: Vec::new(),
                css: None,
                placed_in: None,
                frame: (0, 0, width, height),
                props: Map::new(),
                events: Vec::new(),
                children: Vec::new(),
            },
        );
        unsafe {
            (gtk.gtk_widget_set_direction)(root, if rtl { GTK_TEXT_DIR_RTL } else { GTK_TEXT_DIR_LTR });
        }
        let renderer = Renderer {
            gtk,
            root,
            nodes,
            rtl,
            locale,
            viewport_w: width,
            viewport_h: height,
            on_ui,
        };
        Self {
            inner: Rc::new(RefCell::new(renderer)),
            quiet_map,
            events_map,
            props_map,
            scroll_extent_map,
        }
    }

    pub fn clone_handle(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            quiet_map: self.quiet_map.clone(),
            events_map: self.events_map.clone(),
            props_map: self.props_map.clone(),
            scroll_extent_map: self.scroll_extent_map.clone(),
        }
    }

    pub fn set_viewport(&self, w: i32, h: i32) -> bool {
        let mut r = self.inner.borrow_mut();
        if w > 0 && h > 0 && (r.viewport_w != w || r.viewport_h != h) {
            r.viewport_w = w;
            r.viewport_h = h;
            if let Some(root_node) = r.nodes.get_mut(&0) {
                root_node.frame = (0, 0, w, h);
            }
            return true;
        }
        false
    }

    pub fn viewport(&self) -> (i32, i32) {
        let r = self.inner.borrow();
        (r.viewport_w, r.viewport_h)
    }

    pub fn rtl(&self) -> bool {
        self.inner.borrow().rtl
    }

    pub fn locale(&self) -> String {
        self.inner.borrow().locale.clone()
    }

    pub fn set_locale(&self, tag: &str) {
        let mut r = self.inner.borrow_mut();
        let trimmed = tag.trim();
        if !trimmed.is_empty() {
            r.locale = trimmed.to_string();
        }
        r.rtl = is_rtl_locale(&r.locale);
        let dir = if r.rtl { GTK_TEXT_DIR_RTL } else { GTK_TEXT_DIR_LTR };
        let xalign = if r.rtl { 1.0 } else { 0.0 };
        let gtk = r.gtk;
        let locale = r.locale.clone();
        unsafe {
            (gtk.gtk_widget_set_direction)(r.root, dir);
        }
        for n in r.nodes.values() {
            unsafe {
                (gtk.gtk_widget_set_direction)(n.widget, dir);
                if let Some(lbl) = n.label_widget {
                    (gtk.gtk_label_set_xalign)(lbl, xalign);
                }
            }
            if n.kind == "Format" {
                let txt = cstr(&format_number(&n.props, &locale));
                unsafe { (gtk.gtk_label_set_text)(n.widget, txt.as_ptr()) };
            } else if n.kind == "Time" {
                let txt = cstr(&format_time(&n.props));
                unsafe { (gtk.gtk_label_set_text)(n.widget, txt.as_ptr()) };
            }
        }
    }

    pub fn apply_patch(&self, ops: &[Json]) -> bool {
        let mut r = self.inner.borrow_mut();
        let gtk = r.gtk;
        for op in ops {
            let Some(id) = op["id"].as_u64().map(|v| v as u32) else { continue };
            match op["op"].as_str().unwrap_or("") {
                "create" => {
                    let kind = op["kind"].as_str().unwrap_or("").to_string();
                    let props = op["props"].as_object().cloned().unwrap_or_default();
                    let events: Vec<String> = op["events"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                        .unwrap_or_default();
                    let node = self.create_node(&mut r, id, &kind);
                    r.nodes.insert(id, node);
                    self.events_map.borrow_mut().insert(id, events.clone());
                    if let Some(n) = r.nodes.get_mut(&id) {
                        n.events = events;
                    }
                    self.set_props(&mut r, id, &props, true);
                }
                "update" => {
                    if let Some(ev) = op["events"].as_array() {
                        let events: Vec<String> = ev.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
                        self.events_map.borrow_mut().insert(id, events.clone());
                        if let Some(n) = r.nodes.get_mut(&id) {
                            n.events = events;
                        }
                    }
                    if let Some(props) = op["props"].as_object() {
                        self.set_props(&mut r, id, props, false);
                    }
                }
                "children" => {
                    let ids: Vec<u32> = op["ids"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|v| v.as_u64().map(|u| u as u32)).collect())
                        .unwrap_or_default();
                    let (container, prev_children) = match r.nodes.get(&id) {
                        Some(p) => (p.container, p.children.clone()),
                        None => continue,
                    };
                    let Some(fixed) = container else { continue };
                    // Each Node owns 1 strong ref via g_object_ref_sink at creation, so
                    // gtk_container_remove drops only the container's ref without destroying it.
                    for old_cid in &prev_children {
                        if !ids.contains(old_cid) {
                            if let Some(cn) = r.nodes.get_mut(old_cid) {
                                if cn.placed_in == Some(fixed) {
                                    unsafe {
                                        (gtk.gtk_container_remove)(fixed, cn.widget);
                                    }
                                    cn.placed_in = None;
                                }
                            }
                        }
                    }
                    for &cid in &ids {
                        if let Some(cn) = r.nodes.get_mut(&cid) {
                            if cn.placed_in != Some(fixed) {
                                if let Some(prev_parent) = cn.placed_in.take() {
                                    unsafe {
                                        (gtk.gtk_container_remove)(prev_parent, cn.widget);
                                    }
                                }
                                unsafe {
                                    (gtk.gtk_fixed_put)(fixed, cn.widget, cn.frame.0, cn.frame.1);
                                    (gtk.gtk_widget_show_all)(cn.widget);
                                }
                                cn.placed_in = Some(fixed);
                            }
                        }
                    }
                    if let Some(p) = r.nodes.get_mut(&id) {
                        if p.kind == "List" && ids.len() < p.children.len() {
                            self.scroll_extent_map.borrow_mut().insert(id, -1.0);
                        }
                        p.children = ids;
                    }
                }
                "remove" => {
                    self.quiet_map.borrow_mut().remove(&id);
                    self.events_map.borrow_mut().remove(&id);
                    self.props_map.borrow_mut().remove(&id);
                    self.scroll_extent_map.borrow_mut().remove(&id);
                    if let Some(mut n) = r.nodes.remove(&id) {
                        unsafe {
                            if let Some(css) = n.css.take() {
                                let ctx = (gtk.gtk_widget_get_style_context)(n.widget);
                                (gtk.gtk_style_context_remove_provider)(ctx, css);
                                (gtk.g_object_unref)(css);
                            }
                            (gtk.gtk_widget_destroy)(n.widget);
                            // Release the Node's own strong ref acquired via g_object_ref_sink.
                            (gtk.g_object_unref)(n.widget);
                        }
                    }
                }
                _ => {}
            }
        }
        !ops.is_empty()
    }

    pub fn apply_frames(&self, list: &[Json]) {
        let mut r = self.inner.borrow_mut();
        let gtk = r.gtk;
        for f in list {
            let Some(id) = f["id"].as_u64().map(|v| v as u32) else { continue };
            let x = f["x"].as_f64().unwrap_or(0.0).round() as i32;
            let y = f["y"].as_f64().unwrap_or(0.0).round() as i32;
            let w = (f["w"].as_f64().unwrap_or(0.0).round() as i32).max(1);
            let h = (f["h"].as_f64().unwrap_or(0.0).round() as i32).max(1);
            let content = f["content"].as_f64();
            if let Some(n) = r.nodes.get_mut(&id) {
                n.frame = (x, y, w, h);
                unsafe {
                    (gtk.gtk_widget_set_size_request)(n.widget, w, h);
                    if let Some(parent_fixed) = n.placed_in {
                        (gtk.gtk_fixed_move)(parent_fixed, n.widget, x, y);
                    }
                    if let (Some(c), Some(inner)) = (content, n.container) {
                        if n.kind == "Scroll" || n.kind == "List" {
                            let horiz = n.kind == "List" && n.props.get("horizontal").and_then(Json::as_bool).unwrap_or(false);
                            if horiz {
                                (gtk.gtk_scrolled_window_set_policy)(n.widget, GTK_POLICY_AUTOMATIC, GTK_POLICY_NEVER);
                                (gtk.gtk_widget_set_size_request)(inner, (c.round() as i32).max(w), h);
                            } else {
                                (gtk.gtk_scrolled_window_set_policy)(n.widget, GTK_POLICY_NEVER, GTK_POLICY_AUTOMATIC);
                                (gtk.gtk_widget_set_size_request)(inner, w, (c.round() as i32).max(h));
                            }
                        }
                    }
                }
            }
        }
        unsafe {
            (gtk.gtk_widget_show_all)(r.root);
        }
    }

    fn create_node(&self, r: &mut Renderer, id: u32, kind: &str) -> Node {
        let gtk = r.gtk;
        let dir = if r.rtl { GTK_TEXT_DIR_RTL } else { GTK_TEXT_DIR_LTR };
        let xalign = if r.rtl { 1.0 } else { 0.0 };
        let empty = cstr("");
        let (widget, container, label_widget, switch_widget) = unsafe {
            match kind {
                "Column" | "Row" | "Theme" => {
                    let fixed = (gtk.gtk_fixed_new)();
                    (fixed, Some(fixed), None, None)
                }
                "Scroll" | "List" => {
                    let sw = (gtk.gtk_scrolled_window_new)(std::ptr::null_mut(), std::ptr::null_mut());
                    (gtk.gtk_scrolled_window_set_policy)(sw, GTK_POLICY_NEVER, GTK_POLICY_AUTOMATIC);
                    let content = (gtk.gtk_fixed_new)();
                    (gtk.gtk_container_add)(sw, content);
                    if kind == "List" {
                        self.scroll_extent_map.borrow_mut().insert(id, -1.0);
                        let vadj = (gtk.gtk_scrolled_window_get_vadjustment)(sw);
                        if !vadj.is_null() {
                            let ctx = self.alloc_signal(r, id, "scroll:v");
                            let sig = cstr("value-changed");
                            (gtk.g_signal_connect_data)(vadj, sig.as_ptr(), on_scroll_changed as *const c_void, ctx as *mut c_void, Some(free_signal_ctx), 0);
                        }
                        let hadj = (gtk.gtk_scrolled_window_get_hadjustment)(sw);
                        if !hadj.is_null() {
                            let ctx = self.alloc_signal(r, id, "scroll:h");
                            let sig = cstr("value-changed");
                            (gtk.g_signal_connect_data)(hadj, sig.as_ptr(), on_scroll_changed as *const c_void, ctx as *mut c_void, Some(free_signal_ctx), 0);
                        }
                    }
                    (sw, Some(content), None, None)
                }
                "Button" => {
                    let btn = (gtk.gtk_button_new_with_label)(empty.as_ptr());
                    let ctx = self.alloc_signal(r, id, "onPress");
                    let sig = cstr("clicked");
                    (gtk.g_signal_connect_data)(btn, sig.as_ptr(), on_button_clicked as *const c_void, ctx as *mut c_void, Some(free_signal_ctx), 0);
                    (btn, None, None, None)
                }
                "Input" => {
                    let entry = (gtk.gtk_entry_new)();
                    let ctx1 = self.alloc_signal(r, id, "onChange");
                    let sig1 = cstr("changed");
                    (gtk.g_signal_connect_data)(entry, sig1.as_ptr(), on_entry_changed as *const c_void, ctx1 as *mut c_void, Some(free_signal_ctx), 0);
                    let ctx2 = self.alloc_signal(r, id, "onSubmit");
                    let sig2 = cstr("activate");
                    (gtk.g_signal_connect_data)(entry, sig2.as_ptr(), on_entry_activate as *const c_void, ctx2 as *mut c_void, Some(free_signal_ctx), 0);
                    (entry, None, None, None)
                }
                "Toggle" => {
                    let hbox = (gtk.gtk_box_new)(GTK_ORIENTATION_HORIZONTAL, 8);
                    let sw = (gtk.gtk_switch_new)();
                    let lbl = (gtk.gtk_label_new)(empty.as_ptr());
                    (gtk.gtk_label_set_line_wrap)(lbl, G_TRUE);
                    (gtk.gtk_label_set_xalign)(lbl, xalign);
                    (gtk.gtk_box_pack_start)(hbox, sw, G_FALSE, G_FALSE, 0);
                    (gtk.gtk_box_pack_start)(hbox, lbl, G_TRUE, G_TRUE, 0);
                    let ctx = self.alloc_signal(r, id, "onChange");
                    let sig = cstr("state-set");
                    (gtk.g_signal_connect_data)(sw, sig.as_ptr(), on_switch_state_set as *const c_void, ctx as *mut c_void, Some(free_signal_ctx), 0);
                    (hbox, None, Some(lbl), Some(sw))
                }
                "Tabs" => {
                    let hbox = (gtk.gtk_box_new)(GTK_ORIENTATION_HORIZONTAL, 4);
                    (hbox, None, None, None)
                }
                "Icon" | "Image" => {
                    let icon = cstr("image-missing");
                    let img = (gtk.gtk_image_new_from_icon_name)(icon.as_ptr(), GTK_ICON_SIZE_MENU);
                    (img, None, None, None)
                }
                "Text" | "Time" | "Format" => {
                    let lbl = (gtk.gtk_label_new)(empty.as_ptr());
                    (gtk.gtk_label_set_line_wrap)(lbl, G_TRUE);
                    (gtk.gtk_label_set_line_wrap_mode)(lbl, PANGO_WRAP_WORD_CHAR);
                    (gtk.gtk_label_set_xalign)(lbl, xalign);
                    (lbl, None, Some(lbl), None)
                }
                "Spacer" => {
                    let fixed = (gtk.gtk_fixed_new)();
                    (fixed, None, None, None)
                }
                other => {
                    let tag = cstr(&format!("<{other}>"));
                    let lbl = (gtk.gtk_label_new)(tag.as_ptr());
                    (lbl, None, Some(lbl), None)
                }
            }
        };
        unsafe {
            // Take explicit ownership of the floating GObject ref so reparent/detach/remove stay balanced.
            (gtk.g_object_ref_sink)(widget);
            (gtk.gtk_widget_set_direction)(widget, dir);
        }
        Node {
            kind: kind.to_string(),
            widget,
            container,
            label_widget,
            switch_widget,
            tab_buttons: Vec::new(),
            css: None,
            placed_in: None,
            frame: (0, 0, 0, 0),
            props: Map::new(),
            events: Vec::new(),
            children: Vec::new(),
        }
    }

    fn alloc_signal(&self, r: &Renderer, id: u32, tag: &str) -> *mut SignalCtx {
        Box::into_raw(Box::new(SignalCtx {
            node_id: id,
            tag: tag.to_string(),
            on_ui: r.on_ui.clone(),
            quiet_map: self.quiet_map.clone(),
            events_map: self.events_map.clone(),
            props_map: self.props_map.clone(),
            scroll_extent_map: self.scroll_extent_map.clone(),
        }))
    }

    fn set_props(&self, r: &mut Renderer, id: u32, patch: &Map<String, Json>, is_create: bool) {
        self.quiet_map.borrow_mut().insert(id, true);
        let gtk = r.gtk;
        let locale = r.locale.clone();
        let kind;
        let widget;
        let label_widget;
        let switch_widget;
        {
            let Some(n) = r.nodes.get_mut(&id) else {
                self.quiet_map.borrow_mut().insert(id, false);
                return;
            };
            for (k, v) in patch {
                if v.is_null() {
                    n.props.remove(k);
                } else {
                    n.props.insert(k.clone(), v.clone());
                }
            }
            self.props_map.borrow_mut().insert(id, n.props.clone());
            kind = n.kind.clone();
            widget = n.widget;
            label_widget = n.label_widget;
            switch_widget = n.switch_widget;
        }

        let props = r.nodes[&id].props.clone();
        unsafe {
            // Common props applicable across all widget kinds.
            if let Some(v) = patch.get("id") {
                let cs = cstr(&json_str(v));
                (gtk.gtk_widget_set_name)(widget, cs.as_ptr());
            }
            if let Some(v) = patch.get("disabled") {
                let sensitive = if v.as_bool().unwrap_or(false) { G_FALSE } else { G_TRUE };
                (gtk.gtk_widget_set_sensitive)(widget, sensitive);
            }
            if let Some(v) = patch.get("lines") {
                if let Some(lbl) = label_widget {
                    (gtk.gtk_label_set_lines)(lbl, v.as_i64().unwrap_or(0) as c_int);
                }
            }

            if kind == "Time" {
                let s = cstr(&format_time(&props));
                (gtk.gtk_label_set_text)(widget, s.as_ptr());
            } else if kind == "Format" {
                let s = cstr(&format_number(&props, &locale));
                (gtk.gtk_label_set_text)(widget, s.as_ptr());
            } else if kind == "Icon" {
                let name = props.get("name").and_then(Json::as_str).unwrap_or("");
                let sym = cstr(gtk_icon_name(name));
                (gtk.gtk_image_set_from_icon_name)(widget, sym.as_ptr(), GTK_ICON_SIZE_MENU);
                let sz = props.get("size").and_then(Json::as_f64).unwrap_or(20.0).round() as i32;
                (gtk.gtk_image_set_pixel_size)(widget, sz.max(8));
            } else if kind == "Tabs" {
                if is_create || patch.contains_key("items") {
                    self.rebuild_tabs(r, id);
                } else if patch.contains_key("value") {
                    self.sync_tab_selection(r, id);
                }
            } else {
                let mut reformat_input = false;
                for (k, v) in patch {
                    match k.as_str() {
                        "text" => {
                            let s = json_str(v);
                            let cs = cstr(&s);
                            if kind == "Button" {
                                (gtk.gtk_button_set_label)(widget, cs.as_ptr());
                            } else if let Some(lbl) = label_widget {
                                (gtk.gtk_label_set_text)(lbl, cs.as_ptr());
                            }
                        }
                        "value" => {
                            if let Some(sw) = switch_widget {
                                (gtk.gtk_switch_set_active)(sw, if v.as_bool().unwrap_or(false) { G_TRUE } else { G_FALSE });
                            } else if kind == "Input" {
                                reformat_input = true;
                            }
                        }
                        "type" | "zone" if kind == "Input" => {
                            reformat_input = true;
                        }
                        "placeholder" if kind == "Input" => {
                            let cs = cstr(&json_str(v));
                            (gtk.gtk_entry_set_placeholder_text)(widget, cs.as_ptr());
                        }
                        "secure" if kind == "Input" => {
                            (gtk.gtk_entry_set_visibility)(widget, if v.as_bool().unwrap_or(false) { G_FALSE } else { G_TRUE });
                        }
                        _ => {}
                    }
                }
                if reformat_input {
                    let text = if is_typed_input(&props) {
                        format_typed_input(&props)
                    } else {
                        props.get("value").map(json_str).unwrap_or_default()
                    };
                    let cur = CStr::from_ptr((gtk.gtk_entry_get_text)(widget)).to_string_lossy();
                    if cur != text {
                        let cs = cstr(&text);
                        (gtk.gtk_entry_set_text)(widget, cs.as_ptr());
                    }
                }
            }
        }

        if patch.keys().any(|k| {
            matches!(
                k.as_str(),
                "background" | "color" | "radius" | "border" | "size" | "weight" | "padding" | "surface" | "mode"
            )
        }) {
            apply_css(r, id);
        }

        self.quiet_map.borrow_mut().insert(id, false);
    }

    fn sync_tab_selection(&self, r: &Renderer, id: u32) {
        let gtk = r.gtk;
        let Some(n) = r.nodes.get(&id) else { return };
        let selected = n.props.get("value").map(json_str).unwrap_or_default();
        for (val, btn) in &n.tab_buttons {
            unsafe {
                (gtk.gtk_toggle_button_set_active)(*btn, if *val == selected { G_TRUE } else { G_FALSE });
            }
        }
    }

    fn rebuild_tabs(&self, r: &mut Renderer, id: u32) {
        let gtk = r.gtk;
        let (box_widget, old_buttons, items, selected) = {
            let Some(n) = r.nodes.get_mut(&id) else { return };
            let old = std::mem::take(&mut n.tab_buttons);
            let items = n.props.get("items").and_then(Json::as_array).cloned().unwrap_or_default();
            let sel = n.props.get("value").map(json_str).unwrap_or_default();
            (n.widget, old, items, sel)
        };
        unsafe {
            for (_, btn) in old_buttons {
                (gtk.gtk_widget_destroy)(btn);
            }
        }
        let mut new_buttons = Vec::new();
        for item in items {
            let (val, label) = if let Some(obj) = item.as_object() {
                let v = obj.get("value").map(json_str).unwrap_or_default();
                let l = obj.get("label").map(json_str).filter(|s| !s.is_empty()).unwrap_or_else(|| v.clone());
                (v, l)
            } else {
                let s = json_str(&item);
                (s.clone(), s)
            };
            let cs = cstr(&label);
            let btn = unsafe { (gtk.gtk_toggle_button_new_with_label)(cs.as_ptr()) };
            unsafe {
                (gtk.gtk_toggle_button_set_active)(btn, if val == selected { G_TRUE } else { G_FALSE });
                (gtk.gtk_box_pack_start)(box_widget, btn, G_TRUE, G_TRUE, 0);
                (gtk.gtk_widget_show)(btn);
            }
            let ctx = self.alloc_signal(r, id, &format!("tab:{val}"));
            let sig = cstr("toggled");
            unsafe {
                (gtk.g_signal_connect_data)(btn, sig.as_ptr(), on_tab_toggled as *const c_void, ctx as *mut c_void, Some(free_signal_ctx), 0);
            }
            new_buttons.push((val, btn));
        }
        if let Some(n) = r.nodes.get_mut(&id) {
            n.tab_buttons = new_buttons;
        }
    }

    pub fn for_agent(&self, mut input: Json) -> Json {
        if input["type"].as_str() != Some("ui") {
            return input;
        }
        let Some(text) = input["value"].as_str().map(str::to_string) else { return input };
        let r = self.inner.borrow();
        let target = &input["target"];
        let node = if let Some(nid) = target["node"].as_u64() {
            r.nodes.get(&(nid as u32))
        } else if let Some(id_str) = target["id"].as_str() {
            r.nodes.values().find(|n| n.props.get("id").and_then(Json::as_str) == Some(id_str))
        } else if let Some(txt) = target["text"].as_str() {
            r.nodes.values().find(|n| {
                n.props.get("label").and_then(Json::as_str) == Some(txt)
                    || n.props.get("placeholder").and_then(Json::as_str) == Some(txt)
            })
        } else {
            None
        };
        if let Some(n) = node {
            if n.kind == "Input" && is_typed_input(&n.props) {
                input["value"] = parse_typed_input(&n.props, &text);
            }
        }
        input
    }

    pub fn add_bounds(&self, tree: &Json) -> Json {
        let r = self.inner.borrow();
        add_bounds_rec(&r, tree, (0, 0))
    }

    pub fn screenshot(&self) -> Result<Vec<u8>, String> {
        let (gtk, root, w, h) = {
            let r = self.inner.borrow();
            (r.gtk, r.root, r.viewport_w.max(1), r.viewport_h.max(1))
        };
        let mut png = Vec::<u8>::new();
        unsafe {
            // Flush pending GTK events without holding RefCell borrow (avoids re-entrancy).
            for _ in 0..32 {
                if (gtk.gtk_events_pending)() == 0 {
                    break;
                }
                (gtk.gtk_main_iteration_do)(G_FALSE);
            }
            // Ensure root and descendants have a valid size allocation before drawing.
            let mut min_req = GtkRequisition::default();
            let mut nat_req = GtkRequisition::default();
            (gtk.gtk_widget_get_preferred_size)(root, &mut min_req, &mut nat_req);
            let mut alloc = GtkAllocation { x: 0, y: 0, width: w, height: h };
            (gtk.gtk_widget_size_allocate)(root, &mut alloc);

            let surface = (gtk.cairo_image_surface_create)(CAIRO_FORMAT_ARGB32, w, h);
            if surface.is_null() {
                return Err("cairo_image_surface_create returned null".into());
            }
            let cr = (gtk.cairo_create)(surface);
            // Paint opaque white canvas before drawing widgets so screenshots are never transparent.
            (gtk.cairo_set_source_rgb)(cr, 1.0, 1.0, 1.0);
            (gtk.cairo_paint)(cr);
            (gtk.gtk_widget_draw)(root, cr);
            (gtk.cairo_destroy)(cr);
            let status = (gtk.cairo_surface_write_to_png_stream)(
                surface,
                cairo_write_vec,
                &mut png as *mut Vec<u8> as *mut c_void,
            );
            (gtk.cairo_surface_destroy)(surface);
            if status != CAIRO_STATUS_SUCCESS || png.is_empty() {
                return Err(format!("PNG capture failed (cairo status {status})"));
            }
        }
        Ok(png)
    }
}

impl Measure for SharedRenderer {
    fn measure(&mut self, node: u32, max_width: Option<f32>) -> (f32, f32) {
        let r = self.inner.borrow();
        let Some(n) = r.nodes.get(&node) else { return (0.0, 0.0) };
        let gtk = r.gtk;
        unsafe {
            if n.kind == "Icon" {
                let sz = n.props.get("size").and_then(Json::as_f64).unwrap_or(20.0) as f32;
                return (sz, sz);
            }
            // Temporarily clear explicit size request so GTK measures natural content size.
            (gtk.gtk_widget_set_size_request)(n.widget, -1, -1);
            let mut min_w: c_int = 0;
            let mut nat_w: c_int = 0;
            (gtk.gtk_widget_get_preferred_width)(n.widget, &mut min_w, &mut nat_w);
            let mut width = (nat_w.max(min_w).max(1)) as f32;
            if let Some(mw) = max_width {
                width = width.min(mw.max(1.0));
            }
            let mut min_h: c_int = 0;
            let mut nat_h: c_int = 0;
            (gtk.gtk_widget_get_preferred_height_for_width)(n.widget, width.ceil() as c_int, &mut min_h, &mut nat_h);
            let mut height = nat_h.max(min_h).max(1) as f32;
            if matches!(n.kind.as_str(), "Button" | "Input" | "Toggle" | "Tabs") {
                height = height.max(44.0);
                if n.kind == "Input" {
                    width = width.max(120.0).min(max_width.unwrap_or(width.max(120.0)));
                }
            }
            // Restore previous explicit frame size request if laid out.
            if n.frame.2 > 0 && n.frame.3 > 0 {
                (gtk.gtk_widget_set_size_request)(n.widget, n.frame.2, n.frame.3);
            }
            (width.ceil() + 2.0, height.ceil() + 1.0)
        }
    }
}

fn add_bounds_rec(r: &Renderer, tree: &Json, parent_origin: (i32, i32)) -> Json {
    let Some(arr) = tree.as_array() else { return tree.clone() };
    let gtk = r.gtk;
    let out: Vec<Json> = arr
        .iter()
        .map(|node| {
            let mut obj = node.clone();
            let mut next_origin = parent_origin;
            if let Some(id) = node["id"].as_u64().map(|v| v as u32) {
                if let Some(n) = r.nodes.get(&id) {
                    if n.kind == "Time" || n.kind == "Format" {
                        let shown = if n.kind == "Time" {
                            format_time(&n.props)
                        } else {
                            format_number(&n.props, &r.locale)
                        };
                        if let Some(p) = obj["props"].as_object_mut() {
                            p.insert("text".into(), json!(shown));
                        }
                    }
                    let x = parent_origin.0 + n.frame.0;
                    let y = parent_origin.1 + n.frame.1;
                    let w = n.frame.2;
                    let h = n.frame.3;
                    let visible = w > 0 && h > 0 && x < r.viewport_w && y < r.viewport_h && (x + w) > 0 && (y + h) > 0;
                    obj["bounds"] = json!({ "x": x, "y": y, "w": w, "h": h });
                    obj["visible"] = json!(visible);
                    let mut sx = 0;
                    let mut sy = 0;
                    if n.kind == "Scroll" || n.kind == "List" {
                        unsafe {
                            let hadj = (gtk.gtk_scrolled_window_get_hadjustment)(n.widget);
                            if !hadj.is_null() {
                                sx = (gtk.gtk_adjustment_get_value)(hadj).round() as i32;
                            }
                            let vadj = (gtk.gtk_scrolled_window_get_vadjustment)(n.widget);
                            if !vadj.is_null() {
                                sy = (gtk.gtk_adjustment_get_value)(vadj).round() as i32;
                            }
                        }
                    }
                    next_origin = (x - sx, y - sy);
                }
            }
            if let Some(ch) = node.get("children") {
                obj["children"] = add_bounds_rec(r, ch, next_origin);
            }
            obj
        })
        .collect();
    Json::Array(out)
}

fn apply_css(r: &mut Renderer, id: u32) {
    let gtk = r.gtk;
    let Some(n) = r.nodes.get_mut(&id) else { return };
    let mut rules = String::new();
    let mut bg = n.props.get("background").and_then(Json::as_str).map(str::to_string);
    let mut fg = n.props.get("color").and_then(Json::as_str).map(str::to_string);
    if n.kind == "Theme" {
        if bg.is_none() {
            if let Some(s) = n.props.get("surface").and_then(Json::as_str) {
                bg = Some(s.to_string());
            } else if let Some(m) = n.props.get("mode").and_then(Json::as_str) {
                bg = Some(if m == "dark" { "#111827".into() } else { "#ffffff".into() });
            }
        }
        if fg.is_none() && n.props.get("mode").and_then(Json::as_str) == Some("dark") {
            fg = Some("#f9fafb".into());
        }
    }
    if let Some(c) = bg.as_deref().and_then(safe_css_color) {
        rules.push_str(&format!("background-color: {c}; background-image: none; "));
    }
    if let Some(c) = fg.as_deref().and_then(safe_css_color) {
        rules.push_str(&format!("color: {c}; "));
    }
    if let Some(rad) = n.props.get("radius").and_then(Json::as_f64) {
        rules.push_str(&format!("border-radius: {}px; ", rad.max(0.0)));
    }
    if let Some(b) = n.props.get("border") {
        if let Some(w) = b.as_f64() {
            rules.push_str(&format!("border: {}px solid currentColor; ", w.max(0.0)));
        } else if let Some(spec) = b.as_str() {
            if !spec.contains(';') && !spec.contains('{') && !spec.contains('}') {
                let normalized = normalize_border_css(spec);
                rules.push_str(&format!("border: {normalized}; "));
            }
        }
    }
    if let Some(sz) = n.props.get("size").and_then(Json::as_f64) {
        rules.push_str(&format!("font-size: {}px; ", sz.clamp(6.0, 200.0)));
    }
    if let Some(w) = n.props.get("weight") {
        let bold = w.as_str().is_some_and(|s| s == "bold" || s == "bolder") || w.as_f64().is_some_and(|v| v >= 600.0);
        rules.push_str(if bold { "font-weight: bold; " } else { "font-weight: normal; " });
    }
    if let Some(pad) = n.props.get("padding").and_then(Json::as_f64) {
        if n.container.is_none() {
            rules.push_str(&format!("padding: {}px; ", pad.max(0.0)));
        }
    }

    unsafe {
        let ctx = (gtk.gtk_widget_get_style_context)(n.widget);
        if let Some(old) = n.css.take() {
            (gtk.gtk_style_context_remove_provider)(ctx, old);
            (gtk.g_object_unref)(old);
        }
        if !rules.is_empty() {
            let provider = (gtk.gtk_css_provider_new)();
            let css_text = format!("* {{ {rules} }}");
            let c_css = cstr(&css_text);
            (gtk.gtk_css_provider_load_from_data)(provider, c_css.as_ptr(), -1, std::ptr::null_mut());
            (gtk.gtk_style_context_add_provider)(ctx, provider, GTK_STYLE_PROVIDER_PRIORITY_APPLICATION);
            n.css = Some(provider);
        }
    }
}

fn normalize_border_css(spec: &str) -> String {
    let parts: Vec<&str> = spec.split_whitespace().collect();
    let has_style = parts
        .iter()
        .any(|p| matches!(*p, "solid" | "dashed" | "dotted" | "double" | "none" | "groove" | "ridge" | "inset" | "outset"));
    if !has_style && parts.len() == 2 {
        format!("{} solid {}", parts[0], parts[1])
    } else {
        spec.trim().to_string()
    }
}

fn safe_css_color(s: &str) -> Option<&str> {
    let t = s.trim();
    if t.is_empty() || t.len() > 32 {
        return None;
    }
    if t.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'#' | b'-' | b'(' | b')' | b',' | b'.' | b' ' | b'%')) {
        Some(t)
    } else {
        None
    }
}

fn json_str(v: &Json) -> String {
    match v {
        Json::Null => String::new(),
        Json::String(s) => s.clone(),
        other => other.to_string(),
    }
}

pub fn is_rtl_locale(tag: &str) -> bool {
    let lang = tag.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
    matches!(lang.as_str(), "ar" | "he" | "fa" | "ur" | "ps" | "sd" | "ug" | "yi" | "dv" | "ku")
}

fn is_typed_input(props: &Map<String, Json>) -> bool {
    matches!(props.get("type").and_then(Json::as_str), Some("number" | "date" | "time" | "datetime"))
}

fn format_typed_input(props: &Map<String, Json>) -> String {
    let ty = props.get("type").and_then(Json::as_str).unwrap_or("");
    let Some(ms) = props.get("value").and_then(Json::as_f64).filter(|v| v.is_finite()) else { return String::new() };
    if ty == "number" {
        if ms.fract() == 0.0 && ms.abs() < 1e15 {
            format!("{}", ms as i64)
        } else {
            format!("{ms}")
        }
    } else {
        iso_from_ms(ms, ty)
    }
}

/// Parses typed `<Input>` text into the canonical protocol value (`number | null`):
/// - `number`: finite f64
/// - `date`: `YYYY-MM-DD` -> UTC midnight epoch ms
/// - `time`: `HH:MM` -> ms since midnight
/// - `datetime`: `YYYY-MM-DD[ T]HH:MM` -> UTC epoch ms
fn parse_typed_input(props: &Map<String, Json>, text: &str) -> Json {
    let ty = props.get("type").and_then(Json::as_str).unwrap_or("");
    let v = text.trim();
    if v.is_empty() {
        return Json::Null;
    }
    match ty {
        "number" => v.parse::<f64>().ok().filter(|d| d.is_finite()).map(|d| json!(d)).unwrap_or(Json::Null),
        "date" => parse_iso_date(v).map(|days| json!((days as f64) * 86_400_000.0)).unwrap_or(Json::Null),
        "time" => parse_iso_time(v).map(|ms| json!(ms as f64)).unwrap_or(Json::Null),
        "datetime" => parse_iso_datetime(v).map(|ms| json!(ms as f64)).unwrap_or(Json::Null),
        _ => Json::String(v.to_string()),
    }
}

fn parse_iso_date(s: &str) -> Option<i64> {
    let mut parts = s.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(-9999..=99999).contains(&y) || !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}

fn parse_iso_time(s: &str) -> Option<i64> {
    let mut parts = s.split(':');
    let hh: i64 = parts.next()?.parse().ok()?;
    let mm: i64 = parts.next()?.parse().ok()?;
    let ss: i64 = match parts.next() {
        Some(sec) => sec.parse().ok()?,
        None => 0,
    };
    if parts.next().is_some() || !(0..24).contains(&hh) || !(0..60).contains(&mm) || !(0..60).contains(&ss) {
        return None;
    }
    Some((hh * 3600 + mm * 60 + ss) * 1000)
}

fn parse_iso_datetime(s: &str) -> Option<i64> {
    let (date_part, time_part) = s.split_once('T').or_else(|| s.split_once(' '))?;
    let time_clean = time_part.trim_end_matches('Z');
    let days = parse_iso_date(date_part)?;
    let ms_of_day = parse_iso_time(time_clean)?;
    days.checked_mul(86_400_000)?.checked_add(ms_of_day)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => if is_leap(y) { 29 } else { 28 },
        _ => 0,
    }
}

/// Howard Hinnant's civil-to-days algorithm (UTC Unix epoch days).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m as i64 - 3 } else { m as i64 + 9 };
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn format_time(props: &Map<String, Json>) -> String {
    let Some(ms) = props.get("value").and_then(Json::as_f64).filter(|v| v.is_finite()) else { return String::new() };
    let show = props.get("show").and_then(Json::as_str).unwrap_or("datetime");
    iso_from_ms(ms, show)
}

fn iso_from_ms(ms: f64, show: &str) -> String {
    let total_secs = (ms / 1000.0).floor() as i64;
    let days = total_secs.div_euclid(86400);
    let rem = total_secs.rem_euclid(86400);
    let hh = rem / 3600;
    let mm = (rem % 3600) / 60;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    match show {
        "date" => format!("{y:04}-{m:02}-{d:02}"),
        "time" => format!("{hh:02}:{mm:02}"),
        _ => format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}"),
    }
}

fn format_number(props: &Map<String, Json>, _locale: &str) -> String {
    let Some(v) = props.get("value").and_then(Json::as_f64).filter(|x| x.is_finite()) else { return String::new() };
    let kind = props.get("kind").and_then(Json::as_str).unwrap_or("number");
    let dec = props.get("decimals").and_then(Json::as_u64).map(|d| d.min(20) as usize);
    match kind {
        "currency" => {
            let cur = props.get("currency").and_then(Json::as_str).filter(|s| !s.is_empty()).unwrap_or("USD");
            let d = dec.unwrap_or(2);
            format!("{cur} {v:.d$}")
        }
        "percent" => {
            let d = dec.unwrap_or(0);
            format!("{:.d$}%", v * 100.0)
        }
        _ => match dec {
            Some(d) => format!("{v:.d$}"),
            None => format!("{v}"),
        },
    }
}

fn gtk_icon_name(name: &str) -> &'static str {
    match name {
        "check" => "emblem-ok-symbolic",
        "plus" => "list-add-symbolic",
        "close" => "window-close-symbolic",
        "search" => "system-search-symbolic",
        "home" => "go-home-symbolic",
        "user" => "avatar-default-symbolic",
        "settings" => "emblem-system-symbolic",
        "back" => "go-previous-symbolic",
        "next" => "go-next-symbolic",
        "edit" => "document-edit-symbolic",
        "trash" => "user-trash-symbolic",
        "calendar" => "x-office-calendar-symbolic",
        "clock" => "alarm-symbolic",
        "bell" => "preferences-system-notifications-symbolic",
        "mail" => "mail-unread-symbolic",
        "star" => "starred-symbolic",
        "heart" => "emblem-favorite-symbolic",
        "info" => "dialog-information-symbolic",
        "warning" => "dialog-warning-symbolic",
        "lock" => "changes-prevent-symbolic",
        "menu" => "open-menu-symbolic",
        "share" => "emblem-shared-symbolic",
        "refresh" => "view-refresh-symbolic",
        "download" => "folder-download-symbolic",
        _ => "dialog-information-symbolic",
    }
}

fn fire_if_allowed(ctx: &SignalCtx, event: &str, value: Json) {
    if ctx.quiet_map.borrow().get(&ctx.node_id).copied().unwrap_or(false) {
        return;
    }
    let allowed = ctx.events_map.borrow().get(&ctx.node_id).is_some_and(|e| e.iter().any(|s| s == event));
    if allowed {
        (ctx.on_ui)(ctx.node_id, event, value);
    }
}

unsafe extern "C" fn free_signal_ctx(data: *mut c_void, _closure: *mut c_void) {
    if !data.is_null() {
        drop(Box::from_raw(data as *mut SignalCtx));
    }
}

unsafe extern "C" fn on_button_clicked(_btn: *mut GtkWidget, data: *mut c_void) {
    let ctx = &*(data as *const SignalCtx);
    fire_if_allowed(ctx, "onPress", Json::Null);
}

fn entry_value_for(ctx: &SignalCtx, text: &str) -> Json {
    ctx.props_map
        .borrow()
        .get(&ctx.node_id)
        .filter(|p| is_typed_input(p))
        .map(|p| parse_typed_input(p, text))
        .unwrap_or_else(|| json!(text))
}

unsafe extern "C" fn on_entry_changed(entry: *mut GtkWidget, data: *mut c_void) {
    let ctx = &*(data as *const SignalCtx);
    let Ok(gtk) = gtk3::load() else { return };
    let ptr = (gtk.gtk_entry_get_text)(entry);
    let text = if ptr.is_null() { String::new() } else { CStr::from_ptr(ptr).to_string_lossy().into_owned() };
    fire_if_allowed(ctx, "onChange", entry_value_for(ctx, &text));
}

unsafe extern "C" fn on_entry_activate(entry: *mut GtkWidget, data: *mut c_void) {
    let ctx = &*(data as *const SignalCtx);
    let Ok(gtk) = gtk3::load() else { return };
    let ptr = (gtk.gtk_entry_get_text)(entry);
    let text = if ptr.is_null() { String::new() } else { CStr::from_ptr(ptr).to_string_lossy().into_owned() };
    fire_if_allowed(ctx, "onSubmit", entry_value_for(ctx, &text));
}

unsafe extern "C" fn on_switch_state_set(_sw: *mut GtkWidget, state: c_int, data: *mut c_void) -> c_int {
    let ctx = &*(data as *const SignalCtx);
    fire_if_allowed(ctx, "onChange", json!(state != 0));
    G_FALSE
}

unsafe extern "C" fn on_tab_toggled(btn: *mut GtkWidget, data: *mut c_void) {
    let ctx = &*(data as *const SignalCtx);
    if ctx.quiet_map.borrow().get(&ctx.node_id).copied().unwrap_or(false) {
        return;
    }
    let Ok(gtk) = gtk3::load() else { return };
    let active = (gtk.gtk_toggle_button_get_active)(btn) != 0;
    let Some(val) = ctx.tag.strip_prefix("tab:") else { return };
    if active {
        fire_if_allowed(ctx, "onChange", json!(val));
    } else {
        // Keep the active tab selected if clicked again instead of leaving zero tabs selected.
        let selected = ctx
            .props_map
            .borrow()
            .get(&ctx.node_id)
            .and_then(|p| p.get("value"))
            .map(json_str)
            .unwrap_or_default();
        if selected == val {
            ctx.quiet_map.borrow_mut().insert(ctx.node_id, true);
            (gtk.gtk_toggle_button_set_active)(btn, G_TRUE);
            ctx.quiet_map.borrow_mut().insert(ctx.node_id, false);
        }
    }
}

unsafe extern "C" fn on_scroll_changed(adj: *mut c_void, data: *mut c_void) {
    let ctx = &*(data as *const SignalCtx);
    let Ok(gtk) = gtk3::load() else { return };
    let horiz = ctx
        .props_map
        .borrow()
        .get(&ctx.node_id)
        .and_then(|p| p.get("horizontal"))
        .and_then(Json::as_bool)
        .unwrap_or(false);
    let want_tag = if horiz { "scroll:h" } else { "scroll:v" };
    if ctx.tag != want_tag {
        return;
    }
    let value = (gtk.gtk_adjustment_get_value)(adj);
    let page = (gtk.gtk_adjustment_get_page_size)(adj);
    let upper = (gtk.gtk_adjustment_get_upper)(adj);
    if upper > page && (value + page) >= (upper - 64.0) {
        let mut extents = ctx.scroll_extent_map.borrow_mut();
        let last = extents.get(&ctx.node_id).copied().unwrap_or(-1.0);
        if (upper - last).abs() > 1.0 {
            extents.insert(ctx.node_id, upper);
            drop(extents);
            fire_if_allowed(ctx, "onEndReached", Json::Null);
        }
    }
}

unsafe extern "C" fn cairo_write_vec(closure: *mut c_void, data: *const u8, len: c_uint) -> c_int {
    if !closure.is_null() && !data.is_null() && len > 0 {
        let buf = &mut *(closure as *mut Vec<u8>);
        buf.extend_from_slice(std::slice::from_raw_parts(data, len as usize));
    }
    CAIRO_STATUS_SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typed_dates_times_and_rejects_invalid_calendar_dates() {
        let mut d = Map::new();
        d.insert("type".into(), json!("date"));
        assert_eq!(parse_typed_input(&d, "2026-11-23"), json!(1_795_392_000_000.0));
        assert_eq!(parse_typed_input(&d, "2026-02-30"), Json::Null);
        assert_eq!(parse_typed_input(&d, "2024-02-29"), json!(1_709_164_800_000.0));
        assert_eq!(parse_typed_input(&d, "2025-02-29"), Json::Null);

        let mut t = Map::new();
        t.insert("type".into(), json!("time"));
        assert_eq!(parse_typed_input(&t, "09:30"), json!(34_200_000.0));
        assert_eq!(parse_typed_input(&t, "25:00"), Json::Null);

        let mut dt = Map::new();
        dt.insert("type".into(), json!("datetime"));
        assert_eq!(parse_typed_input(&dt, "2026-11-23T09:30"), json!(1_795_426_200_000.0));
        assert_eq!(parse_typed_input(&dt, "2026-11-23 09:30"), json!(1_795_426_200_000.0));
        assert_eq!(parse_typed_input(&dt, "99999999999999-01-01 00:00"), Json::Null);
        assert_eq!(parse_typed_input(&dt, ""), Json::Null);
    }

    #[test]
    fn normalizes_shorthand_border_css() {
        assert_eq!(normalize_border_css("2px #ccc"), "2px solid #ccc");
        assert_eq!(normalize_border_css("1px dashed #333"), "1px dashed #333");
    }
}

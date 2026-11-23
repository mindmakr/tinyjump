//! Native GTK 3 Linux desktop host for tinyjump.
//! Runs `.tj` or precompiled binary AOT `.tjb` apps directly over `tj_core::host::JsonHost`
//! with native GTK 3 widgets, `@server` SSE sync, local file storage, and `/_tj/{inspect,input,screenshot}`.

mod automation;
mod gtk3;
mod renderer;
mod store;
mod sync;

use automation::AutoReq;
use gtk3::{cstr, Gtk3, GtkWidget, GTK_WINDOW_TOPLEVEL, G_FALSE};
use renderer::SharedRenderer;
use serde_json::{json, Value as Json};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::os::raw::{c_int, c_uint, c_void};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use store::FileStore;
use sync::SyncClient;
use tj_core::host::JsonHost;

enum MainMsg {
    Input(Json),
    Auto(AutoReq),
    SignIn { token: String, user: String, roles: Vec<String> },
    Resize(i32, i32),
}

struct AppState {
    gtk: &'static Gtk3,
    host: JsonHost,
    renderer: SharedRenderer,
    sync: SyncClient,
    pending: HashMap<u64, Sender<Result<Json, String>>>,
    next_reply: u64,
    wake_source: Option<c_uint>,
}

impl AppState {
    fn now_ms() -> f64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64() * 1000.0)
    }

    fn send(&mut self, input: Json) -> Result<(), String> {
        if input["type"].as_str() == Some("sync") {
            if let (Some(k), Some(seq)) = (input["object"].as_str(), input["seq"].as_u64()) {
                self.sync.record_seq(k, seq);
            }
        }
        let text = input.to_string();
        let (code, out) = self.host.handle(Self::now_ms(), &text);
        if code != 0 {
            eprintln!("tinyjump-linux: rejected input ({code}): {out}");
            return Err(out);
        }
        self.apply(&out);
        Ok(())
    }

    fn request(&mut self, mut input: Json, reply: Sender<Result<Json, String>>) {
        let id = self.next_reply;
        self.next_reply += 1;
        input["reply"] = json!(id);
        self.pending.insert(id, reply);
        if let Err(err) = self.send(input) {
            if let Some(tx) = self.pending.remove(&id) {
                let _ = tx.send(Err(err));
            }
        }
    }

    fn layout(&mut self) {
        let (w, h) = self.renderer.viewport();
        if w > 0 && h > 0 {
            let _ = self.send(json!({ "type": "layout", "width": w as f64, "height": h as f64, "rtl": self.renderer.rtl() }));
        }
    }

    fn apply(&mut self, outputs: &str) {
        let Ok(Json::Array(list)) = serde_json::from_str::<Json>(outputs) else {
            eprintln!("tinyjump-linux: bad output from runtime");
            return;
        };
        let mut relayout = false;
        for o in list {
            match o["type"].as_str().unwrap_or("") {
                "patch" => {
                    if let Some(ops) = o["ops"].as_array() {
                        relayout = self.renderer.apply_patch(ops) || relayout;
                    }
                }
                "frames" => {
                    if let Some(frames) = o["frames"].as_array() {
                        self.renderer.apply_frames(frames);
                    }
                }
                "effect" => {
                    let id = o["id"].as_i64().unwrap_or(0);
                    let eff = o["effect"].clone();
                    match eff["kind"].as_str().unwrap_or("") {
                        "call" => self.sync.call(id, eff),
                        "http" => self.sync.http_effect(id, eff),
                        "plugin" => {
                            let name = eff["plugin"].as_str().unwrap_or("");
                            let _ = self.send(json!({ "type": "effectDone", "id": id, "error": format!("no device plugin registered for {name}") }));
                        }
                        other => {
                            let _ = self.send(json!({ "type": "effectDone", "id": id, "error": format!("unsupported effect {other}") }));
                        }
                    }
                }
                "subscribe" => {
                    let obj = o["object"].as_str().unwrap_or("").to_string();
                    let seq = o["seq"].as_u64().unwrap_or(0);
                    self.sync.subscribe(obj, seq);
                }
                "wake" => {
                    if let Some(src) = self.wake_source.take() {
                        unsafe { (self.gtk.g_source_remove)(src) };
                    }
                    if let Some(at) = o["at"].as_f64() {
                        let delay_ms = (at - Self::now_ms()).clamp(0.0, 60_000.0) as c_uint;
                        let src = unsafe { (self.gtk.g_timeout_add)(delay_ms.max(1), on_wake_timeout, std::ptr::null_mut()) };
                        self.wake_source = Some(src);
                    }
                }
                "reply" => {
                    if let Some(id) = o["reply"].as_u64() {
                        if let Some(tx) = self.pending.remove(&id) {
                            if let Some(err) = o["error"].as_str() {
                                let _ = tx.send(Err(err.to_string()));
                            } else {
                                let _ = tx.send(Ok(o.get("ok").cloned().unwrap_or(Json::Null)));
                            }
                        }
                    }
                }
                "log" => eprintln!("[app] {}", o["message"].as_str().unwrap_or("")),
                "error" => eprintln!("[app] error: {}", o["message"].as_str().unwrap_or("")),
                _ => {}
            }
        }
        self.sync.flush_subscriptions();
        if relayout {
            self.layout();
        }
    }
}

struct IdleDrain {
    rx: Receiver<MainMsg>,
    state: Rc<RefCell<AppState>>,
}

thread_local! {
    static DRAIN: RefCell<Option<IdleDrain>> = const { RefCell::new(None) };
}

unsafe extern "C" fn drain_idle(_: *mut c_void) -> c_int {
    let (state, msgs) = DRAIN.with(|slot| {
        let guard = slot.borrow();
        let Some(drain) = guard.as_ref() else { return (None, Vec::new()) };
        let mut batch = Vec::new();
        while let Ok(msg) = drain.rx.try_recv() {
            batch.push(msg);
        }
        (Some(drain.state.clone()), batch)
    });
    let Some(state) = state else { return G_FALSE };
    for msg in msgs {
        match msg {
            MainMsg::Input(v) => {
                let _ = state.borrow_mut().send(v);
            }
            MainMsg::SignIn { token, user, roles } => {
                let mut st = state.borrow_mut();
                if st.sync.set_token(Some(token), Some(user.clone()), roles.clone()) {
                    let _ = st.send(json!({ "type": "identity", "user": user, "roles": roles }));
                }
            }
            MainMsg::Resize(w, h) => {
                let mut st = state.borrow_mut();
                if st.renderer.set_viewport(w, h) {
                    st.layout();
                }
            }
            MainMsg::Auto(req) => match req {
                AutoReq::Inspect(tx) => {
                    let renderer = state.borrow().renderer.clone_handle();
                    let (inner_tx, inner_rx) = channel();
                    state.borrow_mut().request(json!({ "type": "inspect" }), inner_tx);
                    match inner_rx.try_recv() {
                        Ok(Ok(mut snap)) => {
                            snap["tree"] = renderer.add_bounds(&snap["tree"]);
                            let (w, h) = renderer.viewport();
                            snap["viewport"] = json!({ "w": w, "h": h });
                            let _ = tx.send(Ok(snap));
                        }
                        Ok(Err(e)) => {
                            let _ = tx.send(Err(e));
                        }
                        Err(_) => {
                            let _ = tx.send(Err("inspect reply pending".into()));
                        }
                    }
                }
                AutoReq::Input(raw, tx) => {
                    let input = state.borrow().renderer.for_agent(raw);
                    match input["type"].as_str().unwrap_or("") {
                        "call" | "get" | "ui" | "inspect" => {
                            state.borrow_mut().request(input, tx);
                        }
                        "locale" => {
                            let loc = input["locale"].as_str().unwrap_or("").to_string();
                            let mut st = state.borrow_mut();
                            st.renderer.set_locale(&loc);
                            let resolved = st.renderer.locale();
                            let res = st.send(json!({ "type": "locale", "locale": resolved }));
                            st.layout();
                            let _ = tx.send(res.map(|()| Json::Null));
                        }
                        _ => {
                            let res = state.borrow_mut().send(input);
                            let _ = tx.send(res.map(|()| Json::Null));
                        }
                    }
                }
                AutoReq::Screenshot(tx) => {
                    // Release all RefCell borrows before pumping GTK events inside screenshot().
                    let renderer = state.borrow().renderer.clone_handle();
                    let png = renderer.screenshot();
                    let _ = tx.send(png);
                }
            },
        }
    }
    G_FALSE
}

unsafe extern "C" fn on_wake_timeout(_: *mut c_void) -> c_int {
    let state = DRAIN.with(|slot| slot.borrow().as_ref().map(|d| d.state.clone()));
    if let Some(state) = state {
        let mut st = state.borrow_mut();
        st.wake_source = None;
        let _ = st.send(json!({ "type": "tick" }));
    }
    G_FALSE
}

unsafe extern "C" fn on_window_destroy(_: *mut GtkWidget, _: *mut c_void) {
    if let Ok(gtk) = gtk3::load() {
        (gtk.gtk_main_quit)();
    }
}

unsafe extern "C" fn free_tx_box(data: *mut c_void, _closure: *mut c_void) {
    if !data.is_null() {
        drop(Box::from_raw(data as *mut Sender<MainMsg>));
    }
}

unsafe extern "C" fn on_window_configure(win: *mut GtkWidget, _ev: *mut c_void, data: *mut c_void) -> c_int {
    let tx = &*(data as *const Sender<MainMsg>);
    if let Ok(gtk) = gtk3::load() {
        let mut w: c_int = 0;
        let mut h: c_int = 0;
        (gtk.gtk_window_get_size)(win, &mut w, &mut h);
        let _ = tx.send(MainMsg::Resize(w, h));
        (gtk.g_idle_add)(drain_idle, std::ptr::null_mut());
    }
    G_FALSE
}

fn parse_args() -> HashMap<String, String> {
    let raw: Vec<String> = std::env::args().collect();
    let mut out = HashMap::new();
    let mut i = 1;
    while i < raw.len() {
        let cur = &raw[i];
        if cur == "--help" || cur == "-h" {
            out.insert("help".into(), "1".into());
            i += 1;
            continue;
        }
        if let Some(rest) = cur.strip_prefix("--").or_else(|| cur.strip_prefix('-')) {
            if let Some((k, v)) = rest.split_once('=') {
                out.insert(k.to_string(), v.to_string());
                i += 1;
            } else if let Some(v) = raw.get(i + 1) {
                out.insert(rest.to_string(), v.clone());
                i += 2;
            } else {
                out.insert(rest.to_string(), "1".into());
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    out
}

fn default_data_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.trim().is_empty() {
            return PathBuf::from(xdg).join("tinyjump");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home).join(".local").join("share").join("tinyjump");
        }
    }
    std::env::temp_dir().join(format!("tinyjump-linux-{}", std::process::id()))
}

fn load_app_source(entry: &Path) -> Result<String, String> {
    let raw = fs::read(entry).map_err(|e| format!("cannot read {}: {e}", entry.display()))?;
    // Precompiled binary `.tjb` bytecode (`TJB\x02`): wrap into the `//# tjb:1:` bundle format.
    if raw.starts_with(b"TJB") {
        return Ok(format!("//# tjb:1:{}", base64_encode(&raw)));
    }
    let entry_text = String::from_utf8(raw).map_err(|e| format!("{} is not valid UTF-8: {e}", entry.display()))?;
    if entry.extension().and_then(|s| s.to_str()) == Some("tjb") {
        return Ok(entry_text);
    }
    let dir = entry.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let entry_name = entry.file_name().and_then(|s| s.to_str()).unwrap_or("app.tj");
    let mut files = vec![(entry_name.to_string(), entry_text)];
    collect_siblings(dir, dir, entry_name, &mut files)?;
    if files.len() == 1 {
        return Ok(files.remove(0).1);
    }
    let mut out = String::new();
    for (name, text) in files {
        out.push_str("//# file: ");
        out.push_str(&name);
        out.push('\n');
        out.push_str(&text);
        if !text.ends_with('\n') {
            out.push('\n');
        }
    }
    Ok(out)
}

fn collect_siblings(base: &Path, dir: &Path, entry_name: &str, out: &mut Vec<(String, String)>) -> Result<(), String> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for p in entries {
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name.starts_with('.') || name == "tests" || name == "dist" || name == "target" {
            continue;
        }
        if p.is_dir() {
            collect_siblings(base, &p, entry_name, out)?;
        } else {
            let rel = p.strip_prefix(base).unwrap_or(&p).to_string_lossy().replace('\\', "/");
            if rel == entry_name {
                continue;
            }
            if rel.ends_with(".tj") || (rel.starts_with("locales/") && rel.ends_with(".json")) {
                let text = fs::read_to_string(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
                out.push((rel, text));
            }
        }
    }
    Ok(())
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { TABLE[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[(n & 63) as usize] as char } else { '=' });
    }
    out
}

fn read_os_random(buf: &mut [u8]) -> Result<(), String> {
    let mut f = fs::File::open("/dev/urandom").map_err(|e| format!("cannot open /dev/urandom: {e}"))?;
    f.read_exact(buf).map_err(|e| format!("cannot read /dev/urandom: {e}"))
}

fn random_seed_and_entropy() -> Result<(u64, String), String> {
    let mut seed_bytes = [0u8; 8];
    read_os_random(&mut seed_bytes)?;
    let seed = u64::from_le_bytes(seed_bytes).max(1);
    let mut ent = [0u8; 32];
    read_os_random(&mut ent)?;
    let hex = ent.iter().map(|b| format!("{b:02x}")).collect();
    Ok((seed, hex))
}

fn main() {
    if let Err(e) = run() {
        eprintln!("tinyjump-linux: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args();
    if args.contains_key("help") {
        println!(
            "Usage: tinyjump-linux [--app <path.tj|path.tjb> | --example <name>] [--server <url>] [--key <token>] [--data <dir>] [--port <n>] [--dir ltr|rtl] [--width <px>] [--height <px>] [--title <text>]"
        );
        return Ok(());
    }
    let app_path = if let Some(p) = args.get("app") {
        PathBuf::from(p)
    } else {
        let ex = args.get("example").map(String::as_str).unwrap_or("todo");
        PathBuf::from("examples").join(ex).join("app.tj")
    };
    let source = load_app_source(&app_path)?;
    let width: i32 = args.get("width").and_then(|s| s.parse().ok()).unwrap_or(960);
    let height: i32 = args.get("height").and_then(|s| s.parse().ok()).unwrap_or(680);
    let rtl_override = match args.get("dir").map(String::as_str) {
        Some("rtl") => Some(true),
        Some("ltr") => Some(false),
        _ => None,
    };
    let port: u16 = args.get("port").and_then(|s| s.parse().ok()).unwrap_or(4703);
    let data_dir = args.get("data").map(PathBuf::from).unwrap_or_else(default_data_dir);

    let (seed, entropy_hex) = random_seed_and_entropy()?;

    let gtk = gtk3::load()?;
    let mut argc: c_int = 0;
    let mut argv: *mut *mut std::os::raw::c_char = std::ptr::null_mut();
    if unsafe { (gtk.gtk_init_check)(&mut argc, &mut argv) } == 0 {
        return Err("GTK 3 could not open a display (set DISPLAY or run under xvfb-run -a)".into());
    }

    let (tx, rx) = channel::<MainMsg>();
    let wake_idle = move || unsafe {
        (gtk.g_idle_add)(drain_idle, std::ptr::null_mut());
    };

    let window = unsafe { (gtk.gtk_window_new)(GTK_WINDOW_TOPLEVEL) };
    let title = args.get("title").cloned().unwrap_or_else(|| format!("TinyJump — {}", app_path.display()));
    let ctitle = cstr(&title);
    unsafe {
        (gtk.gtk_window_set_title)(window, ctitle.as_ptr());
        (gtk.gtk_window_set_default_size)(window, width, height);
    }
    let root = unsafe { (gtk.gtk_fixed_new)() };
    unsafe {
        (gtk.gtk_container_add)(window, root);
        let sig_destroy = cstr("destroy");
        (gtk.g_signal_connect_data)(window, sig_destroy.as_ptr(), on_window_destroy as *const c_void, std::ptr::null_mut(), None, 0);
        let tx_box = Box::into_raw(Box::new(tx.clone()));
        let sig_cfg = cstr("configure-event");
        (gtk.g_signal_connect_data)(window, sig_cfg.as_ptr(), on_window_configure as *const c_void, tx_box as *mut c_void, Some(free_tx_box), 0);
    }

    let ui_tx = tx.clone();
    let on_ui = Rc::new(move |node: u32, event: &str, value: Json| {
        let _ = ui_tx.send(MainMsg::Input(json!({ "type": "ui", "target": { "node": node }, "event": event, "value": value })));
        wake_idle();
    });
    let renderer = SharedRenderer::new(gtk, root, width, height, rtl_override, on_ui);
    let initial_locale = renderer.locale();

    let store = FileStore::open(data_dir)?;
    let mut host = JsonHost::default();
    let (code, boot_out) = host.boot(&source, Box::new(store), Some(Box::new(renderer.clone_handle())), AppState::now_ms(), seed, true);
    if code != 0 {
        return Err(format!("boot failed ({code}): {boot_out}"));
    }

    let sync_tx = tx.clone();
    let wake_fn: sync::WakeFn = Arc::new(move |msg| {
        let _ = sync_tx.send(MainMsg::Input(msg));
        unsafe { (gtk.g_idle_add)(drain_idle, std::ptr::null_mut()) };
    });
    let sync = SyncClient::new(args.get("server").cloned(), wake_fn);

    let state = Rc::new(RefCell::new(AppState {
        gtk,
        host,
        renderer,
        sync,
        pending: HashMap::new(),
        next_reply: 1,
        wake_source: None,
    }));

    DRAIN.with(|slot| {
        *slot.borrow_mut() = Some(IdleDrain { rx, state: state.clone() });
    });

    {
        let mut st = state.borrow_mut();
        st.apply(&boot_out);
        let _ = st.send(json!({ "type": "entropy", "bytes": entropy_hex }));
        let _ = st.send(json!({ "type": "locale", "locale": initial_locale }));
        st.layout();
    }

    unsafe {
        (gtk.gtk_widget_show_all)(window);
    }

    if port > 0 {
        let auto_tx = tx.clone();
        automation::start(
            port,
            Arc::new(move |req| {
                let _ = auto_tx.send(MainMsg::Auto(req));
                unsafe { (gtk.g_idle_add)(drain_idle, std::ptr::null_mut()) };
            }),
        )?;
    }

    if let (Some(server), Some(key)) = (args.get("server").cloned(), args.get("key").cloned()) {
        let sign_tx = tx.clone();
        sync::sign_in_async(server, key.clone(), move |user, roles| {
            let _ = sign_tx.send(MainMsg::SignIn { token: key, user, roles });
            unsafe { (gtk.g_idle_add)(drain_idle, std::ptr::null_mut()) };
        });
    }

    unsafe {
        (gtk.gtk_main)();
    }
    state.borrow().sync.stop();
    if let Some(src) = state.borrow_mut().wake_source.take() {
        unsafe { (gtk.g_source_remove)(src) };
    }
    DRAIN.with(|slot| {
        *slot.borrow_mut() = None;
    });
    Ok(())
}

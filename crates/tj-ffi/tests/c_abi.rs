//! Drives the C ABI the way a Swift or C host would: raw pointers and C callbacks.

use std::collections::BTreeMap;
use std::os::raw::{c_int, c_void};
use tj_ffi::*;

type Map = BTreeMap<String, String>;

unsafe fn text<'a>(p: *const u8, n: usize) -> &'a str {
    std::str::from_utf8(std::slice::from_raw_parts(p, n)).unwrap()
}

unsafe extern "C" fn load(ctx: *mut c_void, k: *const u8, kl: usize, out: *mut TjBuf) -> c_int {
    let map = &*(ctx as *const Map);
    match map.get(text(k, kl)) {
        Some(v) => {
            tj_buf_set(out, v.as_ptr(), v.len());
            1
        }
        None => 0,
    }
}

unsafe extern "C" fn save(ctx: *mut c_void, k: *const u8, kl: usize, v: *const u8, vl: usize) -> c_int {
    let map = &mut *(ctx as *mut Map);
    map.insert(text(k, kl).to_string(), text(v, vl).to_string());
    0
}

unsafe extern "C" fn keys(ctx: *mut c_void, p: *const u8, pl: usize, out: *mut TjBuf) {
    let map = &*(ctx as *const Map);
    let prefix = text(p, pl);
    let list: Vec<&String> = map.keys().filter(|k| k.starts_with(prefix)).collect();
    let json = format!("[{}]", list.iter().map(|k| format!("{k:?}")).collect::<Vec<_>>().join(","));
    tj_buf_set(out, json.as_ptr(), json.len());
}

unsafe extern "C" fn remove(ctx: *mut c_void, k: *const u8, kl: usize) -> c_int {
    (*(ctx as *mut Map)).remove(text(k, kl));
    0
}

unsafe extern "C" fn measure(_ctx: *mut c_void, _node: u32, max_width: f32, w: *mut f32, h: *mut f32) {
    *w = if max_width >= 0.0 { max_width.min(80.0) } else { 80.0 };
    *h = 20.0;
}

fn host(map: &mut Map) -> TjHost {
    TjHost { ctx: map as *mut Map as *mut c_void, load, save, keys, remove, measure }
}

unsafe fn out(rt: *const TjRuntime) -> String {
    let mut len = 0;
    let p = tj_out(rt, &mut len);
    text(p, len).to_string()
}

#[cfg(feature = "compiler")]
const APP: &str = "object Counter { n = 0\n inc() { this.n += 1; return this.n } }";

#[cfg(feature = "compiler")]
unsafe fn boot(map: &mut Map) -> *mut TjRuntime {
    let rt = tj_new(host(map));
    assert_eq!(tj_boot(rt, APP.as_ptr(), APP.len(), 0.0, 1, 0), 0, "{}", out(rt));
    rt
}

unsafe fn call(rt: *mut TjRuntime) -> String {
    let input = r#"{"type":"call","object":"Counter/c","method":"inc","reply":1}"#;
    assert_eq!(tj_handle(rt, 1.0, input.as_ptr(), input.len()), 0);
    out(rt)
}

#[cfg(feature = "compiler")]
#[test]
fn boots_calls_persists_and_reloads_through_c_callbacks() {
    let mut map = Map::new();
    unsafe {
        let rt = boot(&mut map);
        assert!(call(rt).contains(r#""ok":1"#));
        assert!(call(rt).contains(r#""ok":2"#));
        tj_free(rt);

        assert!(map.contains_key("Counter/c"), "state saved through the save callback");
        let rt = boot(&mut map);
        assert!(call(rt).contains(r#""ok":3"#), "state loaded through the load callback");

        let bad = "{nope";
        assert_eq!(tj_handle(rt, 2.0, bad.as_ptr(), bad.len()), 1);
        assert!(out(rt).contains("bad input"));
        tj_free(rt);
    }
}

#[cfg(feature = "compiler")]
#[test]
fn reports_compile_errors() {
    let mut map = Map::new();
    unsafe {
        let rt = tj_new(host(&mut map));
        let src = "function f() { return nope }";
        assert_eq!(tj_boot(rt, src.as_ptr(), src.len(), 0.0, 1, 0), 1);
        assert!(out(rt).contains("unknown name 'nope'"));
        tj_free(rt);
    }
}

#[cfg(feature = "compiler")]
#[test]
fn lays_out_through_the_measure_callback() {
    let mut map = Map::new();
    unsafe {
        let rt = tj_new(host(&mut map));
        let src = "function App() { return <Row><Text grow={1}>hi</Text><Text>there</Text></Row> }";
        assert_eq!(tj_boot(rt, src.as_ptr(), src.len(), 0.0, 1, 1), 0, "{}", out(rt));
        let input = r#"{"type":"layout","width":300,"height":600}"#;
        assert_eq!(tj_handle(rt, 1.0, input.as_ptr(), input.len()), 0);
        let frames = out(rt);
        // The fake host measures every text as 80 wide: the growing one takes the rest.
        assert!(frames.contains(r#""type":"frames""#), "{frames}");
        assert!(frames.contains(r#""x":0.0,"y":0.0,"w":220.0"#) && frames.contains(r#""x":220.0,"y":0.0,"w":80.0"#), "{frames}");
        tj_free(rt);
    }
}

/// Builds a minimal valid `//# tjb:1:<base64>` (`TJB\x03`) bytecode bundle for
/// `object Counter { n = 0; inc() { this.n += 1; return this.n } }` without using the compiler,
/// so `cargo test -p tj-ffi --no-default-features` proves AOT-only execution end to end.
fn counter_aot_bundle() -> String {
    let builtins = [
        "console", "Math", "JSON", "Object", "Date", "Time", "reveal", "crypto", "Array",
        "String", "Number", "parseInt", "parseFloat", "encodeURIComponent", "Error", "check",
        "t", "__fetch", "plugin", "invoke", "__sort",
    ];
    let mut b = Vec::new();
    let push_str = |b: &mut Vec<u8>, s: &str| {
        b.extend_from_slice(&(s.len() as u32).to_le_bytes());
        b.extend_from_slice(s.as_bytes());
    };
    b.extend_from_slice(b"TJB\x03");
    b.extend_from_slice(&(builtins.len() as u16).to_le_bytes());
    // globals: 21 builtins + Counter (slot 21)
    b.extend_from_slice(&((builtins.len() + 1) as u32).to_le_bytes());
    for name in builtins {
        push_str(&mut b, name);
    }
    push_str(&mut b, "Counter");
    // consts: [0.0, 1.0, "n", Class(0)]
    b.extend_from_slice(&4u32.to_le_bytes());
    b.push(2);
    b.extend_from_slice(&0.0f64.to_le_bytes());
    b.push(2);
    b.extend_from_slice(&1.0f64.to_le_bytes());
    b.push(3);
    push_str(&mut b, "n");
    b.push(4);
    b.extend_from_slice(&0u16.to_le_bytes());
    // elems: []
    b.extend_from_slice(&0u32.to_le_bytes());
    // protos: [Counter.<init>, inc, <main>]
    b.extend_from_slice(&3u32.to_le_bytes());
    let write_proto = |b: &mut Vec<u8>, name: &str, params: u16, locals: u16, ops: &[u8], n_ops: u32| {
        push_str(b, name);
        b.extend_from_slice(&params.to_le_bytes());
        b.extend_from_slice(&locals.to_le_bytes());
        b.push(0); // Source::User
        b.extend_from_slice(&n_ops.to_le_bytes());
        b.extend_from_slice(ops);
        b.extend_from_slice(&n_ops.to_le_bytes());
        for _ in 0..n_ops {
            b.extend_from_slice(&0u32.to_le_bytes());
        }
        b.extend_from_slice(&0u32.to_le_bytes()); // captures
    };
    // Counter.<init>: NewRec(25), Const(1, 0), RecSet(26, 2), Return(52)
    let mut init_ops = vec![25u8, 1];
    init_ops.extend_from_slice(&0u32.to_le_bytes());
    init_ops.push(26);
    init_ops.extend_from_slice(&2u32.to_le_bytes());
    init_ops.push(52);
    write_proto(&mut b, "Counter.<init>", 0, 0, &init_ops, 4);
    // inc(this): LoadLocal(8,0), Dup(6), GetField(18,2), Const(1,1), Add(29), SetField(19,2), Pop(5), LoadLocal(8,0), GetField(18,2), Return(52)
    let mut inc_ops = Vec::new();
    inc_ops.push(8); inc_ops.extend_from_slice(&0u16.to_le_bytes());
    inc_ops.push(6);
    inc_ops.push(18); inc_ops.extend_from_slice(&2u32.to_le_bytes());
    inc_ops.push(1); inc_ops.extend_from_slice(&1u32.to_le_bytes());
    inc_ops.push(29);
    inc_ops.push(19); inc_ops.extend_from_slice(&2u32.to_le_bytes());
    inc_ops.push(5);
    inc_ops.push(8); inc_ops.extend_from_slice(&0u16.to_le_bytes());
    inc_ops.push(18); inc_ops.extend_from_slice(&2u32.to_le_bytes());
    inc_ops.push(52);
    write_proto(&mut b, "inc", 1, 1, &inc_ops, 10);
    // <main>: Const(1,3), StoreGlobal(17,21), Pop(5), Null(2), Return(52)
    let mut main_ops = Vec::new();
    main_ops.push(1); main_ops.extend_from_slice(&3u32.to_le_bytes());
    main_ops.push(17); main_ops.extend_from_slice(&21u32.to_le_bytes());
    main_ops.push(5);
    main_ops.push(2);
    main_ops.push(52);
    write_proto(&mut b, "<main>", 0, 0, &main_ops, 5);
    // objects: [Counter]
    b.extend_from_slice(&1u32.to_le_bytes());
    push_str(&mut b, "Counter");
    b.extend_from_slice(&1u32.to_le_bytes()); // 1 field
    push_str(&mut b, "n");
    b.extend_from_slice(&0u32.to_le_bytes()); // init proto 0
    b.push(0); // server = false
    b.push(0); // read = None
    b.extend_from_slice(&0u32.to_le_bytes()); // pii
    b.extend_from_slice(&0u32.to_le_bytes()); // guards
    b.push(0); // retain = None
    b.push(0); // hold = None
    b.extend_from_slice(&1u32.to_le_bytes()); // 1 method: inc
    push_str(&mut b, "inc");
    b.extend_from_slice(&1u32.to_le_bytes()); // proto 1
    b.push(0); // private = false
    b.push(0); // every = None
    b.push(0); // allow = None
    b.push(0); // exclusive = false
    b.push(0); // invariant = false
    b.push(0); // frame = false
    b.extend_from_slice(&0u32.to_le_bytes()); // params
    // main: [2], app: None, locales: base="", tables=[]
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&2u32.to_le_bytes());
    b.push(0); // app = None
    push_str(&mut b, ""); // locales.base
    b.extend_from_slice(&0u32.to_le_bytes()); // locales.tables

    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut b64 = String::from("//# tjb:1:");
    for chunk in b.chunks(3) {
        let x0 = u32::from(chunk[0]);
        let x1 = if chunk.len() > 1 { u32::from(chunk[1]) } else { 0 };
        let x2 = if chunk.len() > 2 { u32::from(chunk[2]) } else { 0 };
        let n = (x0 << 16) | (x1 << 8) | x2;
        b64.push(TABLE[((n >> 18) & 63) as usize] as char);
        b64.push(TABLE[((n >> 12) & 63) as usize] as char);
        b64.push(if chunk.len() > 1 { TABLE[((n >> 6) & 63) as usize] as char } else { '=' });
        b64.push(if chunk.len() > 2 { TABLE[(n & 63) as usize] as char } else { '=' });
    }
    b64
}

#[test]
fn boots_precompiled_aot_bundle_through_c_callbacks() {
    let bundle = counter_aot_bundle();
    let mut map = Map::new();
    unsafe {
        let rt = tj_new(host(&mut map));
        assert_eq!(tj_boot(rt, bundle.as_ptr(), bundle.len(), 0.0, 1, 0), 0, "{}", out(rt));
        assert!(call(rt).contains(r#""ok":1"#));
        assert!(call(rt).contains(r#""ok":2"#));
        tj_free(rt);

        assert!(map.contains_key("Counter/c"), "AOT state persisted through save callback");
        let rt = tj_new(host(&mut map));
        assert_eq!(tj_boot(rt, bundle.as_ptr(), bundle.len(), 0.0, 1, 0), 0, "{}", out(rt));
        assert!(call(rt).contains(r#""ok":3"#), "AOT state reloaded through load callback");
        tj_free(rt);
    }
}

#[cfg(not(feature = "compiler"))]
#[test]
fn aot_only_build_rejects_raw_source_with_hint() {
    let mut map = Map::new();
    unsafe {
        let rt = tj_new(host(&mut map));
        let src = "object Counter { n = 0 }";
        assert_eq!(tj_boot(rt, src.as_ptr(), src.len(), 0.0, 1, 0), 1);
        let body = out(rt);
        assert!(body.contains("AOT-only runtime") && body.contains("tj build"), "{body}");
        tj_free(rt);
    }
}

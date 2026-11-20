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

const APP: &str = "object Counter { n = 0\n inc() { this.n += 1; return this.n } }";

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
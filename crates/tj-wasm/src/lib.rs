//! The web host's view of the runtime. Messages cross the boundary as UTF-8 JSON:
//! the host writes into memory from `tj_alloc`, calls an entry point, then reads the
//! result from `tj_out_ptr`/`tj_out_len`. Storage calls go back to the host through imports.
//!
//! Only meaningful on `wasm32`; on other targets this crate is empty so the workspace
//! still builds everywhere.
#![cfg(target_family = "wasm")]

use serde_json::{json, Value as Json};
use std::cell::RefCell;
use tj_core::{compile, Input, Options, Runtime, Store};

#[link(wasm_import_module = "env")]
extern "C" {
    /// Writes a host-allocated buffer pointer to `out` and returns its length, or -1 if missing.
    fn tj_host_load(key: *const u8, key_len: usize, out: *mut *mut u8) -> isize;
    /// Returns 0 on success.
    fn tj_host_save(key: *const u8, key_len: usize, val: *const u8, val_len: usize) -> i32;
    /// Like `tj_host_load`, returning a JSON array of keys.
    fn tj_host_keys(prefix: *const u8, prefix_len: usize, out: *mut *mut u8) -> isize;
}

thread_local! {
    static RUNTIME: RefCell<Option<Runtime>> = const { RefCell::new(None) };
    static OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

struct HostStore;

/// Takes ownership of a buffer the host allocated with `tj_alloc`.
fn take_host_string(ptr: *mut u8, len: isize) -> Option<String> {
    if len < 0 || ptr.is_null() {
        return None;
    }
    // SAFETY: the host allocated exactly `len` bytes with `tj_alloc` and handed them over.
    let bytes = unsafe { Vec::from_raw_parts(ptr, len as usize, len as usize) };
    String::from_utf8(bytes).ok()
}

impl Store for HostStore {
    fn load(&mut self, key: &str) -> Option<String> {
        let mut out = std::ptr::null_mut();
        // SAFETY: valid key slice; the host writes a pointer to a fresh allocation into `out`.
        let len = unsafe { tj_host_load(key.as_ptr(), key.len(), &mut out) };
        take_host_string(out, len)
    }

    fn save(&mut self, key: &str, value: &str) -> Result<(), String> {
        // SAFETY: valid slices for the duration of the call.
        match unsafe { tj_host_save(key.as_ptr(), key.len(), value.as_ptr(), value.len()) } {
            0 => Ok(()),
            _ => Err("browser storage refused the write (quota?)".into()),
        }
    }

    fn keys(&mut self, prefix: &str) -> Vec<String> {
        let mut out = std::ptr::null_mut();
        // SAFETY: as in `load`.
        let len = unsafe { tj_host_keys(prefix.as_ptr(), prefix.len(), &mut out) };
        take_host_string(out, len).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
}

fn set_out(j: &Json) {
    OUT.with(|o| *o.borrow_mut() = serde_json::to_vec(j).expect("JSON serializes"));
}

/// # Safety
/// The caller must treat the pointer as an exclusive buffer of `len` bytes and give it back with [`tj_free`].
#[no_mangle]
pub extern "C" fn tj_alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len);
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// # Safety
/// `ptr` and `len` must come from one call to [`tj_alloc`].
#[no_mangle]
pub unsafe extern "C" fn tj_free(ptr: *mut u8, len: usize) {
    drop(Vec::from_raw_parts(ptr, 0, len));
}

#[no_mangle]
pub extern "C" fn tj_out_ptr() -> *const u8 {
    OUT.with(|o| o.borrow().as_ptr())
}

#[no_mangle]
pub extern "C" fn tj_out_len() -> usize {
    OUT.with(|o| o.borrow().len())
}

unsafe fn read_str<'a>(ptr: *const u8, len: usize) -> Result<&'a str, Json> {
    std::str::from_utf8(std::slice::from_raw_parts(ptr, len)).map_err(|_| json!({ "message": "input is not UTF-8" }))
}

/// Compiles and boots `src`. Returns 0 with the outputs array, 1 with compile errors, 2 with a boot error.
///
/// # Safety
/// `src` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn tj_boot(src: *const u8, len: usize, now: f64, seed: f64, render: i32) -> i32 {
    let src = match read_str(src, len) {
        Ok(s) => s,
        Err(e) => {
            set_out(&json!([e]));
            return 1;
        }
    };
    let program = match compile(src) {
        Ok(p) => p,
        Err(errs) => {
            let list: Vec<Json> = errs
                .iter()
                .map(|d| {
                    let (line, col) = tj_core::syntax::line_col(src, d.span.start as usize);
                    json!({ "message": d.message, "hint": d.hint, "line": line, "col": col, "text": d.render("app.tj", src) })
                })
                .collect();
            set_out(&Json::Array(list));
            return 1;
        }
    };
    match Runtime::boot(program, Box::new(HostStore), Options { now, seed: seed as u64, render: render != 0 }) {
        Ok((rt, outputs)) => {
            RUNTIME.with(|r| *r.borrow_mut() = Some(rt));
            set_out(&serde_json::to_value(outputs).expect("outputs serialize"));
            0
        }
        Err(e) => {
            set_out(&json!({ "message": e.message, "trace": e.trace }));
            2
        }
    }
}

/// Handles one JSON [`Input`]. Returns 0 with the outputs array, 1 with an error.
///
/// # Safety
/// `input` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn tj_handle(now: f64, input: *const u8, len: usize) -> i32 {
    let parsed = read_str(input, len).and_then(|s| serde_json::from_str::<Input>(s).map_err(|e| json!({ "message": format!("bad input: {e}") })));
    let input = match parsed {
        Ok(i) => i,
        Err(e) => {
            set_out(&e);
            return 1;
        }
    };
    RUNTIME.with(|r| match r.borrow_mut().as_mut() {
        Some(rt) => {
            let outputs = rt.handle(now, input);
            set_out(&serde_json::to_value(outputs).expect("outputs serialize"));
            0
        }
        None => {
            set_out(&json!({ "message": "not booted" }));
            1
        }
    })
}

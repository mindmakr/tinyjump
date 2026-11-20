//! The runtime as a WebAssembly module for the web host. A thin C ABI over
//! [`tj_core::host::JsonHost`]: the host writes UTF-8 JSON into memory from `tj_alloc`,
//! calls an entry point, then reads the result from `tj_out_ptr`/`tj_out_len`.
//! Storage calls go back to the host through imports.
//!
//! Only meaningful on `wasm32`; on other targets this crate is empty so the workspace
//! still builds everywhere.
#![cfg(target_family = "wasm")]

use std::cell::RefCell;
use tj_core::host::{JsonHost, INVALID};
use tj_core::Store;

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
    static HOST: RefCell<JsonHost> = RefCell::new(JsonHost::default());
    static OUT: RefCell<String> = const { RefCell::new(String::new()) };
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
    fn load(&mut self, key: &str) -> Result<Option<String>, String> {
        let mut out = std::ptr::null_mut();
        // SAFETY: valid key slice; the host writes a pointer to a fresh allocation into `out`.
        let len = unsafe { tj_host_load(key.as_ptr(), key.len(), &mut out) };
        Ok(take_host_string(out, len))
    }

    fn save(&mut self, key: &str, value: &str) -> Result<(), String> {
        // SAFETY: valid slices for the duration of the call.
        match unsafe { tj_host_save(key.as_ptr(), key.len(), value.as_ptr(), value.len()) } {
            0 => Ok(()),
            _ => Err("browser storage refused the write (quota?)".into()),
        }
    }

    fn keys(&mut self, prefix: &str) -> Result<Vec<String>, String> {
        let mut out = std::ptr::null_mut();
        // SAFETY: as in `load`.
        let len = unsafe { tj_host_keys(prefix.as_ptr(), prefix.len(), &mut out) };
        Ok(take_host_string(out, len).map(|s| tj_core::host::parse_keys(&s)).unwrap_or_default())
    }
}

fn finish((code, body): (i32, String)) -> i32 {
    OUT.with(|o| *o.borrow_mut() = body);
    code
}

/// Allocates `len` bytes for the host to fill; release with [`tj_free`].
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

/// # Safety
/// `ptr` must point to `len` readable bytes.
unsafe fn read_str<'a>(ptr: *const u8, len: usize) -> Option<&'a str> {
    std::str::from_utf8(std::slice::from_raw_parts(ptr, len)).ok()
}

fn not_utf8() -> i32 {
    finish((INVALID, r#"{"message":"input is not UTF-8"}"#.into()))
}

/// Compiles and boots `src`. Status codes are those of [`tj_core::host`].
///
/// # Safety
/// `src` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn tj_boot(src: *const u8, len: usize, now: f64, seed: f64, render: i32) -> i32 {
    match read_str(src, len) {
        Some(src) => finish(HOST.with(|h| h.borrow_mut().boot(src, Box::new(HostStore), None, now, seed as u64, render != 0))),
        None => not_utf8(),
    }
}

/// Handles one JSON input. Status codes are those of [`tj_core::host`].
///
/// # Safety
/// `input` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn tj_handle(now: f64, input: *const u8, len: usize) -> i32 {
    match read_str(input, len) {
        Some(input) => finish(HOST.with(|h| h.borrow_mut().handle(now, input))),
        None => not_utf8(),
    }
}

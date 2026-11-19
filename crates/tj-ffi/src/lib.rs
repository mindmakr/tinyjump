//! The runtime behind a C ABI (`include/tinyjump.h`), over [`tj_core::host::JsonHost`].
//! Hosts in Swift, C or C++ embed the static library; storage and widget measurement go back
//! to the host through the `TjHost` callbacks.

use std::os::raw::{c_int, c_void};
use tj_core::host::{JsonHost, INVALID};
use tj_core::ui::layout::Measure;
use tj_core::Store;

/// A Rust-owned buffer that host callbacks fill with [`tj_buf_set`].
pub struct TjBuf(Vec<u8>);

type LoadFn = unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut TjBuf) -> c_int;
type SaveFn = unsafe extern "C" fn(*mut c_void, *const u8, usize, *const u8, usize) -> c_int;
type KeysFn = unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut TjBuf);
type MeasureFn = unsafe extern "C" fn(*mut c_void, u32, f32, *mut f32, *mut f32);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TjHost {
    pub ctx: *mut c_void,
    pub load: LoadFn,
    pub save: SaveFn,
    pub keys: KeysFn,
    pub measure: MeasureFn,
}

impl Measure for TjHost {
    fn measure(&mut self, node: u32, max_width: Option<f32>) -> (f32, f32) {
        let (mut w, mut h) = (0.0, 0.0);
        // SAFETY: the host promised valid callbacks for this runtime's lifetime.
        unsafe { (self.measure)(self.ctx, node, max_width.unwrap_or(-1.0), &mut w, &mut h) };
        (w, h)
    }
}

impl Store for TjHost {
    fn load(&mut self, key: &str) -> Option<String> {
        let mut buf = TjBuf(Vec::new());
        // SAFETY: the host promised valid callbacks for this runtime's lifetime.
        let found = unsafe { (self.load)(self.ctx, key.as_ptr(), key.len(), &mut buf) };
        (found == 1).then(|| String::from_utf8(buf.0).ok()).flatten()
    }

    fn save(&mut self, key: &str, value: &str) -> Result<(), String> {
        // SAFETY: as above.
        match unsafe { (self.save)(self.ctx, key.as_ptr(), key.len(), value.as_ptr(), value.len()) } {
            0 => Ok(()),
            code => Err(format!("host store failed to save {key} ({code})")),
        }
    }

    fn keys(&mut self, prefix: &str) -> Vec<String> {
        let mut buf = TjBuf(Vec::new());
        // SAFETY: as above.
        unsafe { (self.keys)(self.ctx, prefix.as_ptr(), prefix.len(), &mut buf) };
        String::from_utf8(buf.0).map(|s| tj_core::host::parse_keys(&s)).unwrap_or_default()
    }
}

pub struct TjRuntime {
    json: JsonHost,
    host: TjHost,
    out: String,
}

impl TjRuntime {
    fn finish(&mut self, (code, body): (i32, String)) -> c_int {
        self.out = body;
        code
    }
}

/// Hosts pass a null pointer for empty buffers (Swift does); a null slice is undefined behaviour.
unsafe fn bytes<'a>(data: *const u8, len: usize) -> &'a [u8] {
    if data.is_null() || len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(data, len)
    }
}

/// Copies `len` bytes into the buffer.
///
/// # Safety
/// `buf` must be a buffer passed to a store callback, and `data` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn tj_buf_set(buf: *mut TjBuf, data: *const u8, len: usize) {
    if let Some(buf) = buf.as_mut() {
        buf.0.clear();
        buf.0.extend_from_slice(bytes(data, len));
    }
}

/// Creates a runtime that stores and measures through `host`. Free it with [`tj_free`].
#[no_mangle]
pub extern "C" fn tj_new(host: TjHost) -> *mut TjRuntime {
    Box::into_raw(Box::new(TjRuntime { json: JsonHost::default(), host, out: String::new() }))
}

/// # Safety
/// `rt` must come from [`tj_new`]; `source` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn tj_boot(rt: *mut TjRuntime, source: *const u8, len: usize, now: f64, seed: u64, render: c_int) -> c_int {
    let rt = &mut *rt;
    match std::str::from_utf8(bytes(source, len)) {
        Ok(src) => {
            let result = rt.json.boot(src, Box::new(rt.host), Some(Box::new(rt.host)), now, seed, render != 0);
            rt.finish(result)
        }
        Err(_) => rt.finish((INVALID, r#"{"message":"source is not UTF-8"}"#.into())),
    }
}

/// # Safety
/// `rt` must come from [`tj_new`]; `input` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn tj_handle(rt: *mut TjRuntime, now: f64, input: *const u8, len: usize) -> c_int {
    let rt = &mut *rt;
    match std::str::from_utf8(bytes(input, len)) {
        Ok(input) => {
            let result = rt.json.handle(now, input);
            rt.finish(result)
        }
        Err(_) => rt.finish((INVALID, r#"{"message":"input is not UTF-8"}"#.into())),
    }
}

/// # Safety
/// `rt` must come from [`tj_new`]; `len` must be writable.
#[no_mangle]
pub unsafe extern "C" fn tj_out(rt: *const TjRuntime, len: *mut usize) -> *const u8 {
    let rt = &*rt;
    *len = rt.out.len();
    rt.out.as_ptr()
}

/// # Safety
/// `rt` must come from [`tj_new`] and not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn tj_free(rt: *mut TjRuntime) {
    if !rt.is_null() {
        drop(Box::from_raw(rt));
    }
}

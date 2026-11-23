//! Minimal dynamic FFI loader for GTK 3, GDK 3, GLib, GObject, and Cairo via POSIX `dlopen`.
//! Requires only runtime `libgtk-3.so.0` (pre-installed on standard Linux desktops) — zero
//! `libgtk-3-dev` headers or `pkg-config` needed at compile time.

#![allow(non_camel_case_types)]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_double, c_int, c_uint, c_ulong, c_void};
use std::sync::OnceLock;

pub type GtkWidget = c_void;
pub type GtkContainer = c_void;
pub type GtkWindow = c_void;
pub type GtkFixed = c_void;
pub type GtkBox = c_void;
pub type GtkScrolledWindow = c_void;
pub type GtkAdjustment = c_void;
pub type GtkLabel = c_void;
pub type GtkButton = c_void;
pub type GtkEntry = c_void;
pub type GtkSwitch = c_void;
pub type GtkToggleButton = c_void;
pub type GtkImage = c_void;
pub type GtkCssProvider = c_void;
pub type GtkStyleContext = c_void;
pub type GtkStyleProvider = c_void;
pub type CairoSurface = c_void;
pub type CairoT = c_void;
pub type GError = c_void;
pub type GClosureNotify = Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>;
pub type GSourceFunc = unsafe extern "C" fn(*mut c_void) -> c_int;
pub type CairoWriteFunc = unsafe extern "C" fn(*mut c_void, *const u8, c_uint) -> c_int;

pub const GTK_WINDOW_TOPLEVEL: c_int = 0;
pub const GTK_ORIENTATION_HORIZONTAL: c_int = 0;
pub const GTK_POLICY_AUTOMATIC: c_int = 1;
pub const GTK_POLICY_NEVER: c_int = 2;
pub const GTK_TEXT_DIR_LTR: c_int = 1;
pub const GTK_TEXT_DIR_RTL: c_int = 2;
pub const GTK_STYLE_PROVIDER_PRIORITY_APPLICATION: c_uint = 600;
pub const PANGO_WRAP_WORD_CHAR: c_int = 2;
pub const GTK_ICON_SIZE_MENU: c_int = 1;
pub const CAIRO_FORMAT_ARGB32: c_int = 0;
pub const CAIRO_STATUS_SUCCESS: c_int = 0;
pub const G_FALSE: c_int = 0;
pub const G_TRUE: c_int = 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GtkRequisition {
    pub width: c_int,
    pub height: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GtkAllocation {
    pub x: c_int,
    pub y: c_int,
    pub width: c_int,
    pub height: c_int,
}

// Linux/glibc `<dlfcn.h>` constants.
const RTLD_LAZY: c_int = 1;
const RTLD_GLOBAL: c_int = 0x100;

#[cfg_attr(target_os = "linux", link(name = "dl"))]
extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}

unsafe fn load_lib(name: &str) -> Result<*mut c_void, String> {
    let cname = CString::new(name).unwrap();
    let h = dlopen(cname.as_ptr(), RTLD_LAZY | RTLD_GLOBAL);
    if h.is_null() {
        let err = dlerror();
        let msg = if err.is_null() {
            "unknown dlopen error".to_string()
        } else {
            CStr::from_ptr(err).to_string_lossy().into_owned()
        };
        return Err(format!("cannot load {name}: {msg}"));
    }
    Ok(h)
}

unsafe fn sym<T: Copy>(handle: *mut c_void, name: &str) -> Result<T, String> {
    let cname = CString::new(name).unwrap();
    let ptr = dlsym(handle, cname.as_ptr());
    if ptr.is_null() {
        return Err(format!("missing GTK/GLib symbol `{name}`"));
    }
    Ok(std::mem::transmute_copy(&ptr))
}

pub struct Gtk3 {
    // GTK init & loop
    pub gtk_init_check: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char) -> c_int,
    pub gtk_main: unsafe extern "C" fn(),
    pub gtk_main_quit: unsafe extern "C" fn(),
    pub gtk_events_pending: unsafe extern "C" fn() -> c_int,
    pub gtk_main_iteration_do: unsafe extern "C" fn(c_int) -> c_int,
    pub g_idle_add: unsafe extern "C" fn(GSourceFunc, *mut c_void) -> c_uint,
    pub g_timeout_add: unsafe extern "C" fn(c_uint, GSourceFunc, *mut c_void) -> c_uint,
    pub g_source_remove: unsafe extern "C" fn(c_uint) -> c_int,

    // Window & containers
    pub gtk_window_new: unsafe extern "C" fn(c_int) -> *mut GtkWidget,
    pub gtk_window_set_title: unsafe extern "C" fn(*mut GtkWindow, *const c_char),
    pub gtk_window_set_default_size: unsafe extern "C" fn(*mut GtkWindow, c_int, c_int),
    pub gtk_window_get_size: unsafe extern "C" fn(*mut GtkWindow, *mut c_int, *mut c_int),
    pub gtk_fixed_new: unsafe extern "C" fn() -> *mut GtkWidget,
    pub gtk_fixed_put: unsafe extern "C" fn(*mut GtkFixed, *mut GtkWidget, c_int, c_int),
    pub gtk_fixed_move: unsafe extern "C" fn(*mut GtkFixed, *mut GtkWidget, c_int, c_int),
    pub gtk_container_add: unsafe extern "C" fn(*mut GtkContainer, *mut GtkWidget),
    pub gtk_container_remove: unsafe extern "C" fn(*mut GtkContainer, *mut GtkWidget),
    pub gtk_scrolled_window_new: unsafe extern "C" fn(*mut GtkAdjustment, *mut GtkAdjustment) -> *mut GtkWidget,
    pub gtk_scrolled_window_set_policy: unsafe extern "C" fn(*mut GtkScrolledWindow, c_int, c_int),
    pub gtk_scrolled_window_get_vadjustment: unsafe extern "C" fn(*mut GtkScrolledWindow) -> *mut GtkAdjustment,
    pub gtk_scrolled_window_get_hadjustment: unsafe extern "C" fn(*mut GtkScrolledWindow) -> *mut GtkAdjustment,
    pub gtk_adjustment_get_value: unsafe extern "C" fn(*mut GtkAdjustment) -> c_double,
    pub gtk_adjustment_get_upper: unsafe extern "C" fn(*mut GtkAdjustment) -> c_double,
    pub gtk_adjustment_get_page_size: unsafe extern "C" fn(*mut GtkAdjustment) -> c_double,

    // Widgets
    pub gtk_label_new: unsafe extern "C" fn(*const c_char) -> *mut GtkWidget,
    pub gtk_label_set_text: unsafe extern "C" fn(*mut GtkLabel, *const c_char),
    pub gtk_label_set_line_wrap: unsafe extern "C" fn(*mut GtkLabel, c_int),
    pub gtk_label_set_line_wrap_mode: unsafe extern "C" fn(*mut GtkLabel, c_int),
    pub gtk_label_set_lines: unsafe extern "C" fn(*mut GtkLabel, c_int),
    pub gtk_label_set_xalign: unsafe extern "C" fn(*mut GtkLabel, f32),
    pub gtk_button_new_with_label: unsafe extern "C" fn(*const c_char) -> *mut GtkWidget,
    pub gtk_button_set_label: unsafe extern "C" fn(*mut GtkButton, *const c_char),
    pub gtk_entry_new: unsafe extern "C" fn() -> *mut GtkWidget,
    pub gtk_entry_set_text: unsafe extern "C" fn(*mut GtkEntry, *const c_char),
    pub gtk_entry_get_text: unsafe extern "C" fn(*mut GtkEntry) -> *const c_char,
    pub gtk_entry_set_placeholder_text: unsafe extern "C" fn(*mut GtkEntry, *const c_char),
    pub gtk_entry_set_visibility: unsafe extern "C" fn(*mut GtkEntry, c_int),
    pub gtk_switch_new: unsafe extern "C" fn() -> *mut GtkWidget,
    pub gtk_switch_set_active: unsafe extern "C" fn(*mut GtkSwitch, c_int),
    pub gtk_toggle_button_new_with_label: unsafe extern "C" fn(*const c_char) -> *mut GtkWidget,
    pub gtk_toggle_button_set_active: unsafe extern "C" fn(*mut GtkToggleButton, c_int),
    pub gtk_toggle_button_get_active: unsafe extern "C" fn(*mut GtkToggleButton) -> c_int,
    pub gtk_image_new_from_icon_name: unsafe extern "C" fn(*const c_char, c_int) -> *mut GtkWidget,
    pub gtk_image_set_from_icon_name: unsafe extern "C" fn(*mut GtkImage, *const c_char, c_int),
    pub gtk_image_set_pixel_size: unsafe extern "C" fn(*mut GtkImage, c_int),
    pub gtk_box_new: unsafe extern "C" fn(c_int, c_int) -> *mut GtkWidget,
    pub gtk_box_pack_start: unsafe extern "C" fn(*mut GtkBox, *mut GtkWidget, c_int, c_int, c_uint),

    // Widget common operations
    pub gtk_widget_set_size_request: unsafe extern "C" fn(*mut GtkWidget, c_int, c_int),
    pub gtk_widget_get_preferred_size: unsafe extern "C" fn(*mut GtkWidget, *mut GtkRequisition, *mut GtkRequisition),
    pub gtk_widget_get_preferred_width: unsafe extern "C" fn(*mut GtkWidget, *mut c_int, *mut c_int),
    pub gtk_widget_get_preferred_height_for_width: unsafe extern "C" fn(*mut GtkWidget, c_int, *mut c_int, *mut c_int),
    pub gtk_widget_show_all: unsafe extern "C" fn(*mut GtkWidget),
    pub gtk_widget_show: unsafe extern "C" fn(*mut GtkWidget),
    pub gtk_widget_destroy: unsafe extern "C" fn(*mut GtkWidget),
    pub gtk_widget_set_sensitive: unsafe extern "C" fn(*mut GtkWidget, c_int),
    pub gtk_widget_set_direction: unsafe extern "C" fn(*mut GtkWidget, c_int),
    pub gtk_widget_set_name: unsafe extern "C" fn(*mut GtkWidget, *const c_char),
    pub gtk_widget_size_allocate: unsafe extern "C" fn(*mut GtkWidget, *mut GtkAllocation),
    pub gtk_widget_get_style_context: unsafe extern "C" fn(*mut GtkWidget) -> *mut GtkStyleContext,
    pub gtk_widget_draw: unsafe extern "C" fn(*mut GtkWidget, *mut CairoT),

    // CSS & GObject
    pub gtk_css_provider_new: unsafe extern "C" fn() -> *mut GtkCssProvider,
    pub gtk_css_provider_load_from_data: unsafe extern "C" fn(*mut GtkCssProvider, *const c_char, isize, *mut *mut GError) -> c_int,
    pub gtk_style_context_add_provider: unsafe extern "C" fn(*mut GtkStyleContext, *mut GtkStyleProvider, c_uint),
    pub gtk_style_context_remove_provider: unsafe extern "C" fn(*mut GtkStyleContext, *mut GtkStyleProvider),
    pub g_signal_connect_data: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_void, *mut c_void, GClosureNotify, c_int) -> c_ulong,
    pub g_object_ref_sink: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    pub g_object_unref: unsafe extern "C" fn(*mut c_void),

    // Cairo PNG screenshots
    pub cairo_image_surface_create: unsafe extern "C" fn(c_int, c_int, c_int) -> *mut CairoSurface,
    pub cairo_create: unsafe extern "C" fn(*mut CairoSurface) -> *mut CairoT,
    pub cairo_set_source_rgb: unsafe extern "C" fn(*mut CairoT, c_double, c_double, c_double),
    pub cairo_paint: unsafe extern "C" fn(*mut CairoT),
    pub cairo_destroy: unsafe extern "C" fn(*mut CairoT),
    pub cairo_surface_destroy: unsafe extern "C" fn(*mut CairoSurface),
    pub cairo_surface_write_to_png_stream: unsafe extern "C" fn(*mut CairoSurface, CairoWriteFunc, *mut c_void) -> c_int,
}

unsafe impl Sync for Gtk3 {}
unsafe impl Send for Gtk3 {}

static INSTANCE: OnceLock<Result<Gtk3, String>> = OnceLock::new();

pub fn load() -> Result<&'static Gtk3, String> {
    INSTANCE
        .get_or_init(|| unsafe {
            let glib = load_lib("libglib-2.0.so.0")?;
            let gobj = load_lib("libgobject-2.0.so.0")?;
            let cairo = load_lib("libcairo.so.2")?;
            let _gdk = load_lib("libgdk-3.so.0")?;
            let gtk = load_lib("libgtk-3.so.0")?;

            Ok(Gtk3 {
                gtk_init_check: sym(gtk, "gtk_init_check")?,
                gtk_main: sym(gtk, "gtk_main")?,
                gtk_main_quit: sym(gtk, "gtk_main_quit")?,
                gtk_events_pending: sym(gtk, "gtk_events_pending")?,
                gtk_main_iteration_do: sym(gtk, "gtk_main_iteration_do")?,
                g_idle_add: sym(glib, "g_idle_add")?,
                g_timeout_add: sym(glib, "g_timeout_add")?,
                g_source_remove: sym(glib, "g_source_remove")?,

                gtk_window_new: sym(gtk, "gtk_window_new")?,
                gtk_window_set_title: sym(gtk, "gtk_window_set_title")?,
                gtk_window_set_default_size: sym(gtk, "gtk_window_set_default_size")?,
                gtk_window_get_size: sym(gtk, "gtk_window_get_size")?,
                gtk_fixed_new: sym(gtk, "gtk_fixed_new")?,
                gtk_fixed_put: sym(gtk, "gtk_fixed_put")?,
                gtk_fixed_move: sym(gtk, "gtk_fixed_move")?,
                gtk_container_add: sym(gtk, "gtk_container_add")?,
                gtk_container_remove: sym(gtk, "gtk_container_remove")?,
                gtk_scrolled_window_new: sym(gtk, "gtk_scrolled_window_new")?,
                gtk_scrolled_window_set_policy: sym(gtk, "gtk_scrolled_window_set_policy")?,
                gtk_scrolled_window_get_vadjustment: sym(gtk, "gtk_scrolled_window_get_vadjustment")?,
                gtk_scrolled_window_get_hadjustment: sym(gtk, "gtk_scrolled_window_get_hadjustment")?,
                gtk_adjustment_get_value: sym(gtk, "gtk_adjustment_get_value")?,
                gtk_adjustment_get_upper: sym(gtk, "gtk_adjustment_get_upper")?,
                gtk_adjustment_get_page_size: sym(gtk, "gtk_adjustment_get_page_size")?,

                gtk_label_new: sym(gtk, "gtk_label_new")?,
                gtk_label_set_text: sym(gtk, "gtk_label_set_text")?,
                gtk_label_set_line_wrap: sym(gtk, "gtk_label_set_line_wrap")?,
                gtk_label_set_line_wrap_mode: sym(gtk, "gtk_label_set_line_wrap_mode")?,
                gtk_label_set_lines: sym(gtk, "gtk_label_set_lines")?,
                gtk_label_set_xalign: sym(gtk, "gtk_label_set_xalign")?,
                gtk_button_new_with_label: sym(gtk, "gtk_button_new_with_label")?,
                gtk_button_set_label: sym(gtk, "gtk_button_set_label")?,
                gtk_entry_new: sym(gtk, "gtk_entry_new")?,
                gtk_entry_set_text: sym(gtk, "gtk_entry_set_text")?,
                gtk_entry_get_text: sym(gtk, "gtk_entry_get_text")?,
                gtk_entry_set_placeholder_text: sym(gtk, "gtk_entry_set_placeholder_text")?,
                gtk_entry_set_visibility: sym(gtk, "gtk_entry_set_visibility")?,
                gtk_switch_new: sym(gtk, "gtk_switch_new")?,
                gtk_switch_set_active: sym(gtk, "gtk_switch_set_active")?,
                gtk_toggle_button_new_with_label: sym(gtk, "gtk_toggle_button_new_with_label")?,
                gtk_toggle_button_set_active: sym(gtk, "gtk_toggle_button_set_active")?,
                gtk_toggle_button_get_active: sym(gtk, "gtk_toggle_button_get_active")?,
                gtk_image_new_from_icon_name: sym(gtk, "gtk_image_new_from_icon_name")?,
                gtk_image_set_from_icon_name: sym(gtk, "gtk_image_set_from_icon_name")?,
                gtk_image_set_pixel_size: sym(gtk, "gtk_image_set_pixel_size")?,
                gtk_box_new: sym(gtk, "gtk_box_new")?,
                gtk_box_pack_start: sym(gtk, "gtk_box_pack_start")?,

                gtk_widget_set_size_request: sym(gtk, "gtk_widget_set_size_request")?,
                gtk_widget_get_preferred_size: sym(gtk, "gtk_widget_get_preferred_size")?,
                gtk_widget_get_preferred_width: sym(gtk, "gtk_widget_get_preferred_width")?,
                gtk_widget_get_preferred_height_for_width: sym(gtk, "gtk_widget_get_preferred_height_for_width")?,
                gtk_widget_show_all: sym(gtk, "gtk_widget_show_all")?,
                gtk_widget_show: sym(gtk, "gtk_widget_show")?,
                gtk_widget_destroy: sym(gtk, "gtk_widget_destroy")?,
                gtk_widget_set_sensitive: sym(gtk, "gtk_widget_set_sensitive")?,
                gtk_widget_set_direction: sym(gtk, "gtk_widget_set_direction")?,
                gtk_widget_set_name: sym(gtk, "gtk_widget_set_name")?,
                gtk_widget_size_allocate: sym(gtk, "gtk_widget_size_allocate")?,
                gtk_widget_get_style_context: sym(gtk, "gtk_widget_get_style_context")?,
                gtk_widget_draw: sym(gtk, "gtk_widget_draw")?,

                gtk_css_provider_new: sym(gtk, "gtk_css_provider_new")?,
                gtk_css_provider_load_from_data: sym(gtk, "gtk_css_provider_load_from_data")?,
                gtk_style_context_add_provider: sym(gtk, "gtk_style_context_add_provider")?,
                gtk_style_context_remove_provider: sym(gtk, "gtk_style_context_remove_provider")?,
                g_signal_connect_data: sym(gobj, "g_signal_connect_data")?,
                g_object_ref_sink: sym(gobj, "g_object_ref_sink")?,
                g_object_unref: sym(gobj, "g_object_unref")?,

                cairo_image_surface_create: sym(cairo, "cairo_image_surface_create")?,
                cairo_create: sym(cairo, "cairo_create")?,
                cairo_set_source_rgb: sym(cairo, "cairo_set_source_rgb")?,
                cairo_paint: sym(cairo, "cairo_paint")?,
                cairo_destroy: sym(cairo, "cairo_destroy")?,
                cairo_surface_destroy: sym(cairo, "cairo_surface_destroy")?,
                cairo_surface_write_to_png_stream: sym(cairo, "cairo_surface_write_to_png_stream")?,
            })
        })
        .as_ref()
        .map_err(|e| e.clone())
}

pub fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_else(|_| CString::new("").unwrap())
}

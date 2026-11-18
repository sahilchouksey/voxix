use crate::input;
use crate::settings;
use crate::settings::{OverlayPosition, OverlayStyle};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize};

#[cfg(not(target_os = "macos"))]
use log::debug;

#[cfg(not(target_os = "macos"))]
use tauri::WebviewWindowBuilder;

#[cfg(target_os = "macos")]
use tauri::WebviewUrl;

#[cfg(target_os = "macos")]
use tauri_nspanel::{tauri_panel, CollectionBehavior, PanelBuilder, PanelLevel, StyleMask};

#[cfg(target_os = "linux")]
use gtk_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

#[cfg(target_os = "linux")]
use std::env;

#[cfg(target_os = "linux")]
use gtk::prelude::*;

/// Make the overlay window fully transparent on X11 with picom.
///
/// Follows the official WebKitGTK recipe exactly:
/// https://webkitgtk.org/reference/webkit2gtk/stable/method.WebView.set_background_color.html
///
/// "The parent window must have a RGBA visual and GtkWidget:app-paintable
///  property set to TRUE for background colors to work."
///
/// Steps:
/// 1. Get RGBA visual from screen: gdk_screen_get_rgba_visual()
/// 2. Set window visual: gtk_widget_set_visual(window, rgba_visual)
/// 3. Set window app-paintable: gtk_widget_set_app_paintable(window, TRUE)
/// 4. Set webview background: webkit_web_view_set_background_color(webview, transparent)
#[cfg(target_os = "linux")]
fn set_webview_transparent(overlay_window: &tauri::webview::WebviewWindow) {
    // FFI declarations for GTK3 + WebKitGTK functions not exposed by gtk-rs
    #[link(name = "gdk-3")]
    extern "C" {
        fn gdk_screen_get_rgba_visual(screen: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        fn gdk_screen_is_composited(screen: *mut std::ffi::c_void) -> i32;
    }

    #[link(name = "gtk-3")]
    extern "C" {
        fn gtk_widget_set_visual(widget: *mut std::ffi::c_void, visual: *mut std::ffi::c_void);
        fn gtk_widget_set_app_paintable(widget: *mut std::ffi::c_void, app_paintable: i32);
    }

    #[link(name = "webkit2gtk-4.1")]
    extern "C" {
        fn webkit_web_view_set_background_color(
            web_view: *mut std::ffi::c_void,
            rgba: *const GdkRGBA,
        );
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct GdkRGBA {
        red: f64,
        green: f64,
        blue: f64,
        alpha: f64,
    }

    let window_clone = overlay_window.clone();
    let _ = overlay_window.run_on_main_thread(move || {
        let Ok(gtk_win) = window_clone.gtk_window() else {
            return;
        };

        // Step 1: Get the RGBA visual from the screen
        let screen_ptr = unsafe {
            #[link(name = "gtk-3")]
            extern "C" {
                fn gtk_widget_get_screen(widget: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
            }
            gtk_widget_get_screen(gtk_win.as_ptr() as *mut std::ffi::c_void)
        };

        if screen_ptr.is_null() {
            log::warn!("overlay: null screen pointer");
            return;
        }

        let is_composited = unsafe { gdk_screen_is_composited(screen_ptr) };
        log::info!("overlay: screen composited={}", is_composited != 0);

        let rgba_visual = unsafe { gdk_screen_get_rgba_visual(screen_ptr) };
        if rgba_visual.is_null() {
            log::warn!("overlay: no RGBA visual available — transparency impossible");
            return;
        }

        // Step 2: Set the window's visual to the RGBA visual
        unsafe {
            gtk_widget_set_visual(gtk_win.as_ptr() as *mut std::ffi::c_void, rgba_visual);
        }
        log::info!("overlay: set RGBA visual on window");

        // Step 3: Set the window to app-paintable
        unsafe {
            gtk_widget_set_app_paintable(gtk_win.as_ptr() as *mut std::ffi::c_void, 1);
        }

        // Step 4: Find the WebKitWebView and set its background to transparent
        fn find_and_set_webview(widget: &gtk::Widget) -> bool {
            let name = widget.widget_name().to_string();
            if name.to_lowercase().contains("webview") || name.to_lowercase().contains("webkit") {
                let rgba = GdkRGBA {
                    red: 0.0,
                    green: 0.0,
                    blue: 0.0,
                    alpha: 0.0,
                };
                unsafe {
                    webkit_web_view_set_background_color(
                        widget.as_ptr() as *mut std::ffi::c_void,
                        &rgba,
                    );
                }
                log::info!("overlay: set {} background transparent", name);
                return true;
            }
            if let Some(container) = widget.downcast_ref::<gtk::Container>() {
                for child in container.children() {
                    if find_and_set_webview(&child) {
                        return true;
                    }
                }
            }
            false
        }

        if !find_and_set_webview(gtk_win.upcast_ref::<gtk::Widget>()) {
            log::warn!("overlay: WebKitWebView widget not found in tree");
        }

        log::info!(
            "overlay: transparency setup complete (RGBA visual + app-paintable + transparent bg)"
        );
    });
}

/// Set X11 always-on-top and all-desktops properties on the overlay window.
/// Must be called on the main thread. These properties take effect when the
/// window is mapped — set BEFORE `show()`.
#[cfg(target_os = "linux")]
fn force_x11_above_all_desktops(xid: u64) {
    #[link(name = "X11")]
    extern "C" {
        fn XOpenDisplay(name: *const i8) -> *mut std::ffi::c_void;
        fn XChangeProperty(
            dpy: *mut std::ffi::c_void,
            win: u64,
            property: u64,
            type_: u64,
            format: i32,
            mode: i32,
            data: *const u8,
            nelements: i32,
        ) -> i32;
        fn XInternAtom(dpy: *mut std::ffi::c_void, name: *const i8, only_if_exists: i32) -> u64;
        fn XFlush(dpy: *mut std::ffi::c_void) -> i32;
        fn XSync(dpy: *mut std::ffi::c_void, discard_errors: i32) -> i32;
        fn XCloseDisplay(dpy: *mut std::ffi::c_void) -> i32;
        fn XMapRaised(dpy: *mut std::ffi::c_void, win: u64) -> i32;
    }

    unsafe {
        let dpy = XOpenDisplay(std::ptr::null());
        if dpy.is_null() {
            log::error!("overlay: XOpenDisplay failed");
            return;
        }
        // _NET_WM_STATE_ABOVE — keep always on top
        let above = XInternAtom(dpy, b"_NET_WM_STATE_ABOVE\0".as_ptr() as *const i8, 0);
        let state_atom = XInternAtom(dpy, b"_NET_WM_STATE\0".as_ptr() as *const i8, 0);
        XChangeProperty(
            dpy,
            xid,
            state_atom,
            4u64, // XA_ATOM
            32,
            0, // PropModeReplace
            &above as *const u64 as *const u8,
            1,
        );
        // _NET_WM_DESKTOP = 0xFFFFFFFF → show on ALL desktops
        let desktop = XInternAtom(dpy, b"_NET_WM_DESKTOP\0".as_ptr() as *const i8, 0);
        let all_desktops: u32 = 0xFFFFFFFF;
        XChangeProperty(
            dpy,
            xid,
            desktop,
            6u64, // XA_CARDINAL
            32,
            0,
            &all_desktops as *const u32 as *const u8,
            1,
        );
        // _NET_WM_STATE_MODAL intentionally NOT set — modal forces focus on
        // most WMs, which steals keyboard from the user's active window.
        XFlush(dpy);
        XSync(dpy, 0);
        XCloseDisplay(dpy);
        log::info!("overlay: forced above + all desktops + raised, xid={}", xid);
    }
}

/// Tell all X11 window managers (dwm, i3, bspwm, awesome, openbox) to never
/// give keyboard focus to this window. Uses the ICCCM-standard WM_HINTS with
/// input=False. Without this, tiling WMs focus the overlay on map, stealing
/// keyboard input from the user's active window — the paste then goes to the
/// overlay instead of the user's text field.
///
/// WM_HINTS layout (ICCCM §4.1.2.4):
///   flags | input | initial_state | icon_pixmap | icon_window | icon_x |
///   icon_y | icon_mask | window_group
/// flags=InputHint(1), input=False(0) means "WM must not set input focus here".
/// dwm: updatewmhints() → c->neverfocus=1, skipped in focus()
/// i3:  con_new() checks WM_HINTS, marks window as not focusable
/// bspwm: same ICCCM path
#[cfg(target_os = "linux")]
fn force_x11_no_focus(xid: u64) {
    #[repr(C)]
    struct XWMHints {
        flags: i64,
        input: i32,
        initial_state: i32,
        icon_pixmap: u64,
        icon_window: u64,
        icon_x: i32,
        icon_y: i32,
        icon_mask: u64,
        window_group: u64,
    }

    #[link(name = "X11")]
    extern "C" {
        fn XOpenDisplay(name: *const i8) -> *mut std::ffi::c_void;
        fn XSetWMHints(dpy: *mut std::ffi::c_void, win: u64, hints: *const XWMHints) -> i32;
        fn XFlush(dpy: *mut std::ffi::c_void) -> i32;
        fn XCloseDisplay(dpy: *mut std::ffi::c_void) -> i32;
    }

    let hints = XWMHints {
        flags: 1, // InputHint
        input: 0, // False — never give us keyboard focus
        initial_state: 0,
        icon_pixmap: 0,
        icon_window: 0,
        icon_x: 0,
        icon_y: 0,
        icon_mask: 0,
        window_group: 0,
    };

    unsafe {
        let dpy = XOpenDisplay(std::ptr::null());
        if dpy.is_null() {
            return;
        }
        XSetWMHints(dpy, xid, &hints);
        XFlush(dpy);
        XCloseDisplay(dpy);
        log::info!(
            "overlay: set WM_HINTS input=False (never focus), xid={}",
            xid
        );
    }
}

#[cfg(target_os = "macos")]
tauri_panel! {
    panel!(RecordingOverlayPanel {
        config: {
            can_become_key_window: false,
            is_floating_panel: true
        }
    })
}

// Native overlay window sizes (logical points). One window is reused for every
// state and resized in `show_overlay_state`; each size need only be at least as
// large as the card it hosts. Window height fits the pill + hint column (100x48
// compact).
const OVERLAY_WIDTH: f64 = 100.0;
const OVERLAY_HEIGHT: f64 = 48.0;

// Actual is 394x118, just a little extra
const OVERLAY_STREAM_WIDTH: f64 = 400.0;
const OVERLAY_STREAM_HEIGHT: f64 = 120.0;

// Wide pill for surfacing a short error line (below-transcribing context:
// the settings window is usually hidden during dictation).
const OVERLAY_ERROR_WIDTH: f64 = 320.0;
const OVERLAY_ERROR_HEIGHT: f64 = 64.0;

/// Conservative height for the error-pill window: the pill sizes to its text
/// (15px line-height + 12px vertical padding + ~2px borders), so a fixed
/// window clips multi-line messages top and bottom. Wraps are estimated at
/// 44 chars/line, capped at 6 lines.
fn estimate_error_height(text: &str) -> f64 {
    let cpl = 44usize;
    let lines = text.chars().count().div_ceil(cpl).clamp(1, 6) as f64;
    lines * 15.0 + 14.0
}

/// Overlay window size (logical) for a given UI state. For the error state an
/// `error_height` override from [`estimate_error_height`] sizes the window to
/// the message; `None` keeps the fixed default.
fn overlay_dimensions(state: &str, error_height: Option<f64>) -> (f64, f64) {
    if state == "streaming" {
        (OVERLAY_STREAM_WIDTH, OVERLAY_STREAM_HEIGHT)
    } else if state == "error" {
        (
            OVERLAY_ERROR_WIDTH,
            error_height.unwrap_or(OVERLAY_ERROR_HEIGHT),
        )
    } else {
        (OVERLAY_WIDTH, OVERLAY_HEIGHT)
    }
}

/// Monotonic generation for the error pill's auto-hide. Any non-error
/// `show_overlay_state` bumps it, which cancels a pending error auto-hide so
/// a fresh recording started during the error window is never hidden early.
static OVERLAY_ERROR_GEN: AtomicU64 = AtomicU64::new(0);

/// True when `needle` occurs in `haystack` as a whole word: surrounded on
/// both sides by the string's edges or non-alphanumeric characters. Used to
/// avoid the old bare-`"io"` substring matching words like "permission" or
/// "audio". `needle` must be lowercase ASCII.
fn contains_word(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    if needle.is_empty() || needle.len() > bytes.len() {
        return false;
    }
    let mut start = 0;
    while let Some(idx) = haystack[start..].find(needle) {
        let abs = start + idx;
        let end = abs + needle.len();
        let before_ok = abs == 0 || !bytes[abs - 1].is_ascii_alphanumeric();
        let after_ok = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
        // A found match starts at a char boundary; ASCII needles are one byte
        // wide, so abs + 1 is also a boundary.
        start = abs + 1;
    }
    false
}

/// Map raw backend errors to a short, user-readable line for the overlay pill.
pub fn summarize_error(err: &str) -> String {
    let lower = err.to_lowercase();
    // Microphone permission first: the recording gate shows this pill text
    // verbatim, and the bare "permission" bucket below would otherwise miss
    // the actionable System Settings instructions.
    if lower.contains("microphone") {
        "Microphone access needed — enable voxix in System Settings → Privacy & Security → Microphone".to_string()
    } else if lower.contains("accessibility")
        || lower.contains("permission")
        || lower.contains("simulate input")
    {
        "Transcription copied — grant Accessibility to paste".to_string()
    } else if lower.contains("not running")
        || lower.contains("connect")
        || lower.contains("reset")
        || lower.contains("refused")
        || lower.contains("unreachable")
        || lower.contains("os error")
        || lower.contains("i/o")
        || contains_word(&lower, "io")
    {
        "Transcription failed — service unreachable".to_string()
    } else if lower.contains("busy") || lower.contains("another") || lower.contains("in flight") {
        "Transcription failed — engine busy, try again".to_string()
    } else if lower.contains("timed out") || lower.contains("timeout") {
        "Transcription failed — timed out".to_string()
    } else {
        // Flatten control characters and whitespace so a multi-line error
        // never renders as raw garbage in the single-line pill.
        err.chars()
            .map(|c| {
                if c.is_control() || c.is_whitespace() {
                    ' '
                } else {
                    c
                }
            })
            .take(80)
            .collect()
    }
}

/// Show the compact overlay in the `error` state with a short message. The
/// pill auto-hides after 5s; any newer non-error state (e.g. a fresh
/// recording) cancels the pending auto-hide. Respects the same
/// `overlay_style == None` gate as the rest of the overlay.
pub fn show_error_overlay(app_handle: &AppHandle, message: &str) {
    let gen = OVERLAY_ERROR_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let short = summarize_error(message);
    let closure_handle = app_handle.clone();
    let short_msg = short.clone();
    let _ = app_handle.run_on_main_thread(move || {
        if let Some(win) = closure_handle.get_webview_window("recording_overlay") {
            let _ = win.emit("overlay-error-message", short_msg);
        }
    });
    show_overlay_state(
        app_handle,
        "error",
        Some(estimate_error_height(&short).max(80.0)),
    );
    let hide_handle = app_handle.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(5));
        if OVERLAY_ERROR_GEN.load(Ordering::SeqCst) == gen {
            hide_recording_overlay(&hide_handle);
        }
    });
}

static LAST_MIC_LEVEL_EMIT: AtomicU64 = AtomicU64::new(0);
const EMIT_THROTTLE_MS: u64 = 33; // ~30 FPS

#[cfg(target_os = "macos")]
const OVERLAY_TOP_OFFSET: f64 = 46.0;
#[cfg(any(target_os = "windows", target_os = "linux"))]
const OVERLAY_TOP_OFFSET: f64 = 8.0;

#[cfg(target_os = "macos")]
const OVERLAY_BOTTOM_OFFSET: f64 = 15.0;

#[cfg(any(target_os = "windows", target_os = "linux"))]
const OVERLAY_BOTTOM_OFFSET: f64 = 40.0;

#[cfg(target_os = "linux")]
fn update_gtk_layer_shell_anchors(overlay_window: &tauri::webview::WebviewWindow) {
    let window_clone = overlay_window.clone();
    let _ = overlay_window.run_on_main_thread(move || {
        // Try to get the GTK window from the Tauri webview
        if let Ok(gtk_window) = window_clone.gtk_window() {
            let settings = settings::get_settings(window_clone.app_handle());
            match settings.overlay_position {
                OverlayPosition::Top => {
                    gtk_window.set_anchor(Edge::Top, true);
                    gtk_window.set_anchor(Edge::Bottom, false);
                }
                OverlayPosition::Bottom => {
                    gtk_window.set_anchor(Edge::Bottom, true);
                    gtk_window.set_anchor(Edge::Top, false);
                }
            }
        }
    });
}

/// Returns true when the environment variable is set to a truthy value
/// (e.g. "1", "true", "yes", "on").
/// "0", "false", "no", "off" and empty string are treated as falsy (case-insensitive).
/// Returns false when the variable is not set.
#[cfg(target_os = "linux")]
fn env_flag_enabled(name: &str) -> bool {
    match env::var(name) {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        ),
        Err(_) => false,
    }
}

/// Initializes GTK layer shell for Linux overlay window
/// Returns true if layer shell was successfully initialized, false otherwise
#[cfg(target_os = "linux")]
fn init_gtk_layer_shell(overlay_window: &tauri::webview::WebviewWindow) -> bool {
    if env_flag_enabled("VOXIX_NO_GTK_LAYER_SHELL") {
        debug!("Skipping GTK layer shell init (VOXIX_NO_GTK_LAYER_SHELL is enabled)");
        return false;
    }

    if !gtk_layer_shell::is_supported() {
        log::info!("overlay: GTK layer shell NOT supported on this system");
        return false;
    }

    // Try to get the GTK window from the Tauri webview
    if let Ok(gtk_window) = overlay_window.gtk_window() {
        // Initialize layer shell
        gtk_window.init_layer_shell();
        gtk_window.set_layer(Layer::Overlay);
        gtk_window.set_keyboard_mode(KeyboardMode::None);
        gtk_window.set_exclusive_zone(0);

        update_gtk_layer_shell_anchors(overlay_window);

        log::info!("overlay: GTK layer shell initialized with KeyboardMode::None (no focus)");
        return true;
    }
    log::info!("overlay: GTK layer shell failed to get GTK window");
    false
}

/// Forces a window to be topmost using Win32 API (Windows only)
/// This is more reliable than Tauri's set_always_on_top which can be overridden
#[cfg(target_os = "windows")]
fn force_overlay_topmost(overlay_window: &tauri::webview::WebviewWindow) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    };

    // Clone because run_on_main_thread takes 'static
    let overlay_clone = overlay_window.clone();

    // Make sure the Win32 call happens on the UI thread
    let _ = overlay_clone.clone().run_on_main_thread(move || {
        if let Ok(hwnd) = overlay_clone.hwnd() {
            unsafe {
                // Force Z-order: make this window topmost without changing size/pos or stealing focus
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        }
    });
}

fn get_monitor_with_cursor(app_handle: &AppHandle) -> Option<tauri::Monitor> {
    if let Some(mouse_location) = input::get_cursor_position(app_handle) {
        if let Ok(monitors) = app_handle.available_monitors() {
            for monitor in monitors {
                // On Windows both the cursor (enigo -> GetCursorPos) and the
                // monitor bounds are physical pixels, so compare them directly.
                #[cfg(target_os = "windows")]
                if is_mouse_within_monitor(mouse_location, monitor.position(), monitor.size()) {
                    return Some(monitor);
                }

                // macOS/Linux: enigo returns logical coords, so scale the bounds down.
                #[cfg(not(target_os = "windows"))]
                {
                    let scale = monitor.scale_factor();
                    let pos = PhysicalPosition::new(
                        (monitor.position().x as f64 / scale) as i32,
                        (monitor.position().y as f64 / scale) as i32,
                    );
                    let size = PhysicalSize::new(
                        (monitor.size().width as f64 / scale) as u32,
                        (monitor.size().height as f64 / scale) as u32,
                    );
                    if is_mouse_within_monitor(mouse_location, &pos, &size) {
                        return Some(monitor);
                    }
                }
            }
        }
    }

    app_handle.primary_monitor().ok().flatten()
}

fn is_mouse_within_monitor(
    mouse_pos: (i32, i32),
    monitor_pos: &PhysicalPosition<i32>,
    monitor_size: &PhysicalSize<u32>,
) -> bool {
    let (mouse_x, mouse_y) = mouse_pos;
    let PhysicalPosition {
        x: monitor_x,
        y: monitor_y,
    } = *monitor_pos;
    let PhysicalSize {
        width: monitor_width,
        height: monitor_height,
    } = *monitor_size;

    mouse_x >= monitor_x
        && mouse_x < (monitor_x + monitor_width as i32)
        && mouse_y >= monitor_y
        && mouse_y < (monitor_y + monitor_height as i32)
}

/// Returns overlay position in logical coordinates (points on macOS).
///
/// Top-center of the monitor under the cursor (or primary monitor fallback),
/// offset by OVERLAY_TOP_OFFSET / OVERLAY_BOTTOM_OFFSET. This matches the
/// industry convention (top-edge anchored, never window-relative) and avoids
/// the unreliable XGetInputFocus path on XWayland/Hyprland.
///
/// The Bottom anchor uses the macOS work area (visibleFrame) so the overlay
/// tracks the Dock — above it when shown, at the screen edge when hidden.
/// This relies on tauri 2.11's work_area.position.y fix (#14655), the same
/// bug that led PR #969 to abandon work_area for full monitor bounds. Top and
/// the other platforms keep full monitor bounds plus the fixed offsets
/// (work_area is unreliable on Wayland; Windows' offset clears the taskbar).
///
/// We must use LogicalPosition (not PhysicalPosition) because Tauri/tao
/// converts PhysicalPosition using the scale factor of the monitor the window
/// is *currently* on, which is wrong when moving cross-monitor. Windows uses
/// `place_windows_overlay` instead (no single logical space across mixed DPI).
fn calculate_overlay_position(
    app_handle: &AppHandle,
    width: f64,
    height: f64,
) -> Option<(f64, f64)> {
    let monitor = get_monitor_with_cursor(app_handle)?;
    let scale = monitor.scale_factor();
    let monitor_x = monitor.position().x as f64 / scale;
    let monitor_y = monitor.position().y as f64 / scale;
    let monitor_width = monitor.size().width as f64 / scale;
    let monitor_height = monitor.size().height as f64 / scale;

    // Work-area bounds (same source as the Bottom branch below) so the window
    // can never start above or below the visible screen on odd monitor
    // arrangements or fractional scales. Some Linux backends report an empty
    // work area; fall back to full monitor bounds there rather than clamping
    // everything to a zero-height rect.
    let wa = monitor.work_area();
    let (wa_pos, wa_size) = if wa.size.width == 0 || wa.size.height == 0 {
        (*monitor.position(), *monitor.size())
    } else {
        (wa.position, wa.size)
    };
    let wa_top = wa_pos.y as f64 / scale;
    let wa_bottom = wa_top + wa_size.height as f64 / scale;

    let settings = settings::get_settings(app_handle);

    let x = monitor_x + (monitor_width - width) / 2.0;
    let y = match settings.overlay_position {
        OverlayPosition::Top => monitor_y + OVERLAY_TOP_OFFSET,
        OverlayPosition::Bottom => {
            #[cfg(target_os = "macos")]
            let bottom = {
                let wa = monitor.work_area();
                (wa.position.y as f64 + wa.size.height as f64) / scale
            };
            #[cfg(not(target_os = "macos"))]
            let bottom = monitor_y + monitor.size().height as f64 / scale;

            bottom - height - OVERLAY_BOTTOM_OFFSET
        }
    };
    // Hard-clamp into the work area: at least 4px inside the top edge, and
    // pulled up if the window would extend past the bottom edge.
    let y = y.clamp(wa_top + 4.0, (wa_bottom - height - 4.0).max(wa_top + 4.0));

    log::debug!(
        "overlay: monitor position=({},{}) monitor=({},{}) {}x{}",
        x,
        y,
        monitor_x,
        monitor_y,
        monitor_width,
        monitor_height
    );
    Some((x, y))
}

/// Current overlay window size in logical units (points), for repositioning
/// without assuming a fixed size (compact vs. streaming).
#[cfg(not(target_os = "windows"))]
fn current_overlay_logical_size(window: &tauri::webview::WebviewWindow) -> Option<(f64, f64)> {
    let size = window.inner_size().ok()?;
    let scale = window.scale_factor().ok()?;
    Some((size.width as f64 / scale, size.height as f64 / scale))
}

#[cfg(target_os = "windows")]
use std::sync::atomic::AtomicU32;
#[cfg(target_os = "windows")]
use std::sync::atomic::AtomicU8;

#[cfg(target_os = "windows")]
static WINDOWS_OVERLAY_MODE: AtomicU8 = AtomicU8::new(0);
/// Last applied error-state height (logical, rounded), so a different-length
/// message re-resizes instead of being skipped by the mode-only comparison,
/// and the reposition path can rebuild the same size.
#[cfg(target_os = "windows")]
static WINDOWS_OVERLAY_ERROR_HEIGHT: AtomicU32 = AtomicU32::new(0);

/// Overlay rectangle in the destination monitor's physical pixels, so nothing
/// is converted through the window's previous-monitor DPI.
#[cfg(target_os = "windows")]
fn windows_overlay_bounds(
    monitor_position: PhysicalPosition<i32>,
    monitor_size: PhysicalSize<u32>,
    scale: f64,
    logical_width: f64,
    logical_height: f64,
    overlay_position: OverlayPosition,
) -> (i32, i32, i32, i32) {
    let width = (logical_width * scale).round().max(1.0) as i32;
    let height = (logical_height * scale).round().max(1.0) as i32;
    let x = (monitor_position.x as f64 + (monitor_size.width as f64 - width as f64) / 2.0).round()
        as i32;
    let y = match overlay_position {
        OverlayPosition::Top => {
            (monitor_position.y as f64 + OVERLAY_TOP_OFFSET * scale).round() as i32
        }
        OverlayPosition::Bottom => (monitor_position.y as f64 + monitor_size.height as f64
            - height as f64
            - OVERLAY_BOTTOM_OFFSET * scale)
            .round() as i32,
    };

    (x, y, width, height)
}

/// Moves and sizes the overlay in one native SetWindowPos, bypassing tao's
/// current-DPI logical conversion that mislands cross-monitor moves.
#[cfg(target_os = "windows")]
fn place_windows_overlay(
    app_handle: &AppHandle,
    overlay_window: &tauri::webview::WebviewWindow,
    logical_width: f64,
    logical_height: f64,
) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER};

    let monitor = get_monitor_with_cursor(app_handle)
        .ok_or_else(|| "failed to determine the monitor containing the cursor".to_string())?;
    let (x, y, width, height) = windows_overlay_bounds(
        *monitor.position(),
        *monitor.size(),
        monitor.scale_factor(),
        logical_width,
        logical_height,
        settings::get_settings(app_handle).overlay_position,
    );
    let hwnd = overlay_window
        .hwnd()
        .map_err(|error| format!("failed to get overlay window handle: {error}"))?;

    unsafe {
        SetWindowPos(
            hwnd,
            None,
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
        .map_err(|error| format!("failed to set overlay bounds: {error}"))?;
    }

    log::debug!(
        "windows overlay bounds: x={} y={} width={} height={} scale={}",
        x,
        y,
        width,
        height,
        monitor.scale_factor()
    );
    Ok(())
}

/// Creates the recording overlay window and keeps it hidden by default
#[cfg(not(target_os = "macos"))]
pub fn create_recording_overlay(app_handle: &AppHandle) {
    // On Linux (Wayland), monitor detection often fails, but we don't need exact coordinates
    // for Layer Shell as we use anchors. On other platforms, we require a monitor.
    #[cfg(not(target_os = "linux"))]
    {
        let position = calculate_overlay_position(app_handle, OVERLAY_WIDTH, OVERLAY_HEIGHT);
        if position.is_none() {
            debug!("Failed to determine overlay position, not creating overlay window");
            return;
        }
    }

    // Position starts unset — update_overlay_position() sets the correct
    // LogicalPosition before the overlay is shown.
    let mut builder = WebviewWindowBuilder::new(
        app_handle,
        "recording_overlay",
        tauri::WebviewUrl::App("ui/overlay/index.html".into()),
    )
    .title("Recording")
    .resizable(false)
    .inner_size(OVERLAY_WIDTH, OVERLAY_HEIGHT)
    .shadow(false)
    .maximizable(false)
    .minimizable(false)
    .closable(false)
    .accept_first_mouse(true)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .transparent(true)
    .focusable(false)
    .focused(false)
    .visible(false);

    if let Some(data_dir) = crate::portable::data_dir() {
        builder = builder.data_directory(data_dir.join("webview"));
    }

    #[allow(unused_variables)]
    match builder.build() {
        Ok(window) => {
            #[cfg(target_os = "linux")]
            {
                // Try to initialize GTK layer shell, ignore errors if compositor doesn't support it
                if init_gtk_layer_shell(&window) {
                    debug!("GTK layer shell initialized for overlay window");
                } else {
                    debug!("GTK layer shell not available, falling back to regular window");
                }
                // Set WebKitGTK background to fully transparent so picom
                // can composite the pill without any opaque background tint.
                set_webview_transparent(&window);
            }

            debug!("Recording overlay window created successfully (hidden)");
        }
        Err(e) => {
            debug!("Failed to create recording overlay window: {}", e);
        }
    }
}

/// Creates the recording overlay panel and keeps it hidden by default (macOS)
#[cfg(target_os = "macos")]
pub fn create_recording_overlay(app_handle: &AppHandle) {
    if let Some((x, y)) = calculate_overlay_position(app_handle, OVERLAY_WIDTH, OVERLAY_HEIGHT) {
        // PanelBuilder creates a Tauri window then converts it to NSPanel.
        // The window remains registered, so get_webview_window() still works.
        match PanelBuilder::<_, RecordingOverlayPanel>::new(app_handle, "recording_overlay")
            .url(WebviewUrl::App("ui/overlay/index.html".into()))
            .title("Recording")
            .position(tauri::Position::Logical(tauri::LogicalPosition { x, y }))
            .level(PanelLevel::Status)
            .size(tauri::Size::Logical(tauri::LogicalSize {
                width: OVERLAY_WIDTH,
                height: OVERLAY_HEIGHT,
            }))
            .has_shadow(false)
            .transparent(true)
            .no_activate(true)
            .corner_radius(0.0)
            .style_mask(StyleMask::empty().borderless().nonactivating_panel())
            .with_window(|w| w.decorations(false).transparent(true).focusable(false))
            .collection_behavior(
                CollectionBehavior::new()
                    .can_join_all_spaces()
                    .full_screen_auxiliary(),
            )
            .build()
        {
            Ok(panel) => {
                panel.hide();
            }
            Err(e) => {
                log::error!("Failed to create recording overlay panel: {}", e);
            }
        }
    }
}

fn show_overlay_state(app_handle: &AppHandle, state: &str, error_height: Option<f64>) {
    // Whether the overlay shows at all is governed by overlay_style; position
    // only chooses Top vs Bottom placement. Checked here (off the main thread)
    // so the common overlay-disabled case never pays for a main-thread hop.
    let settings = settings::get_settings(app_handle);
    if settings.overlay_style == OverlayStyle::None {
        return;
    }

    // The rest queries monitors and the cursor and mutates window geometry. On
    // Linux the monitor/cursor lookups hit GDK/Xlib on the process's shared X11
    // connection, which is only safe from the GTK main thread — running them on
    // a background thread corrupts the connection and hard-crashes the app
    // (issue #227). Hop to the main thread on every platform to keep the
    // geometry path uniform (a no-op cost on Windows, and it also keeps macOS's
    // NSScreen access main-thread-correct). run_on_main_thread runs the closure
    // inline when already on the main thread, so this never deadlocks.
    let handle = app_handle.clone();
    let state = state.to_string();
    let _ = app_handle
        .run_on_main_thread(move || show_overlay_state_on_main(&handle, &state, error_height));
}

fn show_overlay_state_on_main(app_handle: &AppHandle, state: &str, error_height: Option<f64>) {
    // Any non-error state invalidates a pending error auto-hide.
    if state != "error" {
        OVERLAY_ERROR_GEN.fetch_add(1, Ordering::SeqCst);
    }
    // Size the overlay for this state (compact vs. streaming), then position it.
    let (width, height) = overlay_dimensions(state, error_height);
    if let Some(overlay_window) = app_handle.get_webview_window("recording_overlay") {
        // If the overlay is already visible (e.g. recording -> transcribing)
        // and this state's dimensions match the window's current size, keep
        // the exact same position: re-running set_size/set_position can shift
        // the window by a pixel or two (focused-window re-resolution,
        // rounding), which reads as the pill "jumping" mid-session. The pill
        // centers itself in the fixed-size window, so only the state event
        // is needed. When the dimensions DO differ (e.g. recording -> error),
        // the window must be resized and repositioned first, or the frontend
        // renders the pill at its inline width inside the old compact window
        // and the text clips at both edges.
        let already_visible = overlay_window.is_visible().unwrap_or(false);
        #[cfg(not(target_os = "windows"))]
        let needs_resize = current_overlay_logical_size(&overlay_window)
            .map(|(cw, ch)| (cw - width).abs() > 0.5 || (ch - height).abs() > 0.5)
            .unwrap_or(true);
        #[cfg(target_os = "windows")]
        let needs_resize = {
            let target_mode = match state {
                "streaming" => 1u8,
                "error" => 2u8,
                _ => 0u8,
            };
            let target_height = if target_mode == 2 {
                error_height.unwrap_or(OVERLAY_ERROR_HEIGHT).round() as u32
            } else {
                0
            };
            WINDOWS_OVERLAY_MODE.load(Ordering::Relaxed) != target_mode
                || (target_mode == 2
                    && WINDOWS_OVERLAY_ERROR_HEIGHT.load(Ordering::Relaxed) != target_height)
        };
        if already_visible && !needs_resize {
            let _ = overlay_window.emit("show-overlay", state);
            return;
        }

        #[cfg(target_os = "linux")]
        update_gtk_layer_shell_anchors(&overlay_window);

        let size_started = std::time::Instant::now();
        #[cfg(not(target_os = "windows"))]
        let _ = overlay_window.set_size(tauri::Size::Logical(tauri::LogicalSize { width, height }));
        #[cfg(target_os = "windows")]
        WINDOWS_OVERLAY_MODE.store(
            match state {
                "streaming" => 1,
                "error" => 2,
                _ => 0,
            },
            Ordering::Relaxed,
        );
        #[cfg(target_os = "windows")]
        if state == "error" {
            WINDOWS_OVERLAY_ERROR_HEIGHT.store(
                error_height.unwrap_or(OVERLAY_ERROR_HEIGHT).round() as u32,
                Ordering::Relaxed,
            );
        }
        let size_elapsed = size_started.elapsed();

        let pos_started = std::time::Instant::now();
        #[cfg(not(target_os = "windows"))]
        let set_pos_elapsed =
            if let Some((x, y)) = calculate_overlay_position(app_handle, width, height) {
                let set_pos_started = std::time::Instant::now();
                let _ = overlay_window
                    .set_position(tauri::Position::Logical(tauri::LogicalPosition { x, y }));
                set_pos_started.elapsed()
            } else {
                std::time::Duration::ZERO
            };
        #[cfg(target_os = "windows")]
        let set_pos_elapsed = {
            let set_pos_started = std::time::Instant::now();
            if let Err(error) = place_windows_overlay(app_handle, &overlay_window, width, height) {
                log::error!("Failed to place recording overlay: {error}");
            }
            set_pos_started.elapsed()
        };
        let pos_calc_elapsed = pos_started.elapsed() - set_pos_elapsed;

        // On Linux/dwm, force X11 properties BEFORE show() so the window manager
        // places the overlay on ALL desktops and above everything when it maps.
        // We must realize the GTK window first to get the XID, then set properties,
        // then show() — properties set after mapping won't move the window on dwm.
        #[cfg(target_os = "linux")]
        let pre_show_xid: Option<u64> = {
            log::info!("overlay: attempting pre-show X11 setup");
            match overlay_window.gtk_window() {
                Ok(gtk_win) => {
                    // Force GDK window creation so we can get the XID
                    gtk_win.realize();
                    match gtk_win.window() {
                        Some(gdk_win) => {
                            use gdkx11::X11Window;
                            match gdk_win.downcast_ref::<X11Window>() {
                                Some(x11) => {
                                    let xid = x11.xid();
                                    log::info!(
                                        "overlay: got XID {} before show(), setting properties",
                                        xid
                                    );
                                    // Set always-on-top + all-desktops BEFORE window is mapped
                                    force_x11_above_all_desktops(xid);
                                    // Tell all tiling WMs to never focus this window
                                    force_x11_no_focus(xid);
                                    Some(xid)
                                }
                                None => {
                                    log::warn!("overlay: failed to downcast to X11Window");
                                    None
                                }
                            }
                        }
                        None => {
                            log::warn!("overlay: GDK window not available after realize()");
                            None
                        }
                    }
                }
                Err(e) => {
                    log::warn!("overlay: failed to get GTK window: {}", e);
                    None
                }
            }
        };

        let show_started = std::time::Instant::now();
        let _ = overlay_window.show();
        let show_elapsed = show_started.elapsed();

        // Set _NET_WM_WINDOW_TYPE to [NOTIFICATION, DIALOG] AFTER show().
        // NOTIFICATION prevents focus steal on i3/bspwm; DIALOG triggers dwm's
        // propertynotify → updatewindowtype → isfloating=1.
        #[cfg(target_os = "linux")]
        if let Some(xid) = pre_show_xid {
            unsafe {
                extern "C" {
                    fn XOpenDisplay(name: *const i8) -> *mut std::ffi::c_void;
                    fn XInternAtom(
                        dpy: *mut std::ffi::c_void,
                        name: *const i8,
                        only_if_exists: i32,
                    ) -> u64;
                    fn XChangeProperty(
                        dpy: *mut std::ffi::c_void,
                        win: u64,
                        property: u64,
                        type_: u64,
                        format: i32,
                        mode: i32,
                        data: *const u8,
                        nelements: i32,
                    ) -> i32;
                    fn XMapRaised(dpy: *mut std::ffi::c_void, win: u64) -> i32;
                    fn XFlush(dpy: *mut std::ffi::c_void) -> i32;
                    fn XCloseDisplay(dpy: *mut std::ffi::c_void) -> i32;
                }
                let dpy = XOpenDisplay(std::ptr::null());
                if !dpy.is_null() {
                    let wm_type =
                        XInternAtom(dpy, b"_NET_WM_WINDOW_TYPE\0".as_ptr() as *const i8, 0);
                    let notification = XInternAtom(
                        dpy,
                        b"_NET_WM_WINDOW_TYPE_NOTIFICATION\0".as_ptr() as *const i8,
                        0,
                    );
                    let dialog = XInternAtom(
                        dpy,
                        b"_NET_WM_WINDOW_TYPE_DIALOG\0".as_ptr() as *const i8,
                        0,
                    );
                    let types = [notification, dialog];
                    XChangeProperty(
                        dpy,
                        xid,
                        wm_type,
                        4u64, // XA_ATOM
                        32,
                        0, // PropModeReplace
                        types.as_ptr() as *const u8,
                        2,
                    );
                    // Re-assert _NET_WM_STATE_ABOVE after show() — GTK clears it on map
                    let above = XInternAtom(dpy, b"_NET_WM_STATE_ABOVE\0".as_ptr() as *const i8, 0);
                    let state_atom = XInternAtom(dpy, b"_NET_WM_STATE\0".as_ptr() as *const i8, 0);
                    XChangeProperty(
                        dpy,
                        xid,
                        state_atom,
                        4u64, // XA_ATOM
                        32,
                        0, // PropModeReplace
                        &above as *const u64 as *const u8,
                        1,
                    );
                    // Re-assert _NET_WM_DESKTOP = 0xFFFFFFFF (all workspaces)
                    let desktop = XInternAtom(dpy, b"_NET_WM_DESKTOP\0".as_ptr() as *const i8, 0);
                    let all_desktops: u32 = 0xFFFFFFFF;
                    XChangeProperty(
                        dpy,
                        xid,
                        desktop,
                        6u64, // XA_CARDINAL
                        32,
                        0, // PropModeReplace
                        &all_desktops as *const u32 as *const u8,
                        1,
                    );
                    // Raise the window
                    XMapRaised(dpy, xid);
                    XFlush(dpy);
                    XCloseDisplay(dpy);
                }
            }
            // Re-apply WM_HINTS input=False after show() — GTK may overwrite
            // it when mapping the window.
            force_x11_no_focus(xid);
        }

        // X11 Shape mask DISABLED on Linux without compositor. The Shape
        // block was removed entirely; the RGBA-visual transparency path below
        // handles transparency on Hyprland.

        // On Windows, aggressively re-assert "topmost" in the native Z-order after showing
        #[cfg(target_os = "windows")]
        force_overlay_topmost(&overlay_window);

        // Re-assert bounds after show(): the pre-show move crosses the DPI
        // boundary, and tao's WM_DPICHANGED reflow clobbers the first placement.
        #[cfg(target_os = "windows")]
        if let Err(error) = place_windows_overlay(app_handle, &overlay_window, width, height) {
            log::error!("Failed to re-assert recording overlay position: {error}");
        }

        let _ = overlay_window.emit("show-overlay", state);
        log::debug!(
            "overlay '{}': set_size={:?} pos_calc={:?} set_pos={:?} show={:?}",
            state,
            size_elapsed,
            pos_calc_elapsed,
            set_pos_elapsed,
            show_elapsed
        );
    }
}

/// Shows the recording overlay window with fade-in animation
pub fn show_recording_overlay(app_handle: &AppHandle) {
    show_overlay_state(app_handle, "recording", None);
}

/// Shows the larger streaming overlay that displays live transcription text
pub fn show_streaming_overlay(app_handle: &AppHandle) {
    show_overlay_state(app_handle, "streaming", None);
}

/// Shows the transcribing overlay window
pub fn show_transcribing_overlay(app_handle: &AppHandle) {
    show_overlay_state(app_handle, "transcribing", None);
}

/// Shows the processing overlay window
pub fn show_processing_overlay(app_handle: &AppHandle) {
    show_overlay_state(app_handle, "processing", None);
}

/// Updates the overlay window position based on current settings
pub fn update_overlay_position(app_handle: &AppHandle) {
    // Positioning queries monitors/cursor (GDK/Xlib on Linux) and moves the
    // window, so it must run on the main thread — see show_overlay_state.
    let handle = app_handle.clone();
    let _ = app_handle.run_on_main_thread(move || update_overlay_position_on_main(&handle));
}

fn update_overlay_position_on_main(app_handle: &AppHandle) {
    if let Some(overlay_window) = app_handle.get_webview_window("recording_overlay") {
        #[cfg(target_os = "linux")]
        {
            update_gtk_layer_shell_anchors(&overlay_window);
        }

        #[cfg(target_os = "windows")]
        {
            let state = match WINDOWS_OVERLAY_MODE.load(Ordering::Relaxed) {
                1 => "streaming",
                2 => "error",
                _ => "recording",
            };
            // Error keeps the message-derived height applied at show time.
            let error_height = if WINDOWS_OVERLAY_MODE.load(Ordering::Relaxed) == 2 {
                Some(WINDOWS_OVERLAY_ERROR_HEIGHT.load(Ordering::Relaxed) as f64)
            } else {
                None
            };
            let (width, height) = overlay_dimensions(state, error_height);
            if let Err(error) = place_windows_overlay(app_handle, &overlay_window, width, height) {
                log::error!("Failed to update recording overlay position: {error}");
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            // Use the window's current size so centering stays correct whether the
            // overlay is in compact or streaming layout.
            let (width, height) = current_overlay_logical_size(&overlay_window)
                .unwrap_or((OVERLAY_WIDTH, OVERLAY_HEIGHT));
            if let Some((x, y)) = calculate_overlay_position(app_handle, width, height) {
                let _ = overlay_window
                    .set_position(tauri::Position::Logical(tauri::LogicalPosition { x, y }));
            }
        }
    }
}

/// Hides the recording overlay window with fade-out animation
pub fn hide_recording_overlay(app_handle: &AppHandle) {
    // Always hide the overlay regardless of settings - if setting was changed while recording,
    // we still want to hide it properly
    if let Some(overlay_window) = app_handle.get_webview_window("recording_overlay") {
        // Emit event to trigger fade-out animation
        let _ = overlay_window.emit("hide-overlay", ());
        // Hide the window after a short delay to allow animation to complete
        let window_clone = overlay_window.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            let _ = window_clone.hide();
        });
    }
}

// Cached "overlay is enabled" flag, kept in sync with overlay_style. Avoids
// reading the Tauri store on every audio callback (~24 Hz during recording).
// Defaults to false so the audio path doesn't emit until lib.rs::setup
// populates the cache from initial settings.
static OVERLAY_ENABLED: AtomicBool = AtomicBool::new(false);

/// Update the cached overlay-enabled flag. Called from `lib.rs` at
/// startup after settings load, and from `change_overlay_style_setting`
/// whenever the user changes whether the overlay is shown.
pub fn update_overlay_enabled_cache(enabled: bool) {
    OVERLAY_ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn emit_levels(app_handle: &AppHandle, levels: &[f32]) {
    if !OVERLAY_ENABLED.load(Ordering::Relaxed) {
        return;
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let last = LAST_MIC_LEVEL_EMIT.load(Ordering::Relaxed);
    if now.saturating_sub(last) < EMIT_THROTTLE_MS {
        return;
    }
    LAST_MIC_LEVEL_EMIT.store(now, Ordering::Relaxed);

    // Marshal the emit to the main thread so WebKitGTK's event loop picks it up.
    // emit from the audio worker thread reaches the Rust EventManager but
    // doesn't wake the WebKitGTK webview's JS context.
    let handle = app_handle.clone();
    let payload = levels.to_vec();
    let _ = app_handle.run_on_main_thread(move || {
        let _ = handle.emit("mic-level", payload);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Height actually used by the error window (the call-site clamp).
    fn clamped_error_height(text: &str) -> f64 {
        estimate_error_height(text).max(80.0)
    }

    #[test]
    fn estimate_error_height_floors_short_messages_at_80() {
        assert!(clamped_error_height("") >= 80.0);
        let short = "Transcription failed — service unreachable";
        assert_eq!(short.chars().count(), 42);
        let h = clamped_error_height(short);
        assert!((80.0..104.0).contains(&h), "got {}", h);
    }

    #[test]
    fn estimate_error_height_grows_for_long_messages() {
        // ENIGO_UNAVAILABLE-style paste failure (~250 chars → 6 lines).
        let long: String = "x".repeat(250);
        assert!(clamped_error_height(&long) >= 104.0);
        // 200 chars → 5 lines → 89px, above the floor.
        let medium: String = "x".repeat(200);
        assert!((89.0..104.0).contains(&clamped_error_height(&medium)));
    }

    #[test]
    fn estimate_error_height_is_monotonic_in_length() {
        let lengths = [0usize, 10, 44, 45, 100, 200, 260, 400];
        let mut prev = 0.0;
        for &len in &lengths {
            let h = clamped_error_height(&"x".repeat(len));
            assert!(h >= prev, "non-monotonic at len {}: {} < {}", len, h, prev);
            prev = h;
        }
    }

    #[test]
    fn summarize_error_reports_accessibility_permission_before_unreachable() {
        let paste_permission = "Accessibility permission is missing, so voxix could not paste. The transcription was copied to your clipboard — grant voxix permission in System Settings → Privacy & Security → Accessibility, or press Cmd+V to paste it manually.";
        assert_eq!(
            summarize_error(paste_permission),
            "Transcription copied — grant Accessibility to paste"
        );

        // Any permission/accessibility mention routes to the paste bucket,
        // even without the full message above.
        assert_eq!(
            summarize_error("Enigo: permission denied while trying to simulate input"),
            "Transcription copied — grant Accessibility to paste"
        );
        assert_eq!(
            summarize_error("enigo: ACCESSIBILITY not granted"),
            "Transcription copied — grant Accessibility to paste"
        );
    }

    #[test]
    fn summarize_error_reports_microphone_permission_with_settings_path() {
        let message = "Microphone access needed — enable voxix in System Settings → Privacy & Security → Microphone";
        assert_eq!(summarize_error(message), message);

        // Any microphone mention routes to the microphone bucket before the
        // generic permission bucket can catch it.
        assert_eq!(
            summarize_error("microphone permission denied"),
            "Microphone access needed — enable voxix in System Settings → Privacy & Security → Microphone"
        );
    }

    #[test]
    fn summarize_error_reports_connection_failures_as_unreachable() {
        assert_eq!(
            summarize_error("daemon: connection refused (os error 61)"),
            "Transcription failed — service unreachable"
        );
        assert_eq!(
            summarize_error("server not running on port 47832"),
            "Transcription failed — service unreachable"
        );
        assert_eq!(
            summarize_error("websocket connect: i/o error on stream"),
            "Transcription failed — service unreachable"
        );
        // Bare "io" as a whole word still matches.
        assert_eq!(
            summarize_error("io: broken pipe while reading"),
            "Transcription failed — service unreachable"
        );
    }

    #[test]
    fn summarize_error_reports_engine_busy() {
        assert_eq!(
            summarize_error("transcription already in flight, ignoring request"),
            "Transcription failed — engine busy, try again"
        );
        assert_eq!(
            summarize_error("engine busy with another recording"),
            "Transcription failed — engine busy, try again"
        );
    }

    #[test]
    fn summarize_error_truncates_unknown_errors_and_flattens_newlines() {
        assert_eq!(
            summarize_error("mystery failure:\nline two\u{7}end"),
            "mystery failure: line two end"
        );

        let long = "x".repeat(200);
        let out = summarize_error(&long);
        assert_eq!(out.len(), 80);
        assert!(out.chars().all(|c| c == 'x'));
    }

    #[test]
    fn summarize_error_does_not_match_io_inside_words() {
        // "audio"/"position"/"motion"/"permission" all contain "io" as a
        // substring; none of them may hit the service-unreachable bucket.
        assert_ne!(
            summarize_error(
                "failed to configure audio input: position out of range for motion event"
            ),
            "Transcription failed — service unreachable"
        );
        assert_eq!(
            summarize_error(
                "failed to configure audio input: position out of range for motion event"
            ),
            "failed to configure audio input: position out of range for motion event"
        );
    }

    #[test]
    fn contains_word_matches_whole_words_only() {
        assert!(contains_word("io broken pipe", "io"));
        assert!(contains_word("error (io)", "io"));
        assert!(!contains_word("permission denied", "io"));
        assert!(!contains_word("audio input", "io"));
        assert!(!contains_word("libio", "io"));
        assert!(!contains_word("os error", "io"));
        assert!(!contains_word("", "io"));
    }

    #[test]
    fn monitor_hit_test_uses_half_open_physical_bounds() {
        let position = PhysicalPosition::new(-2560, -200);
        let size = PhysicalSize::new(2560, 1440);

        assert!(is_mouse_within_monitor((-2560, -200), &position, &size));
        assert!(is_mouse_within_monitor((-1, 1239), &position, &size));
        assert!(!is_mouse_within_monitor((0, 0), &position, &size));
        assert!(!is_mouse_within_monitor((-1, 1240), &position, &size));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_cursor_hit_test_does_not_scale_physical_monitor_bounds() {
        let position = PhysicalPosition::new(1920, 0);
        let size = PhysicalSize::new(3840, 2160);
        let cursor = (5000, 1000);

        assert!(is_mouse_within_monitor(cursor, &position, &size));

        // This is the old mixed-coordinate comparison. It excludes a cursor
        // that is visibly inside a secondary display running at 150%.
        let scale = 1.5;
        let logical_position = PhysicalPosition::new(
            (position.x as f64 / scale) as i32,
            (position.y as f64 / scale) as i32,
        );
        let logical_size = PhysicalSize::new(
            (size.width as f64 / scale) as u32,
            (size.height as f64 / scale) as u32,
        );
        assert!(!is_mouse_within_monitor(
            cursor,
            &logical_position,
            &logical_size
        ));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_overlay_bounds_use_destination_monitor_scale() {
        let monitor_position = PhysicalPosition::new(1920, 0);
        let monitor_size = PhysicalSize::new(3840, 2160);

        assert_eq!(
            windows_overlay_bounds(
                monitor_position,
                monitor_size,
                1.5,
                OVERLAY_WIDTH,
                OVERLAY_HEIGHT,
                OverlayPosition::Bottom,
            ),
            (3648, 2031, 384, 69)
        );
        assert_eq!(
            windows_overlay_bounds(
                monitor_position,
                monitor_size,
                1.5,
                OVERLAY_WIDTH,
                OVERLAY_HEIGHT,
                OverlayPosition::Top,
            ),
            (3648, 6, 384, 69)
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_overlay_bounds_support_negative_monitor_origins() {
        assert_eq!(
            windows_overlay_bounds(
                PhysicalPosition::new(-2560, -200),
                PhysicalSize::new(2560, 1440),
                1.25,
                OVERLAY_STREAM_WIDTH,
                OVERLAY_STREAM_HEIGHT,
                OverlayPosition::Bottom,
            ),
            (-1530, 1040, 500, 150)
        );
    }
}

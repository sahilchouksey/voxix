use crate::input::{self, EnigoState};
#[cfg(target_os = "linux")]
use crate::settings::TypingTool;
use crate::settings::{get_settings, AutoSubmitKey, ClipboardHandling, PasteMethod};
use enigo::{Direction, Enigo, Key, Keyboard};
use log::info;
use std::process::Command;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tauri_plugin_clipboard_manager::ClipboardExt;

#[cfg(target_os = "linux")]
use crate::utils::{is_gnome_wayland, is_kde_plasma, is_kde_wayland, is_wayland};

/// Neither KWin nor Mutter implement zwp_virtual_keyboard_manager_v1, so
/// wtype can only work on wlroots-style compositors.
#[cfg(target_os = "linux")]
fn is_wtype_usable() -> bool {
    crate::utils::virtual_keyboard_protocol_available() && is_wtype_available()
}

/// Locks the managed Enigo instance on demand. Callers that have a
/// native-tool path should only do this as a fallback, so pastes keep working
/// when `initializeEnigo` failed (e.g. Wayland without X). The `State` guard
/// must be held by the caller for the lock's lifetime.
fn lock_enigo(state: &EnigoState) -> Result<std::sync::MutexGuard<'_, Enigo>, String> {
    state
        .0
        .lock()
        .map_err(|e| format!("Failed to lock Enigo: {e}"))
}

/// Error returned when no input-simulation backend is available. Callers use
/// this to keep the transcription on the clipboard instead of restoring over it.
pub(crate) const ENIGO_UNAVAILABLE: &str = "Enigo is not initialized";

/// Makes sure the managed Enigo state exists. The frontend initializes Enigo
/// once at mount; if that failed (e.g. the Accessibility permission had not been
/// granted yet) a later grant would otherwise only take effect after an app
/// restart. Retrying here makes the next paste succeed as soon as the user has
/// granted permission.
fn ensure_enigo(app_handle: &AppHandle) {
    if app_handle.try_state::<EnigoState>().is_some() {
        return;
    }
    if let Err(e) = crate::commands::initialize_enigo(app_handle.clone()) {
        log::warn!("Enigo still unavailable at paste time: {e}");
    }
}

/// Whether the previous clipboard contents should be restored after a paste
/// chord attempt. A missing input backend must keep the transcription on the
/// clipboard; every other outcome preserves the historical restore behavior.
fn should_restore_clipboard(chord_result: &Result<(), String>) -> bool {
    !matches!(chord_result, Err(e) if e == ENIGO_UNAVAILABLE)
}

/// Pastes text using the clipboard: saves current content, writes text, sends paste keystroke, restores clipboard.
fn paste_via_clipboard(
    text: &str,
    app_handle: &AppHandle,
    paste_method: &PasteMethod,
    paste_delay_ms: u64,
    paste_delay_after_ms: u64,
) -> Result<(), String> {
    let clipboard = app_handle.clipboard();
    let saved_text = clipboard.read_text().ok().filter(|t| !t.is_empty());
    // Only probe for an image when there is no text to restore. Text is by far the
    // common case, and reading an image decodes the full bitmap, so this keeps the
    // text path exactly as cheap as it was before.
    let saved_image = if saved_text.is_none() {
        clipboard.read_image().ok().map(|image| image.to_owned())
    } else {
        None
    };

    // Linux-only pre-clear: arboard is Wayland-backed here, so without this
    // (plus the X11-direct write below) an XWayland target would keep seeing
    // a stale image and paste it instead of the transcript. macOS/Windows
    // keep the exact upstream behavior (write replaces the selection there).
    #[cfg(target_os = "linux")]
    {
        // Clear any prior image so the target app doesn't prefer image/png over text/plain
        let _ = clipboard.clear();
        if is_wayland() {
            // Stdio::null + status(): wl-copy forks a selection daemon that
            // inherits piped fds, so .output() would hang (same as the write path).
            let _ = std::process::Command::new("wl-copy")
                .arg("--clear")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    // Write the text to the clipboard. arboard uses the Wayland backend here
    // (wayland-data-control), so XWayland targets would only see the text via
    // the compositor's bridge — which has proven unreliable at paste time and
    // left stale images (screenshots) on the X11 clipboard, pasting an image
    // instead of the transcript. Write the X11 clipboard DIRECTLY too, then
    // verify it before sending the chord. Each write fully replaces its
    // selection, which also clears stale images.
    #[cfg(target_os = "linux")]
    let write_result = {
        let result = clipboard
            .write_text(text)
            .map_err(|e| format!("Failed to write to clipboard: {}", e));
        if is_wayland() {
            if is_wl_copy_available() {
                if let Err(e) = write_clipboard_via_wl_copy(text) {
                    log::warn!("wl-copy write failed: {e}");
                }
            }
            // Direct X11 write: this is what XWayland targets (WezTerm,
            // Electron, browsers-in-X11-mode) actually read on Ctrl+V.
            if let Err(e) = write_x11_clipboard_text(text) {
                log::warn!("X11 clipboard write failed: {e}");
            }
        }
        result
    };

    #[cfg(not(target_os = "linux"))]
    let write_result = clipboard
        .write_text(text)
        .map_err(|e| format!("Failed to write to clipboard: {}", e));

    write_result?;

    #[cfg(target_os = "linux")]
    if is_wayland() {
        let deadline = std::time::Instant::now() + Duration::from_millis(300);
        while std::time::Instant::now() < deadline {
            if let Ok(cur) = clipboard.read_text() {
                if cur == text {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // The chord must only fire once the X11 clipboard actually holds the
        // transcript — otherwise an XWayland target pastes the stale image.
        // Do not fail the paste if X11 tools are missing (native Wayland
        // targets read the Wayland clipboard, already written above).
        if is_x11_clipboard_tool_available() {
            let deadline = std::time::Instant::now() + Duration::from_millis(500);
            loop {
                if read_x11_clipboard_text().as_deref() == Some(text) {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    log::warn!("X11 clipboard did not converge to pasted text in time");
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    // The pre-chord hold floor is Linux-only: some X11/Wayland targets poll
    // global modifier state when handling the key, so a sub-100ms setting
    // risks a dropped chord. macOS/Windows keep the user's exact setting.
    #[cfg(target_os = "linux")]
    std::thread::sleep(Duration::from_millis(paste_delay_ms.max(100)));
    #[cfg(not(target_os = "linux"))]
    std::thread::sleep(Duration::from_millis(paste_delay_ms));

    // Send paste key combo. The chord result is captured, NOT propagated with
    // `?`: the clipboard MUST be restored below either way, otherwise a
    // failed chord leaves the transcript behind (and a stale-image restore
    // undone). The error is returned after restore.
    let chord_result = send_paste_chord(app_handle, paste_method);

    // If no input backend exists, do NOT restore: the transcription is still on
    // the clipboard and must stay there so the user can paste it manually.
    // Every other chord outcome restores the previous clipboard as before.
    if !should_restore_clipboard(&chord_result) {
        log::warn!("Paste chord unavailable; keeping transcription on clipboard");
        return chord_result;
    }

    #[cfg(target_os = "linux")]
    let after_delay = if is_wayland() {
        paste_delay_after_ms.max(500)
    } else {
        // X11 targets read synchronously from our holder, but a small floor
        // still guards against restore winning a scheduling race with the
        // target's read.
        paste_delay_after_ms.max(200)
    };
    #[cfg(not(target_os = "linux"))]
    let after_delay = paste_delay_after_ms;
    std::thread::sleep(Duration::from_millis(after_delay));

    // Restore original clipboard content.
    // Text takes priority so this path stays identical to the previous behavior;
    // an image is only restored when the clipboard held no text at all, which is
    // the case that used to silently wipe screenshots.
    if let Some(clipboard_content) = saved_text {
        // Restore to both clipboards (see write path above)
        let _ = clipboard.write_text(&clipboard_content);
        #[cfg(target_os = "linux")]
        if is_wayland() {
            if is_wl_copy_available() {
                let _ = write_clipboard_via_wl_copy(&clipboard_content);
            }
            let _ = write_x11_clipboard_text(&clipboard_content);
        }
    } else if let Some(image) = saved_image {
        info!("Restoring image to clipboard");
        let _ = clipboard.write_image(&image);
        // No X11 image restore: without a PNG encoder dependency we cannot
        // feed image/png to xclip, and the pre-existing behavior never wrote
        // X11 either. The X11 side keeps the transcript text, which is the
        // desired paste content anyway.
    } else {
        // Nothing was there to begin with — don't leave the transcription behind.
        let _ = clipboard.clear();
        #[cfg(target_os = "linux")]
        if is_wayland() {
            // Leave no transcript on the X11 side either: an XWayland target
            // reading it on its next manual Ctrl+V would otherwise paste our
            // transcript. An empty write keeps a live (empty) selection; the
            // holder exits on the next ownership change, so nothing leaks.
            let _ = write_x11_clipboard_text("");
        }
    }

    // Report the chord outcome only after restore has run.
    chord_result
}

/// Sends the paste chord: native Linux tools first, enigo as fallback.
/// Separated from `paste_via_clipboard` so a chord failure can never skip
/// the clipboard restore (callers must propagate the result AFTER restoring).
fn send_paste_chord(app_handle: &AppHandle, paste_method: &PasteMethod) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    let key_combo_sent = try_send_key_combo_linux(paste_method)?;

    #[cfg(not(target_os = "linux"))]
    let key_combo_sent = false;

    // Fall back to enigo if no native tool handled it
    if !key_combo_sent {
        ensure_enigo(app_handle);
        let enigo_state = app_handle
            .try_state::<EnigoState>()
            .ok_or_else(|| ENIGO_UNAVAILABLE.to_string())?;
        let mut enigo = lock_enigo(&enigo_state)?;
        match paste_method {
            // The legacy path cannot detect a mistimed chord, so it keeps the
            // conservative 100ms modifier hold.
            PasteMethod::CtrlV => input::send_paste_ctrl_v(&mut enigo, 100)?,
            PasteMethod::CtrlShiftV => input::send_paste_ctrl_shift_v(&mut enigo, 100)?,
            PasteMethod::ShiftInsert => input::send_paste_shift_insert(&mut enigo, 100)?,
            _ => return Err("Invalid paste method for clipboard paste".into()),
        }
    }

    Ok(())
}

/// Attempts to send a key combination using Linux-native tools.
/// Returns `Ok(true)` if a native tool handled it, `Ok(false)` to fall back to enigo.
#[cfg(target_os = "linux")]
fn try_send_key_combo_linux(paste_method: &PasteMethod) -> Result<bool, String> {
    if is_wayland() {
        // Wayland: prefer wtype on compositors that implement the virtual
        // keyboard protocol (wlroots etc. — NOT KDE/GNOME), then dotool,
        // then ydotool. kwtype is intentionally absent: it can only type
        // text, not send modifier chords, so KDE chords use dotool/ydotool.
        if is_wtype_usable() {
            info!("Using wtype for key combo");
            send_key_combo_via_wtype(paste_method)?;
            return Ok(true);
        }
        if is_dotool_available() {
            info!("Using dotool for key combo");
            send_key_combo_via_dotool(paste_method)?;
            return Ok(true);
        }
        if is_ydotool_available() {
            info!("Using ydotool for key combo");
            send_key_combo_via_ydotool(paste_method)?;
            return Ok(true);
        }
        if is_kde_wayland() || is_gnome_wayland() {
            // The enigo fallback keeps its X11 backend on KDE/GNOME (Wayland
            // backend is dead there), so XWayland targets can still paste —
            // only native Wayland apps are unreachable without dotool/ydotool.
            log::warn!(
                "No paste-chord tool (dotool/ydotool) on KDE/GNOME Wayland; native Wayland targets cannot receive the paste chord (XWayland targets still can via enigo). Install dotool (daemon + input group) or ydotool (ydotoold daemon), or switch Paste method to Direct{}",
                if is_kde_wayland() {
                    " with typing tool kwtype"
                } else {
                    ""
                }
            );
        }
    } else {
        // X11: prefer xdotool, then ydotool
        if is_xdotool_available() {
            info!("Using xdotool for key combo");
            send_key_combo_via_xdotool(paste_method)?;
            return Ok(true);
        }
        if is_ydotool_available() {
            info!("Using ydotool for key combo");
            send_key_combo_via_ydotool(paste_method)?;
            return Ok(true);
        }
    }

    Ok(false)
}

/// Attempts to type text directly using Linux-native tools.
/// Returns `Ok(true)` if a native tool handled it, `Ok(false)` to fall back to enigo.
#[cfg(target_os = "linux")]
fn try_direct_typing_linux(text: &str, preferred_tool: TypingTool) -> Result<bool, String> {
    // If user specified a tool, try only that one
    if preferred_tool != TypingTool::Auto {
        return match preferred_tool {
            TypingTool::Wtype if is_wtype_available() => {
                // wtype cannot inject on KDE/GNOME (no virtual-keyboard
                // protocol) — fail loudly instead of silently typing nothing.
                if !crate::utils::virtual_keyboard_protocol_available() {
                    return Err("wtype is not supported on KDE/GNOME Wayland (compositor lacks the virtual-keyboard protocol); use kwtype (KDE) or dotool/ydotool".into());
                }
                info!("Using user-specified wtype");
                type_text_via_wtype(text)?;
                Ok(true)
            }
            TypingTool::Kwtype if is_kwtype_available() => {
                // kwtype speaks the KWin-only fake-input protocol — anywhere
                // else it cannot inject. Fail loudly instead of typing nothing.
                if !is_kde_plasma() {
                    return Err("kwtype only works on KDE Plasma (KWin fake-input protocol); use wtype (wlroots), or dotool/ydotool".into());
                }
                info!("Using user-specified kwtype");
                type_text_via_kwtype(text)?;
                Ok(true)
            }
            TypingTool::Dotool if is_dotool_available() => {
                info!("Using user-specified dotool");
                type_text_via_dotool(text)?;
                Ok(true)
            }
            TypingTool::Ydotool if is_ydotool_available() => {
                info!("Using user-specified ydotool");
                type_text_via_ydotool(text)?;
                Ok(true)
            }
            TypingTool::Xdotool if is_xdotool_available() => {
                // xdotool drives X11 via XTEST — on Wayland it can at most
                // reach XWayland clients, never native ones.
                if is_wayland() {
                    return Err("xdotool only works on X11 sessions (it cannot inject into native Wayland apps); use wtype, dotool/ydotool, or kwtype (KDE)".into());
                }
                info!("Using user-specified xdotool");
                type_text_via_xdotool(text)?;
                Ok(true)
            }
            _ => Err(format!(
                "Typing tool {:?} is not available on this system",
                preferred_tool
            )),
        };
    }

    // Auto mode - existing fallback chain
    if is_wayland() {
        // KDE Wayland: prefer kwtype (uses KDE Fake Input protocol, supports umlauts)
        if is_kde_wayland() && is_kwtype_available() {
            info!("Using kwtype for direct text input on KDE Wayland");
            type_text_via_kwtype(text)?;
            return Ok(true);
        }
        // Wayland: prefer wtype on compositors with virtual-keyboard support,
        // then dotool, then ydotool. wtype works on neither KDE nor GNOME.
        if is_wtype_usable() {
            info!("Using wtype for direct text input");
            type_text_via_wtype(text)?;
            return Ok(true);
        }
        if is_dotool_available() {
            info!("Using dotool for direct text input");
            type_text_via_dotool(text)?;
            return Ok(true);
        }
        if is_ydotool_available() {
            info!("Using ydotool for direct text input");
            type_text_via_ydotool(text)?;
            return Ok(true);
        }
    } else {
        // X11: prefer xdotool, then ydotool
        if is_xdotool_available() {
            info!("Using xdotool for direct text input");
            type_text_via_xdotool(text)?;
            return Ok(true);
        }
        if is_ydotool_available() {
            info!("Using ydotool for direct text input");
            type_text_via_ydotool(text)?;
            return Ok(true);
        }
    }

    Ok(false)
}

/// Check if wtype is available (Wayland text input tool)
#[cfg(target_os = "linux")]
fn is_wtype_available() -> bool {
    Command::new("which")
        .arg("wtype")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Check if dotool is available (another Wayland text input tool)
#[cfg(target_os = "linux")]
fn is_dotool_available() -> bool {
    Command::new("which")
        .arg("dotool")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Check if ydotool is available (uinput-based, works on both Wayland and X11)
#[cfg(target_os = "linux")]
fn is_ydotool_available() -> bool {
    Command::new("which")
        .arg("ydotool")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn is_xdotool_available() -> bool {
    Command::new("which")
        .arg("xdotool")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Check if kwtype is available (KDE Wayland virtual keyboard input tool)
#[cfg(target_os = "linux")]
fn is_kwtype_available() -> bool {
    Command::new("which")
        .arg("kwtype")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Check if wl-copy is available (Wayland clipboard tool)
#[cfg(target_os = "linux")]
fn is_wl_copy_available() -> bool {
    Command::new("which")
        .arg("wl-copy")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Type text directly via wtype on Wayland.
#[cfg(target_os = "linux")]
fn type_text_via_wtype(text: &str) -> Result<(), String> {
    let output = Command::new("wtype")
        .arg("--") // Protect against text starting with -
        .arg(text)
        .output()
        .map_err(|e| format!("Failed to execute wtype: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("wtype failed: {}", stderr));
    }

    Ok(())
}

/// Type text directly via xdotool on X11.
#[cfg(target_os = "linux")]
fn type_text_via_xdotool(text: &str) -> Result<(), String> {
    let output = Command::new("xdotool")
        .arg("type")
        .arg("--clearmodifiers")
        .arg("--")
        .arg(text)
        .output()
        .map_err(|e| format!("Failed to execute xdotool: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("xdotool failed: {}", stderr));
    }

    Ok(())
}

/// Type text directly via dotool (works on both Wayland and X11 via uinput).
#[cfg(target_os = "linux")]
fn type_text_via_dotool(text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::Stdio;

    // dotool opens /dev/uinput itself on every invocation. A bare `which`
    // hit says nothing about delivery — fail loudly with setup guidance
    // instead of returning Ok on a keystroke that went nowhere.
    check_dotool_ready()?;

    let mut child = Command::new("dotool")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn dotool: {}", e))?;

    if let Some(mut stdin) = child.stdin.take() {
        // dotool uses "type <text>" command
        writeln!(stdin, "type {}", text)
            .map_err(|e| format!("Failed to write to dotool stdin: {}", e))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("Failed to wait for dotool: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("dotool failed: {}", stderr));
    }

    Ok(())
}

/// Type text directly via ydotool (uinput-based, requires ydotoold daemon).
#[cfg(target_os = "linux")]
fn type_text_via_ydotool(text: &str) -> Result<(), String> {
    // Same silent-success hazard as the chord path: without a reachable
    // ydotoold the keystrokes go nowhere. Fail loudly instead.
    check_ydotool_ready()?;

    let output = Command::new("ydotool")
        .arg("type")
        .arg("--")
        .arg(text)
        .output()
        .map_err(|e| format!("Failed to execute ydotool: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ydotool failed: {}", stderr));
    }

    Ok(())
}

/// Type text directly via kwtype (KDE Wayland virtual keyboard, uses KDE Fake Input protocol).
#[cfg(target_os = "linux")]
fn type_text_via_kwtype(text: &str) -> Result<(), String> {
    let output = Command::new("kwtype")
        .arg("--")
        .arg(text)
        .output()
        .map_err(|e| format!("Failed to execute kwtype: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("kwtype failed: {}", stderr));
    }

    Ok(())
}

/// Write text to clipboard via wl-copy (Wayland clipboard tool).
/// Uses Stdio::null() to avoid blocking on repeated calls — wl-copy forks a
/// daemon that inherits piped fds, causing read_to_end to hang indefinitely.
#[cfg(target_os = "linux")]
fn write_clipboard_via_wl_copy(text: &str) -> Result<(), String> {
    use std::process::Stdio;
    let status = Command::new("wl-copy")
        .arg("--")
        .arg(text)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("Failed to execute wl-copy: {}", e))?;

    if !status.success() {
        return Err("wl-copy failed".into());
    }

    Ok(())
}

/// Check if a direct X11 clipboard tool is available (xclip preferred, xsel fallback).
#[cfg(target_os = "linux")]
fn is_x11_clipboard_tool_available() -> bool {
    is_xclip_available() || is_xsel_available()
}

#[cfg(target_os = "linux")]
fn is_xclip_available() -> bool {
    Command::new("which")
        .arg("xclip")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn is_xsel_available() -> bool {
    Command::new("which")
        .arg("xsel")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Write text directly to the X11 CLIPBOARD selection. Required because
/// arboard uses the Wayland backend on Wayland sessions, and the
/// compositor's Wayland→X11 bridge cannot be relied on at paste time:
/// XWayland targets would otherwise read a stale image (screenshot) and
/// paste it instead of the transcript. The holder process exits on its own
/// once another client takes ownership, so repeated pastes do not leak.
#[cfg(target_os = "linux")]
fn write_x11_clipboard_text(text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::Stdio;

    if is_xclip_available() {
        let mut child = Command::new("xclip")
            .arg("-selection")
            .arg("clipboard")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to spawn xclip: {}", e))?;
        child
            .stdin
            .take()
            .ok_or("Failed to open xclip stdin")?
            .write_all(text.as_bytes())
            .map_err(|e| format!("Failed to write to xclip stdin: {}", e))?;
        let status = child
            .wait()
            .map_err(|e| format!("Failed to wait for xclip: {}", e))?;
        return status
            .success()
            .then_some(())
            .ok_or_else(|| "xclip failed".to_string());
    }

    if is_xsel_available() {
        let mut child = Command::new("xsel")
            .arg("--clipboard")
            .arg("--input")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to spawn xsel: {}", e))?;
        child
            .stdin
            .take()
            .ok_or("Failed to open xsel stdin")?
            .write_all(text.as_bytes())
            .map_err(|e| format!("Failed to write to xsel stdin: {}", e))?;
        let status = child
            .wait()
            .map_err(|e| format!("Failed to wait for xsel: {}", e))?;
        return status
            .success()
            .then_some(())
            .ok_or_else(|| "xsel failed".to_string());
    }

    Err("neither xclip nor xsel is available".into())
}

/// Read the current X11 CLIPBOARD text, if any.
#[cfg(target_os = "linux")]
fn read_x11_clipboard_text() -> Option<String> {
    use std::process::Stdio;
    let output = if is_xclip_available() {
        Command::new("xclip")
            .arg("-selection")
            .arg("clipboard")
            .arg("-o")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()?
    } else if is_xsel_available() {
        Command::new("xsel")
            .arg("--clipboard")
            .arg("--output")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()?
    } else {
        return None;
    };
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// Send a key combination (e.g., Ctrl+V) via wtype on Wayland.
/// Modifiers are released explicitly (-m) so a stuck modifier cannot survive
/// an abnormal wtype exit.
#[cfg(target_os = "linux")]
fn send_key_combo_via_wtype(paste_method: &PasteMethod) -> Result<(), String> {
    let args: Vec<&str> = match paste_method {
        PasteMethod::CtrlV => vec!["-M", "ctrl", "-k", "v", "-m", "ctrl"],
        PasteMethod::ShiftInsert => vec!["-M", "shift", "-k", "Insert", "-m", "shift"],
        PasteMethod::CtrlShiftV => {
            vec![
                "-M", "ctrl", "-M", "shift", "-k", "v", "-m", "shift", "-m", "ctrl",
            ]
        }
        _ => return Err("Unsupported paste method".into()),
    };

    let output = Command::new("wtype")
        .args(&args)
        .output()
        .map_err(|e| format!("Failed to execute wtype: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("wtype failed: {}", stderr));
    }

    Ok(())
}

/// Probe whether dotool can actually inject: it opens /dev/uinput on every
/// invocation, so the device must exist and be writable (user in the `input`
/// group + re-login, or root, with the uinput module loaded). A bare `which
/// dotool` hit is NOT delivery readiness. Cached per process.
#[cfg(target_os = "linux")]
fn check_dotool_ready() -> Result<(), String> {
    use std::sync::OnceLock;
    static READY: OnceLock<bool> = OnceLock::new();
    let ready = *READY.get_or_init(|| {
        std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .is_ok()
    });
    if ready {
        Ok(())
    } else {
        Err("dotool cannot open /dev/uinput (needs the uinput module loaded and the user in the `input` group — re-login after `usermod -aG input $USER` — or root)".into())
    }
}

/// Probe whether the ydotool daemon is reachable: the `ydotool` client only
/// talks to a running `ydotoold` over its socket (default
/// /tmp/.ydotool_socket, overridable via YDOTOOL_SOCKET). Without the daemon
/// the client fails or, worse, exits 0 having delivered nothing. Cached per
/// process.
#[cfg(target_os = "linux")]
fn check_ydotool_ready() -> Result<(), String> {
    use std::sync::OnceLock;
    static READY: OnceLock<bool> = OnceLock::new();
    let ready = *READY.get_or_init(|| {
        let socket =
            std::env::var("YDOTOOL_SOCKET").unwrap_or_else(|_| "/tmp/.ydotool_socket".to_string());
        std::os::unix::net::UnixStream::connect(&socket).is_ok()
    });
    if ready {
        Ok(())
    } else {
        Err("ydotoold daemon is not reachable (start ydotoold with /dev/uinput access — e.g. as a systemd service for a user in the `input` group — and ensure YDOTOOL_SOCKET matches)".into())
    }
}

/// Send a key combination (e.g., Ctrl+V) via dotool.
#[cfg(target_os = "linux")]
fn send_key_combo_via_dotool(paste_method: &PasteMethod) -> Result<(), String> {
    check_dotool_ready()?;

    let command;
    match paste_method {
        PasteMethod::CtrlV => command = "echo key ctrl+v | dotool",
        PasteMethod::ShiftInsert => command = "echo key shift+insert | dotool",
        PasteMethod::CtrlShiftV => command = "echo key ctrl+shift+v | dotool",
        _ => return Err("Unsupported paste method".into()),
    }
    use std::process::Stdio;
    let status = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("Failed to execute dotool: {}", e))?;
    if !status.success() {
        return Err("dotool failed".into());
    }

    Ok(())
}

/// ydotool changed its `key` syntax between generations: ≤0.1.8 takes symbolic
/// chords (`key ctrl+v`), ≥1.0.4 takes raw keycodes (`key 29:1 47:1 47:0 29:0`).
/// Feeding raw codes to an old build types literal digits and exits 0 — a
/// silent corruption. Probe `ydotool key --help` once per process and cache it.
#[cfg(target_os = "linux")]
fn ydotool_uses_symbolic_key_syntax() -> bool {
    use std::sync::OnceLock;
    static SYMBOLIC: OnceLock<bool> = OnceLock::new();
    *SYMBOLIC.get_or_init(|| {
        Command::new("ydotool")
            .arg("key")
            .arg("--help")
            .output()
            .map(|output| {
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                )
                .to_lowercase();
                // Old (symbolic) builds document chord syntax with key names;
                // new builds document <keycode>:<state> pairs. Default to raw
                // (current generation) when the help text is inconclusive.
                (text.contains("ctrl") || text.contains("key name")) && !text.contains("keycode")
            })
            .unwrap_or(false)
    })
}

/// Send a key combination (e.g., Ctrl+V) via ydotool (requires ydotoold daemon).
#[cfg(target_os = "linux")]
fn send_key_combo_via_ydotool(paste_method: &PasteMethod) -> Result<(), String> {
    // Daemon check first: without ydotoold the client delivers nothing.
    check_ydotool_ready()?;

    let args: Vec<&str> = if ydotool_uses_symbolic_key_syntax() {
        // ydotool ≤0.1.8 symbolic syntax.
        match paste_method {
            PasteMethod::CtrlV => vec!["key", "ctrl+v"],
            PasteMethod::ShiftInsert => vec!["key", "shift+insert"],
            PasteMethod::CtrlShiftV => vec!["key", "ctrl+shift+v"],
            _ => return Err("Unsupported paste method".into()),
        }
    } else {
        // ydotool ≥1.0.4: Linux input event keycodes, <keycode>:<pressed>
        // where pressed is 1 for down, 0 for up. ctrl=29, shift=42, v=47, insert=110.
        match paste_method {
            PasteMethod::CtrlV => vec!["key", "29:1", "47:1", "47:0", "29:0"],
            PasteMethod::ShiftInsert => vec!["key", "42:1", "110:1", "110:0", "42:0"],
            PasteMethod::CtrlShiftV => {
                vec!["key", "29:1", "42:1", "47:1", "47:0", "42:0", "29:0"]
            }
            _ => return Err("Unsupported paste method".into()),
        }
    };

    let output = Command::new("ydotool")
        .args(&args)
        .output()
        .map_err(|e| format!("Failed to execute ydotool: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ydotool failed: {}", stderr));
    }

    Ok(())
}

/// Send a key combination (e.g., Ctrl+V) via xdotool on X11.
#[cfg(target_os = "linux")]
fn send_key_combo_via_xdotool(paste_method: &PasteMethod) -> Result<(), String> {
    let key_combo = match paste_method {
        PasteMethod::CtrlV => "ctrl+v",
        PasteMethod::CtrlShiftV => "ctrl+shift+v",
        PasteMethod::ShiftInsert => "shift+Insert",
        _ => return Err("Unsupported paste method".into()),
    };

    let output = Command::new("xdotool")
        .arg("key")
        .arg("--clearmodifiers")
        .arg(key_combo)
        .output()
        .map_err(|e| format!("Failed to execute xdotool: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("xdotool failed: {}", stderr));
    }

    Ok(())
}

/// Pastes text by invoking an external script.
/// The script receives the text to paste as a single argument.
fn paste_via_external_script(text: &str, script_path: &str) -> Result<(), String> {
    info!("Pasting via external script: {}", script_path);

    let output = Command::new(script_path)
        .arg(text)
        .output()
        .map_err(|e| format!("Failed to execute external script '{}': {}", script_path, e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "External script '{}' failed with exit code {:?}. stderr: {}, stdout: {}",
            script_path,
            output.status.code(),
            stderr.trim(),
            stdout.trim()
        ));
    }

    Ok(())
}

/// Types text directly by simulating individual key presses. On Linux the
/// native tools (wtype/dotool/ydotool/xdotool) do the whole job and Enigo is
/// only the last-resort fallback.
fn paste_direct(
    text: &str,
    app: &AppHandle,
    #[cfg(target_os = "linux")] typing_tool: TypingTool,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        if try_direct_typing_linux(text, typing_tool)? {
            return Ok(());
        }
        info!("Falling back to enigo for direct text input");
        ensure_enigo(app);
        let enigo_state = app
            .try_state::<EnigoState>()
            .ok_or_else(|| ENIGO_UNAVAILABLE.to_string())?;
        let mut enigo = lock_enigo(&enigo_state)?;
        input::paste_text_direct(&mut enigo, text)
    }

    #[cfg(not(target_os = "linux"))]
    {
        ensure_enigo(app);
        let enigo_state = app
            .try_state::<EnigoState>()
            .ok_or_else(|| ENIGO_UNAVAILABLE.to_string())?;
        let mut enigo = lock_enigo(&enigo_state)?;
        input::paste_text_direct(&mut enigo, text)
    }
}

pub(crate) fn send_return_key(enigo: &mut Enigo, key_type: AutoSubmitKey) -> Result<(), String> {
    match key_type {
        AutoSubmitKey::Enter => {
            enigo
                .key(Key::Return, Direction::Press)
                .map_err(|e| format!("Failed to press Return key: {}", e))?;
            enigo
                .key(Key::Return, Direction::Release)
                .map_err(|e| format!("Failed to release Return key: {}", e))?;
        }
        AutoSubmitKey::CtrlEnter => {
            enigo
                .key(Key::Control, Direction::Press)
                .map_err(|e| format!("Failed to press Control key: {}", e))?;
            enigo
                .key(Key::Return, Direction::Press)
                .map_err(|e| format!("Failed to press Return key: {}", e))?;
            enigo
                .key(Key::Return, Direction::Release)
                .map_err(|e| format!("Failed to release Return key: {}", e))?;
            enigo
                .key(Key::Control, Direction::Release)
                .map_err(|e| format!("Failed to release Control key: {}", e))?;
        }
        AutoSubmitKey::CmdEnter => {
            enigo
                .key(Key::Meta, Direction::Press)
                .map_err(|e| format!("Failed to press Meta/Cmd key: {}", e))?;
            enigo
                .key(Key::Return, Direction::Press)
                .map_err(|e| format!("Failed to press Return key: {}", e))?;
            enigo
                .key(Key::Return, Direction::Release)
                .map_err(|e| format!("Failed to release Return key: {}", e))?;
            enigo
                .key(Key::Meta, Direction::Release)
                .map_err(|e| format!("Failed to release Meta/Cmd key: {}", e))?;
        }
    }

    Ok(())
}

fn should_send_auto_submit(auto_submit: bool, paste_method: PasteMethod) -> bool {
    auto_submit && paste_method != PasteMethod::None
}

pub fn paste(text: String, app_handle: AppHandle) -> Result<(), String> {
    let settings = get_settings(&app_handle);
    let paste_method = settings.paste_method;
    let paste_delay_ms = settings.paste_delay_ms;
    let paste_delay_after_ms = settings.paste_delay_after_ms;

    // Append trailing space if setting is enabled
    let text = if settings.append_trailing_space {
        format!("{} ", text)
    } else {
        text
    };

    info!(
        "Using paste method: {:?}, delay before: {}ms, delay after: {}ms",
        paste_method, paste_delay_ms, paste_delay_after_ms
    );

    // Perform the paste operation. Enigo is acquired lazily per path (see
    // `lock_enigo`): native typing tools can complete a paste without it.
    let paste_result = match paste_method {
        PasteMethod::None => {
            info!("PasteMethod::None selected - skipping paste action");
            Ok(())
        }
        PasteMethod::Direct => paste_direct(
            &text,
            &app_handle,
            #[cfg(target_os = "linux")]
            settings.typing_tool,
        ),
        PasteMethod::CtrlV | PasteMethod::CtrlShiftV | PasteMethod::ShiftInsert => {
            // Debug-gated receipt-sequenced paste (#502): restore the clipboard
            // after the target actually reads the transcript, not on a timer.
            // On success it fully handles the paste (including auto-submit and
            // clipboard handling) asynchronously; on failure fall through to
            // the legacy path untouched.
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            if settings.reliable_paste {
                // A permission grant may have landed since mount; pick it up
                // before deciding the reliable path is unavailable.
                ensure_enigo(&app_handle);
                match app_handle.try_state::<EnigoState>() {
                    Some(enigo_state) => match lock_enigo(&enigo_state) {
                        Ok(mut enigo) => {
                            match crate::paste_tx::try_reliable_paste(
                                &text,
                                &app_handle,
                                &paste_method,
                                &mut enigo,
                                settings.auto_submit,
                                settings.auto_submit_key,
                                settings.clipboard_handling,
                            ) {
                                Ok(()) => return Ok(()),
                                Err(e) => {
                                    log::warn!("Reliable paste unavailable ({e}); falling back to legacy paste")
                                }
                            }
                        }
                        Err(e) => {
                            log::warn!("Reliable paste skipped: {e}")
                        }
                    },
                    None => {
                        log::warn!("Reliable paste skipped: {ENIGO_UNAVAILABLE}")
                    }
                }
            }
            paste_via_clipboard(
                &text,
                &app_handle,
                &paste_method,
                paste_delay_ms,
                paste_delay_after_ms,
            )
        }
        PasteMethod::ExternalScript => settings
            .external_script_path
            .as_ref()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| "External script path is not configured".to_string())
            .and_then(|script_path| paste_via_external_script(&text, script_path)),
    };

    // Recovery: when input simulation is unavailable the transcription must not
    // be lost — keep it on the clipboard so the user can paste it manually.
    if let Err(e) = &paste_result {
        if e == ENIGO_UNAVAILABLE {
            if let Err(clip_err) = app_handle.clipboard().write_text(&text) {
                log::warn!("Failed to keep transcription on clipboard: {clip_err}");
            }
        }
    }

    paste_result?;

    if should_send_auto_submit(settings.auto_submit, paste_method) {
        std::thread::sleep(Duration::from_millis(50));
        // Auto-submit is a bonus path: degrade to a warning instead of failing
        // the whole paste when Enigo is unavailable.
        match app_handle.try_state::<EnigoState>() {
            Some(enigo_state) => match lock_enigo(&enigo_state) {
                Ok(mut enigo) => send_return_key(&mut enigo, settings.auto_submit_key)?,
                Err(e) => log::warn!("Auto-submit skipped: {e}"),
            },
            None => log::warn!("Auto-submit skipped: Enigo is not initialized"),
        }
    }

    // After pasting, optionally copy to clipboard based on settings
    if settings.clipboard_handling == ClipboardHandling::CopyToClipboard {
        let clipboard = app_handle.clipboard();
        clipboard
            .write_text(&text)
            .map_err(|e| format!("Failed to copy to clipboard: {}", e))?;
        // Keep the X11 clipboard in sync too (see write path above).
        #[cfg(target_os = "linux")]
        if is_wayland() {
            if is_wl_copy_available() {
                let _ = write_clipboard_via_wl_copy(&text);
            }
            if let Err(e) = write_x11_clipboard_text(&text) {
                log::warn!("X11 clipboard write failed: {e}");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_submit_requires_setting_enabled() {
        assert!(!should_send_auto_submit(false, PasteMethod::CtrlV));
        assert!(!should_send_auto_submit(false, PasteMethod::Direct));
    }

    #[test]
    fn auto_submit_skips_none_paste_method() {
        assert!(!should_send_auto_submit(true, PasteMethod::None));
    }

    #[test]
    fn auto_submit_runs_for_active_paste_methods() {
        assert!(should_send_auto_submit(true, PasteMethod::CtrlV));
        assert!(should_send_auto_submit(true, PasteMethod::Direct));
        assert!(should_send_auto_submit(true, PasteMethod::CtrlShiftV));
        assert!(should_send_auto_submit(true, PasteMethod::ShiftInsert));
    }

    #[test]
    fn restore_is_skipped_only_when_input_backend_is_unavailable() {
        // The clipboard must only be restored when the chord actually had a
        // chance to fire; otherwise the transcription is all the user has left.
        assert!(should_restore_clipboard(&Ok(())));
        assert!(should_restore_clipboard(&Err("other failure".to_string())));
        assert!(!should_restore_clipboard(&Err(
            ENIGO_UNAVAILABLE.to_string()
        )));
    }

    /// Round-trips text through the real X11 clipboard. Requires a running X
    /// server / XWayland with xclip or xsel; skipped otherwise so CI without
    /// a display still passes.
    #[cfg(target_os = "linux")]
    #[test]
    fn x11_clipboard_text_round_trip() {
        if std::env::var_os("DISPLAY").is_none() || !is_x11_clipboard_tool_available() {
            eprintln!("skipping: no DISPLAY or X11 clipboard tool");
            return;
        }
        let probe = "voxix-x11-roundtrip-äöü-123";
        write_x11_clipboard_text(probe).expect("X11 clipboard write failed");
        assert_eq!(read_x11_clipboard_text().as_deref(), Some(probe));
    }

    /// The uinput/daemon readiness probes must fail LOUDLY (with setup
    /// guidance) when the backends are absent — never Ok on a keystroke that
    /// would go nowhere. On machines where the daemons exist these asserts
    /// are skipped by the early return; that path is covered by live paste.
    #[cfg(target_os = "linux")]
    #[test]
    fn uinput_readiness_probes_fail_loudly_when_absent() {
        // dotool side: /dev/uinput must be writable. If some environment
        // actually provides it, skip (live paste covers the positive path).
        let uinput_writable = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .is_ok();
        if !uinput_writable {
            let err = check_dotool_ready().expect_err("dotool probe must fail without /dev/uinput");
            assert!(
                err.contains("uinput") && err.contains("input"),
                "dotool error must name /dev/uinput and the input group, got: {err}"
            );
        } else {
            eprintln!("skipping dotool-absent assert: /dev/uinput is writable here");
        }

        // ydotool side: no socket listener expected on test/CI machines.
        let socket =
            std::env::var("YDOTOOL_SOCKET").unwrap_or_else(|_| "/tmp/.ydotool_socket".to_string());
        let daemon_reachable = std::os::unix::net::UnixStream::connect(&socket).is_ok();
        if !daemon_reachable {
            let err = check_ydotool_ready().expect_err("ydotool probe must fail without ydotoold");
            assert!(
                err.contains("ydotoold"),
                "ydotool error must name the daemon, got: {err}"
            );
        } else {
            eprintln!("skipping ydotool-absent assert: daemon reachable here");
        }
    }

    /// ydotool generation probe must not panic when the binary is missing
    /// (defaults to the current raw-keycode generation) and must cache.
    #[cfg(target_os = "linux")]
    #[test]
    fn ydotool_syntax_probe_defaults_safely() {
        // Must simply return without panicking regardless of environment.
        let _ = ydotool_uses_symbolic_key_syntax();
        // Second call hits the OnceLock cache — same answer, no re-probe.
        let a = ydotool_uses_symbolic_key_syntax();
        let b = ydotool_uses_symbolic_key_syntax();
        assert_eq!(a, b);
    }
}

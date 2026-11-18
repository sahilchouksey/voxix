//! voxix keyboard adapter keyboard shortcut implementation
//!
//! This module provides an alternative to Tauri's global-shortcut plugin
//! using the voxix-keys library for more control over keyboard events.
//!
//! ## Architecture
//!
//! The implementation uses a dedicated manager thread that owns the `HotkeyManager`:
//!
//! ```text
//! ┌─────────────────┐     commands      ┌──────────────────────┐
//! │   Main Thread   │ ───────────────▶ │   Manager Thread     │
//! │                 │   (via channel)   │                      │
//! │ - register()    │                   │ - owns HotkeyManager │
//! │ - unregister()  │                   │ - polls for events   │
//! └─────────────────┘                   │ - dispatches actions │
//!                                       └──────────────────────┘
//! ```
//!
//! This design ensures thread-safety since `HotkeyManager` is only accessed
//! from a single thread. Commands (register/unregister) are sent via an mpsc
//! channel and responses are synchronously awaited.
//!
//! ## Recording Mode
//!
//! For UI key capture, a separate `KeyboardListener` is created on-demand and
//! polled from a dedicated recording thread. Events are emitted to the frontend
//! via Tauri's event system.

use log::{debug, error, info, warn};
use serde::Serialize;
use specta::Type;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use tauri::{AppHandle, Emitter, Manager};
use voxix_keys::{Hotkey, HotkeyId, HotkeyManager, HotkeyState, KeyboardListener};

use crate::settings::{self, get_settings, ShortcutBinding};

use super::handler::handle_shortcut_event;

/// Commands that can be sent to the hotkey manager thread
enum ManagerCommand {
    Register {
        binding_id: String,
        hotkey_string: String,
        response: Sender<Result<(), String>>,
    },
    Unregister {
        binding_id: String,
        response: Sender<Result<(), String>>,
    },
    Shutdown,
}

/// State for the voxix-keys shortcut manager
pub struct VoxixKeysState {
    /// Channel to send commands to the manager thread (wrapped in Mutex for Sync)
    command_sender: Mutex<Sender<ManagerCommand>>,
    /// Handle to the manager thread (wrapped in Mutex for Sync, allows proper join on drop)
    thread_handle: Mutex<Option<JoinHandle<()>>>,
    /// Recording listener for UI key capture (only active during recording)
    recording_listener: Mutex<Option<KeyboardListener>>,
    /// Flag indicating if we're in recording mode
    is_recording: AtomicBool,
    /// The binding ID being recorded (if any)
    recording_binding_id: Mutex<Option<String>>,
    /// Flag to stop recording loop
    recording_running: Arc<AtomicBool>,
}

/// Key event sent to frontend during recording mode
#[derive(Debug, Clone, Serialize, Type)]
pub struct FrontendKeyEvent {
    /// Currently pressed modifier keys
    pub modifiers: Vec<String>,
    /// The key that was pressed (if any)
    pub key: Option<String>,
    /// Whether this is a key down event
    pub is_key_down: bool,
    /// The full hotkey string (e.g., "option+space")
    pub hotkey_string: String,
}

impl VoxixKeysState {
    /// Create a new VoxixKeysState
    pub fn new(app: AppHandle) -> Result<Self, String> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<ManagerCommand>();

        // Use a oneshot channel to detect if the manager thread fails to start
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);

        // Start the manager thread
        let app_clone = app.clone();
        let thread_handle = thread::spawn(move || {
            Self::manager_thread(cmd_rx, app_clone, ready_tx);
        });

        // Wait for the manager thread to signal readiness (or failure)
        match ready_rx.recv() {
            Ok(Ok(())) => { /* manager thread is running */ }
            Ok(Err(e)) => {
                // Manager thread failed to start; wait for it to exit
                let _ = thread_handle.join();
                return Err(e);
            }
            Err(_) => {
                // Channel closed without sending — thread panicked or exited early
                let _ = thread_handle.join();
                return Err("Manager thread failed to start".into());
            }
        }

        Ok(Self {
            command_sender: Mutex::new(cmd_tx),
            thread_handle: Mutex::new(Some(thread_handle)),
            recording_listener: Mutex::new(None),
            is_recording: AtomicBool::new(false),
            recording_binding_id: Mutex::new(None),
            recording_running: Arc::new(AtomicBool::new(false)),
        })
    }

    /// The main manager thread - owns the HotkeyManager and processes commands
    fn manager_thread(
        cmd_rx: Receiver<ManagerCommand>,
        app: AppHandle,
        ready_tx: mpsc::SyncSender<Result<(), String>>,
    ) {
        info!("voxix-keys manager thread started");

        // Create the HotkeyManager in this thread.
        // NOTE: non-blocking mode on Linux only. new_with_blocking() grabs
        // every keyboard exclusively via EVIOCGRAB (including remapper
        // virtual outputs like keyd that feed the compositor) and re-injects
        // through uinput — when any hop in that chain stalls, the whole
        // system keyboard goes dead while the app runs. Passive observation
        // can never break input; the only trade-off is a pressed hotkey also
        // reaches the focused app. macOS/Windows keep blocking mode: there it
        // is a safe CGEventTap/SendInput-level swallow with no device grabs.
        #[cfg(target_os = "linux")]
        let manager = match HotkeyManager::new() {
            Ok(m) => m,
            Err(e) => {
                error!("Failed to create HotkeyManager: {}", e);
                let _ = ready_tx.send(Err(format!("Failed to create HotkeyManager: {}", e)));
                return;
            }
        };
        #[cfg(not(target_os = "linux"))]
        let manager = match HotkeyManager::new_with_blocking() {
            Ok(m) => m,
            Err(e) => {
                error!("Failed to create HotkeyManager: {}", e);
                let _ = ready_tx.send(Err(format!("Failed to create HotkeyManager: {}", e)));
                return;
            }
        };

        // Signal that the manager is ready
        if ready_tx.send(Ok(())).is_err() {
            error!("Manager thread: ready receiver dropped before we could signal");
            return;
        }

        // Maps binding IDs to HotkeyIds and hotkey strings
        let mut binding_to_hotkey: HashMap<String, HotkeyId> = HashMap::new();
        let mut hotkey_to_binding: HashMap<HotkeyId, (String, String)> = HashMap::new(); // (binding_id, hotkey_string)

        loop {
            // Check for hotkey events (non-blocking)
            while let Some(event) = manager.try_recv() {
                if let Some((binding_id, hotkey_string)) = hotkey_to_binding.get(&event.id) {
                    debug!(
                        "voxix-keys event: binding={}, hotkey={}, state={:?}",
                        binding_id, hotkey_string, event.state
                    );
                    let is_pressed = event.state == HotkeyState::Pressed;
                    handle_shortcut_event(&app, binding_id, hotkey_string, is_pressed);
                }
            }

            // Check for commands (non-blocking with timeout)
            match cmd_rx.recv_timeout(std::time::Duration::from_millis(10)) {
                Ok(cmd) => match cmd {
                    ManagerCommand::Register {
                        binding_id,
                        hotkey_string,
                        response,
                    } => {
                        let result = Self::do_register(
                            &manager,
                            &mut binding_to_hotkey,
                            &mut hotkey_to_binding,
                            &binding_id,
                            &hotkey_string,
                        );
                        let _ = response.send(result);
                    }
                    ManagerCommand::Unregister {
                        binding_id,
                        response,
                    } => {
                        let result = Self::do_unregister(
                            &manager,
                            &mut binding_to_hotkey,
                            &mut hotkey_to_binding,
                            &binding_id,
                        );
                        let _ = response.send(result);
                    }
                    ManagerCommand::Shutdown => {
                        info!("voxix-keys manager thread shutting down");
                        break;
                    }
                },
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // No command, continue
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    info!("Command channel disconnected, shutting down");
                    break;
                }
            }
        }

        info!("voxix-keys manager thread stopped");
    }

    /// Register a hotkey
    fn do_register(
        manager: &HotkeyManager,
        binding_to_hotkey: &mut HashMap<String, HotkeyId>,
        hotkey_to_binding: &mut HashMap<HotkeyId, (String, String)>,
        binding_id: &str,
        hotkey_string: &str,
    ) -> Result<(), String> {
        let hotkey: Hotkey = hotkey_string
            .parse()
            .map_err(|e| format!("Failed to parse hotkey '{}': {}", hotkey_string, e))?;

        // Pure modifiers are served by the rdev backend on Linux, and a
        // key-less hotkey left behind in the manager after a binding change
        // would keep matching every press of that modifier (the
        // stale-Super bug). On macOS and Windows the native listener
        // supports modifier-only chords directly, so register them like any
        // other hotkey.
        #[cfg(target_os = "linux")]
        if hotkey.key.is_none() {
            warn!(
                "Not registering modifier-only hotkey '{}' for '{}': it is handled by the rdev backend",
                hotkey_string, binding_id
            );
            return Ok(());
        }

        // Self-heal: if a previous registration still sits under this binding
        // id (an older stale value, or a routing slip), drop it first so at
        // most one hotkey can ever exist per binding id.
        if let Some(old) = binding_to_hotkey.remove(binding_id) {
            if let Err(e) = manager.unregister(old) {
                warn!("Failed to drop stale hotkey for '{}': {}", binding_id, e);
            }
            hotkey_to_binding.remove(&old);
        }

        let id = manager
            .register(hotkey)
            .map_err(|e| format!("Failed to register hotkey: {}", e))?;

        binding_to_hotkey.insert(binding_id.to_string(), id);
        hotkey_to_binding.insert(id, (binding_id.to_string(), hotkey_string.to_string()));

        debug!(
            "Registered voxix-keys shortcut: {} -> {:?}",
            binding_id, hotkey
        );
        Ok(())
    }

    /// Unregister a hotkey
    fn do_unregister(
        manager: &HotkeyManager,
        binding_to_hotkey: &mut HashMap<String, HotkeyId>,
        hotkey_to_binding: &mut HashMap<HotkeyId, (String, String)>,
        binding_id: &str,
    ) -> Result<(), String> {
        if let Some(id) = binding_to_hotkey.remove(binding_id) {
            manager
                .unregister(id)
                .map_err(|e| format!("Failed to unregister hotkey: {}", e))?;
            hotkey_to_binding.remove(&id);
            debug!("Unregistered voxix-keys shortcut: {}", binding_id);
        }
        Ok(())
    }

    /// Register a shortcut binding
    pub fn register(&self, binding: &ShortcutBinding) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.command_sender
            .lock()
            .map_err(|_| "Failed to lock command_sender")?
            .send(ManagerCommand::Register {
                binding_id: binding.id.clone(),
                hotkey_string: binding.current_binding.clone(),
                response: tx,
            })
            .map_err(|_| "Failed to send register command")?;

        rx.recv()
            .map_err(|_| "Failed to receive register response")?
    }

    /// Unregister a shortcut binding
    pub fn unregister(&self, binding: &ShortcutBinding) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.command_sender
            .lock()
            .map_err(|_| "Failed to lock command_sender")?
            .send(ManagerCommand::Unregister {
                binding_id: binding.id.clone(),
                response: tx,
            })
            .map_err(|_| "Failed to send unregister command")?;

        rx.recv()
            .map_err(|_| "Failed to receive unregister response")?
    }

    /// Start recording mode for a specific binding
    pub fn start_recording(&self, app: &AppHandle, binding_id: String) -> Result<(), String> {
        if self.is_recording.load(Ordering::SeqCst) {
            return Err("Already recording".into());
        }

        // Create a new keyboard listener for recording
        let listener = KeyboardListener::new()
            .map_err(|e| format!("Failed to create keyboard listener: {}", e))?;

        {
            let mut recording = self
                .recording_listener
                .lock()
                .map_err(|_| "Failed to lock recording_listener")?;
            *recording = Some(listener);
        }
        {
            let mut binding = self
                .recording_binding_id
                .lock()
                .map_err(|_| "Failed to lock recording_binding_id")?;
            *binding = Some(binding_id);
        }

        self.is_recording.store(true, Ordering::SeqCst);
        self.recording_running.store(true, Ordering::SeqCst);

        // Start a thread to emit key events to the frontend
        let app_clone = app.clone();
        let recording_running = Arc::clone(&self.recording_running);
        thread::spawn(move || {
            Self::recording_loop(app_clone, recording_running);
        });

        debug!("Started voxix-keys recording mode");
        Ok(())
    }

    /// Recording loop - emits key events to frontend during recording
    fn recording_loop(app: AppHandle, running: Arc<AtomicBool>) {
        while running.load(Ordering::SeqCst) {
            let event = {
                let state = match app.try_state::<VoxixKeysState>() {
                    Some(s) => s,
                    None => break,
                };
                let listener = state.recording_listener.lock().ok();
                listener.as_ref().and_then(|l| l.as_ref()?.try_recv())
            };

            if let Some(key_event) = event {
                // Convert to frontend-friendly format
                let frontend_event = FrontendKeyEvent {
                    modifiers: modifiers_to_strings(key_event.modifiers),
                    key: key_event.key.map(|k| k.to_string().to_lowercase()),
                    is_key_down: key_event.is_key_down,
                    hotkey_string: key_event
                        .as_hotkey()
                        .map(|h| h.to_registered_string())
                        .unwrap_or_default(),
                };

                // Emit to frontend
                if let Err(e) = app.emit("voxix-keys-event", &frontend_event) {
                    error!("Failed to emit key event: {}", e);
                }
            } else {
                thread::sleep(std::time::Duration::from_millis(10));
            }
        }

        debug!("Recording loop ended");
    }

    /// Stop recording mode
    pub fn stop_recording(&self) -> Result<(), String> {
        self.is_recording.store(false, Ordering::SeqCst);
        self.recording_running.store(false, Ordering::SeqCst);

        {
            let mut recording = self
                .recording_listener
                .lock()
                .map_err(|_| "Failed to lock recording_listener")?;
            *recording = None;
        }
        {
            let mut binding = self
                .recording_binding_id
                .lock()
                .map_err(|_| "Failed to lock recording_binding_id")?;
            *binding = None;
        }

        debug!("Stopped voxix-keys recording mode");
        Ok(())
    }
}

impl Drop for VoxixKeysState {
    fn drop(&mut self) {
        // Signal recording to stop
        self.recording_running.store(false, Ordering::SeqCst);
        self.is_recording.store(false, Ordering::SeqCst);

        // Send shutdown command
        if let Ok(sender) = self.command_sender.lock() {
            let _ = sender.send(ManagerCommand::Shutdown);
        }

        // Wait for the manager thread to finish
        if let Ok(mut handle) = self.thread_handle.lock() {
            if let Some(h) = handle.take() {
                let _ = h.join();
            }
        }
    }
}

/// Convert voxix-keys Modifiers to a list of strings
fn modifiers_to_strings(modifiers: voxix_keys::Modifiers) -> Vec<String> {
    let mut result = Vec::new();

    // Side-agnostic match: compounds like `OPT` are `OPT_LEFT | OPT_RIGHT`,
    // but a single modifier press only sets one side bit, so `contains`
    // would never fire for e.g. a lone left Option.
    if modifiers.intersects(voxix_keys::Modifiers::CTRL) {
        result.push("ctrl".to_string());
    }
    if modifiers.intersects(voxix_keys::Modifiers::OPT) {
        #[cfg(target_os = "macos")]
        result.push("option".to_string());
        #[cfg(not(target_os = "macos"))]
        result.push("alt".to_string());
    }
    if modifiers.intersects(voxix_keys::Modifiers::SHIFT) {
        result.push("shift".to_string());
    }
    if modifiers.intersects(voxix_keys::Modifiers::CMD) {
        #[cfg(target_os = "macos")]
        result.push("command".to_string());
        #[cfg(not(target_os = "macos"))]
        result.push("super".to_string());
    }
    if modifiers.intersects(voxix_keys::Modifiers::FN) {
        result.push("fn".to_string());
    }

    result
}

/// Validate a shortcut string for the VoxixKeys implementation.
/// VoxixKeys is more permissive: allows modifier-only combos and the fn key.
pub fn validate_shortcut(raw: &str) -> Result<(), String> {
    if raw.trim().is_empty() {
        return Err("Shortcut cannot be empty".into());
    }
    // VoxixKeys accepts modifier-only, key-only, and modifier+key combos
    // Just verify the string is parseable
    raw.parse::<Hotkey>()
        .map(|_| ())
        .map_err(|e| format!("Invalid shortcut for VoxixKeys: {}", e))
}

/// Initialize voxix-keys shortcuts
pub fn init_shortcuts(app: &AppHandle) -> Result<(), String> {
    let state = VoxixKeysState::new(app.clone())?;

    let default_bindings = settings::get_default_settings().bindings;
    let user_settings = settings::load_or_create_app_settings(app);

    // Register all bindings except cancel (which is dynamic)
    for (id, default_binding) in default_bindings {
        if id == "cancel" {
            continue;
        }
        // Skip post-processing shortcut when the feature is disabled
        if id == "transcribe_with_post_process" && !user_settings.post_process_enabled {
            continue;
        }

        let binding = user_settings
            .bindings
            .get(&id)
            .cloned()
            .unwrap_or(default_binding);

        if let Err(e) = state.register(&binding) {
            error!(
                "Failed to register voxix-keys shortcut {} during init: {}",
                id, e
            );
        }
    }

    app.manage(state);
    info!("voxix-keys shortcuts initialized");
    Ok(())
}

/// Register the cancel shortcut (called when recording starts)
pub fn register_cancel_shortcut(app: &AppHandle) {
    // Disabled on Linux due to instability
    #[cfg(target_os = "linux")]
    {
        let _ = app;
        return;
    }

    #[cfg(not(target_os = "linux"))]
    {
        let app_clone = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(cancel_binding) = get_settings(&app_clone).bindings.get("cancel").cloned() {
                if let Some(state) = app_clone.try_state::<VoxixKeysState>() {
                    if let Err(e) = state.register(&cancel_binding) {
                        error!("Failed to register cancel shortcut: {}", e);
                    }
                }
            }
        });
    }
}

/// Unregister the cancel shortcut (called when recording stops)
pub fn unregister_cancel_shortcut(app: &AppHandle) {
    #[cfg(target_os = "linux")]
    {
        let _ = app;
        return;
    }

    #[cfg(not(target_os = "linux"))]
    {
        let app_clone = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(cancel_binding) = get_settings(&app_clone).bindings.get("cancel").cloned() {
                if let Some(state) = app_clone.try_state::<VoxixKeysState>() {
                    let _ = state.unregister(&cancel_binding);
                }
            }
        });
    }
}

/// Register a shortcut
pub fn register_shortcut(app: &AppHandle, binding: ShortcutBinding) -> Result<(), String> {
    let state = app
        .try_state::<VoxixKeysState>()
        .ok_or("VoxixKeysState not initialized")?;
    state.register(&binding)
}

/// Unregister a shortcut
pub fn unregister_shortcut(app: &AppHandle, binding: ShortcutBinding) -> Result<(), String> {
    let state = app
        .try_state::<VoxixKeysState>()
        .ok_or("VoxixKeysState not initialized")?;
    state.unregister(&binding)
}

/// Start key recording mode
#[tauri::command]
#[specta::specta]
pub fn start_voxix_keys_recording(app: AppHandle, binding_id: String) -> Result<(), String> {
    // While Secure Input is active the tap receives no KeyDown/KeyUp, so the
    // recorder would silently capture just the modifier and overwrite the
    // binding with it (issue #1578). Refuse instead; the frontend maps this
    // marker to a localized explanation, and the noted impact makes the
    // warning banner appear with the full story.
    if crate::secure_input::is_enabled_now() {
        crate::secure_input::note_recorder_blocked(&app);
        return Err("secure-input-active".into());
    }

    // Key *capture* is independent of which backend fires registered shortcuts
    // (tauri vs voxix_keys), so lazily ensure the capture state exists for
    // either implementation.
    let state = match app.try_state::<VoxixKeysState>() {
        Some(s) => s,
        None => {
            let s = VoxixKeysState::new(app.clone())?;
            app.manage(s);
            app
                .try_state::<VoxixKeysState>()
                .ok_or("failed to manage voxix-keys capture state")?
        }
    };

    // Suspend every registered shortcut so a combo that overlaps an existing
    // binding can't fire it (or have its keys swallowed) mid-capture.
    super::suspend_all_shortcuts(&app);

    let result = state.start_recording(&app, binding_id);
    if result.is_err() {
        super::resume_all_shortcuts(&app);
    }
    result
}

/// Stop key recording mode
#[tauri::command]
#[specta::specta]
pub fn stop_voxix_keys_recording(app: AppHandle) -> Result<(), String> {
    // Nothing to stop if the capture state was never created.
    let Some(state) = app.try_state::<VoxixKeysState>() else {
        return Ok(());
    };

    // Restore shortcuts from settings regardless of how recording ended.
    // A commit has already registered the new binding via change_binding;
    // re-registering it here fails cleanly and is ignored.
    let result = state.stop_recording();
    super::resume_all_shortcuts(&app);
    result
}

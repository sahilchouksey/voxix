//! rdev-based shortcut backend for modifier-only hotkeys on Linux.
//!
//! The `global_hotkey` / `muda` crate can't handle modifier-only shortcuts
//! (e.g. just "Alt") because X11's `XGrabKey` requires a keycode. This module
//! uses `rdev::listen` to detect modifier key press/release events system-wide
//! and dispatches them through the same `handle_shortcut_event` path.

use log::{debug, error, info};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use tauri::AppHandle;

use crate::settings::ShortcutBinding;

use super::handler::handle_shortcut_event;

/// Modifier names that rdev reports (from Debug format of Key enum).
/// The rdev fork only defines `MetaLeft`/`MetaRight` variants — the older
/// `Meta`/`Super*` names were never reported, so Super events fell through
/// unhandled.
const MODIFIER_NAMES: &[(&str, &str)] = &[
    ("Alt", "alt"),
    ("AltGr", "alt"),
    ("ControlLeft", "ctrl"),
    ("ControlRight", "ctrl"),
    ("ShiftLeft", "shift"),
    ("ShiftRight", "shift"),
    ("MetaLeft", "super"),
    ("MetaRight", "super"),
];

/// Map an rdev key name to our modifier token
fn rdev_key_to_modifier(name: &str) -> Option<&'static str> {
    MODIFIER_NAMES
        .iter()
        .find(|(rdev_name, _)| *rdev_name == name)
        .map(|(_, mod_name)| *mod_name)
}

/// State for the rdev-based modifier listener
pub struct RdevBackend {
    running: Arc<AtomicBool>,
    handle: Mutex<Option<JoinHandle<()>>>,
    /// The modifier bindings registered: binding_id -> modifier_name
    bindings: Arc<Mutex<HashSet<(String, String)>>>,
}

impl RdevBackend {
    pub fn new() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            handle: Mutex::new(None),
            bindings: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Start the rdev listener thread
    pub fn start(&self, app: AppHandle) -> Result<(), String> {
        if self.running.load(Ordering::Relaxed) {
            return Ok(()); // Already running
        }

        let running = self.running.clone();
        let bindings = self.bindings.clone();

        let handle = thread::spawn(move || {
            running.store(true, Ordering::Relaxed);

            info!("rdev modifier listener thread started");

            // Track which modifiers are currently held
            let mut held_modifiers: HashSet<String> = HashSet::new();
            // Track which bindings have already fired (to avoid re-firing on key repeat)
            let mut fired_bindings: HashSet<String> = HashSet::new();

            let callback = move |event: rdev::Event| {
                let modifier = match &event.event_type {
                    rdev::EventType::KeyPress(key) => {
                        let name = format!("{:?}", key);
                        debug!("rdev: KeyPress {:?}", name);
                        rdev_key_to_modifier(&name)
                    }
                    rdev::EventType::KeyRelease(key) => {
                        let name = format!("{:?}", key);
                        debug!("rdev: KeyRelease {:?}", name);
                        rdev_key_to_modifier(&name)
                    }
                    _ => None,
                };

                let modifier = match modifier {
                    Some(m) => m,
                    None => return,
                };

                let is_release = matches!(event.event_type, rdev::EventType::KeyRelease(_));

                if is_release {
                    held_modifiers.remove(modifier);

                    // Check each registered binding
                    let current_bindings = bindings.lock().unwrap().clone();
                    for (binding_id, mod_name) in &current_bindings {
                        if *mod_name == modifier && fired_bindings.contains(binding_id) {
                            debug!(
                                "rdev: modifier released, dispatching {} release",
                                binding_id
                            );
                            fired_bindings.remove(binding_id);
                            handle_shortcut_event(&app, binding_id, mod_name, false);
                        }
                    }
                } else {
                    held_modifiers.insert(modifier.to_string());

                    // Check each registered binding
                    let current_bindings = bindings.lock().unwrap().clone();
                    for (binding_id, mod_name) in &current_bindings {
                        if *mod_name == modifier && !fired_bindings.contains(binding_id) {
                            debug!("rdev: modifier pressed, dispatching {} press", binding_id);
                            fired_bindings.insert(binding_id.clone());
                            handle_shortcut_event(&app, binding_id, mod_name, true);
                        }
                    }
                }
            };

            if let Err(e) = rdev::listen(callback) {
                error!("rdev listen failed: {:?}", e);
            }

            running.store(false, Ordering::Relaxed);
            info!("rdev modifier listener thread stopped");
        });

        *self.handle.lock().unwrap() = Some(handle);
        Ok(())
    }

    /// Register a modifier-only binding
    pub fn register(&self, binding: &ShortcutBinding) -> Result<(), String> {
        let modifier = binding_to_modifier(&binding.current_binding)?;
        self.bindings
            .lock()
            .unwrap()
            .insert((binding.id.clone(), modifier.to_string()));
        info!(
            "rdev: registered binding '{}' as modifier '{}'",
            binding.id, modifier
        );
        Ok(())
    }

    /// Unregister a binding
    pub fn unregister(&self, binding: &ShortcutBinding) -> Result<(), String> {
        // Remove by binding id: computing the modifier from the current string
        // would abort on a combo binding and leak a stale modifier entry.
        self.bindings
            .lock()
            .unwrap()
            .retain(|(id, _)| id != &binding.id);
        debug!("rdev: unregistered binding '{}'", binding.id);
        Ok(())
    }

    /// Stop the listener thread
    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.handle.lock().unwrap().take() {
            let _ = handle.join();
        }
    }
}

/// Extract the modifier name from a modifier-only binding
fn binding_to_modifier(raw: &str) -> Result<String, String> {
    let part = raw.trim().to_lowercase();
    match part.as_str() {
        "alt" | "option" => Ok("alt".to_string()),
        "ctrl" | "control" => Ok("ctrl".to_string()),
        "shift" => Ok("shift".to_string()),
        "super" | "meta" | "cmd" | "command" | "win" | "windows" => Ok("super".to_string()),
        _ => Err(format!("'{}' is not a recognized modifier", raw)),
    }
}

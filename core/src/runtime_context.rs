//! Runtime runtime-context extraction (SDK plan §3.1).
//!
//! Managers depend on a `tauri::AppHandle` for paths, settings, and events.
//! A real no-display service must be able to run the same engine machinery
//! without initializing Tauri/Wry/GTK. This module hides the ways a manager
//! reaches those three concerns behind a narrow context union:
//!
//! - [`RuntimePaths`]      — where user data and bundled resources live
//! - [`SettingsRepository`]— how settings are loaded/stored
//! - [`DomainEventSink`]   — where domain events are delivered
//!
//! [`TauriRuntimeContext`] adapts the existing Tauri store/paths/plugin
//! machinery; [`FileRuntimeContext`] serves the standalone daemon with the
//! same app-data layout plus atomic JSON settings. Managers take an
//! `Arc<RuntimeContext>`; compatibility constructors keep working.

use crate::settings::{self, AppSettings};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

/// Where user data and bundled resources live.
pub trait RuntimePaths: Send + Sync {
    fn app_data_dir(&self) -> Result<PathBuf, String>;
    fn resource_dir(&self) -> Result<PathBuf, String>;
}

/// How settings are loaded / stored.
pub trait SettingsRepository: Send + Sync {
    fn load(&self) -> Result<AppSettings, String>;
    fn store(&self, s: &AppSettings) -> Result<(), String>;
}

/// A domain event emitted by managers. `Named` carries the legacy free-form
/// Tauri event names; the streaming events map onto the typed tauri-specta
/// events the frontend already listens to. The file-backed runtime logs them
/// (at debug level) instead of emitting to a bus.
#[derive(Clone)]
pub enum DomainEvent {
    Named {
        event: String,
        payload: serde_json::Value,
    },
    StreamText {
        committed: String,
        tentative: String,
    },
    StreamPhaseWorking {
        kind: Option<crate::managers::transcription::StreamWorkKind>,
    },
}

impl DomainEvent {
    /// Named event with an arbitrary serializable payload. Serialization
    /// failures (practically unreachable) degrade to a null payload instead
    /// of slipping a panic into manager control flow.
    pub fn named(event: impl Into<String>, payload: &impl serde::Serialize) -> Self {
        let event = event.into();
        let payload = serde_json::to_value(payload).unwrap_or_else(|e| {
            log::warn!(target: "service", "event serialization failed for '{event}': {e}");
            serde_json::Value::Null
        });
        DomainEvent::Named { event, payload }
    }
}

/// Where domain events are delivered.
pub trait DomainEventSink: Send + Sync {
    fn emit(&self, event: DomainEvent);
}

/// Tauri-backed context: delegates paths to Tauri's path resolver (portable
/// aware), settings to the shared store helpers, and events to the running
/// app's event bus (typed events preserved for the frontend contract). An
/// optional sink receives a copy of every domain event first (the in-process
/// service forwards stream partials onto its bus; frontend delivery is never
/// replaced).
pub struct TauriRuntimeContext {
    app: AppHandle,
    sink: Option<Arc<dyn Fn(DomainEvent) + Send + Sync>>,
}

impl RuntimePaths for TauriRuntimeContext {
    fn app_data_dir(&self) -> Result<PathBuf, String> {
        crate::portable::app_data_dir(&self.app).map_err(|e| e.to_string())
    }

    fn resource_dir(&self) -> Result<PathBuf, String> {
        self.app.path().resource_dir().map_err(|e| e.to_string())
    }
}

impl SettingsRepository for TauriRuntimeContext {
    fn load(&self) -> Result<AppSettings, String> {
        Ok(settings::get_settings(&self.app))
    }

    fn store(&self, s: &AppSettings) -> Result<(), String> {
        settings::write_settings(&self.app, s.clone());
        Ok(())
    }
}

impl DomainEventSink for TauriRuntimeContext {
    fn emit(&self, event: DomainEvent) {
        if let Some(sink) = &self.sink {
            sink(event.clone());
        }
        match event {
            DomainEvent::Named { event, payload } => {
                let _ = self.app.emit(&event, payload);
            }
            DomainEvent::StreamText {
                committed,
                tentative,
            } => {
                use tauri_specta::Event;
                let _ = crate::managers::transcription::StreamTextEvent {
                    committed,
                    tentative,
                }
                .emit(&self.app);
            }
            DomainEvent::StreamPhaseWorking { kind } => {
                use tauri_specta::Event;
                let _ = crate::managers::transcription::StreamPhaseEvent {
                    phase: crate::managers::transcription::StreamPhase::Working,
                    kind,
                }
                .emit(&self.app);
            }
        }
    }
}

/// File-backed context for the standalone daemon: the same app-data layout on
/// disk, settings persisted atomically as JSON (mode 0600 where the OS
/// supports it), and events reduced to structured log lines — or delivered to
/// a service-owned sink (broadcast partials, §3.1) when one is installed.
pub struct FileRuntimeContext {
    data_dir: PathBuf,
    resource_dir: Option<PathBuf>,
    /// Optional event sink replacing the default structured-log delivery.
    sink: Option<Arc<dyn Fn(DomainEvent) + Send + Sync>>,
}

impl FileRuntimeContext {
    /// A temp-dir scoped context, most useful for tests and CI.
    pub fn in_temp_dir(scope: &str) -> Self {
        let dir = std::env::temp_dir()
            .join(format!(
                "voxi-{scope}-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_millis()
            ))
            .join("data");
        let _ = std::fs::create_dir_all(&dir);
        Self {
            data_dir: dir,
            resource_dir: None,
            sink: None,
        }
    }

    /// Install the domain-event sink (the standalone service forwards
    /// manager-emitted stream partials onto its broadcast bus). Replaces the
    /// default structured-log delivery.
    pub fn with_event_sink(mut self, sink: Arc<dyn Fn(DomainEvent) + Send + Sync>) -> Self {
        self.sink = Some(sink);
        self
    }
}

impl RuntimePaths for FileRuntimeContext {
    fn app_data_dir(&self) -> Result<PathBuf, String> {
        Ok(self.data_dir.clone())
    }

    fn resource_dir(&self) -> Result<PathBuf, String> {
        if let Some(dir) = self.resource_dir.clone() {
            return Ok(dir);
        }
        if let Some(dir) = std::env::var_os("VOXIX_RESOURCE_DIR") {
            return Ok(PathBuf::from(dir));
        }
        Err("no resource directory for file runtime".to_string())
    }
}

impl SettingsRepository for FileRuntimeContext {
    fn load(&self) -> Result<AppSettings, String> {
        settings::file_settings::load(&self.settings_file())
    }

    fn store(&self, s: &AppSettings) -> Result<(), String> {
        settings::file_settings::store(&self.settings_file(), s)
    }
}

impl DomainEventSink for FileRuntimeContext {
    fn emit(&self, event: DomainEvent) {
        if let Some(sink) = &self.sink {
            sink(event);
            return;
        }
        match event {
            DomainEvent::Named { event, payload } => {
                log::debug!(target: "service", "[{event}] {payload}");
            }
            DomainEvent::StreamText {
                committed,
                tentative,
            } => {
                log::debug!(target: "service", "[stream_text] committed={committed} tentative={tentative}");
            }
            DomainEvent::StreamPhaseWorking { kind } => {
                log::debug!(target: "service", "[stream_phase] working {:?}", kind);
            }
        }
    }
}

impl FileRuntimeContext {
    fn settings_file(&self) -> PathBuf {
        self.data_dir.join(settings::SETTINGS_STORE_PATH)
    }
}

/// The context union managers are constructed with.
#[derive(Clone)]
pub enum RuntimeContext {
    Tauri(Arc<TauriRuntimeContext>),
    File(Arc<FileRuntimeContext>),
}

impl RuntimeContext {
    pub fn tauri(app: &AppHandle) -> Self {
        RuntimeContext::Tauri(Arc::new(TauriRuntimeContext {
            app: app.clone(),
            sink: None,
        }))
    }

    /// Tauri variant with a domain-event sink installed (the in-process
    /// service forwards stream partials onto its broadcast bus — Option B).
    /// The sink runs *before* frontend delivery; the frontend contract is
    /// unchanged.
    pub fn tauri_with_sink(app: &AppHandle, sink: Arc<dyn Fn(DomainEvent) + Send + Sync>) -> Self {
        RuntimeContext::Tauri(Arc::new(TauriRuntimeContext {
            app: app.clone(),
            sink: Some(sink),
        }))
    }
    pub fn file(data_dir: PathBuf, resource_dir: Option<PathBuf>) -> Self {
        let _ = std::fs::create_dir_all(&data_dir);
        RuntimeContext::File(Arc::new(FileRuntimeContext {
            data_dir,
            resource_dir,
            sink: None,
        }))
    }

    /// File variant with a domain-event sink installed (service broadcast
    /// partials — SDK plan §3.1: standalone uses service-owned channels).
    pub fn file_with_sink(
        data_dir: PathBuf,
        resource_dir: Option<PathBuf>,
        sink: Arc<dyn Fn(DomainEvent) + Send + Sync>,
    ) -> Self {
        let _ = std::fs::create_dir_all(&data_dir);
        RuntimeContext::File(Arc::new(
            FileRuntimeContext {
                data_dir,
                resource_dir,
                sink: None,
            }
            .with_event_sink(sink),
        ))
    }

    /// Absolute path of a stored file or subdirectory (portable-aware for the
    /// Tauri variant, data-dir-relative for the file variant). Opaque relative
    /// paths stay data-relative like `app.path().resolve(r, AppData)`.
    pub fn data_path(&self, relative: &str) -> Result<PathBuf, String> {
        Ok(self.app_data_dir()?.join(relative))
    }

    pub fn app_data_dir(&self) -> Result<PathBuf, String> {
        match self {
            RuntimeContext::Tauri(ctx) => RuntimePaths::app_data_dir(ctx.as_ref()),
            RuntimeContext::File(ctx) => RuntimePaths::app_data_dir(ctx.as_ref()),
        }
    }

    pub fn resource_dir(&self) -> Result<PathBuf, String> {
        match self {
            RuntimeContext::Tauri(ctx) => RuntimePaths::resource_dir(ctx.as_ref()),
            RuntimeContext::File(ctx) => RuntimePaths::resource_dir(ctx.as_ref()),
        }
    }

    pub fn settings(&self) -> AppSettings {
        match self {
            RuntimeContext::Tauri(ctx) => {
                SettingsRepository::load(ctx.as_ref()).unwrap_or_else(|e| {
                    log::warn!("settings load failed ({e}); falling back to defaults");
                    AppSettings::default()
                })
            }
            RuntimeContext::File(ctx) => {
                SettingsRepository::load(ctx.as_ref()).unwrap_or_else(|e| {
                    log::warn!("settings load failed ({e}); falling back to defaults");
                    AppSettings::default()
                })
            }
        }
    }

    pub fn write_settings(&self, s: &AppSettings) {
        let result = match self {
            RuntimeContext::Tauri(ctx) => SettingsRepository::store(ctx.as_ref(), s),
            RuntimeContext::File(ctx) => SettingsRepository::store(ctx.as_ref(), s),
        };
        if let Err(e) = result {
            log::warn!("failed to persist settings: {e}");
        }
    }

    /// Whether recording (microphone capture) is currently active in the
    /// embeddable runtime. The file runtime has no recorder: always false,
    /// which means the idle watcher may unload per settings.
    pub fn is_recording(&self) -> bool {
        match self {
            RuntimeContext::Tauri(ctx) => ctx
                .app
                .try_state::<Arc<crate::managers::audio::AudioRecordingManager>>()
                .is_some_and(|a| a.is_recording()),
            RuntimeContext::File(_) => false,
        }
    }
}

impl RuntimeContext {
    /// Inherent emit: call-sites reach for `ctx.emit(...)` without importing
    /// the sink trait. Delegates per variant.
    pub fn emit(&self, event: DomainEvent) {
        match self {
            RuntimeContext::Tauri(ctx) => DomainEventSink::emit(ctx.as_ref(), event),
            RuntimeContext::File(ctx) => DomainEventSink::emit(ctx.as_ref(), event),
        }
    }
}

/// Resolve a CLI-provided data directory: absolute/relative paths pass
/// through (with `~` expansion); empty/None yields `None` and callers fall
/// back to the default app-data layout.
pub fn expand_data_dir(arg: Option<&Path>) -> Option<PathBuf> {
    let Some(p) = arg else {
        return None;
    };
    let s = p.to_string_lossy();
    if s.starts_with('~') {
        if let Some(home) = std::env::var_os("HOME") {
            return Some(
                Path::new(&home)
                    .join(s.trim_start_matches('~'))
                    .to_path_buf(),
            );
        }
    }
    Some(p.to_path_buf())
}

/// Default (non-portable) app data directory, mirroring Tauri's per-OS
/// resolver without needing a running app. The bundle identifier is build-time
/// environment injected by build.rs.
pub fn default_app_data_dir() -> Result<PathBuf, String> {
    let identifier = option_env!("VOXIX_IDENTIFIER").unwrap_or("dev.xix3r.voxix");
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").map_err(|_| "no HOME".to_string())?;
        return Ok(Path::new(&home)
            .join("Library/Application Support")
            .join(identifier));
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA").map_err(|_| "no APPDATA".to_string())?;
        return Ok(Path::new(&appdata).join(identifier));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let base = std::env::var("XDG_DATA_HOME")
            .ok()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| Path::new(&h).join(".local/share"))
            })
            .ok_or_else(|| "no XDG_DATA_HOME or HOME".to_string())?;
        return Ok(base.join(identifier));
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        Err("unsupported platform".to_string())
    }
}

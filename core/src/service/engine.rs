//! Engine adapter behind the runtime trait (SDK plan §3.1 + §15 G6/G21).
//!
//! The `EngineRuntime` trait is the narrow seam between the service control
//! plane and the transcription machinery. For M1 the production adapter wraps
//! the existing [`ModelManager`] / [`TranscriptionManager`] — reusing their
//! state, loaders, and idle-watchers rather than forking the logic — plus the
//! RuntimeContext for settings access. A deterministic in-memory adapter is
//! provided for tests/CI where no model artifacts exist.
//!
//! M2 adds the media-plane methods: lazy model load with a warmup benchmark
//! (§7: measured realtime factor drives the partial-cadence gate), the
//! native stream lifecycle (§3.2/§7: feed/finalize/cancel through the
//! existing `StreamRouter` worker), and the final-only batch decode used by
//! ONNX engines (§7: authoritative full final decode, no speculative
//! partials).

use crate::managers::model::ModelManager;
use crate::managers::transcription::TranscriptionManager;
use crate::runtime_context::RuntimeContext;
use crate::service::api::Capabilities;

pub struct EngineRuntimeStatus {
    pub engine_version: String,
    pub selected_model_id: Option<String>,
    pub model_state: ModelState,
    pub backend: Option<String>,
    /// The model id currently resident in the runtime (M3: differs from the
    /// selection while an eager load is pending or has failed).
    pub loaded_model_id: Option<String>,
    pub max_realtime_factor: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelState {
    Unloaded,
    Loading,
    Loaded,
}

/// The engine seam. M1 exercises status, version handoff, and the lifecycle
/// hooks; leasing of the single inference slot is owned by `ServiceState`.
/// M2 adds the media-plane hooks (stream lifecycle, lazy load + benchmark,
/// final-only batch decode).
pub trait EngineRuntime: Send + Sync {
    /// Build version + short git SHA (§15 G6/G21), e.g. `0.1.0+0a1b2c3`.
    fn engine_version(&self) -> String;
    fn capabilities(&self) -> Capabilities;
    fn status(&self) -> EngineRuntimeStatus;
    /// A session begins using the engine (M1: reservation notification; the
    /// real audio/decode handshake arrives in M2).
    fn session_open(&self, session_id: &str, model: Option<&str>) -> Result<(), String>;
    /// A session released the engine (canceled/finished/disconnected).
    fn session_close(&self, session_id: &str, finished: bool);

    /// M2: lazy-load the session's snapshot model and resolve the media
    /// mode/facts for the upcoming stream. Blocking by contract — callers run
    /// it on the engine worker (spawn_blocking). `Ok` with a native-capable
    /// model resolves mode `native`; every other backend stays
    /// `final_only` in v1 (ONNX default per §7; partials ship only behind the
    /// benchmark gate).
    fn ensure_model_loaded(&self, _model: Option<&str>) -> Result<EngineMediaFacts, String> {
        Ok(EngineMediaFacts {
            model_id: String::new(),
            backend: None,
            streaming_mode: "final_only",
            measured_realtime_factor: None,
        })
    }

    /// M2: begin the session's native stream via the manager's worker. The
    /// manager holds a single stream; the service lease guarantees only one
    /// stream owns it.
    fn stream_start(&self) -> Result<(), String> {
        Ok(())
    }
    /// M2: feed one audio chunk into the live stream (16 kHz mono f32).
    fn stream_feed(&self, _pcm: &[f32]) -> Result<(), String> {
        Ok(())
    }
    /// M2: flush the live stream; `Ok(Some(text))` is the final text, `Ok(None)`
    /// means the stream never produced one (manager-fallback path).
    fn stream_finalize(&self) -> Result<Option<String>, String> {
        let _ = self.status();
        Ok(None)
    }
    /// M2: abandon the live stream (cancel). The manager may still complete a
    /// native decode tail before dropping it (§15 G5).
    fn stream_cancel(&self) -> Result<(), String> {
        Ok(())
    }

    /// M2: final-only authoritative batch decode of accumulated audio.
    fn batch_transcribe(&self, _audio: &[f32]) -> Result<String, String> {
        Ok(String::new())
    }

    /// M2: negotiated streaming mode for a session snapshot (`native`,
    /// `windowed_batch` when the gated adapter exists, or `final_only`).
    fn negotiated_streaming_mode(&self, _model: Option<&str>) -> &'static str {
        "final_only"
    }

    /// M2: benchmark-gated observed realtime factor (audio_seconds / compute_seconds).
    fn observed_realtime_factor(&self) -> Option<f64> {
        None
    }

    /// M2: current rolling worst-case observed realtime factor (§15 S8).
    fn max_realtime_factor(&self) -> Option<f64> {
        None
    }

    // ------------------------------------------------------------------
    // M3: model-surface hooks (SDK plan §5 Models + §15 G7).
    // ------------------------------------------------------------------

    /// All models the engine knows (catalog + on-disk discovery). The service
    /// projects a safe view and never sees sources URLs.
    fn list_models(&self) -> Vec<crate::managers::model::ModelInfo>;

    /// Model info for one id.
    fn model_info(&self, _model_id: &str) -> Option<crate::managers::model::ModelInfo> {
        None
    }

    /// The persisted selection (may differ from the loaded runtime).
    fn selected_model(&self) -> Option<String>;

    /// Whether the requested model is ready for an immediate decode (the
    /// loaded runtime matches the request; batch never implicitly loads —
    /// §15 G7 "preparation is explicit/prepaid").
    fn model_ready(&self, _model: Option<&str>) -> bool {
        false
    }

    /// Persist a model selection and optionally eager-load it. On a failed
    /// eager load the engine reverts the selection so the previous warm
    /// runtime keeps running (artifact publication ≠ runtime activation).
    /// Blocking by contract — callers run it on the engine worker.
    fn model_select(
        &self,
        _model_id: &str,
        _eager: bool,
    ) -> Result<crate::service::engine::ModelSelectionOutcome, String> {
        Ok(crate::service::engine::ModelSelectionOutcome {
            model_id: String::new(),
            loaded: false,
        })
    }

    /// Refresh on-disk download status (`is_downloaded` / `partial_size`) so
    /// the list surface reflects the shared app-data dir (models installed by
    /// any client become visible on the next poll). Default: no-op.
    fn refresh_model_status(&self) {}

    /// Managed download (v1.1 additive): validate and start (or converge on) a
    /// single in-flight download for `model_id`. Sync seam over the async
    /// manager — the default refuses.
    fn download_model_start(&self, _model_id: &str) -> Result<(), String> {
        Err("download_unsupported".to_string())
    }

    /// Cancel an in-flight managed download (v1.1 additive). Keeps partials
    /// for resume; never deletes user data. Default: refuses.
    fn cancel_model_download(&self, _model_id: &str) -> Result<(), String> {
        Err("cancel_unsupported".to_string())
    }
}

/// Result of a model selection: the id that is now selected and whether the
/// runtime was eagerly loaded.
#[derive(Debug, Clone)]
pub struct ModelSelectionOutcome {
    pub model_id: String,
    pub loaded: bool,
}

/// Media facts resolved at stream start (M2).
pub struct EngineMediaFacts {
    pub model_id: String,
    pub backend: Option<String>,
    pub streaming_mode: &'static str,
    pub measured_realtime_factor: Option<f64>,
}

/// Streaming mode vocabulary (SDK plan §5/§7, §15 §S6/S8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum StreamingMode {
    Native,
    WindowedBatch,
    FinalOnly,
}

impl StreamingMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            StreamingMode::Native => "native",
            StreamingMode::WindowedBatch => "windowed_batch",
            StreamingMode::FinalOnly => "final_only",
        }
    }
    pub fn from_str_label(label: &str) -> Self {
        match label {
            "native" => StreamingMode::Native,
            "windowed_batch" => StreamingMode::WindowedBatch,
            _ => StreamingMode::FinalOnly,
        }
    }
}

/// Broadcast frame bridging the manager's [`crate::runtime_context::DomainEvent`]
/// stream-text partials to WS sessions (engine-local partials; the service owns
/// the bus and attributes events to the active streaming session).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StreamTextFrame {
    pub committed: String,
    pub tentative: String,
}

/// Benchmark gate for the future ONNX windowed-batch adapter (§7): partial
/// emissions require the decode to keep ahead of realtime by at least
/// `ONNX_WINDOWED_REALTIME_FACTOR_GATE`× (the ≥ 2× headroom behind the
/// 500–900 ms partial cadence of fast ONNX rolling batches, §7 table).
pub const ONNX_WINDOWED_REALTIME_FACTOR_GATE: f64 = 2.0;
/// Audio used for the load/warmup benchmark (seconds).
pub const BENCHMARK_WARMUP_SECONDS: f64 = 1.0;
/// Maximum synthesized gap silence inserted on flagged discontinuity (ms).
pub const MAX_GAP_SILENCE_MS: u64 = 5_000;

/// Deterministic 16 kHz mono benchmark waveform (summed low-frequency sines,
/// amplitude clipped low so decoders do not clip) for the load/warmup
/// realtime-factor benchmark.
pub fn benchmark_pcm(audio_secs: f64) -> Vec<f32> {
    let total = ((audio_secs * 16_000.0) as usize).clamp(160, 16_000);
    let mut out = Vec::with_capacity(total);
    for i in 0..total {
        let t = i as f64 / 16_000.0;
        let sample = 0.4 * (0.7 * (t * 43.0)).sin()
            + 0.3 * (0.3 * (t * 137.0)).sin()
            + 0.3 * (0.3 * (t * 89.0)).sin();
        out.push((sample * 0.25) as f32);
    }
    out
}

/// The engine version for this build: package version + short commit SHA.
/// The SHA is injected by build.rs (`VOXIX_BUILD_SHA`).
pub fn engine_version_string() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let sha = std::env::var("VOXIX_BUILD_SHA")
        .ok()
        .unwrap_or_else(|| "no-build-sha".to_string());
    if sha == "no-build-sha" {
        version.to_string()
    } else {
        format!("{version}+{sha}")
    }
}

/// Production adapter: reuses the existing managers, dereferencing through
/// the extracted RuntimeContext for settings.
pub struct ManagersEngine {
    model_manager: std::sync::Arc<ModelManager>,
    transcription_manager: std::sync::Arc<TranscriptionManager>,
    settings: std::sync::Arc<RuntimeContext>,
    /// Rolling worst-case observed realtime factor (audio_seconds / compute_seconds).
    max_observed_realtime_factor: std::sync::Mutex<Option<f64>>,
    /// Last-use timestamps per model (prewarm-run-once semantics, §15 G7:
    /// the load-time silence inference is skipped when the model ran within
    /// the last [`PREWARM_REPEAT_SECS`]).
    last_model_used: std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
}

/// Prewarm repeat window (§15 G7 prewarm-run-once): skip the silence
/// inference when the model ran within the last minute.
pub const PREWARM_REPEAT_MS: u128 = 60_000;

impl ManagersEngine {
    pub fn new(
        settings: std::sync::Arc<RuntimeContext>,
        model_manager: std::sync::Arc<ModelManager>,
        transcription_manager: std::sync::Arc<TranscriptionManager>,
    ) -> Self {
        ManagersEngine {
            model_manager,
            transcription_manager,
            settings,
            max_observed_realtime_factor: std::sync::Mutex::new(None),
            last_model_used: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn seen_recently(&self, model_id: &str) -> bool {
        self.last_model_used
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(model_id)
            .map(|t| std::time::Instant::now().duration_since(*t).as_millis())
            .unwrap_or(u128::MAX)
            < PREWARM_REPEAT_MS
    }

    /// Record a successful decode/prewarm of the model (usage marker).
    fn recorded_model_use(&self, model_id: &str) {
        self.last_model_used
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(model_id.to_string(), std::time::Instant::now());
    }
}

impl ManagersEngine {
    /// Real-time factor after a decode (audio_seconds / compute_seconds);
    /// observed factor is a max over the process lifetime.
    fn observed_max_realtime_factor(&self) -> Option<f64> {
        *self
            .max_observed_realtime_factor
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn record_realtime_factor(&self, audio_secs: f64, compute_secs: f64) {
        if compute_secs <= 0.0 {
            return;
        }
        let factor = audio_secs / compute_secs;
        let mut max = self
            .max_observed_realtime_factor
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *max = Some(max.map_or(factor, |m| m.max(factor)));
    }

    /// Single-shot decode of the full buffer with the model-use marker
    /// recorded on success (the pre-chunking behavior, unchanged).
    fn transcribe_and_mark(&self, audio: &[f32]) -> Result<String, String> {
        self.transcription_manager
            .transcribe(audio.to_vec())
            .map(|text| {
                self.recorded_model_use(
                    &self
                        .transcription_manager
                        .get_current_model()
                        .unwrap_or_default(),
                );
                text
            })
            .map_err(|e| format!("{e:#}"))
    }
}

impl EngineRuntime for ManagersEngine {
    fn engine_version(&self) -> String {
        engine_version_string()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    fn status(&self) -> EngineRuntimeStatus {
        let selected = self.settings.settings().selected_model;
        let selected = if selected.is_empty() {
            None
        } else {
            Some(selected)
        };
        let loaded = self.transcription_manager.is_model_loaded();
        let loading = self.transcription_manager.is_loading();
        let active_model = self.transcription_manager.get_current_model();
        let state = if loading {
            ModelState::Loading
        } else if loaded {
            ModelState::Loaded
        } else {
            ModelState::Unloaded
        };
        let backend = if loaded {
            self.transcription_manager.current_backend()
        } else {
            None
        };
        let loaded_model_id = active_model.clone();
        EngineRuntimeStatus {
            engine_version: engine_version_string(),
            selected_model_id: active_model.or(selected),
            model_state: state,
            backend,
            loaded_model_id,
            max_realtime_factor: self.observed_max_realtime_factor(),
        }
    }

    fn session_open(&self, _session_id: &str, model: Option<&str>) -> Result<(), String> {
        // M1: the lease is reserved; no model load is kicked. Lazy loading
        // arrives with the M2 stream handshake.
        let _ = self.model_manager.get_model_info(model.unwrap_or_default());
        Ok(())
    }

    fn session_close(&self, _session_id: &str, _finished: bool) {
        // Idle unload consults service leases in M2/M3; nothing to unload yet
        // beyond what the managers already track.
    }

    fn ensure_model_loaded(&self, model: Option<&str>) -> Result<EngineMediaFacts, String> {
        // Resolve the target model id (session snapshot → None means the
        // selected model; empty is the same default).
        let selected = self.settings.settings().selected_model;
        let target = model
            .filter(|m| !m.is_empty())
            .unwrap_or(&selected)
            .to_string();
        let needs_load = !self.transcription_manager.is_model_loaded()
            || self
                .transcription_manager
                .get_current_model()
                .as_deref()
                .map(|m| m != target.as_str())
                .unwrap_or(true);
        if needs_load {
            if target.is_empty() {
                return Err("no transcription model selected".to_string());
            }
            self.transcription_manager
                .load_model(&target)
                .map_err(|e| format!("model load failed: {e:#}"))?;
        }
        let loaded_model = self
            .transcription_manager
            .get_current_model()
            .unwrap_or_default();
        let backend = self.transcription_manager.current_backend();

        // The registry metadata was reconciled against the loaded GGUF at
        // load time, so a transcribe-cpp model that supports streaming is the
        // native path; every other backend stays final_only in v1.
        let info = self.model_manager.get_model_info(&loaded_model);
        let supports_streaming = info.map(|i| i.supports_streaming).unwrap_or(false);
        let streaming_mode: &'static str = if supports_streaming && backend.is_some() {
            "native"
        } else {
            "final_only"
        };

        // Benchmark-gated partials: ONNX engines run final-only in v1 (G1/G19
        // shipping order); the measured warmup realtime factor is recorded and
        // stays exposed via status for the future windowed adapter. Prewarm is
        // run-once (§15 G7): the silence inference is skipped when the model
        // ran within the last minute.
        let mut measured: Option<f64> = None;
        if streaming_mode == "final_only" && !self.seen_recently(&target) {
            let audio_secs = crate::service::engine::BENCHMARK_WARMUP_SECONDS;
            let buffer: Vec<f32> = crate::service::engine::benchmark_pcm(audio_secs);
            let started = std::time::Instant::now();
            self.transcription_manager
                .transcribe(buffer)
                .map_err(|e| format!("benchmark decode failed: {e:#}"))?;
            let elapsed = started.elapsed().as_secs_f64();
            measured = Some(if elapsed > 0.0 {
                audio_secs / elapsed
            } else {
                0.0
            });
            self.record_realtime_factor(audio_secs, elapsed);
            self.recorded_model_use(&target);
        }

        Ok(EngineMediaFacts {
            model_id: loaded_model,
            backend,
            streaming_mode,
            measured_realtime_factor: measured,
        })
    }

    fn stream_start(&self) -> Result<(), String> {
        self.transcription_manager.start_stream();
        Ok(())
    }

    fn stream_feed(&self, pcm: &[f32]) -> Result<(), String> {
        self.transcription_manager.stream_router().feed(pcm);
        Ok(())
    }

    fn stream_finalize(&self) -> Result<Option<String>, String> {
        self.transcription_manager
            .finalize_stream()
            .map_err(|e| format!("{e:#}"))
    }

    fn stream_cancel(&self) -> Result<(), String> {
        self.transcription_manager.cancel_stream();
        Ok(())
    }

    fn batch_transcribe(&self, audio: &[f32]) -> Result<String, String> {
        // Latency path — treat a batch POST as demand at decode time (parity
        // with the WS session handshake). G7 is preserved: preparation never
        // *downloads*; it loads only a locally installed model.
        self.ensure_model_loaded(None)
            .map_err(|e| format!("{e:#}"))?;
        let bounds = crate::service::chunk::chunk_bounds(audio);
        if bounds.len() <= 1 {
            // Short audio: unchanged single-shot behavior.
            return self.transcribe_and_mark(audio);
        }
        // Long audio: the model degrades into repetition loops on >~40–60 s
        // inputs, so decode bounded chunks sequentially and join. Any chunk
        // failure fails the whole call (same failure semantics as today, so
        // the service persists the WAV and writes the Retry-able history row).
        log::info!(
            target: "service::engine",
            "batch audio {:.1}s exceeds {}s cap; transcribing in {} chunks",
            audio.len() as f64 / crate::service::chunk::SAMPLE_RATE_HZ as f64,
            crate::service::chunk::MAX_CHUNK_SAMPLES / crate::service::chunk::SAMPLE_RATE_HZ,
            bounds.len()
        );
        let total = bounds.len();
        let mut texts: Vec<String> = Vec::with_capacity(total);
        for (i, &(start, end)) in bounds.iter().enumerate() {
            log::info!(
                target: "service::engine",
                "chunk {}/{} start={:.1}s len={:.1}s",
                i + 1,
                total,
                start as f64 / crate::service::chunk::SAMPLE_RATE_HZ as f64,
                (end - start) as f64 / crate::service::chunk::SAMPLE_RATE_HZ as f64
            );
            let text = self
                .transcription_manager
                .transcribe(audio[start..end].to_vec())
                .map_err(|e| format!("chunk {}/{} failed: {e:#}", i + 1, total))?;
            texts.push(text.trim().to_string());
        }
        self.recorded_model_use(
            &self
                .transcription_manager
                .get_current_model()
                .unwrap_or_default(),
        );
        Ok(texts
            .into_iter()
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string())
    }

    fn negotiated_streaming_mode(&self, model: Option<&str>) -> &'static str {
        let selected = self.settings.settings().selected_model;
        let target = model.filter(|m| !m.is_empty()).unwrap_or(&selected);
        let Some(info) = self.model_manager.get_model_info(target) else {
            return "final_only";
        };
        if matches!(
            info.engine_type,
            crate::managers::model::EngineType::TranscribeCpp
        ) && info.supports_streaming
        {
            "native"
        } else {
            "final_only"
        }
    }

    fn observed_realtime_factor(&self) -> Option<f64> {
        self.observed_max_realtime_factor()
    }

    fn max_realtime_factor(&self) -> Option<f64> {
        self.observed_max_realtime_factor()
    }

    fn list_models(&self) -> Vec<crate::managers::model::ModelInfo> {
        self.model_manager.get_available_models()
    }

    fn model_info(&self, model_id: &str) -> Option<crate::managers::model::ModelInfo> {
        self.model_manager.get_model_info(model_id)
    }

    fn selected_model(&self) -> Option<String> {
        let selected = self.settings.settings().selected_model;
        (!selected.is_empty()).then_some(selected)
    }

    fn model_ready(&self, model: Option<&str>) -> bool {
        let selected = self.settings.settings().selected_model;
        let target = model
            .filter(|m| !m.is_empty())
            .unwrap_or(&selected)
            .to_string();
        if target.is_empty() {
            return false;
        }
        if !self.transcription_manager.is_model_loaded() {
            return false;
        }
        self.transcription_manager
            .get_current_model()
            .as_deref()
            .map(|m| m == target.as_str())
            .unwrap_or(false)
    }

    fn refresh_model_status(&self) {
        if let Err(e) = self.model_manager.update_download_status() {
            log::warn!(target: "service::engine", "model status refresh failed: {e}");
        }
    }

    fn download_model_start(&self, model_id: &str) -> Result<(), String> {
        let Some(info) = self.model_manager.get_model_info(model_id) else {
            return Err("model_not_found".to_string());
        };
        if matches!(info.source, crate::managers::model::ModelSource::Local) {
            return Err("model has no download source".to_string());
        }
        // Idempotent: already installed or already in flight converges on the
        // single download; never queues. The cancellation-token registry is
        // authoritative — the `is_downloading` flag is only a UI hint and
        // refreshes clear it.
        if info.is_downloaded
            || info.is_downloading
            || self.model_manager.download_in_flight(model_id)
        {
            return Ok(());
        }
        let manager = self.model_manager.clone();
        let id = model_id.to_string();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    if let Err(e) = manager.download_model(&id).await {
                        // Completion is observed via status polling /
                        // `model-download-complete`; spawned-task errors only log.
                        log::error!(target: "service::engine", "managed download of {id} failed: {e}");
                    }
                });
                Ok(())
            }
            Err(_) => {
                // No ambient runtime: a dedicated current-thread runtime drives
                // the download on a helper thread.
                std::thread::spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build();
                    match runtime {
                        Ok(runtime) => {
                            if let Err(e) = runtime.block_on(manager.download_model(&id)) {
                                log::error!(target: "service::engine", "managed download of {id} failed: {e}");
                            }
                        }
                        Err(e) => {
                            log::error!(target: "service::engine", "managed download runtime build failed: {e}");
                        }
                    }
                });
                Ok(())
            }
        }
    }

    fn cancel_model_download(&self, model_id: &str) -> Result<(), String> {
        let Some(_info) = self.model_manager.get_model_info(model_id) else {
            return Err("model_not_found".to_string());
        };
        if !self.model_manager.download_in_flight(model_id) {
            return Err(format!(
                "nothing_to_cancel: no download in flight for model '{model_id}'"
            ));
        }
        self.model_manager
            .cancel_download(model_id)
            .map_err(|e| format!("{e:#}"))
    }

    fn model_select(
        &self,
        model_id: &str,
        eager: bool,
    ) -> Result<crate::service::engine::ModelSelectionOutcome, String> {
        let info = self
            .model_manager
            .get_model_info(model_id)
            .ok_or_else(|| format!("model not found: {model_id}"))?;
        if !info.is_downloaded {
            return Err("model_not_ready: model is not installed locally".to_string());
        }
        let previous = self.settings.settings().selected_model;
        if previous != model_id {
            let mut updated = self.settings.settings();
            updated.selected_model = model_id.to_string();
            self.settings.write_settings(&updated);
        }
        let outcome = crate::service::engine::ModelSelectionOutcome {
            model_id: model_id.to_string(),
            loaded: previous == model_id && self.transcription_manager.is_model_loaded(),
        };
        if !eager {
            return Ok(outcome);
        }
        // Eager load through the single loader. On failure the persisted
        // selection reverts so the previous warm runtime is untouched
        // (artifact publication ≠ runtime activation, §15 G7).
        match self.ensure_model_loaded(Some(model_id)) {
            Ok(_) => Ok(crate::service::engine::ModelSelectionOutcome {
                model_id: model_id.to_string(),
                loaded: true,
            }),
            Err(e) => {
                if previous != model_id {
                    let mut revert = self.settings.settings();
                    revert.selected_model = previous;
                    self.settings.write_settings(&revert);
                }
                Err(e)
            }
        }
    }
}

/// Test adapter: deterministic in-memory engine with fixed capabilities and
/// version, no manager construction, no model artifacts. Session openings are
/// recorded so integration tests can assert close/release semantics. M2 adds
/// the scripted stream/batch surface (final-only turns, final text, engine
/// busy refusals, partial emission for backpressure flows).
pub struct TestEngine {
    inner: std::sync::Mutex<TestEngineInner>,
    text_bus: tokio::sync::broadcast::Sender<StreamTextFrame>,
}

#[derive(Default)]
struct TestEngineInner {
    opened: Vec<String>,
    closed: Vec<(String, bool)>,
    open_error: Option<String>,
    /// Whether feeds are being emulated as streaming (partial-emitting).
    streaming: bool,
    /// Deterministic slow-decode emulation for intake-overflow tests: each
    /// stream_feed sleeps this long (outside the lock).
    feed_delay: Option<std::time::Duration>,
    /// Final text returned by batch_transcribe / stream_finalize.
    final_text: String,
    /// Finalize/batch/emit call counters.
    finalized: u64,
    batch_decodes: u64,
    /// Feed counter (audio accepted through the streaming channel).
    feeds: u64,
    /// Last accepted audio length in samples.
    last_audio_samples: usize,
    /// M3: emulate the loaded runtime + a scripted load failure.
    loaded: bool,
    last_loaded_model: Option<String>,
    load_error: Option<String>,
    /// M3: the deterministic model catalog this test engine serves.
    models: Vec<crate::managers::model::ModelInfo>,
    /// M3: sleep (ms) inside `ensure_model_loaded` before returning Ok —
    /// makes the prepare single-flight CAS observable.
    load_hold_ms: Option<u64>,
}

/// The deterministic "test-model" for the in-memory adapter: a locally
/// installed transcribe-cpp entry so model routes/prepare flows exercise the
/// service logic without artifacts.
fn test_model_info(id: &str) -> crate::managers::model::ModelInfo {
    crate::managers::model::ModelInfo {
        id: id.to_string(),
        name: format!("Test {id}"),
        description: "deterministic test model".to_string(),
        filename: "fixture.gguf".to_string(),
        source: crate::managers::model::ModelSource::Local,
        size_mb: 100,
        is_downloaded: true,
        is_downloading: false,
        partial_size: 0,
        is_directory: false,
        engine_type: crate::managers::model::EngineType::TranscribeCpp,
        accuracy_score: 0.5,
        speed_score: 0.5,
        supports_translation: false,
        is_recommended: false,
        supported_languages: vec!["en".to_string()],
        supports_language_selection: true,
        is_custom: false,
        supports_streaming: false,
        supports_language_detection: true,
        quant_files: Vec::new(),
    }
}

impl TestEngine {
    pub fn new(text_bus: tokio::sync::broadcast::Sender<StreamTextFrame>) -> Self {
        TestEngine {
            inner: std::sync::Mutex::new(TestEngineInner::default()),
            text_bus,
        }
    }

    /// Force `session_open` errors (used to deterministically assert the
    /// service_busy envelope when the engine refuses).
    pub fn set_open_error(&self, error: Option<String>) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .open_error = error;
    }

    pub fn snapshot(&self) -> (Vec<String>, Vec<(String, bool)>) {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        (inner.opened.clone(), inner.closed.clone())
    }

    pub fn set_streaming(&self, streaming: bool) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .streaming = streaming;
    }

    /// Make `stream_feed` block for the given duration (deterministic
    /// slow-decode emulation for the bounded-intake overflow tests).
    pub fn set_feed_delay(&self, delay: Option<std::time::Duration>) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .feed_delay = delay;
    }

    pub fn set_final_text(&self, text: impl Into<String>) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .final_text = text.into();
    }

    pub fn final_text(&self) -> String {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .final_text
            .clone()
    }

    pub fn finalize_count(&self) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .finalized
    }

    pub fn batch_decode_count(&self) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .batch_decodes
    }

    pub fn feed_count(&self) -> u64 {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).feeds
    }

    /// Emit a stream partial through the text bus (native-mode emulation).
    pub fn emit_partial(&self, committed: impl Into<String>, tentative: impl Into<String>) {
        let _ = self.text_bus.send(crate::service::engine::StreamTextFrame {
            committed: committed.into(),
            tentative: tentative.into(),
        });
    }

    /// Register a deterministic catalog model for the M3 model-surface tests:
    /// a locally installed transcribe-cpp entry (`is_downloaded == false`
    /// skips the install marker to exercise not-ready/prepare-refusal paths).
    pub fn insert_test_model(&self, id: &str, is_downloaded: bool) {
        let mut model = test_model_info(id);
        model.is_downloaded = is_downloaded;
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .models
            .push(model);
    }

    /// Script a model-load failure for `ensure_model_loaded`/`model_select`.
    pub fn set_load_error(&self, error: Option<String>) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .load_error = error;
    }

    /// Script a delay inside the deterministic load before it resolves Ok
    /// (makes the prepare single-flight CAS observable in tests).
    pub fn set_load_hold(&self, millis: Option<u64>) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .load_hold_ms = millis;
    }

    /// Whether the runtime currently holds a loaded model (M3).
    pub fn runtime_loaded(&self) -> bool {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).loaded
    }

    /// The id of the loaded runtime model (M3).
    pub fn runtime_model(&self) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_loaded_model
            .clone()
    }
}

impl Default for TestEngine {
    fn default() -> Self {
        // A private bus keeps `TestEngine::default()` self-contained; tests
        // that need to observe partials construct with the state-owned bus.
        let (tx, _rx) = tokio::sync::broadcast::channel(64);
        TestEngine::new(tx)
    }
}

impl EngineRuntime for TestEngine {
    fn engine_version(&self) -> String {
        "1.0.0+testsha".to_string()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    fn status(&self) -> EngineRuntimeStatus {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        EngineRuntimeStatus {
            engine_version: self.engine_version(),
            selected_model_id: Some("test-model".to_string()),
            model_state: if inner.loaded {
                ModelState::Loaded
            } else {
                ModelState::Unloaded
            },
            backend: Some("test".to_string()),
            loaded_model_id: inner.loaded.then(|| "test-model".to_string()),
            max_realtime_factor: None,
        }
    }

    fn session_open(&self, session_id: &str, _model: Option<&str>) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(err) = inner.open_error.clone() {
            return Err(err);
        }
        inner.opened.push(session_id.to_string());
        Ok(())
    }

    fn session_close(&self, session_id: &str, finished: bool) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .closed
            .push((session_id.to_string(), finished));
    }

    fn ensure_model_loaded(&self, model: Option<&str>) -> Result<EngineMediaFacts, String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let loaded_model = model
            .filter(|m| !m.is_empty())
            .map(|m| m.to_string())
            .or_else(|| inner.last_loaded_model.clone())
            .unwrap_or_else(|| "test-model".to_string());
        if let Some(error) = inner.load_error.clone() {
            return Err(error);
        }
        if let Some(ms) = inner.load_hold_ms {
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
        inner.loaded = true;
        inner.last_loaded_model = Some(loaded_model.clone());
        Ok(EngineMediaFacts {
            model_id: loaded_model,
            backend: Some("test".to_string()),
            streaming_mode: if inner.streaming {
                "native"
            } else {
                "final_only"
            },
            measured_realtime_factor: None,
        })
    }

    fn stream_start(&self) -> Result<(), String> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.open_error.is_some() {
            return Err(inner.open_error.clone().unwrap_or_default());
        }
        Ok(())
    }

    fn stream_feed(&self, pcm: &[f32]) -> Result<(), String> {
        let delay = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.feed_delay
        };
        if let Some(delay) = delay {
            std::thread::sleep(delay);
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.feeds += 1;
        inner.last_audio_samples = pcm.len();
        Ok(())
    }

    fn stream_finalize(&self) -> Result<Option<String>, String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.finalized += 1;
        Ok(Some(inner.final_text.clone()))
    }

    fn stream_cancel(&self) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.finalized += 1;
        Ok(())
    }

    fn batch_transcribe(&self, _audio: &[f32]) -> Result<String, String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.batch_decodes += 1;
        Ok(inner.final_text.clone())
    }

    fn list_models(&self) -> Vec<crate::managers::model::ModelInfo> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.models.clone()
    }

    fn model_info(&self, model_id: &str) -> Option<crate::managers::model::ModelInfo> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.models.iter().find(|m| m.id == model_id).cloned()
    }

    fn selected_model(&self) -> Option<String> {
        Some("test-model".to_string())
    }

    fn model_ready(&self, model: Option<&str>) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if !inner.loaded {
            return false;
        }
        match model.filter(|m| !m.is_empty()) {
            Some(m) => inner.last_loaded_model.as_deref() == Some(m),
            None => true,
        }
    }

    fn model_select(
        &self,
        model_id: &str,
        eager: bool,
    ) -> Result<crate::service::engine::ModelSelectionOutcome, String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(error) = inner.load_error.clone() {
            return Err(error);
        }
        if eager {
            inner.loaded = true;
            inner.last_loaded_model = Some(model_id.to_string());
        }
        Ok(crate::service::engine::ModelSelectionOutcome {
            model_id: model_id.to_string(),
            loaded: eager,
        })
    }

    fn negotiated_streaming_mode(&self, _model: Option<&str>) -> &'static str {
        if self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .streaming
        {
            "native"
        } else {
            "final_only"
        }
    }

    fn observed_realtime_factor(&self) -> Option<f64> {
        None
    }

    fn max_realtime_factor(&self) -> Option<f64> {
        None
    }
}

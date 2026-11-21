//! Serde DTOs and the standard error envelope (SDK plan §4.7 / §5).

use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

/// Error envelope carried by every non-2xx `/v1` response.
/// Shape: `{error:{code,message,retryable,request_id,details?}}`. Never
/// expose paths, transcript text, tokens, or panic strings.
#[derive(Serialize, Debug, Clone)]
pub struct ApiError {
    pub error: ErrorBody,
}

#[derive(Serialize, Debug, Clone)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl ApiError {
    pub fn new(code: &str, message: &str, retryable: bool, request_id: &str) -> Self {
        ApiError {
            error: ErrorBody {
                code: code.to_string(),
                message: message.to_string(),
                retryable,
                request_id: request_id.to_string(),
                details: None,
            },
        }
    }

    /// The engine is contended (one-inference lease held). Retryable.
    pub fn service_busy(request_id: &str) -> Self {
        ApiError::new(
            "service_busy",
            "One live inference lease is already held; retry shortly",
            true,
            request_id,
        )
    }

    /// Unknown session or wrong/missing per-session sub-credential. A wrong
    /// session token answers 404 (never 403) so ids cannot be probed.
    pub fn session_not_found(request_id: &str) -> Self {
        ApiError::new("session_not_found", "No such session", false, request_id)
    }

    pub fn invalid_request(code: &str, message: &str, request_id: &str) -> Self {
        ApiError::new(code, message, false, request_id)
    }

    pub fn internal(request_id: &str) -> Self {
        ApiError::new("internal_error", "Internal server error", false, request_id)
    }

    /// HTTP status for this error in the sessions/REST surface.
    pub fn status(&self) -> (u16, Option<u64>) {
        match self.error.code.as_str() {
            "service_busy" => (503, Some(Duration::from_secs(1).as_secs())),
            "session_not_found" => (404, None),
            _ => (500, None),
        }
    }
}

/// `GET /v1/health` (SDK plan §5, with `engine_version` handoff — §15 G6/G21).
#[derive(Serialize, Debug, Clone)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: String,
    pub engine_version: String,
    pub protocol_version: u32,
    pub uptime_ms: u64,
    pub ready: bool,
}

/// `GET /v1/status` capability vocabulary (§15 S6/S8): explicit values, never
/// omitted keys.
#[derive(Serialize, Debug, Clone)]
pub struct Capabilities {
    pub requires_server_vad: bool,
    pub prefers_auto_gain: bool,
    pub prefers_noise_reduction: bool,
    pub max_concurrent_streams: u32,
}

impl Default for Capabilities {
    fn default() -> Self {
        Capabilities {
            requires_server_vad: false,
            prefers_auto_gain: false,
            prefers_noise_reduction: false,
            max_concurrent_streams: 1,
        }
    }
}

/// `GET /v1/status` snapshot.
#[derive(Serialize, Debug, Clone)]
pub struct StatusResponse {
    pub mode: &'static str,
    pub bind: String,
    pub active_model_id: Option<String>,
    pub model_state: String,
    pub backend: Option<String>,
    pub active_session_id: Option<String>,
    pub active_sessions: u32,
    pub max_realtime_factor: Option<f64>,
    pub lease_held: bool,
    pub capabilities: Capabilities,
}

/// `POST /v1/sessions` request — §5 body. Omitted model snapshots the
/// selected model; omitted language is "auto"; the audio/vad blocks default
/// as documented. Unknown fields stay 422 (deny_unknown_fields).
#[derive(serde::Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct CreateSessionRequest {
    /// Model id to snapshot for this session. Omit/empty for the selected
    /// model. Accepted as `model` (alias) or the canonical `model_id`.
    #[serde(default, alias = "model")]
    pub model_id: Option<String>,
    /// Language hint; defaults to "auto".
    #[serde(default)]
    pub language: Option<String>,
    /// Task; only "transcribe" is supported in v1 (defaults to it).
    #[serde(default)]
    pub task: Option<String>,
    /// Audio format request; only the negotiated defaults are supported in
    /// v1 (16 kHz mono pcm_s16le), validated before allocation.
    #[serde(default)]
    pub audio: Option<AudioRequest>,
    /// VAD configuration (§3.4). Defaults to server mode with no auto stop.
    #[serde(default)]
    pub vad: Option<VadRequest>,
    /// Whether the session wants partial events (default true).
    #[serde(default)]
    pub partial_results: Option<bool>,
    /// Whether the session's transcript may be saved to history (default
    /// follows the service save-history default; history writing ships M3).
    #[serde(default)]
    pub save_history: Option<bool>,
    /// Idempotency key: a duplicate within 10 minutes returns the existing
    /// nonterminal session instead of allocating a new one.
    #[serde(default)]
    pub client_session_key: Option<String>,
}

/// `POST /v1/sessions` `audio` block (§5). All four fields must match the
/// v1 profile or the create is refused 422 before any allocation.
#[derive(serde::Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct AudioRequest {
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub sample_rate_hz: Option<u32>,
    #[serde(default)]
    pub channels: Option<u8>,
    #[serde(default)]
    pub chunk_ms: Option<u32>,
}

/// `POST /v1/sessions` `vad` block (§3.4).
#[derive(serde::Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct VadRequest {
    /// `server` (default) | `client` (speech-only input, server VAD bypassed)
    /// | `disabled`.
    #[serde(default)]
    pub mode: Option<String>,
    /// Silence milliseconds after speech that triggers an automatic finalize.
    /// None means boundaries only (no auto stop).
    #[serde(default)]
    pub auto_finalize_ms: Option<u64>,
}

/// Effective VAD configuration for a session after validation/defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadMode {
    Server,
    Client,
    Disabled,
}

impl VadMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            VadMode::Server => "server",
            VadMode::Client => "client",
            VadMode::Disabled => "disabled",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VadConfig {
    pub mode: VadMode,
    pub auto_finalize_ms: Option<u64>,
}

#[derive(Serialize, Debug, Clone)]
pub struct NegotiatedAudio {
    pub codec: &'static str,
    pub sample_rate_hz: u32,
    pub channels: u8,
    pub chunk_ms: u32,
}

/// `POST /v1/sessions` 201 response (§5): carries the per-session
/// `session_token` sub-credential (§15 G9/G14).
#[derive(Serialize, Debug, Clone)]
pub struct CreatedSessionResponse {
    pub session_id: String,
    pub session_token: String,
    pub state: &'static str,
    pub ws_url: String,
    pub expires_at: String,
    pub negotiated_audio: NegotiatedAudio,
    /// Mode negotiated for the snapshot model: `native` (transcribe-cpp
    /// streaming), `windowed_batch` (future adapter), or `final_only`.
    pub streaming_mode: String,
    pub resume_window_ms: u64,
}

/// `GET /v1/sessions/{id}` projection.
#[derive(Serialize, Debug, Clone)]
pub struct SessionResponse {
    pub session_id: String,
    pub state: String,
    pub model_id: Option<String>,
    pub language: String,
    pub audio_received_ms: u64,
    pub last_audio_seq: u64,
    pub last_event_seq: u64,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

/// `PATCH /v1/sessions/{id}` request (M1 phase-complete surface).
#[derive(serde::Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct PatchSessionRequest {
    /// `completed` finishes the session and releases its engine lease.
    pub state: String,
}

/// `GET /v1/config` projection (no secrets).
#[derive(Serialize, Debug, Clone)]
pub struct ConfigResponse {
    pub bind: String,
    pub max_sessions: u32,
    pub max_audio_seconds: u64,
    pub session_resume_window_ms: u64,
    pub save_history_default: bool,
    pub vad_enabled_default: bool,
    pub restart_required: bool,
}

// ---------------------------------------------------------------------------
// M3 model surface (SDK plan §5 Models + §15 G7).
// ---------------------------------------------------------------------------

/// Per-model download status (`GET /v1/models` adds download status; never a
/// URL or a fabricated managed-download state — the service downloads nothing).
#[derive(Serialize, Debug, Clone)]
pub struct ModelDownloadStatus {
    /// "not_started" | "downloading" | "completed" (the remote download-admin
    /// routes are out of M3 scope: owner-blocked host data).
    pub state: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percentage: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Safe `ModelInfo` mirror (§5: "mirror safe ModelInfo, omit source URL/path,
/// add download status").
#[derive(Serialize, Debug, Clone)]
pub struct ModelView {
    pub id: String,
    pub name: String,
    pub description: String,
    pub filename: String,
    /// Neutral source marker: "download" (remote-listed) or "local" (on-disk).
    pub source_kind: String,
    pub size_mb: u64,
    pub is_downloaded: bool,
    pub is_downloading: bool,
    pub partial_size: u64,
    pub is_directory: bool,
    /// Snake_case engine family label, e.g. `transcribe_cpp`.
    pub engine_type: String,
    pub accuracy_score: f32,
    pub speed_score: f32,
    pub supports_translation: bool,
    pub is_recommended: bool,
    pub supported_languages: Vec<String>,
    pub supports_language_selection: bool,
    pub is_custom: bool,
    pub supports_streaming: bool,
    pub supports_language_detection: bool,
    /// Convenience alias for the installed state (§5 `?installed=true` filter).
    pub installed: bool,
    pub download: ModelDownloadStatus,
}

/// `GET /v1/models` response (§5).
#[derive(Serialize, Debug, Clone)]
pub struct ModelsResponse {
    pub models: Vec<ModelView>,
    pub selected_model_id: Option<String>,
    pub loaded_model_id: Option<String>,
}

/// `GET /v1/models/{id}` response: `{model, prepare}` (additive over `model`).
#[derive(Serialize, Debug, Clone)]
pub struct SingleModelResponse {
    pub model: ModelView,
    /// Live prepare stage projection for phone-side polling.
    pub prepare: serde_json::Value,
}

/// `PUT /v1/models/selected` request (§5).
#[derive(serde::Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct SelectedModelRequest {
    pub model_id: String,
    /// "eager" loads now; "lazy" only updates the selection (loads on next
    /// session start / batch admission). Defaults to "eager" when omitted.
    #[serde(default)]
    pub load: Option<String>,
}

/// Prepare stage vocabulary (§15 G7 SSE: `downloading → verifying → loading
/// → ok`, or one terminal `failed` for polling). The download stage is
/// unreachable for the service itself — the catalog download data files are
/// owner-blocked and the service downloads nothing, so the service-visible
/// entry point is `verifying`; `Downloading` exists to keep the vocabulary
/// complete for future owned-upload managed-download work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepareStage {
    NotStarted,
    Downloading,
    Verifying,
    Loading,
    Ok,
    Failed(String),
}

impl PrepareStage {
    pub fn as_str(&self) -> &str {
        match self {
            PrepareStage::NotStarted => "not_started",
            PrepareStage::Downloading => "downloading",
            PrepareStage::Verifying => "verifying",
            PrepareStage::Loading => "loading",
            PrepareStage::Ok => "ok",
            PrepareStage::Failed(_) => "failed",
        }
    }
}

// ---------------------------------------------------------------------------
// M3 batch transcription surface (SDK plan §7 / §15 G11).
// ---------------------------------------------------------------------------

/// `POST /v1/transcriptions` 202 response (§7).
#[derive(Serialize, Debug, Clone)]
pub struct BatchJobResponse {
    pub job_id: String,
    pub state: &'static str,
    pub status_url: String,
}

/// `GET /v1/transcriptions/{job}` response.
#[derive(Serialize, Debug, Clone)]
pub struct BatchJobStatus {
    pub job_id: String,
    /// "processing" | "completed" | "failed" | "cancelled"
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<BatchResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<BatchJobError>,
}

/// Successful batch result.
#[derive(Serialize, Debug, Clone)]
pub struct BatchResult {
    pub transcript: String,
    pub duration_ms: u64,
}

/// Batch job error envelope (matching the WS error payload shape).
#[derive(Serialize, Debug, Clone)]
pub struct BatchJobError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// M3 auth surface (SDK plan §4 / §15 G9).
// ---------------------------------------------------------------------------

/// `POST /v1/auth/tokens` request.
#[derive(serde::Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct CreateTokenRequest {
    /// Display name for the token.
    #[serde(default)]
    pub name: Option<String>,
    /// If true, rotate the primary token: the existing verifier is replaced
    /// and all extant WS connections on the old token receive 4001.
    #[serde(default)]
    pub rotate_primary: Option<bool>,
}

/// `POST /v1/auth/tokens` 201 response.
#[derive(Serialize, Debug, Clone)]
pub struct CreatedTokenResponse {
    pub id: String,
    pub name: String,
    pub token: String,
    pub prefix: String,
    pub created_at: String,
}

/// `POST /v1/auth/ws-ticket` request.
#[derive(serde::Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct CreateTicketRequest {
    /// If present, bind the ticket to this session's credential scope.
    #[serde(default)]
    pub session_id: Option<String>,
}

/// `POST /v1/auth/ws-ticket` 201 response.
#[derive(Serialize, Debug, Clone)]
pub struct CreatedTicketResponse {
    pub ticket: String,
    pub expires_at: String,
}

// ---------------------------------------------------------------------------
// M3 config surface (SDK plan §3.1 / §15).
// ---------------------------------------------------------------------------

/// `PATCH /v1/config` request (mutable set only; unknown fields → 422).
#[derive(serde::Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct PatchConfigRequest {
    #[serde(default)]
    pub allowed_origins: Option<Vec<String>>,
    /// 1–64.
    #[serde(default)]
    pub max_sessions: Option<u32>,
    /// 1–7200 seconds.
    #[serde(default)]
    pub max_audio_seconds: Option<u64>,
    /// 0–120000 ms (0 = unlimited).
    #[serde(default)]
    pub session_resume_window_ms: Option<u64>,
    /// 0 = disabled; ≤ 3_600_000 ms.
    #[serde(default)]
    pub pause_max_ms: Option<u64>,
    /// 0 = disabled; ≤ 3_600_000 ms.
    #[serde(default)]
    pub save_history_default: Option<bool>,
    /// 0 = disabled; ≤ 3_600_000 ms.
    #[serde(default)]
    pub vad_enabled_default: Option<bool>,
    /// Opt-in Opus codec for the v1 audio plane (SDK plan §6; default off).
    /// Only accepted when the `service-opus` feature is compiled into the
    /// running binary — enabling without it answers 422.
    #[serde(default)]
    pub opus_enabled: Option<bool>,
}

/// Extended `GET /v1/config` projection including M3 fields.
#[derive(Serialize, Debug, Clone)]
pub struct ConfigResponseV2 {
    pub bind: String,
    pub allowed_origins: Vec<String>,
    pub max_sessions: u32,
    pub max_audio_seconds: u64,
    pub session_resume_window_ms: u64,
    pub pause_max_ms: u64,
    pub save_history_default: bool,
    pub vad_enabled_default: bool,
    pub opus_enabled: bool,
    pub restart_required: bool,
}

// ---------------------------------------------------------------------------
// M3 history surface (SDK plan §12).
// ---------------------------------------------------------------------------

/// One history entry returned by `GET /v1/history`.
#[derive(Serialize, Debug, Clone)]
pub struct ServiceHistoryEntry {
    pub id: i64,
    pub timestamp_ms: i64,
    pub title: String,
    pub transcription_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// "batch" | "stream"
    pub source: String,
    /// Display name of the client that submitted this transcription.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_label: Option<String>,
    /// Emoji, unicode character, or image URL for the client's icon.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_icon: Option<String>,
}

/// `GET /v1/history` paginated response.
#[derive(Serialize, Debug, Clone)]
pub struct ServiceHistoryPage {
    pub entries: Vec<ServiceHistoryEntry>,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<i64>,
}

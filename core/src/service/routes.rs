//! Axum router and REST handlers for the M1 control plane (SDK plan §5).
//!
//! Auth-before-dispatch: every `/v1` route sits behind bearer verification
//! middleware. Rejected calls carry an **empty 401** body plus a throttled
//! auth-failure observation log. `GET /alive` is the only bare route: an
//! unauthenticated liveness check for local supervisors/proxies.
//!
//! Session routes additionally require the per-session `X-Session-Token`
//! sub-credential; wrong/missing answers 404 (never 403), identical to an
//! unknown session id.

use crate::service::api::{
    ApiError, AudioRequest, ConfigResponseV2, CreateSessionRequest, CreateTicketRequest,
    CreateTokenRequest, CreatedSessionResponse, HealthResponse, NegotiatedAudio,
    PatchConfigRequest, PatchSessionRequest, ServiceHistoryPage, SessionResponse, VadConfig,
    VadMode, VadRequest,
};
use crate::service::state::{LeaseGuard, ServiceState, SessionState, SessionStateName};
use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, header::AUTHORIZATION, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use serde_json::json;
use std::sync::Arc;

/// Query parameters for GET /v1/history.
#[derive(serde::Deserialize, Default)]
pub struct HistoryQuery {
    pub cursor: Option<i64>,
    pub limit: Option<usize>,
}
/// POST /v1/sessions negotiated-audio defaults (SDK plan §5 row).
pub const DEFAULT_NEGOTIATED_AUDIO: NegotiatedAudio = NegotiatedAudio {
    codec: "pcm_s16le",
    sample_rate_hz: 16_000,
    channels: 1,
    chunk_ms: 20,
};

/// Live-apply a PATCHed `opus_enabled` value on `PATCH /v1/config`. With the
/// `service-opus` feature compiled in, both directions apply; enabling without
/// the feature answers 422 (`opus codec is not compiled into this build`).
#[cfg(feature = "service-opus")]
fn apply_opus_enabled_patch(
    patch: &mut crate::service::state::PatchableConfig,
    v: bool,
) -> Result<(), String> {
    patch.opus_enabled = v;
    Ok(())
}

#[cfg(not(feature = "service-opus"))]
fn apply_opus_enabled_patch(
    _patch: &mut crate::service::state::PatchableConfig,
    v: bool,
) -> Result<(), String> {
    if v {
        Err("the opus codec is not compiled into this build".to_string())
    } else {
        Ok(())
    }
}

/// Validate the §5 audio block: only the v1 profile is supported (16 kHz mono
/// pcm_s16le; chunk 10/20/30/40/60 ms). Unsupported values are 422 before any
/// allocation. The optional Opus codec (`opus_enabled`, default off) accepts
/// the same profile at codec `opus` instead of `pcm_s16le`.
fn validate_audio(
    request: Option<&AudioRequest>,
    opus_enabled: bool,
) -> Result<NegotiatedAudio, String> {
    let Some(audio) = request else {
        return Ok(DEFAULT_NEGOTIATED_AUDIO);
    };
    if let Some(codec) = &audio.codec {
        let supported = if opus_enabled {
            "pcm_s16le|opus"
        } else {
            "pcm_s16le"
        };
        if codec != "pcm_s16le" && !(codec == "opus" && opus_enabled) {
            return Err(format!(
                "unsupported audio codec '{codec}' (only {supported} in v1)"
            ));
        }
    }
    if let Some(hrz) = audio.sample_rate_hz {
        if hrz != 16_000 {
            return Err(format!(
                "unsupported sample rate {hrz} Hz (only 16000 in v1)"
            ));
        }
    }
    if let Some(channels) = audio.channels {
        if channels != 1 {
            return Err(format!(
                "unsupported channel count {channels} (mono only in v1)"
            ));
        }
    }
    if let Some(chunk) = audio.chunk_ms {
        if !matches!(chunk, 10 | 20 | 30 | 40 | 60) {
            return Err(format!(
                "unsupported chunk size {chunk} ms (10/20/30/40/60 only)"
            ));
        }
    }
    let codec = if opus_enabled && audio.codec.as_deref() == Some("opus") {
        "opus"
    } else {
        "pcm_s16le"
    };
    Ok(NegotiatedAudio {
        codec,
        sample_rate_hz: 16_000,
        channels: 1,
        chunk_ms: 20,
    })
}

/// Validate the §3.4 VAD block. `auto_finalize_ms` requires server VAD (the
/// boundaries it counts do not exist in client/disabled modes).
fn validate_vad(request: Option<&VadRequest>) -> Result<VadConfig, String> {
    let Some(vad) = request else {
        return Ok(VadConfig {
            mode: VadMode::Server,
            auto_finalize_ms: None,
        });
    };
    let mode = match vad.mode.as_deref().unwrap_or("server") {
        "server" => VadMode::Server,
        "client" => VadMode::Client,
        "disabled" => VadMode::Disabled,
        other => return Err(format!("unknown vad mode '{other}'")),
    };
    let auto = vad.auto_finalize_ms;
    if auto.is_some() && mode != VadMode::Server {
        return Err("auto_finalize_ms requires vad.mode = 'server'".to_string());
    }
    Ok(VadConfig {
        mode,
        auto_finalize_ms: auto,
    })
}

const RETRY_AFTER_ADMISSION: &str = "30";
const RETRY_AFTER_BUSY: &str = "1";

pub fn router(state: Arc<ServiceState>) -> Router {
    let api = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/status", get(status))
        .route("/v1/sessions", post(create_session))
        .route(
            "/v1/sessions/{id}",
            get(get_session).patch(patch_session).delete(delete_session),
        )
        .route(
            "/v1/sessions/{id}/stream",
            get(crate::service::ws::ws_connect),
        )
        .route("/v1/config", get(get_config).patch(patch_config))
        .route("/v1/models", get(crate::service::models::get_models))
        .route(
            "/v1/models/{*model_path}",
            get(crate::service::models::get_model_path)
                .post(crate::service::models::model_path_post)
                .delete(crate::service::models::delete_model_path),
        )
        .route(
            "/v1/models/selected",
            put(crate::service::models::put_selected),
        )
        // M3 batch transcription.
        .route(
            "/v1/transcriptions",
            post(crate::service::batch::post_transcriptions),
        )
        .route(
            "/v1/transcriptions/{job}",
            get(crate::service::batch::get_transcription)
                .delete(crate::service::batch::delete_transcription),
        )
        // M3 auth.
        .route("/v1/auth/tokens", post(post_auth_tokens))
        .route("/v1/auth/ws-ticket", post(post_ws_ticket))
        // M3 history.
        .route("/v1/history", get(get_history))
        .route("/v1/history/{id}", get(get_history_entry))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            bearer_auth_before_dispatch,
        ))
        .with_state(state);

    Router::new()
        .route("/alive", get(alive))
        .merge(api)
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
}

fn request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Model ids may contain `/`. Axum's wildcard capture (`{*id}`) already
/// percent-DECODES the captured remainder (`Path`), so no re-decoding or
/// re-splitting happens here — splitting would truncate ids like
/// `ggml/openai/whisper`. Only any accidental leading slash is dropped.
pub fn decode_model_id(raw: &str) -> String {
    let decoded = raw.trim_start_matches('/');
    decoded.to_string()
}

pub fn error_response(status: StatusCode, err: ApiError, retry_after: Option<&str>) -> Response {
    let body = serde_json::to_string(&err).unwrap_or_else(|_| {
        json!({"error": {
            "code": "internal_error", "message": "error serialization failed",
            "retryable": false, "request_id": ""}})
        .to_string()
    });
    let mut response = (status, [(header::CONTENT_TYPE, "application/json")], body).into_response();
    if let Some(retry) = retry_after {
        if let Ok(value) = axum::http::HeaderValue::from_str(retry) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
    }
    response
}

/// Bearer verification middleware, applied before dispatch. Rejection body:
/// empty (SDK plan §4: no probe enrichment). M3: also accepts a valid
/// one-use ?ticket= when the bearer is absent/invalid.
async fn bearer_auth_before_dispatch(
    State(state): State<Arc<ServiceState>>,
    request: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    let bearer = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| raw.strip_prefix("Bearer "));
    // M3: rotating-aware bearer check.
    if state.verify_bearer_rotating(bearer) {
        return next.run(request).await;
    }
    // M3: ?ticket= accepted for WS upgrade alongside bearer (one-use, 60 s).
    // Tickets carry the session_id in the path — consumed by ws_connect.
    // Here we only admit the request if a *valid unexpired unconsumed* ticket
    // is present; consumption happens in ws_connect.
    let ticket_valid = {
        let path = request.uri().path();
        // Only for session stream upgrades.
        if path.contains("/stream") {
            let query = request.uri().query().unwrap_or("");
            if let Some(ticket) = query.split('&').find_map(|p| p.strip_prefix("ticket=")) {
                let map = state.ws_tickets.lock().unwrap_or_else(|e| e.into_inner());
                map.get(ticket)
                    .map(|t| !t.consumed && t.expires_at_ms > crate::service::state::now_ms())
                    .unwrap_or(false)
            } else {
                false
            }
        } else {
            false
        }
    };
    if ticket_valid {
        return next.run(request).await;
    }
    (StatusCode::UNAUTHORIZED, "").into_response()
}

async fn alive() -> Response {
    Json(json!({ "alive": true })).into_response()
}

async fn health(State(state): State<Arc<ServiceState>>) -> Response {
    let uptime_ms = crate::service::state::now_ms().saturating_sub(state.started_at_ms);
    let response = HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION").to_string(),
        engine_version: state.engine.engine_version(),
        protocol_version: 1,
        uptime_ms,
        ready: true,
    };
    Json(response).into_response()
}

async fn status(State(state): State<Arc<ServiceState>>) -> Response {
    Json(state.status()).into_response()
}

async fn get_config(State(state): State<Arc<ServiceState>>) -> Response {
    let patch = state
        .patch_config
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let config = ConfigResponseV2 {
        bind: state.config.bind.clone(),
        allowed_origins: patch.allowed_origins,
        max_sessions: patch.max_sessions,
        max_audio_seconds: patch.max_audio_seconds,
        session_resume_window_ms: patch.session_resume_window_ms,
        pause_max_ms: patch.pause_max_ms,
        save_history_default: patch.save_history_default,
        vad_enabled_default: patch.vad_enabled_default,
        opus_enabled: patch.opus_enabled,
        restart_required: false,
    };
    Json(config).into_response()
}

/// PATCH /v1/config — mutable set; unknown fields → 422; bind not patchable.
async fn patch_config(State(state): State<Arc<ServiceState>>, body: axum::body::Bytes) -> Response {
    let req: PatchConfigRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::invalid_request("invalid_request", &e.to_string(), &request_id()),
                None,
            );
        }
    };
    // Validate ranges.
    if let Some(ms) = req.max_sessions {
        if !(1..=64).contains(&ms) {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::invalid_request(
                    "invalid_request",
                    "max_sessions must be 1–64",
                    &request_id(),
                ),
                None,
            );
        }
    }
    if let Some(s) = req.max_audio_seconds {
        if !(1..=7200).contains(&s) {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::invalid_request(
                    "invalid_request",
                    "max_audio_seconds must be 1–7200",
                    &request_id(),
                ),
                None,
            );
        }
    }
    if let Some(ms) = req.session_resume_window_ms {
        if ms > 120_000 {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::invalid_request(
                    "invalid_request",
                    "session_resume_window_ms must be 0–120000",
                    &request_id(),
                ),
                None,
            );
        }
    }
    if let Some(ms) = req.pause_max_ms {
        if ms != 0 && ms > 3_600_000 {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::invalid_request(
                    "invalid_request",
                    "pause_max_ms must be 0 (disabled) or ≤ 3600000",
                    &request_id(),
                ),
                None,
            );
        }
    }
    // Apply.
    {
        let mut patch = state.patch_config.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = req.allowed_origins {
            patch.allowed_origins = v;
        }
        if let Some(v) = req.max_sessions {
            patch.max_sessions = v;
        }
        if let Some(v) = req.max_audio_seconds {
            patch.max_audio_seconds = v;
        }
        if let Some(v) = req.session_resume_window_ms {
            patch.session_resume_window_ms = v;
        }
        if let Some(v) = req.pause_max_ms {
            patch.pause_max_ms = v;
        }
        if let Some(v) = req.save_history_default {
            patch.save_history_default = v;
        }
        if let Some(v) = req.vad_enabled_default {
            patch.vad_enabled_default = v;
        }
        if let Some(v) = req.opus_enabled {
            if let Err(e) = apply_opus_enabled_patch(&mut patch, v) {
                return error_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    ApiError::invalid_request("invalid_request", &e, &request_id()),
                    None,
                );
            }
        }
    }
    // Return the full updated config.
    get_config(State(state)).await
}

/// POST /v1/auth/tokens `{name?, rotate_primary?}` → 201.
async fn post_auth_tokens(
    State(state): State<Arc<ServiceState>>,
    body: axum::body::Bytes,
) -> Response {
    let req: CreateTokenRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::invalid_request("invalid_request", &e.to_string(), &request_id()),
                None,
            );
        }
    };
    let rotate = req.rotate_primary.unwrap_or(false);
    if rotate {
        // Embedded mode has no persistent token store.
        if state.auth.verifier.is_none() && state.auth.embedded_token.is_none() {
            return error_response(
                StatusCode::CONFLICT,
                ApiError::new(
                    "rotation_unavailable",
                    "token rotation requires daemon mode",
                    false,
                    &request_id(),
                ),
                None,
            );
        }
        let new_token = match crate::service::auth::generate_token() {
            Ok(t) => t,
            Err(e) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ApiError::new("internal_error", &e, false, &request_id()),
                    None,
                );
            }
        };
        let new_verifier = crate::service::auth::sha256_digest(&new_token);
        let _epoch = state.rotate_token(new_verifier);
        let prefix = crate::service::auth::token_prefix_of(&new_token);
        let name = req.name.as_deref().unwrap_or("primary").to_string();
        let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let response = crate::service::api::CreatedTokenResponse {
            id: request_id(),
            name,
            token: new_token,
            prefix,
            created_at,
        };
        return (StatusCode::CREATED, Json(response)).into_response();
    }
    // Non-rotate: mint a named supplemental token (not persisted in this impl).
    let new_token = match crate::service::auth::generate_token() {
        Ok(t) => t,
        Err(e) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                ApiError::new("internal_error", &e, false, &request_id()),
                None,
            );
        }
    };
    let prefix = crate::service::auth::token_prefix_of(&new_token);
    let name = req.name.as_deref().unwrap_or("supplemental").to_string();
    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let response = crate::service::api::CreatedTokenResponse {
        id: request_id(),
        name,
        token: new_token,
        prefix,
        created_at,
    };
    (StatusCode::CREATED, Json(response)).into_response()
}

/// POST /v1/auth/ws-ticket `{session_id?}` → 201.
async fn post_ws_ticket(
    State(state): State<Arc<ServiceState>>,
    body: axum::body::Bytes,
) -> Response {
    let req: CreateTicketRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::invalid_request("invalid_request", &e.to_string(), &request_id()),
                None,
            );
        }
    };
    let (ticket, expires_at) = state.mint_ticket(req.session_id);
    let response = crate::service::api::CreatedTicketResponse { ticket, expires_at };
    (StatusCode::CREATED, Json(response)).into_response()
}

/// GET /v1/history?cursor&limit (M3 §12).
async fn get_history(
    State(state): State<Arc<ServiceState>>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let Some(ref sh) = state.service_history else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            ApiError::new(
                "history_unavailable",
                "history database is not available",
                false,
                &request_id(),
            ),
            None,
        );
    };
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    match sh.list(query.cursor, limit) {
        Ok((rows, has_more)) => {
            let next_cursor = if has_more {
                rows.last().map(|r| r.id)
            } else {
                None
            };
            let entries = rows.iter().map(|r| r.to_api()).collect();
            Json(ServiceHistoryPage {
                entries,
                has_more,
                next_cursor,
            })
            .into_response()
        }
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::new("internal_error", &e, false, &request_id()),
            None,
        ),
    }
}

/// GET /v1/history/{id}
async fn get_history_entry(
    State(state): State<Arc<ServiceState>>,
    Path(id): Path<i64>,
) -> Response {
    let Some(ref sh) = state.service_history else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            ApiError::new(
                "history_unavailable",
                "history database is not available",
                false,
                &request_id(),
            ),
            None,
        );
    };
    match sh.get(id) {
        Ok(Some(row)) => Json(row.to_api()).into_response(),
        Ok(None) => error_response(
            StatusCode::NOT_FOUND,
            ApiError::new("not_found", "no such history entry", false, &request_id()),
            None,
        ),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::new("internal_error", &e, false, &request_id()),
            None,
        ),
    }
}

/// POST /v1/sessions. Admission per §15 S4 (503 + Retry-After without
/// allocating session state when full), then pre-reservation of the
/// single-inference lease (`service_busy` envelope when contended), then
/// session creation with the `session_token` sub-credential in the 201 body.
async fn create_session(
    State(state): State<Arc<ServiceState>>,
    body: axum::body::Bytes,
) -> Response {
    let request: CreateSessionRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::invalid_request("invalid_request", &e.to_string(), &request_id()),
                None,
            );
        }
    };
    create_session_impl(state, request).await
}

/// M1 create-session orchestration shared with direct tests and CLI.
/// M2 extends the body: task/audio/vad validation, partial_results,
/// save_history, and the `client_session_key` idempotency window.
pub async fn create_session_impl(state: Arc<ServiceState>, body: CreateSessionRequest) -> Response {
    // 0) Body validation (task/audio/vad) — 422 before any allocation.
    if let Some(task) = body.task.as_deref() {
        if task != "transcribe" {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::invalid_request(
                    "invalid_request",
                    &format!("unsupported task '{task}' (only 'transcribe' in v1)"),
                    &request_id(),
                ),
                None,
            );
        }
    }
    // The live PATCH value gates optional codecs (opus) for new sessions; the
    // boot value is merely its seed.
    let patch_snapshot = state
        .patch_config
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let negotiated = match validate_audio(body.audio.as_ref(), patch_snapshot.opus_enabled) {
        Ok(audio) => audio,
        Err(e) => {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::invalid_request("unsupported_audio", &e, &request_id()),
                None,
            );
        }
    };
    let vad = match validate_vad(body.vad.as_ref()) {
        Ok(vad) => vad,
        Err(e) => {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::invalid_request("invalid_request", &e, &request_id()),
                None,
            );
        }
    };

    // 0b) client_session_key idempotency: a duplicate key within 10 minutes
    // returns the existing nonterminal session rather than allocating.
    if let Some(key) = body.client_session_key.as_deref() {
        if !key.is_empty() {
            let found = state
                .client_sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(key)
                .cloned();
            if let Some((session_id, created_ms)) = found {
                if crate::service::state::now_ms() - created_ms <= 600_000
                    && state
                        .live
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .contains_key(&session_id)
                {
                    let Some(session) = state.find_session(&session_id) else {
                        return error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            ApiError::internal(&request_id()),
                            None,
                        );
                    };
                    let selected = session.model_id.clone();
                    let response = CreatedSessionResponse {
                        session_id: session.session_id.clone(),
                        session_token: session.session_token.clone(),
                        state: session.state.as_str(),
                        ws_url: format!("/v1/sessions/{}/stream", session.session_id),
                        expires_at: crate::service::state::session_expiry(
                            state.config.session_resume_window_ms,
                        ),
                        negotiated_audio: negotiated,
                        streaming_mode: state
                            .engine
                            .negotiated_streaming_mode(selected.as_deref())
                            .to_string(),
                        resume_window_ms: state.config.session_resume_window_ms,
                    };
                    return (StatusCode::CREATED, Json(response)).into_response();
                }
            }
        }
    }

    // 1) Admission: refuse without allocating state when full.
    let admission = state.admission.clone().try_acquire_owned();
    let Some(admission) = admission.ok() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            ApiError::new(
                "service_full",
                "Maximum concurrent sessions reached",
                true,
                &request_id(),
            ),
            Some(RETRY_AFTER_ADMISSION),
        );
    };

    // 2) Engine lease: contended → service_busy envelope, no session record.
    let lease = state.inference.clone().try_acquire_owned();
    let lease = match lease {
        Ok(permit) => permit,
        Err(_) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                ApiError::service_busy(&request_id()),
                Some(RETRY_AFTER_BUSY),
            );
        }
    };

    // 3) Engine may still refuse (e.g. load/unload limits); release both.
    let selected = state.engine.status().selected_model_id;
    let requested_model = body
        .model_id
        .as_ref()
        .filter(|m| !m.is_empty())
        .cloned()
        .or(selected);
    let session_id = uuid::Uuid::now_v7().to_string();
    let streaming_mode = state
        .engine
        .negotiated_streaming_mode(requested_model.as_deref())
        .to_string();
    if let Err(engine_error) = state
        .engine
        .session_open(&session_id, requested_model.as_deref())
    {
        std::mem::drop(lease);
        std::mem::drop(admission);
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            ApiError::new("engine_error", &engine_error, true, &request_id()),
            None,
        );
    }

    // 4) Session record + guards (both released together at terminal state).
    let session_token = crate::service::auth::generate_token()
        .unwrap_or_else(|_| format!("vx_local_{}", uuid::Uuid::new_v4()));
    let language = body.language.unwrap_or_else(|| "auto".to_string());
    let session = SessionState {
        session_id: session_id.clone(),
        session_token: session_token.clone(),
        state: SessionStateName::Created,
        model_id: requested_model,
        language,
        created_at_ms: crate::service::state::now_ms(),
        audio_received_ms: 0,
        last_audio_seq: 0,
        last_event_seq: 0,
        vad,
        partial_results: body.partial_results.unwrap_or(true),
        negotiated_audio: negotiated.clone(),
        final_result: None,
        error: None,
        resume_token: None,
        resume_epoch: 0,
    };
    state.insert_live(session, LeaseGuard::new(lease, admission));

    // 5) Record the idempotency key binding after successful allocation.
    if let Some(key) = body.client_session_key.as_deref() {
        if !key.is_empty() {
            state
                .client_sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    key.to_string(),
                    (session_id.clone(), crate::service::state::now_ms()),
                );
        }
    }

    let response = CreatedSessionResponse {
        session_id: session_id.clone(),
        session_token,
        state: "created",
        ws_url: format!("/v1/sessions/{session_id}/stream"),
        expires_at: crate::service::state::session_expiry(state.config.session_resume_window_ms),
        negotiated_audio: negotiated,
        streaming_mode,
        resume_window_ms: state.config.session_resume_window_ms,
    };
    (StatusCode::CREATED, Json(response)).into_response()
}

async fn get_session(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    match session_guarded(&state, &headers, &id) {
        Ok(session) => Json(session_projection(&session)).into_response(),
        Err(response) => response,
    }
}

/// DELETE /v1/sessions/{id}: idempotent cancel; retains a terminal record and
/// releases the engine lease (engine close hook fires too).
async fn delete_session(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = session_guarded(&state, &headers, &id) {
        return response;
    }
    if state
        .set_terminal(&id, SessionStateName::Cancelled, None, None)
        .is_some()
    {
        state.engine.session_close(&id, false);
        state.mark_cancelled(&id);
    }
    StatusCode::NO_CONTENT.into_response()
}

/// PATCH /v1/sessions/{id}: finish (`{"state":"completed"}`) or cancel. Both
/// release the lease. Unknown states => 422 (spec: unknown fields 422).
async fn patch_session(
    State(state): State<Arc<ServiceState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    if let Err(response) = session_guarded(&state, &headers, &id) {
        return response;
    }
    let request: PatchSessionRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::invalid_request("invalid_request", &e.to_string(), &request_id()),
                None,
            );
        }
    };
    let target = match request.state.as_str() {
        "completed" => SessionStateName::Completed,
        "cancelled" => SessionStateName::Cancelled,
        other => {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::invalid_request(
                    "invalid_request",
                    &format!("unsupported state '{other}'"),
                    &request_id(),
                ),
                None,
            );
        }
    };
    let finished = target == SessionStateName::Completed;
    if state.set_terminal(&id, target, None, None).is_some() {
        state.engine.session_close(&id, finished);
        state.mark_cancelled(&id);
    }
    if target == SessionStateName::Cancelled {
        return StatusCode::NO_CONTENT.into_response();
    }
    let Some(session) = state.find_session(&id) else {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::internal(&request_id()),
            None,
        );
    };
    Json(session_projection(&session)).into_response()
}

fn session_projection(session: &SessionState) -> SessionResponse {
    session.projection()
}

fn session_guarded(
    state: &Arc<ServiceState>,
    headers: &HeaderMap,
    id: &str,
) -> Result<SessionState, Response> {
    let sub_token = headers.get("x-session-token").and_then(|v| v.to_str().ok());
    if !state.verify_session_token(id, sub_token) {
        return Err(error_response(
            StatusCode::NOT_FOUND,
            ApiError::session_not_found(&request_id()),
            None,
        ));
    }
    state.find_session(id).ok_or_else(|| {
        error_response(
            StatusCode::NOT_FOUND,
            ApiError::session_not_found(&request_id()),
            None,
        )
    })
}

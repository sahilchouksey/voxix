//! M3 model REST surface (SDK plan §5 Models + §15 G7).
//!
//! Routes:
//! - `GET /v1/models` (optional `?installed=true|false` filter) →
//!   `{models, selected_model_id, loaded_model_id}` with the safe `ModelInfo`
//!   projection (source URLs/paths omitted) plus per-model download status.
//! - `GET /v1/models/{id}` → `{model, prepare}` (live prepare-stage projection
//!   for the phone-side polling fallback).
//! - `PUT /v1/models/selected` `{model_id, load}` → selection/load state;
//!   409 during an inference lease or a failed eager load.
//! - `POST /v1/models/{id}/prepare` → SSE `verifying → loading → ok` with a
//!   single terminal `error {code,message}` on failure and keep-alive comments;
//!   a concurrent prepare is refused 409 `prepare-busy`.
//! - `POST /v1/models/{id}/download` (v1.1 additive) → managed download;
//!   `GET /v1/models/{id}/downloads` polls the status projection.
//! - `DELETE /v1/models/{id}/download` (v1.1 additive) → cancel an in-flight
//!   managed download; partials are kept for resume.
//!
//! Owner-blocked data note (§15 G7 / repo law): catalog download data stays
//! untouched — prepare intentionally serves models that are already present
//! locally and never fabricates a download URL or a managed-download state.

use crate::managers::model::{EngineType, ModelInfo, ModelSource};
use crate::service::api::{
    ApiError, ModelDownloadStatus, ModelView, ModelsResponse, PrepareStage, SelectedModelRequest,
    SingleModelResponse,
};
use crate::service::engine::EngineRuntime;
use crate::service::state::ServiceState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{sse::Event, IntoResponse, Response, Sse},
    Json,
};
use serde_json::json;
use std::convert::Infallible;
use std::sync::Arc;

fn request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Snake_case label for the `engine_type` enum (e.g. `transcribe_cpp`).
pub fn engine_type_label(engine_type: &EngineType) -> String {
    let raw = format!("{engine_type:?}");
    let mut out = String::new();
    for (index, ch) in raw.chars().enumerate() {
        if ch.is_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(ch.to_lowercase().to_string().chars().next().unwrap());
        } else {
            out.push(ch);
        }
    }
    out
}

/// The download status projection: states derive from the manager's
/// on-disk/in-flight flags only (the service never administers downloads).
pub fn download_status(info: &ModelInfo) -> ModelDownloadStatus {
    let total_bytes = info.size_mb.saturating_mul(1024 * 1024);
    let (state, downloaded) = if info.is_downloading {
        ("downloading", info.partial_size)
    } else if info.is_downloaded {
        ("completed", total_bytes)
    } else {
        ("not_started", 0)
    };
    let percentage = if total_bytes > 0 {
        Some(((downloaded as f64 / total_bytes as f64) * 10_000.0).round() / 100.0)
    } else {
        None
    };
    ModelDownloadStatus {
        state: state.to_string(),
        downloaded_bytes: downloaded,
        total_bytes,
        percentage,
        error: None,
    }
}

/// Safe `ModelInfo` mirror: source URL/path projected to a neutral marker.
pub fn model_view(info: &ModelInfo) -> ModelView {
    let source_kind = match &info.source {
        ModelSource::Url { .. } => "download",
        ModelSource::HuggingFace { .. } => "download",
        ModelSource::Local => "local",
    };
    ModelView {
        id: info.id.clone(),
        name: info.name.clone(),
        description: info.description.clone(),
        filename: info.filename.clone(),
        source_kind: source_kind.to_string(),
        size_mb: info.size_mb,
        is_downloaded: info.is_downloaded,
        is_downloading: info.is_downloading,
        partial_size: info.partial_size,
        is_directory: info.is_directory,
        engine_type: engine_type_label(&info.engine_type),
        accuracy_score: info.accuracy_score,
        speed_score: info.speed_score,
        supports_translation: info.supports_translation,
        is_recommended: info.is_recommended,
        supported_languages: info.supported_languages.clone(),
        supports_language_selection: info.supports_language_selection,
        is_custom: info.is_custom,
        supports_streaming: info.supports_streaming,
        supports_language_detection: info.supports_language_detection,
        installed: info.is_downloaded,
        download: download_status(info),
    }
}

#[derive(serde::Deserialize)]
pub struct ModelsQuery {
    #[serde(default)]
    installed: Option<bool>,
}

/// GET /v1/models?installed=true|false
pub async fn get_models(
    State(state): State<Arc<ServiceState>>,
    Query(query): Query<ModelsQuery>,
) -> Response {
    state.engine.refresh_model_status();
    let selected = state.engine.selected_model();
    let status = state.engine.status();
    let models: Vec<ModelView> = state
        .engine
        .list_models()
        .iter()
        .filter(|info| match query.installed {
            Some(true) => info.is_downloaded,
            Some(false) => !info.is_downloaded,
            None => true,
        })
        .map(model_view)
        .collect();
    Json(ModelsResponse {
        models,
        selected_model_id: selected,
        loaded_model_id: status.loaded_model_id,
    })
    .into_response()
}

/// GET /v1/models/{encoded_id} → `{model, prepare}`.
pub async fn get_model(State(state): State<Arc<ServiceState>>, Path(id): Path<String>) -> Response {
    let id = crate::service::routes::decode_model_id(&id);
    state.engine.refresh_model_status();
    let Some(info) = state.engine.model_info(&id) else {
        return error_response_not_found();
    };
    Json(SingleModelResponse {
        model: model_view(&info),
        prepare: json!({ "stage": state.prepare_stage(&id).as_str() }),
    })
    .into_response()
}

/// GET /v1/models/{encoded_id}/downloads → status projection; managed
/// downloads are administered via POST/DELETE `/v1/models/{id}/download`
/// (v1.1 additive).
pub async fn get_model_downloads(
    State(state): State<Arc<ServiceState>>,
    Path(id): Path<String>,
) -> Response {
    let id = crate::service::routes::decode_model_id(&id);
    state.engine.refresh_model_status();
    let Some(info) = state.engine.model_info(&id) else {
        return error_response_not_found();
    };
    Json(download_status(&info)).into_response()
}

/// GET dispatcher over the wildcard capture: `{id}` or `{id}/downloads`.
/// The capture arrives percent-decoded (axum `Path`), so ids containing `/`
/// survive intact.
pub async fn get_model_path(
    State(state): State<Arc<ServiceState>>,
    Path(rest): Path<String>,
) -> Response {
    let rest = rest.trim_start_matches('/');
    if let Some(id) = rest.strip_suffix("/downloads") {
        return get_model_downloads(State(state), Path(id.to_string())).await;
    }
    if rest.ends_with("/prepare") {
        return error_response(
            StatusCode::METHOD_NOT_ALLOWED,
            ApiError::invalid_request(
                "invalid_request",
                "use POST for /v1/models/{id}/prepare",
                &request_id(),
            ),
            None,
        );
    }
    get_model(State(state), Path(rest.to_string())).await
}

/// POST dispatcher: only `{id}/prepare` reaches the SSE prepare flow;
/// `{id}/download` starts (or converges on) a managed download.
pub async fn model_path_post(
    State(state): State<Arc<ServiceState>>,
    Path(rest): Path<String>,
) -> Response {
    let rest = rest.trim_start_matches('/');
    if let Some(id) = rest.strip_suffix("/prepare") {
        return prepare_model(State(state), id.to_string()).await;
    }
    if let Some(id) = rest.strip_suffix("/download") {
        return download_model(State(state), Path(id.to_string())).await;
    }
    error_response(
        StatusCode::NOT_FOUND,
        ApiError::new("model_not_found", "No such model", false, &request_id()),
        None,
    )
}

/// DELETE dispatcher: only `{id}/download` reaches managed-download cancel.
pub async fn delete_model_path(
    State(state): State<Arc<ServiceState>>,
    Path(rest): Path<String>,
) -> Response {
    let rest = rest.trim_start_matches('/');
    if let Some(id) = rest.strip_suffix("/download") {
        return cancel_download(State(state), Path(id.to_string())).await;
    }
    error_response(
        StatusCode::NOT_FOUND,
        ApiError::new("model_not_found", "No such model", false, &request_id()),
        None,
    )
}

/// POST /v1/models/{encoded_id}/download — managed download (v1.1 additive).
///
/// Bearer-authed like the rest of `/v1/*`. Matrix: unknown id → 404
/// `model_not_found`; `ModelSource::Local` → 422 `invalid_request`; already
/// installed → 200 with the `completed` status projection; already
/// downloading → 202 (converge on the single in-flight download, never
/// queue); otherwise start the managed download → 202 with the refreshed
/// projection. `download_unsupported` → 503; anything else → 500.
pub async fn download_model(
    State(state): State<Arc<ServiceState>>,
    Path(id): Path<String>,
) -> Response {
    let id = crate::service::routes::decode_model_id(&id);
    let Some(info) = state.engine.model_info(&id) else {
        return error_response(
            StatusCode::NOT_FOUND,
            ApiError::new("model_not_found", "No such model", false, &request_id()),
            None,
        );
    };
    if matches!(&info.source, ModelSource::Local) {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::invalid_request(
                "invalid_request",
                "model has no download source",
                &request_id(),
            ),
            None,
        );
    }
    if info.is_downloaded {
        state.engine.refresh_model_status();
        let info = state.engine.model_info(&id).unwrap_or(info);
        return (StatusCode::OK, Json(download_status(&info))).into_response();
    }
    if info.is_downloading {
        // Already in flight — surface the live projection; refreshing here
        // would clear the manager's in-memory `is_downloading` hint (the
        // on-disk re-derive only covers complete/partial artifacts).
        return (StatusCode::ACCEPTED, Json(download_status(&info))).into_response();
    }
    match state.engine.download_model_start(&id) {
        Ok(()) => {
            // Reflect reality: the download task flips `is_downloading` (and
            // on-disk partials re-derive on the next refresh).
            state.engine.refresh_model_status();
            let info = state.engine.model_info(&id).unwrap_or(info);
            (StatusCode::ACCEPTED, Json(download_status(&info))).into_response()
        }
        Err(message) if message.starts_with("download_unsupported") => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            ApiError::new("download_unsupported", &message, false, &request_id()),
            None,
        ),
        Err(message) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::new(
                "internal_error",
                &format!("managed download refused to start: {message}"),
                false,
                &request_id(),
            ),
            None,
        ),
    }
}

/// DELETE /v1/models/{encoded_id}/download — cancel an in-flight managed
/// download (v1.1 additive). 204 on success (partials kept for resume; never
/// deletes user data); 404 unknown model; 422 when nothing is in flight;
/// 503 unsupported.
pub async fn cancel_download(
    State(state): State<Arc<ServiceState>>,
    Path(id): Path<String>,
) -> Response {
    let id = crate::service::routes::decode_model_id(&id);
    if state.engine.model_info(&id).is_none() {
        return error_response(
            StatusCode::NOT_FOUND,
            ApiError::new("model_not_found", "No such model", false, &request_id()),
            None,
        );
    }
    match state.engine.cancel_model_download(&id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(message) if message.starts_with("cancel_unsupported") => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            ApiError::new("cancel_unsupported", &message, false, &request_id()),
            None,
        ),
        Err(message) if message.starts_with("model_not_found") => error_response(
            StatusCode::NOT_FOUND,
            ApiError::new("model_not_found", "No such model", false, &request_id()),
            None,
        ),
        Err(message) => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::invalid_request("invalid_request", &message, &request_id()),
            None,
        ),
    }
}

/// PUT /v1/models/selected `{model_id, load:"eager"|"lazy"}`.
pub async fn put_selected(
    State(state): State<Arc<ServiceState>>,
    Json(request): Json<SelectedModelRequest>,
) -> Response {
    let load = match request.load.as_deref().unwrap_or("eager") {
        "eager" => true,
        "lazy" => false,
        other => {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::invalid_request(
                    "invalid_request",
                    &format!("unsupported load '{other}' (eager|lazy only)"),
                    &request_id(),
                ),
                None,
            );
        }
    };
    let model_id = request.model_id;

    // 409 during inference (§5: "409 during inference").
    if state.inference.available_permits() == 0 {
        return error_response(
            StatusCode::CONFLICT,
            ApiError::new(
                "service_busy",
                "an inference is in progress; try again after it completes",
                true,
                &request_id(),
            ),
            None,
        );
    }

    let model_info = match state.engine.model_info(&model_id) {
        Some(info) => info,
        None => {
            return error_response(
                StatusCode::NOT_FOUND,
                ApiError::new("model_not_found", "No such model", false, &request_id()),
                None,
            );
        }
    };
    // Preparation is explicit/prepaid (§15 G7): selection requires the
    // artifact to already be local; the service never implicitly downloads.
    if !model_info.is_downloaded {
        return error_response(
            StatusCode::CONFLICT,
            ApiError::new(
                "model_not_ready",
                "model is not installed locally; prepare it first",
                false,
                &request_id(),
            ),
            None,
        );
    }

    let engine = state.engine.clone();
    let persist = tokio::task::spawn_blocking(move || engine.model_select(&model_id, load)).await;
    match persist {
        Ok(Ok(outcome)) => {
            let info = model_view(&model_info);
            (
                axum::http::StatusCode::OK,
                Json(json!({
                    "model_id": outcome.model_id,
                    "load": if load { "eager" } else { "lazy" },
                    "loaded": outcome.loaded,
                    "model": info,
                })),
            )
                .into_response()
        }
        Ok(Err(message)) => error_response(
            StatusCode::CONFLICT,
            ApiError::new("engine_error", &message, true, &request_id()),
            None,
        ),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::internal(&format!("selection worker failed: {e}")),
            None,
        ),
    }
}

async fn send_sse(
    sender: async_channel::Sender<Result<Event, Infallible>>,
    name: &str,
    payload: serde_json::Value,
) {
    let event = Event::default()
        .event(name)
        .data(serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()));
    let _ = sender.send(Ok(event)).await;
}

/// POST /v1/models/{encoded_id}/prepare → SSE progress stream.
/// Single-flight CAS: a concurrent prepare is refused 409 `prepare-busy`
/// before any work. Artifact publication is never confused with runtime
/// activation: a failed load leaves the installed artifact and the previous
/// warm runtime untouched.
pub async fn prepare_model(State(state): State<Arc<ServiceState>>, model_id: String) -> Response {
    // Single-flight CAS: immediate refusal while another prepare is in flight.
    let Ok(_slot) = state.prepare_slot.clone().try_lock_owned() else {
        return error_response(
            StatusCode::CONFLICT,
            ApiError::new(
                "prepare-busy",
                "another model prepare is already in flight",
                true,
                &request_id(),
            ),
            None,
        );
    };

    let Some(info) = state.engine.model_info(&model_id) else {
        return error_response(
            StatusCode::NOT_FOUND,
            ApiError::new("model_not_found", "No such model", false, &request_id()),
            None,
        );
    };
    // Owner-blocked download data: prepare only reaches models already
    // present locally; nothing fabricates a URL or download state.
    if !info.is_downloaded {
        return prepare_refused_not_local(state, model_id);
    }

    let (sender, receiver) = async_channel::unbounded::<Result<Event, Infallible>>();
    let task_state = state.clone();
    let prepare_model_id = model_id;
    tokio::spawn(async move {
        let _keep = _slot;
        task_state
            .set_prepare_stage(&prepare_model_id, PrepareStage::Verifying)
            .ok();
        send_sse(
            sender.clone(),
            "verifying",
            json!({ "model_id": prepare_model_id, "stage": "verifying" }),
        )
        .await;

        task_state
            .set_prepare_stage(&prepare_model_id, PrepareStage::Loading)
            .ok();
        send_sse(
            sender.clone(),
            "loading",
            json!({ "model_id": prepare_model_id, "stage": "loading" }),
        )
        .await;
        let engine = task_state.engine.clone();
        let target = prepare_model_id.clone();
        let loaded = tokio::task::spawn_blocking(move || {
            EngineRuntime::ensure_model_loaded(&*engine, Some(&target))
        })
        .await;
        match loaded {
            Ok(Ok(facts)) => {
                task_state
                    .set_prepare_stage(&prepare_model_id, PrepareStage::Ok)
                    .ok();
                send_sse(
                    sender.clone(),
                    "ok",
                    json!({
                        "model_id": facts.model_id,
                        "backend": facts.backend,
                        "stage": "ok",
                    }),
                )
                .await;
            }
            Ok(Err(e)) => {
                let message = format!("model load failed: {e}");
                task_state
                    .set_prepare_stage_error(&prepare_model_id, &message)
                    .ok();
                send_sse(
                    sender.clone(),
                    "error",
                    json!({ "code": "engine_error", "message": message, "retryable": true }),
                )
                .await;
            }
            Err(e) => {
                let message = format!("engine worker failed: {e}");
                task_state
                    .set_prepare_stage_error(&prepare_model_id, &message)
                    .ok();
                send_sse(
                    sender.clone(),
                    "error",
                    json!({ "code": "engine_error", "message": message, "retryable": true }),
                )
                .await;
            }
        }
        let _ = sender.close();
    });

    Sse::new(receiver)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response()
}

/// A prepare request for a model the service cannot obtain locally (owner-
/// blocked download data): terminal SSE `error` with `model_not_ready`.
fn prepare_refused_not_local(state: Arc<ServiceState>, id: String) -> Response {
    let (sender, receiver) = async_channel::unbounded::<Result<Event, Infallible>>();
    let _ = state.set_prepare_stage(&id, PrepareStage::Failed("no local artifact".to_string()));
    tokio::spawn(async move {
        send_sse(
            sender.clone(),
            "error",
            json!({
                "code": "model_not_ready",
                "message": "model is not installed locally and the service downloads nothing",
                "retryable": false,
            }),
        )
        .await;
        let _ = sender.close();
    });
    Sse::new(receiver)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response()
}

fn error_response(
    status: StatusCode,
    err: crate::service::api::ApiError,
    retry_after: Option<&str>,
) -> Response {
    use crate::service::routes::error_response as route_error;
    route_error(status, err, retry_after)
}

fn error_response_not_found() -> Response {
    error_response(
        StatusCode::NOT_FOUND,
        ApiError::new("model_not_found", "No such model", false, &request_id()),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::TokenStdoutMode;
    use crate::managers::model::{EngineType, ModelSource};
    use crate::service::config::{ServiceConfig, ServiceMode};
    use crate::service::engine::{EngineRuntimeStatus, ModelState};
    use std::sync::{Arc, Mutex};

    /// Minimal catalog entry for the fake engine. Source URLs stay out of
    /// the projection, so tests only branch on the kind.
    fn fake_model(
        id: &str,
        source: ModelSource,
        is_downloaded: bool,
        is_downloading: bool,
    ) -> ModelInfo {
        ModelInfo {
            id: id.to_string(),
            name: format!("Fake {id}"),
            description: "fake model for service tests".to_string(),
            filename: format!("{id}.bin"),
            source,
            size_mb: 100,
            is_downloaded,
            is_downloading,
            partial_size: 0,
            is_directory: false,
            engine_type: EngineType::TranscribeCpp,
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

    #[derive(Default)]
    struct FakeEngineState {
        models: Vec<ModelInfo>,
        refreshes: u64,
        spawned: Vec<String>,
        download_error: Option<String>,
        cancelled: Vec<String>,
        cancel_error: Option<String>,
    }

    /// Minimal `EngineRuntime` recording the v1.1 hooks; a scripted error
    /// short-circuits each capability so route mapping is observable.
    struct FakeEngine {
        inner: Mutex<FakeEngineState>,
    }

    impl FakeEngine {
        fn new(models: Vec<ModelInfo>) -> Self {
            FakeEngine {
                inner: Mutex::new(FakeEngineState {
                    models,
                    ..FakeEngineState::default()
                }),
            }
        }

        fn refreshes(&self) -> u64 {
            self.inner.lock().unwrap().refreshes
        }

        fn spawned(&self) -> Vec<String> {
            self.inner.lock().unwrap().spawned.clone()
        }

        fn cancelled(&self) -> Vec<String> {
            self.inner.lock().unwrap().cancelled.clone()
        }
    }

    impl EngineRuntime for FakeEngine {
        fn engine_version(&self) -> String {
            "1.0.0+testsha".to_string()
        }

        fn capabilities(&self) -> crate::service::api::Capabilities {
            crate::service::api::Capabilities::default()
        }

        fn status(&self) -> EngineRuntimeStatus {
            EngineRuntimeStatus {
                engine_version: self.engine_version(),
                selected_model_id: None,
                model_state: ModelState::Unloaded,
                backend: None,
                loaded_model_id: None,
                max_realtime_factor: None,
            }
        }

        fn session_open(&self, _session_id: &str, _model: Option<&str>) -> Result<(), String> {
            Ok(())
        }

        fn session_close(&self, _session_id: &str, _finished: bool) {}

        fn list_models(&self) -> Vec<ModelInfo> {
            self.inner.lock().unwrap().models.clone()
        }

        fn model_info(&self, model_id: &str) -> Option<ModelInfo> {
            self.inner
                .lock()
                .unwrap()
                .models
                .iter()
                .find(|m| m.id == model_id)
                .cloned()
        }

        fn selected_model(&self) -> Option<String> {
            None
        }

        fn refresh_model_status(&self) {
            self.inner.lock().unwrap().refreshes += 1;
        }

        fn download_model_start(&self, model_id: &str) -> Result<(), String> {
            let mut inner = self.inner.lock().unwrap();
            inner.spawned.push(model_id.to_string());
            match inner.download_error.clone() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }

        fn cancel_model_download(&self, model_id: &str) -> Result<(), String> {
            let mut inner = self.inner.lock().unwrap();
            inner.cancelled.push(model_id.to_string());
            match inner.cancel_error.clone() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }
    }

    fn test_state(engine: Arc<FakeEngine>) -> Arc<ServiceState> {
        let tmp = std::env::temp_dir().join(format!(
            "voxix-service-models-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&tmp).expect("create temp data dir");
        let cfg = ServiceConfig {
            mode: ServiceMode::Standalone,
            bind: "127.0.0.1:0".to_string(),
            data_dir: tmp,
            token_stdout: TokenStdoutMode::Never,
            max_sessions: 1,
            max_audio_seconds: 60,
            session_resume_window_ms: 30_000,
            opus_enabled: false,
        };
        let (stream_text, _rx) = tokio::sync::broadcast::channel(8);
        Arc::new(ServiceState::new(
            cfg,
            engine,
            None,
            Some("test-token".to_string()),
            stream_text,
        ))
    }

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }

    #[tokio::test]
    async fn get_models_calls_refresh_hook() {
        let engine = Arc::new(FakeEngine::new(vec![fake_model(
            "catalog-a",
            ModelSource::HuggingFace {
                repo_id: "org/catalog-a".to_string(),
                revision: "main".to_string(),
            },
            false,
            false,
        )]));
        let state = test_state(engine.clone());
        let response = get_models(State(state), Query(ModelsQuery { installed: None })).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(engine.refreshes(), 1);
    }

    #[tokio::test]
    async fn download_unknown_id_is_404() {
        let engine = Arc::new(FakeEngine::new(vec![]));
        let state = test_state(engine);
        let response = download_model(State(state), Path("no-such-model".to_string())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = body_json(response).await;
        assert_eq!(body["error"]["code"], "model_not_found");
    }

    #[tokio::test]
    async fn download_local_source_is_422() {
        let engine = Arc::new(FakeEngine::new(vec![fake_model(
            "my-local",
            ModelSource::Local,
            false,
            false,
        )]));
        let state = test_state(engine.clone());
        let response = download_model(State(state), Path("my-local".to_string())).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = body_json(response).await;
        assert_eq!(body["error"]["code"], "invalid_request");
        assert_eq!(engine.spawned().len(), 0);
    }

    #[tokio::test]
    async fn download_unsupported_engine_is_503() {
        let engine = Arc::new(FakeEngine::new(vec![fake_model(
            "catalog-b",
            ModelSource::HuggingFace {
                repo_id: "org/catalog-b".to_string(),
                revision: "main".to_string(),
            },
            false,
            false,
        )]));
        engine.inner.lock().unwrap().download_error = Some("download_unsupported".to_string());
        let state = test_state(engine);
        let response = download_model(State(state), Path("catalog-b".to_string())).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = body_json(response).await;
        assert_eq!(body["error"]["code"], "download_unsupported");
    }

    #[tokio::test]
    async fn download_starts_and_returns_202_with_refreshed_projection() {
        let engine = Arc::new(FakeEngine::new(vec![fake_model(
            "catalog-c",
            ModelSource::HuggingFace {
                repo_id: "org/catalog-c".to_string(),
                revision: "main".to_string(),
            },
            false,
            false,
        )]));
        let state = test_state(engine.clone());
        let response = download_model(State(state), Path("catalog-c".to_string())).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(engine.spawned(), vec!["catalog-c".to_string()]);
        assert!(engine.refreshes() >= 1);
        let body = body_json(response).await;
        assert_eq!(body["total_bytes"], 100 * 1024 * 1024);
    }

    #[tokio::test]
    async fn download_already_installed_is_200_without_spawn() {
        let engine = Arc::new(FakeEngine::new(vec![fake_model(
            "catalog-d",
            ModelSource::HuggingFace {
                repo_id: "org/catalog-d".to_string(),
                revision: "main".to_string(),
            },
            true,
            false,
        )]));
        let state = test_state(engine.clone());
        let response = download_model(State(state), Path("catalog-d".to_string())).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(engine.spawned().len(), 0);
        let body = body_json(response).await;
        assert_eq!(body["state"], "completed");
    }

    #[tokio::test]
    async fn download_already_downloading_is_202_without_spawn() {
        let engine = Arc::new(FakeEngine::new(vec![fake_model(
            "catalog-e",
            ModelSource::HuggingFace {
                repo_id: "org/catalog-e".to_string(),
                revision: "main".to_string(),
            },
            false,
            true,
        )]));
        let state = test_state(engine.clone());
        let response = download_model(State(state), Path("catalog-e".to_string())).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(engine.spawned().len(), 0);
        let body = body_json(response).await;
        assert_eq!(body["state"], "downloading");
    }

    #[tokio::test]
    async fn cancel_unknown_id_is_404() {
        let engine = Arc::new(FakeEngine::new(vec![]));
        let state = test_state(engine);
        let response = cancel_download(State(state), Path("no-such-model".to_string())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = body_json(response).await;
        assert_eq!(body["error"]["code"], "model_not_found");
    }

    #[tokio::test]
    async fn cancel_nothing_in_flight_is_422() {
        let engine = Arc::new(FakeEngine::new(vec![fake_model(
            "catalog-f",
            ModelSource::HuggingFace {
                repo_id: "org/catalog-f".to_string(),
                revision: "main".to_string(),
            },
            false,
            false,
        )]));
        engine.inner.lock().unwrap().cancel_error =
            Some("nothing_to_cancel: no download in flight".to_string());
        let state = test_state(engine);
        let response = cancel_download(State(state), Path("catalog-f".to_string())).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = body_json(response).await;
        assert_eq!(body["error"]["code"], "invalid_request");
    }

    #[tokio::test]
    async fn cancel_unsupported_engine_is_503() {
        let engine = Arc::new(FakeEngine::new(vec![fake_model(
            "catalog-g",
            ModelSource::HuggingFace {
                repo_id: "org/catalog-g".to_string(),
                revision: "main".to_string(),
            },
            false,
            true,
        )]));
        engine.inner.lock().unwrap().cancel_error = Some("cancel_unsupported".to_string());
        let state = test_state(engine);
        let response = cancel_download(State(state), Path("catalog-g".to_string())).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = body_json(response).await;
        assert_eq!(body["error"]["code"], "cancel_unsupported");
    }

    #[tokio::test]
    async fn cancel_success_is_204() {
        let engine = Arc::new(FakeEngine::new(vec![fake_model(
            "catalog-h",
            ModelSource::HuggingFace {
                repo_id: "org/catalog-h".to_string(),
                revision: "main".to_string(),
            },
            false,
            true,
        )]));
        let state = test_state(engine.clone());
        let response = cancel_download(State(state), Path("catalog-h".to_string())).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(engine.cancelled(), vec!["catalog-h".to_string()]);
    }
}

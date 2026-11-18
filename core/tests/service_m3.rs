//! M3 model REST surface integration suite (SDK plan §5 / §12-M3 / §15-G7).
//!
//! Full Axum router + deterministic TestEngine over HTTP for the M3 model
//! mechanisms:
//! - `GET /v1/models` catalog projection (+ `?installed=` filter, selection ids).
//! - `GET /v1/models/{id}` polling projection `{model, prepare}` and wildcard
//!   percent-decoded ids (ids containing `/`).
//! - `GET /v1/models/{id}/downloads` read-only state projection.
//! - `PUT /v1/models/selected` lazy/eager, not-installed 409, inference-busy 409,
//!   failed eager load keeps the warm runtime.
//! - `POST /v1/models/{id}/prepare` SSE `verifying→loading→ok|error`, the
//!   single-flight `409 prepare-busy` CAS, and a not-installed SSE refusal.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use voxix_app_lib::service::auth::sha256_digest;
use voxix_app_lib::service::config::{ServiceConfig, ServiceMode};
use voxix_app_lib::service::engine::TestEngine;
use voxix_app_lib::service::routes;
use voxix_app_lib::service::state::ServiceState;

const IN_PROCESS_TOKEN: &str = "vx_m3_in_process_models_token_5d21";
const MODEL_A: &str = "test-model";
const MODEL_B: &str = "test-alt-model";
const SLASH_ID: &str = "vendor/nested-model";
const READ_TIMEOUT: Duration = Duration::from_secs(10);

fn temp_data_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "voxix-m3-{tag}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().as_simple()
    ));
    std::fs::create_dir_all(&dir).expect("create temp data dir");
    dir
}

fn in_process_config() -> ServiceConfig {
    ServiceConfig {
        mode: ServiceMode::Standalone,
        bind: "127.0.0.1:0".to_string(),
        data_dir: temp_data_dir("ip"),
        token_stdout: voxix_app_lib::cli::TokenStdoutMode::Never,
        max_sessions: 8,
        max_audio_seconds: 1800,
        opus_enabled: false,
        session_resume_window_ms: 30_000,
    }
}

fn test_state() -> (Arc<ServiceState>, Arc<TestEngine>) {
    let engine = Arc::new(TestEngine::default());
    let (tx, _rx) = voxix_app_lib::service::stream_text_bus();
    let state = Arc::new(ServiceState::new(
        in_process_config(),
        engine.clone() as _,
        Some(sha256_digest(IN_PROCESS_TOKEN)),
        None,
        tx,
    ));
    (state, engine)
}

async fn serve_router(state: Arc<ServiceState>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, routes::router(state))
            .await
            .expect("in-process router serve");
    });
    addr
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(READ_TIMEOUT)
        .build()
        .expect("http client")
}

fn req(http: &reqwest::Client, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
    http.request(method, url).header(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&format!("Bearer {IN_PROCESS_TOKEN}"))
            .expect("bearer"),
    )
}

/// Consume an SSE body until it closes, returning the raw event text.
async fn drain_sse(response: reqwest::Response) -> String {
    let mut body = String::new();
    let mut stream = response.bytes_stream();
    let deadline = tokio::time::Instant::now() + READ_TIMEOUT;
    while let Some(chunk) = tokio::time::timeout_at(deadline, stream.next())
        .await
        .ok()
        .flatten()
    {
        match chunk {
            Ok(bytes) => body.push_str(&String::from_utf8_lossy(&bytes)),
            Err(_) => break,
        }
    }
    body
}

// ---------------------------------------------------------------------------
// Catalog / polling / downloads
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn models_catalog_projection_normalize_and_installed_filter() {
    let (state, engine) = test_state();
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();
    engine.insert_test_model(MODEL_A, true);
    engine.insert_test_model(MODEL_B, false);

    // Unfiltered: every catalog entry projects (safe mirror: no URL/path).
    let response = req(&http, reqwest::Method::GET, &format!("{base}/v1/models"))
        .send()
        .await
        .expect("GET models");
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.expect("models json");
    let ids: Vec<&str> = body["models"]
        .as_array()
        .expect("models array")
        .iter()
        .map(|m| m["id"].as_str().expect("id"))
        .collect();
    assert!(ids.contains(&MODEL_A) && ids.contains(&MODEL_B), "{body}");
    assert!(!serde_json::to_string(&body).unwrap().contains("source_url"));
    assert!(!serde_json::to_string(&body).unwrap().contains("path"));
    let a = body["models"]
        .as_array()
        .expect("models")
        .iter()
        .find(|m| m["id"] == MODEL_A)
        .expect("test model entry")
        .clone();
    assert_eq!(a["installed"], serde_json::json!(true));
    assert_eq!(a["download"]["state"], serde_json::json!("completed"));
    assert_eq!(a["source_kind"], serde_json::json!("local"));
    assert_eq!(a["engine_type"], serde_json::json!("transcribe_cpp"));

    // ?installed=true only lists downloaded entries.
    let response = req(
        &http,
        reqwest::Method::GET,
        &format!("{base}/v1/models?installed=true"),
    )
    .send()
    .await
    .expect("GET models installed=true");
    let body: serde_json::Value = response.json().await.expect("models json");
    let ids: Vec<&str> = body["models"]
        .as_array()
        .expect("models")
        .iter()
        .map(|m| m["id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, vec![MODEL_A], "{body}");

    // ?installed=false only lists the not-downloaded entry.
    let response = req(
        &http,
        reqwest::Method::GET,
        &format!("{base}/v1/models?installed=false"),
    )
    .send()
    .await
    .expect("GET models installed=false");
    let body: serde_json::Value = response.json().await.expect("models json");
    let ids: Vec<&str> = body["models"]
        .as_array()
        .expect("models")
        .iter()
        .map(|m| m["id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, vec![MODEL_B], "{body}");

    // The 401 gate still applies (bearer middleware).
    let response = http
        .get(format!("{base}/v1/models"))
        .send()
        .await
        .expect("unauthenticated GET");
    assert_eq!(response.status(), 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn single_model_polling_downloads_and_wildcard_ids() {
    let (state, engine) = test_state();
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();
    engine.insert_test_model(MODEL_A, true);
    engine.insert_test_model(SLASH_ID, true);

    // Plain id: {model, prepare} with the pre-prepare polling stage.
    let response = req(
        &http,
        reqwest::Method::GET,
        &format!("{base}/v1/models/{MODEL_A}"),
    )
    .send()
    .await
    .expect("GET model");
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.expect("model json");
    assert_eq!(body["model"]["id"], serde_json::json!(MODEL_A));
    assert_eq!(body["prepare"]["stage"], serde_json::json!("not_started"));

    // An id containing `/` survives the wildcard route intact.
    let response = req(
        &http,
        reqwest::Method::GET,
        &format!("{base}/v1/models/{SLASH_ID}"),
    )
    .send()
    .await
    .expect("GET slash model");
    assert_eq!(response.status(), 200, "wildcard id lookup");
    let body: serde_json::Value = response.json().await.expect("slash model json");
    assert_eq!(body["model"]["id"], serde_json::json!(SLASH_ID));

    // /downloads suffix is the read-only state projection.
    let response = req(
        &http,
        reqwest::Method::GET,
        &format!("{base}/v1/models/{MODEL_A}/downloads"),
    )
    .send()
    .await
    .expect("GET downloads");
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.expect("downloads json");
    assert_eq!(body["state"], serde_json::json!("completed"));
    assert_eq!(body["total_bytes"], serde_json::json!(100 * 1024 * 1024));

    // Unknown model → 404 + envelope.
    let response = req(
        &http,
        reqwest::Method::GET,
        &format!("{base}/v1/models/absent"),
    )
    .send()
    .await
    .expect("GET unknown model");
    assert_eq!(response.status(), 404);
    let body: serde_json::Value = response.json().await.expect("error json");
    assert_eq!(body["error"]["code"], serde_json::json!("model_not_found"));
    assert_eq!(body["error"]["retryable"], serde_json::json!(false));
}

// ---------------------------------------------------------------------------
// PUT /v1/models/selected
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn selected_lazy_eager_and_rejections() {
    let (state, engine) = test_state();
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();
    engine.insert_test_model(MODEL_A, true);
    engine.insert_test_model(MODEL_B, false);

    // Lazy select an installed model: 200, selection recorded, not loaded.
    let response = req(
        &http,
        reqwest::Method::PUT,
        &format!("{base}/v1/models/selected"),
    )
    .json(&serde_json::json!({"model_id": MODEL_A, "load": "lazy"}))
    .send()
    .await
    .expect("PUT selected lazy");
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.expect("select json");
    assert_eq!(body["model_id"], serde_json::json!(MODEL_A));
    assert_eq!(body["load"], serde_json::json!("lazy"));
    assert_eq!(body["loaded"], serde_json::json!(false));
    assert_eq!(engine.runtime_loaded(), false);

    // Eager select loads into the runtime.
    let response = req(
        &http,
        reqwest::Method::PUT,
        &format!("{base}/v1/models/selected"),
    )
    .json(&serde_json::json!({"model_id": MODEL_A, "load": "eager"}))
    .send()
    .await
    .expect("PUT selected eager");
    assert_eq!(
        response.status(),
        200,
        "{}",
        response.text().await.unwrap_or_default()
    );
    let body: serde_json::Value = response.json().await.expect("select json");
    assert_eq!(body["loaded"], serde_json::json!(true));
    assert_eq!(body["model"]["id"], serde_json::json!(MODEL_A));

    // Unsupported load value → 422.
    let response = req(
        &http,
        reqwest::Method::PUT,
        &format!("{base}/v1/models/selected"),
    )
    .json(&serde_json::json!({"model_id": MODEL_A, "load": "instant"}))
    .send()
    .await
    .expect("PUT selected bad load");
    assert_eq!(response.status(), 422);
    let body: serde_json::Value = response.json().await.expect("error json");
    assert_eq!(body["error"]["code"], serde_json::json!("invalid_request"));

    // Unknown model → 404.
    let response = req(
        &http,
        reqwest::Method::PUT,
        &format!("{base}/v1/models/selected"),
    )
    .json(&serde_json::json!({"model_id": "ghost"}))
    .send()
    .await
    .expect("PUT selected unknown");
    assert_eq!(response.status(), 404);

    // Not-installed model → 409 model_not_ready (explicit/prepaid §15 G7).
    let response = req(
        &http,
        reqwest::Method::PUT,
        &format!("{base}/v1/models/selected"),
    )
    .json(&serde_json::json!({"model_id": MODEL_B, "load": "eager"}))
    .send()
    .await
    .expect("PUT selected not installed");
    assert_eq!(response.status(), 409);
    let body: serde_json::Value = response.json().await.expect("error json");
    assert_eq!(body["error"]["code"], serde_json::json!("model_not_ready"));
}

#[tokio::test(flavor = "multi_thread")]
async fn selected_refused_while_lease_held_and_failed_load_keeps_warm_runtime() {
    let (state, engine) = test_state();
    let state_for_lease = state.clone();
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();
    engine.insert_test_model(MODEL_A, true);
    engine.insert_test_model(MODEL_B, true);

    // 409 during inference (§5): hold the single-permit lease from the test.
    let lease = state_for_lease
        .inference
        .clone()
        .try_acquire_owned()
        .expect("hold the lease");
    let response = req(
        &http,
        reqwest::Method::PUT,
        &format!("{base}/v1/models/selected"),
    )
    .json(&serde_json::json!({"model_id": MODEL_A}))
    .send()
    .await
    .expect("PUT selected while busy");
    assert_eq!(response.status(), 409);
    let body: serde_json::Value = response.json().await.expect("error json");
    assert_eq!(body["error"]["code"], serde_json::json!("service_busy"));
    assert_eq!(body["error"]["retryable"], serde_json::json!(true));
    drop(lease);

    // Warm the runtime on MODEL_A, then script a failure and eager-select
    // MODEL_B: the runtime must keep MODEL_A resident (artifact publication
    // ≠ runtime activation, §15 G7).
    engine.set_load_hold(None);
    let response = req(
        &http,
        reqwest::Method::PUT,
        &format!("{base}/v1/models/selected"),
    )
    .json(&serde_json::json!({"model_id": MODEL_A, "load": "eager"}))
    .send()
    .await
    .expect("warm MODEL_A");
    assert_eq!(response.status(), 200);
    assert_eq!(engine.runtime_model().as_deref(), Some(MODEL_A));

    engine.set_load_error(Some("scripted load failure".to_string()));
    let response = req(
        &http,
        reqwest::Method::PUT,
        &format!("{base}/v1/models/selected"),
    )
    .json(&serde_json::json!({"model_id": MODEL_B, "load": "eager"}))
    .send()
    .await
    .expect("failed eager load");
    assert_eq!(response.status(), 409);
    let body: serde_json::Value = response.json().await.expect("error json");
    assert_eq!(body["error"]["code"], serde_json::json!("engine_error"));
    // The warm runtime is untouched and the selection reverted.
    assert_eq!(engine.runtime_model().as_deref(), Some(MODEL_A));
    engine.set_load_error(None);
}

// ---------------------------------------------------------------------------
// POST /v1/models/{id}/prepare — SSE + single-flight CAS
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn prepare_sse_streams_stages_to_ok_and_concurrent_prepare_is_busy() {
    let (state, engine) = test_state();
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();
    engine.insert_test_model(MODEL_A, true);

    // Make the deterministic load slow enough to observe the CAS.
    engine.set_load_hold(Some(250));
    let first = req(
        &http,
        reqwest::Method::POST,
        &format!("{base}/v1/models/{MODEL_A}/prepare"),
    )
    .send()
    .await
    .expect("POST prepare");
    assert_eq!(first.status(), 200);
    assert_eq!(
        first.headers()["content-type"].to_str().unwrap_or(""),
        "text/event-stream",
        "prepare answers SSE"
    );

    // While the first prepare is in flight, the CAS refuses 409 prepare-busy.
    let second = req(
        &http,
        reqwest::Method::POST,
        &format!("{base}/v1/models/{MODEL_A}/prepare"),
    )
    .send()
    .await
    .expect("POST concurrent prepare");
    assert_eq!(second.status(), 409, "single-flight CAS refusal");
    let body: serde_json::Value = second.json().await.expect("409 json");
    assert_eq!(body["error"]["code"], serde_json::json!("prepare-busy"));
    assert_eq!(body["error"]["retryable"], serde_json::json!(true));

    // The first stream completes verifying → loading → ok.
    let text = drain_sse(first).await;
    assert!(text.contains("event: verifying"), "{text}");
    assert!(text.contains("event: loading"), "{text}");
    assert!(text.contains("event: ok"), "{text}");
    assert!(text.contains("\"stage\":\"ok\""), "{text}");
    engine.set_load_hold(None);

    // The polling projection reflects the terminal stage afterwards.
    let response = req(
        &http,
        reqwest::Method::GET,
        &format!("{base}/v1/models/{MODEL_A}"),
    )
    .send()
    .await
    .expect("GET model after prepare");
    let body: serde_json::Value = response.json().await.expect("poll json");
    assert_eq!(body["prepare"]["stage"], serde_json::json!("ok"));

    // A second prepare after completion must not answer busy (the CAS is
    // free) and re-borrows the resident engine.
    let again = req(
        &http,
        reqwest::Method::POST,
        &format!("{base}/v1/models/{MODEL_A}/prepare"),
    )
    .send()
    .await
    .expect("POST re-prepare");
    assert_eq!(again.status(), 200);
    let text = drain_sse(again).await;
    assert!(text.contains("event: ok"), "{text}");
    assert_eq!(engine.runtime_model().as_deref(), Some(MODEL_A));
}

#[tokio::test(flavor = "multi_thread")]
async fn prepare_load_failure_emits_terminal_error_and_polling_failed() {
    let (state, engine) = test_state();
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();
    engine.insert_test_model(MODEL_A, true);
    engine.set_load_error(Some("scripted".to_string()));

    let response = req(
        &http,
        reqwest::Method::POST,
        &format!("{base}/v1/models/{MODEL_A}/prepare"),
    )
    .send()
    .await
    .expect("POST failing prepare");
    assert_eq!(response.status(), 200, "SSE answers then errors in-stream");
    let text = drain_sse(response).await;
    assert!(text.contains("event: verifying"), "{text}");
    assert!(text.contains("event: error"), "{text}");
    assert!(text.contains("engine_error"), "{text}");

    let poll = req(
        &http,
        reqwest::Method::GET,
        &format!("{base}/v1/models/{MODEL_A}"),
    )
    .send()
    .await
    .expect("GET failed stage");
    let body: serde_json::Value = poll.json().await.expect("poll json");
    assert_eq!(body["prepare"]["stage"], serde_json::json!("failed"));
    engine.set_load_error(None);
}

#[tokio::test(flavor = "multi_thread")]
async fn prepare_of_missing_model_not_found_and_not_installed_stream_refusal() {
    let (state, engine) = test_state();
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();
    engine.insert_test_model(MODEL_B, false);

    // Unknown id → plain 404 envelope (no SSE body started).
    let response = req(
        &http,
        reqwest::Method::POST,
        &format!("{base}/v1/models/absent/prepare"),
    )
    .send()
    .await
    .expect("POST prepare unknown");
    assert_eq!(response.status(), 404);

    // Not-installed model → SSE-refusal stream with a terminal error; the
    // service downloads nothing (owner-blocked catalog data untouched).
    let response = req(
        &http,
        reqwest::Method::POST,
        &format!("{base}/v1/models/{MODEL_B}/prepare"),
    )
    .send()
    .await
    .expect("POST prepare not installed");
    assert_eq!(response.status(), 200, "refusal rides the SSE channel");
    let text = drain_sse(response).await;
    assert!(text.contains("event: error"), "{text}");
    assert!(text.contains("model_not_ready"), "{text}");
    assert!(text.contains("\"retryable\":false"), "{text}");

    // Polling stage for the refused model is `failed`.
    let poll = req(
        &http,
        reqwest::Method::GET,
        &format!("{base}/v1/models/{MODEL_B}"),
    )
    .send()
    .await
    .expect("GET refused stage");
    let body: serde_json::Value = poll.json().await.expect("poll json");
    assert_eq!(body["prepare"]["stage"], serde_json::json!("failed"));
}

#[tokio::test(flavor = "multi_thread")]
async fn prepare_suffix_route_guarding() {
    let (state, engine) = test_state();
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();
    engine.insert_test_model(MODEL_A, true);

    // GET on the prepare subpath → 405 (POST-only).
    let response = req(
        &http,
        reqwest::Method::GET,
        &format!("{base}/v1/models/{MODEL_A}/prepare"),
    )
    .send()
    .await
    .expect("GET on prepare path");
    assert_eq!(response.status(), 405);

    // POST without the /prepare suffix → 404 envelope.
    let response = req(
        &http,
        reqwest::Method::POST,
        &format!("{base}/v1/models/{MODEL_A}"),
    )
    .send()
    .await
    .expect("POST bare model path");
    assert_eq!(response.status(), 404);
    let body: serde_json::Value = response.json().await.expect("error json");
    assert_eq!(body["error"]["code"], serde_json::json!("model_not_found"));
}

// ---------------------------------------------------------------------------
// PATCH /v1/config
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn patch_config_apply_live_and_validate_ranges() {
    let (state, _engine) = test_state();
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();

    // Initial GET returns boot defaults.
    let response = req(&http, reqwest::Method::GET, &format!("{base}/v1/config"))
        .send()
        .await
        .expect("GET config");
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.expect("config json");
    assert_eq!(body["max_sessions"], serde_json::json!(8));

    // Valid PATCH → 200 updated config.
    let response = req(&http, reqwest::Method::PATCH, &format!("{base}/v1/config"))
        .json(&serde_json::json!({
            "max_sessions": 4,
            "max_audio_seconds": 900,
            "session_resume_window_ms": 60000,
            "pause_max_ms": 5000,
            "save_history_default": false,
            "vad_enabled_default": false,
            "allowed_origins": ["https://example.com"]
        }))
        .send()
        .await
        .expect("PATCH config");
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.expect("patched config json");
    assert_eq!(body["max_sessions"], serde_json::json!(4));
    assert_eq!(body["max_audio_seconds"], serde_json::json!(900));
    assert_eq!(body["session_resume_window_ms"], serde_json::json!(60000));
    assert_eq!(body["pause_max_ms"], serde_json::json!(5000));
    assert_eq!(body["save_history_default"], serde_json::json!(false));
    assert_eq!(
        body["allowed_origins"],
        serde_json::json!(["https://example.com"])
    );
    // bind is not patchable.
    assert!(body["bind"].as_str().is_some());

    // Out-of-range max_sessions → 422.
    let response = req(&http, reqwest::Method::PATCH, &format!("{base}/v1/config"))
        .json(&serde_json::json!({ "max_sessions": 0 }))
        .send()
        .await
        .expect("PATCH bad max_sessions");
    assert_eq!(response.status(), 422);

    // Out-of-range max_audio_seconds → 422.
    let response = req(&http, reqwest::Method::PATCH, &format!("{base}/v1/config"))
        .json(&serde_json::json!({ "max_audio_seconds": 0 }))
        .send()
        .await
        .expect("PATCH bad max_audio_seconds");
    assert_eq!(response.status(), 422);

    // Out-of-range session_resume_window_ms → 422.
    let response = req(&http, reqwest::Method::PATCH, &format!("{base}/v1/config"))
        .json(&serde_json::json!({ "session_resume_window_ms": 200000 }))
        .send()
        .await
        .expect("PATCH bad resume_window");
    assert_eq!(response.status(), 422);

    // Oversized pause_max_ms → 422.
    let response = req(&http, reqwest::Method::PATCH, &format!("{base}/v1/config"))
        .json(&serde_json::json!({ "pause_max_ms": 3700000 }))
        .send()
        .await
        .expect("PATCH bad pause_max_ms");
    assert_eq!(response.status(), 422);

    // Unknown field → 422.
    let response = req(&http, reqwest::Method::PATCH, &format!("{base}/v1/config"))
        .json(&serde_json::json!({ "bind": "0.0.0.0:9999" }))
        .send()
        .await
        .expect("PATCH bind rejected");
    assert_eq!(response.status(), 422);
}

// ---------------------------------------------------------------------------
// Auth: token rotation (4001) + ws-ticket
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn token_rotation_rejects_old_token_and_accepts_new() {
    let (state, _engine) = test_state();
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();

    // Baseline: old token works.
    let response = req(&http, reqwest::Method::GET, &format!("{base}/v1/health"))
        .send()
        .await
        .expect("health before rotate");
    assert_eq!(response.status(), 200);

    // Rotate the primary token.
    let response = req(
        &http,
        reqwest::Method::POST,
        &format!("{base}/v1/auth/tokens"),
    )
    .json(&serde_json::json!({ "rotate_primary": true, "name": "new-primary" }))
    .send()
    .await
    .expect("POST rotate token");
    assert_eq!(
        response.status(),
        201,
        "{}",
        response.text().await.unwrap_or_default()
    );
    let body: serde_json::Value = response.json().await.expect("token json");
    let new_token = body["token"].as_str().expect("token string").to_string();
    assert!(new_token.starts_with("vx_"), "token prefix");
    assert_eq!(body["name"], serde_json::json!("new-primary"));

    // Old token is now rejected.
    let response = http
        .get(format!("{base}/v1/health"))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {IN_PROCESS_TOKEN}"),
        )
        .send()
        .await
        .expect("old token probe");
    assert_eq!(
        response.status(),
        401,
        "old token must be rejected after rotation"
    );

    // New token is accepted.
    let response = http
        .get(format!("{base}/v1/health"))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {new_token}"),
        )
        .send()
        .await
        .expect("new token probe");
    assert_eq!(response.status(), 200, "new token must be accepted");
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_ticket_mint_and_one_use_expiry_shape() {
    let (state, _engine) = test_state();
    let addr = serve_router(state.clone()).await;
    let base = format!("http://{addr}");
    let http = http_client();

    // Mint a ticket (no session binding).
    let response = req(
        &http,
        reqwest::Method::POST,
        &format!("{base}/v1/auth/ws-ticket"),
    )
    .json(&serde_json::json!({}))
    .send()
    .await
    .expect("POST ws-ticket");
    assert_eq!(response.status(), 201);
    let body: serde_json::Value = response.json().await.expect("ticket json");
    let ticket = body["ticket"].as_str().expect("ticket string");
    assert!(ticket.starts_with("vx_"), "ticket has vx_ prefix");
    assert!(body["expires_at"].as_str().is_some(), "expires_at present");

    // The ticket is now in the service map (not yet consumed).
    let map_size = state.ws_tickets.lock().unwrap().len();
    assert_eq!(map_size, 1, "ticket in registry");

    // Consume the ticket via the state helper.
    let valid = state.consume_ticket(ticket, "any-session");
    assert!(valid, "first consume should succeed");

    // A second consume of the same ticket fails.
    let valid2 = state.consume_ticket(ticket, "any-session");
    assert!(!valid2, "second consume must fail (one-use)");
}

// ---------------------------------------------------------------------------
// POST /v1/transcriptions (batch) — admission, WAV validation, caps
// ---------------------------------------------------------------------------

/// Build a minimal valid PCM16 WAV with the given number of 16 kHz mono samples.
fn make_wav_pcm16(samples: u32) -> Vec<u8> {
    let data_bytes = samples * 2; // 16-bit = 2 bytes/sample
    let mut buf = Vec::with_capacity(44 + data_bytes as usize);
    // RIFF header
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    // fmt chunk
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&1u16.to_le_bytes()); // mono
    buf.extend_from_slice(&16000u32.to_le_bytes()); // 16 kHz
    buf.extend_from_slice(&32000u32.to_le_bytes()); // byte rate
    buf.extend_from_slice(&2u16.to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits/sample
                                                 // data chunk
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_bytes.to_le_bytes());
    buf.extend(vec![0u8; data_bytes as usize]);
    buf
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_admission_202_job_lifecycle() {
    let (state, _engine) = test_state();
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();

    // A tiny valid WAV (100 samples = 6.25 ms at 16 kHz).
    let wav = make_wav_pcm16(100);

    let response = req(
        &http,
        reqwest::Method::POST,
        &format!("{base}/v1/transcriptions"),
    )
    .header("content-type", "audio/wav")
    .body(wav)
    .send()
    .await
    .expect("POST transcriptions");
    assert_eq!(
        response.status(),
        202,
        "{}",
        response.text().await.unwrap_or_default()
    );
    let body: serde_json::Value = response.json().await.expect("batch json");
    assert_eq!(body["state"], serde_json::json!("processing"));
    let job_id = body["job_id"].as_str().expect("job_id").to_string();
    assert!(body["status_url"].as_str().unwrap_or("").contains(&job_id));

    // Give the background job a moment to complete (TestEngine is instant).
    tokio::time::sleep(Duration::from_millis(200)).await;

    // GET the job status.
    let response = req(
        &http,
        reqwest::Method::GET,
        &format!("{base}/v1/transcriptions/{job_id}"),
    )
    .send()
    .await
    .expect("GET transcription");
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.expect("job status json");
    // TestEngine batch_transcribe returns "". State should be completed.
    assert_eq!(body["state"], serde_json::json!("completed"), "{body}");
    assert!(body["result"].is_object(), "result present");
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_hostile_wav_rejected_pre_allocation() {
    let (state, _engine) = test_state();
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();

    // Not a WAV at all.
    let response = req(
        &http,
        reqwest::Method::POST,
        &format!("{base}/v1/transcriptions"),
    )
    .header("content-type", "audio/wav")
    .body(b"this is not a wav file at all".to_vec())
    .send()
    .await
    .expect("POST bad wav");
    assert_eq!(response.status(), 400);
    let body: serde_json::Value = response.json().await.expect("error json");
    assert_eq!(body["error"]["code"], serde_json::json!("invalid-audio"));

    // WAV claiming 64 MiB data but actual body is tiny (truncated).
    let mut truncated_wav = make_wav_pcm16(100);
    // Overwrite the data chunk size field to claim 64 MiB.
    let data_chunk_pos = truncated_wav.len() - 100 * 2 - 4;
    let big_size = (64u32 * 1024 * 1024).to_le_bytes();
    truncated_wav[data_chunk_pos..data_chunk_pos + 4].copy_from_slice(&big_size);
    let response = req(
        &http,
        reqwest::Method::POST,
        &format!("{base}/v1/transcriptions"),
    )
    .header("content-type", "audio/wav")
    .body(truncated_wav)
    .send()
    .await
    .expect("POST truncated wav");
    assert_eq!(response.status(), 400);
    let body: serde_json::Value = response.json().await.expect("error json");
    assert_eq!(body["error"]["code"], serde_json::json!("invalid-audio"));

    // Bad audio format (0x0002 = ADPCM, not supported).
    let mut bad_fmt = make_wav_pcm16(100);
    // fmt chunk audio_format is at byte 20 (8-byte RIFF + 4-byte WAVE + 8-byte fmt header = 20).
    bad_fmt[20] = 0x02;
    bad_fmt[21] = 0x00;
    let response = req(
        &http,
        reqwest::Method::POST,
        &format!("{base}/v1/transcriptions"),
    )
    .header("content-type", "audio/wav")
    .body(bad_fmt)
    .send()
    .await
    .expect("POST bad format wav");
    assert_eq!(response.status(), 400);
    let body: serde_json::Value = response.json().await.expect("error json");
    assert_eq!(body["error"]["code"], serde_json::json!("invalid-audio"));
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_queue_full_returns_429() {
    let (state, _engine) = test_state();
    // Hold the command slot ourselves so the next batch → 429 immediately.
    let slot = state.command_slot.clone().try_lock_owned().expect("slot");
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();

    let wav = make_wav_pcm16(100);
    let response = req(
        &http,
        reqwest::Method::POST,
        &format!("{base}/v1/transcriptions"),
    )
    .header("content-type", "audio/wav")
    .body(wav)
    .send()
    .await
    .expect("POST transcriptions while slot held");
    assert_eq!(response.status(), 429);
    let body: serde_json::Value = response.json().await.expect("error json");
    assert_eq!(body["error"]["code"], serde_json::json!("queue-full"));
    assert_eq!(body["error"]["retryable"], serde_json::json!(true));
    drop(slot);
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_delete_cancel_and_settled_409() {
    let (state, _engine) = test_state();
    let addr = serve_router(state.clone()).await;
    let base = format!("http://{addr}");
    let http = http_client();

    // Submit a tiny WAV job and wait for it to complete.
    let wav = make_wav_pcm16(100);
    let response = req(
        &http,
        reqwest::Method::POST,
        &format!("{base}/v1/transcriptions"),
    )
    .header("content-type", "audio/wav")
    .body(wav)
    .send()
    .await
    .expect("POST transcriptions");
    assert_eq!(response.status(), 202);
    let body: serde_json::Value = response.json().await.expect("job json");
    let job_id = body["job_id"].as_str().expect("job_id").to_string();

    // TestEngine is instant; give the background task a moment.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // DELETE a settled job → 409 already settled.
    let response = req(
        &http,
        reqwest::Method::DELETE,
        &format!("{base}/v1/transcriptions/{job_id}"),
    )
    .send()
    .await
    .expect("DELETE settled transcription");
    assert_eq!(response.status(), 409);
    let body: serde_json::Value = response.json().await.expect("409 json");
    assert_eq!(body["error"]["code"], serde_json::json!("job_settled"));

    // DELETE unknown → 404.
    let response = req(
        &http,
        reqwest::Method::DELETE,
        &format!("{base}/v1/transcriptions/ghost-id"),
    )
    .send()
    .await
    .expect("DELETE unknown transcription");
    assert_eq!(response.status(), 404);

    // Inject a processing job directly and cancel it.
    let jid2 = uuid::Uuid::new_v4().to_string();
    state.register_job(voxix_app_lib::service::state::BatchJob {
        job_id: jid2.clone(),
        state: voxix_app_lib::service::state::BatchJobState::Processing,
        result: None,
        error: None,
        created_at_ms: 0,
    });
    let response = req(
        &http,
        reqwest::Method::DELETE,
        &format!("{base}/v1/transcriptions/{jid2}"),
    )
    .send()
    .await
    .expect("DELETE processing job");
    assert_eq!(response.status(), 204);
    let job = state.get_job(&jid2).expect("job still in registry");
    assert_eq!(
        job.state,
        voxix_app_lib::service::state::BatchJobState::Cancelled
    );
}

// ---------------------------------------------------------------------------
// GET /v1/history  (service-owned SQLite store)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn history_empty_and_unavailable_shape() {
    // history is None if the DB open failed; use a state with a real DB dir
    let (state, _engine) = test_state();
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();

    let response = req(&http, reqwest::Method::GET, &format!("{base}/v1/history"))
        .send()
        .await
        .expect("GET history");
    // Either 200 (DB opened fine) or 503 (DB init failed in temp dir).
    assert!(
        [200u16, 503].contains(&response.status().as_u16()),
        "unexpected status {}",
        response.status()
    );
    if response.status() == 200 {
        let body: serde_json::Value = response.json().await.expect("history json");
        assert!(body["entries"].is_array());
        assert_eq!(body["entries"].as_array().unwrap().len(), 0);
        assert_eq!(body["has_more"], serde_json::json!(false));
    }
}

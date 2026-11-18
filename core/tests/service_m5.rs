//! M5 integration suite (SDK plan §12 M5 + §15 S5; M3 polish debt).
//!
//! Covers the S5 pause-reaping runtime behavior (pause_max_ms budget,
//! ping-refreshed) and the streamed-final history write parity with batch.
//! Pure decision-matrix tests live in `service::ws::tests`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::WebSocketStream;

use voxix_app_lib::cli::TokenStdoutMode;
use voxix_app_lib::service::audio::{encode_audio_frame, AudioFrame, SAMPLE_RATE_HZ};
use voxix_app_lib::service::auth::sha256_digest;
use voxix_app_lib::service::config::{ServiceConfig, ServiceMode};
use voxix_app_lib::service::engine::TestEngine;
use voxix_app_lib::service::routes;
use voxix_app_lib::service::state::ServiceState;

const TOKEN: &str = "vx_m5_in_process_suite_token_0001";
const READ_EVENT_TIMEOUT: Duration = Duration::from_secs(8);
const READ_CLOSE_TIMEOUT: Duration = Duration::from_secs(12);
const FRAME_MS: u16 = 20;
const FRAME_SAMPLES: usize = (SAMPLE_RATE_HZ as usize) * FRAME_MS as usize / 1000;

fn temp_data_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "voxix-m5-{tag}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().as_simple()
    ));
    std::fs::create_dir_all(&dir).expect("create temp data dir");
    dir
}

fn in_process_config(resume_window_ms: u64) -> ServiceConfig {
    ServiceConfig {
        mode: ServiceMode::Standalone,
        bind: "127.0.0.1:0".to_string(),
        data_dir: temp_data_dir("ip"),
        token_stdout: TokenStdoutMode::Never,
        max_sessions: 4,
        max_audio_seconds: 1800,
        opus_enabled: false,
        session_resume_window_ms: resume_window_ms,
    }
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

fn test_state(resume_window_ms: u64) -> (Arc<ServiceState>, Arc<TestEngine>) {
    let engine = Arc::new(TestEngine::default());
    let (tx, _rx) = voxix_app_lib::service::stream_text_bus();
    let state = Arc::new(ServiceState::new(
        in_process_config(resume_window_ms),
        engine.clone() as _,
        Some(sha256_digest(TOKEN)),
        None,
        tx,
    ));
    (state, engine)
}

fn req(http: &reqwest::Client, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
    http.request(method, url).header(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&format!("Bearer {TOKEN}")).expect("bearer"),
    )
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("http client")
}

async fn create_session(http: &reqwest::Client, base: &str) -> (String, String) {
    let response = req(http, reqwest::Method::POST, &format!("{base}/v1/sessions"))
        .json(&serde_json::json!({ "task": "transcribe" }))
        .send()
        .await
        .expect("POST sessions");
    assert_eq!(response.status(), 201, "create session");
    let payload: serde_json::Value = response.json().await.expect("create body json");
    (
        payload["session_id"]
            .as_str()
            .expect("session_id")
            .to_string(),
        payload["session_token"]
            .as_str()
            .expect("session_token")
            .to_string(),
    )
}

async fn get_session(
    http: &reqwest::Client,
    base: &str,
    id: &str,
    session_token: &str,
) -> serde_json::Value {
    let response = req(
        http,
        reqwest::Method::GET,
        &format!("{base}/v1/sessions/{id}"),
    )
    .header("X-Session-Token", session_token)
    .send()
    .await
    .expect("GET session");
    assert_eq!(response.status(), 200, "get session");
    response.json().await.expect("session json")
}

fn text_frame(seq: u32) -> AudioFrame {
    AudioFrame {
        seq: seq as u64,
        duration_ms: FRAME_MS,
        discontinuity: false,
        pcm: vec![0; FRAME_SAMPLES],
    }
}

#[derive(Debug, Clone)]
struct WireEvent {
    kind: String,
    payload: serde_json::Value,
}

struct WsSession {
    sink: futures_util::stream::SplitSink<WebSocketStream<tokio::net::TcpStream>, WsMessage>,
    stream: futures_util::stream::SplitStream<WebSocketStream<tokio::net::TcpStream>>,
}

async fn open_stream(addr: SocketAddr, session_id: &str, session_token: &str) -> WsSession {
    let tcp = tokio::net::TcpStream::connect(addr)
        .await
        .expect("tcp connect");
    let mut request = format!("ws://{addr}/v1/sessions/{session_id}/stream")
        .into_client_request()
        .expect("ws request");
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {TOKEN}")).expect("bearer"),
    );
    request.headers_mut().insert(
        "x-session-token",
        HeaderValue::from_str(session_token).expect("session token"),
    );
    let (socket, _) = tokio_tungstenite::client_async(request, tcp)
        .await
        .expect("upgrade");
    let (sink, stream) = socket.split();
    WsSession { sink, stream }
}

impl WsSession {
    async fn send_json(&mut self, value: serde_json::Value) {
        self.sink
            .send(WsMessage::text(value.to_string()))
            .await
            .expect("send text frame");
    }

    async fn start(&mut self) {
        self.send_json(serde_json::json!({"v":1,"type":"start","payload":{}}))
            .await;
    }

    async fn ping(&mut self, nonce: &str) {
        self.send_json(serde_json::json!({"v":1,"type":"ping","payload":{"nonce":nonce}}))
            .await;
    }

    async fn pause(&mut self) {
        self.send_json(serde_json::json!({"v":1,"type":"pause","payload":{}}))
            .await;
    }

    async fn resume(&mut self) {
        self.send_json(serde_json::json!({"v":1,"type":"resume","payload":{}}))
            .await;
    }

    async fn stop(&mut self, reason: &str) {
        self.send_json(serde_json::json!({"v":1,"type":"stop","payload":{"reason":reason}}))
            .await;
    }

    async fn send_frame(&mut self, frame: &AudioFrame) {
        self.sink
            .send(WsMessage::Binary(encode_audio_frame(frame).into()))
            .await
            .expect("send binary frame");
    }

    async fn next(&mut self) -> Option<WsMessage> {
        tokio::time::timeout(READ_EVENT_TIMEOUT, self.stream.next())
            .await
            .ok()
            .flatten()
            .and_then(|r| r.ok())
    }

    async fn read_until(&mut self, kind: &str) -> WireEvent {
        loop {
            let message = self
                .next()
                .await
                .unwrap_or_else(|| panic!("stream ended before a '{kind}' event"));
            if let WsMessage::Text(text) = &message {
                let value: serde_json::Value =
                    serde_json::from_str(text.as_str()).expect("event json");
                let event_kind = value["type"].as_str().expect("event type").to_string();
                if event_kind == kind {
                    return WireEvent {
                        kind: event_kind,
                        payload: value["payload"].clone(),
                    };
                }
            }
        }
    }

    async fn events_until_close(&mut self) -> Option<u16> {
        loop {
            let message = tokio::time::timeout(READ_CLOSE_TIMEOUT, self.stream.next())
                .await
                .ok()
                .flatten();
            let Some(message) = message else {
                return None;
            };
            match message {
                Ok(WsMessage::Close(frame)) => return frame.map(|f| u16::from(f.code)),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// §15 S5 pause reaper
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn pause_max_ms_reaps_a_long_paused_session_with_4003() {
    let (state, engine) = test_state(30_000);
    engine.set_final_text("unused");
    // Opt into the hard pause cap; the lease is held from create, so only
    // this opt-in may end the session (S5 "never while a lease is held"
    // applies to the window rule, not the cap).
    {
        let mut patch = state.patch_config.lock().unwrap_or_else(|e| e.into_inner());
        patch.pause_max_ms = 900;
    }
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();

    let (session_id, session_token) = create_session(&http, &base).await;
    let mut ws = open_stream(addr, &session_id, &session_token).await;
    let _hello = ws.read_until("hello").await;
    ws.start().await;
    let _started = ws.read_until("started").await;
    ws.send_frame(&text_frame(0)).await;
    // The pause barrier flips the reader phase immediately; let the FIFO
    // intake settle first so the pre-pause frame is not raced (the reactive
    // audio barrier treats in-flight frames as a §15 S5 violation).
    tokio::time::sleep(Duration::from_millis(150)).await;
    ws.pause().await;
    let paused = ws.read_until("paused").await;
    assert_eq!(paused.payload["last_audio_seq"], serde_json::json!(0));

    // No ping at all: the budget expires after 900 ms.
    let error = ws.read_until("error").await;
    assert_eq!(error.payload["code"], serde_json::json!("stale_resume"));
    let close = ws.events_until_close().await;
    assert_eq!(close, Some(4003));

    // The record left the registry through the ordinary teardown path.
    let projection = get_session(&http, &base, &session_id, &session_token).await;
    assert!(
        projection["state"].as_str() != Some("created"),
        "the session must be terminal after the reap: {projection}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_ping_refreshes_the_pause_budget() {
    let (state, engine) = test_state(30_000);
    engine.set_final_text("after refresh");
    {
        let mut patch = state.patch_config.lock().unwrap_or_else(|e| e.into_inner());
        patch.pause_max_ms = 900;
    }
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();

    let (session_id, session_token) = create_session(&http, &base).await;
    let mut ws = open_stream(addr, &session_id, &session_token).await;
    let _hello = ws.read_until("hello").await;
    ws.start().await;
    let _started = ws.read_until("started").await;

    // Stay paused well beyond the single budget (3×700ms = 2.1s > 900ms),
    // refreshing only through client pings.
    ws.pause().await;
    let _paused = ws.read_until("paused").await;
    for n in 0..3 {
        tokio::time::sleep(Duration::from_millis(600)).await;
        ws.ping(&format!("n{n}")).await;
        let pong = ws.read_until("pong").await;
        assert_eq!(pong.payload["nonce"], serde_json::json!(format!("n{n}")));
    }

    // The session survived; work resumes and finishes normally.
    ws.resume().await;
    ws.send_frame(&text_frame(0)).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    ws.stop("user").await;
    let final_event = ws.read_until("final").await;
    assert_eq!(
        final_event.payload["text"],
        serde_json::json!("after refresh")
    );
    let close = ws.events_until_close().await;
    assert_eq!(close, Some(1000));
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_reaper_disable_default_keeps_leased_sessions_alive() {
    let (state, engine) = test_state(30_000);
    engine.set_final_text("still alive");
    // Default: pause_max_ms = 0 (disabled). A paused lease-held session is
    // never reaped by the window rule (S5); the socket lives on.
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();

    let (session_id, session_token) = create_session(&http, &base).await;
    let mut ws = open_stream(addr, &session_id, &session_token).await;
    let _hello = ws.read_until("hello").await;
    ws.start().await;
    let _started = ws.read_until("started").await;
    ws.send_frame(&text_frame(0)).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    ws.pause().await;
    let _paused = ws.read_until("paused").await;

    // Wait clearly past the resume window: nothing may arrive.
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    // Leave the paused phase (stop is valid only while streaming) and
    // finalize normally — the "still alive" proof of no hidden reap.
    ws.resume().await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    ws.stop("user").await;
    let final_event = ws.read_until("final").await;
    assert_eq!(
        final_event.payload["text"],
        serde_json::json!("still alive")
    );
    let projection = get_session(&http, &base, &session_id, &session_token).await;
    assert_eq!(projection["state"], serde_json::json!("completed"));
}

// ---------------------------------------------------------------------------
// Streamed-final history write (batch parity)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn streamed_final_persists_a_stream_history_row() {
    let (state, engine) = test_state(30_000);
    engine.set_final_text("streamed hello");
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();

    let (session_id, session_token) = create_session(&http, &base).await;
    let mut ws = open_stream(addr, &session_id, &session_token).await;
    let _hello = ws.read_until("hello").await;
    ws.start().await;
    let started = ws.read_until("started").await;
    let model_id = started.payload["model_id"].as_str().expect("model id");
    ws.send_frame(&text_frame(0)).await;
    ws.stop("user").await;
    let final_event = ws.read_until("final").await;
    let history_id = final_event.payload["history_id"]
        .as_i64()
        .expect("history id");
    let _close = ws.events_until_close().await;

    // The same surface batch rows land in: GET /v1/history.
    let response = req(&http, reqwest::Method::GET, &format!("{base}/v1/history"))
        .send()
        .await
        .expect("GET history");
    assert_eq!(response.status(), 200, "history readable");
    let page: serde_json::Value = response.json().await.expect("history json");
    let entries = page["entries"].as_array().expect("entries").clone();
    assert_eq!(entries.len(), 1, "exactly one streamed row: {page}");
    let row = &entries[0];
    assert_eq!(row["id"], serde_json::json!(history_id));
    assert_eq!(
        row["transcription_text"],
        serde_json::json!("streamed hello")
    );
    assert_eq!(row["source"], serde_json::json!("stream"));
    assert_eq!(row["model_id"], serde_json::json!(model_id));
    assert!(row["duration_ms"].as_i64().is_some(), "duration recorded");

    // The terminal `final` projection matches the written row.
    let projection = get_session(&http, &base, &session_id, &session_token).await;
    assert_eq!(projection["state"], serde_json::json!("completed"));
    assert_eq!(
        projection["final_result"]["history_id"], page["entries"][0]["id"],
        "the final envelope carries the history id"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn streamed_final_respects_save_history_default_off() {
    let (state, engine) = test_state(30_000);
    engine.set_final_text("not persisted");
    {
        let mut patch = state.patch_config.lock().unwrap_or_else(|e| e.into_inner());
        patch.save_history_default = false;
    }
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();

    let (session_id, session_token) = create_session(&http, &base).await;
    let mut ws = open_stream(addr, &session_id, &session_token).await;
    let _hello = ws.read_until("hello").await;
    ws.start().await;
    let _started = ws.read_until("started").await;
    ws.send_frame(&text_frame(0)).await;
    ws.stop("user").await;
    let final_event = ws.read_until("final").await;
    // No row → the envelope's history_id stays null.
    assert!(final_event.payload["history_id"].is_null(), "no row, no id");
    let _close = ws.events_until_close().await;

    let response = req(&http, reqwest::Method::GET, &format!("{base}/v1/history"))
        .send()
        .await
        .expect("GET history");
    if response.status() == 200 {
        let page: serde_json::Value = response.json().await.expect("history json");
        assert_eq!(
            page["entries"].as_array().expect("entries").len(),
            0,
            "save_history_default=false must not persist streamed finals: {page}"
        );
    }
}

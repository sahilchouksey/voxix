//! M2 streaming integration suite (SDK plan §6/§11/§12-M2/§15).
//!
//! Full Axum router + tokio-tungstenite WS client over the deterministic
//! TestEngine. Covers the M2 acceptance rows: ordered hello→started once per
//! session, single-socket admission refusal (503 + Retry-After, the HTTP
//! mirror of the WS 1013 overflow), cumulative acks, final-only turns,
//! client-VAD mode, pause/ping/resume flow, cancel semantics, the §6 gap
//! rules, and the exactly-one-terminal invariant.

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

const IN_PROCESS_TOKEN: &str = "vx_m2_in_process_suite_token_63a1";
const READ_EVENT_TIMEOUT: Duration = Duration::from_secs(8);
const READ_CLOSE_TIMEOUT: Duration = Duration::from_secs(12);
const FRAME_MS: u16 = 20;
const FRAME_SAMPLES: usize = (SAMPLE_RATE_HZ as usize) * FRAME_MS as usize / 1000;

fn temp_data_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "voxix-m2-{tag}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().as_simple()
    ));
    std::fs::create_dir_all(&dir).expect("create temp data dir");
    dir
}

fn in_process_config(max_sessions: u32) -> ServiceConfig {
    ServiceConfig {
        mode: ServiceMode::Standalone,
        bind: "127.0.0.1:0".to_string(),
        data_dir: temp_data_dir("ip"),
        token_stdout: TokenStdoutMode::Never,
        max_sessions,
        max_audio_seconds: 1800,
        opus_enabled: false,
        session_resume_window_ms: 30_000,
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

fn test_state(max_sessions: u32, streaming: bool) -> (Arc<ServiceState>, Arc<TestEngine>) {
    let engine = Arc::new(TestEngine::default());
    engine.set_streaming(streaming);
    let (tx, _rx) = voxix_app_lib::service::stream_text_bus();
    let state = Arc::new(ServiceState::new(
        in_process_config(max_sessions),
        engine.clone() as _,
        Some(sha256_digest(IN_PROCESS_TOKEN)),
        None,
        tx,
    ));
    (state, engine)
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("http client")
}

async fn create_session(
    http: &reqwest::Client,
    base: &str,
    body: serde_json::Value,
) -> (String, String) {
    let response = http
        .post(format!("{base}/v1/sessions"))
        .header(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str("Bearer vx_m2_in_process_suite_token_63a1")
                .expect("bearer"),
        )
        .json(&body)
        .send()
        .await
        .expect("POST sessions");
    assert_eq!(
        response.status(),
        201,
        "create session: {}",
        response.text().await.unwrap_or_default()
    );
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
    token: &str,
) -> serde_json::Value {
    let response = http
        .get(format!("{base}/v1/sessions/{id}"))
        .header(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str("Bearer vx_m2_in_process_suite_token_63a1")
                .expect("bearer"),
        )
        .header("x-session-token", token)
        .send()
        .await
        .expect("GET session");
    assert_eq!(response.status(), 200, "GET session status");
    response.json().await.expect("session json")
}

fn text_frame(seq: u32, discontinuity: bool) -> AudioFrame {
    AudioFrame {
        seq: seq as u64,
        duration_ms: FRAME_MS,
        discontinuity,
        pcm: vec![0; FRAME_SAMPLES],
    }
}

fn voiced_frame(seq: u32, discontinuity: bool) -> AudioFrame {
    AudioFrame {
        seq: seq as u64,
        duration_ms: FRAME_MS,
        discontinuity,
        pcm: vec![0x2f00; FRAME_SAMPLES],
    }
}

struct WsSession {
    sink: futures_util::stream::SplitSink<WebSocketStream<tokio::net::TcpStream>, WsMessage>,
    stream: futures_util::stream::SplitStream<WebSocketStream<tokio::net::TcpStream>>,
}

#[derive(Debug, Clone)]
struct WireEvent {
    kind: String,
    seq: u64,
    payload: serde_json::Value,
}

async fn ws_upgrade(
    addr: SocketAddr,
    session_id: &str,
    session_token: &str,
) -> Result<
    (
        WebSocketStream<tokio::net::TcpStream>,
        tokio_tungstenite::tungstenite::handshake::client::Response,
    ),
    tokio_tungstenite::tungstenite::Error,
> {
    let tcp = tokio::net::TcpStream::connect(addr)
        .await
        .expect("tcp connect");
    let mut request = format!("ws://{addr}/v1/sessions/{session_id}/stream")
        .into_client_request()
        .expect("ws request");
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {IN_PROCESS_TOKEN}")).expect("bearer"),
    );
    request.headers_mut().insert(
        "x-session-token",
        HeaderValue::from_str(session_token).expect("session token"),
    );
    tokio_tungstenite::client_async(request, tcp).await
}

async fn open_stream(addr: SocketAddr, session_id: &str, session_token: &str) -> WsSession {
    let (socket, response) = ws_upgrade(addr, session_id, session_token)
        .await
        .unwrap_or_else(|e| panic!("upgrade failed: {e}"));
    let _ = response;
    let (sink, stream) = socket.split();
    WsSession { sink, stream }
}

impl WsSession {
    async fn send_json(&mut self, value: serde_json::Value) {
        let message = WsMessage::text(value.to_string());
        self.sink.send(message).await.expect("send text frame");
    }

    async fn send_frame(&mut self, frame: &AudioFrame) {
        let bytes = encode_audio_frame(frame);
        self.sink
            .send(WsMessage::Binary(bytes.into()))
            .await
            .expect("send binary frame");
    }

    async fn start(&mut self) {
        self.start_with(serde_json::json!({})).await;
    }

    async fn start_with(&mut self, payload: serde_json::Value) {
        self.send_json(serde_json::json!({"v":1,"type":"start","payload":payload}))
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

    async fn cancel(&mut self) {
        self.send_json(serde_json::json!({"v":1,"type":"cancel","payload":{}}))
            .await;
    }

    async fn push_frames(&mut self, seq: u32, n: usize) -> u32 {
        let mut seq = seq;
        for _ in 0..n {
            self.send_frame(&text_frame(seq, false)).await;
            seq += 1;
        }
        seq
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
                .unwrap_or_else(|| panic!("stream ended before a '{kind}' event arrived"));
            if let Some(event) = parse_event(&message) {
                if event.kind == kind {
                    return event;
                }
            }
        }
    }

    async fn wait_ack(&mut self, through: u32) {
        loop {
            let event = self.read_until("audio_ack").await;
            let got = event.payload["through_seq"].as_u64().unwrap_or(0);
            if got >= through as u64 {
                return;
            }
        }
    }

    async fn events_until_close(&mut self) -> (Vec<WireEvent>, Option<u16>) {
        let mut events = Vec::new();
        loop {
            let message = tokio::time::timeout(READ_CLOSE_TIMEOUT, self.stream.next())
                .await
                .ok()
                .flatten();
            let Some(message) = message else {
                return (events, None);
            };
            let Ok(message) = message else {
                return (events, None);
            };
            match message {
                WsMessage::Close(frame) => return (events, frame.map(|f| u16::from(f.code))),
                _ => {
                    if let Some(event) = parse_event(&message) {
                        events.push(event);
                    }
                }
            }
        }
    }

    async fn close_client(&mut self) {
        let _ = self.sink.send(WsMessage::Close(None)).await;
    }
}

fn parse_event(message: &WsMessage) -> Option<WireEvent> {
    if let WsMessage::Text(text) = message {
        let value: serde_json::Value = serde_json::from_str(text.as_str()).ok()?;
        return Some(WireEvent {
            kind: value["type"].as_str()?.to_string(),
            seq: value["seq"].as_u64().unwrap_or(0),
            payload: value["payload"].clone(),
        });
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn happy_final_only_turn_streams_frames_acks_and_finalizes() {
    let (state, engine) = test_state(4, false);
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();
    engine.set_final_text("final hello");

    let (session_id, session_token) = create_session(
        &http,
        &base,
        serde_json::json!({
            "task": "transcribe",
            "partial_results": false,
        }),
    )
    .await;

    let mut ws = open_stream(addr, &session_id, &session_token).await;
    let hello = ws.read_until("hello").await;
    assert_eq!(hello.kind, "hello");
    assert_eq!(hello.seq, 1);
    assert_eq!(hello.payload["protocol_version"], serde_json::json!(1));
    assert_eq!(hello.payload["heartbeat_ms"], serde_json::json!(15000));
    assert_eq!(hello.payload["max_frame_bytes"], serde_json::json!(65536));
    assert_eq!(
        hello.payload["negotiated_audio"]["codec"],
        serde_json::json!("pcm_s16le")
    );
    assert_eq!(
        hello.payload["streaming_mode"],
        serde_json::json!("final_only")
    );

    ws.start().await;
    let started = ws.read_until("started").await;
    assert_eq!(started.seq, 2);
    assert_eq!(started.payload["resumed"], serde_json::json!(false));
    assert_eq!(started.payload["next_audio_seq"], serde_json::json!(0));
    assert_eq!(started.payload["resume_epoch"], serde_json::json!(1));

    // Second start inside a live turn gets the retryable service_busy.
    ws.start_with(serde_json::json!({})).await;
    let second_error = ws.read_until("error").await;
    assert_eq!(second_error.kind, "error");
    assert_eq!(
        second_error.payload["code"],
        serde_json::json!("service_busy")
    );
    assert_eq!(second_error.payload["retryable"], serde_json::json!(true));

    let next_seq = ws.push_frames(0, 10).await;
    assert_eq!(next_seq, 10);
    ws.wait_ack(9).await;

    ws.stop("user").await;
    let final_event = ws.read_until("final").await;
    assert_eq!(final_event.kind, "final");
    assert_eq!(
        final_event.payload["text"],
        serde_json::json!("final hello")
    );
    assert_eq!(final_event.payload["duration_ms"], serde_json::json!(200));
    assert!(final_event.payload["metrics"]["decode_ms"].is_u64());

    let (events, close_code) = ws.events_until_close().await;
    assert_eq!(close_code, Some(1000));
    assert!(
        !events.iter().any(|e| e.kind == "final"),
        "the final was already consumed once: {events:?}"
    );

    let projection = get_session(&http, &base, &session_id, &session_token).await;
    assert_eq!(projection["state"], serde_json::json!("completed"));
    assert_eq!(
        projection["final_result"]["text"],
        serde_json::json!("final hello")
    );
    assert_eq!(projection["audio_received_ms"], serde_json::json!(200));
    assert_eq!(projection["last_audio_seq"], serde_json::json!(9));
}

#[tokio::test(flavor = "multi_thread")]
async fn intake_overflow_closes_1013_with_service_busy() {
    let (state, engine) = test_state(4, false);
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();
    engine.set_final_text("final hello");
    // The decoder stalls far beyond the bounded intake wait (5 s). With
    // client VAD every frame is guaranteed to reach the engine feed, so the
    // FIFO intake channel (capacity 100) overflows with the 101st frame and
    // the session closes 1013 `service_busy` deterministically.
    engine.set_feed_delay(Some(Duration::from_secs(15)));

    let (session_id, session_token) = create_session(
        &http,
        &base,
        serde_json::json!({
            "task": "transcribe",
            "vad": { "mode": "client" },
        }),
    )
    .await;

    let mut ws = open_stream(addr, &session_id, &session_token).await;
    ws.read_until("hello").await;
    ws.start().await;
    let _started = ws.read_until("started").await;
    // The channel (capacity 100) admits frames 0..99 while the decoder holds
    // frame 0; frame 100 fills the last slot and frame 101 finds a full FIFO
    // whose only consumer is stalled — the bounded intake wait expires, the
    // client completes its push, and the overflow lands as the 1013 close.
    let _next_seq = ws.push_frames(0, 102).await;

    let error_event = ws.read_until("error").await;
    assert_eq!(error_event.kind, "error");
    assert_eq!(
        error_event.payload["code"],
        serde_json::json!("service_busy")
    );
    assert_eq!(error_event.payload["retryable"], serde_json::json!(true));

    let (events, close_code) = ws.events_until_close().await;
    assert_eq!(close_code, Some(1013));
    assert!(events.iter().all(|e| e.kind == "error"));

    let projection = get_session(&http, &base, &session_id, &session_token).await;
    assert_eq!(projection["state"], serde_json::json!("failed"));
}

#[tokio::test(flavor = "multi_thread")]
async fn client_vad_mode_feeds_every_frame_and_finalizes() {
    let (state, engine) = test_state(4, false);
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();
    engine.set_final_text("final hello");

    let (session_id, session_token) = create_session(
        &http,
        &base,
        serde_json::json!({
            "task": "transcribe",
            "vad": { "mode": "client" },
            "partial_results": false,
        }),
    )
    .await;

    let mut ws = open_stream(addr, &session_id, &session_token).await;
    let hello = ws.read_until("hello").await;
    assert_eq!(hello.kind, "hello");
    assert_eq!(
        hello.payload["streaming_mode"],
        serde_json::json!("final_only")
    );
    ws.start().await;
    let started = ws.read_until("started").await;
    assert_eq!(started.payload["backend"], serde_json::json!("test"));
    assert_eq!(started.payload["resume_epoch"], serde_json::json!(1));
    let _next_seq = ws.push_frames(0, 8).await;
    ws.wait_ack(7).await;
    assert_eq!(engine.feed_count(), 8, "client VAD mode feeds every frame");

    ws.stop("user").await;
    let final_event = ws.read_until("final").await;
    assert_eq!(
        final_event.payload["text"],
        serde_json::json!("final hello")
    );

    let (_events, close_code) = ws.events_until_close().await;
    assert_eq!(close_code, Some(1000));

    let projection = get_session(&http, &base, &session_id, &session_token).await;
    assert_eq!(projection["state"], serde_json::json!("completed"));
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_ping_resume_and_resume_frame_flow() {
    let (state, engine) = test_state(4, false);
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();
    engine.set_final_text("final hello");

    let (session_id, session_token) =
        create_session(&http, &base, serde_json::json!({ "task": "transcribe" })).await;

    let mut ws = open_stream(addr, &session_id, &session_token).await;
    let _hello = ws.read_until("hello").await;
    ws.start().await;
    let _started = ws.read_until("started").await;

    let _next_seq = ws.push_frames(0, 4).await;
    ws.wait_ack(3).await;
    ws.pause().await;
    let paused = ws.read_until("paused").await;
    assert_eq!(paused.kind, "paused");
    assert_eq!(paused.payload["last_audio_seq"], serde_json::json!(3));

    ws.ping("n1").await;
    let pong = ws.read_until("pong").await;
    assert_eq!(pong.kind, "pong");
    assert_eq!(pong.payload["nonce"], serde_json::json!("n1"));

    ws.resume().await;
    let _next_seq = ws.push_frames(4, 5).await;
    ws.wait_ack(8).await;

    ws.stop("user").await;
    let final_event = ws.read_until("final").await;
    assert_eq!(final_event.kind, "final");
    let (_events, close_code) = ws.events_until_close().await;
    assert_eq!(close_code, Some(1000));

    let projection = get_session(&http, &base, &session_id, &session_token).await;
    assert_eq!(projection["state"], serde_json::json!("completed"));
}

#[tokio::test(flavor = "multi_thread")]
async fn audio_while_paused_is_policy_rejected() {
    let (state, _engine) = test_state(4, false);
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();

    let (session_id, session_token) =
        create_session(&http, &base, serde_json::json!({ "task": "transcribe" })).await;

    let mut ws = open_stream(addr, &session_id, &session_token).await;
    let _hello = ws.read_until("hello").await;
    ws.start().await;
    let _started = ws.read_until("started").await;
    let _next_seq = ws.push_frames(0, 3).await;
    ws.wait_ack(2).await;
    ws.pause().await;
    let _paused = ws.read_until("paused").await;

    let _next_seq = ws.push_frames(3, 1).await;
    let error_event = ws.read_until("error").await;
    assert_eq!(
        error_event.payload["code"],
        serde_json::json!("invalid_request")
    );
    let (_events, close_code) = ws.events_until_close().await;
    assert_eq!(close_code, Some(1008));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_closes_cleanly_without_final() {
    let (state, engine) = test_state(4, false);
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();

    let (session_id, session_token) =
        create_session(&http, &base, serde_json::json!({ "task": "transcribe" })).await;

    let mut ws = open_stream(addr, &session_id, &session_token).await;
    let _hello = ws.read_until("hello").await;
    ws.start().await;
    let _started = ws.read_until("started").await;
    let _next_seq = ws.push_frames(0, 3).await;
    ws.wait_ack(2).await;

    ws.cancel().await;
    let (events, close_code) = ws.events_until_close().await;
    assert_eq!(close_code, Some(1000));
    assert!(
        !events.iter().any(|e| e.kind == "final"),
        "cancel suppresses the final: {events:?}"
    );

    let projection = get_session(&http, &base, &session_id, &session_token).await;
    assert_eq!(projection["state"], serde_json::json!("cancelled"));
    assert_eq!(engine.batch_decode_count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn rest_delete_mid_stream_closes_1000_without_final() {
    let (state, _engine) = test_state(4, false);
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();

    let (session_id, session_token) =
        create_session(&http, &base, serde_json::json!({ "task": "transcribe" })).await;

    let mut ws = open_stream(addr, &session_id, &session_token).await;
    let _hello = ws.read_until("hello").await;
    ws.start().await;
    let _started = ws.read_until("started").await;
    let _next_seq = ws.push_frames(0, 2).await;
    ws.wait_ack(1).await;

    let response = http
        .delete(format!("{base}/v1/sessions/{session_id}"))
        .header(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str("Bearer vx_m2_in_process_suite_token_63a1")
                .expect("bearer"),
        )
        .header("x-session-token", session_token.clone())
        .send()
        .await
        .expect("DELETE session");
    assert_eq!(response.status(), 204);

    let (events, close_code) = ws.events_until_close().await;
    assert_eq!(close_code, Some(1000));
    assert!(!events.iter().any(|e| e.kind == "final"));

    let projection = get_session(&http, &base, &session_id, &session_token).await;
    assert_eq!(projection["state"], serde_json::json!("cancelled"));
}

#[tokio::test(flavor = "multi_thread")]
async fn exactly_one_terminal_after_stop_and_late_cancel() {
    let (state, engine) = test_state(4, false);
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();
    engine.set_final_text("final hello");

    // Stop then cancel: the FIFO finalize barrier makes the cancel land after
    // the finalize started, so the final is the one terminal.
    let (session_id, session_token) =
        create_session(&http, &base, serde_json::json!({ "task": "transcribe" })).await;
    let mut ws = open_stream(addr, &session_id, &session_token).await;
    let _hello = ws.read_until("hello").await;
    ws.start().await;
    let _started = ws.read_until("started").await;
    let _next_seq = ws.push_frames(0, 3).await;
    ws.wait_ack(2).await;
    ws.stop("user").await;
    ws.cancel().await;
    let final_event = ws.read_until("final").await;
    assert_eq!(final_event.kind, "final");
    let (events, close_code) = ws.events_until_close().await;
    assert_eq!(close_code, Some(1000));
    assert!(!events.iter().any(|e| e.kind == "final"));

    let projection = get_session(&http, &base, &session_id, &session_token).await;
    assert_eq!(projection["state"], serde_json::json!("completed"));
}

#[tokio::test(flavor = "multi_thread")]
async fn gap_without_discontinuity_is_audio_gap() {
    let (state, _engine) = test_state(4, false);
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();

    let (session_id, session_token) =
        create_session(&http, &base, serde_json::json!({ "task": "transcribe" })).await;

    let mut ws = open_stream(addr, &session_id, &session_token).await;
    let _hello = ws.read_until("hello").await;
    ws.start().await;
    let _started = ws.read_until("started").await;
    let _next_seq = ws.push_frames(0, 3).await;
    ws.wait_ack(2).await;

    let _next_seq = ws.push_frames(5, 1).await;
    let error_event = ws.read_until("error").await;
    assert_eq!(error_event.payload["code"], serde_json::json!("audio_gap"));
    let (_events, close_code) = ws.events_until_close().await;
    assert_eq!(close_code, Some(4004));
}

#[tokio::test(flavor = "multi_thread")]
async fn gap_with_discontinuity_synthesizes_silence_and_continues() {
    let (state, engine) = test_state(4, false);
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();
    engine.set_final_text("final hello");

    let (session_id, session_token) =
        create_session(&http, &base, serde_json::json!({ "task": "transcribe" })).await;

    let mut ws = open_stream(addr, &session_id, &session_token).await;
    let _hello = ws.read_until("hello").await;
    ws.start().await;
    let _started = ws.read_until("started").await;
    let _next_seq = ws.push_frames(0, 5).await;
    ws.wait_ack(4).await;

    // Jump ahead with a discontinuity flag: bounded silence is synthesized
    // and the turn continues at the new sequence.
    ws.send_frame(&text_frame(12, true)).await;
    let _next_seq = ws.push_frames(13, 2).await;
    ws.wait_ack(14).await;

    ws.stop("user").await;
    let final_event = ws.read_until("final").await;
    assert_eq!(final_event.kind, "final");
    let (_events, close_code) = ws.events_until_close().await;
    assert_eq!(close_code, Some(1000));

    let projection = get_session(&http, &base, &session_id, &session_token).await;
    assert_eq!(projection["state"], serde_json::json!("completed"));
    // Frames 0,1,2 (real 60 ms) + the 180 ms gap silence for seqs 3..11
    // (flagged discontinuity) + frames 12,13,14 (real 60 ms): the received
    // accounting counts the stream span, so 300 ms in total.
    assert_eq!(projection["audio_received_ms"], serde_json::json!(300));
}

#[tokio::test(flavor = "multi_thread")]
async fn second_upgrade_is_refused_and_post_close_is_terminal() {
    let (state, _engine) = test_state(4, false);
    let addr = serve_router(state).await;
    let base = format!("http://{addr}");
    let http = http_client();

    let (session_id, session_token) =
        create_session(&http, &base, serde_json::json!({ "task": "transcribe" })).await;

    let mut first = open_stream(addr, &session_id, &session_token).await;
    let hello = first.read_until("hello").await;
    assert_eq!(hello.kind, "hello");

    let second = ws_upgrade(addr, &session_id, &session_token).await;
    assert!(matches!(
        second.as_ref().map_err(|e| e),
        Err(tokio_tungstenite::tungstenite::Error::Http(_))
    ));
    if let Err(tokio_tungstenite::tungstenite::Error::Http(response)) = second {
        assert_eq!(response.status(), 503);
        assert_eq!(
            response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok()),
            Some("1")
        );
    }

    first.close_client().await;
    // A client-initiated close may or may not surface the server's close
    // reply in this transport; what is observable is the teardown: the
    // disconnect-canceled session then refuses every fresh upgrade with 404.
    let _ = first.events_until_close().await;
    let mut final_status: Option<u16> = None;
    for _ in 0..40 {
        match ws_upgrade(addr, &session_id, &session_token).await {
            Ok(_reopened) => panic!("a terminated session must not accept an upgrade"),
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                final_status = Some(response.status().as_u16());
                if response.status().as_u16() == 404 {
                    break;
                }
            }
            Err(other) => panic!("unexpected upgrade error: {other}"),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(final_status, Some(404));
    let projection = get_session(&http, &base, &session_id, &session_token).await;
    assert_eq!(projection["state"], serde_json::json!("cancelled"));
}

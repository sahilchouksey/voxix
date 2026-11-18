//! M1 service integration suite (SDK plan §15 acceptance rows for M1).
//!
//! Two layers:
//! 1. **Process tests** run the real `voxix serve` binary via
//!    `CARGO_BIN_EXE_voxix` against an isolated `--data-dir`: token
//!    discovery.json publish/own-removal, `\--embed` ready-line +
//!    stdin-EOF lifetime lease, exit-code-73 contention, stale-document
//!    replacement, and the no-background-throttling policy helper.
//! 2. **In-process tests** build the full Axum router over the deterministic
//!    `TestEngine` and exercise admission (503+Retry-After), the
//!    `service_busy` envelope, lease release on cancel/finish, 404-on-token
//!    mismatch, and DTO shapes.

use std::io::{BufRead, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use voxix_app_lib::cli::TokenStdoutMode;
use voxix_app_lib::service::auth::sha256_digest;
use voxix_app_lib::service::config::{
    from_options, ServeOptions, ServiceConfig, ServiceMode, DEFAULT_BIND,
};
use voxix_app_lib::service::discovery::{self, DiscoveryDocument};
use voxix_app_lib::service::embed;
use voxix_app_lib::service::engine::TestEngine;
use voxix_app_lib::service::notify;
use voxix_app_lib::service::routes;
use voxix_app_lib::service::state::ServiceState;
use voxix_app_lib::service::EMBED_GRACE;

const BIN: &str = env!("CARGO_BIN_EXE_voxix");
const IN_PROCESS_TOKEN: &str = "vx_in_process_suite_token_4172";
const SESSION_TOKEN_HEADER: &str = "x-session-token";

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn temp_data_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "voxix-m1-{tag}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().as_simple()
    ));
    std::fs::create_dir_all(&dir).expect("create temp data dir");
    dir
}

fn state_dir_of(data_dir: &Path) -> PathBuf {
    data_dir.join("state")
}

/// Read the first line of a piped stdout with a deadline (blocking read on a
/// helper thread so a missing line cannot hang the suite).
fn read_first_line(stdout: impl std::io::Read + Send + 'static, timeout: Duration) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stdout);
        let mut line = String::new();
        if reader.read_line(&mut line).is_ok() && !line.trim().is_empty() {
            let _ = tx.send(line.trim().to_string());
        }
    });
    rx.recv_timeout(timeout)
        .unwrap_or_else(|_| panic!("expected one stdout line within {timeout:?}"))
}

/// Drain stderr after the child terminated (pipes cannot block then).
fn drain_stderr(child: &mut Child) -> String {
    let mut stderr = child.stderr.take().expect("stderr piped");
    let mut out = String::new();
    let _ = stderr.read_to_string(&mut out);
    out
}

fn spawn_serve(data_dir: &Path, args: &[&str]) -> Child {
    std::process::Command::new(BIN)
        .args([
            "serve",
            "--data-dir",
            data_dir.to_str().expect("utf8 data dir"),
        ])
        .args(args)
        .env("VOXIX_SERVICE_TEST_ENGINE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn voxix serve")
}

/// Poll an exit within the deadline; panics if it does not terminate.
fn wait_exit(child: &mut Child, timeout: Duration) -> Option<i32> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => return status.code(),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("process did not exit within {timeout:?}")
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn wait_for_discovery(data_dir: &Path, timeout: Duration) -> DiscoveryDocument {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(doc) = discovery::read_document(&state_dir_of(data_dir)) {
            return doc;
        }
        assert!(
            Instant::now() < deadline,
            "no discovery document published within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Wait until a discovery document published by `pid` appears (a bootstrap
/// phase can transiently publish foreign docs — e.g. a stale document we
/// planted — before the server publishes its own).
fn wait_for_discovery_owned_by(data_dir: &Path, pid: u32, timeout: Duration) -> DiscoveryDocument {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(doc) = discovery::read_document(&state_dir_of(data_dir)) {
            if doc.pid == pid {
                return doc;
            }
        }
        assert!(
            Instant::now() < deadline,
            "no discovery document owned by pid {pid} within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn sigterm(child: &Child) {
    unsafe {
        libc::kill(child.id() as libc::c_int, libc::SIGTERM);
    }
}

fn base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

fn bearer(value: &str) -> reqwest::header::HeaderValue {
    reqwest::header::HeaderValue::from_str(&format!("Bearer {value}")).expect("bearer header")
}

fn session_token_header(value: &str) -> reqwest::header::HeaderValue {
    reqwest::header::HeaderValue::from_str(value).expect("session token header")
}

/// Spawn the full router on an ephemeral loopback port (in-process tests).
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

fn test_state(max_sessions: u32) -> (Arc<ServiceState>, Arc<TestEngine>) {
    let engine = Arc::new(TestEngine::default());
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

async fn create_session(
    http: &reqwest::Client,
    base: &str,
    token: Option<&str>,
    body: serde_json::Value,
) -> reqwest::Response {
    let mut builder = http
        .post(format!("{base}/v1/sessions"))
        .header(reqwest::header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        builder = builder.header(reqwest::header::AUTHORIZATION, bearer(token));
    }
    builder
        .body(body.to_string())
        .send()
        .await
        .expect("POST sessions")
}

/// Spawn the standalone daemon, wait for discovery, and return (child, port,
/// token, document). With `--token-stdout always` the first stdout line is
/// the plaintext token.
fn spawn_and_ready(data_dir: &Path) -> (Child, u16, String, DiscoveryDocument) {
    let mut child = spawn_serve(
        data_dir,
        &["--bind", "127.0.0.1:0", "--token-stdout", "always"],
    );
    let line = read_first_line(
        child
            .stdout
            .take()
            .expect("--token-stdout always prints the plaintext once"),
        Duration::from_secs(30),
    );
    assert!(line.starts_with("vx_"), "first stdout line is the token");

    let doc = wait_for_discovery(data_dir, Duration::from_secs(30));
    assert_eq!(doc.protocol_version, 1);
    assert!(doc.pid > 0);
    let port = doc.port;
    (child, port, line, doc)
}

/// A pid that is guaranteed dead: the pid of an already-reaped subprocess.
fn dead_pid_for_test() -> u32 {
    let mut probe = std::process::Command::new("true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn probe");
    let pid = probe.id();
    let status = probe.wait().expect("reap probe");
    assert_eq!(status.code(), Some(0));
    pid
}

// ---------------------------------------------------------------------------
// In-process tests (full router over TestEngine)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn alive_is_public_and_health_requires_auth_with_engine_version() {
    let (state, _engine) = test_state(4);
    let base = base_url(serve_router(state).await.port());
    let http = reqwest::Client::new();

    // GET /alive is the only bare route.
    let alive = http.get(format!("{base}/alive")).send().await.unwrap();
    assert_eq!(alive.status(), reqwest::StatusCode::OK);
    assert_eq!(
        alive.json::<serde_json::Value>().await.unwrap()["alive"],
        true
    );

    // Health without, with a missing, and with a wrong credential: empty 401.
    for bad in [None, Some("vx_nope_watch")] {
        let mut builder = http.get(format!("{base}/v1/health"));
        if let Some(t) = bad {
            builder = builder.header(reqwest::header::AUTHORIZATION, bearer(t));
        }
        let response = builder.send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
        assert_eq!(response.text().await.unwrap(), "", "401 body is empty");
    }

    // Correct bearer: health carries the engine_version handoff (G6/G21).
    let health = http
        .get(format!("{base}/v1/health"))
        .header(reqwest::header::AUTHORIZATION, bearer(IN_PROCESS_TOKEN))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = health.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["engine_version"], "1.0.0+testsha");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["protocol_version"], 1);
    assert_eq!(body["ready"], true);
    assert!(body["uptime_ms"].as_u64().is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn status_config_and_capabilities_shape() {
    let (state, _engine) = test_state(8);
    let base = base_url(serve_router(state).await.port());
    let http = reqwest::Client::new();

    let status = http
        .get(format!("{base}/v1/status"))
        .header(reqwest::header::AUTHORIZATION, bearer(IN_PROCESS_TOKEN))
        .send()
        .await
        .unwrap();
    assert_eq!(status.status(), reqwest::StatusCode::OK);
    let status: serde_json::Value = status.json().await.unwrap();
    assert_eq!(status["mode"], "standalone");
    assert_eq!(status["active_sessions"], 0);
    assert_eq!(status["active_session_id"], serde_json::Value::Null);
    assert_eq!(status["lease_held"], false);
    assert_eq!(status["max_realtime_factor"], serde_json::Value::Null);
    assert_eq!(status["backend"], "test");
    let capabilities = &status["capabilities"];
    assert_eq!(capabilities["requires_server_vad"], false);
    assert_eq!(capabilities["prefers_auto_gain"], false);
    assert_eq!(capabilities["prefers_noise_reduction"], false);
    assert_eq!(capabilities["max_concurrent_streams"], 1);

    let config = http
        .get(format!("{base}/v1/config"))
        .header(reqwest::header::AUTHORIZATION, bearer(IN_PROCESS_TOKEN))
        .send()
        .await
        .unwrap();
    assert_eq!(config.status(), reqwest::StatusCode::OK);
    let config: serde_json::Value = config.json().await.unwrap();
    assert_eq!(config["max_sessions"], 8);
    assert_eq!(config["max_audio_seconds"], 1800);
    assert_eq!(config["session_resume_window_ms"], 30_000);
    assert_eq!(config["restart_required"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn admission_503_retry_after_and_release_on_cancel() {
    // Single admission slot: max_sessions = 1.
    let (state, _engine) = test_state(1);
    let base = base_url(serve_router(state).await.port());
    let http = reqwest::Client::new();

    // First session admits; the model alias and language defaults apply.
    let created = create_session(
        &http,
        &base,
        Some(IN_PROCESS_TOKEN),
        serde_json::json!({"model": "custom-model", "language": "de"}),
    )
    .await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = created.json().await.unwrap();
    assert!(session["session_id"].as_str().unwrap().len() > 10);
    assert!(session["session_token"]
        .as_str()
        .unwrap()
        .starts_with("vx_"));
    assert_eq!(session["state"], "created");
    assert_eq!(session["streaming_mode"], "final_only");
    assert!(session["ws_url"]
        .as_str()
        .unwrap()
        .contains(&session["session_id"].as_str().unwrap().to_string()));
    assert!(session["expires_at"].as_str().unwrap().ends_with("Z"));
    assert_eq!(
        session["negotiated_audio"],
        serde_json::json!({"codec":"pcm_s16le","sample_rate_hz":16000,"channels":1,"chunk_ms":20})
    );
    let (id, sub) = (
        session["session_id"].as_str().unwrap().to_string(),
        session["session_token"].as_str().unwrap().to_string(),
    );

    // Projection honors the request body's documented defaults.
    let got = http
        .get(format!("{base}/v1/sessions/{id}"))
        .header(reqwest::header::AUTHORIZATION, bearer(IN_PROCESS_TOKEN))
        .header(SESSION_TOKEN_HEADER, session_token_header(&sub))
        .send()
        .await
        .unwrap();
    assert_eq!(got.status(), reqwest::StatusCode::OK);
    let projection: serde_json::Value = got.json().await.unwrap();
    assert_eq!(projection["model_id"], "custom-model");
    assert_eq!(projection["language"], "de");

    // Second create: full => 503 + Retry-After without allocation.
    let rejected =
        create_session(&http, &base, Some(IN_PROCESS_TOKEN), serde_json::json!({})).await;
    assert_eq!(rejected.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(rejected.headers()["retry-after"], "30");
    let err: serde_json::Value = rejected.json().await.unwrap();
    assert_eq!(err["error"]["code"], "service_full");
    assert_eq!(err["error"]["retryable"], true);
    assert!(!err["error"]["request_id"].as_str().unwrap().is_empty());

    // Cancel releases both permits; the freed capacity admits again.
    let cancel = http
        .delete(format!("{base}/v1/sessions/{id}"))
        .header(reqwest::header::AUTHORIZATION, bearer(IN_PROCESS_TOKEN))
        .header(SESSION_TOKEN_HEADER, session_token_header(&sub))
        .send()
        .await
        .unwrap();
    assert_eq!(cancel.status(), reqwest::StatusCode::NO_CONTENT);
    let admitted =
        create_session(&http, &base, Some(IN_PROCESS_TOKEN), serde_json::json!({})).await;
    assert_eq!(admitted.status(), reqwest::StatusCode::CREATED);
}

#[tokio::test(flavor = "multi_thread")]
async fn service_busy_envelope_and_lease_release_on_cancel_and_finish() {
    // Inference is single-permit; admission has room, so the contended second
    // create exercises exactly the lease path (`service_busy`).
    let (state, engine) = test_state(8);
    let base = base_url(serve_router(state).await.port());
    let http = reqwest::Client::new();

    let created = create_session(&http, &base, Some(IN_PROCESS_TOKEN), serde_json::json!({}))
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let (id_a, token_a) = (
        created["session_id"].as_str().unwrap().to_string(),
        created["session_token"].as_str().unwrap().to_string(),
    );
    let opened = engine.snapshot().0;
    assert_eq!(opened, vec![id_a.clone()], "engine sees exactly one open");

    let busy = create_session(&http, &base, Some(IN_PROCESS_TOKEN), serde_json::json!({})).await;
    assert_eq!(busy.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(busy.headers()["retry-after"], "1");
    let err: serde_json::Value = busy.json().await.unwrap();
    assert_eq!(err["error"]["code"], "service_busy");
    assert_eq!(err["error"]["retryable"], true);

    // Cancel ends the lease; a fresh create fits again.
    http.delete(format!("{base}/v1/sessions/{id_a}"))
        .header(reqwest::header::AUTHORIZATION, bearer(IN_PROCESS_TOKEN))
        .header(SESSION_TOKEN_HEADER, session_token_header(&token_a))
        .send()
        .await
        .unwrap();
    let closed = engine.snapshot().1;
    assert_eq!(closed, vec![(id_a.clone(), false)]);
    let after_cancel = create_session(&http, &base, Some(IN_PROCESS_TOKEN), serde_json::json!({}))
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let (id_b, token_b) = (
        after_cancel["session_id"].as_str().unwrap().to_string(),
        after_cancel["session_token"].as_str().unwrap().to_string(),
    );

    // Finish (PATCH completed) also releases, so one more create fits.
    let finished = http
        .patch(format!("{base}/v1/sessions/{id_b}"))
        .header(reqwest::header::AUTHORIZATION, bearer(IN_PROCESS_TOKEN))
        .header(SESSION_TOKEN_HEADER, session_token_header(&token_b))
        .body(serde_json::json!({"state": "completed"}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(finished.status(), reqwest::StatusCode::OK);
    assert_eq!(
        finished.json::<serde_json::Value>().await.unwrap()["state"],
        "completed"
    );
    let closed = engine.snapshot().1;
    assert_eq!(closed[1], (id_b.clone(), true));

    let after_finish =
        create_session(&http, &base, Some(IN_PROCESS_TOKEN), serde_json::json!({})).await;
    assert_eq!(after_finish.status(), reqwest::StatusCode::CREATED);
}

#[tokio::test(flavor = "multi_thread")]
async fn session_token_mismatch_is_404_with_terminal_retention() {
    let (state, _engine) = test_state(8);
    let base = base_url(serve_router(state).await.port());
    let http = reqwest::Client::new();

    let created = create_session(&http, &base, Some(IN_PROCESS_TOKEN), serde_json::json!({}))
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let id = created["session_id"].as_str().unwrap().to_string();
    let token = created["session_token"].as_str().unwrap().to_string();

    // Wrong or missing sub-credential: indistinguishable from an unknown id
    // (404 with the session_not_found envelope, never 403).
    for sub in ["vx_forged_token_01234", ""] {
        let response = http
            .get(format!("{base}/v1/sessions/{id}"))
            .header(SESSION_TOKEN_HEADER, session_token_header(sub))
            .header(reqwest::header::AUTHORIZATION, bearer(IN_PROCESS_TOKEN))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
        let err: serde_json::Value = response.json().await.unwrap();
        assert_eq!(err["error"]["code"], "session_not_found");
    }
    let no_header = http
        .get(format!("{base}/v1/sessions/{id}"))
        .header(reqwest::header::AUTHORIZATION, bearer(IN_PROCESS_TOKEN))
        .send()
        .await
        .unwrap();
    assert_eq!(no_header.status(), reqwest::StatusCode::NOT_FOUND);

    // Unknown session id with a syntactically-valid credential: same 404.
    let unknown = http
        .get(format!(
            "{base}/v1/sessions/00000000-0000-0000-0000-000000000000"
        ))
        .header(reqwest::header::AUTHORIZATION, bearer(IN_PROCESS_TOKEN))
        .header(SESSION_TOKEN_HEADER, session_token_header(&token))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);

    // Unsupported PATCH state => 422 (deny_unknown_fields request shape).
    let invalid = http
        .patch(format!("{base}/v1/sessions/{id}"))
        .header(reqwest::header::AUTHORIZATION, bearer(IN_PROCESS_TOKEN))
        .header(SESSION_TOKEN_HEADER, session_token_header(&token))
        .body(serde_json::json!({"state": "paused"}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);

    // Cancel → terminal record remains readable while the lease returns; a
    // second DELETE stays idempotent 204.
    http.delete(format!("{base}/v1/sessions/{id}"))
        .header(reqwest::header::AUTHORIZATION, bearer(IN_PROCESS_TOKEN))
        .header(SESSION_TOKEN_HEADER, session_token_header(&token))
        .send()
        .await
        .unwrap();
    let retained = http
        .get(format!("{base}/v1/sessions/{id}"))
        .header(reqwest::header::AUTHORIZATION, bearer(IN_PROCESS_TOKEN))
        .header(SESSION_TOKEN_HEADER, session_token_header(&token))
        .send()
        .await
        .unwrap();
    assert_eq!(retained.status(), reqwest::StatusCode::OK);
    assert_eq!(
        retained.json::<serde_json::Value>().await.unwrap()["state"],
        "cancelled"
    );
    let again = http
        .delete(format!("{base}/v1/sessions/{id}"))
        .header(reqwest::header::AUTHORIZATION, bearer(IN_PROCESS_TOKEN))
        .header(SESSION_TOKEN_HEADER, session_token_header(&token))
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), reqwest::StatusCode::NO_CONTENT);
}

#[tokio::test(flavor = "multi_thread")]
async fn create_body_rejects_unknown_fields_and_bad_json() {
    let (state, _engine) = test_state(2);
    let base = base_url(serve_router(state).await.port());
    let http = reqwest::Client::new();

    let unknown = create_session(
        &http,
        &base,
        Some(IN_PROCESS_TOKEN),
        serde_json::json!({"model_id": "m", "bogus": 1}),
    )
    .await;
    assert_eq!(unknown.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let err: serde_json::Value = unknown.json().await.unwrap();
    assert_eq!(err["error"]["code"], "invalid_request");

    let bad = http
        .post(format!("{base}/v1/sessions"))
        .header(reqwest::header::AUTHORIZATION, bearer(IN_PROCESS_TOKEN))
        .body("not json")
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test(flavor = "multi_thread")]
async fn engine_refusal_yields_engine_error_and_frees_permits() {
    let (state, engine) = test_state(2);
    engine.set_open_error(Some("backend unavailable".to_string()));
    let base = base_url(serve_router(state.clone()).await.port());
    let http = reqwest::Client::new();

    let refused = create_session(&http, &base, Some(IN_PROCESS_TOKEN), serde_json::json!({})).await;
    assert_eq!(refused.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let err: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(err["error"]["code"], "engine_error");

    // Both permits were released (no session record): the engine now accepts.
    engine.set_open_error(None);
    let ok = create_session(&http, &base, Some(IN_PROCESS_TOKEN), serde_json::json!({})).await;
    assert_eq!(ok.status(), reqwest::StatusCode::CREATED);
}

#[test]
fn default_and_embed_configuration_resolution() {
    // Standalone defaults to the documented loopback control-plane port.
    let standalone = from_options(ServeOptions {
        bind: None,
        data_dir: Some(temp_data_dir("cfg-a")),
        token_stdout: TokenStdoutMode::Auto,
        max_audio_seconds: None,
        embed: false,
        opus: false,
    })
    .unwrap();
    assert_eq!(standalone.bind, DEFAULT_BIND);
    assert_eq!(standalone.mode, ServiceMode::Standalone);

    // Embed always binds an ephemeral loopback port and never discloses.
    let embedded = from_options(ServeOptions {
        bind: Some("0.0.0.0:9".to_string()),
        data_dir: Some(temp_data_dir("cfg-b")),
        token_stdout: TokenStdoutMode::Always,
        max_audio_seconds: Some(60),
        embed: true,
        opus: false,
    })
    .unwrap();
    assert_eq!(embedded.bind, "127.0.0.1:0");
    assert_eq!(embedded.mode, ServiceMode::Embedded);
    assert_eq!(embedded.token_stdout, TokenStdoutMode::Never);
    assert_eq!(embedded.max_audio_seconds, 60);
}

#[test]
fn ready_line_shape_is_the_single_json_handshake() {
    // The consumer contract: type url token apiVersion pid (+ the spec's
    // integer api_version twin key).
    let line = embed::ready_line_json("http://127.0.0.1:40001", "vx_draft", 7);
    let value: serde_json::Value = serde_json::from_str(&line).expect("single JSON object");
    assert_eq!(value["type"], "ready");
    assert_eq!(value["url"], "http://127.0.0.1:40001");
    assert_eq!(value["token"], "vx_draft");
    assert_eq!(value["apiVersion"], "1");
    assert_eq!(value["api_version"], 1);
    assert_eq!(value["pid"], 7);
    // Exactly one line, no newline inside the payload beyond the terminator.
    assert!(!line.contains('\n'));
}

// ---------------------------------------------------------------------------
// Process tests (the real binary)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn daemon_serves_rest_and_removes_its_discovery_on_sigterm() {
    let data_dir = temp_data_dir("daemon");
    let (mut child, port, token, _doc) = spawn_and_ready(&data_dir);
    let base = base_url(port);
    let http = reqwest::Client::new();

    // Unauthed liveness + authed health on the real daemon.
    let alive = http.get(format!("{base}/alive")).send().await.unwrap();
    assert_eq!(alive.status(), reqwest::StatusCode::OK);
    let health = http
        .get(format!("{base}/v1/health"))
        .header(reqwest::header::AUTHORIZATION, bearer(&token))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), reqwest::StatusCode::OK);
    let health: serde_json::Value = health.json().await.unwrap();
    assert_eq!(health["status"], "ok");
    assert!(!health["engine_version"].as_str().unwrap().is_empty());

    // Wrong bearer on the real stack: empty 401.
    let bad = http
        .get(format!("{base}/v1/health"))
        .header(reqwest::header::AUTHORIZATION, bearer("vx_evil"))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(bad.text().await.unwrap(), "");

    // Session lifecycle over the wire.
    let created = create_session(&http, &base, Some(&token), serde_json::json!({})).await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let created: serde_json::Value = created.json().await.unwrap();
    let (id, sub) = (
        created["session_id"].as_str().unwrap().to_string(),
        created["session_token"].as_str().unwrap().to_string(),
    );
    let got = http
        .get(format!("{base}/v1/sessions/{id}"))
        .header(reqwest::header::AUTHORIZATION, bearer(&token))
        .header(SESSION_TOKEN_HEADER, session_token_header(&sub))
        .send()
        .await
        .unwrap();
    assert_eq!(got.status(), reqwest::StatusCode::OK);

    // SIGTERM proves the graceful ladder: exit 0 + owned removal, and the
    // process output never reports background throttling (G8).
    sigterm(&child);
    assert_eq!(wait_exit(&mut child, Duration::from_secs(20)), Some(0));
    let output = drain_stderr(&mut child);
    assert!(
        notify::output_is_clean(&output),
        "forbidden phrase appeared in daemon output"
    );
    assert!(
        discovery::read_document(&state_dir_of(&data_dir)).is_none(),
        "discovery.json must be removed on graceful shutdown"
    );
    std::fs::remove_dir_all(&data_dir).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn second_daemon_on_same_data_dir_is_refused_and_stale_document_replaced() {
    let data_dir = temp_data_dir("collision");
    let (mut first, _port, _token, _doc) = spawn_and_ready(&data_dir);

    // Contention: live discovery describes a running owner → exit 73.
    let mut second = spawn_serve(
        &data_dir,
        &["--bind", "127.0.0.1:0", "--token-stdout", "never"],
    );
    assert_eq!(
        wait_exit(&mut second, Duration::from_secs(30)),
        Some(voxix_app_lib::service::EXIT_LOCK_HELD),
        "a live daemon must refuse a second instance on the same data dir"
    );

    sigterm(&first);
    assert_eq!(wait_exit(&mut first, Duration::from_secs(20)), Some(0));

    // A stale document (dead pid) never reads as live and is replaced.
    let stale = DiscoveryDocument {
        port: 1, // closed; combined with a dead pid this is not a live server
        pid: dead_pid_for_test(),
        protocol_version: 1,
        token_fingerprint: "f".repeat(32),
    };
    discovery::publish(&state_dir_of(&data_dir), &stale).unwrap();
    assert!(
        !discovery::alive_probe(stale.port),
        "a document whose daemon is gone must not probe live"
    );

    let mut replacement = spawn_serve(
        &data_dir,
        &["--bind", "127.0.0.1:0", "--token-stdout", "never"],
    );
    let owned = wait_for_discovery_owned_by(&data_dir, replacement.id(), Duration::from_secs(30));
    assert_ne!(
        owned.token_fingerprint,
        "f".repeat(32),
        "stale document replaced by the new owner"
    );
    assert_ne!(owned.port, 1, "the stale port is gone");
    sigterm(&replacement);
    assert_eq!(
        wait_exit(&mut replacement, Duration::from_secs(20)),
        Some(0)
    );
    assert!(discovery::read_document(&state_dir_of(&data_dir)).is_none());
    std::fs::remove_dir_all(&data_dir).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn embed_ready_line_pairwise_auth_and_stdin_eof_lease() {
    let data_dir = temp_data_dir("embed");
    let mut child = spawn_serve(&data_dir, &["--embed"]);
    let started = Instant::now();
    let line = read_first_line(
        child
            .stdout
            .take()
            .expect("embed prints exactly one ready line"),
        Duration::from_secs(30),
    );

    // The ready-line contract: a single JSON object with url/token/handshake.
    let ready: serde_json::Value =
        serde_json::from_str(&line).expect("ready line is a single JSON line");
    assert_eq!(ready["type"], "ready");
    let url = ready["url"].as_str().unwrap().to_string();
    let token = ready["token"].as_str().unwrap().to_string();
    assert!(
        url.starts_with("http://127.0.0.1:") && ready["pid"].as_u64().is_some(),
        "url is the loopback ephemeral endpoint"
    );
    let port: u16 = url
        .rsplit(':')
        .next()
        .unwrap()
        .parse()
        .expect("ephemeral port parses");
    assert_eq!(ready["apiVersion"], "1");
    assert_eq!(ready["api_version"], 1);
    assert!(ready["pid"].as_u64().unwrap() > 0);
    assert!(token.starts_with("vx_"));

    // The embedded child writes no discovery document at all.
    assert!(
        discovery::read_document(&state_dir_of(&data_dir)).is_none(),
        "the embedded child writes no discovery document"
    );

    // Pairwise auth with the ready-line bearer on the ephemeral loopback url.
    let base = base_url(port);
    let http = reqwest::Client::new();
    let alive = http.get(format!("{base}/alive")).send().await.unwrap();
    assert_eq!(alive.status(), reqwest::StatusCode::OK);
    let health = http
        .get(format!("{base}/v1/health"))
        .header(reqwest::header::AUTHORIZATION, bearer(&token))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), reqwest::StatusCode::OK);

    // stdin EOF is the lifetime lease: shutdown after the five-second grace.
    drop(child.stdin.take());
    let deadline = EMBED_GRACE + Duration::from_secs(7);
    assert_eq!(wait_exit(&mut child, deadline), Some(0));
    assert!(
        started.elapsed() >= Duration::from_millis(4_000),
        "child exited before the stdin-EOF grace period elapsed"
    );
    std::fs::remove_dir_all(&data_dir).ok();
}

#[test]
fn check_process_policy_helper_reports_interactive_qos() {
    let mut child = std::process::Command::new(BIN)
        .args(["serve", "--check-process-policy"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn --check-process-policy");
    let line = read_first_line(
        child
            .stdout
            .take()
            .expect("policy helper prints one JSON line"),
        Duration::from_secs(30),
    );
    let value: serde_json::Value = serde_json::from_str(&line).expect("JSON policy report");
    assert_eq!(value["interactive"], true);
    // No-background-throttling assertion (G8) mirrored on the parsed value:
    // nice must remain at the OS default (no voluntary down-prioritization).
    assert_eq!(value["nice"].as_i64().unwrap(), 0);

    assert_eq!(wait_exit(&mut child, Duration::from_secs(30)), Some(0));
    let stderr = drain_stderr(&mut child);
    assert!(notify::output_is_clean(&stderr));
}

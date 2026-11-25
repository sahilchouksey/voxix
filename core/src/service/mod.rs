//! `voxix serve` — headless authenticated service daemon (SDK plan §3/§12 M1).
//!
//! Runs entirely without Tauri app initialization. `main.rs` dispatches
//! `serve` before constructing `tauri::Builder`; this module creates the
//! multithread Tokio runtime, builds the engine adapter over the existing
//! managers via the extracted RuntimeContext, runs Axum, and handles
//! SIGINT/SIGTERM with an owned-removal shutdown ladder.

pub mod api;
pub mod audio;
pub mod audio_record;
pub mod auth;
pub mod batch;
pub mod chunk;
pub mod config;
pub mod discovery;
pub mod embed;
pub mod engine;
pub mod history;
pub mod inproc;
pub mod lock;
pub mod models;
pub mod notify;
#[cfg(feature = "service-opus")]
pub mod opus;
pub mod routes;
pub mod state;
pub mod ws;

use crate::cli::TokenStdoutMode;
use crate::managers::model::ModelManager;
use crate::managers::transcription::TranscriptionManager;
use crate::runtime_context::RuntimeContext;
use crate::service::auth::{EmbeddedToken, TokenStore};
use crate::service::config::{from_options, ServeOptions, ServiceConfig, ServiceMode};
use crate::service::discovery::DiscoveryDocument;
use crate::service::engine::{EngineRuntime, ManagersEngine};
use crate::service::state::ServiceState;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

pub const EXIT_OK: i32 = 0;
pub const EXIT_INTERNAL: i32 = 1;
pub const EXIT_POLICY: i32 = 65;
pub const EXIT_USAGE: i32 = 2;
/// BSD ex-style contention exit used by the advisory lock and discovery
/// refusal paths (§3.2 / §15 G2).
pub const EXIT_LOCK_HELD: i32 = 73;

/// Grace period: pending requests settle before the embedded child exits on
/// stdin EOF (SDK plan §15 G1.1).
pub const EMBED_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Integration tests drive admission/lease semantics against the full stack
/// by opting into the deterministic adapter (no model artifacts in CI).
fn use_test_engine() -> bool {
    matches!(
        std::env::var("VOXIX_SERVICE_TEST_ENGINE").as_deref(),
        Ok("1")
    )
}

/// Build the service-owned stream-text bus ("one engine" partials → active WS
/// session attribution; SDK plan §3.1 service-owned channels).
pub fn stream_text_bus() -> (
    tokio::sync::broadcast::Sender<crate::service::engine::StreamTextFrame>,
    tokio::sync::broadcast::Receiver<crate::service::engine::StreamTextFrame>,
) {
    tokio::sync::broadcast::channel(64)
}

/// Production engine adapter plus the concrete manager handle the shutdown
/// ladder needs to unload a loaded engine before process exit, and the
/// service-owned stream-text bus the state must share.
pub struct EngineBundle {
    pub engine: Arc<dyn EngineRuntime>,
    pub transcription: Option<Arc<TranscriptionManager>>,
    pub stream_text: tokio::sync::broadcast::Sender<crate::service::engine::StreamTextFrame>,
}

pub fn build_engine(
    cfg: &ServiceConfig,
    text_bus: tokio::sync::broadcast::Sender<crate::service::engine::StreamTextFrame>,
) -> Result<EngineBundle, String> {
    if use_test_engine() {
        return Ok(EngineBundle {
            engine: Arc::new(engine::TestEngine::new(text_bus.clone())) as Arc<dyn EngineRuntime>,
            transcription: None,
            stream_text: text_bus,
        });
    }

    // The daemon's managers report partials through the runtime-context event
    // sink; the sink forwards them onto the service broadcast bus, attribute
    // to the active session by the WS layer (§6 partial flow).
    let tx = text_bus.clone();
    let ctx = Arc::new(RuntimeContext::file_with_sink(
        cfg.data_dir.clone(),
        None,
        Arc::new(move |event: crate::runtime_context::DomainEvent| {
            if let crate::runtime_context::DomainEvent::StreamText {
                committed,
                tentative,
            } = event
            {
                let _ = tx.send(crate::service::engine::StreamTextFrame {
                    committed,
                    tentative,
                });
            }
        }),
    ));
    let model_manager = Arc::new(
        ModelManager::with_context(ctx.clone())
            .map_err(|e| format!("model manager init failed: {e}"))?,
    );
    let transcription_manager = Arc::new(
        TranscriptionManager::with_context(ctx.clone(), model_manager.clone())
            .map_err(|e| format!("transcription manager init failed: {e}"))?,
    );

    // Reuse — do not fork — the backend/accelerator initialization the GUI
    // path performs.
    crate::managers::transcription::init_transcribe_backend();
    crate::managers::transcription::apply_accelerator_settings(&ctx.settings());

    let engine: Arc<dyn EngineRuntime> = Arc::new(ManagersEngine::new(
        ctx,
        model_manager,
        transcription_manager.clone(),
    ));
    Ok(EngineBundle {
        engine,
        transcription: Some(transcription_manager),
        stream_text: text_bus,
    })
}

/// Bind the configured address; port `0` resolves to the ephemeral port.
/// When `cfg.bind_is_default` is true and the port is already in use,
/// falls back to an OS-assigned loopback port so the daemon stays alive
/// while something else (e.g. a local SOCKS proxy) holds the default port.
/// The actual port is published in `discovery.json` so the GUI always finds it.
/// Explicit `--bind` is never silently changed.
pub async fn bind_listener(cfg: &ServiceConfig) -> Result<tokio::net::TcpListener, String> {
    let addr: std::net::SocketAddr = cfg
        .bind
        .parse()
        .map_err(|e| format!("invalid --bind '{}': {e}", cfg.bind))?;
    match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => Ok(l),
        Err(e) if cfg.bind_is_default && e.kind() == std::io::ErrorKind::AddrInUse => {
            let fallback = std::net::SocketAddr::new(addr.ip(), 0);
            eprintln!(
                "warning: {addr} is already in use ({e}); \
falling back to an ephemeral loopback port — discovery.json will carry the actual port"
            );
            tokio::net::TcpListener::bind(fallback)
                .await
                .map_err(|e2| format!("failed to bind fallback {fallback}: {e2}"))
        }
        Err(e) => Err(format!("failed to bind {addr}: {e}")),
    }
}

/// SIGINT/SIGTERM shutdown trigger (Unix).
#[cfg(unix)]
pub async fn terminate_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigint = signal(SignalKind::interrupt()).expect("SIGINT handler");
    let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = sigint.recv() => {}
        _ = sigterm.recv() => {}
    }
}

/// CTRL+C shutdown trigger (Windows).
#[cfg(windows)]
pub async fn terminate_signal() {
    let mut ctrl_c = tokio::signal::windows::ctrl_c().expect("CTRL+C handler");
    ctrl_c.recv().await;
}

/// Standalone daemon lifecycle: token storage/disclosure, instance lock,
/// discovery refusal/publish, Axum serve, graceful shutdown ladder with
/// owned removal. Returns a process exit code.
pub async fn serve_standalone(
    cfg: ServiceConfig,
    state_dir: PathBuf,
    bundle: EngineBundle,
    token_stdout: TokenStdoutMode,
) -> i32 {
    let token_store = TokenStore::new(&state_dir);
    let first_run_token = token_store.ensure_record().ok().flatten();
    if let Some(token) = first_run_token {
        match token_stdout {
            TokenStdoutMode::Always => println!("{token}"),
            TokenStdoutMode::Auto => {
                if std::io::stderr().is_terminal() {
                    eprintln!("Service token (first start; stored only as a verifier hash):");
                    eprintln!("{token}");
                }
            }
            TokenStdoutMode::Never => {}
        }
    }
    let verifier = match token_store.load_record() {
        Ok(Some(record)) => Some(record.verifier),
        Ok(None) => None,
        Err(e) => {
            eprintln!("error: token store unreadable: {e}");
            return EXIT_INTERNAL;
        }
    };
    let Some(verifier) = verifier else {
        eprintln!("error: no auth token on record after first-run initialization");
        return EXIT_INTERNAL;
    };

    let server_lock = match crate::service::lock::acquire(&state_dir) {
        Ok(server_lock) => server_lock,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_LOCK_HELD;
        }
    };

    let listener = match bind_listener(&cfg).await {
        Ok(listener) => listener,
        Err(e) => {
            drop(server_lock);
            eprintln!("error: {e}");
            return EXIT_INTERNAL;
        }
    };

    let state = Arc::new(ServiceState::new(
        cfg.clone(),
        bundle.engine,
        Some(verifier.clone()),
        None,
        bundle.stream_text,
    ));
    // In-process (GUI) clients hit loopback. A local proxy/tool squatting
    // 127.0.0.1:47832 would win the more-specific v4 bind and reset the
    // GUI's requests, even though this daemon is healthy. A second loopback
    // bind on IPv6 gives the GUI a squatter-proof endpoint. Non-fatal.
    {
        let state_v6 = Arc::clone(&state);
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
        match tokio::net::TcpListener::bind(std::net::SocketAddr::from((
            std::net::Ipv6Addr::LOCALHOST,
            port,
        )))
        .await
        {
            Ok(v6) => {
                eprintln!("voxix serve also listening on [::1]:{port} (in-process endpoint)");
                tokio::spawn(async move {
                    if let Err(e) = axum::serve(v6, routes::router(state_v6)).await {
                        eprintln!("error: IPv6 loopback listener ended: {e}");
                    }
                });
            }
            Err(e) => {
                eprintln!("warning: IPv6 loopback [::1]:{port} bind failed: {e}; GUI will fall back to 127.0.0.1")
            }
        }
    }
    let fingerprint: String = verifier.chars().take(32).collect();
    if let Err(e) = discovery::publish(
        &state_dir,
        &DiscoveryDocument {
            port: listener.local_addr().map(|a| a.port()).unwrap_or(0),
            pid: std::process::id(),
            protocol_version: crate::service::discovery::PROTOCOL_VERSION,
            token_fingerprint: fingerprint,
        },
    ) {
        drop(server_lock);
        eprintln!("error: discovery publish failed: {e}");
        return EXIT_INTERNAL;
    }
    eprintln!(
        "voxix serve listening on {} (pid {})",
        listener.local_addr().map(|a| a.to_string()).unwrap_or_else(|_| cfg.bind.clone()),
        std::process::id()
    );

    // `Type=notify` readiness: the API answers (and discovery is published)
    // before we signal READY=1; systemd holds dependents until then.
    let _ = notify::sd_notify("READY=1");

    let result = axum::serve(listener, routes::router(state))
        .with_graceful_shutdown(async { terminate_signal().await })
        .await;

    let _ = notify::sd_notify("STOPPING=1");

    // Graceful shutdown ladder: owned removal (compare-then-delete), then
    // explicit engine unload (the loaded-model teardown guard).
    let _ = discovery::remove_owned(&state_dir, std::process::id());
    if let Some(tm) = bundle.transcription {
        let _ = tm.unload_model();
    }
    drop(server_lock);
    match result {
        Ok(()) => EXIT_OK,
        Err(e) => {
            eprintln!("error: server error: {e}");
            EXIT_INTERNAL
        }
    }
}

/// Entry point from main.rs: returns a process exit code. The function never
/// initializes Tauri/Wry/GTK, windows, tray, overlay, cpal, shortcuts,
/// Enigo, secure-input monitoring, paste, updater, or autostart.
pub fn run_serve_cli(options: ServeOptions) -> i32 {
    // Portable mode detection must run before the data directory resolves.
    crate::portable::init();

    let cfg = match from_options(options) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };

    // Process-policy gate (§15 G8): refuse a background-throttled service.
    if let Err(e) = notify::check_self_policy() {
        eprintln!("error: {e}");
        return EXIT_POLICY;
    }

    let state_dir = match cfg.state_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_INTERNAL;
        }
    };

    let text_bus = stream_text_bus();
    let engine_bundle = match build_engine(&cfg, text_bus.0.clone()) {
        Ok(bundle) => bundle,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_INTERNAL;
        }
    };

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: tokio runtime build failed: {e}");
            return EXIT_INTERNAL;
        }
    };

    match cfg.mode.clone() {
        ServiceMode::Standalone => {
            let token_stdout = cfg.token_stdout;
            rt.block_on(serve_standalone(
                cfg,
                state_dir,
                engine_bundle,
                token_stdout,
            ))
        }
        ServiceMode::Embedded => {
            let cfg_embed = cfg.clone();
            let minted = match EmbeddedToken::mint() {
                Ok(minted) => minted,
                Err(e) => {
                    eprintln!("error: embedded token mint failed: {e}");
                    return EXIT_INTERNAL;
                }
            };
            let transcription = engine_bundle.transcription.clone();
            let exit = rt.block_on(async move {
                let listener = match bind_listener(&cfg_embed).await {
                    Ok(listener) => listener,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return EXIT_INTERNAL;
                    }
                };
                let state = Arc::new(ServiceState::new(
                    cfg_embed,
                    engine_bundle.engine,
                    Some(minted.record.verifier.clone()),
                    Some(minted.plaintext.clone()),
                    engine_bundle.stream_text,
                ));
                embed::serve_embedded(listener, state).await
            });
            if let Some(tm) = transcription {
                let _ = tm.unload_model();
            }
            exit
        }
    }
}

//! Embedded child launch mode (SDK plan §15 G1.1).
//!
//! `serve --embed` binds `127.0.0.1:0`, generates a fresh per-child token,
//! writes no discovery file, takes no instance/engine locks, and sends no
//! CORS headers. When the API is ready it writes exactly one JSON ready-line
//! to stdout; stdin EOF is the lifetime lease — shutdown after a five second
//! grace period. Hash-token storage, stderr-once disclosure, and the visible
//! serve daemon remain the LAN model — the embedded child is a
//! self-authenticating pairwise lease, not a service instance.

use crate::service::routes;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

/// The exact ready-line bytes for this child. Carries both the consumer
/// contract's `apiVersion` string and the spec's `api_version` integer, so
/// both validators accept the same line (additive payload fields are ignored
/// by older readers per the protocol's contract).
pub fn ready_line_json(url: &str, token: &str, pid: u32) -> String {
    serde_json::json!({
        "type": "ready",
        "url": url,
        "token": token,
        "apiVersion": "1",
        "api_version": 1,
        "pid": pid,
    })
    .to_string()
}

/// Write the ready line (single JSON line + flush). After this handshake the
/// process never writes to stdout again — diagnostics go to stderr.
pub fn write_ready_line(url: &str, token: &str) {
    let line = ready_line_json(url, token, std::process::id());
    {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
    }
    // stdout intentionally dropped: nothing else is written there.
}

/// EOF on stdin ends the lifetime lease.
pub async fn stdin_eof() {
    let mut stdin = tokio::io::stdin();
    let mut buf = [0u8; 512];
    loop {
        match stdin.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                // Inputs on stdin are ignored in M1: the handshake speaks
                // typed JSON on stdout only.
                continue;
            }
        }
    }
}

/// Bind an ephemeral loopback listener (G1.1: `127.0.0.1:0`).
pub async fn bind_embed_listener() -> Result<TcpListener, String> {
    TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("embedded listener bind failed: {e}"))
}

/// Run the embedded child: ready-line handshake, then serve until stdin EOF
/// (plus a 5 s grace period for in-flight calls) or an OS signal.
pub async fn serve_embedded(
    listener: TcpListener,
    state: Arc<crate::service::state::ServiceState>,
) -> i32 {
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let url = format!("http://127.0.0.1:{port}");
    let Some(token) = state.auth.embedded_token.clone() else {
        eprintln!("embedded: missing freshly minted bearer token");
        return super::EXIT_INTERNAL;
    };
    write_ready_line(&url, &token);

    let shutdown = async {
        tokio::select! {
            _ = stdin_eof() => {
                tokio::time::sleep(super::EMBED_GRACE).await;
            }
            _ = super::terminate_signal() => {}
        }
    };
    match axum::serve(listener, routes::router(state))
        .with_graceful_shutdown(shutdown)
        .await
    {
        Ok(()) => super::EXIT_OK,
        Err(e) => {
            eprintln!("embedded: server error: {e}");
            super::EXIT_INTERNAL
        }
    }
}

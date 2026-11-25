//! Local control plane for the voxix service daemon, bound for the GUI.
//!
//! The GUI is a *local* client of the daemon: it reads the discovery document
//! (`<app-data>/state/discovery.json`), probes the unauthenticated `/alive`
//! route and — with an in-memory-only bearer the user pastes or the GUI
//! captures from a first-run `serve` spawn — the authenticated `/v1/health`,
//! `/v1/status`, and `/v1/auth/tokens` routes.
//!
//! The bearer token is never written to disk, settings, or logs: it lives in
//! `ServiceTokenHolder` (process memory) and is dropped with the app. The
//! first plaintext token a GUI-spawned daemon prints on stdout (first run
//! only) is captured into the same holder; every later start leaves the
//! holder untouched, which is why the token panel always accepts a manual
//! paste.

use crate::portable;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, State};

/// In-memory-only bearer for the service daemon. Never persisted.
#[derive(Default)]
pub struct ServiceTokenHolder(pub Arc<Mutex<Option<String>>>);

/// Handle to the daemon child this GUI process spawned, if any. Kept so the
/// stdout/stderr pumps can be joined on stop; the graceful stop itself is a
/// SIGTERM against the discovery document's pid (the daemon owns reaping).
#[derive(Default)]
pub struct ServiceDaemonChild(pub Mutex<Option<std::process::Child>>);

fn state_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = portable::app_data_dir(app)
        .map_err(|e| format!("Failed to resolve app data directory: {e}"))?;
    Ok(dir.join("state"))
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    false
}

fn port_open(port: u16) -> bool {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

fn discovery_info(app: &AppHandle) -> Result<ServiceDiscoveryInfo, String> {
    let dir = state_dir(app)?;
    let dir_display = dir.to_string_lossy().to_string();
    Ok(match crate::service::discovery::read_document(&dir) {
        Some(doc) => ServiceDiscoveryInfo {
            present: true,
            state_dir: dir_display,
            port: Some(doc.port),
            pid: Some(doc.pid),
            protocol_version: Some(doc.protocol_version),
            token_fingerprint: Some(doc.token_fingerprint),
            pid_alive: pid_alive(doc.pid),
            port_open: port_open(doc.port),
        },
        None => ServiceDiscoveryInfo {
            present: false,
            state_dir: dir_display,
            port: None,
            pid: None,
            protocol_version: None,
            token_fingerprint: None,
            pid_alive: false,
            port_open: false,
        },
    })
}

fn bearer(holder: &State<'_, ServiceTokenHolder>) -> Option<String> {
    holder.0.lock().ok().and_then(|guard| guard.clone())
}

fn daemon_base(app: &AppHandle) -> Result<String, String> {
    let info = discovery_info(app)?;
    match (info.port, info.pid_alive) {
        (Some(port), true) => Ok(format!("http://127.0.0.1:{port}")),
        (Some(_), false) => Err("discovery.json is stale (pid is not alive)".to_string()),
        (None, _) => Err("discovery.json not present; the daemon is not running".to_string()),
    }
}

fn http_client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| format!("http client init failed: {e}"))
}

/// Parse the v1 error envelope (`{error:{code,message}}`) into a short
/// user-facing string; never includes paths or tokens.
async fn error_body(response: reqwest::Response) -> String {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    let parsed = serde_json::from_str::<serde_json::Value>(&text).ok();
    let code = parsed
        .as_ref()
        .and_then(|v| v.get("error"))
        .and_then(|e| e.get("code"))
        .and_then(|c| c.as_str())
        .unwrap_or("http_error");
    format!("{code} (HTTP {})", status.as_u16())
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ServiceDiscoveryInfo {
    pub present: bool,
    pub state_dir: String,
    pub port: Option<u16>,
    pub pid: Option<u32>,
    pub protocol_version: Option<u32>,
    pub token_fingerprint: Option<String>,
    pub pid_alive: bool,
    pub port_open: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ServiceHealthInfo {
    pub status: String,
    pub version: String,
    pub engine_version: String,
    pub protocol_version: u32,
    pub uptime_ms: u64,
    pub ready: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ServiceCapabilities {
    pub requires_server_vad: bool,
    pub prefers_auto_gain: bool,
    pub prefers_noise_reduction: bool,
    pub max_concurrent_streams: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ServiceStatusInfo {
    pub mode: String,
    pub bind: String,
    pub active_model_id: Option<String>,
    pub model_state: String,
    pub backend: Option<String>,
    pub active_session_id: Option<String>,
    pub active_sessions: u32,
    pub max_realtime_factor: Option<f64>,
    pub lease_held: bool,
    pub capabilities: ServiceCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ServiceProbeReport {
    pub alive: bool,
    pub token_present: bool,
    pub health: Option<ServiceHealthInfo>,
    pub health_error: Option<String>,
    pub status: Option<ServiceStatusInfo>,
    pub status_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ServiceStartResult {
    pub already_running: bool,
    pub info: ServiceDiscoveryInfo,
    /// True when this spawn captured a first-run token from stdout. The token
    /// itself stays in memory; the GUI never sees it through this channel.
    pub token_captured: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ServiceStopResult {
    pub stopped: bool,
    pub pid: Option<u32>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ServiceRotateResult {
    pub id: String,
    pub name: String,
    /// Plaintext, returned exactly once by the daemon. Held by the GUI in
    /// memory only (never persisted).
    pub token: String,
    pub prefix: String,
    pub created_at: String,
}

/// Read the discovery document and report pid liveness/port reachability.
/// Unauthenticated; safe to poll.
#[tauri::command]
#[specta::specta]
pub fn service_discover(app: AppHandle) -> Result<ServiceDiscoveryInfo, String> {
    discovery_info(&app)
}

/// Probe `/alive` (no credentials) and — when an in-memory token is held —
/// `/v1/health` and `/v1/status`. `health_error`/`status_error` carry a short
/// machine-ish code (`no_token`, `unauthorized`, `<code> (HTTP n)`); tokens
/// and paths never appear in them.
#[tauri::command]
#[specta::specta]
pub async fn service_probe(
    app: AppHandle,
    token_holder: State<'_, ServiceTokenHolder>,
) -> Result<ServiceProbeReport, String> {
    let info = discovery_info(&app)?;
    let token = bearer(&token_holder);
    let token_present = token.is_some();

    let mut report = ServiceProbeReport {
        alive: false,
        token_present,
        health: None,
        health_error: None,
        status: None,
        status_error: None,
    };

    let Some(port) = info.port.filter(|_| info.pid_alive) else {
        return Ok(report);
    };
    let base = format!("http://127.0.0.1:{port}");
    let client = http_client(Duration::from_secs(2))?;

    report.alive = client
        .get(format!("{base}/alive"))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);

    let Some(token) = token else {
        report.health_error = Some("no_token".to_string());
        report.status_error = Some("no_token".to_string());
        return Ok(report);
    };

    let bearer_header = format!("Bearer {token}");

    match client
        .get(format!("{base}/v1/health"))
        .header(reqwest::header::AUTHORIZATION, &bearer_header)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            match response
                .json::<serde_json::Value>()
                .await
                .ok()
                .map(|v| ServiceHealthInfo {
                    status: v
                        .get("status")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    version: v
                        .get("version")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    engine_version: v
                        .get("engine_version")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    protocol_version: v
                        .get("protocol_version")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0) as u32,
                    uptime_ms: v.get("uptime_ms").and_then(|x| x.as_u64()).unwrap_or(0),
                    ready: v.get("ready").and_then(|x| x.as_bool()).unwrap_or(false),
                }) {
                Some(health) => report.health = Some(health),
                None => report.health_error = Some("malformed_response".to_string()),
            }
        }
        Ok(response) if response.status() == reqwest::StatusCode::UNAUTHORIZED => {
            report.health_error = Some("unauthorized".to_string());
        }
        Ok(response) => report.health_error = Some(error_body(response).await),
        Err(_) => report.health_error = Some("unreachable".to_string()),
    }

    match client
        .get(format!("{base}/v1/status"))
        .header(reqwest::header::AUTHORIZATION, &bearer_header)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            match response
                .json::<serde_json::Value>()
                .await
                .ok()
                .map(|v| ServiceStatusInfo {
                    mode: v
                        .get("mode")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    bind: v
                        .get("bind")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    active_model_id: v
                        .get("active_model_id")
                        .and_then(|x| x.as_str())
                        .map(str::to_string),
                    model_state: v
                        .get("model_state")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    backend: v
                        .get("backend")
                        .and_then(|x| x.as_str())
                        .map(str::to_string),
                    active_session_id: v
                        .get("active_session_id")
                        .and_then(|x| x.as_str())
                        .map(str::to_string),
                    active_sessions: v
                        .get("active_sessions")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0) as u32,
                    max_realtime_factor: v.get("max_realtime_factor").and_then(|x| x.as_f64()),
                    lease_held: v
                        .get("lease_held")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false),
                    capabilities: ServiceCapabilities {
                        requires_server_vad: v
                            .get("capabilities")
                            .and_then(|c| c.get("requires_server_vad"))
                            .and_then(|x| x.as_bool())
                            .unwrap_or(false),
                        prefers_auto_gain: v
                            .get("capabilities")
                            .and_then(|c| c.get("prefers_auto_gain"))
                            .and_then(|x| x.as_bool())
                            .unwrap_or(false),
                        prefers_noise_reduction: v
                            .get("capabilities")
                            .and_then(|c| c.get("prefers_noise_reduction"))
                            .and_then(|x| x.as_bool())
                            .unwrap_or(false),
                        max_concurrent_streams: v
                            .get("capabilities")
                            .and_then(|c| c.get("max_concurrent_streams"))
                            .and_then(|x| x.as_u64())
                            .unwrap_or(1) as u32,
                    },
                }) {
                Some(status) => report.status = Some(status),
                None => report.status_error = Some("malformed_response".to_string()),
            }
        }
        Ok(response) if response.status() == reqwest::StatusCode::UNAUTHORIZED => {
            report.status_error = Some("unauthorized".to_string());
        }
        Ok(response) => report.status_error = Some(error_body(response).await),
        Err(_) => report.status_error = Some("unreachable".to_string()),
    }

    Ok(report)
}

/// Spawn the daemon (`<this-binary> serve --token-stdout always`) and wait
/// for its discovery document. A first-run token printed on stdout is
/// captured into memory; a daemon that is already running is reported as
/// `already_running` instead of spawning a contender (it would exit 73).
#[tauri::command]
#[specta::specta]
pub async fn service_start(
    app: AppHandle,
    token_holder: State<'_, ServiceTokenHolder>,
    child_slot: State<'_, ServiceDaemonChild>,
) -> Result<ServiceStartResult, String> {
    let pre = discovery_info(&app)?;
    let port = pre.port.unwrap_or(0);
    // Liveness = document + a live daemon answering `/alive` with 200. Pid
    // existence alone cannot tell our daemon from a recycled pid or a
    // squatter on the port.
    if pre.present && crate::service::discovery::alive_probe(port) {
        return Ok(ServiceStartResult {
            already_running: true,
            info: pre,
            token_captured: false,
        });
    }
    // Stale document — sideline it so the fresh daemon can publish its own
    // without tripping startup checks, and the old one is not clobbered.
    if pre.present {
        let dir = state_dir(&app)?;
        let doc_path = dir.join(crate::service::discovery::DISCOVERY_FILE_NAME);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let stale = dir.join(format!("discovery.json.stale-{ts}"));
        match std::fs::rename(&doc_path, &stale) {
            Ok(()) => log::info!("sidelined stale discovery document to {}", stale.display()),
            Err(e) => log::warn!(
                "could not sideline stale discovery document {}: {e}",
                doc_path.display()
            ),
        }
    }

    let exe = std::env::current_exe().map_err(|e| format!("current executable: {e}"))?;
    let mut child = std::process::Command::new(&exe)
        .arg("serve")
        .arg("--token-stdout")
        .arg("always")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn daemon: {e}"))?;

    // Pump stdout: capture the first-run token line (`vx_…`), drop the rest.
    if let Some(stdout) = child.stdout.take() {
        let holder_pid = child.id();
        let token_slot = Arc::clone(&token_holder.0);
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let trimmed = line.trim();
                if trimmed.starts_with("vx_") {
                    let mut guard = token_slot.lock().unwrap_or_else(|e| e.into_inner());
                    *guard = Some(trimmed.to_string());
                    log::info!(
                        "captured first-run service token from spawned daemon (pid {holder_pid})"
                    );
                    break;
                }
            }
        });
    }
    // Drain stderr so the child never blocks on a full pipe; content is
    // already on the daemon's own logs when run from a terminal.
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                match line {
                    Ok(line) => log::debug!("voxix serve: {line}"),
                    Err(_) => break,
                }
            }
        });
    }

    {
        let mut slot = child_slot.0.lock().unwrap_or_else(|e| e.into_inner());
        *slot = Some(child);
    }

    // Wait (bounded) for the daemon to publish discovery.json.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let info = discovery_info(&app)?;
        if info.present && info.pid_alive && info.port_open {
            let captured = token_holder.0.lock().map(|g| g.is_some()).unwrap_or(false);
            return Ok(ServiceStartResult {
                already_running: false,
                info,
                token_captured: captured,
            });
        }
        if std::time::Instant::now() >= deadline {
            return Err("daemon did not become ready within 10 s (check that no other instance holds the lock)".to_string());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Graceful stop: SIGTERM to the pid in discovery.json, then wait (bounded)
/// for it to exit. The daemon's shutdown ladder (owned discovery removal,
/// model unload) runs to completion before the pid disappears.
#[tauri::command]
#[specta::specta]
pub async fn service_stop(
    app: AppHandle,
    child_slot: State<'_, ServiceDaemonChild>,
) -> Result<ServiceStopResult, String> {
    let info = discovery_info(&app)?;
    let Some(pid) = info.pid.filter(|_| info.present) else {
        return Ok(ServiceStopResult {
            stopped: false,
            pid: None,
            detail: "not_running".to_string(),
        });
    };
    if !pid_alive(pid) {
        return Ok(ServiceStopResult {
            stopped: false,
            pid: Some(pid),
            detail: "stale".to_string(),
        });
    }

    #[cfg(unix)]
    {
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if rc != 0 {
            return Err(format!(
                "failed to signal daemon pid {pid}: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    #[cfg(not(unix))]
    {
        return Err("graceful stop is only implemented on unix".to_string());
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if !pid_alive(pid) {
            // Reap our child handle if we spawned this daemon.
            if let Ok(mut slot) = child_slot.0.lock() {
                if let Some(mut child) = slot.take() {
                    let _ = child.wait();
                }
            }
            return Ok(ServiceStopResult {
                stopped: true,
                pid: Some(pid),
                detail: "stopped".to_string(),
            });
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(ServiceStopResult {
        stopped: false,
        pid: Some(pid),
        detail: "timeout".to_string(),
    })
}

/// Replace (or clear) the in-memory bearer used by the probes and rotate.
/// Plaintext never touches disk; call with `None` to forget it.
#[tauri::command]
#[specta::specta]
pub fn service_set_token(
    token: Option<String>,
    token_holder: State<'_, ServiceTokenHolder>,
) -> Result<(), String> {
    let mut guard = token_holder.0.lock().map_err(|e| e.to_string())?;
    *guard = token;
    Ok(())
}

/// Read the gui-client token from disk (`state/gui-client.json`, 0600) and
/// return the plaintext. The token is stable across daemon restarts and primary
/// token rotations — the GUI has its own independent credential.
///
/// This is a Tauri IPC command: callable only from the GUI process, not over
/// the network. The token is already on disk at the same path the GUI process
/// has read access to; this command is a convenience accessor.
#[tauri::command]
#[specta::specta]
pub fn service_get_gui_token(app: AppHandle) -> Result<String, String> {
    let dir = state_dir(&app)?;
    let path = dir.join("gui-client.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("gui-client.json not found (service may not have run yet): {e}"))?;
    let body: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("gui-client.json parse error: {e}"))?;
    body.get("token")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| "gui-client.json missing 'token' field".to_string())
}

/// Rotate the primary bearer via `POST /v1/auth/tokens` (`rotate_primary`).
/// The daemon responds 201 with the plaintext exactly once; it replaces the
/// in-memory bearer and is returned to the GUI (memory-only) for one-time
/// display. Existing sockets close with 4001.
#[tauri::command]
#[specta::specta]
pub async fn service_rotate_token(
    app: AppHandle,
    token_holder: State<'_, ServiceTokenHolder>,
) -> Result<ServiceRotateResult, String> {
    let base = daemon_base(&app)?;
    let Some(token) = bearer(&token_holder) else {
        return Err("no_token".to_string());
    };
    let client = http_client(Duration::from_secs(5))?;
    let response = client
        .post(format!("{base}/v1/auth/tokens"))
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
        .json(&serde_json::json!({"name": "primary", "rotate_primary": true}))
        .send()
        .await
        .map_err(|_| "unreachable".to_string())?;

    if !response.status().is_success() {
        return Err(error_body(response).await);
    }
    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|_| "malformed_response".to_string())?;
    let rotated = ServiceRotateResult {
        id: value
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        name: value
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("primary")
            .to_string(),
        token: value
            .get("token")
            .and_then(|x| x.as_str())
            .ok_or("malformed_response")?
            .to_string(),
        prefix: value
            .get("prefix")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        created_at: value
            .get("created_at")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
    };
    if let Ok(mut guard) = token_holder.0.lock() {
        *guard = Some(rotated.token.clone());
    }
    Ok(rotated)
}

/// Tailscale network information for this device.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct TailscaleInfo {
    /// IPv4 Tailscale address (100.x.x.x), if available.
    pub ip: Option<String>,
    /// MagicDNS hostname (e.g. `my-mac.tailxxxxx.ts.net`), if available.
    pub dns: Option<String>,
    /// True when Tailscale appears to be installed and running.
    pub available: bool,
}

fn find_tailscale_bin() -> Option<PathBuf> {
    // 1. Try PATH first (works on any platform when tailscale is in PATH)
    #[cfg(windows)]
    let path_cmd = "tailscale.exe";
    #[cfg(not(windows))]
    let path_cmd = "tailscale";

    let mut test_cmd = std::process::Command::new(path_cmd);
    test_cmd.arg("--version");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        test_cmd.creation_flags(0x08000000);
    }
    if let Ok(out) = test_cmd.output() {
        if out.status.success() {
            return Some(PathBuf::from(path_cmd));
        }
    }

    // 2. Platform-specific known candidate locations
    let mut candidates: Vec<PathBuf> = Vec::new();

    #[cfg(target_os = "macos")]
    {
        candidates.push(PathBuf::from("/Applications/Tailscale.app/Contents/MacOS/Tailscale"));
        candidates.push(PathBuf::from("/opt/homebrew/bin/tailscale"));
        candidates.push(PathBuf::from("/usr/local/bin/tailscale"));
    }

    #[cfg(target_os = "linux")]
    {
        candidates.push(PathBuf::from("/usr/bin/tailscale"));
        candidates.push(PathBuf::from("/usr/local/bin/tailscale"));
        candidates.push(PathBuf::from("/bin/tailscale"));
        candidates.push(PathBuf::from("/snap/bin/tailscale"));
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(pf) = std::env::var("ProgramFiles") {
            candidates.push(PathBuf::from(pf).join("Tailscale").join("tailscale.exe"));
        }
        if let Ok(pfx86) = std::env::var("ProgramFiles(x86)") {
            candidates.push(PathBuf::from(pfx86).join("Tailscale").join("tailscale.exe"));
        }
        if let Ok(la) = std::env::var("LOCALAPPDATA") {
            candidates.push(PathBuf::from(la).join("Tailscale").join("tailscale.exe"));
        }
        candidates.push(PathBuf::from("C:\\Program Files\\Tailscale\\tailscale.exe"));
    }

    candidates.into_iter().find(|p| p.exists())
}

/// Probe Tailscale for the device's IP and MagicDNS hostname.
/// Runs `tailscale status --json`; never fails — missing/off returns
/// `available: false` with null fields.
#[tauri::command]
#[specta::specta]
pub fn service_get_tailscale_info() -> TailscaleInfo {
    let empty = TailscaleInfo {
        ip: None,
        dns: None,
        available: false,
    };

    let Some(bin) = find_tailscale_bin() else {
        return empty;
    };

    let mut cmd = std::process::Command::new(bin);
    cmd.args(["status", "--json"]);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    let Ok(out) = cmd.output() else {
        return empty;
    };
    if !out.status.success() {
        return empty;
    }

    let Ok(json): Result<serde_json::Value, _> = serde_json::from_slice(&out.stdout) else {
        return empty;
    };

    // If backend state is not Running, Tailscale is disconnected or inactive
    if let Some(state) = json.get("BackendState").and_then(|s| s.as_str()) {
        if state != "Running" {
            return empty;
        }
    }

    let me = &json["Self"];
    // First IPv4 address from TailscaleIPs array (CGNAT 100.x.y.z)
    let ip = me["TailscaleIPs"]
        .as_array()
        .and_then(|arr| {
            arr.iter()
                .find_map(|v| v.as_str().filter(|s| s.starts_with("100.") || s.contains('.')).map(|s| s.to_string()))
        });
    // MagicDNS hostname — trim trailing dot
    let dns = me["DNSName"]
        .as_str()
        .map(|s| s.trim_end_matches('.').to_string())
        .filter(|s| !s.is_empty());

    let available = ip.is_some() || dns.is_some();

    TailscaleInfo {
        available,
        ip,
        dns,
    }
}

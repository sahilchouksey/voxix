//! GUI-side client for the always-on voxix service.
//!
//! The `dev.xix3r.voxix-serve` launchd agent is the single service. It
//! dual-binds: `0.0.0.0:47832` for LAN/Tailscale clients and `[::1]:47832`
//! for in-process (GUI) traffic. IPv6 loopback is used by the GUI because
//! loopback ports on 127.0.0.1 can be squatted by unrelated local services
//! (e.g. local SOCKS/HTTP proxies) — the connection would be reset and the
//! GUI would report a service error even though the daemon is healthy.
//! The GUI registers itself once at launch (`voxix-gui` client in the
//! service's client sidecar) and authenticates exactly like the phone and
//! any SDK client. The hot path POSTs the recorded WAV to
//! `POST /v1/transcriptions` and polls `GET /v1/transcriptions/{job}`.
//!
//! No in-process hosting: there is exactly one service, one port, one
//! bearer-verifier family (`state/service-auth.json` + optional rotated
//! verifier + `state/clients.json`). Failure to register means the GUI
//! dictates nothing and tells the user plainly.

use tauri::{AppHandle, Manager};

/// Endpoint + bearer, read from Tauri managed state.
pub struct GuiClientState(pub std::sync::Mutex<Option<(String, String)>>);

/// Resolve the directory the service reads its secrets from — same resolution
/// logic the service itself uses (`portable > default app data dir`).
pub fn service_state_dir() -> Result<std::path::PathBuf, String> {
    let data_dir = crate::service::config::ServiceConfig::data_dir()?;
    Ok(data_dir.join("state"))
}

/// GUI entry point: register/refresh the `voxix-gui` client and return
/// (url, token) for the managed state. Idempotent across launches.
pub fn ensure_client_for_gui() -> Result<(String, String), String> {
    let state_dir = service_state_dir()?;
    let token = crate::service::auth::ensure_gui_client_token(&state_dir)?;
    // IPv6 loopback first: nothing else on this machine listens there by
    // default, unlike 127.0.0.1 which local proxies frequently take over.
    Ok(("http://[::1]:47832".to_string(), token))
}

/// Loopback client for the dictation hot path. Submits the captured audio to
/// the service (raw WAV body, bearer), then polls to completion and returns
/// the transcript. Errors carry a user-readable message suitable for the
/// `transcription-error` toast.
/// Service base-URL candidates, in preference order: IPv6 loopback first
/// (immune to local IPv4 loopback proxies squatting the port), then the
/// historical IPv4 loopback endpoint as fallback.
fn service_base_candidates(primary: &str) -> Vec<String> {
    if primary.starts_with("http://[::1]") {
        vec![
            "http://[::1]:47832".to_string(),
            "http://127.0.0.1:47832".to_string(),
        ]
    } else {
        vec![primary.to_string()]
    }
}

pub async fn transcribe_batch(app: &AppHandle, samples: &[f32]) -> Result<String, String> {
    let (base, plaintext) = {
        let state = app.state::<GuiClientState>();
        let slot = state
            .0
            .lock()
            .map_err(|_| "service client lock poisoned".to_string())?;
        slot.clone()
            .unwrap_or_else(|| ("http://[::1]:47832".to_string(), String::new()))
    };
    if plaintext.is_empty() {
        return Err(
            "voxix service client is not registered yet; try again in a moment".to_string(),
        );
    }

    // 16 kHz s16le mono ≈ 32 KB/s; the encode is trivially cheap.
    let wav = crate::audio_toolkit::encode_wav_bytes(samples)
        .map_err(|e| format!("failed to encode recorded audio: {e}"))?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(150))
        .build()
        .map_err(|e| format!("http client build failed: {e}"))?;

    let mut last_err = "voxix service is not reachable".to_string();
    for base in service_base_candidates(&base) {
        match transcribe_via(&client, &base, &plaintext, &wav).await {
            Ok(t) => return Ok(t),
            Err((is_connect, msg)) => {
                // Only fall back on connection-level failures (e.g. the port
                // was squatted by another local service). HTTP error responses
                // are service errors — no point re-trying on another port.
                if !is_connect {
                    return Err(msg);
                }
                last_err = msg;
            }
        }
    }
    Err(last_err)
}

/// Distinguishes connection-level failure (try next candidate) from an
/// actual service error (return immediately).
async fn transcribe_via(
    client: &reqwest::Client,
    base: &str,
    plaintext: &str,
    wav: &[u8],
) -> Result<String, (bool, String)> {
    let connect = |e: reqwest::Error| -> (bool, String) {
        if e.is_connect() {
            (
                true,
                "voxix service is not running; start it (login item) or check the daemon"
                    .to_string(),
            )
        } else {
            (false, format!("voxix service error: {e}"))
        }
    };
    let post = client
        .post(format!("{base}/v1/transcriptions"))
        .bearer_auth(&plaintext)
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .body(wav.to_vec())
        .send()
        .await
        .map_err(connect)?;

    let status = post.status();
    if status.as_u16() != 202 {
        let code = status.as_u16();
        let body = post.text().await.unwrap_or_default();
        let msg = body.chars().take(300).collect::<String>();
        return Err((
            false,
            match code {
                400 => format!("voxix service rejected the audio: {msg}"),
                409 => {
                    "speech engine is busy with another session; try again in a moment".to_string()
                }
                413 => "the recording exceeded the size limit (~10 minutes)".to_string(),
                429 => {
                    "another transcription is already in flight; try again in a moment".to_string()
                }
                _ => format!("voxix service error (HTTP {code}): {msg}"),
            },
        ));
    }

    #[derive(serde::Deserialize)]
    struct BatchAccepted {
        status_url: String,
    }
    let accepted: BatchAccepted = post
        .json()
        .await
        .map_err(|e| (false, format!("voxix service response parse failed: {e}")))?;
    let status_url = format!("{base}{}", accepted.status_url);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    loop {
        if std::time::Instant::now() >= deadline {
            let _ = client
                .delete(&status_url)
                .bearer_auth(plaintext)
                .send()
                .await;
            return Err((
                false,
                "transcription timed out after 90 seconds".to_string(),
            ));
        }
        #[derive(serde::Deserialize)]
        struct JobStatus {
            state: String,
            result: Option<JobResult>,
            error: Option<JobError>,
        }
        #[derive(serde::Deserialize)]
        struct JobResult {
            transcript: String,
        }
        #[derive(serde::Deserialize)]
        struct JobError {
            message: String,
        }
        let poll = client
            .get(&status_url)
            .bearer_auth(plaintext)
            .send()
            .await
            .map_err(|e| (true, format!("status poll failed: {e}")))?;
        let job: JobStatus = poll
            .json()
            .await
            .map_err(|e| (false, format!("status parse failed: {e}")))?;
        match job.state.as_str() {
            "completed" => {
                return job
                    .result
                    .map(|r| r.transcript)
                    .ok_or_else(|| (false, "completed job carried no transcript".to_string()));
            }
            "failed" => {
                return Err((
                    false,
                    job.error
                        .map(|e| e.message)
                        .unwrap_or_else(|| "transcription failed".to_string()),
                ))
            }
            "cancelled" => {
                return Err((false, "transcription was cancelled".to_string()));
            }
            _ => {}
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

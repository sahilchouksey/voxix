//! M3 batch transcription surface (SDK plan §7 / §15 G11).
//!
//! POST /v1/transcriptions:
//! - Admission slot taken BEFORE body read (anti-slowloris).
//! - WAV metadata validated BEFORE allocation (hostile-header → 400
//!   `invalid-audio`; NaN/inf floats and truncated sample counts → 400).
//! - 64 MiB body cap → 413; normalized-sample cap 16,777,216 f32 → 400.
//! - Capacity-1 command queue shared with prepare → 429 `queue-full`.
//! - Response: 202 `{job_id, state:"processing", status_url}`.
//!
//! GET /v1/transcriptions/{job} → `{job_id, state, result?, error?}`.
//! DELETE /v1/transcriptions/{job} → best-effort cancel 204/409/404.

use crate::service::api::{ApiError, BatchJobError, BatchJobResponse, BatchJobStatus, BatchResult};
use crate::service::state::{BatchJob, BatchJobState, ServiceState};
use axum::{
    extract::{Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use std::sync::Arc;

/// 64 MiB body cap (§15 G11).
pub const BATCH_BODY_CAP: usize = 64 * 1024 * 1024;
/// Normalized-sample cap: 16,777,216 f32 samples ≈ 17.5 min at 16 kHz.
pub const BATCH_SAMPLE_CAP: usize = 16_777_216;

fn rid() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn job_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn err(status: StatusCode, code: &str, message: &str) -> Response {
    crate::service::routes::error_response(
        status,
        ApiError::new(code, message, false, &rid()),
        None,
    )
}

fn err_retryable(status: StatusCode, code: &str, message: &str, retry: &str) -> Response {
    crate::service::routes::error_response(
        status,
        ApiError::new(code, message, true, &rid()),
        Some(retry),
    )
}

// ---------------------------------------------------------------------------
// WAV parsing helpers.
// ---------------------------------------------------------------------------

/// RIFF WAV header fields we need to validate.
#[derive(Debug)]
struct WavMeta {
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
    audio_format: u16,
    /// Total data chunk byte size (from the header).
    data_size: u32,
}

/// Parse the WAV header from the raw body bytes. Returns `Err` with an
/// `invalid-audio` message for any violation (hostile headers, unsupported
/// formats, truncated sample counts, NaN/Inf in float32 streams).
fn parse_wav_header(bytes: &[u8]) -> Result<(WavMeta, usize), String> {
    // Minimum RIFF header: "RIFF"(4) + size(4) + "WAVE"(4) = 12 bytes.
    if bytes.len() < 12 {
        return Err("file too short to be a WAV".into());
    }
    if &bytes[0..4] != b"RIFF" {
        return Err("not a RIFF file".into());
    }
    if &bytes[8..12] != b"WAVE" {
        return Err("RIFF type is not WAVE".into());
    }

    // Walk chunks.
    let mut pos = 12usize;
    let mut fmt_parsed = false;
    let mut meta = WavMeta {
        channels: 0,
        sample_rate: 0,
        bits_per_sample: 0,
        audio_format: 0,
        data_size: 0,
    };
    let mut data_offset = 0usize;

    while pos + 8 <= bytes.len() {
        let chunk_id = &bytes[pos..pos + 4];
        let chunk_size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        pos += 8;

        if chunk_id == b"fmt " {
            if chunk_size < 16 {
                return Err(format!("fmt chunk too small: {chunk_size}"));
            }
            if pos + 16 > bytes.len() {
                return Err("fmt chunk truncated in header".into());
            }
            meta.audio_format = u16::from_le_bytes(bytes[pos..pos + 2].try_into().unwrap());
            meta.channels = u16::from_le_bytes(bytes[pos + 2..pos + 4].try_into().unwrap());
            meta.sample_rate = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap());
            meta.bits_per_sample =
                u16::from_le_bytes(bytes[pos + 14..pos + 16].try_into().unwrap());
            fmt_parsed = true;
        } else if chunk_id == b"data" {
            meta.data_size = chunk_size as u32;
            data_offset = pos;
        }

        // Advance (chunks are word-aligned).
        let advance = chunk_size + (chunk_size & 1);
        pos = pos.saturating_add(advance);
        if fmt_parsed && data_offset > 0 {
            break;
        }
    }

    if !fmt_parsed {
        return Err("no fmt chunk found".into());
    }
    if data_offset == 0 {
        return Err("no data chunk found".into());
    }

    // audio_format: 1 = PCM, 3 = IEEE_FLOAT, 0xFFFE = EXTENSIBLE.
    match meta.audio_format {
        1 => {
            // Integer PCM: 8/16/24/32-bit.
            if !matches!(meta.bits_per_sample, 8 | 16 | 24 | 32) {
                return Err(format!(
                    "PCM bit depth {} not supported (8/16/24/32 only)",
                    meta.bits_per_sample
                ));
            }
        }
        3 => {
            // IEEE float: only 32-bit in v1.
            if meta.bits_per_sample != 32 {
                return Err(format!(
                    "IEEE float bit depth {} not supported (32 only)",
                    meta.bits_per_sample
                ));
            }
        }
        other => {
            return Err(format!(
                "unsupported audio_format 0x{other:04X} (PCM=1, IEEE_FLOAT=3 only)"
            ));
        }
    }

    // Channel and sample-rate range checks.
    if meta.channels == 0 || meta.channels > 8 {
        return Err(format!(
            "channel count {} out of range (1–8)",
            meta.channels
        ));
    }
    if meta.sample_rate < 8_000 || meta.sample_rate > 192_000 {
        return Err(format!(
            "sample rate {} Hz out of range (8000–192000)",
            meta.sample_rate
        ));
    }

    // Truncated sample count: the declared data_size must fit in what we have.
    let available = bytes.len().saturating_sub(data_offset);
    let declared = meta.data_size as usize;
    if declared > available {
        return Err(format!(
            "data chunk declares {declared} bytes but only {available} present (truncated)"
        ));
    }

    // Sample cap pre-check: compute the decoded sample count.
    let bytes_per_sample = (meta.bits_per_sample as usize + 7) / 8;
    let frame_bytes = bytes_per_sample * meta.channels as usize;
    if frame_bytes == 0 {
        return Err("zero-width audio frame".into());
    }
    let frame_count = declared / frame_bytes;
    // Downmix to mono then resample to 16 kHz → normalized sample count.
    let normalized = if meta.sample_rate != 16_000 {
        // Rough upper-bound for pre-allocation check (conservative: ceiling).
        (frame_count as u64 * 16_000 + meta.sample_rate as u64 - 1) / meta.sample_rate as u64
    } else {
        frame_count as u64
    };
    if normalized as usize > BATCH_SAMPLE_CAP {
        return Err(format!(
            "audio exceeds normalized-sample cap ({} > {BATCH_SAMPLE_CAP})",
            normalized
        ));
    }

    Ok((meta, data_offset))
}

/// Decode WAV bytes to 16 kHz mono f32 samples.
fn decode_wav_to_mono16k(bytes: &[u8]) -> Result<(Vec<f32>, u64), String> {
    let (meta, data_offset) = parse_wav_header(bytes)?;
    let declared = meta.data_size as usize;
    let data = &bytes[data_offset..data_offset + declared];

    let bytes_per_sample = (meta.bits_per_sample as usize + 7) / 8;
    let frame_bytes = bytes_per_sample * meta.channels as usize;
    let frame_count = declared / frame_bytes;

    // Decode each frame to one f32 per channel.
    let mut samples: Vec<f32> = Vec::with_capacity(frame_count);
    for i in 0..frame_count {
        let frame = &data[i * frame_bytes..(i + 1) * frame_bytes];
        let mut sum = 0.0f64;
        for ch in 0..meta.channels as usize {
            let s = &frame[ch * bytes_per_sample..(ch + 1) * bytes_per_sample];
            let fval = match (meta.audio_format, meta.bits_per_sample) {
                (1, 8) => (s[0] as f64 - 128.0) / 128.0,
                (1, 16) => i16::from_le_bytes(s.try_into().unwrap()) as f64 / 32768.0,
                (1, 24) => {
                    let v = i32::from_le_bytes([s[0], s[1], s[2], 0]);
                    (v >> 8) as f64 / 8388608.0
                }
                (1, 32) => i32::from_le_bytes(s.try_into().unwrap()) as f64 / 2147483648.0,
                (3, 32) => f32::from_le_bytes(s.try_into().unwrap()) as f64,
                _ => 0.0,
            };
            sum += fval;
        }
        samples.push((sum / meta.channels as f64) as f32);
    }

    // Scan for NaN/Inf in IEEE float32 streams.
    if meta.audio_format == 3 {
        for &s in &samples {
            if s.is_nan() || s.is_infinite() {
                return Err("IEEE float32 WAV contains NaN or Inf samples".into());
            }
        }
    }

    // Resample to 16 kHz if needed (linear interpolation).
    let mono16k = if meta.sample_rate != 16_000 {
        let ratio = meta.sample_rate as f64 / 16_000.0;
        let out_len = ((samples.len() as f64) / ratio).ceil() as usize;
        let mut out = Vec::with_capacity(out_len);
        for i in 0..out_len {
            let src_pos = i as f64 * ratio;
            let idx = src_pos as usize;
            let frac = (src_pos - idx as f64) as f32;
            let a = samples.get(idx).copied().unwrap_or(0.0);
            let b = samples.get(idx + 1).copied().unwrap_or(a);
            out.push(a + (b - a) * frac);
        }
        out
    } else {
        samples
    };

    // Final sample cap check (post resample).
    if mono16k.len() > BATCH_SAMPLE_CAP {
        return Err(format!(
            "decoded audio exceeds sample cap ({} > {BATCH_SAMPLE_CAP})",
            mono16k.len()
        ));
    }

    // Duration from input frame count / sample_rate.
    let duration_ms = (frame_count as u64 * 1000) / meta.sample_rate as u64;

    Ok((mono16k, duration_ms))
}

// ---------------------------------------------------------------------------
// Route handlers.
// ---------------------------------------------------------------------------

/// `POST /v1/transcriptions` — raw request handler so we can take the
/// admission slot BEFORE reading the body (anti-slowloris).
pub async fn post_transcriptions(
    State(state): State<Arc<ServiceState>>,
    request: Request,
) -> Response {
    // 1. Try to acquire the command slot immediately (capacity-1 queue).
    let slot = match state.command_slot.clone().try_lock_owned() {
        Ok(slot) => slot,
        Err(_) => {
            // Both prepare-busy and queue-full share the same slot; the
            // spec names the batch-on-batch case 429 `queue-full`.
            return err_retryable(
                StatusCode::TOO_MANY_REQUESTS,
                "queue-full",
                "another batch transcription or prepare is already in progress; retry shortly",
                "2",
            );
        }
    };

    // 2. Enforce the 64 MiB body cap BEFORE collecting the body.
    let content_length = request
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok());
    if let Some(cl) = content_length {
        if cl > BATCH_BODY_CAP {
            return crate::service::routes::error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                ApiError::new(
                    "payload_too_large",
                    &format!("body exceeds 64 MiB limit ({cl} > {BATCH_BODY_CAP})"),
                    false,
                    &rid(),
                ),
                None,
            );
        }
    }

    // 3. Collect the body (bounded by axum DefaultBodyLimit already at 64 MiB).
    let body_bytes = match axum::body::to_bytes(request.into_body(), BATCH_BODY_CAP).await {
        Ok(b) => b,
        Err(_) => {
            return crate::service::routes::error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                ApiError::new(
                    "payload_too_large",
                    "body exceeds 64 MiB limit",
                    false,
                    &rid(),
                ),
                None,
            );
        }
    };

    // 3.5 Persist the raw WAV verbatim (best-effort; failure must not fail
    // the request). The body is written as received — no re-encode — so the
    // keyboard's Retry can re-transcribe exactly what was sent.
    let wav_file_name = crate::service::audio_record::wav_file_name();
    let recorded_file_name = match crate::service::audio_record::persist_wav(
        &state.config.data_dir,
        &wav_file_name,
        &body_bytes,
    ) {
        Ok(name) => name,
        Err(e) => {
            log::error!(target: "service::batch", "failed to persist WAV {wav_file_name}: {e}");
            wav_file_name
        }
    };

    // 4. Validate WAV metadata BEFORE allocation (hostile-header rejection).
    let (audio, duration_ms) = match decode_wav_to_mono16k(&body_bytes) {
        Ok(pair) => pair,
        Err(msg) => {
            return err(
                StatusCode::BAD_REQUEST,
                "invalid-audio",
                &format!("WAV validation failed: {msg}"),
            );
        }
    };

    // 5. Try to acquire the inference lease (409 service_busy if contended).
    let inference_permit = match state.inference.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            return err_retryable(
                StatusCode::CONFLICT,
                "service_busy",
                "engine lease is held by a live session; retry shortly",
                "1",
            );
        }
    };

    // 6. Register job + kick off background task.
    let jid = job_id();
    let status_url = format!("/v1/transcriptions/{}", jid);
    let job = BatchJob {
        job_id: jid.clone(),
        state: BatchJobState::Processing,
        result: None,
        error: None,
        created_at_ms: crate::service::state::now_ms(),
    };
    state.register_job(job);

    let task_state = state.clone();
    let task_jid = jid.clone();
    tokio::spawn(async move {
        // Hold the slot and the inference permit for the duration.
        let _slot = slot;
        let _inf = inference_permit;

        let engine = task_state.engine.clone();
        let result = tokio::task::spawn_blocking(move || engine.batch_transcribe(&audio)).await;

        let (new_state, result_field, error_field) = match result {
            Ok(Ok(text)) => (
                BatchJobState::Completed,
                Some(BatchResult {
                    transcript: text,
                    duration_ms,
                }),
                None,
            ),
            Ok(Err(msg)) => {
                log::error!(target: "service::batch", "job {task_jid} engine failure: {msg}");
                (
                    BatchJobState::Failed,
                    None,
                    Some(BatchJobError {
                        code: "engine_error".into(),
                        message: msg,
                        retryable: true,
                        details: None,
                    }),
                )
            }
            Err(e) => {
                log::error!(target: "service::batch", "job {task_jid} batch worker panicked: {e}");
                (
                    BatchJobState::Failed,
                    None,
                    Some(BatchJobError {
                        code: "internal_error".into(),
                        message: format!("batch worker panicked: {e}"),
                        retryable: false,
                        details: None,
                    }),
                )
            }
        };

        // Only commit if the job wasn't cancelled while we were working.
        let mut committed = false;
        {
            let mut jobs = task_state.jobs.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(job) = jobs.get_mut(&task_jid) {
                if job.state == BatchJobState::Processing {
                    job.state = new_state.clone();
                    job.result = result_field.clone();
                    job.error = error_field;
                    committed = true;
                }
            }
        }

        // GUI history row (skipped only on user-cancel — mirrors the GUI,
        // which does not save on cancel). Written regardless of outcome, so
        // failed/panicked jobs keep a History entry the user can Retry.
        if committed {
            let text = result_field
                .as_ref()
                .map(|r| r.transcript.clone())
                .unwrap_or_default();
            if let Err(e) = crate::service::audio_record::save_gui_history_entry(
                &task_state.config.data_dir,
                &recorded_file_name,
                &text,
            ) {
                log::error!(target: "service::batch", "job {task_jid} gui history write failed: {e}");
            }
        }

        // History write (non-fatal, log on error).
        if new_state == BatchJobState::Completed {
            if let Some(ref result) = result_field {
                if let Some(ref sh) = task_state.service_history {
                    let patch = task_state
                        .patch_config
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .save_history_default;
                    if patch {
                        if let Err(e) = sh.append(&crate::service::history::HistoryWriteRequest {
                            transcription_text: result.transcript.clone(),
                            duration_ms: Some(result.duration_ms as i64),
                            model_id: None,
                            language: None,
                            source: "batch",
                        }) {
                            log::warn!(
                                target: "service::batch",
                                "history write failed for job {task_jid}: {e}"
                            );
                        }
                    }
                }
            }
        }
    });

    let response = BatchJobResponse {
        job_id: jid,
        state: "processing",
        status_url,
    };
    (StatusCode::ACCEPTED, Json(response)).into_response()
}

/// `GET /v1/transcriptions/{job}`
pub async fn get_transcription(
    State(state): State<Arc<ServiceState>>,
    Path(job_id): Path<String>,
) -> Response {
    let Some(job) = state.get_job(&job_id) else {
        return err(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such transcription job",
        );
    };
    Json(BatchJobStatus {
        job_id: job.job_id,
        state: job.state.as_str().to_string(),
        result: job.result,
        error: job.error,
    })
    .into_response()
}

/// `DELETE /v1/transcriptions/{job}` — best-effort cancel (202 nonterminal,
/// 409 if already settled, 404 unknown).
pub async fn delete_transcription(
    State(state): State<Arc<ServiceState>>,
    Path(job_id): Path<String>,
) -> Response {
    let Some(job) = state.get_job(&job_id) else {
        return err(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such transcription job",
        );
    };
    if job.state != BatchJobState::Processing {
        return crate::service::routes::error_response(
            StatusCode::CONFLICT,
            ApiError::new(
                "job_settled",
                "job is already in a terminal state",
                false,
                &rid(),
            ),
            None,
        );
    }
    state.cancel_job(&job_id);
    StatusCode::NO_CONTENT.into_response()
}

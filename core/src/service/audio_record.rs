//! Persist raw batch WAV bytes + GUI history rows (best-effort, log-only).
//!
//! Mirrors the GUI dictation path: before decoding, the raw request body is
//! written verbatim to `<data_dir>/recordings/<file_name>.wav` using the same
//! `voxix-<unix-secs>.wav` naming the GUI uses. After the background batch
//! task settles (completed/failed/panicked — never cancelled), a row is
//! inserted into the GUI's `history.db` `transcription_history` table so the
//! keyboard's History tab can replay the audio or retry the transcription.
//!
//! All failures are log-only; nothing in this module may panic the worker.

use chrono::{DateTime, Local, Utc};
use rusqlite::{params, Connection};
use std::path::Path;

/// Same naming the GUI uses: `voxix-<unix-secs>.wav`. On a same-second
/// collision (the GUI has the same property) a nanosecond suffix is appended
/// so a concurrent request never overwrites another recording.
pub fn wav_file_name() -> String {
    format!("voxix-{}.wav", Utc::now().timestamp())
}

/// Persist the raw WAV body verbatim (no re-encode). Returns the file_name
/// actually used (it differs from `file_name` only on a same-second name
/// collision). Errors are returned as strings; callers log and continue —
/// persistence failure must not fail the request.
pub fn persist_wav(data_dir: &Path, file_name: &str, body: &[u8]) -> Result<String, String> {
    let recordings_dir = data_dir.join("recordings");
    std::fs::create_dir_all(&recordings_dir).map_err(|e| {
        format!(
            "cannot create recordings dir {}: {e}",
            recordings_dir.display()
        )
    })?;
    let mut path = recordings_dir.join(file_name);
    if path.exists() {
        path = recordings_dir.join(format!(
            "voxix-{}-{}.wav",
            Utc::now().timestamp(),
            Utc::now().timestamp_subsec_nanos()
        ));
    }
    std::fs::write(&path, body).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_name.to_string()))
}

/// Title generation copied from `HistoryManager::format_timestamp_title` so
/// service-written rows are indistinguishable from GUI-written ones.
fn gui_title(timestamp: i64) -> String {
    match DateTime::from_timestamp(timestamp, 0) {
        Some(utc_datetime) => {
            let local_datetime = utc_datetime.with_timezone(&Local);
            local_datetime.format("%B %e, %Y - %l:%M%p").to_string()
        }
        None => format!("Recording {timestamp}"),
    }
}

const REQUIRED_COLUMNS: [&str; 9] = [
    "id",
    "file_name",
    "timestamp",
    "saved",
    "title",
    "transcription_text",
    "post_processed_text",
    "post_process_prompt",
    "post_process_requested",
];

/// Insert a `transcription_history` row mirroring `HistoryManager::save_entry`:
/// `timestamp` in Unix *seconds*, same local-format title, `saved=0`,
/// `post_processed_text`/`post_process_prompt` NULL, `post_process_requested=0`.
///
/// Defensive schema check first: `CREATE TABLE IF NOT EXISTS` matching the live
/// schema, then `PRAGMA table_info` must contain every expected column — if it
/// doesn't, the insert is skipped rather than writing garbage into a DB whose
/// schema drifted.
pub fn save_gui_history_entry(
    data_dir: &Path,
    file_name: &str,
    transcription_text: &str,
) -> Result<(), String> {
    let db_path = data_dir.join("history.db");
    let conn = Connection::open(&db_path)
        .map_err(|e| format!("cannot open {}: {e}", db_path.display()))?;
    // WAL + busy_timeout: tolerate WAL contention with the running GUI.
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
        .map_err(|e| format!("cannot configure {} pragmas: {e}", db_path.display()))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS transcription_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_name TEXT NOT NULL,
            timestamp INTEGER NOT NULL,
            saved BOOLEAN NOT NULL DEFAULT 0,
            title TEXT NOT NULL,
            transcription_text TEXT NOT NULL,
            post_processed_text TEXT,
            post_process_prompt TEXT,
            post_process_requested BOOLEAN NOT NULL DEFAULT 0
        );",
    )
    .map_err(|e| format!("cannot ensure transcription_history schema: {e}"))?;

    let mut stmt = conn
        .prepare("PRAGMA table_info(transcription_history)")
        .map_err(|e| format!("cannot read history.db schema: {e}"))?;
    let columns: std::collections::HashSet<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| format!("cannot read history.db schema: {e}"))?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);
    for required in REQUIRED_COLUMNS {
        if !columns.contains(required) {
            return Err(format!(
                "history.db transcription_history is missing column `{required}`; refusing to insert"
            ));
        }
    }

    let timestamp = Utc::now().timestamp(); // seconds, matches save_entry
    let title = gui_title(timestamp);
    conn.execute(
        "INSERT INTO transcription_history (
            file_name,
            timestamp,
            saved,
            title,
            transcription_text,
            post_processed_text,
            post_process_prompt,
            post_process_requested
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            file_name,
            timestamp,
            false,
            &title,
            transcription_text,
            Option::<String>::None,
            Option::<String>::None,
            false,
        ],
    )
    .map_err(|e| format!("history.db insert failed: {e}"))?;
    Ok(())
}

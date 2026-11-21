//! Service-owned history database (SDK plan §12 M3).
//!
//! A lightweight SQLite store for batch/stream transcription history that is
//! entirely independent of the Tauri `AppHandle`-bound
//! [`crate::managers::history::HistoryManager`]. Writers are batch completion
//! and WS final events; errors never block inference (log-only).
//!
//! Schema is forward-migratable: nullable `duration_ms`, `model_id`,
//! `language` columns are added if missing.

use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;

/// A single history entry in the service-owned store.
#[derive(Debug, Clone)]
pub struct ServiceHistoryRow {
    pub id: i64,
    /// Unix millis.
    pub timestamp_ms: i64,
    pub title: String,
    pub transcription_text: String,
    pub duration_ms: Option<i64>,
    pub model_id: Option<String>,
    pub language: Option<String>,
    /// "batch" | "stream"
    pub source: String,
    /// Display name of the client that submitted this transcription (e.g. "voxix-gui (macos)").
    pub client_label: Option<String>,
    /// Emoji, unicode character, or image URL for the client's icon.
    pub client_icon: Option<String>,
}

/// Write request submitted by batch/stream completion paths.
#[derive(Debug, Clone)]
pub struct HistoryWriteRequest {
    pub transcription_text: String,
    pub duration_ms: Option<i64>,
    pub model_id: Option<String>,
    pub language: Option<String>,
    /// "batch" or "stream"
    pub source: &'static str,
    /// Display name of the submitting client (from X-Client-Label header).
    pub client_label: Option<String>,
    /// Client icon (from X-Client-Icon header): emoji, unicode, or image URL.
    pub client_icon: Option<String>,
}

/// Service-owned history store. Thread-safe via an internal `Mutex<Connection>`.
pub struct ServiceHistory {
    conn: Mutex<Connection>,
}

impl ServiceHistory {
    /// Open (or create) the service history database at `state_dir/service_history.db`.
    pub fn open(state_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(state_dir).map_err(|e| format!("cannot create state dir: {e}"))?;
        let db_path = state_dir.join("service_history.db");
        let conn = Connection::open(&db_path)
            .map_err(|e| format!("cannot open service_history.db: {e}"))?;
        // The launchd daemon and the GUI's in-process service can both hold
        // this database; WAL + a busy timeout keep concurrent writers safe.
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=2000;")
            .map_err(|e| format!("cannot configure service_history.db pragmas: {e}"))?;
        let history = ServiceHistory {
            conn: Mutex::new(conn),
        };
        history.migrate()?;
        Ok(history)
    }

    fn migrate(&self) -> Result<(), String> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS service_history (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp_ms    INTEGER NOT NULL,
                title           TEXT    NOT NULL,
                transcription_text TEXT NOT NULL,
                duration_ms     INTEGER,
                model_id        TEXT,
                language        TEXT,
                source          TEXT    NOT NULL DEFAULT 'service'
            );
            ",
        )
        .map_err(|e| format!("service_history migration failed: {e}"))?;
        // Additive migration: add client_label and client_icon columns if absent.
        // ALTER TABLE ADD COLUMN fails with SQLITE_ERROR when the column already
        // exists; we match on that code and treat it as a no-op.
        for stmt in [
            "ALTER TABLE service_history ADD COLUMN client_label TEXT",
            "ALTER TABLE service_history ADD COLUMN client_icon TEXT",
        ] {
            match conn.execute(stmt, []) {
                Ok(_) => {}
                Err(rusqlite::Error::SqliteFailure(err, _))
                    if err.code == rusqlite::ErrorCode::Unknown => {}
                Err(e) => return Err(format!("service_history migration failed: {e}")),
            }
        }
        Ok(())
    }

    /// Append a new entry. Errors are non-fatal — callers log and continue.
    pub fn append(&self, req: &HistoryWriteRequest) -> Result<i64, String> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let title = chrono::DateTime::from_timestamp(now_ms / 1000, 0)
            .map(|dt| dt.format("%B %-d, %Y – %-I:%M%p").to_string())
            .unwrap_or_else(|| format!("Recording {now_ms}"));
        conn.execute(
            "INSERT INTO service_history
                (timestamp_ms, title, transcription_text, duration_ms, model_id, language, source,
                 client_label, client_icon)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                now_ms,
                title,
                &req.transcription_text,
                req.duration_ms,
                req.model_id.as_deref(),
                req.language.as_deref(),
                req.source,
                req.client_label.as_deref(),
                req.client_icon.as_deref(),
            ],
        )
        .map_err(|e| format!("service_history insert failed: {e}"))?;
        Ok(conn.last_insert_rowid())
    }

    /// Paginated query. `cursor` is the last seen id (exclusive); returns at
    /// most `limit` (1–100) entries ordered newest-first.
    pub fn list(
        &self,
        cursor: Option<i64>,
        limit: usize,
    ) -> Result<(Vec<ServiceHistoryRow>, bool), String> {
        let limit = limit.clamp(1, 100);
        let fetch = (limit + 1) as i64;
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows = Vec::new();
        if let Some(cur) = cursor {
            let mut stmt = conn
                .prepare(
                    "SELECT id, timestamp_ms, title, transcription_text,
                            duration_ms, model_id, language, source,
                            client_label, client_icon
                     FROM service_history WHERE id < ?1
                     ORDER BY id DESC LIMIT ?2",
                )
                .map_err(|e| e.to_string())?;
            let mut q = stmt.query(params![cur, fetch]).map_err(|e| e.to_string())?;
            while let Some(row) = q.next().map_err(|e| e.to_string())? {
                rows.push(Self::map_row(row)?);
            }
        } else {
            let mut stmt = conn
                .prepare(
                    "SELECT id, timestamp_ms, title, transcription_text,
                            duration_ms, model_id, language, source,
                            client_label, client_icon
                     FROM service_history ORDER BY id DESC LIMIT ?1",
                )
                .map_err(|e| e.to_string())?;
            let mut q = stmt.query(params![fetch]).map_err(|e| e.to_string())?;
            while let Some(row) = q.next().map_err(|e| e.to_string())? {
                rows.push(Self::map_row(row)?);
            }
        }
        let has_more = rows.len() > limit;
        if has_more {
            rows.pop();
        }
        Ok((rows, has_more))
    }

    fn map_row(row: &rusqlite::Row<'_>) -> Result<ServiceHistoryRow, String> {
        Ok(ServiceHistoryRow {
            id: row.get("id").map_err(|e| e.to_string())?,
            timestamp_ms: row.get("timestamp_ms").map_err(|e| e.to_string())?,
            title: row.get("title").map_err(|e| e.to_string())?,
            transcription_text: row.get("transcription_text").map_err(|e| e.to_string())?,
            duration_ms: row.get("duration_ms").map_err(|e| e.to_string())?,
            model_id: row.get("model_id").map_err(|e| e.to_string())?,
            language: row.get("language").map_err(|e| e.to_string())?,
            source: row.get("source").map_err(|e| e.to_string())?,
            client_label: row.get("client_label").map_err(|e| e.to_string())?,
            client_icon: row.get("client_icon").map_err(|e| e.to_string())?,
        })
    }

    /// Fetch a single entry by id.
    pub fn get(&self, id: i64) -> Result<Option<ServiceHistoryRow>, String> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let result = conn.query_row(
            "SELECT id, timestamp_ms, title, transcription_text,
                    duration_ms, model_id, language, source,
                    client_label, client_icon
             FROM service_history WHERE id = ?1",
            params![id],
            |row| {
                Ok(ServiceHistoryRow {
                    id: row.get("id")?,
                    timestamp_ms: row.get("timestamp_ms")?,
                    title: row.get("title")?,
                    transcription_text: row.get("transcription_text")?,
                    duration_ms: row.get("duration_ms")?,
                    model_id: row.get("model_id")?,
                    language: row.get("language")?,
                    source: row.get("source")?,
                    client_label: row.get("client_label")?,
                    client_icon: row.get("client_icon")?,
                })
            },
        );
        match result {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
}

impl ServiceHistoryRow {
    pub fn to_api(&self) -> crate::service::api::ServiceHistoryEntry {
        crate::service::api::ServiceHistoryEntry {
            id: self.id,
            timestamp_ms: self.timestamp_ms,
            title: self.title.clone(),
            transcription_text: self.transcription_text.clone(),
            duration_ms: self.duration_ms,
            model_id: self.model_id.clone(),
            language: self.language.clone(),
            source: self.source.clone(),
            client_label: self.client_label.clone(),
            client_icon: self.client_icon.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_history() -> ServiceHistory {
        let conn = Connection::open_in_memory().expect("in-memory db");
        let h = ServiceHistory {
            conn: Mutex::new(conn),
        };
        h.migrate().expect("migrate");
        h
    }

    #[test]
    fn append_and_list_and_get() {
        let h = mem_history();
        let id = h
            .append(&HistoryWriteRequest {
                transcription_text: "hello world".into(),
                duration_ms: Some(1234),
                model_id: Some("tiny".into()),
                language: Some("en".into()),
                source: "batch",
                client_label: Some("voxix-gui (macos)".into()),
                client_icon: None,
            })
            .expect("append");
        assert!(id > 0);

        let (rows, has_more) = h.list(None, 10).expect("list");
        assert_eq!(rows.len(), 1);
        assert!(!has_more);
        assert_eq!(rows[0].transcription_text, "hello world");
        assert_eq!(rows[0].client_label.as_deref(), Some("voxix-gui (macos)"));
        assert!(rows[0].client_icon.is_none());

        let row = h.get(id).expect("get").expect("exists");
        assert_eq!(row.source, "batch");
        assert_eq!(row.duration_ms, Some(1234));
    }

    #[test]
    fn list_pagination_cursor() {
        let h = mem_history();
        for i in 0..5 {
            h.append(&HistoryWriteRequest {
                transcription_text: format!("entry {i}"),
                duration_ms: None,
                model_id: None,
                language: None,
                source: "stream",
                client_label: None,
                client_icon: None,
            })
            .expect("append");
        }
        let (page1, has_more) = h.list(None, 3).expect("page1");
        assert_eq!(page1.len(), 3);
        assert!(has_more);
        let cursor = page1.last().unwrap().id;
        let (page2, has_more2) = h.list(Some(cursor), 3).expect("page2");
        assert_eq!(page2.len(), 2);
        assert!(!has_more2);
    }

    #[test]
    fn get_unknown_id_returns_none() {
        let h = mem_history();
        assert!(h.get(9999).expect("no err").is_none());
    }

    #[test]
    fn client_label_and_icon_round_trip() {
        let h = mem_history();
        let id = h
            .append(&HistoryWriteRequest {
                transcription_text: "test with client".into(),
                duration_ms: None,
                model_id: None,
                language: None,
                source: "batch",
                client_label: Some("my-sdk-client".into()),
                client_icon: Some("🎙".into()),
            })
            .expect("append");
        let row = h.get(id).expect("get").expect("exists");
        assert_eq!(row.client_label.as_deref(), Some("my-sdk-client"));
        assert_eq!(row.client_icon.as_deref(), Some("🎙"));
    }

    #[test]
    fn migration_adds_columns_to_legacy_schema() {
        // Simulate a database that was created without the client columns.
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(
            "
            CREATE TABLE service_history (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp_ms    INTEGER NOT NULL,
                title           TEXT    NOT NULL,
                transcription_text TEXT NOT NULL,
                duration_ms     INTEGER,
                model_id        TEXT,
                language        TEXT,
                source          TEXT    NOT NULL DEFAULT 'service'
            );
            ",
        )
        .expect("create legacy table");
        // Seed a legacy row (no client columns).
        conn.execute(
            "INSERT INTO service_history (timestamp_ms, title, transcription_text, source) VALUES (1, 'Old', 'old text', 'batch')",
            [],
        )
        .expect("seed legacy row");

        let h = ServiceHistory {
            conn: std::sync::Mutex::new(conn),
        };
        // Running migrate() on the legacy table must succeed.
        h.migrate().expect("migrate legacy db");

        // Legacy row reads back with None for client fields.
        let (rows, _) = h.list(None, 10).expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].transcription_text, "old text");
        assert!(rows[0].client_label.is_none());
        assert!(rows[0].client_icon.is_none());

        // New row with client fields writes and reads back correctly.
        let new_id = h
            .append(&HistoryWriteRequest {
                transcription_text: "new entry".into(),
                duration_ms: None,
                model_id: None,
                language: None,
                source: "batch",
                client_label: Some("test-client".into()),
                client_icon: None,
            })
            .expect("append after migration");
        let new_row = h.get(new_id).expect("get").expect("exists");
        assert_eq!(new_row.client_label.as_deref(), Some("test-client"));
    }
}

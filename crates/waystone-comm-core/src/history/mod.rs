//! Session history database — SQLite-backed record of connection attempts and
//! session metadata (MASTERPLAN §10.2).
//!
//! The TOML directory file remains the authoritative source for entry
//! configuration. This module only stores history (what happened, when, how
//! long) so it never needs to be human-edited.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use sqlx::{sqlite::SqliteConnectOptions, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

use crate::directory::DirectoryEntry;

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ── Record types ──────────────────────────────────────────────────────────────

/// One row from `session_logs`.
#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub id: Uuid,
    pub entry_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub protocol: String,
    pub host: Option<String>,
    pub bytes_sent: i64,
    pub bytes_recv: i64,
    pub log_path: Option<String>,
}

/// One row from `connection_history`.
#[derive(Debug, Clone)]
pub struct ConnectionRecord {
    pub id: i64,
    pub entry_id: Uuid,
    pub connected_at: DateTime<Utc>,
    pub duration_s: Option<i64>,
    /// One of: `"connected"`, `"refused"`, `"timeout"`, `"error"`.
    pub outcome: String,
}

// ── Outcome constants ─────────────────────────────────────────────────────────

pub const OUTCOME_CONNECTED: &str = "connected";
pub const OUTCOME_REFUSED: &str = "refused";
pub const OUTCOME_TIMEOUT: &str = "timeout";
pub const OUTCOME_ERROR: &str = "error";

// ── Schema SQL ────────────────────────────────────────────────────────────────

const CREATE_SESSION_LOGS: &str = "
CREATE TABLE IF NOT EXISTS session_logs (
    id          TEXT    PRIMARY KEY,
    entry_id    TEXT    NOT NULL,
    started_at  TEXT    NOT NULL,
    ended_at    TEXT,
    protocol    TEXT    NOT NULL,
    host        TEXT,
    bytes_sent  INTEGER NOT NULL DEFAULT 0,
    bytes_recv  INTEGER NOT NULL DEFAULT 0,
    log_path    TEXT
)";

const CREATE_CONNECTION_HISTORY: &str = "
CREATE TABLE IF NOT EXISTS connection_history (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    entry_id     TEXT    NOT NULL,
    connected_at TEXT    NOT NULL,
    duration_s   INTEGER,
    outcome      TEXT
)";

// ── SessionHistoryDb ──────────────────────────────────────────────────────────

/// SQLite-backed session history store.
///
/// Thread-safe: the internal `SqlitePool` is cheaply cloneable and
/// async-safe — wrap in `Arc` when sharing across tasks.
pub struct SessionHistoryDb {
    pool: SqlitePool,
}

impl SessionHistoryDb {
    /// Open (or create) the history database at the default location
    /// (`~/.config/waystone-comm/history.db`).
    ///
    /// # Errors
    /// Returns an error if the database file cannot be created or the schema
    /// migration fails.
    pub async fn open_default() -> Result<Self, HistoryError> {
        let path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("waystone-comm")
            .join("history.db");
        Self::open(&path).await
    }

    /// Open (or create) the history database at the given path.
    ///
    /// # Errors
    /// Returns an error if the database file cannot be created or the schema
    /// migration fails.
    pub async fn open(path: &Path) -> Result<Self, HistoryError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePool::connect_with(opts).await?;
        sqlx::query(CREATE_SESSION_LOGS).execute(&pool).await?;
        sqlx::query(CREATE_CONNECTION_HISTORY)
            .execute(&pool)
            .await?;
        Ok(Self { pool })
    }

    // ── Write operations ──────────────────────────────────────────────────────

    /// Record the start of a new session. Returns the generated session ID
    /// which must be passed to [`end_session`] when the session finishes.
    ///
    /// # Errors
    /// Returns an error if the row cannot be inserted.
    pub async fn begin_session(
        &self,
        entry_id: Uuid,
        protocol: &str,
        host: Option<&str>,
    ) -> Result<Uuid, HistoryError> {
        let id = Uuid::new_v4();
        let started_at = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO session_logs (id, entry_id, started_at, protocol, host)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(entry_id.to_string())
        .bind(&started_at)
        .bind(protocol)
        .bind(host)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Update a session row with end-of-session statistics.
    ///
    /// # Errors
    /// Returns an error if the row cannot be updated.
    pub async fn end_session(
        &self,
        session_id: Uuid,
        bytes_sent: i64,
        bytes_recv: i64,
        log_path: Option<&str>,
    ) -> Result<(), HistoryError> {
        let ended_at = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE session_logs
             SET ended_at = ?, bytes_sent = ?, bytes_recv = ?, log_path = ?
             WHERE id = ?",
        )
        .bind(&ended_at)
        .bind(bytes_sent)
        .bind(bytes_recv)
        .bind(log_path)
        .bind(session_id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Append a row to `connection_history`.
    ///
    /// `outcome` should be one of the `OUTCOME_*` constants defined in this
    /// module.
    ///
    /// # Errors
    /// Returns an error if the row cannot be inserted.
    pub async fn record_connection(
        &self,
        entry_id: Uuid,
        duration_s: Option<i64>,
        outcome: &str,
    ) -> Result<(), HistoryError> {
        let connected_at = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO connection_history (entry_id, connected_at, duration_s, outcome)
             VALUES (?, ?, ?, ?)",
        )
        .bind(entry_id.to_string())
        .bind(&connected_at)
        .bind(duration_s)
        .bind(outcome)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ── Read operations ───────────────────────────────────────────────────────

    /// Return all `session_logs` rows for the given entry, newest first.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub async fn session_logs(&self, entry_id: Uuid) -> Result<Vec<SessionRecord>, HistoryError> {
        type Row = (
            String,
            String,
            String,
            Option<String>,
            String,
            Option<String>,
            i64,
            i64,
            Option<String>,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT id, entry_id, started_at, ended_at, protocol, host,
                    bytes_sent, bytes_recv, log_path
             FROM session_logs
             WHERE entry_id = ?
             ORDER BY started_at DESC",
        )
        .bind(entry_id.to_string())
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let id = Uuid::parse_str(&r.0).ok()?;
                let eid = Uuid::parse_str(&r.1).ok()?;
                let started_at = DateTime::parse_from_rfc3339(&r.2).ok()?.with_timezone(&Utc);
                let ended_at =
                    r.3.as_deref()
                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                        .map(|d| d.with_timezone(&Utc));
                Some(SessionRecord {
                    id,
                    entry_id: eid,
                    started_at,
                    ended_at,
                    protocol: r.4,
                    host: r.5,
                    bytes_sent: r.6,
                    bytes_recv: r.7,
                    log_path: r.8,
                })
            })
            .collect())
    }

    /// Return all `connection_history` rows for the given entry, newest first.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub async fn connection_history(
        &self,
        entry_id: Uuid,
    ) -> Result<Vec<ConnectionRecord>, HistoryError> {
        type Row = (i64, String, String, Option<i64>, String);
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT id, entry_id, connected_at, duration_s, outcome
             FROM connection_history
             WHERE entry_id = ?
             ORDER BY connected_at DESC",
        )
        .bind(entry_id.to_string())
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let eid = Uuid::parse_str(&r.1).ok()?;
                let connected_at = DateTime::parse_from_rfc3339(&r.2).ok()?.with_timezone(&Utc);
                Some(ConnectionRecord {
                    id: r.0,
                    entry_id: eid,
                    connected_at,
                    duration_s: r.3,
                    outcome: r.4,
                })
            })
            .collect())
    }

    /// Return the newest successful connection timestamp for a directory entry.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub async fn latest_successful_connection(
        &self,
        entry_id: Uuid,
    ) -> Result<Option<DateTime<Utc>>, HistoryError> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT connected_at
             FROM connection_history
             WHERE entry_id = ? AND outcome = ?
             ORDER BY connected_at DESC
             LIMIT 1",
        )
        .bind(entry_id.to_string())
        .bind(OUTCOME_CONNECTED)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|r| {
            DateTime::parse_from_rfc3339(&r.0)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        }))
    }

    // ── Migration ─────────────────────────────────────────────────────────────

    /// Seed `connection_history` from TOML `last_connected` timestamps.
    ///
    /// Only inserts a record for entries that have no existing
    /// `connection_history` rows, making this safe to call on every startup.
    /// Returns the number of records inserted.
    ///
    /// # Errors
    /// Returns an error if any database operation fails.
    pub async fn import_from_directory(
        &self,
        entries: &[DirectoryEntry],
    ) -> Result<usize, HistoryError> {
        let mut count = 0;
        for entry in entries {
            let Some(ts) = entry.last_connected else {
                continue;
            };
            let entry_id_str = entry.id.to_string();

            // Check whether any history already exists for this entry.
            let (existing,): (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM connection_history WHERE entry_id = ?")
                    .bind(&entry_id_str)
                    .fetch_one(&self.pool)
                    .await?;

            if existing == 0 {
                sqlx::query(
                    "INSERT INTO connection_history
                         (entry_id, connected_at, duration_s, outcome)
                     VALUES (?, ?, NULL, 'connected')",
                )
                .bind(&entry_id_str)
                .bind(ts.to_rfc3339())
                .execute(&self.pool)
                .await?;
                count += 1;
            }
        }
        Ok(count)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::Protocol;
    use tempfile::tempdir;

    async fn make_db() -> (tempfile::TempDir, SessionHistoryDb) {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("history.db");
        let db = SessionHistoryDb::open(&path).await.unwrap();
        (tmp, db)
    }

    #[tokio::test]
    async fn begin_and_end_session() {
        let (_tmp, db) = make_db().await;
        let entry_id = Uuid::new_v4();

        let session_id = db
            .begin_session(entry_id, "ssh", Some("host.example.com"))
            .await
            .unwrap();

        db.end_session(session_id, 512, 4096, Some("/tmp/session.log"))
            .await
            .unwrap();

        let logs = db.session_logs(entry_id).await.unwrap();
        assert_eq!(logs.len(), 1);
        let rec = &logs[0];
        assert_eq!(rec.id, session_id);
        assert_eq!(rec.entry_id, entry_id);
        assert_eq!(rec.protocol, "ssh");
        assert_eq!(rec.host.as_deref(), Some("host.example.com"));
        assert_eq!(rec.bytes_sent, 512);
        assert_eq!(rec.bytes_recv, 4096);
        assert_eq!(rec.log_path.as_deref(), Some("/tmp/session.log"));
        assert!(rec.ended_at.is_some());
    }

    #[tokio::test]
    async fn record_connection_and_query() {
        let (_tmp, db) = make_db().await;
        let entry_id = Uuid::new_v4();

        db.record_connection(entry_id, Some(42), OUTCOME_CONNECTED)
            .await
            .unwrap();
        db.record_connection(entry_id, None, OUTCOME_REFUSED)
            .await
            .unwrap();

        let history = db.connection_history(entry_id).await.unwrap();
        assert_eq!(history.len(), 2);
        // Newest first
        assert_eq!(history[0].outcome, OUTCOME_REFUSED);
        assert_eq!(history[1].outcome, OUTCOME_CONNECTED);
        assert_eq!(history[1].duration_s, Some(42));
    }

    #[tokio::test]
    async fn latest_successful_connection_ignores_failures() {
        let (_tmp, db) = make_db().await;
        let entry_id = Uuid::new_v4();

        db.record_connection(entry_id, None, OUTCOME_ERROR)
            .await
            .unwrap();
        assert!(db
            .latest_successful_connection(entry_id)
            .await
            .unwrap()
            .is_none());

        db.record_connection(entry_id, Some(5), OUTCOME_CONNECTED)
            .await
            .unwrap();
        assert!(db
            .latest_successful_connection(entry_id)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn session_logs_filtered_by_entry() {
        let (_tmp, db) = make_db().await;
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();

        db.begin_session(a, "ssh", Some("a.example.com"))
            .await
            .unwrap();
        db.begin_session(b, "telnet", Some("b.example.com"))
            .await
            .unwrap();

        let a_logs = db.session_logs(a).await.unwrap();
        let b_logs = db.session_logs(b).await.unwrap();
        assert_eq!(a_logs.len(), 1);
        assert_eq!(b_logs.len(), 1);
        assert_eq!(a_logs[0].protocol, "ssh");
        assert_eq!(b_logs[0].protocol, "telnet");
    }

    #[tokio::test]
    async fn import_from_directory_is_idempotent() {
        let (_tmp, db) = make_db().await;
        let mut entry = DirectoryEntry::new("Test BBS", Protocol::Telnet, "bbs.example.com");
        entry.last_connected = Some(Utc::now());

        let n1 = db.import_from_directory(&[entry.clone()]).await.unwrap();
        assert_eq!(n1, 1);

        // Second call must not insert a duplicate.
        let n2 = db.import_from_directory(&[entry]).await.unwrap();
        assert_eq!(n2, 0);
    }

    #[tokio::test]
    async fn import_skips_entries_without_last_connected() {
        let (_tmp, db) = make_db().await;
        let entry = DirectoryEntry::new("New BBS", Protocol::Ssh, "new.example.com");
        // last_connected is None by default
        let n = db.import_from_directory(&[entry]).await.unwrap();
        assert_eq!(n, 0);
    }
}

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::directory::DirectoryEntry;
use crate::logging::SessionLog;
use crate::terminal::TerminalEmulator;

use super::Connection;

// ── Events (MASTERPLAN §2.4) ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum SessionEvent {
    Connected(Uuid),
    Disconnected(Uuid, String),
    DataReceived(Uuid, Vec<u8>),
    TransferProgress(Uuid, TransferStats),
}

#[derive(Debug, Clone)]
pub struct TransferStats {
    pub bytes_sent: u64,
    pub bytes_total: u64,
    pub bytes_per_sec: f64,
}

// ── Session (MASTERPLAN §2.3) ─────────────────────────────────────────────────

pub struct Session {
    pub id: Uuid,
    pub entry: DirectoryEntry,
    pub connection: Box<dyn Connection>,
    pub terminal: TerminalEmulator,
    pub log: SessionLog,
    pub created_at: DateTime<Utc>,
}

impl Session {
    pub fn new(entry: DirectoryEntry, connection: Box<dyn Connection>, log: SessionLog) -> Self {
        let cols = entry.terminal.cols;
        let rows = entry.terminal.rows;
        Self {
            id: Uuid::new_v4(),
            entry,
            connection,
            terminal: TerminalEmulator::new(cols, rows),
            log,
            created_at: Utc::now(),
        }
    }
}

// ── SessionManager ────────────────────────────────────────────────────────────

/// Central registry for all open sessions.
///
/// Uses a Tokio broadcast channel as the event bus so any number of
/// subscribers (TUI, scripting engine, AI module) can observe session events.
pub struct SessionManager {
    sessions: HashMap<Uuid, Session>,
    event_tx: broadcast::Sender<SessionEvent>,
}

impl SessionManager {
    pub fn new() -> (Self, broadcast::Receiver<SessionEvent>) {
        let (tx, rx) = broadcast::channel(256);
        (
            Self {
                sessions: HashMap::new(),
                event_tx: tx,
            },
            rx,
        )
    }

    /// Subscribe to session events.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.event_tx.subscribe()
    }

    /// Register a new session.  Caller is responsible for calling
    /// `session.connection.connect()` before handing the session in.
    pub fn open_session(&mut self, session: Session) -> Uuid {
        let id = session.id;
        self.sessions.insert(id, session);
        let _ = self.event_tx.send(SessionEvent::Connected(id));
        id
    }

    /// Close a session and emit a disconnect event.
    pub async fn close_session(&mut self, id: Uuid, reason: impl Into<String>) {
        if let Some(mut session) = self.sessions.remove(&id) {
            let _ = session.connection.disconnect().await;
            let _ = self
                .event_tx
                .send(SessionEvent::Disconnected(id, reason.into()));
        }
    }

    #[must_use]
    pub fn get_session(&self, id: Uuid) -> Option<&Session> {
        self.sessions.get(&id)
    }

    pub fn get_session_mut(&mut self, id: Uuid) -> Option<&mut Session> {
        self.sessions.get_mut(&id)
    }

    #[must_use]
    pub fn list_sessions(&self) -> Vec<Uuid> {
        self.sessions.keys().copied().collect()
    }

    /// Broadcast a data-received event (called by the read loop in the TUI layer).
    pub fn notify_data(&self, session_id: Uuid, data: Vec<u8>) {
        let _ = self
            .event_tx
            .send(SessionEvent::DataReceived(session_id, data));
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new().0
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{ConnectionError, ConnectionStatus, Protocol};
    use async_trait::async_trait;

    // Minimal stub connection for tests
    struct StubConn {
        status: ConnectionStatus,
    }

    #[async_trait]
    impl Connection for StubConn {
        async fn connect(&mut self, _entry: &DirectoryEntry) -> Result<(), ConnectionError> {
            self.status = ConnectionStatus::Connected;
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), ConnectionError> {
            self.status = ConnectionStatus::Disconnected;
            Ok(())
        }
        async fn read(&mut self) -> Result<Vec<u8>, ConnectionError> {
            Ok(vec![])
        }
        async fn write(&mut self, _data: &[u8]) -> Result<(), ConnectionError> {
            Ok(())
        }
        fn protocol(&self) -> Protocol {
            Protocol::Raw
        }
        fn status(&self) -> ConnectionStatus {
            self.status.clone()
        }
        fn supports_file_transfer(&self) -> bool {
            false
        }
    }

    fn make_session() -> Session {
        use tempfile::tempdir;
        let entry = DirectoryEntry::new("test", Protocol::Raw, "localhost");
        let conn = Box::new(StubConn {
            status: ConnectionStatus::Connected,
        });
        let dir = tempdir().unwrap();
        let log_path = dir.path().join("test.log");
        let log =
            SessionLog::new("test", log_path, crate::logging::LogSettings::default()).unwrap();
        Session::new(entry, conn, log)
    }

    #[tokio::test]
    async fn open_and_list() {
        let (mut mgr, _rx) = SessionManager::new();
        let session = make_session();
        let id = mgr.open_session(session);
        assert!(mgr.list_sessions().contains(&id));
        assert!(mgr.get_session(id).is_some());
    }

    #[tokio::test]
    async fn close_removes_session() {
        let (mut mgr, _rx) = SessionManager::new();
        let session = make_session();
        let id = mgr.open_session(session);
        mgr.close_session(id, "done").await;
        assert!(mgr.get_session(id).is_none());
        assert!(!mgr.list_sessions().contains(&id));
    }

    #[tokio::test]
    async fn events_are_broadcast() {
        let (mut mgr, mut rx) = SessionManager::new();
        let session = make_session();
        let id = mgr.open_session(session);

        let event = rx.try_recv().unwrap();
        assert!(matches!(event, SessionEvent::Connected(eid) if eid == id));
    }
}

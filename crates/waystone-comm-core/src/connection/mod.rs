pub mod session_manager;

use std::fmt;

use async_trait::async_trait;
use thiserror::Error;

use crate::directory::DirectoryEntry;

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum ConnectionError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Authentication failed: {0}")]
    AuthFailed(String),

    #[error("Host key verification failed: {0}")]
    HostKeyMismatch(String),

    #[error("Connection refused by {host}:{port}")]
    Refused { host: String, port: u16 },

    #[error("Connection timed out after {seconds}s")]
    Timeout { seconds: u64 },

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Disconnected: {0}")]
    Disconnected(String),

    #[error("Unsupported operation: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, ConnectionError>;

impl From<russh::Error> for ConnectionError {
    fn from(value: russh::Error) -> Self {
        Self::Protocol(value.to_string())
    }
}

// ── Status ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    Error(String),
}

impl fmt::Display for ConnectionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disconnected => write!(f, "Disconnected"),
            Self::Connecting => write!(f, "Connecting…"),
            Self::Connected => write!(f, "Connected"),
            Self::Error(msg) => write!(f, "Error: {msg}"),
        }
    }
}

// ── Protocol enum (all protocols from MASTERPLAN §8.1) ───────────────────────

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    // Phase 1
    Ssh,
    Telnet,
    Serial,
    Raw,
    // Phase 2
    Sftp,
    Ftp,
    Ftps,
    Rlogin,
    // Phase 3
    Mosh,
    Gemini,
    Gopher,
    Irc,
    Nntp,
    Finger,
    Http,
    Https,
    WebSocket,
    Tftp,
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Ssh => "SSH",
            Self::Telnet => "Telnet",
            Self::Serial => "Serial",
            Self::Raw => "Raw TCP",
            Self::Sftp => "SFTP",
            Self::Ftp => "FTP",
            Self::Ftps => "FTPS",
            Self::Rlogin => "Rlogin",
            Self::Mosh => "Mosh",
            Self::Gemini => "Gemini",
            Self::Gopher => "Gopher",
            Self::Irc => "IRC",
            Self::Nntp => "NNTP",
            Self::Finger => "Finger",
            Self::Http => "HTTP",
            Self::Https => "HTTPS",
            Self::WebSocket => "WebSocket",
            Self::Tftp => "TFTP",
        };
        write!(f, "{s}")
    }
}

// ── Connection trait (MASTERPLAN §2.2) ───────────────────────────────────────

#[async_trait]
pub trait Connection: Send + Sync {
    /// Establish the connection using the given directory entry configuration.
    async fn connect(&mut self, entry: &DirectoryEntry) -> Result<()>;

    /// Gracefully close the connection.
    async fn disconnect(&mut self) -> Result<()>;

    /// Read the next chunk of data from the remote. Blocks until data is available.
    async fn read(&mut self) -> Result<Vec<u8>>;

    /// Write raw bytes to the remote.
    async fn write(&mut self, data: &[u8]) -> Result<()>;

    /// Notify the remote side that the terminal size changed.
    async fn resize(&mut self, _cols: u16, _rows: u16) -> Result<()> {
        Ok(())
    }

    /// Report which protocol this connection implements.
    fn protocol(&self) -> Protocol;

    /// Report the current connection status.
    fn status(&self) -> ConnectionStatus;

    /// Whether this connection supports in-band file transfer (e.g. Zmodem over SSH/Telnet).
    fn supports_file_transfer(&self) -> bool;
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_status_display() {
        assert_eq!(ConnectionStatus::Disconnected.to_string(), "Disconnected");
        assert_eq!(ConnectionStatus::Connecting.to_string(), "Connecting…");
        assert_eq!(ConnectionStatus::Connected.to_string(), "Connected");
        assert_eq!(
            ConnectionStatus::Error("boom".into()).to_string(),
            "Error: boom"
        );
    }

    #[test]
    fn connection_status_eq() {
        assert_eq!(ConnectionStatus::Connected, ConnectionStatus::Connected);
        assert_ne!(ConnectionStatus::Connected, ConnectionStatus::Disconnected);
        assert_eq!(
            ConnectionStatus::Error("x".into()),
            ConnectionStatus::Error("x".into())
        );
    }

    #[test]
    fn protocol_display() {
        assert_eq!(Protocol::Ssh.to_string(), "SSH");
        assert_eq!(Protocol::Telnet.to_string(), "Telnet");
        assert_eq!(Protocol::Serial.to_string(), "Serial");
        assert_eq!(Protocol::Raw.to_string(), "Raw TCP");
        assert_eq!(Protocol::Gemini.to_string(), "Gemini");
    }

    #[test]
    fn protocol_serde_roundtrip() {
        let p = Protocol::Ssh;
        let serialized = serde_json::to_string(&p).unwrap();
        let deserialized: Protocol = serde_json::from_str(&serialized).unwrap();
        assert_eq!(p, deserialized);
    }

    #[test]
    fn connection_error_display() {
        let err = ConnectionError::Refused {
            host: "192.168.1.1".into(),
            port: 22,
        };
        assert!(err.to_string().contains("192.168.1.1"));
        assert!(err.to_string().contains("22"));

        let err = ConnectionError::Timeout { seconds: 30 };
        assert!(err.to_string().contains("30"));
    }
}

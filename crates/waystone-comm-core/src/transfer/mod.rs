//! File transfer protocols: Xmodem, Ymodem, Zmodem.
//!
//! All protocol handlers run in `tokio::task::spawn_blocking` threads and
//! communicate via [`BlockingByteStream`], which wraps a pair of mpsc channels
//! connecting the blocking thread to the async session I/O task.

mod crc;
mod stream;
pub mod xmodem;
pub mod ymodem;
pub mod zmodem;

pub use stream::BlockingByteStream;
pub use xmodem::{XmodemMode, XmodemReceiver, XmodemSender};
pub use ymodem::{YmodemReceiver, YmodemSender};
pub use zmodem::{ZmodemReceiver, ZmodemSender, ZMODEM_AUTOSTART_SIGNATURE};

use thiserror::Error;

// ── Shared types ──────────────────────────────────────────────────────────────

/// Direction of a file transfer from Waystone Comm's perspective.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Send,
    Receive,
}

/// Current high-level phase of a file transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferPhase {
    Waiting,
    Metadata,
    Data,
    Finishing,
}

impl TransferPhase {
    pub fn label(self) -> &'static str {
        match self {
            TransferPhase::Waiting => "waiting",
            TransferPhase::Metadata => "metadata",
            TransferPhase::Data => "data",
            TransferPhase::Finishing => "finishing",
        }
    }
}

/// Live progress snapshot (updated by the blocking transfer thread).
#[derive(Debug, Clone)]
pub struct TransferProgress {
    pub direction: Direction,
    pub phase: TransferPhase,
    pub filename: String,
    pub bytes: u64,
    /// `None` if the file size is not known (e.g. classic Xmodem).
    pub total: Option<u64>,
    /// Characters per second.
    pub cps: u64,
}

impl TransferProgress {
    /// Percentage 0–100, or `None` if total is unknown.
    pub fn percent(&self) -> Option<u8> {
        self.total
            .filter(|&t| t > 0)
            .map(|t| ((self.bytes * 100) / t).min(100) as u8)
    }
}

/// Completed transfer statistics.
#[derive(Debug, Clone)]
pub struct TransferStats {
    pub bytes: u64,
    pub filename: String,
}

/// Transfer protocol error.
#[derive(Debug, Error)]
pub enum TransferError {
    #[error("transfer cancelled by remote")]
    Cancelled,
    #[error("too many errors: {0}")]
    TooManyErrors(String),
    #[error("timeout waiting for response")]
    Timeout,
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
}

// ── Zmodem auto-detect ────────────────────────────────────────────────────────

/// Return the byte offset of the Zmodem auto-start signature in `data`, or
/// `None` if not found.
pub fn find_zmodem_signature(data: &[u8]) -> Option<usize> {
    data.windows(ZMODEM_AUTOSTART_SIGNATURE.len())
        .position(|w| w == ZMODEM_AUTOSTART_SIGNATURE)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_sig_at_start() {
        let mut data = ZMODEM_AUTOSTART_SIGNATURE.to_vec();
        data.extend_from_slice(b"rest of frame");
        assert_eq!(find_zmodem_signature(&data), Some(0));
    }

    #[test]
    fn find_sig_mid_stream() {
        let mut data = b"normal terminal output\r\n".to_vec();
        data.extend_from_slice(ZMODEM_AUTOSTART_SIGNATURE);
        data.extend_from_slice(b"00000000\r\n");
        assert_eq!(find_zmodem_signature(&data), Some(24));
    }

    #[test]
    fn find_sig_absent() {
        assert_eq!(find_zmodem_signature(b"nothing here"), None);
    }

    #[test]
    fn find_sig_ignores_receiver_zrinit() {
        let data = b"**\x18B0100000023be50\r\n";
        assert_eq!(find_zmodem_signature(data), None);
    }

    #[test]
    fn transfer_progress_percent() {
        let p = TransferProgress {
            direction: Direction::Receive,
            phase: TransferPhase::Data,
            filename: "f".into(),
            bytes: 512,
            total: Some(1024),
            cps: 0,
        };
        assert_eq!(p.percent(), Some(50));

        let p2 = TransferProgress {
            total: None,
            ..p.clone()
        };
        assert_eq!(p2.percent(), None);

        let p3 = TransferProgress {
            bytes: 2000,
            total: Some(1024),
            ..p
        };
        assert_eq!(p3.percent(), Some(100)); // capped at 100
    }
}

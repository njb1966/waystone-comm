//! `BlockingByteStream` — synchronous byte stream for file transfer protocols.
//!
//! Wraps a `std::sync::mpsc` receiver (incoming data from the session I/O task)
//! and a `tokio::sync::mpsc` sender (for writing bytes back to the remote).
//! Designed to run inside `tokio::task::spawn_blocking`.

use std::{
    fs::OpenOptions,
    io::Write,
    sync::mpsc::{Receiver, RecvTimeoutError},
    time::{Duration, Instant},
};

use super::TransferError;

pub struct BlockingByteStream {
    rx: Receiver<Vec<u8>>,
    write_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    /// Buffered bytes not yet consumed.
    buf: Vec<u8>,
    pos: usize,
}

impl BlockingByteStream {
    pub fn new(rx: Receiver<Vec<u8>>, write_tx: tokio::sync::mpsc::Sender<Vec<u8>>) -> Self {
        Self {
            rx,
            write_tx,
            buf: Vec::new(),
            pos: 0,
        }
    }

    /// Pre-fill the buffer with bytes that were already received before the
    /// blocking task was spawned (e.g. Zmodem signature bytes captured by the
    /// async drain loop).
    pub fn prepend_bytes(&mut self, initial: Vec<u8>) {
        if self.pos < self.buf.len() {
            // Rare: existing buffered bytes — merge
            let mut combined = initial;
            combined.extend_from_slice(&self.buf[self.pos..]);
            self.buf = combined;
        } else {
            self.buf = initial;
        }
        self.pos = 0;
    }

    // ── Internal ─────────────────────────────────────────────────────────────

    /// Refill `buf` from the channel.  Returns `Err(Timeout)` if `deadline` passes.
    fn fill(&mut self, deadline: Instant) -> Result<(), TransferError> {
        self.buf.clear();
        self.pos = 0;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(TransferError::Timeout);
            }
            match self
                .rx
                .recv_timeout(remaining.min(Duration::from_millis(200)))
            {
                Ok(data) if data.is_empty() => continue,
                Ok(data) => {
                    self.buf = data;
                    return Ok(());
                }
                Err(RecvTimeoutError::Timeout) => {
                    // The 200ms internal cap fired — check if the overall deadline
                    // has actually passed before giving up.
                    if Instant::now() >= deadline {
                        return Err(TransferError::Timeout);
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(TransferError::Protocol("stream closed".into()))
                }
            }
        }
    }

    // ── Public API ────────────────────────────────────────────────────────────

    /// Read one byte, waiting up to `timeout`.
    pub fn read_byte_timeout(&mut self, timeout: Duration) -> Result<u8, TransferError> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.pos < self.buf.len() {
                let b = self.buf[self.pos];
                self.pos += 1;
                debug_transfer_bytes("IN ", &[b]);
                return Ok(b);
            }
            self.fill(deadline)?;
        }
    }

    /// Read one byte with the default 10-second timeout.
    pub fn read_byte(&mut self) -> Result<u8, TransferError> {
        self.read_byte_timeout(Duration::from_secs(10))
    }

    /// Fill `out` completely, waiting up to `timeout`.
    pub fn read_exact_timeout(
        &mut self,
        out: &mut [u8],
        timeout: Duration,
    ) -> Result<(), TransferError> {
        let deadline = Instant::now() + timeout;
        let mut written = 0;
        while written < out.len() {
            let available = self.buf.len() - self.pos;
            if available > 0 {
                let to_copy = available.min(out.len() - written);
                out[written..written + to_copy]
                    .copy_from_slice(&self.buf[self.pos..self.pos + to_copy]);
                debug_transfer_bytes("IN ", &self.buf[self.pos..self.pos + to_copy]);
                self.pos += to_copy;
                written += to_copy;
                continue;
            }
            self.fill(deadline)?;
        }
        Ok(())
    }

    /// Fill `out` with default 10-second timeout.
    pub fn read_exact(&mut self, out: &mut [u8]) -> Result<(), TransferError> {
        self.read_exact_timeout(out, Duration::from_secs(10))
    }

    /// Peek at the next byte without consuming it. Returns `None` if the
    /// buffer is empty and no data arrives within 50 ms.
    pub fn peek_byte(&mut self) -> Option<u8> {
        if self.pos < self.buf.len() {
            return Some(self.buf[self.pos]);
        }
        let deadline = Instant::now() + Duration::from_millis(50);
        self.fill(deadline).ok()?;
        self.buf.first().copied()
    }

    /// Write bytes to the remote via the async sender.
    /// Safe to call from a blocking thread.
    pub fn write_bytes(&self, data: &[u8]) -> Result<(), TransferError> {
        self.write_tx
            .blocking_send(data.to_vec())
            .map_err(|_| TransferError::Protocol("write channel closed".into()))?;
        debug_transfer_bytes("OUT", data);
        Ok(())
    }

    /// Discard all buffered + channel input for up to `timeout`.
    pub fn flush_input(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        self.buf.clear();
        self.pos = 0;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match self
                .rx
                .recv_timeout(remaining.min(Duration::from_millis(50)))
            {
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    }
}

fn debug_transfer_bytes(direction: &str, data: &[u8]) {
    let Ok(path) = std::env::var("WAYSTONE_COMM_TRANSFER_DEBUG") else {
        return;
    };
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };

    let mut line = String::with_capacity(direction.len() + 1 + data.len() * 3 + 1);
    line.push_str(direction);
    line.push(' ');
    for (idx, byte) in data.iter().enumerate() {
        if idx > 0 {
            line.push(' ');
        }
        use std::fmt::Write as _;
        let _ = write!(line, "{byte:02X}");
    }
    line.push('\n');
    let _ = file.write_all(line.as_bytes());
}

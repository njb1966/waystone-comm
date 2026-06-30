//! Ymodem file transfer protocol (batch).
//!
//! Ymodem extends Xmodem-1K with:
//! - A special block 0 carrying `filename\0size\0` in the data area
//! - Batch transfers (multiple files per session)
//! - A null block 0 to signal end-of-batch

use std::{
    fs::File,
    path::Path,
    sync::{Arc, Mutex},
};

use super::{
    crc::crc16,
    stream::BlockingByteStream,
    xmodem::{XmodemMode, XmodemReceiver, XmodemSender, ACK, CAN, EOT, NAK, SOH, STX},
    TransferError, TransferProgress, TransferStats,
};

// ── Receiver ──────────────────────────────────────────────────────────────────

pub struct YmodemReceiver;

impl Default for YmodemReceiver {
    fn default() -> Self {
        Self::new()
    }
}

impl YmodemReceiver {
    pub fn new() -> Self {
        Self
    }

    /// Receive one or more files into `dest_dir`.
    pub fn receive(
        &self,
        stream: &mut BlockingByteStream,
        dest_dir: &Path,
        progress: Arc<Mutex<Option<TransferProgress>>>,
    ) -> Result<Vec<TransferStats>, TransferError> {
        let inner = XmodemReceiver::new(XmodemMode::OneK);
        let mut results = Vec::new();

        loop {
            // Send 'C' to request first/next file header (block 0)
            stream.write_bytes(b"C")?;

            let header_byte = stream.read_byte_timeout(std::time::Duration::from_secs(60))?;

            // Null block 0 (all zeros) indicates end of batch
            if header_byte == EOT {
                stream.write_bytes(&[ACK])?;
                break;
            }

            if header_byte != SOH && header_byte != STX {
                return Err(TransferError::Protocol(format!(
                    "expected SOH/STX for header block, got 0x{header_byte:02X}"
                )));
            }

            let bsz: usize = if header_byte == STX { 1024 } else { 128 };

            let blk_num = stream.read_byte()?;
            let blk_inv = stream.read_byte()?;

            if blk_num.wrapping_add(blk_inv) != 0xFF {
                return Err(TransferError::Protocol("bad block-0 complement".into()));
            }

            let mut hdr_data = vec![0u8; bsz];
            stream.read_exact(&mut hdr_data)?;

            // Validate CRC-16
            let hi = stream.read_byte()?;
            let lo = stream.read_byte()?;
            let recv_crc = u16::from_be_bytes([hi, lo]);
            if crc16(&hdr_data) != recv_crc {
                stream.write_bytes(&[NAK])?;
                continue;
            }

            // Block 0 all-null → end of batch
            if hdr_data.iter().all(|&b| b == 0) {
                stream.write_bytes(&[ACK])?;
                break;
            }

            // Parse filename and optional size from block 0
            let nul = hdr_data.iter().position(|&b| b == 0).unwrap_or(bsz);
            let filename = std::str::from_utf8(&hdr_data[..nul])
                .unwrap_or("unknown")
                .to_string();

            let known_size: Option<u64> = hdr_data
                .get(nul + 1..)
                .and_then(|s| {
                    let end = s
                        .iter()
                        .position(|&b| b == 0 || b == b' ')
                        .unwrap_or(s.len());
                    std::str::from_utf8(&s[..end]).ok()
                })
                .and_then(|s| s.parse().ok());

            // ACK header block, then send 'C' to start data
            stream.write_bytes(&[ACK])?;
            stream.write_bytes(b"C")?;

            let dest_path = dest_dir.join(sanitise_filename(&filename));
            let mut file = File::create(&dest_path)?;

            // Receive file data using Xmodem-1K; init byte was already sent above.
            let stats = inner.receive_data(
                stream,
                &mut file,
                filename,
                known_size,
                Arc::clone(&progress),
            )?;

            // Truncate to declared file size if we know it
            if let Some(sz) = known_size {
                file.set_len(sz)?;
            }

            results.push(stats);
        }

        Ok(results)
    }
}

// ── Sender ────────────────────────────────────────────────────────────────────

pub struct YmodemSender;

impl Default for YmodemSender {
    fn default() -> Self {
        Self::new()
    }
}

impl YmodemSender {
    pub fn new() -> Self {
        Self
    }

    /// Send one or more files.
    pub fn send(
        &self,
        stream: &mut BlockingByteStream,
        files: &[&Path],
        progress: Arc<Mutex<Option<TransferProgress>>>,
    ) -> Result<Vec<TransferStats>, TransferError> {
        let inner = XmodemSender::new(XmodemMode::OneK);
        let mut results = Vec::new();

        for &file_path in files {
            let filename = file_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();

            let file_size = file_path.metadata().map(|m| m.len()).unwrap_or(0);

            // Wait for receiver 'C' to send header block 0
            let init = stream.read_byte_timeout(std::time::Duration::from_secs(60))?;
            if init != b'C' {
                return Err(TransferError::Protocol(format!(
                    "expected 'C' for header, got 0x{init:02X}"
                )));
            }

            // Build block 0: filename\0size\0 padded with zeros
            let header_str = format!("{filename}\0{file_size}\0");
            let mut blk0 = vec![0u8; 1024];
            let hlen = header_str.len().min(1024);
            blk0[..hlen].copy_from_slice(&header_str.as_bytes()[..hlen]);

            let crc = crc16(&blk0);
            let mut frame = Vec::with_capacity(3 + 1024 + 2);
            frame.push(STX); // 1024-byte block
            frame.push(0u8); // block 0
            frame.push(0xFF); // ~block 0
            frame.extend_from_slice(&blk0);
            frame.extend_from_slice(&crc.to_be_bytes());
            stream.write_bytes(&frame)?;

            // Wait for ACK + 'C' before starting data transfer
            let ack = stream.read_byte_timeout(std::time::Duration::from_secs(10))?;
            if ack == CAN {
                return Err(TransferError::Cancelled);
            }
            if ack != ACK {
                return Err(TransferError::Protocol(format!(
                    "expected ACK after header, got 0x{ack:02X}"
                )));
            }

            let c = stream.read_byte_timeout(std::time::Duration::from_secs(10))?;
            if c != b'C' {
                return Err(TransferError::Protocol(format!(
                    "expected 'C' after header ACK, got 0x{c:02X}"
                )));
            }

            // Transfer data — init handshake was already handled above.
            let mut file = File::open(file_path)?;
            let stats = inner.send_blocks(
                stream,
                &mut file,
                Some(file_size),
                filename,
                Arc::clone(&progress),
            )?;

            results.push(stats);
        }

        // Send null block 0 to end batch
        let init = stream.read_byte_timeout(std::time::Duration::from_secs(60))?;
        if init == b'C' {
            let null_blk = vec![0u8; 1024];
            let crc = crc16(&null_blk);
            let mut frame = Vec::with_capacity(3 + 1024 + 2);
            frame.push(STX);
            frame.push(0u8);
            frame.push(0xFF);
            frame.extend_from_slice(&null_blk);
            frame.extend_from_slice(&crc.to_be_bytes());
            stream.write_bytes(&frame)?;
            stream
                .read_byte_timeout(std::time::Duration::from_secs(5))
                .ok();
        }

        Ok(results)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn sanitise_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitise_filename_basic() {
        assert_eq!(sanitise_filename("file.txt"), "file.txt");
        assert_eq!(sanitise_filename("my file.txt"), "my_file.txt");
        // '/' is sanitised; '.' is kept (valid in filenames, not traversable as component)
        assert_eq!(sanitise_filename("../../etc/passwd"), ".._.._etc_passwd");
    }

    #[test]
    fn block0_header_encoding() {
        let filename = "test.txt";
        let file_size = 1234u64;
        let header_str = format!("{filename}\0{file_size}\0");
        let mut blk0 = vec![0u8; 1024];
        blk0[..header_str.len()].copy_from_slice(header_str.as_bytes());

        // Verify filename extracted
        let nul = blk0.iter().position(|&b| b == 0).unwrap();
        assert_eq!(&blk0[..nul], b"test.txt");

        // Verify size extracted
        let size_str_start = nul + 1;
        let size_end = blk0[size_str_start..]
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(0);
        let size: u64 = std::str::from_utf8(&blk0[size_str_start..size_str_start + size_end])
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(size, 1234);
    }
}

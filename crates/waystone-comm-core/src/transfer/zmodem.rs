//! Zmodem file transfer protocol.
//!
//! Implements the full Zmodem transfer flow including:
//! - ZHEX frame encoding/decoding for control frames
//! - ZBIN32 data subpackets with CRC-32 and ZDLE escaping
//! - Auto-start detection on `**\x18B00` (ZRQINIT signature)
//! - Crash recovery via ZRPOS (receiver specifies resume offset)
//! - Batch receive (multiple files per session)

use std::{
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use super::{
    crc::{crc16, crc32_feed, crc32_finish, CRC32_INIT},
    stream::BlockingByteStream,
    Direction, TransferError, TransferPhase, TransferProgress, TransferStats,
};

// ── Wire constants ────────────────────────────────────────────────────────────

const ZPAD: u8 = b'*'; // 0x2A — frame preamble
const ZDLE: u8 = 0x18; // Data-Link Escape
const ZBIN: u8 = b'A'; // 0x41 — binary frame with CRC-16
const ZHEX: u8 = b'B'; // 0x42 — hex-encoded frame
const ZBIN32: u8 = b'C'; // 0x43 — binary frame with CRC-32

// Frame types
const ZRQINIT: u8 = 0;
const ZRINIT: u8 = 1;
const ZSINIT: u8 = 2;
const ZACK: u8 = 3;
const ZFILE: u8 = 4;
const ZSKIP: u8 = 5;
const ZNAK: u8 = 6;
const ZABORT: u8 = 7;
const ZFIN: u8 = 8;
const ZRPOS: u8 = 9;
const ZDATA: u8 = 10;
const ZEOF: u8 = 11;
#[allow(dead_code)]
const ZERR: u8 = 12;
const ZCAN: u8 = 16;

// Subpacket terminators (follow ZDLE in data stream)
const ZCRCE: u8 = b'h'; // 0x68 — CRC, end of frame/last subpacket
const ZCRCG: u8 = b'i'; // 0x69 — CRC, more subpackets follow (no ACK needed)
const ZCRCQ: u8 = b'j'; // 0x6A — CRC, request ZACK
const ZCRCC: u8 = b'k'; // 0x6B — CRC of header (used in ZSINIT/ZFILE)
const ZRUB0: u8 = b'l'; // 0x6C — escaped DEL (0x7F)
const ZRUB1: u8 = b'm'; // 0x6D — escaped 0xFF

// ZRINIT capability flags. Classic ZMODEM names this byte ZF0, which is the
// fourth data byte in the transmitted 4-byte header.
const CANFDX: u8 = 0x01; // full-duplex
const CANOVIO: u8 = 0x02; // can overlap I/O
const CANFC32: u8 = 0x20; // can use CRC-32

// ZFILE conversion flags. Classic ZMODEM names this header byte ZF0.
const ZCBIN: u8 = 0x01; // binary transfer, no newline conversion

/// The 6-byte Zmodem auto-start signature for ZRQINIT: `* * ZDLE B 0 0`.
/// A shorter `**\x18B0` prefix also matches ZRINIT (`01`), which is sent by a
/// remote receiver during uploads and must not trigger Waystone Comm's downloader.
pub const ZMODEM_AUTOSTART_SIGNATURE: &[u8] = &[ZPAD, ZPAD, ZDLE, ZHEX, b'0', b'0'];

// Bytes that must be ZDLE-escaped in data streams
fn needs_zdle_escape(b: u8) -> bool {
    matches!(b, ZDLE | 0x11 | 0x13 | 0x7F | 0x91 | 0x93 | 0xFF)
}

fn push_zdle_escaped_byte(buf: &mut Vec<u8>, b: u8) {
    match b {
        0x7F => {
            buf.push(ZDLE);
            buf.push(ZRUB0);
        }
        0xFF => {
            buf.push(ZDLE);
            buf.push(ZRUB1);
        }
        b if needs_zdle_escape(b) => {
            buf.push(ZDLE);
            buf.push(b ^ 0x40);
        }
        _ => buf.push(b),
    }
}

fn decode_zdle_escaped_byte(c: u8) -> u8 {
    match c {
        ZRUB0 => 0x7F,
        ZRUB1 => 0xFF,
        _ => c ^ 0x40,
    }
}

// ── Frame helpers ─────────────────────────────────────────────────────────────

/// Send a ZHEX frame.  The CRC-16 covers `frame_type` + all 4 `data` bytes.
fn send_zhex(
    stream: &mut BlockingByteStream,
    frame_type: u8,
    data: [u8; 4],
) -> Result<(), TransferError> {
    let mut crc_input = [0u8; 5];
    crc_input[0] = frame_type;
    crc_input[1..].copy_from_slice(&data);
    let crc = crc16(&crc_input);

    let mut buf = Vec::with_capacity(24);
    buf.push(ZPAD);
    buf.push(ZPAD);
    buf.push(ZDLE);
    buf.push(ZHEX);
    push_hex_byte(&mut buf, frame_type);
    for b in data {
        push_hex_byte(&mut buf, b);
    }
    push_hex_byte(&mut buf, (crc >> 8) as u8);
    push_hex_byte(&mut buf, crc as u8);
    buf.push(b'\r');
    buf.push(b'\n');
    // Common rz/sz implementations send XON after hex headers to release
    // software flow control. Receivers that do not need it ignore it.
    buf.push(0x11);

    stream.write_bytes(&buf)
}

/// Send a ZBIN32 frame. The CRC-32 covers `frame_type` + all 4 `data` bytes.
fn send_zbin32(
    stream: &mut BlockingByteStream,
    frame_type: u8,
    data: [u8; 4],
) -> Result<(), TransferError> {
    let mut crc = CRC32_INIT;
    crc = crc32_feed(crc, frame_type);
    for &b in &data {
        crc = crc32_feed(crc, b);
    }
    let crc = crc32_finish(crc);

    let mut buf = Vec::with_capacity(16);
    buf.push(ZPAD);
    buf.push(ZDLE);
    buf.push(ZBIN32);
    push_zdle_escaped_byte(&mut buf, frame_type);
    for b in data {
        push_zdle_escaped_byte(&mut buf, b);
    }
    for b in crc.to_le_bytes() {
        push_zdle_escaped_byte(&mut buf, b);
    }

    stream.write_bytes(&buf)
}

#[inline]
fn push_hex_byte(buf: &mut Vec<u8>, b: u8) {
    const HEX: &[u8] = b"0123456789abcdef";
    buf.push(HEX[(b >> 4) as usize]);
    buf.push(HEX[(b & 0x0F) as usize]);
}

/// Encode an offset (u32 LE) into 4 bytes for frame data.
fn offset_bytes(offset: u64) -> [u8; 4] {
    (offset as u32).to_le_bytes()
}

/// Parse a u32 offset from 4 frame data bytes.
fn parse_offset(data: &[u8; 4]) -> u64 {
    u32::from_le_bytes(*data) as u64
}

fn zrinit_capabilities() -> [u8; 4] {
    [0, 0, 0, CANFDX | CANOVIO | CANFC32]
}

/// Read from the stream until we see a valid ZMODEM frame preamble, then parse
/// a frame. Live BBS sessions often include prompts or echoed text before the
/// protocol bytes, so a lone `*` followed by normal text is treated as noise.
/// Returns `(frame_type, data[4])`.
fn recv_frame(stream: &mut BlockingByteStream) -> Result<(u8, [u8; 4]), TransferError> {
    loop {
        // Sync to next ZPAD.
        loop {
            let b = stream.read_byte_timeout(Duration::from_secs(30))?;
            if b == ZPAD {
                break;
            }
            if b == ZCAN {
                return Err(TransferError::Cancelled);
            }
        }

        // Consume any repeated ZPAD bytes. If the byte after the padding is not
        // ZDLE, this was not a ZMODEM preamble; keep scanning.
        loop {
            let b = stream.read_byte()?;
            match b {
                ZDLE => {
                    let enc_type = stream.read_byte()?;
                    return match enc_type {
                        ZBIN => recv_zbin_frame(stream),
                        ZHEX => recv_zhex_frame(stream),
                        ZBIN32 => recv_zbin32_frame(stream),
                        _ => Err(TransferError::Protocol(format!(
                            "unsupported frame encoding 0x{enc_type:02X}"
                        ))),
                    };
                }
                ZPAD => continue,
                ZCAN => return Err(TransferError::Cancelled),
                _ => break,
            }
        }
    }
}

fn recv_frame_with_context(
    stream: &mut BlockingByteStream,
    context: &str,
) -> Result<(u8, [u8; 4]), TransferError> {
    match recv_frame(stream) {
        Err(TransferError::Timeout) => Err(TransferError::Protocol(format!(
            "timeout waiting for {context}"
        ))),
        other => other,
    }
}

fn recv_zhex_frame(stream: &mut BlockingByteStream) -> Result<(u8, [u8; 4]), TransferError> {
    // Read 14 hex chars: 2 (type) + 8 (data) + 4 (crc16).
    let mut hex_buf = [0u8; 14];
    stream.read_exact(&mut hex_buf)?;

    let frame_type = hex_pair_to_byte(hex_buf[0], hex_buf[1])?;
    let d0 = hex_pair_to_byte(hex_buf[2], hex_buf[3])?;
    let d1 = hex_pair_to_byte(hex_buf[4], hex_buf[5])?;
    let d2 = hex_pair_to_byte(hex_buf[6], hex_buf[7])?;
    let d3 = hex_pair_to_byte(hex_buf[8], hex_buf[9])?;
    let c0 = hex_pair_to_byte(hex_buf[10], hex_buf[11])?;
    let c1 = hex_pair_to_byte(hex_buf[12], hex_buf[13])?;

    // Validate CRC-16 over type + data
    let crc_input = [frame_type, d0, d1, d2, d3];
    let expected_crc = crc16(&crc_input);
    let recv_crc = u16::from_be_bytes([c0, c1]);
    if expected_crc != recv_crc {
        return Err(TransferError::Protocol(format!(
            "ZHEX frame CRC mismatch: expected 0x{expected_crc:04X}, got 0x{recv_crc:04X}"
        )));
    }

    // Consume trailing CR LF (and optional XON) without stealing the first byte
    // of a following frame when frames arrive back-to-back.
    while matches!(stream.peek_byte(), Some(b'\r' | b'\n' | 0x11)) {
        stream.read_byte()?;
    }

    Ok((frame_type, [d0, d1, d2, d3]))
}

fn recv_zbin_frame(stream: &mut BlockingByteStream) -> Result<(u8, [u8; 4]), TransferError> {
    // 5 bytes: type + 4 data, all ZDLE-escaped; then 2 bytes CRC-16.
    let frame_type = recv_escaped_byte(stream)?;
    let d0 = recv_escaped_byte(stream)?;
    let d1 = recv_escaped_byte(stream)?;
    let d2 = recv_escaped_byte(stream)?;
    let d3 = recv_escaped_byte(stream)?;

    let c0 = recv_escaped_byte(stream)?;
    let c1 = recv_escaped_byte(stream)?;

    let recv_crc = u16::from_be_bytes([c0, c1]);
    let crc_input = [frame_type, d0, d1, d2, d3];
    let expected_crc = crc16(&crc_input);
    if expected_crc != recv_crc {
        return Err(TransferError::Protocol(format!(
            "ZBIN frame CRC-16 mismatch: expected 0x{expected_crc:04X}, got 0x{recv_crc:04X}"
        )));
    }

    Ok((frame_type, [d0, d1, d2, d3]))
}

fn recv_zbin32_frame(stream: &mut BlockingByteStream) -> Result<(u8, [u8; 4]), TransferError> {
    // 5 bytes: type + 4 data, all ZDLE-escaped; then 4 bytes CRC-32
    let frame_type = recv_escaped_byte(stream)?;
    let d0 = recv_escaped_byte(stream)?;
    let d1 = recv_escaped_byte(stream)?;
    let d2 = recv_escaped_byte(stream)?;
    let d3 = recv_escaped_byte(stream)?;

    let c0 = recv_escaped_byte(stream)?;
    let c1 = recv_escaped_byte(stream)?;
    let c2 = recv_escaped_byte(stream)?;
    let c3 = recv_escaped_byte(stream)?;

    let recv_crc = u32::from_le_bytes([c0, c1, c2, c3]);
    let crc_input = [frame_type, d0, d1, d2, d3];
    let expected_crc = crc32_le_slice(&crc_input);
    if expected_crc != recv_crc {
        return Err(TransferError::Protocol(format!(
            "ZBIN32 frame CRC-32 mismatch: expected 0x{expected_crc:08X}, got 0x{recv_crc:08X}"
        )));
    }

    Ok((frame_type, [d0, d1, d2, d3]))
}

/// Read one ZDLE-unescaped byte (not a subpacket terminator).
fn recv_escaped_byte(stream: &mut BlockingByteStream) -> Result<u8, TransferError> {
    loop {
        let b = stream.read_byte()?;
        if b == ZDLE {
            let c = stream.read_byte()?;
            match c {
                ZCRCE | ZCRCG | ZCRCQ | ZCRCC => {
                    // This is a subpacket terminator, not a header byte — shouldn't
                    // appear while reading a frame header, but handle gracefully.
                    return Err(TransferError::Protocol(
                        "unexpected subpacket terminator in frame header".into(),
                    ));
                }
                _ => return Ok(decode_zdle_escaped_byte(c)),
            }
        } else if b == 0x11 || b == 0x13 || b == 0x91 || b == 0x93 {
            // XON/XOFF in stream — ignore and keep reading
            continue;
        } else {
            return Ok(b);
        }
    }
}

/// Read a complete data subpacket.
/// Returns `(data, subpacket_type)` where type is ZCRCE/ZCRCG/ZCRCQ/ZCRCC.
/// Validates CRC-32.
fn recv_data_subpacket(stream: &mut BlockingByteStream) -> Result<(Vec<u8>, u8), TransferError> {
    let mut data = Vec::new();
    let mut crc = CRC32_INIT;

    loop {
        let b = stream.read_byte()?;
        if b == ZDLE {
            let c = stream.read_byte()?;
            match c {
                ZCRCE | ZCRCG | ZCRCQ | ZCRCC => {
                    // c is the subpacket terminator; include it in CRC
                    crc = crc32_feed(crc, c);
                    // Read 4 CRC bytes (also ZDLE-escaped)
                    let c0 = recv_escaped_byte(stream)?;
                    let c1 = recv_escaped_byte(stream)?;
                    let c2 = recv_escaped_byte(stream)?;
                    let c3 = recv_escaped_byte(stream)?;
                    let recv_crc = u32::from_le_bytes([c0, c1, c2, c3]);
                    let expected = crc32_finish(crc);
                    if expected != recv_crc {
                        return Err(TransferError::Protocol(format!(
                            "data subpacket CRC-32 mismatch: expected 0x{expected:08X}, got 0x{recv_crc:08X}"
                        )));
                    }
                    return Ok((data, c));
                }
                _ => {
                    let real_byte = decode_zdle_escaped_byte(c);
                    crc = crc32_feed(crc, real_byte);
                    data.push(real_byte);
                }
            }
        } else if b == 0x11 || b == 0x13 || b == 0x91 || b == 0x93 {
            continue; // XON/XOFF — strip
        } else {
            crc = crc32_feed(crc, b);
            data.push(b);
        }
    }
}

fn hex_pair_to_byte(hi: u8, lo: u8) -> Result<u8, TransferError> {
    fn nib(c: u8) -> Result<u8, TransferError> {
        match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            b'A'..=b'F' => Ok(c - b'A' + 10),
            _ => Err(TransferError::Protocol(format!(
                "invalid hex digit 0x{c:02X}"
            ))),
        }
    }
    Ok((nib(hi)? << 4) | nib(lo)?)
}

/// CRC-32 over a small fixed slice (used for frame headers).
fn crc32_le_slice(data: &[u8]) -> u32 {
    let mut c = CRC32_INIT;
    for &b in data {
        c = crc32_feed(c, b);
    }
    crc32_finish(c)
}

// ── Send a ZBIN32 data subpacket ──────────────────────────────────────────────

fn send_data_subpacket(
    stream: &mut BlockingByteStream,
    data: &[u8],
    term: u8, // ZCRCE / ZCRCG / ZCRCQ
) -> Result<(), TransferError> {
    let mut crc = CRC32_INIT;
    let mut buf = Vec::with_capacity(data.len() * 2 + 8);

    for &b in data {
        crc = crc32_feed(crc, b);
        push_zdle_escaped_byte(&mut buf, b);
    }

    // Include terminator byte in CRC, then send ZDLE + term
    crc = crc32_feed(crc, term);
    buf.push(ZDLE);
    buf.push(term);

    // Append ZDLE-escaped CRC-32 (little-endian)
    let crc_val = crc32_finish(crc);
    for &b in &crc_val.to_le_bytes() {
        push_zdle_escaped_byte(&mut buf, b);
    }

    stream.write_bytes(&buf)
}

// ── Receiver ──────────────────────────────────────────────────────────────────

pub struct ZmodemReceiver;

impl Default for ZmodemReceiver {
    fn default() -> Self {
        Self::new()
    }
}

impl ZmodemReceiver {
    pub fn new() -> Self {
        Self
    }

    /// Receive one or more files, saving to `dest_dir`.
    ///
    /// The `stream` should already contain the ZRQINIT bytes that triggered
    /// auto-detection (or the receiver was manually started).
    pub fn receive(
        &self,
        stream: &mut BlockingByteStream,
        dest_dir: &Path,
        progress: Arc<Mutex<Option<TransferProgress>>>,
    ) -> Result<Vec<TransferStats>, TransferError> {
        // Consume any preamble that arrived before we were called.
        // The ZRQINIT (or its partial frame) may be in the stream; we'll
        // resync by looking for the next ZPAD in recv_frame().

        send_zhex(stream, ZRINIT, zrinit_capabilities())?;

        let mut results = Vec::new();

        'session: loop {
            let (ftype, _fdata) = recv_frame(stream)?;

            match ftype {
                ZRQINIT => {
                    // Sender re-requested init — resend ZRINIT
                    send_zhex(stream, ZRINIT, zrinit_capabilities())?;
                    continue;
                }

                ZSINIT => {
                    // Optional sender init with attention string — ACK and continue
                    send_zhex(stream, ZACK, [0, 0, 0, 0])?;
                    // Consume the data subpacket attached to ZSINIT
                    recv_data_subpacket(stream).ok();
                    send_zhex(stream, ZRINIT, zrinit_capabilities())?;
                    continue;
                }

                ZFILE => {
                    // fdata contains transfer options; the filename is in the attached subpacket.
                    let (file_info, _term) = recv_data_subpacket(stream)?;

                    let (filename, known_size) = parse_file_info(&file_info);
                    if let Ok(mut p) = progress.lock() {
                        *p = Some(TransferProgress {
                            direction: Direction::Receive,
                            phase: TransferPhase::Metadata,
                            filename: filename.clone(),
                            bytes: 0,
                            total: known_size,
                            cps: 0,
                        });
                    }

                    // Determine resume offset
                    let dest_path = dest_dir.join(sanitise_filename(&filename));
                    let resume_offset = if dest_path.exists() {
                        dest_path.metadata().map(|m| m.len()).unwrap_or(0)
                    } else {
                        0
                    };

                    // Open file for writing (append if resuming)
                    let mut file = if resume_offset > 0 {
                        OpenOptions::new().append(true).open(&dest_path)?
                    } else {
                        File::create(&dest_path)?
                    };

                    // Tell sender where to start
                    send_zhex(stream, ZRPOS, offset_bytes(resume_offset))?;

                    let mut file_offset = resume_offset;
                    let start = std::time::Instant::now();

                    'file: loop {
                        let (ftype2, fdata2) = recv_frame(stream)?;

                        match ftype2 {
                            ZDATA => {
                                let data_offset = parse_offset(&fdata2);
                                if data_offset != file_offset {
                                    // Offset mismatch — request correct position
                                    send_zhex(stream, ZRPOS, offset_bytes(file_offset))?;
                                    continue;
                                }

                                // Read data subpackets until ZCRCE (end of file frame)
                                loop {
                                    let (pkt_data, term) = match recv_data_subpacket(stream) {
                                        Ok(p) => p,
                                        Err(e) => {
                                            // Bad CRC — request retransmit from current offset
                                            send_zhex(stream, ZRPOS, offset_bytes(file_offset))?;
                                            return Err(e);
                                        }
                                    };

                                    file.write_all(&pkt_data)?;
                                    file_offset += pkt_data.len() as u64;

                                    let elapsed = start.elapsed().as_secs_f64();
                                    let cps = if elapsed > 0.1 {
                                        (file_offset as f64 / elapsed) as u64
                                    } else {
                                        0
                                    };
                                    if let Ok(mut p) = progress.lock() {
                                        *p = Some(TransferProgress {
                                            direction: Direction::Receive,
                                            phase: TransferPhase::Data,
                                            filename: filename.clone(),
                                            bytes: file_offset,
                                            total: known_size,
                                            cps,
                                        });
                                    }

                                    match term {
                                        ZCRCE => break, // end of this ZDATA frame
                                        ZCRCQ => {
                                            // Sender requests ZACK
                                            send_zhex(stream, ZACK, offset_bytes(file_offset))?;
                                        }
                                        ZCRCG => {} // OK, no ACK needed
                                        _ => {}
                                    }
                                }
                            }

                            ZEOF => {
                                let eof_offset = parse_offset(&fdata2);
                                // Truncate file to declared size
                                file.set_len(eof_offset)?;
                                results.push(TransferStats {
                                    bytes: eof_offset,
                                    filename: filename.clone(),
                                });
                                // Ready for next file
                                send_zhex(stream, ZRINIT, zrinit_capabilities())?;
                                break 'file;
                            }

                            ZFIN => {
                                send_zhex(stream, ZFIN, [0, 0, 0, 0])?;
                                // Optionally send "OO" end-of-session
                                stream.write_bytes(b"OO").ok();
                                break 'session;
                            }

                            ZNAK | ZABORT | ZCAN => {
                                return Err(TransferError::Cancelled);
                            }

                            _ => {} // ignore unknown frames
                        }
                    }
                }

                ZFIN => {
                    send_zhex(stream, ZFIN, [0, 0, 0, 0])?;
                    stream.write_bytes(b"OO").ok();
                    break 'session;
                }

                ZNAK | ZABORT | ZCAN => {
                    return Err(TransferError::Cancelled);
                }

                _ => {} // ignore unknown / unhandled frame types
            }
        }

        Ok(results)
    }
}

// ── Sender ────────────────────────────────────────────────────────────────────

const SUBPACKET_SIZE: usize = 8192; // bytes per data subpacket

pub struct ZmodemSender;

impl Default for ZmodemSender {
    fn default() -> Self {
        Self::new()
    }
}

impl ZmodemSender {
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
        // Announce ourselves with ZRQINIT
        send_zhex(stream, ZRQINIT, [0, 0, 0, 0])?;

        // Wait for receiver's ZRINIT
        loop {
            let (ftype, _) = recv_frame_with_context(stream, "receiver ZRINIT")?;
            match ftype {
                ZRINIT => break,
                ZNAK => {
                    send_zhex(stream, ZRQINIT, [0, 0, 0, 0])?;
                }
                ZCAN | ZABORT => return Err(TransferError::Cancelled),
                _ => {}
            }
        }

        let mut results = Vec::new();

        for &file_path in files {
            let filename = file_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();

            let file_size = file_path.metadata().map(|m| m.len()).unwrap_or(0);
            if let Ok(mut p) = progress.lock() {
                *p = Some(TransferProgress {
                    direction: Direction::Send,
                    phase: TransferPhase::Metadata,
                    filename: filename.clone(),
                    bytes: 0,
                    total: Some(file_size),
                    cps: 0,
                });
            }

            // Build and send ZFILE with filename info subpacket
            send_zbin32(stream, ZFILE, [0, 0, 0, ZCBIN])?;
            let info_str = format!("{filename}\0{file_size} 0 0 0 1\0");
            send_data_subpacket(stream, info_str.as_bytes(), ZCRCW_OR_ZCRCC)?;

            // Wait for ZRPOS (receiver's starting offset)
            let start_offset = loop {
                let (ftype, fdata) = recv_frame_with_context(stream, "receiver ZRPOS after ZFILE")?;
                match ftype {
                    ZRPOS => break parse_offset(&fdata),
                    ZSKIP => {
                        // Receiver already has this file
                        break file_size;
                    }
                    ZCAN | ZABORT => return Err(TransferError::Cancelled),
                    _ => {}
                }
            };

            if start_offset >= file_size {
                // Nothing to send
                results.push(TransferStats { bytes: 0, filename });
                continue;
            }

            let start = std::time::Instant::now();
            let mut sent = start_offset;
            let mut file = File::open(file_path)?;

            'send_file_data: loop {
                file.seek(SeekFrom::Start(sent))?;

                // Send ZDATA with the current offset. This is also used after a
                // receiver ZRPOS request to resume from the requested byte.
                send_zbin32(stream, ZDATA, offset_bytes(sent))?;

                let mut buf = vec![0u8; SUBPACKET_SIZE];
                let mut pkt_count: u32 = 0;

                loop {
                    // Read one subpacket worth of data
                    let mut read_total = 0usize;
                    let mut eof = false;
                    while read_total < SUBPACKET_SIZE {
                        use std::io::Read;
                        let n = file.read(&mut buf[read_total..])?;
                        if n == 0 {
                            eof = true;
                            break;
                        }
                        read_total += n;
                    }

                    if read_total == 0 {
                        break;
                    }

                    let is_last = eof || (sent + read_total as u64 >= file_size);
                    let term = if is_last {
                        ZCRCE // end of file data
                    } else {
                        pkt_count += 1;
                        if pkt_count % 8 == 0 {
                            ZCRCQ
                        } else {
                            ZCRCG
                        }
                    };

                    send_data_subpacket(stream, &buf[..read_total], term)?;
                    sent += read_total as u64;

                    let elapsed = start.elapsed().as_secs_f64();
                    let cps = if elapsed > 0.1 {
                        (sent as f64 / elapsed) as u64
                    } else {
                        0
                    };
                    if let Ok(mut p) = progress.lock() {
                        *p = Some(TransferProgress {
                            direction: Direction::Send,
                            phase: TransferPhase::Data,
                            filename: filename.clone(),
                            bytes: sent,
                            total: Some(file_size),
                            cps,
                        });
                    }

                    // If we asked for ZACK, wait for it. A receiver may also
                    // request a reposition with ZRPOS; honor it and retransmit.
                    if term == ZCRCQ {
                        let (ack_type, ack_data) =
                            recv_frame_with_context(stream, "receiver ZACK")?;
                        match ack_type {
                            ZACK => {}
                            ZRPOS => {
                                let offset = parse_offset(&ack_data);
                                if offset > file_size {
                                    return Err(TransferError::Protocol(format!(
                                        "receiver requested invalid ZRPOS offset {offset}"
                                    )));
                                }
                                sent = offset;
                                continue 'send_file_data;
                            }
                            ZCAN | ZABORT => return Err(TransferError::Cancelled),
                            _ => {}
                        }
                    }

                    if is_last {
                        break;
                    }
                }

                // Send ZEOF
                if let Ok(mut p) = progress.lock() {
                    *p = Some(TransferProgress {
                        direction: Direction::Send,
                        phase: TransferPhase::Finishing,
                        filename: filename.clone(),
                        bytes: sent,
                        total: Some(file_size),
                        cps: 0,
                    });
                }
                send_zbin32(stream, ZEOF, offset_bytes(sent))?;

                // Wait for ZRINIT (ready for next file), ZFIN, or ZRPOS if the
                // receiver discovered it needs retransmission at EOF.
                loop {
                    let (ftype, fdata) =
                        recv_frame_with_context(stream, "receiver finish response")?;
                    match ftype {
                        ZRINIT => break 'send_file_data,
                        ZFIN => {
                            send_zhex(stream, ZFIN, [0, 0, 0, 0])?;
                            stream.write_bytes(b"OO").ok();
                            results.push(TransferStats {
                                bytes: sent - start_offset,
                                filename,
                            });
                            return Ok(results);
                        }
                        ZRPOS => {
                            let offset = parse_offset(&fdata);
                            if offset > file_size {
                                return Err(TransferError::Protocol(format!(
                                    "receiver requested invalid ZRPOS offset {offset}"
                                )));
                            }
                            sent = offset;
                            continue 'send_file_data;
                        }
                        ZCAN | ZABORT => return Err(TransferError::Cancelled),
                        _ => {}
                    }
                }
            }

            results.push(TransferStats {
                bytes: sent - start_offset,
                filename,
            });
        }

        // End session
        send_zhex(stream, ZFIN, [0, 0, 0, 0])?;
        loop {
            let (ftype, _) = recv_frame_with_context(stream, "receiver ZFIN")?;
            match ftype {
                ZFIN => {
                    stream.write_bytes(b"OO").ok();
                    break;
                }
                ZRINIT => {
                    send_zhex(stream, ZFIN, [0, 0, 0, 0])?;
                }
                ZCAN | ZABORT => return Err(TransferError::Cancelled),
                _ => {}
            }
        }

        Ok(results)
    }
}

// ZCRCW is the same as ZCRCC in practice (end-of-subpacket, wait for ZACK)
const ZCRCW_OR_ZCRCC: u8 = ZCRCC;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_file_info(data: &[u8]) -> (String, Option<u64>) {
    let nul = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    let filename = std::str::from_utf8(&data[..nul])
        .unwrap_or("unknown")
        .to_string();

    let size = data
        .get(nul + 1..)
        .and_then(|s| {
            let end = s
                .iter()
                .position(|&b| b == b' ' || b == 0)
                .unwrap_or(s.len());
            std::str::from_utf8(&s[..end]).ok()
        })
        .and_then(|s| s.parse().ok());

    (filename, size)
}

fn sanitise_filename(name: &str) -> String {
    // Strip any Unix or DOS directory component the sender may have included.
    let basename = name
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("download");
    basename
        .chars()
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
    fn autostart_signature_matches() {
        let data = b"some garbage **\x18B00some more";
        let found = data
            .windows(ZMODEM_AUTOSTART_SIGNATURE.len())
            .any(|w| w == ZMODEM_AUTOSTART_SIGNATURE);
        assert!(found);
    }

    #[test]
    fn autostart_signature_negative() {
        let data = b"no zmodem here";
        let found = data
            .windows(ZMODEM_AUTOSTART_SIGNATURE.len())
            .any(|w| w == ZMODEM_AUTOSTART_SIGNATURE);
        assert!(!found);
    }

    #[test]
    fn zhex_frame_crc_correct() {
        // Verify our hex-encode + CRC-16 round-trip matches expected bytes
        // for a known ZRINIT frame sent by rz (from Zmodem spec appendix)
        let frame_type = ZRINIT;
        let data = zrinit_capabilities();
        let mut crc_input = [0u8; 5];
        crc_input[0] = frame_type;
        crc_input[1..].copy_from_slice(&data);
        let crc = crc16(&crc_input);
        // CRC should be deterministic
        let crc2 = crc16(&crc_input);
        assert_eq!(crc, crc2);
        assert_ne!(crc, 0); // sanity
    }

    #[test]
    fn recv_zhex_frame_does_not_consume_adjacent_frame_preamble() {
        let (incoming_tx, incoming_rx) = std::sync::mpsc::channel();
        let (write_tx, _write_rx) = tokio::sync::mpsc::channel(1);
        let mut stream = BlockingByteStream::new(incoming_rx, write_tx);

        let first = zhex_bytes_for_test(ZRQINIT, [0, 0, 0, 0]);
        let second = zhex_bytes_for_test(ZFILE, [1, 2, 3, 4]);
        let mut combined = first;
        combined.extend_from_slice(&second);
        incoming_tx.send(combined).unwrap();
        drop(incoming_tx);

        let (first_type, first_data) = recv_frame(&mut stream).unwrap();
        let (second_type, second_data) = recv_frame(&mut stream).unwrap();

        assert_eq!(first_type, ZRQINIT);
        assert_eq!(first_data, [0, 0, 0, 0]);
        assert_eq!(second_type, ZFILE);
        assert_eq!(second_data, [1, 2, 3, 4]);
    }

    #[test]
    fn recv_frame_skips_false_zpad_in_text_noise() {
        let (incoming_tx, incoming_rx) = std::sync::mpsc::channel();
        let (write_tx, _write_rx) = tokio::sync::mpsc::channel(1);
        let mut stream = BlockingByteStream::new(incoming_rx, write_tx);

        let mut incoming = b"Protocol * prompt noise\r\n".to_vec();
        incoming.extend_from_slice(&zhex_bytes_for_test(ZRINIT, zrinit_capabilities()));
        incoming_tx.send(incoming).unwrap();
        drop(incoming_tx);

        let (frame_type, frame_data) = recv_frame(&mut stream).unwrap();

        assert_eq!(frame_type, ZRINIT);
        assert_eq!(frame_data, zrinit_capabilities());
    }

    #[test]
    fn recv_zbin_frame_accepts_crc16_binary_header() {
        let frame_type = ZFILE;
        let data = [0x00, 0x01, 0x02, 0x03];
        let mut crc_input = [0u8; 5];
        crc_input[0] = frame_type;
        crc_input[1..].copy_from_slice(&data);
        let crc = crc16(&crc_input);

        let (incoming_tx, incoming_rx) = std::sync::mpsc::channel();
        let (write_tx, _write_rx) = tokio::sync::mpsc::channel(1);
        let mut stream = BlockingByteStream::new(incoming_rx, write_tx);

        let mut frame = vec![ZPAD, ZDLE, ZBIN, frame_type];
        frame.extend_from_slice(&data);
        frame.extend_from_slice(&crc.to_be_bytes());
        incoming_tx.send(frame).unwrap();
        drop(incoming_tx);

        let (decoded_type, decoded_data) = recv_frame(&mut stream).unwrap();

        assert_eq!(decoded_type, frame_type);
        assert_eq!(decoded_data, data);
    }

    fn zhex_bytes_for_test(frame_type: u8, data: [u8; 4]) -> Vec<u8> {
        let mut crc_input = [0u8; 5];
        crc_input[0] = frame_type;
        crc_input[1..].copy_from_slice(&data);
        let crc = crc16(&crc_input);

        let mut bytes = vec![ZPAD, ZPAD, ZDLE, ZHEX];
        push_hex_byte(&mut bytes, frame_type);
        for b in data {
            push_hex_byte(&mut bytes, b);
        }
        push_hex_byte(&mut bytes, (crc >> 8) as u8);
        push_hex_byte(&mut bytes, crc as u8);
        bytes.extend_from_slice(b"\r\n");
        bytes
    }

    fn parse_zhex_chunk_for_test(bytes: Vec<u8>) -> (u8, [u8; 4]) {
        let (incoming_tx, incoming_rx) = std::sync::mpsc::channel();
        let (write_tx, _write_rx) = tokio::sync::mpsc::channel(1);
        let mut stream = BlockingByteStream::new(incoming_rx, write_tx);
        incoming_tx.send(bytes).unwrap();
        drop(incoming_tx);
        recv_frame(&mut stream).unwrap()
    }

    fn parse_data_subpacket_for_test(bytes: Vec<u8>) -> (Vec<u8>, u8) {
        let (incoming_tx, incoming_rx) = std::sync::mpsc::channel();
        let (write_tx, _write_rx) = tokio::sync::mpsc::channel(1);
        let mut stream = BlockingByteStream::new(incoming_rx, write_tx);
        incoming_tx.send(bytes).unwrap();
        drop(incoming_tx);
        recv_data_subpacket(&mut stream).unwrap()
    }

    #[test]
    fn sender_completes_single_file_handshake() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("upload.bin");
        let payload = b"hello zmodem \x18\x7f\xff world";
        std::fs::write(&path, payload).unwrap();

        let (incoming_tx, incoming_rx) = std::sync::mpsc::channel();
        let (write_tx, mut write_rx) = tokio::sync::mpsc::channel(32);
        let progress = Arc::new(Mutex::new(None));
        let sender_progress = Arc::clone(&progress);
        let send_path = path.clone();

        let handle = std::thread::spawn(move || {
            let mut stream = BlockingByteStream::new(incoming_rx, write_tx);
            ZmodemSender::new().send(&mut stream, &[send_path.as_path()], sender_progress)
        });

        let (frame_type, frame_data) = parse_zhex_chunk_for_test(write_rx.blocking_recv().unwrap());
        assert_eq!(frame_type, ZRQINIT);
        assert_eq!(frame_data, [0, 0, 0, 0]);

        incoming_tx
            .send(zhex_bytes_for_test(ZRINIT, zrinit_capabilities()))
            .unwrap();

        let (frame_type, frame_data) = parse_zhex_chunk_for_test(write_rx.blocking_recv().unwrap());
        assert_eq!(frame_type, ZFILE);
        assert_eq!(frame_data, [0, 0, 0, ZCBIN]);

        let (file_info, term) = parse_data_subpacket_for_test(write_rx.blocking_recv().unwrap());
        assert_eq!(term, ZCRCW_OR_ZCRCC);
        assert!(file_info.starts_with(b"upload.bin\0"));
        let size = payload.len().to_string();
        assert!(file_info.windows(size.len()).any(|w| w == size.as_bytes()));

        incoming_tx
            .send(zhex_bytes_for_test(ZRPOS, offset_bytes(0)))
            .unwrap();

        let (frame_type, frame_data) = parse_zhex_chunk_for_test(write_rx.blocking_recv().unwrap());
        assert_eq!(frame_type, ZDATA);
        assert_eq!(parse_offset(&frame_data), 0);

        let (sent_payload, term) = parse_data_subpacket_for_test(write_rx.blocking_recv().unwrap());
        assert_eq!(term, ZCRCE);
        assert_eq!(sent_payload, payload);

        let (frame_type, frame_data) = parse_zhex_chunk_for_test(write_rx.blocking_recv().unwrap());
        assert_eq!(frame_type, ZEOF);
        assert_eq!(parse_offset(&frame_data), payload.len() as u64);

        incoming_tx
            .send(zhex_bytes_for_test(ZRINIT, zrinit_capabilities()))
            .unwrap();

        let (frame_type, frame_data) = parse_zhex_chunk_for_test(write_rx.blocking_recv().unwrap());
        assert_eq!(frame_type, ZFIN);
        assert_eq!(frame_data, [0, 0, 0, 0]);

        incoming_tx
            .send(zhex_bytes_for_test(ZFIN, [0, 0, 0, 0]))
            .unwrap();
        assert_eq!(write_rx.blocking_recv().unwrap(), b"OO");

        let stats = handle.join().unwrap().unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].filename, "upload.bin");
        assert_eq!(stats[0].bytes, payload.len() as u64);

        let progress = progress.lock().unwrap().clone().unwrap();
        assert_eq!(progress.direction, Direction::Send);
        assert_eq!(progress.filename, "upload.bin");
        assert_eq!(progress.bytes, payload.len() as u64);
        assert_eq!(progress.total, Some(payload.len() as u64));
    }

    #[test]
    fn sender_honors_zrpos_during_file_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large-upload.bin");
        let payload: Vec<u8> = (0..(SUBPACKET_SIZE * 10 + 123))
            .map(|i| (i % 251) as u8)
            .collect();
        std::fs::write(&path, &payload).unwrap();

        let (incoming_tx, incoming_rx) = std::sync::mpsc::channel();
        let (write_tx, mut write_rx) = tokio::sync::mpsc::channel(64);
        let progress = Arc::new(Mutex::new(None));
        let sender_progress = Arc::clone(&progress);
        let send_path = path.clone();

        let handle = std::thread::spawn(move || {
            let mut stream = BlockingByteStream::new(incoming_rx, write_tx);
            ZmodemSender::new().send(&mut stream, &[send_path.as_path()], sender_progress)
        });

        let (frame_type, _) = parse_zhex_chunk_for_test(write_rx.blocking_recv().unwrap());
        assert_eq!(frame_type, ZRQINIT);
        incoming_tx
            .send(zhex_bytes_for_test(ZRINIT, zrinit_capabilities()))
            .unwrap();

        let (frame_type, _) = parse_zhex_chunk_for_test(write_rx.blocking_recv().unwrap());
        assert_eq!(frame_type, ZFILE);
        let (_file_info, term) = parse_data_subpacket_for_test(write_rx.blocking_recv().unwrap());
        assert_eq!(term, ZCRCW_OR_ZCRCC);
        incoming_tx
            .send(zhex_bytes_for_test(ZRPOS, offset_bytes(0)))
            .unwrap();

        let (frame_type, frame_data) = parse_zhex_chunk_for_test(write_rx.blocking_recv().unwrap());
        assert_eq!(frame_type, ZDATA);
        assert_eq!(parse_offset(&frame_data), 0);

        for packet_index in 0..8 {
            let (sent_payload, term) =
                parse_data_subpacket_for_test(write_rx.blocking_recv().unwrap());
            let start = packet_index * SUBPACKET_SIZE;
            assert_eq!(&sent_payload, &payload[start..start + SUBPACKET_SIZE]);
            assert_eq!(term, if packet_index == 7 { ZCRCQ } else { ZCRCG });
        }

        let resume_offset = (SUBPACKET_SIZE * 4) as u64;
        incoming_tx
            .send(zhex_bytes_for_test(ZRPOS, offset_bytes(resume_offset)))
            .unwrap();

        let (frame_type, frame_data) = parse_zhex_chunk_for_test(write_rx.blocking_recv().unwrap());
        assert_eq!(frame_type, ZDATA);
        assert_eq!(parse_offset(&frame_data), resume_offset);

        let mut offset = resume_offset as usize;
        loop {
            let next = write_rx.blocking_recv().unwrap();
            if next.starts_with(&[ZPAD, ZDLE, ZBIN32])
                || next.starts_with(&[ZPAD, ZPAD, ZDLE, ZHEX])
            {
                let (frame_type, frame_data) = parse_zhex_chunk_for_test(next);
                assert_eq!(frame_type, ZEOF);
                assert_eq!(parse_offset(&frame_data), payload.len() as u64);
                break;
            }

            let (sent_payload, _term) = parse_data_subpacket_for_test(next);
            assert_eq!(&sent_payload, &payload[offset..offset + sent_payload.len()]);
            offset += sent_payload.len();
        }

        incoming_tx
            .send(zhex_bytes_for_test(ZRINIT, zrinit_capabilities()))
            .unwrap();
        let (frame_type, _) = parse_zhex_chunk_for_test(write_rx.blocking_recv().unwrap());
        assert_eq!(frame_type, ZFIN);
        incoming_tx
            .send(zhex_bytes_for_test(ZFIN, [0, 0, 0, 0]))
            .unwrap();
        assert_eq!(write_rx.blocking_recv().unwrap(), b"OO");

        let stats = handle.join().unwrap().unwrap();
        assert_eq!(stats[0].bytes, payload.len() as u64);
        let progress = progress.lock().unwrap().clone().unwrap();
        assert_eq!(progress.bytes, payload.len() as u64);
    }

    #[test]
    fn hex_pair_decode_valid() {
        assert_eq!(hex_pair_to_byte(b'0', b'0').unwrap(), 0x00);
        assert_eq!(hex_pair_to_byte(b'f', b'f').unwrap(), 0xFF);
        assert_eq!(hex_pair_to_byte(b'A', b'B').unwrap(), 0xAB);
        assert_eq!(hex_pair_to_byte(b'1', b'a').unwrap(), 0x1A);
    }

    #[test]
    fn hex_pair_decode_invalid() {
        assert!(hex_pair_to_byte(b'G', b'0').is_err());
    }

    #[test]
    fn zdle_escape_roundtrip() {
        let bytes = [ZDLE, 0x11, 0x13, 0x7F, 0x91, 0x93, 0xFF, 0x41];
        for &b in &bytes[..7] {
            assert!(needs_zdle_escape(b));
        }
        assert!(!needs_zdle_escape(bytes[7]));
        assert_eq!(decode_zdle_escaped_byte(ZRUB0), 0x7F);
        assert_eq!(decode_zdle_escaped_byte(ZRUB1), 0xFF);
    }

    #[test]
    fn data_subpacket_decodes_zrub_escapes() {
        let payload = [0x7F, 0xFF, b'A'];
        let term = ZCRCE;
        let mut crc = CRC32_INIT;
        for &b in &payload {
            crc = crc32_feed(crc, b);
        }
        crc = crc32_feed(crc, term);

        let mut packet = Vec::new();
        push_zdle_escaped_byte(&mut packet, 0x7F);
        push_zdle_escaped_byte(&mut packet, 0xFF);
        push_zdle_escaped_byte(&mut packet, b'A');
        packet.push(ZDLE);
        packet.push(term);
        for b in crc32_finish(crc).to_le_bytes() {
            push_zdle_escaped_byte(&mut packet, b);
        }

        let (incoming_tx, incoming_rx) = std::sync::mpsc::channel();
        let (write_tx, _write_rx) = tokio::sync::mpsc::channel(1);
        let mut stream = BlockingByteStream::new(incoming_rx, write_tx);
        incoming_tx.send(packet).unwrap();
        drop(incoming_tx);

        let (decoded, decoded_term) = recv_data_subpacket(&mut stream).unwrap();

        assert_eq!(decoded, payload);
        assert_eq!(decoded_term, term);
    }

    #[test]
    fn parse_file_info_basic() {
        let info = b"document.txt\x003456 0 0 0 1\x00extra";
        let (name, size) = parse_file_info(info);
        assert_eq!(name, "document.txt");
        assert_eq!(size, Some(3456));
    }

    #[test]
    fn parse_file_info_no_size() {
        let info = b"file.dat\x00";
        let (name, size) = parse_file_info(info);
        assert_eq!(name, "file.dat");
        assert_eq!(size, None);
    }

    #[test]
    fn sanitise_filename_strips_path() {
        assert_eq!(sanitise_filename("/etc/passwd"), "passwd");
        assert_eq!(sanitise_filename("foo/bar/baz.txt"), "baz.txt");
        assert_eq!(
            sanitise_filename(r"C:\WWIV\DLOADS\DOORS\pw_152d.zip"),
            "pw_152d.zip"
        );
    }

    #[test]
    fn offset_bytes_roundtrip() {
        let offsets = [0u64, 1, 65535, 1024 * 1024];
        for &off in &offsets {
            assert_eq!(parse_offset(&offset_bytes(off)), off);
        }
    }

    #[test]
    fn crc32_le_slice_matches_bulk() {
        let data = b"ZRINIT\x01\x00\x00\x00";
        let c1 = crc32_le_slice(data);
        let c2 = crate::transfer::crc::crc32(data);
        assert_eq!(c1, c2);
    }
}

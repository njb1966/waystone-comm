//! CRC utilities for file transfer protocols.
//!
//! - CRC-16/CCITT (0x1021 polynomial) — Xmodem, Ymodem, Zmodem headers
//! - CRC-32 (ISO 3309 / Ethernet, 0xEDB88320 reflected) — Zmodem data subpackets

// ── CRC-16/CCITT ──────────────────────────────────────────────────────────────

/// Calculate CRC-16/CCITT over `data`. Initial value 0x0000.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Simple additive byte checksum (original Xmodem).
pub fn checksum(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |acc, &b| acc.wrapping_add(b))
}

// ── CRC-32 ────────────────────────────────────────────────────────────────────

const fn make_crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut c = i as u32;
        let mut j = 0;
        while j < 8 {
            c = if c & 1 != 0 {
                0xEDB88320 ^ (c >> 1)
            } else {
                c >> 1
            };
            j += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

static CRC32_TABLE: [u32; 256] = make_crc32_table();

/// CRC-32 initial register value.
pub const CRC32_INIT: u32 = 0xFFFF_FFFF;

/// Feed one byte into a running CRC-32 register (start with `CRC32_INIT`).
#[inline]
pub fn crc32_feed(crc: u32, b: u8) -> u32 {
    let idx = ((crc ^ b as u32) & 0xFF) as usize;
    (crc >> 8) ^ CRC32_TABLE[idx]
}

/// Finalise a CRC-32 register (inverts all bits).
#[inline]
pub fn crc32_finish(crc: u32) -> u32 {
    !crc
}

/// Calculate CRC-32 over a complete slice. Used in tests.
#[allow(dead_code)]
pub fn crc32(data: &[u8]) -> u32 {
    crc32_finish(data.iter().fold(CRC32_INIT, |crc, &b| crc32_feed(crc, b)))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_known_value() {
        // CRC-16/CCITT with init=0 and poly=0x1021, as used by Xmodem/Zmodem.
        // "123456789" → 0x31C3 (not 0x29B1, which is the init=0xFFFF variant)
        assert_eq!(crc16(b"123456789"), 0x31C3);
    }

    #[test]
    fn crc16_empty() {
        assert_eq!(crc16(b""), 0x0000);
    }

    #[test]
    fn checksum_basic() {
        assert_eq!(checksum(&[0x01, 0x02, 0x03]), 0x06);
        assert_eq!(checksum(&[0xFF, 0x01]), 0x00); // wrapping
    }

    #[test]
    fn crc32_known_value() {
        // "123456789" → 0xCBF43926 (standard CRC-32 test vector)
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn crc32_empty() {
        assert_eq!(crc32(b""), 0x0000_0000);
    }

    #[test]
    fn crc32_incremental_matches_bulk() {
        let data = b"hello world";
        let bulk = crc32(data);
        let incremental = crc32_finish(data.iter().fold(CRC32_INIT, |c, &b| crc32_feed(c, b)));
        assert_eq!(bulk, incremental);
    }
}

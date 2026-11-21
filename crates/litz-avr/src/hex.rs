//! Intel HEX parsing and raw-binary loading.
//!
//! The parser is strict on purpose: a record whose length does not match its
//! byte count, whose digits are not hexadecimal, or whose checksum does not
//! make the record sum to zero is refused with a line number. Record types
//! 00, 01, 02, 03, 04 and 05 are understood; anything else is an error rather
//! than a silently skipped line.

use std::fmt;

/// Largest image this loader will build.
pub const MAX_IMAGE: u32 = 32 * 1024;

/// Why an Intel HEX image was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HexError {
    /// A non-empty line did not begin with `:`.
    MissingColon { line: usize },
    /// A line had an odd number of hexadecimal digits.
    OddLength { line: usize },
    /// A digit was not `0`–`9`, `a`–`f` or `A`–`F`.
    NotHex { line: usize },
    /// The length byte did not agree with the record, or the record was too
    /// short to hold its own header.
    Truncated { line: usize },
    /// The checksum byte did not make the record sum to zero.
    BadChecksum {
        line: usize,
        expected: u8,
        found: u8,
    },
    /// A record type this loader does not implement.
    UnsupportedRecord { line: usize, kind: u8 },
    /// A data record pointed past the end of flash.
    AddressOutOfRange { line: usize, address: u32 },
}

impl fmt::Display for HexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HexError::MissingColon { line } => {
                write!(f, "line {line}: record does not start with ':'")
            }
            HexError::OddLength { line } => write!(f, "line {line}: odd number of hex digits"),
            HexError::NotHex { line } => write!(f, "line {line}: non-hexadecimal digit"),
            HexError::Truncated { line } => {
                write!(f, "line {line}: record length does not match its bytes")
            }
            HexError::BadChecksum {
                line,
                expected,
                found,
            } => write!(
                f,
                "line {line}: checksum {found:#04x} does not match {expected:#04x}"
            ),
            HexError::UnsupportedRecord { line, kind } => {
                write!(f, "line {line}: unsupported record type {kind:#04x}")
            }
            HexError::AddressOutOfRange { line, address } => {
                write!(f, "line {line}: address {address:#06x} is outside flash")
            }
        }
    }
}

impl std::error::Error for HexError {}

/// Whether an image looks like Intel HEX rather than a raw binary.
pub fn looks_like_hex(bytes: &[u8]) -> bool {
    matches!(
        bytes.iter().find(|byte| !byte.is_ascii_whitespace()),
        Some(b':')
    )
}

/// Load Intel HEX text into an image, zero-filled up to its highest written
/// byte. An empty input yields an empty image.
pub fn load_hex(text: &str) -> Result<Vec<u8>, HexError> {
    parse_hex(text)
}

/// Parse Intel HEX text into a sparse image, zero-filled up to its highest
/// written byte. An empty input yields an empty image.
pub fn parse_hex(text: &str) -> Result<Vec<u8>, HexError> {
    let mut image: Vec<u8> = Vec::new();
    let mut base: u32 = 0;

    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let body = raw
            .strip_prefix(':')
            .ok_or(HexError::MissingColon { line })?;
        if body.len() % 2 != 0 {
            return Err(HexError::OddLength { line });
        }
        let mut record = Vec::with_capacity(body.len() / 2);
        for pair in body.as_bytes().chunks(2) {
            let text = core::str::from_utf8(pair).map_err(|_| HexError::NotHex { line })?;
            let byte = u8::from_str_radix(text, 16).map_err(|_| HexError::NotHex { line })?;
            record.push(byte);
        }
        if record.len() < 5 {
            return Err(HexError::Truncated { line });
        }
        let length = usize::from(record[0]);
        if record.len() != length + 5 {
            return Err(HexError::Truncated { line });
        }

        // The checksum is chosen so all bytes, the checksum included, sum to
        // zero modulo 256.
        let sum = record.iter().fold(0u8, |acc, byte| acc.wrapping_add(*byte));
        if sum != 0 {
            let data_sum = record[..record.len() - 1]
                .iter()
                .fold(0u8, |acc, byte| acc.wrapping_add(*byte));
            let expected = 0u8.wrapping_sub(data_sum);
            return Err(HexError::BadChecksum {
                line,
                expected,
                found: record[record.len() - 1],
            });
        }

        let offset = u32::from(record[1]) << 8 | u32::from(record[2]);
        let kind = record[3];
        let data = &record[4..4 + length];

        match kind {
            0x00 => {
                let start = base + offset;
                let end = start + length as u32;
                if end > MAX_IMAGE {
                    return Err(HexError::AddressOutOfRange {
                        line,
                        address: start,
                    });
                }
                let end = end as usize;
                if image.len() < end {
                    image.resize(end, 0);
                }
                image[start as usize..end].copy_from_slice(data);
            }
            0x01 => break, // end of file
            0x02 => {
                if length != 2 {
                    return Err(HexError::Truncated { line });
                }
                base = (u32::from(data[0]) << 8 | u32::from(data[1])) << 4;
            }
            0x03 | 0x05 => {
                // Start address records are valid but carry no image bytes.
            }
            0x04 => {
                if length != 2 {
                    return Err(HexError::Truncated { line });
                }
                base = (u32::from(data[0]) << 8 | u32::from(data[1])) << 16;
            }
            kind => return Err(HexError::UnsupportedRecord { line, kind }),
        }
    }

    Ok(image)
}

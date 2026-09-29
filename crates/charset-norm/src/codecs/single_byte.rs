//! ASCII, Latin-1 and table-driven single-byte code pages.

use std::sync::OnceLock;

use super::{DecodeError, Errors};
use crate::tables::SINGLE_BYTE_CODECS;

pub(super) fn decode_ascii(data: &[u8], errors: Errors) -> Result<String, DecodeError> {
    if data.is_ascii() {
        // SAFETY-free: ASCII is valid UTF-8.
        return Ok(String::from_utf8(data.to_vec()).unwrap_or_default());
    }
    match errors {
        Errors::Strict => Err(DecodeError::Invalid),
        Errors::Ignore => Ok(data
            .iter()
            .filter(|byte| byte.is_ascii())
            .map(|&byte| byte as char)
            .collect()),
    }
}

/// Length of the leading run of ASCII bytes, scanned a word at a time.
#[inline]
pub(super) fn ascii_prefix(data: &[u8]) -> usize {
    let mut length = 0;
    for chunk in data.as_chunks::<8>().0 {
        let word = u64::from_le_bytes([
            chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
        ]);
        if word & 0x8080_8080_8080_8080 != 0 {
            break;
        }
        length += 8;
    }
    length
        + data[length..]
            .iter()
            .position(|byte| !byte.is_ascii())
            .unwrap_or(data.len() - length)
}

/// Single-byte code pages as `char` tables (`None` for undefined bytes).
pub(super) fn single_byte_chars(index: usize) -> &'static [Option<char>; 256] {
    static TABLES: OnceLock<Vec<[Option<char>; 256]>> = OnceLock::new();
    let tables = TABLES.get_or_init(|| {
        SINGLE_BYTE_CODECS
            .iter()
            .map(|(_, table)| {
                let mut chars = [None; 256];
                for (slot, &value) in chars.iter_mut().zip(table.iter()) {
                    if value != 0xFFFE {
                        *slot = char::from_u32(u32::from(value));
                    }
                }
                chars
            })
            .collect()
    });
    &tables[index]
}

/// Copy a run of ASCII bytes that the codec maps onto themselves.
#[inline]
pub(super) fn push_ascii(out: &mut String, run: &[u8]) {
    out.push_str(std::str::from_utf8(run).unwrap_or_default());
}

pub(super) fn decode_single_byte(
    data: &[u8],
    index: usize,
    errors: Errors,
) -> Result<String, DecodeError> {
    let chars = single_byte_chars(index);
    let mut out = String::with_capacity(data.len() * 2);
    for &byte in data {
        match chars[byte as usize] {
            Some(character) => out.push(character),
            None if errors == Errors::Strict => return Err(DecodeError::Invalid),
            None => {}
        }
    }
    Ok(out)
}

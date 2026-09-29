//! ASCII, Latin-1 and table-driven single-byte code pages.

use std::sync::OnceLock;

use super::{DecodeError, Errors};
use crate::tables::SINGLE_BYTE_CODECS;

pub(super) fn decode_ascii(
    data: &[u8],
    errors: Errors,
    out: &mut String,
) -> Result<(), DecodeError> {
    if data.is_ascii() {
        push_ascii(out, data);
        return Ok(());
    }
    if errors == Errors::Strict {
        return Err(DecodeError::Invalid);
    }
    out.extend(
        data.iter()
            .filter(|byte| byte.is_ascii())
            .map(|&byte| char::from(byte)),
    );
    Ok(())
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

/// Number of leading bytes of `data` in whole 8-byte words of ASCII.
#[inline]
fn ascii_words(data: &[u8]) -> usize {
    data.as_chunks::<8>()
        .0
        .iter()
        .take_while(|chunk| u64::from_ne_bytes(**chunk) & 0x8080_8080_8080_8080 == 0)
        .count()
        * 8
}

/// A single-byte code page as a `char` table (`None` for undefined bytes).
pub(super) struct SingleByteTable {
    pub(super) chars: [Option<char>; 256],
    /// Bytes below 0x80 decode to themselves.
    ascii_identity: bool,
}

impl SingleByteTable {
    /// Whether every byte of `data` is defined.
    pub(super) fn is_valid(&self, data: &[u8]) -> bool {
        let defined = |bytes: &[u8]| {
            bytes
                .iter()
                .all(|&byte| self.chars[usize::from(byte)].is_some())
        };
        if !self.ascii_identity {
            return defined(data);
        }
        let (words, tail) = data.as_chunks::<8>();
        words
            .iter()
            .all(|word| u64::from_ne_bytes(*word) & 0x8080_8080_8080_8080 == 0 || defined(word))
            && defined(tail)
    }
}

/// Single-byte code pages, by index into `SINGLE_BYTE_CODECS`.
pub(super) fn single_byte_table(index: usize) -> &'static SingleByteTable {
    static TABLES: OnceLock<Vec<SingleByteTable>> = OnceLock::new();
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
                let ascii_identity =
                    (0u8..0x80).all(|byte| chars[usize::from(byte)] == Some(char::from(byte)));
                SingleByteTable {
                    chars,
                    ascii_identity,
                }
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
    out: &mut String,
) -> Result<(), DecodeError> {
    let table = single_byte_table(index);
    out.reserve(data.len() * 2);
    let mut position = 0;
    while let Some(&byte) = data.get(position) {
        // Copy runs of whole ASCII words at once; short runs stay per byte.
        if byte < 0x80 && table.ascii_identity {
            let run = ascii_words(&data[position..]);
            if run > 0 {
                push_ascii(out, &data[position..position + run]);
                position += run;
                continue;
            }
        }
        position += 1;
        match table.chars[usize::from(byte)] {
            Some(character) => out.push(character),
            None if errors == Errors::Strict => return Err(DecodeError::Invalid),
            None => {}
        }
    }
    Ok(())
}

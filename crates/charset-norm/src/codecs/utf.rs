//! UTF-8, UTF-16, UTF-32 and UTF-7 decoders with `CPython`'s error handling.

use super::{DecodeError, Endian, Errors};

pub(super) fn decode_utf8(data: &[u8], errors: Errors) -> Result<String, DecodeError> {
    match std::str::from_utf8(data) {
        Ok(value) => Ok(value.to_owned()),
        Err(_) if errors == Errors::Strict => Err(DecodeError::Invalid),
        Err(_) => {
            // Invalid maximal subparts are dropped, as CPython's "ignore" does.
            let mut out = String::with_capacity(data.len());
            for chunk in data.utf8_chunks() {
                out.push_str(chunk.valid());
            }
            Ok(out)
        }
    }
}

/// Strict UTF-16 validity, checked before any output is built: arbitrary
/// bytes often look like UTF-16 for a long stretch before failing.
fn valid_utf16(data: &[u8], little: bool) -> bool {
    if !data.len().is_multiple_of(2) {
        return false;
    }
    let mut pending_high = false;
    for pair in data.as_chunks::<2>().0 {
        let unit = if little {
            u16::from_le_bytes([pair[0], pair[1]])
        } else {
            u16::from_be_bytes([pair[0], pair[1]])
        };
        let low = (0xDC00..0xE000).contains(&unit);
        if pending_high != low {
            return false;
        }
        pending_high = (0xD800..0xDC00).contains(&unit);
    }
    !pending_high
}

pub(super) fn decode_utf16(
    data: &[u8],
    endian: Endian,
    errors: Errors,
) -> Result<String, DecodeError> {
    let (mut data, little) = match endian {
        Endian::Little => (data, true),
        Endian::Big => (data, false),
        Endian::Detect => {
            if let Some(rest) = data.strip_prefix(b"\xff\xfe") {
                (rest, true)
            } else if let Some(rest) = data.strip_prefix(b"\xfe\xff") {
                (rest, false)
            } else {
                (data, cfg!(target_endian = "little"))
            }
        }
    };
    let unit = |bytes: &[u8]| {
        if little {
            u16::from_le_bytes([bytes[0], bytes[1]])
        } else {
            u16::from_be_bytes([bytes[0], bytes[1]])
        }
    };
    if errors == Errors::Strict && !valid_utf16(data, little) {
        return Err(DecodeError::Invalid);
    }
    let mut out = String::with_capacity(data.len());
    while !data.is_empty() {
        if data.len() < 2 {
            // Truncated data: the error spans the rest of the input.
            if errors == Errors::Strict {
                return Err(DecodeError::Invalid);
            }
            break;
        }
        let first = unit(data);
        if !(0xD800..0xE000).contains(&first) {
            out.push(char::from_u32(u32::from(first)).ok_or(DecodeError::Invalid)?);
            data = &data[2..];
            continue;
        }
        if first < 0xDC00 && data.len() >= 4 {
            let second = unit(&data[2..]);
            if (0xDC00..0xE000).contains(&second) {
                let value =
                    0x10000 + ((u32::from(first) - 0xD800) << 10) + (u32::from(second) - 0xDC00);
                out.push(char::from_u32(value).ok_or(DecodeError::Invalid)?);
                data = &data[4..];
                continue;
            }
        }
        if errors == Errors::Strict {
            return Err(DecodeError::Invalid);
        }
        if first < 0xDC00 && data.len() < 4 {
            break; // unexpected end of data
        }
        data = &data[2..];
    }
    Ok(out)
}

pub(super) fn decode_utf32(
    data: &[u8],
    endian: Endian,
    errors: Errors,
) -> Result<String, DecodeError> {
    let (mut data, little) = match endian {
        Endian::Little => (data, true),
        Endian::Big => (data, false),
        Endian::Detect => {
            if let Some(rest) = data.strip_prefix(b"\xff\xfe\x00\x00") {
                (rest, true)
            } else if let Some(rest) = data.strip_prefix(b"\x00\x00\xfe\xff") {
                (rest, false)
            } else {
                (data, cfg!(target_endian = "little"))
            }
        }
    };
    if errors == Errors::Strict {
        let valid = data.len() % 4 == 0
            && data.as_chunks::<4>().0.iter().all(|quad| {
                let bytes = [quad[0], quad[1], quad[2], quad[3]];
                let value = if little {
                    u32::from_le_bytes(bytes)
                } else {
                    u32::from_be_bytes(bytes)
                };
                char::from_u32(value).is_some()
            });
        if !valid {
            return Err(DecodeError::Invalid);
        }
    }
    let mut out = String::with_capacity(data.len());
    while !data.is_empty() {
        if data.len() < 4 {
            if errors == Errors::Strict {
                return Err(DecodeError::Invalid);
            }
            break;
        }
        let bytes = [data[0], data[1], data[2], data[3]];
        let value = if little {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        };
        match char::from_u32(value) {
            Some(character) => out.push(character),
            None if errors == Errors::Strict => return Err(DecodeError::Invalid),
            None => {}
        }
        data = &data[4..];
    }
    Ok(out)
}

fn is_base64(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/'
}

fn from_base64(byte: u8) -> u32 {
    match byte {
        b'A'..=b'Z' => u32::from(byte - b'A'),
        b'a'..=b'z' => u32::from(byte - b'a') + 26,
        b'0'..=b'9' => u32::from(byte - b'0') + 52,
        b'+' => 62,
        _ => 63,
    }
}

/// Port of `CPython`'s `PyUnicode_DecodeUTF7Stateful` (final mode). A lone
/// surrogate, which a Rust string cannot hold, is reported as an error.
pub(super) fn decode_utf7(data: &[u8], errors: Errors) -> Result<String, DecodeError> {
    let mut out = String::with_capacity(data.len());
    let mut position = 0usize;
    let mut in_shift = false;
    let mut shift_start = 0usize;
    let mut bits = 0u32;
    let mut buffer = 0u64;
    let mut surrogate = 0u32;
    let strict = errors == Errors::Strict;

    macro_rules! fail {
        () => {{
            if strict {
                return Err(DecodeError::Invalid);
            }
        }};
    }

    while position < data.len() {
        let byte = data[position];
        if in_shift {
            if is_base64(byte) {
                buffer = (buffer << 6) | u64::from(from_base64(byte));
                bits += 6;
                position += 1;
                if bits >= 16 {
                    let unit = u32::try_from((buffer >> (bits - 16)) & 0xFFFF).unwrap_or_default();
                    bits -= 16;
                    buffer &= (1u64 << bits) - 1;
                    if surrogate != 0 {
                        if (0xDC00..0xE000).contains(&unit) {
                            let value = 0x10000 + ((surrogate - 0xD800) << 10) + (unit - 0xDC00);
                            out.push(char::from_u32(value).ok_or(DecodeError::Invalid)?);
                            surrogate = 0;
                            continue;
                        }
                        fail!(); // lone high surrogate
                        surrogate = 0;
                    }
                    if (0xD800..0xDC00).contains(&unit) {
                        surrogate = unit;
                    } else if let Some(character) = char::from_u32(unit) {
                        out.push(character);
                    } else {
                        fail!(); // lone low surrogate
                    }
                }
            } else {
                in_shift = false;
                if bits > 0 {
                    if bits >= 6 {
                        position += 1;
                        fail!(); // partial character in shift sequence
                        continue;
                    } else if buffer != 0 {
                        position += 1;
                        fail!(); // non-zero padding bits in shift sequence
                        continue;
                    }
                }
                if surrogate != 0 && byte <= 127 && byte != b'+' {
                    fail!(); // lone high surrogate
                }
                surrogate = 0;
                if byte == b'-' {
                    position += 1;
                }
            }
        } else if byte == b'+' {
            shift_start = position;
            position += 1;
            if position < data.len() && data[position] == b'-' {
                position += 1;
                out.push('+');
            } else if position < data.len() && !is_base64(data[position]) {
                position += 1;
                fail!(); // ill-formed sequence
            } else {
                in_shift = true;
                surrogate = 0;
                bits = 0;
                buffer = 0;
            }
        } else if byte <= 127 {
            position += 1;
            out.push(byte as char);
        } else {
            position += 1;
            fail!(); // unexpected special character
        }
    }
    if in_shift && (surrogate != 0 || bits >= 6 || (bits > 0 && buffer != 0)) {
        // Unterminated shift sequence; the error spans [shift_start, end).
        let _ = shift_start;
        fail!();
    }
    Ok(out)
}

//! Native implementations of the CPython codecs charset-normalizer probes.
//!
//! Decoding must match CPython bit for bit: the detector's verdict depends on
//! which byte sequences a codec rejects. Unicode transformation formats are
//! implemented directly, single-byte code pages and CJK codecs are driven by
//! tables generated from CPython (see `bin/generate_native_tables.py`), and
//! the stateful ISO-2022 / HZ / UTF-7 decoders are ports of CPython's.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::tables::{iana_lookup, SINGLE_BYTE_CODECS};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Errors {
    Strict,
    Ignore,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// No native implementation for this codec name.
    Unknown,
    /// The payload is not valid for the codec (strict mode).
    Invalid,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Endian {
    Little,
    Big,
    Detect,
}

#[derive(Clone, Copy)]
enum Iso2022Variant {
    Kr,
    Jp,
    Jp1,
    Jp2,
    Jp2004,
    Jp3,
    JpExt,
}

#[derive(Clone, Copy)]
enum Codec {
    Ascii,
    Latin1,
    /// Index into `SINGLE_BYTE_CODECS`.
    SingleByte(usize),
    Utf8 {
        sig: bool,
    },
    Utf16(Endian),
    Utf32(Endian),
    Utf7,
    Cjk(&'static str),
    Iso2022(Iso2022Variant),
    Hz,
}

fn codec_by_name(name: &str) -> Option<Codec> {
    Some(match name {
        "ascii" => Codec::Ascii,
        "latin_1" => Codec::Latin1,
        "utf_8" => Codec::Utf8 { sig: false },
        "utf_8_sig" => Codec::Utf8 { sig: true },
        "utf_16" => Codec::Utf16(Endian::Detect),
        "utf_16_le" => Codec::Utf16(Endian::Little),
        "utf_16_be" => Codec::Utf16(Endian::Big),
        "utf_32" => Codec::Utf32(Endian::Detect),
        "utf_32_le" => Codec::Utf32(Endian::Little),
        "utf_32_be" => Codec::Utf32(Endian::Big),
        "utf_7" => Codec::Utf7,
        "hz" => Codec::Hz,
        "iso2022_kr" => Codec::Iso2022(Iso2022Variant::Kr),
        "iso2022_jp" => Codec::Iso2022(Iso2022Variant::Jp),
        "iso2022_jp_1" => Codec::Iso2022(Iso2022Variant::Jp1),
        "iso2022_jp_2" => Codec::Iso2022(Iso2022Variant::Jp2),
        "iso2022_jp_2004" => Codec::Iso2022(Iso2022Variant::Jp2004),
        "iso2022_jp_3" => Codec::Iso2022(Iso2022Variant::Jp3),
        "iso2022_jp_ext" => Codec::Iso2022(Iso2022Variant::JpExt),
        "big5" | "big5hkscs" | "cp932" | "cp949" | "cp950" | "euc_jis_2004" | "euc_jisx0213"
        | "euc_jp" | "euc_kr" | "gb18030" | "gb2312" | "gbk" | "johab" | "shift_jis"
        | "shift_jis_2004" | "shift_jisx0213" => {
            let index = CJK_NAMES.iter().position(|value| *value == name)?;
            Codec::Cjk(CJK_NAMES[index])
        }
        _ => {
            let index = SINGLE_BYTE_CODECS
                .binary_search_by(|(codec, _)| (*codec).cmp(name))
                .ok()?;
            Codec::SingleByte(index)
        }
    })
}

/// `encodings.normalize_encoding` followed by the alias table.
fn lookup(encoding: &str) -> Option<Codec> {
    if let Some(codec) = codec_by_name(encoding) {
        return Some(codec);
    }
    let mut normalized = String::with_capacity(encoding.len());
    let mut punctuation = false;
    for character in encoding.chars() {
        if character.is_alphanumeric() || character == '.' {
            if punctuation && !normalized.is_empty() {
                normalized.push('_');
            }
            normalized.push(character.to_ascii_lowercase());
            punctuation = false;
        } else {
            punctuation = true;
        }
    }
    codec_by_name(&normalized).or_else(|| codec_by_name(iana_lookup(&normalized)?))
}

pub fn is_known(encoding: &str) -> bool {
    lookup(encoding).is_some()
}

pub fn decode(data: &[u8], encoding: &str, errors: Errors) -> Result<String, DecodeError> {
    let codec = lookup(encoding).ok_or(DecodeError::Unknown)?;
    match codec {
        Codec::Ascii => decode_ascii(data, errors),
        Codec::Latin1 => Ok(data.iter().map(|&byte| byte as char).collect()),
        Codec::SingleByte(index) => decode_single_byte(data, index, errors),
        Codec::Utf8 { sig } => {
            let data = if sig {
                data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data)
            } else {
                data
            };
            decode_utf8(data, errors)
        }
        Codec::Utf16(endian) => decode_utf16(data, endian, errors),
        Codec::Utf32(endian) => decode_utf32(data, endian, errors),
        Codec::Utf7 => decode_utf7(data, errors),
        Codec::Cjk(name) => decode_cjk(cjk().codec(name), data, errors),
        Codec::Iso2022(variant) => decode_iso2022(variant, data, errors),
        Codec::Hz => decode_hz(data, errors),
    }
}

/// Whether `decode(data, encoding, Strict)` would succeed, without building
/// the text when the codec allows a cheaper check.
pub fn is_valid(data: &[u8], encoding: &str) -> Result<bool, DecodeError> {
    match lookup(encoding).ok_or(DecodeError::Unknown)? {
        Codec::Ascii => Ok(data.is_ascii()),
        Codec::Latin1 => Ok(true),
        Codec::SingleByte(index) => {
            let chars = single_byte_chars(index);
            Ok(data.iter().all(|&byte| chars[byte as usize].is_some()))
        }
        Codec::Utf8 { .. } => Ok(std::str::from_utf8(data).is_ok()),
        _ => match decode(data, encoding, Errors::Strict) {
            Ok(_) => Ok(true),
            Err(DecodeError::Invalid) => Ok(false),
            Err(error) => Err(error),
        },
    }
}

/// Decode one byte in isolation, as `IncrementalDecoder(errors="ignore")`
/// does for single-byte code pages. `None` for multi-byte codecs.
pub fn single_byte_decoder(encoding: &str) -> Option<impl Fn(u8) -> Option<char>> {
    let codec = lookup(encoding)?;
    let chars = match codec {
        Codec::Ascii | Codec::Latin1 => None,
        Codec::SingleByte(index) => Some(single_byte_chars(index)),
        _ => return None,
    };
    let ascii = matches!(codec, Codec::Ascii);
    Some(move |byte: u8| match chars {
        Some(chars) => chars[byte as usize],
        None if ascii => (byte < 0x80).then_some(byte as char),
        None => Some(byte as char),
    })
}

/// Encode with `errors="replace"`. `None` when there is no native encoder.
pub fn encode(text: &str, encoding: &str) -> Option<Vec<u8>> {
    match lookup(encoding)? {
        Codec::Utf8 { sig } => {
            let mut out = Vec::with_capacity(text.len() + 3);
            if sig {
                out.extend_from_slice(b"\xef\xbb\xbf");
            }
            out.extend_from_slice(text.as_bytes());
            Some(out)
        }
        Codec::Utf16(endian) => {
            let mut out = Vec::with_capacity(text.len() * 2 + 2);
            let little = endian != Endian::Big;
            if endian == Endian::Detect {
                out.extend_from_slice(b"\xff\xfe");
            }
            for unit in text.encode_utf16() {
                out.extend_from_slice(&if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            Some(out)
        }
        Codec::Utf32(endian) => {
            let mut out = Vec::with_capacity(text.len() * 4 + 4);
            let little = endian != Endian::Big;
            if endian == Endian::Detect {
                out.extend_from_slice(b"\xff\xfe\x00\x00");
            }
            for character in text.chars() {
                let value = character as u32;
                out.extend_from_slice(&if little {
                    value.to_le_bytes()
                } else {
                    value.to_be_bytes()
                });
            }
            Some(out)
        }
        Codec::Ascii => Some(
            text.chars()
                .map(|c| if c.is_ascii() { c as u8 } else { b'?' })
                .collect(),
        ),
        Codec::Latin1 => Some(
            text.chars()
                .map(|c| if (c as u32) < 256 { c as u8 } else { b'?' })
                .collect(),
        ),
        Codec::SingleByte(index) => {
            let table = &SINGLE_BYTE_CODECS[index].1;
            // Later bytes win on duplicates, like codecs.charmap_build.
            let mut reverse = HashMap::with_capacity(256);
            for (byte, &value) in table.iter().enumerate() {
                if value != 0xFFFE {
                    reverse.insert(value as u32, byte as u8);
                }
            }
            let question = *reverse.get(&('?' as u32))?;
            Some(
                text.chars()
                    .map(|c| reverse.get(&(c as u32)).copied().unwrap_or(question))
                    .collect(),
            )
        }
        _ => None,
    }
}

fn decode_ascii(data: &[u8], errors: Errors) -> Result<String, DecodeError> {
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
fn ascii_prefix(data: &[u8]) -> usize {
    let mut length = 0;
    for chunk in data.chunks_exact(8) {
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
fn single_byte_chars(index: usize) -> &'static [Option<char>; 256] {
    static TABLES: OnceLock<Vec<[Option<char>; 256]>> = OnceLock::new();
    let tables = TABLES.get_or_init(|| {
        SINGLE_BYTE_CODECS
            .iter()
            .map(|(_, table)| {
                let mut chars = [None; 256];
                for (slot, &value) in chars.iter_mut().zip(table.iter()) {
                    if value != 0xFFFE {
                        *slot = char::from_u32(value as u32);
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
fn push_ascii(out: &mut String, run: &[u8]) {
    out.push_str(std::str::from_utf8(run).unwrap_or_default());
}

fn decode_single_byte(data: &[u8], index: usize, errors: Errors) -> Result<String, DecodeError> {
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

fn decode_utf8(data: &[u8], errors: Errors) -> Result<String, DecodeError> {
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
    if data.len() % 2 != 0 {
        return false;
    }
    let mut pending_high = false;
    for pair in data.chunks_exact(2) {
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

fn decode_utf16(data: &[u8], endian: Endian, errors: Errors) -> Result<String, DecodeError> {
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
            out.push(char::from_u32(first as u32).ok_or(DecodeError::Invalid)?);
            data = &data[2..];
            continue;
        }
        if first < 0xDC00 && data.len() >= 4 {
            let second = unit(&data[2..]);
            if (0xDC00..0xE000).contains(&second) {
                let value = 0x10000 + (((first as u32) - 0xD800) << 10) + (second as u32 - 0xDC00);
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

fn decode_utf32(data: &[u8], endian: Endian, errors: Errors) -> Result<String, DecodeError> {
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
            && data.chunks_exact(4).all(|quad| {
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
        b'A'..=b'Z' => (byte - b'A') as u32,
        b'a'..=b'z' => (byte - b'a') as u32 + 26,
        b'0'..=b'9' => (byte - b'0') as u32 + 52,
        b'+' => 62,
        _ => 63,
    }
}

/// Port of CPython's `PyUnicode_DecodeUTF7Stateful` (final mode). A lone
/// surrogate, which a Rust string cannot hold, is reported as an error.
fn decode_utf7(data: &[u8], errors: Errors) -> Result<String, DecodeError> {
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
                buffer = (buffer << 6) | from_base64(byte) as u64;
                bits += 6;
                position += 1;
                if bits >= 16 {
                    let unit = (buffer >> (bits - 16)) as u32 & 0xFFFF;
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

/* ---------------------------------------------------------------------- */
/* Table-driven CJK codecs                                                 */
/* ---------------------------------------------------------------------- */

const CJK_NAMES: [&str; 16] = [
    "big5",
    "big5hkscs",
    "cp932",
    "cp949",
    "cp950",
    "euc_jis_2004",
    "euc_jisx0213",
    "euc_jp",
    "euc_kr",
    "gb18030",
    "gb2312",
    "gbk",
    "johab",
    "shift_jis",
    "shift_jis_2004",
    "shift_jisx0213",
];

const NONE: u32 = 0xFF_FFFF;
const PAIR_BASE: u32 = 0x11_0000;
/// Marks an invalid sequence spanning `value - ERROR_BASE` bytes.
const ERROR_BASE: u32 = 0xFF_FFF0;

struct Row {
    first: u8,
    values: Vec<u32>,
}

/// Rows keyed by their first byte, each spanning a contiguous trail range.
struct Rows {
    rows: Vec<Option<Row>>,
}

impl Rows {
    fn get(&self, first: u8, second: u8) -> Option<u32> {
        let row = self.rows[first as usize].as_ref()?;
        let offset = second.checked_sub(row.first)? as usize;
        row.values
            .get(offset)
            .copied()
            .filter(|value| *value != NONE)
    }
}

struct CjkCodec {
    /// Bytes below 0x80 decode to themselves.
    ascii_identity: bool,
    need: [u8; 256],
    single: [u32; 256],
    double: Rows,
    triple_prefix: u8,
    triple: Rows,
}

struct CjkData {
    codecs: HashMap<&'static str, CjkCodec>,
    iso2022: HashMap<&'static str, Rows>,
    gb18030_ranges: Vec<(u32, u32)>,
    pairs: Vec<(char, char)>,
}

impl CjkData {
    fn codec(&self, name: &str) -> &CjkCodec {
        &self.codecs[name]
    }
}

static CJK_BLOB: &[u8] = include_bytes!("generated/cjk.bin");

struct Reader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> &'a [u8] {
        let slice = &self.data[self.position..self.position + length];
        self.position += length;
        slice
    }
    fn u8(&mut self) -> u8 {
        self.take(1)[0]
    }
    fn u16(&mut self) -> u16 {
        let bytes = self.take(2);
        u16::from_le_bytes([bytes[0], bytes[1]])
    }
    fn u24(&mut self) -> u32 {
        let bytes = self.take(3);
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0])
    }
    fn u32(&mut self) -> u32 {
        let bytes = self.take(4);
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    }
    fn rows(&mut self) -> Rows {
        let mut rows: Vec<Option<Row>> = (0..256).map(|_| None).collect();
        for _ in 0..self.u16() {
            let lead = self.u8();
            let first = self.u8();
            let count = self.u16() as usize;
            let values = (0..count).map(|_| self.u24()).collect();
            rows[lead as usize] = Some(Row { first, values });
        }
        Rows { rows }
    }
}

fn cjk() -> &'static CjkData {
    static DATA: OnceLock<CjkData> = OnceLock::new();
    DATA.get_or_init(|| {
        let mut reader = Reader {
            data: CJK_BLOB,
            position: 0,
        };
        assert_eq!(reader.take(6), b"CNCJK1", "corrupted CJK tables");
        let mut data = CjkData {
            codecs: HashMap::new(),
            iso2022: HashMap::new(),
            gb18030_ranges: Vec::new(),
            pairs: Vec::new(),
        };
        for _ in 0..reader.u16() {
            let length = reader.u8() as usize;
            let name = std::str::from_utf8(reader.take(length)).unwrap_or_default();
            let size = reader.u32() as usize;
            let mut section = Reader {
                data: reader.take(size),
                position: 0,
            };
            if let Some(&codec) = CJK_NAMES.iter().find(|value| **value == name) {
                let mut need = [0u8; 256];
                need.copy_from_slice(section.take(256));
                let mut single = [NONE; 256];
                for value in single.iter_mut() {
                    *value = section.u24();
                }
                let double = section.rows();
                let triple_prefix = section.u8();
                let triple = section.rows();
                let ascii_identity =
                    (0..0x80).all(|byte| need[byte] == 1 && single[byte] == byte as u32);
                data.codecs.insert(
                    codec,
                    CjkCodec {
                        ascii_identity,
                        need,
                        single,
                        double,
                        triple_prefix,
                        triple,
                    },
                );
            } else if name == "gb18030_ranges" {
                for _ in 0..section.u16() {
                    let index = section.u32();
                    let codepoint = section.u32();
                    data.gb18030_ranges.push((index, codepoint));
                }
            } else if name == "pairs" {
                for _ in 0..section.u16() {
                    let first = char::from_u32(section.u32()).unwrap_or('\u{fffd}');
                    let second = char::from_u32(section.u32()).unwrap_or('\u{fffd}');
                    data.pairs.push((first, second));
                }
            } else {
                let name: &'static str = match name {
                    "jisx0208" => "jisx0208",
                    "jisx0212" => "jisx0212",
                    "ksx1001" => "ksx1001",
                    "gb2312_7bit" => "gb2312_7bit",
                    "jisx0201_r" => "jisx0201_r",
                    "jisx0201_k" => "jisx0201_k",
                    "jisx0213_2000_1" => "jisx0213_2000_1",
                    "jisx0213_2000_2" => "jisx0213_2000_2",
                    "jisx0213_2004_1" => "jisx0213_2004_1",
                    "jisx0213_2004_2" => "jisx0213_2004_2",
                    _ => continue,
                };
                data.iso2022.insert(name, section.rows());
            }
        }
        data
    })
}

#[inline]
fn push_value(out: &mut String, value: u32, pairs: &[(char, char)]) -> Result<(), DecodeError> {
    if value >= PAIR_BASE {
        let (first, second) = pairs
            .get((value - PAIR_BASE) as usize)
            .ok_or(DecodeError::Invalid)?;
        out.push(*first);
        out.push(*second);
    } else {
        out.push(char::from_u32(value).ok_or(DecodeError::Invalid)?);
    }
    Ok(())
}

const CGK2U_CHOSEONG: [u8; 30] = [
    0, 1, 127, 2, 127, 127, 3, 4, 5, 127, 127, 127, 127, 127, 127, 127, 6, 7, 8, 127, 9, 10, 11,
    12, 13, 14, 15, 16, 17, 18,
];
const CGK2U_JONGSEONG: [u8; 30] = [
    1, 2, 3, 4, 5, 6, 7, 127, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 127, 18, 19, 20, 21, 22, 127,
    23, 24, 25, 26, 27,
];

/// KS X 1001:1998 Annex 3 make-up sequence (`A4 D4 A4 xx A4 xx A4 xx`).
fn euc_kr_makeup(data: &[u8]) -> Option<char> {
    if data[2] != 0xA4 || data[4] != 0xA4 || data[6] != 0xA4 {
        return None;
    }
    let choseong = match data[3] {
        value @ 0xA1..=0xBE => CGK2U_CHOSEONG[(value - 0xA1) as usize],
        _ => 127,
    };
    let jungseong = match data[5] {
        value @ 0xBF..=0xD3 => value - 0xBF,
        _ => 127,
    };
    let jongseong = match data[7] {
        0xD4 => 0,
        value @ 0xA1..=0xBE => CGK2U_JONGSEONG[(value - 0xA1) as usize],
        _ => 127,
    };
    if choseong == 127 || jungseong == 127 || jongseong == 127 {
        return None;
    }
    char::from_u32(0xAC00 + choseong as u32 * 588 + jungseong as u32 * 28 + jongseong as u32)
}

enum Step {
    /// Consumed this many bytes.
    Ok(usize),
    /// Invalid sequence of this many bytes.
    Error(usize),
    /// Not enough input left for the sequence.
    Incomplete,
}

fn gb18030_four_byte(data: &[u8], out: &mut String, ranges: &[(u32, u32)]) -> Step {
    if data.len() < 4 {
        return Step::Incomplete;
    }
    let (c1, c2, c3, c4) = (data[0], data[1], data[2], data[3]);
    if !(0x81..=0xFE).contains(&c1) || !(0x81..=0xFE).contains(&c3) || !(0x30..=0x39).contains(&c4)
    {
        return Step::Error(1);
    }
    let (c1, c2, c3, c4) = (
        (c1 - 0x81) as u32,
        (c2 - 0x30) as u32,
        (c3 - 0x81) as u32,
        (c4 - 0x30) as u32,
    );
    if c1 < 4 {
        let index = (c1 * 10 + c2) * 1260 + c3 * 10 + c4;
        if index < 39420 {
            let run = ranges.partition_point(|entry| entry.0 <= index) - 1;
            let (base, first) = ranges[run];
            if let Some(character) = char::from_u32(first + index - base) {
                out.push(character);
                return Step::Ok(4);
            }
        }
    } else if c1 >= 15 {
        let value = 0x10000 + ((c1 - 15) * 10 + c2) * 1260 + c3 * 10 + c4;
        if let Some(character) = char::from_u32(value) {
            out.push(character);
            return Step::Ok(4);
        }
    }
    Step::Error(1)
}

fn table_step(
    value: Option<u32>,
    width: usize,
    out: &mut String,
    pairs: &[(char, char)],
) -> Result<Step, DecodeError> {
    Ok(match value {
        Some(value) if value >= ERROR_BASE => Step::Error((value - ERROR_BASE) as usize),
        Some(value) => {
            push_value(out, value, pairs)?;
            Step::Ok(width)
        }
        None => Step::Error(1),
    })
}

fn decode_cjk(codec: &CjkCodec, data: &[u8], errors: Errors) -> Result<String, DecodeError> {
    let tables = cjk();
    let is_euc_kr = std::ptr::eq(codec, tables.codec("euc_kr"));
    let is_gb18030 = std::ptr::eq(codec, tables.codec("gb18030"));
    let mut out = String::with_capacity(data.len() * 2);
    let mut position = 0usize;
    while position < data.len() {
        if codec.ascii_identity {
            let run = ascii_prefix(&data[position..]);
            push_ascii(&mut out, &data[position..position + run]);
            position += run;
            if position == data.len() {
                break;
            }
        }
        let rest = &data[position..];
        let lead = rest[0];
        let step = match codec.need[lead as usize] {
            1 => {
                push_value(&mut out, codec.single[lead as usize], &tables.pairs)?;
                Step::Ok(1)
            }
            2 if rest.len() < 2 => Step::Incomplete,
            2 if is_euc_kr && lead == 0xA4 && rest[1] == 0xD4 => {
                if rest.len() < 8 {
                    Step::Incomplete
                } else if let Some(character) = euc_kr_makeup(rest) {
                    out.push(character);
                    Step::Ok(8)
                } else {
                    Step::Error(1)
                }
            }
            2 if is_gb18030 && (0x30..=0x39).contains(&rest[1]) => {
                gb18030_four_byte(rest, &mut out, &tables.gb18030_ranges)
            }
            2 => table_step(codec.double.get(lead, rest[1]), 2, &mut out, &tables.pairs)?,
            3 if rest.len() < 3 => Step::Incomplete,
            3 if lead == codec.triple_prefix => table_step(
                codec.triple.get(rest[1], rest[2]),
                3,
                &mut out,
                &tables.pairs,
            )?,
            _ => Step::Error(1),
        };
        match step {
            Step::Ok(length) => position += length,
            Step::Error(_) | Step::Incomplete if errors == Errors::Strict => {
                return Err(DecodeError::Invalid)
            }
            Step::Error(length) => position += length,
            Step::Incomplete => break,
        }
    }
    Ok(out)
}

/* ---------------------------------------------------------------------- */
/* HZ and ISO-2022 (ports of CPython's stateful decoders)                  */
/* ---------------------------------------------------------------------- */

fn decode_hz(data: &[u8], errors: Errors) -> Result<String, DecodeError> {
    let tables = cjk();
    let gb2312 = &tables.iso2022["gb2312_7bit"];
    let mut out = String::with_capacity(data.len());
    let mut position = 0usize;
    let mut gb_mode = false;
    while position < data.len() {
        let rest = &data[position..];
        let byte = rest[0];
        let step = if byte == b'~' {
            if rest.len() < 2 {
                Step::Incomplete
            } else {
                match (rest[1], gb_mode) {
                    (b'~', false) => {
                        out.push('~');
                        Step::Ok(2)
                    }
                    (b'{', false) => {
                        gb_mode = true;
                        Step::Ok(2)
                    }
                    (b'\n', false) => Step::Ok(2),
                    (b'}', true) => {
                        gb_mode = false;
                        Step::Ok(2)
                    }
                    _ => Step::Error(1),
                }
            }
        } else if byte & 0x80 != 0 {
            Step::Error(1)
        } else if !gb_mode {
            out.push(byte as char);
            Step::Ok(1)
        } else if rest.len() < 2 {
            Step::Incomplete
        } else {
            match gb2312.get(byte, rest[1]) {
                Some(value) => {
                    push_value(&mut out, value, &tables.pairs)?;
                    Step::Ok(2)
                }
                None => Step::Error(1),
            }
        };
        match step {
            Step::Ok(length) => position += length,
            Step::Error(_) | Step::Incomplete if errors == Errors::Strict => {
                return Err(DecodeError::Invalid)
            }
            Step::Error(length) => position += length,
            Step::Incomplete => break,
        }
    }
    Ok(out)
}

const DBCS: u8 = 0x80;
const CHARSET_ASCII: u8 = b'B';
const CHARSET_ISO8859_1: u8 = b'A';
const CHARSET_ISO8859_7: u8 = b'F';

#[derive(Clone, Copy)]
enum Designation {
    /// Double-byte charset decoded through an ISO-2022 table.
    Double(&'static str),
    /// Single-byte charset decoded through an ISO-2022 table.
    Single(&'static str),
    /// Charsets only usable through single shifts (G2); direct use fails.
    Dummy,
}

struct Iso2022Config {
    no_shift: bool,
    use_g2: bool,
    jisx0208_ext: bool,
    designations: &'static [(u8, Designation)],
}

const JISX0208: (u8, Designation) = (b'B' | DBCS, Designation::Double("jisx0208"));
const JISX0208_O: (u8, Designation) = (b'@' | DBCS, Designation::Double("jisx0208"));
const JISX0212: (u8, Designation) = (b'D' | DBCS, Designation::Double("jisx0212"));
const KSX1001: (u8, Designation) = (b'C' | DBCS, Designation::Double("ksx1001"));
const GB2312: (u8, Designation) = (b'A' | DBCS, Designation::Double("gb2312_7bit"));
const JISX0201_R: (u8, Designation) = (b'J', Designation::Single("jisx0201_r"));
const JISX0201_K: (u8, Designation) = (b'I', Designation::Single("jisx0201_k"));
const ISO8859_1: (u8, Designation) = (CHARSET_ISO8859_1, Designation::Dummy);
const ISO8859_7: (u8, Designation) = (CHARSET_ISO8859_7, Designation::Dummy);

fn iso2022_config(variant: Iso2022Variant) -> Iso2022Config {
    match variant {
        Iso2022Variant::Kr => Iso2022Config {
            no_shift: false,
            use_g2: false,
            jisx0208_ext: false,
            designations: &[KSX1001],
        },
        Iso2022Variant::Jp => Iso2022Config {
            no_shift: true,
            use_g2: false,
            jisx0208_ext: true,
            designations: &[JISX0208, JISX0201_R, JISX0208_O],
        },
        Iso2022Variant::Jp1 => Iso2022Config {
            no_shift: true,
            use_g2: false,
            jisx0208_ext: true,
            designations: &[JISX0208, JISX0212, JISX0201_R, JISX0208_O],
        },
        Iso2022Variant::Jp2 => Iso2022Config {
            no_shift: true,
            use_g2: true,
            jisx0208_ext: true,
            designations: &[
                JISX0208, JISX0212, KSX1001, GB2312, JISX0201_R, JISX0208_O, ISO8859_1, ISO8859_7,
            ],
        },
        Iso2022Variant::Jp2004 => Iso2022Config {
            no_shift: true,
            use_g2: false,
            jisx0208_ext: true,
            designations: &[
                (b'Q' | DBCS, Designation::Double("jisx0213_2004_1")),
                JISX0208,
                (b'P' | DBCS, Designation::Double("jisx0213_2004_2")),
            ],
        },
        Iso2022Variant::Jp3 => Iso2022Config {
            no_shift: true,
            use_g2: false,
            jisx0208_ext: true,
            designations: &[
                (b'O' | DBCS, Designation::Double("jisx0213_2000_1")),
                JISX0208,
                (b'P' | DBCS, Designation::Double("jisx0213_2000_2")),
            ],
        },
        Iso2022Variant::JpExt => Iso2022Config {
            no_shift: true,
            use_g2: false,
            jisx0208_ext: true,
            designations: &[JISX0208, JISX0212, JISX0201_R, JISX0201_K, JISX0208_O],
        },
    }
}

fn is_escape_end(byte: u8) -> bool {
    byte.is_ascii_uppercase() || byte == b'@'
}

/// `iso2022processesc`: returns the designation to apply or the error step.
fn iso2022_escape(config: &Iso2022Config, data: &[u8]) -> Result<(usize, usize, u8), Step> {
    let mut escape_length = 0usize;
    let mut index = 1usize;
    while index < 16 {
        if index >= data.len() {
            return Err(Step::Incomplete);
        }
        if is_escape_end(data[index]) {
            escape_length = index + 1;
            break;
        } else if config.jisx0208_ext
            && index + 1 < data.len()
            && data[index] == b'&'
            && data[index + 1] == b'@'
        {
            index += 2;
        }
        index += 1;
    }
    let (charset, designation) = match escape_length {
        0 => return Err(Step::Error(1)),
        3 => {
            if data[1] == b'$' {
                (data[2] | DBCS, 0)
            } else {
                let designation = match data[1] {
                    b'(' => 0,
                    b')' => 1,
                    b'.' if config.use_g2 => 2,
                    _ => return Err(Step::Error(3)),
                };
                (data[2], designation)
            }
        }
        4 => {
            if data[1] != b'$' {
                return Err(Step::Error(4));
            }
            let designation = match data[2] {
                b'(' => 0,
                b')' => 1,
                _ => return Err(Step::Error(4)),
            };
            (data[3] | DBCS, designation)
        }
        6 => {
            if config.jisx0208_ext && data[3] == 0x1B && data[4] == b'$' && data[5] == b'B' {
                (b'B' | DBCS, 0)
            } else {
                return Err(Step::Error(6));
            }
        }
        length => return Err(Step::Error(length)),
    };
    if charset != CHARSET_ASCII && !config.designations.iter().any(|(mark, _)| *mark == charset) {
        return Err(Step::Error(escape_length));
    }
    Ok((escape_length, designation, charset))
}

fn iso8859_7_decode(byte: u8) -> Option<char> {
    let c = byte as u32;
    let value = if c < 0xA0 || (c < 0xC0 && (0x288f_3bc9u32 & (1u32 << (c - 0xA0))) != 0) {
        c
    } else if (0xB4..=0xFE).contains(&c)
        && (c >= 0xD4 || (0xbfff_fd77u32 & (1u32 << (c - 0xB4))) != 0)
    {
        0x02D0 + c
    } else if c == 0xA1 {
        0x2018
    } else if c == 0xA2 {
        0x2019
    } else if c == 0xAF {
        0x2015
    } else {
        return None;
    };
    char::from_u32(value)
}

fn decode_iso2022(
    variant: Iso2022Variant,
    data: &[u8],
    errors: Errors,
) -> Result<String, DecodeError> {
    const ESC: u8 = 0x1B;
    const SO: u8 = 0x0E;
    const SI: u8 = 0x0F;
    const LF: u8 = 0x0A;

    let tables = cjk();
    let config = iso2022_config(variant);
    let mut out = String::with_capacity(data.len());
    let mut g = [CHARSET_ASCII; 4];
    let mut shifted = false;
    let mut escape_throughout = false;
    let mut position = 0usize;

    while position < data.len() {
        let rest = &data[position..];
        let byte = rest[0];
        if escape_throughout {
            out.push(byte as char); // assume ISO-8859-1
            position += 1;
            if is_escape_end(byte) {
                escape_throughout = false;
            }
            continue;
        }
        let bypass = |out: &mut String| {
            out.push(byte as char);
            Step::Ok(1)
        };
        let step = match byte {
            ESC if rest.len() < 2 => Step::Incomplete,
            ESC if matches!(rest[1], b'(' | b')' | b'$' | b'.' | b'&') => {
                match iso2022_escape(&config, rest) {
                    Ok((length, designation, charset)) => {
                        g[designation] = charset;
                        Step::Ok(length)
                    }
                    Err(step) => step,
                }
            }
            ESC if config.use_g2 && rest[1] == b'N' => {
                if rest.len() < 3 {
                    Step::Incomplete
                } else {
                    let value = rest[2];
                    let decoded = match g[2] {
                        CHARSET_ISO8859_1 => (value < 0x80).then(|| (value + 0x80) as char),
                        CHARSET_ISO8859_7 => iso8859_7_decode(value ^ 0x80),
                        CHARSET_ASCII => (value & 0x80 == 0).then_some(value as char),
                        _ => None, // CPython raises an internal codec error
                    };
                    match decoded {
                        Some(character) => {
                            out.push(character);
                            Step::Ok(3)
                        }
                        None => Step::Error(3),
                    }
                }
            }
            ESC => {
                out.push(ESC as char);
                escape_throughout = true;
                Step::Ok(1)
            }
            SI if config.no_shift => bypass(&mut out),
            SI => {
                shifted = false;
                Step::Ok(1)
            }
            SO if config.no_shift => bypass(&mut out),
            SO => {
                shifted = true;
                Step::Ok(1)
            }
            LF => {
                shifted = false;
                out.push('\n');
                Step::Ok(1)
            }
            _ if byte < 0x20 => bypass(&mut out),
            _ if byte >= 0x80 => Step::Error(1),
            _ => {
                let charset = if shifted { g[1] } else { g[0] };
                if charset == CHARSET_ASCII {
                    bypass(&mut out)
                } else {
                    let designation = config
                        .designations
                        .iter()
                        .find(|(mark, _)| *mark == charset)
                        .map(|(_, designation)| *designation)
                        .unwrap_or(Designation::Dummy);
                    match designation {
                        Designation::Double(table) => {
                            if rest.len() < 2 {
                                Step::Incomplete
                            } else {
                                match tables.iso2022[table].get(rest[0], rest[1]) {
                                    Some(value) => {
                                        push_value(&mut out, value, &tables.pairs)?;
                                        Step::Ok(2)
                                    }
                                    None => Step::Error(2),
                                }
                            }
                        }
                        Designation::Single(table) => match tables.iso2022[table].get(0, byte) {
                            Some(value) => {
                                push_value(&mut out, value, &tables.pairs)?;
                                Step::Ok(1)
                            }
                            None => Step::Error(1),
                        },
                        Designation::Dummy => Step::Error(1),
                    }
                }
            }
        };
        match step {
            Step::Ok(length) => position += length,
            Step::Error(_) | Step::Incomplete if errors == Errors::Strict => {
                return Err(DecodeError::Invalid)
            }
            Step::Error(length) => position += length,
            Step::Incomplete => break,
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf_family() {
        assert_eq!(
            decode(b"caf\xc3\xa9", "utf_8", Errors::Strict).unwrap(),
            "café"
        );
        assert!(decode(b"\xc3", "utf_8", Errors::Strict).is_err());
        assert_eq!(decode(b"a\xffb", "utf_8", Errors::Ignore).unwrap(), "ab");
        assert_eq!(
            decode(b"\xff\xfea\x00", "utf_16", Errors::Strict).unwrap(),
            "a"
        );
        assert_eq!(decode(b"\x00a", "utf_16_be", Errors::Strict).unwrap(), "a");
        assert_eq!(
            decode(b"+AGEAYgBj-", "utf_7", Errors::Strict).unwrap(),
            "abc"
        );
    }

    #[test]
    fn cjk_codecs() {
        assert_eq!(decode(b"\xa4\xa4", "big5", Errors::Strict).unwrap(), "中");
        assert_eq!(
            decode(b"\x1b$B%F%9%H\x1b(B", "iso2022_jp", Errors::Strict).unwrap(),
            "テスト"
        );
        assert_eq!(decode(b"~{VP~}", "hz", Errors::Strict).unwrap(), "中");
        assert_eq!(
            decode(b"\x81\x30\x81\x30", "gb18030", Errors::Strict).unwrap(),
            "\u{80}"
        );
        assert!(decode(b"\xa4", "big5", Errors::Strict).is_err());
    }

    #[test]
    fn aliases_resolve() {
        assert!(is_known("UTF-8"));
        assert!(is_known("latin-1"));
        assert!(is_known("windows-1252"));
        assert!(!is_known("not-a-codec"));
    }
}

//! Native implementations of the CPython codecs charset-normalizer probes.
//!
//! Decoding must match CPython bit for bit: the detector's verdict depends on
//! which byte sequences a codec rejects. Unicode transformation formats are
//! implemented directly, single-byte code pages and CJK codecs are driven by
//! tables generated from CPython (see `bin/generate_native_tables.py`), and
//! the stateful ISO-2022 / HZ / UTF-7 decoders are ports of CPython's.

//!
//! Codec names follow CPython (`"cp1252"`, `"shift_jis"`, ...); aliases such
//! as `"windows-1252"` or `"UTF-8"` are accepted too.
//!
//! ```
//! use charset_norm::codecs::{decode, encode, Errors};
//!
//! let bytes = encode("Grüße", "cp1252").unwrap();
//! assert_eq!(bytes, b"Gr\xfc\xdfe");
//! assert_eq!(decode(&bytes, "windows-1252", Errors::Strict).unwrap(), "Grüße");
//! ```

mod cjk;
mod iso2022;
mod single_byte;
mod utf;

use std::collections::HashMap;
use std::fmt;

use crate::tables::{iana_lookup, SINGLE_BYTE_CODECS};

use cjk::{cjk, decode_cjk, CJK_NAMES};
use iso2022::{decode_hz, decode_iso2022};
use single_byte::{decode_ascii, decode_single_byte, single_byte_chars};
use utf::{decode_utf16, decode_utf32, decode_utf7, decode_utf8};

/// How decoding handles invalid input.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Errors {
    /// Fail on the first invalid sequence.
    Strict,
    /// Skip invalid sequences, as CPython's `errors="ignore"` does.
    Ignore,
}

/// Why decoding failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// No native implementation for this codec name.
    Unknown,
    /// The payload is not valid for the codec (strict mode).
    Invalid,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Unknown => f.write_str("unknown encoding"),
            DecodeError::Invalid => f.write_str("payload is not valid for the encoding"),
        }
    }
}

impl std::error::Error for DecodeError {}

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

/// Whether a native codec exists for `encoding` (or one of its aliases).
pub fn is_known(encoding: &str) -> bool {
    lookup(encoding).is_some()
}

/// Decode `data` with `encoding`, exactly as CPython's `bytes.decode` would.
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

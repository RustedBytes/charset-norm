//! Code page names, signatures and the languages a code page can express.
//!
//! Names are canonical `CPython` codec names (`"cp1252"`, `"iso8859_5"`,
//! `"shift_jis"`). [`iana_name`] resolves aliases to them, and
//! [`supported_encodings`] lists every code page the detector knows.
//!
//! ```
//! use charset_norm::encoding;
//!
//! assert_eq!(encoding::iana_name("Windows-1251", true).unwrap(), "cp1251");
//! assert!(encoding::is_multi_byte_encoding("gb18030"));
//! assert!(encoding::encoding_languages("cp1251").unwrap().contains(&"Russian"));
//!
//! // Byte order marks and in-document declarations.
//! let (bom, mark) = encoding::identify_sig_or_bom(b"\xef\xbb\xbfhello");
//! assert_eq!((bom, mark), (Some("utf_8"), &b"\xef\xbb\xbf"[..]));
//! assert_eq!(encoding::any_specified_encoding(b"# -*- coding: latin-1 -*-", 4096), Some("latin_1"));
//! ```

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

use std::fmt::Write as _;

use crate::Error;
use crate::codecs;
use crate::stackfmt::StackStr;
use crate::tables::{self, ENCODING_ALIASES, IANA_SUPPORTED_MB_FIRST, KO_NAMES, ZH_NAMES};
use crate::unicode;

/// Every code page the detector tries, multi-byte ones first (the order used
/// during detection).
#[must_use]
pub fn supported_encodings() -> &'static [&'static str] {
    IANA_SUPPORTED_MB_FIRST
}

/// Resolve a code page name or alias to its canonical (`CPython`) name.
///
/// With `strict`, unknown names are an error; otherwise the normalized name
/// (lowercase, `-` replaced by `_`) is returned as is.
///
/// ```
/// assert_eq!(charset_norm::encoding::iana_name("UTF-8", true).unwrap(), "utf_8");
/// assert_eq!(charset_norm::encoding::iana_name("windows-1252", true).unwrap(), "cp1252");
/// ```
///
/// # Errors
///
/// [`Error::UnknownEncoding`] for an unknown name when `strict` is set.
pub fn iana_name(name: &str, strict: bool) -> Result<String, Error> {
    let normalized = name.to_lowercase().replace('-', "_");
    if let Some(value) = tables::iana_lookup(&normalized) {
        return Ok(value.to_owned());
    }
    if strict {
        return Err(Error::UnknownEncoding(normalized));
    }
    Ok(normalized)
}

/// Other names `CPython` knows the canonical `encoding` by.
#[must_use]
pub fn aliases(encoding: &str) -> Vec<&'static str> {
    let mut known = Vec::new();
    for &(alias, canonical) in ENCODING_ALIASES {
        if encoding == alias {
            known.push(canonical);
        } else if encoding == canonical {
            known.push(alias);
        }
    }
    known
}

/// Whether decoding `encoding` involves multi-byte sequences.
#[must_use]
pub fn is_multi_byte_encoding(encoding: &str) -> bool {
    tables::is_multi_byte_encoding(encoding)
}

/// Whether two single-byte code pages are near-identical.
#[must_use]
pub fn is_cp_similar(encoding_a: &str, encoding_b: &str) -> bool {
    tables::similar_encodings(encoding_a).contains(&encoding_b)
}

pub(crate) fn similar_encodings(encoding: &str) -> &'static [&'static str] {
    tables::similar_encodings(encoding)
}

/// Share of the 256 byte values two single-byte code pages decode alike.
///
/// # Errors
///
/// [`Error::UnknownEncoding`] when either name is not a supported code page.
pub fn cp_similarity(encoding_a: &str, encoding_b: &str) -> Result<f64, Error> {
    if is_multi_byte_encoding(encoding_a) || is_multi_byte_encoding(encoding_b) {
        return Ok(0.0);
    }
    let decoder_a = single_byte_decoder(encoding_a)?;
    let decoder_b = single_byte_decoder(encoding_b)?;
    let matches = (0u8..=255)
        .filter(|&byte| decoder_a(byte) == decoder_b(byte))
        .count();
    Ok(matches as f64 / 256.0)
}

fn single_byte_decoder(encoding: &str) -> Result<impl Fn(u8) -> Option<char> + use<>, Error> {
    codecs::single_byte_decoder(encoding).ok_or_else(|| Error::UnknownEncoding(encoding.to_owned()))
}

/// Whether a detected signature/BOM should be removed before decoding.
#[must_use]
pub fn should_strip_sig_or_bom(encoding: &str) -> bool {
    encoding != "utf_16" && encoding != "utf_32"
}

/// Identify a byte order mark or signature at the start of `payload`,
/// returning the encoding it announces and the mark itself.
///
/// ```
/// let (encoding, mark) = charset_norm::encoding::identify_sig_or_bom(b"\xef\xbb\xbfhi");
/// assert_eq!(encoding, Some("utf_8"));
/// assert_eq!(mark, b"\xef\xbb\xbf");
/// ```
#[must_use]
pub fn identify_sig_or_bom(payload: &[u8]) -> (Option<&'static str>, &'static [u8]) {
    const MARKS: [(&str, &[u8]); 10] = [
        ("utf_8", b"\xef\xbb\xbf"),
        ("utf_7", b"\x2b\x2f\x76\x38"),
        ("utf_7", b"\x2b\x2f\x76\x39"),
        ("utf_7", b"\x2b\x2f\x76\x2b"),
        ("utf_7", b"\x2b\x2f\x76\x2f"),
        ("gb18030", b"\x84\x31\x95\x33"),
        ("utf_32", b"\x00\x00\xfe\xff"),
        ("utf_32", b"\xff\xfe\x00\x00"),
        ("utf_16", b"\xfe\xff"),
        ("utf_16", b"\xff\xfe"),
    ];
    MARKS
        .iter()
        .find(|(_, mark)| payload.starts_with(mark))
        .map_or((None, b""), |(encoding, mark)| (Some(*encoding), *mark))
}

pub(crate) fn encoding_indication() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r#"(?i)(?:(?:encoding)|(?:charset)|(?:coding))(?:[:= ]{1,10})(?:["']?)([a-zA-Z0-9\-_]+)(?:["']?)"#,
        )
        .expect("valid encoding indication pattern")
    })
}

/// Look for an encoding declared in the first `search_zone` bytes, such as
/// `<meta charset="...">` or a `# coding: ...` comment.
///
/// ```
/// let html = br#"<meta charset="windows-1252">"#;
/// assert_eq!(charset_norm::encoding::any_specified_encoding(html, 8192), Some("cp1252"));
/// ```
#[must_use]
pub fn any_specified_encoding(payload: &[u8], search_zone: usize) -> Option<&'static str> {
    let search = &payload[..payload.len().min(search_zone)];
    if !search
        .windows(6)
        .any(|part| part.eq_ignore_ascii_case(b"coding"))
        && !search
            .windows(7)
            .any(|part| part.eq_ignore_ascii_case(b"charset"))
    {
        return None;
    }
    // Non-ASCII bytes are dropped before matching, as the reference does.
    let filtered: String;
    let decoded: &str = if search.is_ascii() {
        std::str::from_utf8(search).unwrap_or_default()
    } else {
        filtered = search
            .iter()
            .filter(|byte| byte.is_ascii())
            .map(|&byte| char::from(byte))
            .collect();
        &filtered
    };
    let regex = encoding_indication();
    let mut locations = regex.capture_locations();
    let mut start = 0;
    while let Some(found) = regex.captures_read_at(&mut locations, decoded, start) {
        start = found.end();
        let Some((from, to)) = locations.get(1) else {
            continue;
        };
        // Code page names are short; longer candidates cannot be known.
        let mut normalized = StackStr::<64>::new();
        let fits = decoded[from..to].chars().all(|character| {
            let character = if character == '-' {
                '_'
            } else {
                character.to_ascii_lowercase()
            };
            normalized.write_char(character).is_ok()
        });
        if let Some(name) = fits
            .then(|| tables::iana_lookup(normalized.as_str()))
            .flatten()
        {
            return Some(name);
        }
    }
    None
}

/// Languages associated with a multi-byte (CJK) code page.
#[must_use]
pub fn mb_encoding_languages(encoding: &str) -> &'static [&'static str] {
    if encoding.starts_with("shift_")
        || encoding.starts_with("iso2022_jp")
        || encoding.starts_with("euc_j")
        || encoding == "cp932"
    {
        return &["Japanese"];
    }
    if encoding.starts_with("gb") || ZH_NAMES.contains(&encoding) {
        return &["Chinese"];
    }
    if encoding.starts_with("iso2022_kr") || KO_NAMES.contains(&encoding) {
        return &["Korean"];
    }
    &[]
}

/// Unicode ranges a single-byte code page mostly decodes into.
///
/// # Errors
///
/// [`Error::MultiByteEncoding`] for multi-byte encodings and
/// [`Error::UnknownEncoding`] for unsupported code pages.
pub fn encoding_unicode_range(encoding: &str) -> Result<Vec<&'static str>, Error> {
    if is_multi_byte_encoding(encoding) {
        return Err(Error::MultiByteEncoding(encoding.to_owned()));
    }
    let decoder = single_byte_decoder(encoding)?;
    let mut order = Vec::<&'static str>::new();
    let mut counts = HashMap::<&'static str, usize>::new();
    let mut character_count = 0usize;
    for byte in 0x40u8..0xffu8 {
        let Some(character) = decoder(byte) else {
            continue;
        };
        let Some(range) = unicode::range_of(character) else {
            continue;
        };
        if range.secondary {
            continue;
        }
        let count = counts.entry(range.name).or_default();
        if *count == 0 {
            order.push(range.name);
        }
        *count += 1;
        character_count += 1;
    }
    let mut result: Vec<&'static str> = order
        .into_iter()
        .filter(|range| counts[range] as f64 / character_count as f64 >= 0.15)
        .collect();
    result.sort_unstable();
    Ok(result)
}

/// Languages whose alphabet has characters in `primary_range`.
#[must_use]
pub fn unicode_range_languages(primary_range: &str) -> Vec<&'static str> {
    tables::languages()
        .iter()
        .filter(|language| {
            language
                .characters
                .iter()
                .any(|&character| unicode::unicode_range(character) == Some(primary_range))
        })
        .map(|language| language.name)
        .collect()
}

/// Languages a single-byte code page can express (`["Latin Based"]` for
/// Latin-only code pages). Unknown code pages yield no language.
///
/// # Errors
///
/// [`Error::MultiByteEncoding`] for multi-byte encodings.
pub fn encoding_languages(encoding: &str) -> Result<&'static [&'static str], Error> {
    if is_multi_byte_encoding(encoding) {
        return Err(Error::MultiByteEncoding(encoding.to_owned()));
    }
    Ok(single_byte_languages(encoding))
}

/// [`encoding_languages`] for single-byte code pages, computed once for
/// every native code page.
pub(crate) fn single_byte_languages(encoding: &str) -> &'static [&'static str] {
    static TABLE: OnceLock<Vec<(&'static str, Vec<&'static str>)>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table: Vec<(&'static str, Vec<&'static str>)> = codecs::single_byte_names()
            .map(|name| {
                let languages = match encoding_unicode_range(name) {
                    Err(_) => Vec::new(),
                    Ok(ranges) => match ranges.iter().find(|range| !range.contains("Latin")) {
                        None => vec!["Latin Based"],
                        Some(primary) => unicode_range_languages(primary),
                    },
                };
                (name, languages)
            })
            .collect();
        table.sort_unstable_by_key(|entry| entry.0);
        table
    });
    codecs::single_byte_name(encoding)
        .and_then(|name| {
            table
                .binary_search_by_key(&name, |entry| entry.0)
                .ok()
                .map(|index| table[index].1.as_slice())
        })
        .unwrap_or(&[])
}

/// Languages an encoding can express, whether single- or multi-byte.
pub(crate) fn target_languages(encoding: &str) -> &'static [&'static str] {
    if is_multi_byte_encoding(encoding) {
        mb_encoding_languages(encoding)
    } else {
        single_byte_languages(encoding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_marks() {
        assert_eq!(iana_name("not-real", false).unwrap(), "not_real");
        assert!(iana_name("not-real", true).is_err());
        assert!(is_cp_similar("latin_1", "cp1252"));
        assert_eq!(any_specified_encoding(b"plain", 8192), None);
        assert_eq!(mb_encoding_languages("shift_jis"), ["Japanese"]);
        assert!(encoding_languages("cp1251").unwrap().contains(&"Russian"));
    }
}

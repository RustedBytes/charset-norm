use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use pyo3::exceptions::{
    PyImportError, PyKeyError, PyLookupError, PyOSError, PyTypeError, PyUnicodeDecodeError,
    PyValueError, PyZeroDivisionError,
};
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyAny, PyByteArray, PyBytes};
use regex::Regex;
use rustc_hash::FxHashMap;

/// Detection allocates many short-lived strings; mimalloc handles that
/// pattern noticeably faster than the system allocators.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod api;
mod codecs;
mod mess;
mod models;
mod tables;
mod unicode;

use codecs::{DecodeError, Errors};
use tables::{Language, KO_NAMES, ZH_NAMES};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/* ---------------------------------------------------------------------- */
/* Shared helpers                                                          */
/* ---------------------------------------------------------------------- */

/// `round(value, digits)` with CPython's correctly-rounded semantics.
pub(crate) fn py_round(value: f64, digits: usize) -> f64 {
    if !value.is_finite() {
        return value;
    }
    const POWERS: [f64; 9] = [1.0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8];
    if digits < POWERS.len() {
        // Below 1e9 the scaled product is within 1.2e-7 of the exact value, so
        // away from a decimal tie it selects the same integer k as CPython's
        // correctly-rounded path; k / 10^d is then that decimal's nearest double.
        let scale = POWERS[digits];
        let scaled = value * scale;
        let fraction = scaled - scaled.floor();
        if scaled.abs() < 1e9 && (fraction - 0.5).abs() > 1e-6 {
            return scaled.round() / scale;
        }
    }
    format!("{value:.digits$}").parse().unwrap_or(value)
}

/// `sum()` over floats as CPython 3.12+ computes it (Neumaier compensation).
pub(crate) fn py_sum(values: &[f64]) -> f64 {
    let mut total = 0.0f64;
    let mut compensation = 0.0f64;
    for &value in values {
        let next = total + value;
        if total.abs() >= value.abs() {
            compensation += (total - next) + value;
        } else {
            compensation += (value - next) + total;
        }
        total = next;
    }
    if compensation != 0.0 && compensation.is_finite() {
        total += compensation;
    }
    total
}

pub(crate) const DEBUG: i32 = 10;

/// Emit a record on the `charset_normalizer` logger when the level is enabled.
/// Logging is the one Python facility kept: users observe it through handlers.
pub(crate) fn log(py: Python<'_>, level: i32, message: impl FnOnce() -> String) -> PyResult<()> {
    static LOGGER: PyOnceLock<Py<PyAny>> = PyOnceLock::new();
    let logger = LOGGER.get_or_try_init(py, || {
        py.import("logging")?
            .getattr("getLogger")?
            .call1(("charset_normalizer",))
            .map(Bound::unbind)
    })?;
    let logger = logger.bind(py);
    if logger.call_method1("isEnabledFor", (level,))?.is_truthy()? {
        logger.call_method1("log", (level, message()))?;
    }
    Ok(())
}

fn one_char(character: &str) -> PyResult<char> {
    let mut chars = character.chars();
    let Some(value) = chars.next() else {
        return Err(PyTypeError::new_err(
            "ord() expected a character, but string of length 0 found",
        ));
    };
    if chars.next().is_some() {
        return Err(PyTypeError::new_err(format!(
            "ord() expected a character, but string of length {} found",
            character.chars().count()
        )));
    }
    Ok(value)
}

pub(crate) fn decode_error(encoding: &str, error: DecodeError) -> PyErr {
    match error {
        DecodeError::Unknown => PyLookupError::new_err(format!("unknown encoding: {encoding}")),
        DecodeError::Invalid => PyUnicodeDecodeError::new_err((
            encoding.to_owned(),
            Vec::<u8>::new(),
            0usize,
            0usize,
            "invalid data",
        )),
    }
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

fn bytes_of<'a>(sequence: &'a Bound<'_, PyAny>, owned: &'a mut Vec<u8>) -> PyResult<&'a [u8]> {
    if let Ok(value) = sequence.cast::<PyBytes>() {
        return Ok(value.as_bytes());
    }
    *owned = sequence.extract()?;
    Ok(owned)
}

fn language_or_err(name: &str) -> PyResult<&'static Language> {
    tables::language(name).ok_or_else(|| PyValueError::new_err(format!("{name} not available")))
}

/* ---------------------------------------------------------------------- */
/* Character classification (utils.py)                                     */
/* ---------------------------------------------------------------------- */

#[pyfunction]
fn character_flags(character: &str) -> PyResult<u16> {
    Ok(unicode::character_flags(one_char(character)?))
}

#[pyfunction]
fn remove_accent(character: &str) -> PyResult<String> {
    Ok(unicode::remove_accent(one_char(character)?).to_string())
}

#[pyfunction]
fn is_punctuation(character: &str) -> PyResult<bool> {
    Ok(unicode::is_punctuation(one_char(character)?))
}

#[pyfunction]
fn is_symbol(character: &str) -> PyResult<bool> {
    Ok(unicode::is_symbol(one_char(character)?))
}

#[pyfunction]
fn is_emoticon(character: &str) -> PyResult<bool> {
    Ok(unicode::is_emoticon(one_char(character)?))
}

#[pyfunction]
fn is_separator(character: &str) -> PyResult<bool> {
    Ok(unicode::is_separator(one_char(character)?))
}

#[pyfunction]
fn is_case_variable(character: &str) -> PyResult<bool> {
    Ok(unicode::is_case_variable(one_char(character)?))
}

#[pyfunction]
fn is_unprintable(character: &str) -> PyResult<bool> {
    Ok(unicode::is_unprintable(one_char(character)?))
}

#[pyfunction]
fn unicode_range(character: &str) -> PyResult<Option<&'static str>> {
    Ok(unicode::unicode_range(one_char(character)?))
}

fn range_info(name: &str) -> PyResult<&'static unicode::RangeInfo> {
    unicode::range_index(name)
        .map(|index| &unicode::ranges()[index])
        .ok_or_else(|| PyKeyError::new_err(name.to_owned()))
}

#[pyfunction]
fn is_suspiciously_successive_range(
    unicode_range_a: Option<&str>,
    unicode_range_b: Option<&str>,
) -> PyResult<bool> {
    let (Some(a), Some(b)) = (unicode_range_a, unicode_range_b) else {
        return Ok(true);
    };
    Ok(unicode::suspicious_ranges(
        Some(range_info(a)?),
        Some(range_info(b)?),
    ))
}

/* ---------------------------------------------------------------------- */
/* Encoding helpers (utils.py)                                             */
/* ---------------------------------------------------------------------- */

pub(crate) fn specified_encoding(bytes: &[u8], search_zone: usize) -> Option<&'static str> {
    let search = &bytes[..bytes.len().min(search_zone)];
    let lowered: Vec<u8> = search.iter().map(u8::to_ascii_lowercase).collect();
    if !lowered.windows(6).any(|part| part == b"coding")
        && !lowered.windows(7).any(|part| part == b"charset")
    {
        return None;
    }
    let decoded: String = search
        .iter()
        .filter(|byte| byte.is_ascii())
        .map(|&byte| byte as char)
        .collect();
    encoding_indication()
        .captures_iter(&decoded)
        .filter_map(|captures| captures.get(1))
        .find_map(|specified| {
            tables::iana_lookup(&specified.as_str().to_lowercase().replace('-', "_"))
        })
}

#[pyfunction(signature = (sequence, search_zone=8192))]
fn any_specified_encoding(
    sequence: &Bound<'_, PyAny>,
    search_zone: usize,
) -> PyResult<Option<&'static str>> {
    if !sequence.is_instance_of::<PyBytes>() && !sequence.is_instance_of::<PyByteArray>() {
        return Err(PyTypeError::new_err(""));
    }
    let mut owned = Vec::new();
    Ok(specified_encoding(
        bytes_of(sequence, &mut owned)?,
        search_zone,
    ))
}

#[pyfunction]
fn backend_name() -> &'static str {
    "rust-pyo3"
}

#[pyfunction]
fn should_strip_sig_or_bom(iana_encoding: &str) -> bool {
    iana_encoding != "utf_16" && iana_encoding != "utf_32"
}

pub(crate) fn iana_name_impl(cp_name: &str, strict: bool) -> PyResult<String> {
    let normalized = cp_name.to_lowercase().replace('-', "_");
    if let Some(value) = tables::iana_lookup(&normalized) {
        return Ok(value.to_owned());
    }
    if strict {
        return Err(PyValueError::new_err(format!(
            "Unable to retrieve IANA for '{normalized}'"
        )));
    }
    Ok(normalized)
}

#[pyfunction(signature = (cp_name, strict=true))]
fn iana_name(cp_name: &str, strict: bool) -> PyResult<String> {
    iana_name_impl(cp_name, strict)
}

pub(crate) fn sig_or_bom(raw: &[u8]) -> (Option<&'static str>, &'static [u8]) {
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
        .find(|(_, mark)| raw.starts_with(mark))
        .map_or((None, b""), |(encoding, mark)| (Some(*encoding), *mark))
}

#[pyfunction]
fn identify_sig_or_bom<'py>(
    py: Python<'py>,
    sequence: &Bound<'py, PyAny>,
) -> PyResult<(Option<&'static str>, Bound<'py, PyBytes>)> {
    let mut owned = Vec::new();
    let raw = bytes_of(sequence, &mut owned).map_err(|_| {
        PyTypeError::new_err("sequence must be an object supporting the bytes protocol")
    })?;
    let (encoding, mark) = sig_or_bom(raw);
    Ok((encoding, PyBytes::new(py, mark)))
}

#[pyfunction]
fn is_cp_similar(iana_name_a: &str, iana_name_b: &str) -> bool {
    tables::similar_encodings(iana_name_a).contains(&iana_name_b)
}

#[pyfunction]
fn is_multi_byte_encoding(name: &str) -> bool {
    tables::is_multi_byte_encoding(name)
}

fn native_single_byte_decoder(encoding: &str) -> PyResult<impl Fn(u8) -> Option<char>> {
    codecs::single_byte_decoder(encoding)
        .ok_or_else(|| PyImportError::new_err(format!("No module named 'encodings.{encoding}'")))
}

#[pyfunction]
fn cp_similarity(encoding_a: &str, encoding_b: &str) -> PyResult<f64> {
    if tables::is_multi_byte_encoding(encoding_a) || tables::is_multi_byte_encoding(encoding_b) {
        return Ok(0.0);
    }
    let decoder_a = native_single_byte_decoder(encoding_a)?;
    let decoder_b = native_single_byte_decoder(encoding_b)?;
    let matches = (0u8..=255)
        .filter(|&byte| decoder_a(byte) == decoder_b(byte))
        .count();
    Ok(matches as f64 / 256.0)
}

/* ---------------------------------------------------------------------- */
/* Coherence detection (cd.py)                                             */
/* ---------------------------------------------------------------------- */

pub(crate) fn mb_languages(iana_name: &str) -> Vec<&'static str> {
    if iana_name.starts_with("shift_")
        || iana_name.starts_with("iso2022_jp")
        || iana_name.starts_with("euc_j")
        || iana_name == "cp932"
    {
        return vec!["Japanese"];
    }
    if iana_name.starts_with("gb") || ZH_NAMES.contains(&iana_name) {
        return vec!["Chinese"];
    }
    if iana_name.starts_with("iso2022_kr") || KO_NAMES.contains(&iana_name) {
        return vec!["Korean"];
    }
    Vec::new()
}

#[pyfunction]
fn mb_encoding_languages(iana_name: &str) -> Vec<&'static str> {
    mb_languages(iana_name)
}

fn encoding_unicode_range_impl(encoding: &str) -> PyResult<Vec<&'static str>> {
    if tables::is_multi_byte_encoding(encoding) {
        return Err(PyOSError::new_err(
            "Function not supported on multi-byte code page",
        ));
    }
    let decoder = native_single_byte_decoder(encoding)?;
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

#[pyfunction]
fn encoding_unicode_range(encoding: &str) -> PyResult<Vec<&'static str>> {
    encoding_unicode_range_impl(encoding)
}

fn range_languages(primary_range: &str) -> Vec<&'static str> {
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

#[pyfunction]
fn unicode_range_languages(primary_range: &str) -> Vec<&'static str> {
    range_languages(primary_range)
}

/// Languages a single-byte code page can express; memoized per encoding.
pub(crate) fn single_byte_languages(encoding: &str) -> Vec<&'static str> {
    static CACHE: OnceLock<Mutex<HashMap<String, Vec<&'static str>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(value) = cache.lock().ok().and_then(|map| map.get(encoding).cloned()) {
        return value;
    }
    let value = match encoding_unicode_range_impl(encoding) {
        Err(_) => Vec::new(),
        Ok(ranges) => match ranges.iter().find(|range| !range.contains("Latin")) {
            None => vec!["Latin Based"],
            Some(primary) => range_languages(primary),
        },
    };
    if let Ok(mut map) = cache.lock() {
        map.insert(encoding.to_owned(), value.clone());
    }
    value
}

#[pyfunction]
fn encoding_languages(encoding: &str) -> PyResult<Vec<&'static str>> {
    if tables::is_multi_byte_encoding(encoding) {
        return Err(PyOSError::new_err(
            "Function not supported on multi-byte code page",
        ));
    }
    Ok(single_byte_languages(encoding))
}

#[pyfunction]
fn get_target_features(language: &str) -> PyResult<(bool, bool)> {
    let language = language_or_err(language)?;
    Ok((language.has_accents, language.pure_latin))
}

pub(crate) fn alphabet_languages_impl(
    characters: &[char],
    ignore_non_latin: bool,
) -> Vec<&'static str> {
    let source_has_accents = characters
        .iter()
        .any(|&character| unicode::character_flags(character) & unicode::ACCENTUATED != 0);
    // Count, per language, how many distinct source characters it lists.
    let mut unique: Vec<char> = characters.to_vec();
    unique.sort_unstable();
    unique.dedup();
    let mut counts = [0usize; 64];
    for &character in &unique {
        let mut mask = tables::language_mask(character);
        while mask != 0 {
            counts[mask.trailing_zeros() as usize] += 1;
            mask &= mask - 1;
        }
    }
    let mut matches = Vec::new();
    for (index, language) in tables::languages().iter().enumerate() {
        if (ignore_non_latin && !language.pure_latin)
            || (!language.has_accents && source_has_accents)
        {
            continue;
        }
        let ratio = counts[index] as f64 / language.characters.len() as f64;
        if ratio >= 0.2 {
            matches.push((language.name, ratio));
        }
    }
    matches.sort_by(|a, b| b.1.total_cmp(&a.1));
    matches.into_iter().map(|item| item.0).collect()
}

#[pyfunction(signature = (characters, ignore_non_latin=false))]
fn alphabet_languages(
    characters: Vec<String>,
    ignore_non_latin: bool,
) -> PyResult<Vec<&'static str>> {
    let characters = characters
        .iter()
        .map(|character| one_char(character))
        .collect::<PyResult<Vec<char>>>()?;
    Ok(alphabet_languages_impl(&characters, ignore_non_latin))
}

fn popularity_compare(language: &Language, ordered: &[char]) -> f64 {
    let target_count = language.characters.len();
    if ordered.is_empty() {
        return f64::NAN;
    }
    let large_alphabet = target_count > 26;
    let large_threshold = target_count as f64 / 3.0;
    let projection_ratio = target_count as f64 / ordered.len() as f64;
    let common: Vec<(usize, usize)> = ordered
        .iter()
        .enumerate()
        .filter_map(|(popularity_rank, character)| {
            language
                .ranks
                .get(character)
                .map(|&language_rank| (language_rank, popularity_rank))
        })
        .collect();

    let mut approved = 0usize;
    for &(language_rank, popularity_rank) in &common {
        let projected = (popularity_rank as f64 * projection_ratio) as usize;
        let distance = projected.abs_diff(language_rank);

        if !large_alphabet && distance > 4 {
            continue;
        }
        if large_alphabet && (distance as f64) < large_threshold {
            approved += 1;
            continue;
        }
        if language_rank == 0 {
            approved += 1;
            continue;
        }

        let after_len = target_count - language_rank;
        let mut before = 0usize;
        let mut after = 0usize;
        for &(other_language_rank, other_popularity_rank) in &common {
            if other_language_rank < language_rank {
                if other_popularity_rank < popularity_rank {
                    before += 1;
                    if 5 * before >= 2 * language_rank {
                        approved += 1;
                        break;
                    }
                }
            } else if other_popularity_rank >= popularity_rank {
                after += 1;
                if 5 * after >= 2 * after_len {
                    approved += 1;
                    break;
                }
            }
        }
    }

    approved as f64 / ordered.len() as f64
}

#[pyfunction]
fn characters_popularity_compare(language: &str, ordered_characters: Vec<String>) -> PyResult<f64> {
    if ordered_characters.is_empty() {
        return Err(PyZeroDivisionError::new_err("division by zero"));
    }
    let language = language_or_err(language)?;
    // Strings that are not a single character can never match a profile
    // entry; U+FFFF (a noncharacter) stands in for them.
    let ordered: Vec<char> = ordered_characters
        .iter()
        .map(|value| one_char(value).unwrap_or('\u{ffff}'))
        .collect();
    Ok(popularity_compare(language, &ordered))
}

/// Average each language's ratios across chunks, rounded, best first.
fn merge_by<K: Clone + Eq + std::hash::Hash>(results: Vec<Vec<(K, f64)>>) -> Vec<(K, f64)> {
    let mut order = Vec::new();
    let mut ratios: FxHashMap<K, Vec<f64>> = FxHashMap::default();
    for result in results {
        for (language, ratio) in result {
            ratios
                .entry(language)
                .or_insert_with_key(|language| {
                    order.push(language.clone());
                    Vec::new()
                })
                .push(ratio);
        }
    }
    let mut merged: Vec<(K, f64)> = order
        .into_iter()
        .map(|language| {
            let values = &ratios[&language];
            let mean = values.iter().sum::<f64>() / values.len() as f64;
            (language, py_round(mean, 4))
        })
        .collect();
    merged.sort_by(|a, b| b.1.total_cmp(&a.1));
    merged
}

pub(crate) fn merge_coherence(results: Vec<Vec<(&'static str, f64)>>) -> Vec<(&'static str, f64)> {
    merge_by(results)
}

#[pyfunction]
fn merge_coherence_ratios(results: Vec<Vec<(String, f64)>>) -> Vec<(String, f64)> {
    merge_by(results)
}

/// Fold alternative profiles (`"English—"`) into their base language,
/// keeping the best ratio, when any language appears more than once.
fn filter_alt_by<K: Clone + Eq + std::hash::Hash>(
    results: Vec<(K, f64)>,
    normalize: impl Fn(&K) -> K,
) -> Vec<(K, f64)> {
    let mut order = Vec::new();
    let mut ratios: FxHashMap<K, Vec<f64>> = FxHashMap::default();
    for (language, ratio) in &results {
        ratios
            .entry(normalize(language))
            .or_insert_with_key(|normalized| {
                order.push(normalized.clone());
                Vec::new()
            })
            .push(*ratio);
    }
    if !ratios.values().any(|values| values.len() > 1) {
        return results;
    }
    order
        .into_iter()
        .map(|language| {
            let best = ratios[&language]
                .iter()
                .copied()
                .max_by(f64::total_cmp)
                .unwrap_or(0.0);
            (language, best)
        })
        .collect()
}

#[pyfunction]
fn filter_alt_coherence_matches(results: Vec<(String, f64)>) -> Vec<(String, f64)> {
    filter_alt_by(results, |language| language.replace('—', ""))
}

fn alpha_split(decoded_sequence: &str) -> Vec<String> {
    let mut layers: Vec<(u16, String)> = Vec::new();
    let mut previous: Option<(u16, usize)> = None;

    for character in decoded_sequence.chars() {
        let (alpha, range) = mess::alpha_range(character);
        if !alpha || range == unicode::NO_RANGE {
            continue;
        }
        if let Some((previous_range, target)) = previous {
            if previous_range == range {
                layers[target].1.push(character);
                continue;
            }
        }
        let target = layers
            .iter()
            .position(|(discovered, _)| !unicode::suspicious_range_indices(*discovered, range))
            .unwrap_or_else(|| {
                layers.push((range, String::new()));
                layers.len() - 1
            });
        layers[target].1.push(character);
        previous = Some((range, target));
    }

    layers
        .into_iter()
        .map(|(_, layer)| layer.to_lowercase())
        .collect()
}

#[pyfunction]
fn alpha_unicode_split(decoded_sequence: &str) -> Vec<String> {
    alpha_split(decoded_sequence)
}

pub(crate) fn coherence(
    decoded_sequence: &str,
    threshold: f64,
    lg_inclusion: Option<&str>,
) -> PyResult<Vec<(&'static str, f64)>> {
    let mut results = Vec::<(&'static str, f64)>::new();
    let mut inclusion: Vec<&str> = lg_inclusion
        .map(|value| value.split(',').collect())
        .unwrap_or_default();
    let ignore_non_latin = inclusion.contains(&"Latin Based");
    inclusion.retain(|language| *language != "Latin Based");
    let mut sufficient = 0usize;

    for layer in alpha_split(decoded_sequence) {
        let mut counts = FxHashMap::<char, usize>::default();
        let mut order = Vec::<char>::new();
        let mut length = 0usize;
        for character in layer.chars() {
            length += 1;
            let count = counts.entry(character).or_default();
            if *count == 0 {
                order.push(character);
            }
            *count += 1;
        }
        if length <= 32 {
            continue;
        }
        order.sort_by(|a, b| counts[b].cmp(&counts[a]));
        let detected;
        let languages: &[&str] = if inclusion.is_empty() {
            detected = alphabet_languages_impl(&order, ignore_non_latin);
            &detected
        } else {
            &inclusion
        };
        for &name in languages {
            let language = language_or_err(name)?;
            let ratio = popularity_compare(language, &order);
            if ratio < threshold {
                continue;
            }
            if ratio >= 0.8 {
                sufficient += 1;
            }
            results.push((language.name, py_round(ratio, 4)));
            if sufficient >= 3 {
                break;
            }
        }
    }
    // Alternative profile names only carry trailing em dashes, so trimming
    // yields the (static) base name.
    let mut filtered = filter_alt_by(results, |language| language.trim_end_matches('—'));
    filtered.sort_by(|a, b| b.1.total_cmp(&a.1));
    Ok(filtered)
}

#[pyfunction(signature = (decoded_sequence, threshold=0.1, lg_inclusion=None))]
fn coherence_ratio(
    decoded_sequence: &str,
    threshold: f64,
    lg_inclusion: Option<&str>,
) -> PyResult<Vec<(&'static str, f64)>> {
    coherence(decoded_sequence, threshold, lg_inclusion)
}

/* ---------------------------------------------------------------------- */
/* Chunk sampling                                                          */
/* ---------------------------------------------------------------------- */

fn char_slice(value: &str, start: usize, count: usize) -> String {
    value.chars().skip(start).take(count).collect()
}

#[pyfunction]
#[allow(clippy::too_many_arguments)]
#[pyo3(signature = (sequences, encoding_iana, offsets, chunk_size, bom_or_sig_available, strip_sig_or_bom, sig_payload, is_multi_byte_decoder, decoded_payload=None, deferred_decoding=false))]
fn cut_sequence_chunks(
    sequences: Vec<u8>,
    encoding_iana: &str,
    offsets: Vec<usize>,
    chunk_size: usize,
    bom_or_sig_available: bool,
    strip_sig_or_bom: bool,
    sig_payload: Vec<u8>,
    is_multi_byte_decoder: bool,
    decoded_payload: Option<&str>,
    deferred_decoding: bool,
) -> PyResult<Vec<String>> {
    cut_sequence_chunks_impl(
        &sequences,
        encoding_iana,
        offsets,
        chunk_size,
        bom_or_sig_available,
        strip_sig_or_bom,
        &sig_payload,
        is_multi_byte_decoder,
        decoded_payload,
        deferred_decoding,
    )
    .map_err(|error| decode_error(encoding_iana, error))
}

/// `prefix in decoded`, probing first around where a chunk starting at byte
/// `offset` of the payload is expected to land (any hit there is a hit).
fn contains_near(decoded: &str, prefix: &str, offset: usize, payload_len: usize) -> bool {
    const RADIUS: usize = 32768 * 4;
    let expected = (offset as u128 * decoded.len() as u128 / payload_len.max(1) as u128) as usize;
    let mut start = expected.saturating_sub(RADIUS).min(decoded.len());
    let mut end = (expected + RADIUS + prefix.len()).min(decoded.len());
    while !decoded.is_char_boundary(start) {
        start -= 1;
    }
    while !decoded.is_char_boundary(end) {
        end += 1;
    }
    decoded[start..end].contains(prefix) || decoded.contains(prefix)
}

/// Chunk sampler for one candidate encoding. Chunks are produced lazily so
/// the detector can stop decoding as soon as it has seen enough of them.
pub(crate) struct ChunkCutter<'a> {
    sequences: &'a [u8],
    encoding: &'a str,
    offsets: std::vec::IntoIter<usize>,
    chunk_size: usize,
    bom_or_sig_available: bool,
    strip_sig_or_bom: bool,
    sig_payload: &'a [u8],
    is_multi_byte_decoder: bool,
    decoded_payload: Option<&'a str>,
    deferred_decoding: bool,
    decoded_len: Option<usize>,
    done: bool,
}

impl<'a> ChunkCutter<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        sequences: &'a [u8],
        encoding: &'a str,
        offsets: Vec<usize>,
        chunk_size: usize,
        bom_or_sig_available: bool,
        strip_sig_or_bom: bool,
        sig_payload: &'a [u8],
        is_multi_byte_decoder: bool,
        decoded_payload: Option<&'a str>,
        deferred_decoding: bool,
    ) -> Self {
        Self {
            sequences,
            encoding,
            offsets: offsets.into_iter(),
            chunk_size,
            bom_or_sig_available,
            strip_sig_or_bom,
            sig_payload,
            is_multi_byte_decoder,
            decoded_payload,
            deferred_decoding,
            decoded_len: None,
            done: false,
        }
    }

    fn iso2022_decoded(&self) -> Option<&'a str> {
        self.decoded_payload
            .filter(|_| self.encoding.starts_with("iso2022_"))
    }

    fn single_byte_decoded(&self) -> Option<&'a str> {
        self.decoded_payload.filter(|_| !self.is_multi_byte_decoder)
    }

    fn deferred_base(&self) -> &'a [u8] {
        if self.strip_sig_or_bom {
            &self.sequences[self.sig_payload.len()..]
        } else {
            self.sequences
        }
    }

    /// `sequences[start:end]`, with the signature prepended when it is kept.
    fn cut(&self, start: usize, end: usize) -> std::borrow::Cow<'a, [u8]> {
        let cut = &self.sequences[start.min(end)..end];
        if self.bom_or_sig_available && !self.strip_sig_or_bom {
            let mut prefixed = self.sig_payload.to_vec();
            prefixed.extend_from_slice(cut);
            std::borrow::Cow::Owned(prefixed)
        } else {
            std::borrow::Cow::Borrowed(cut)
        }
    }

    /// Check up front that every chunk a full pass would strictly decode is
    /// valid, so stopping early never hides a decoding failure.
    pub(crate) fn validate(&self) -> Result<(), DecodeError> {
        if self.iso2022_decoded().is_some() || self.single_byte_decoded().is_some() {
            return Ok(());
        }
        let offsets = self.offsets.as_slice();
        if self.deferred_decoding {
            let base = self.deferred_base();
            for &offset in offsets {
                let cut = &base[offset.min(base.len())..(offset + self.chunk_size).min(base.len())];
                if cut.is_empty() {
                    break;
                }
                if !codecs::is_valid(cut, self.encoding)? {
                    return Err(DecodeError::Invalid);
                }
            }
        } else if !self.is_multi_byte_decoder {
            for &offset in offsets {
                let chunk_end = offset + self.chunk_size;
                if chunk_end > self.sequences.len() + 8 {
                    continue;
                }
                let end = chunk_end.min(self.sequences.len());
                if !codecs::is_valid(&self.cut(offset, end), self.encoding)? {
                    return Err(DecodeError::Invalid);
                }
            }
        }
        Ok(())
    }

    fn next_chunk(&mut self) -> Option<Result<String, DecodeError>> {
        loop {
            let offset = self.offsets.next()?;
            if let Some(decoded) = self.iso2022_decoded() {
                let decoded_len = *self
                    .decoded_len
                    .get_or_insert_with(|| decoded.chars().count());
                let decoded_offset = offset * decoded_len / self.sequences.len();
                let chunk = char_slice(decoded, decoded_offset, self.chunk_size);
                return (!chunk.is_empty()).then_some(Ok(chunk));
            }
            if let Some(decoded) = self.single_byte_decoded() {
                let chunk = char_slice(decoded, offset, self.chunk_size);
                return (!chunk.is_empty()).then_some(Ok(chunk));
            }
            if self.deferred_decoding {
                let base = self.deferred_base();
                let cut = &base[offset.min(base.len())..(offset + self.chunk_size).min(base.len())];
                if cut.is_empty() {
                    return None;
                }
                return Some(codecs::decode(cut, self.encoding, Errors::Strict));
            }
            let errors = if self.is_multi_byte_decoder {
                Errors::Ignore
            } else {
                Errors::Strict
            };
            let chunk_end = offset + self.chunk_size;
            if chunk_end > self.sequences.len() + 8 {
                continue;
            }
            let end = chunk_end.min(self.sequences.len());
            let mut chunk = match codecs::decode(&self.cut(offset, end), self.encoding, errors) {
                Ok(chunk) => chunk,
                Err(error) => return Some(Err(error)),
            };
            if self.is_multi_byte_decoder && offset > 0 {
                if let Some(decoded) = self.decoded_payload {
                    let prefix: String = chunk.chars().take(self.chunk_size.min(16)).collect();
                    if !contains_near(decoded, &prefix, offset, self.sequences.len()) {
                        for delta in 0..4usize {
                            let signed_start = offset as isize - delta as isize;
                            let adjusted_start = if signed_start < 0 {
                                self.sequences
                                    .len()
                                    .saturating_sub((-signed_start) as usize)
                            } else {
                                signed_start as usize
                            };
                            chunk = match codecs::decode(
                                &self.cut(adjusted_start, end),
                                self.encoding,
                                Errors::Ignore,
                            ) {
                                Ok(chunk) => chunk,
                                Err(error) => return Some(Err(error)),
                            };
                            let adjusted_prefix: String =
                                chunk.chars().take(self.chunk_size.min(16)).collect();
                            if decoded.contains(&adjusted_prefix) {
                                break;
                            }
                        }
                    }
                }
            }
            return Some(Ok(chunk));
        }
    }
}

impl Iterator for ChunkCutter<'_> {
    type Item = Result<String, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let item = self.next_chunk();
        if item.is_none() {
            self.done = true;
        }
        item
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn cut_sequence_chunks_impl(
    sequences: &[u8],
    encoding_iana: &str,
    offsets: Vec<usize>,
    chunk_size: usize,
    bom_or_sig_available: bool,
    strip_sig_or_bom: bool,
    sig_payload: &[u8],
    is_multi_byte_decoder: bool,
    decoded_payload: Option<&str>,
    deferred_decoding: bool,
) -> Result<Vec<String>, DecodeError> {
    ChunkCutter::new(
        sequences,
        encoding_iana,
        offsets,
        chunk_size,
        bom_or_sig_available,
        strip_sig_or_bom,
        sig_payload,
        is_multi_byte_decoder,
        decoded_payload,
        deferred_decoding,
    )
    .collect()
}

#[pymodule(gil_used = false)]
mod _native {
    #[pymodule_export]
    use super::api::from_bytes;
    #[pymodule_export]
    use super::mess::mess_ratio;
    #[pymodule_export]
    use super::models::{CharsetMatch, CharsetMatches, CliDetectionResult};
    #[pymodule_export]
    use super::{
        alpha_unicode_split, alphabet_languages, any_specified_encoding, backend_name,
        character_flags, characters_popularity_compare, coherence_ratio, cp_similarity,
        cut_sequence_chunks, encoding_languages, encoding_unicode_range,
        filter_alt_coherence_matches, get_target_features, iana_name, identify_sig_or_bom,
        is_case_variable, is_cp_similar, is_emoticon, is_multi_byte_encoding, is_punctuation,
        is_separator, is_suspiciously_successive_range, is_symbol, is_unprintable,
        mb_encoding_languages, merge_coherence_ratios, remove_accent, should_strip_sig_or_bom,
        unicode_range, unicode_range_languages,
    };

    #[pymodule_export]
    #[allow(non_upper_case_globals)]
    const __version__: &str = super::VERSION;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popularity_scores_ranked_input() {
        let english = tables::language("English").unwrap();
        assert_eq!(popularity_compare(english, &['e', 'e', 't', 'a']), 0.25);
    }

    #[test]
    fn popularity_empty_input_matches_python_division_shape() {
        let english = tables::language("English").unwrap();
        assert!(popularity_compare(english, &[]).is_nan());
    }

    #[test]
    fn python_rounding_and_sum() {
        assert_eq!(py_round(0.125, 2), 0.12);
        assert_eq!(py_round(2.675, 2), 2.67);
        assert_eq!(py_sum(&[0.1; 10]), 1.0);
    }

    #[test]
    fn declared_encoding() {
        assert_eq!(
            specified_encoding(b"<meta charset=\"windows-1252\">", 8192),
            Some("cp1252")
        );
        assert_eq!(specified_encoding(b"plain", 8192), None);
    }
}

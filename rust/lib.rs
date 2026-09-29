use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use pyo3::exceptions::{
    PyAttributeError, PyImportError, PyKeyError, PyLookupError, PyOSError, PyRuntimeError,
    PyTypeError, PyValueError, PyZeroDivisionError,
};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict, PyList, PyString};

mod api;
mod mess;
mod models;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const LATIN: u16 = 1;
const ACCENTUATED: u16 = 1 << 1;
const CJK: u16 = 1 << 2;
const HANGUL: u16 = 1 << 3;
const KATAKANA: u16 = 1 << 4;
const HIRAGANA: u16 = 1 << 5;
const THAI: u16 = 1 << 6;
const ARABIC: u16 = 1 << 7;
const ARABIC_ISOLATED_FORM: u16 = 1 << 8;
const HALFWIDTH_KATAKANA: u16 = 1 << 9;
const LIGATURE: u16 = 1 << 10;
const SUPERSCRIPT: u16 = 1 << 11;
const SENTENCE_OPEN_PUNCTUATION: u16 = 1 << 12;

static UNICODE_RANGES: OnceLock<Vec<(u32, u32, String)>> = OnceLock::new();

fn constants(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    py.import("charset_normalizer.constant")
        .map(|m| m.into_any())
}

fn frequency_profile(py: Python<'_>, language: &str) -> PyResult<Vec<String>> {
    let module = constants(py)?;
    let frequencies = module.getattr("FREQUENCIES")?.cast_into::<PyDict>()?;
    let profile = frequencies
        .get_item(language)?
        .ok_or_else(|| PyValueError::new_err(format!("{language} not available")))?;
    profile.extract()
}

fn char_info_flags(py: Python<'_>, character: &str) -> PyResult<(bool, bool)> {
    let flags = character_flags(py, character)?;
    Ok((flags & ACCENTUATED != 0, flags & LATIN != 0))
}

fn popularity_compare(profile: &[String], ordered: &[String]) -> f64 {
    let target_count = profile.len();
    if ordered.is_empty() {
        return f64::NAN;
    }

    let ranks: HashMap<&str, usize> = profile
        .iter()
        .enumerate()
        .map(|(rank, character)| (character.as_str(), rank))
        .collect();
    let large_alphabet = target_count > 26;
    let large_threshold = target_count as f64 / 3.0;
    let projection_ratio = target_count as f64 / ordered.len() as f64;
    let mut common = Vec::with_capacity(ordered.len());

    for (popularity_rank, character) in ordered.iter().enumerate() {
        if let Some(language_rank) = ranks.get(character.as_str()) {
            common.push((*language_rank, popularity_rank));
        }
    }

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
        let mut accepted = false;
        let mut before = 0usize;
        let mut after = 0usize;
        for &(other_language_rank, other_popularity_rank) in &common {
            if other_language_rank < language_rank {
                if other_popularity_rank < popularity_rank {
                    before += 1;
                    if 5 * before >= 2 * language_rank {
                        accepted = true;
                        break;
                    }
                }
            } else if other_popularity_rank >= popularity_rank {
                after += 1;
                if 5 * after >= 2 * after_len {
                    accepted = true;
                    break;
                }
            }
        }
        if accepted {
            approved += 1;
        }
    }

    approved as f64 / ordered.len() as f64
}

fn one_codepoint(character: &str) -> PyResult<u32> {
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
    Ok(value as u32)
}

#[pyfunction]
fn character_flags(py: Python<'_>, character: &str) -> PyResult<u16> {
    one_codepoint(character)?;
    let unicodedata = py.import("unicodedata")?;
    let description: String = match unicodedata.getattr("name")?.call1((character,)) {
        Ok(value) => value.extract()?,
        Err(error) if error.is_instance_of::<PyValueError>(py) => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut flags = 0u16;
    if description.contains("LATIN") {
        flags |= LATIN;
    }
    if description.contains("CJK") {
        flags |= CJK;
    }
    if description.contains("HANGUL") {
        flags |= HANGUL;
    }
    if description.contains("KATAKANA") {
        flags |= KATAKANA;
        if description.contains("HALFWIDTH") {
            flags |= HALFWIDTH_KATAKANA;
        }
    }
    if description.contains("HIRAGANA") {
        flags |= HIRAGANA;
    }
    if description.contains("THAI") {
        flags |= THAI;
    }
    if description.contains("ARABIC") {
        flags |= ARABIC;
        if description.contains("ISOLATED FORM") {
            flags |= ARABIC_ISOLATED_FORM;
        }
    }
    if description.contains("LIGATURE") || description.ends_with("LETTER AE") {
        flags |= LIGATURE;
    }
    if description.contains("SUPERSCRIPT") {
        flags |= SUPERSCRIPT;
    }
    if description == "INVERTED QUESTION MARK" || description == "INVERTED EXCLAMATION MARK" {
        flags |= SENTENCE_OPEN_PUNCTUATION;
    }
    let keywords: Vec<String> = constants(py)?.getattr("_ACCENT_KEYWORDS")?.extract()?;
    if keywords.iter().any(|keyword| description.contains(keyword)) {
        flags |= ACCENTUATED;
    }
    Ok(flags)
}

fn unicode_category(py: Python<'_>, character: &str) -> PyResult<String> {
    one_codepoint(character)?;
    py.import("unicodedata")?
        .getattr("category")?
        .call1((character,))?
        .extract()
}

#[pyfunction]
fn remove_accent(py: Python<'_>, character: &str) -> PyResult<String> {
    one_codepoint(character)?;
    let decomposition: String = py
        .import("unicodedata")?
        .getattr("decomposition")?
        .call1((character,))?
        .extract()?;
    let Some(first) = decomposition.split_whitespace().next() else {
        return Ok(character.to_owned());
    };
    let codepoint =
        u32::from_str_radix(first, 16).map_err(|error| PyValueError::new_err(error.to_string()))?;
    char::from_u32(codepoint)
        .map(|value| value.to_string())
        .ok_or_else(|| PyValueError::new_err("invalid Unicode decomposition"))
}

#[pyfunction]
fn is_punctuation(py: Python<'_>, character: &str) -> PyResult<bool> {
    let category = unicode_category(py, character)?;
    Ok(category.contains('P')
        || unicode_range(py, character)?.is_some_and(|name| name.contains("Punctuation")))
}

#[pyfunction]
fn is_symbol(py: Python<'_>, character: &str) -> PyResult<bool> {
    let category = unicode_category(py, character)?;
    Ok(category.contains('S')
        || category.contains('N')
        || (unicode_range(py, character)?.is_some_and(|name| name.contains("Forms"))
            && category != "Lo"))
}

#[pyfunction]
fn is_emoticon(py: Python<'_>, character: &str) -> PyResult<bool> {
    Ok(unicode_range(py, character)?
        .is_some_and(|name| name.contains("Emoticons") || name.contains("Pictographs")))
}

#[pyfunction]
fn is_separator(py: Python<'_>, character: &str) -> PyResult<bool> {
    let value = one_codepoint(character)?;
    let py_character = pyo3::types::PyString::new(py, character);
    if py_character.call_method0("isspace")?.extract::<bool>()?
        || matches!(char::from_u32(value), Some('｜' | '+' | '<' | '>'))
    {
        return Ok(true);
    }
    let category = unicode_category(py, character)?;
    Ok(category.contains('Z') || matches!(category.as_str(), "Po" | "Pd" | "Pc"))
}

#[pyfunction]
fn is_case_variable(py: Python<'_>, character: &str) -> PyResult<bool> {
    one_codepoint(character)?;
    let value = pyo3::types::PyString::new(py, character);
    let lower: bool = value.call_method0("islower")?.extract()?;
    let upper: bool = value.call_method0("isupper")?.extract()?;
    Ok(lower != upper)
}

#[pyfunction]
fn is_unprintable(py: Python<'_>, character: &str) -> PyResult<bool> {
    one_codepoint(character)?;
    let value = pyo3::types::PyString::new(py, character);
    let space: bool = value.call_method0("isspace")?.extract()?;
    let printable: bool = value.call_method0("isprintable")?.extract()?;
    Ok(!space && !printable && character != "\u{1a}" && character != "\u{feff}")
}

#[pyfunction(signature = (sequence, search_zone=8192))]
fn any_specified_encoding(
    py: Python<'_>,
    sequence: &Bound<'_, PyAny>,
    search_zone: usize,
) -> PyResult<Option<String>> {
    if !sequence.is_instance_of::<PyBytes>()
        && !sequence.is_instance_of::<pyo3::types::PyByteArray>()
    {
        return Err(PyTypeError::new_err(""));
    }
    let bytes: Vec<u8> = sequence.extract()?;
    let search = &bytes[..bytes.len().min(search_zone)];
    let lowered: Vec<u8> = search.iter().map(u8::to_ascii_lowercase).collect();
    if !lowered.windows(6).any(|part| part == b"coding")
        && !lowered.windows(7).any(|part| part == b"charset")
    {
        return Ok(None);
    }
    let decoded = String::from_utf8(search.iter().copied().filter(u8::is_ascii).collect())
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    let regex = constants(py)?.getattr("RE_POSSIBLE_ENCODING_INDICATION")?;
    for item in regex.call_method1("finditer", (&decoded,))?.try_iter()? {
        let matched = item?;
        let specified: String = matched.call_method1("group", (1,))?.extract()?;
        let normalized = specified.to_lowercase().replace('-', "_");
        let names = constants(py)?
            .getattr("_IANA_NAMES")?
            .cast_into::<PyDict>()?;
        if let Some(value) = names.get_item(&normalized)? {
            return Ok(Some(value.extract()?));
        }
    }
    Ok(None)
}

#[pyfunction]
fn unicode_range(py: Python<'_>, character: &str) -> PyResult<Option<String>> {
    let codepoint = one_codepoint(character)?;
    if codepoint < 32 {
        return Ok(Some("Control character".to_owned()));
    }
    if codepoint < 128 {
        return Ok(Some("Basic Latin".to_owned()));
    }
    if UNICODE_RANGES.get().is_none() {
        let raw = constants(py)?
            .getattr("UNICODE_RANGES_COMBINED")?
            .cast_into::<PyDict>()?;
        let mut ranges = Vec::with_capacity(raw.len());
        for (name, value) in raw.iter() {
            ranges.push((
                value.getattr("start")?.extract()?,
                value.getattr("stop")?.extract()?,
                name.extract()?,
            ));
        }
        ranges.sort_by_key(|entry| entry.0);
        let _ = UNICODE_RANGES.set(ranges);
    }
    let ranges = UNICODE_RANGES
        .get()
        .ok_or_else(|| PyRuntimeError::new_err("Unicode range table initialization failed"))?;
    let index = ranges.partition_point(|entry| entry.0 <= codepoint);
    if index == 0 {
        return Ok(None);
    }
    let (start, stop, name) = &ranges[index - 1];
    if *start <= codepoint && codepoint < *stop {
        return Ok(Some(name.clone()));
    }
    Ok(None)
}

fn compatible_range_families(a: &str, b: &str) -> bool {
    let pair = if a <= b { (a, b) } else { (b, a) };
    matches!(
        pair,
        ("CJK", "Hiragana")
            | ("CJK", "Katakana")
            | ("CJK", "Kana")
            | ("Bopomofo", "CJK")
            | ("CJK", "Kanbun")
            | ("Hiragana", "Katakana")
            | ("Hiragana", "Kana")
            | ("Kana", "Katakana")
            | ("CJK", "Hangul")
            | ("IPA", "Latin")
            | ("Latin", "Phonetic")
            | ("IPA", "Phonetic")
            | ("Latin", "Spacing Modifier Letters")
            | ("IPA", "Spacing Modifier Letters")
            | ("Phonetic", "Spacing Modifier Letters")
            | ("Alphabetic Presentation Forms", "Latin")
            | ("Alphabetic Presentation Forms", "Armenian")
            | ("Alphabetic Presentation Forms", "Hebrew")
            | ("Halfwidth and Fullwidth Forms", "Latin")
            | ("CJK", "Halfwidth and Fullwidth Forms")
            | ("Halfwidth and Fullwidth Forms", "Hiragana")
            | ("Halfwidth and Fullwidth Forms", "Katakana")
            | ("Halfwidth and Fullwidth Forms", "Kana")
            | ("Halfwidth and Fullwidth Forms", "Hangul")
    )
}

pub(crate) fn suspicious_ranges_impl(
    py: Python<'_>,
    range_a: Option<&str>,
    range_b: Option<&str>,
) -> PyResult<bool> {
    let (Some(range_a), Some(range_b)) = (range_a, range_b) else {
        return Ok(true);
    };
    let module = constants(py)?;
    let families = module.getattr("_RANGE_FAMILIES")?.cast_into::<PyDict>()?;
    let family_a: String = families
        .get_item(range_a)?
        .ok_or_else(|| PyKeyError::new_err(range_a.to_owned()))?
        .extract()?;
    let family_b: String = families
        .get_item(range_b)?
        .ok_or_else(|| PyKeyError::new_err(range_b.to_owned()))?
        .extract()?;
    if family_a == family_b {
        return Ok(false);
    }
    let compatible_any = module.getattr("_COMPATIBLE_WITH_ANY_RANGE_FAMILIES")?;
    if compatible_any.contains(&family_a)? || compatible_any.contains(&family_b)? {
        return Ok(false);
    }
    if compatible_range_families(&family_a, &family_b) {
        return Ok(false);
    }
    let basic_compatible = module.getattr("_BASIC_LATIN_COMPATIBLE_RANGE_FAMILIES")?;
    if range_a == "Basic Latin" {
        return Ok(!basic_compatible.contains(&family_b)?);
    }
    if range_b == "Basic Latin" {
        return Ok(!basic_compatible.contains(&family_a)?);
    }
    Ok(true)
}

#[pyfunction]
fn is_suspiciously_successive_range(
    py: Python<'_>,
    unicode_range_a: Option<&str>,
    unicode_range_b: Option<&str>,
) -> PyResult<bool> {
    suspicious_ranges_impl(py, unicode_range_a, unicode_range_b)
}

#[pyfunction]
fn backend_name() -> &'static str {
    "rust-pyo3"
}

#[pyfunction]
fn should_strip_sig_or_bom(iana_encoding: &str) -> bool {
    iana_encoding != "utf_16" && iana_encoding != "utf_32"
}

#[pyfunction(signature = (cp_name, strict=true))]
fn iana_name(py: Python<'_>, cp_name: &str, strict: bool) -> PyResult<String> {
    let normalized = cp_name.to_lowercase().replace('-', "_");
    let names = constants(py)?
        .getattr("_IANA_NAMES")?
        .cast_into::<PyDict>()?;
    if let Some(value) = names.get_item(&normalized)? {
        return value.extract();
    }
    if strict {
        return Err(PyValueError::new_err(format!(
            "Unable to retrieve IANA for '{normalized}'"
        )));
    }
    Ok(normalized)
}

#[pyfunction]
fn identify_sig_or_bom<'py>(
    py: Python<'py>,
    sequence: &Bound<'py, PyAny>,
) -> PyResult<(Option<String>, Bound<'py, PyBytes>)> {
    let raw: Vec<u8> = sequence.extract().map_err(|_| {
        PyTypeError::new_err("sequence must be an object supporting the bytes protocol")
    })?;
    let marks: [(&str, &[u8]); 9] = [
        ("utf_8", b"\xef\xbb\xbf"),
        ("utf_7", b"\x2b\x2f\x76\x38"),
        ("utf_7", b"\x2b\x2f\x76\x39"),
        ("utf_7", b"\x2b\x2f\x76\x2b"),
        ("utf_7", b"\x2b\x2f\x76\x2f"),
        ("gb18030", b"\x84\x31\x95\x33"),
        ("utf_32", b"\x00\x00\xfe\xff"),
        ("utf_32", b"\xff\xfe\x00\x00"),
        ("utf_16", b"\xfe\xff"),
    ];
    for (encoding, mark) in marks {
        if raw.starts_with(mark) {
            return Ok((Some(encoding.to_owned()), PyBytes::new(py, mark)));
        }
    }
    if raw.starts_with(b"\xff\xfe") {
        return Ok((Some("utf_16".to_owned()), PyBytes::new(py, b"\xff\xfe")));
    }
    Ok((None, PyBytes::new(py, b"")))
}

#[pyfunction]
fn is_cp_similar(py: Python<'_>, iana_name_a: &str, iana_name_b: &str) -> PyResult<bool> {
    let similar = constants(py)?
        .getattr("IANA_SUPPORTED_SIMILAR")?
        .cast_into::<PyDict>()?;
    let Some(items) = similar.get_item(iana_name_a)? else {
        return Ok(false);
    };
    items.contains(iana_name_b)
}

#[pyfunction]
fn mb_encoding_languages(py: Python<'_>, iana_name: &str) -> PyResult<Vec<String>> {
    if iana_name.starts_with("shift_")
        || iana_name.starts_with("iso2022_jp")
        || iana_name.starts_with("euc_j")
        || iana_name == "cp932"
    {
        return Ok(vec!["Japanese".to_owned()]);
    }
    let module = constants(py)?;
    if iana_name.starts_with("gb") || module.getattr("ZH_NAMES")?.contains(iana_name)? {
        return Ok(vec!["Chinese".to_owned()]);
    }
    if iana_name.starts_with("iso2022_kr") || module.getattr("KO_NAMES")?.contains(iana_name)? {
        return Ok(vec!["Korean".to_owned()]);
    }
    Ok(Vec::new())
}

#[pyfunction]
fn get_target_features(py: Python<'_>, language: &str) -> PyResult<(bool, bool)> {
    let profile = frequency_profile(py, language)?;
    let mut has_accents = false;
    let mut pure_latin = true;
    for character in profile {
        let (accentuated, latin) = char_info_flags(py, &character)?;
        has_accents |= accentuated;
        pure_latin &= latin;
    }
    Ok((has_accents, pure_latin))
}

#[pyfunction(signature = (characters, ignore_non_latin=false))]
fn alphabet_languages(
    py: Python<'_>,
    characters: Vec<String>,
    ignore_non_latin: bool,
) -> PyResult<Vec<String>> {
    let source_has_accents = characters.iter().try_fold(false, |found, character| {
        Ok::<_, PyErr>(found || char_info_flags(py, character)?.0)
    })?;
    let source: HashSet<&str> = characters.iter().map(String::as_str).collect();
    let frequencies = constants(py)?
        .getattr("FREQUENCIES")?
        .cast_into::<PyDict>()?;
    let mut matches = Vec::new();

    for (language, raw_profile) in frequencies.iter() {
        let language: String = language.extract()?;
        let profile: Vec<String> = raw_profile.extract()?;
        let mut has_accents = false;
        let mut pure_latin = true;
        for character in &profile {
            let (accentuated, latin) = char_info_flags(py, character)?;
            has_accents |= accentuated;
            pure_latin &= latin;
        }
        if (ignore_non_latin && !pure_latin) || (!has_accents && source_has_accents) {
            continue;
        }
        let count = profile
            .iter()
            .filter(|character| source.contains(character.as_str()))
            .count();
        let ratio = count as f64 / profile.len() as f64;
        if ratio >= 0.2 {
            matches.push((language, ratio));
        }
    }
    matches.sort_by(|a, b| b.1.total_cmp(&a.1));
    Ok(matches.into_iter().map(|item| item.0).collect())
}

#[pyfunction]
fn characters_popularity_compare(
    py: Python<'_>,
    language: &str,
    ordered_characters: Vec<String>,
) -> PyResult<f64> {
    if ordered_characters.is_empty() {
        return Err(PyZeroDivisionError::new_err("division by zero"));
    }
    let profile = frequency_profile(py, language)?;
    Ok(popularity_compare(&profile, &ordered_characters))
}

#[pyfunction]
fn merge_coherence_ratios(py: Python<'_>, results: Vec<Vec<(String, f64)>>) -> PyResult<Py<PyAny>> {
    let mut order = Vec::new();
    let mut ratios: HashMap<String, Vec<f64>> = HashMap::new();
    for result in results {
        for (language, ratio) in result {
            if !ratios.contains_key(&language) {
                order.push(language.clone());
            }
            ratios.entry(language).or_default().push(ratio);
        }
    }
    let builtins = py.import("builtins")?;
    let round = builtins.getattr("round")?;
    let mut merged = Vec::new();
    for language in order {
        let values = &ratios[&language];
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        let rounded: f64 = round.call1((mean, 4))?.extract()?;
        merged.push((language, rounded));
    }
    merged.sort_by(|a, b| b.1.total_cmp(&a.1));
    Ok(PyList::new(py, merged)?.into_any().unbind())
}

#[pyfunction]
fn filter_alt_coherence_matches(
    py: Python<'_>,
    results: Vec<(String, f64)>,
) -> PyResult<Py<PyAny>> {
    let mut order = Vec::new();
    let mut ratios: HashMap<String, Vec<f64>> = HashMap::new();
    for (language, ratio) in &results {
        let normalized = language.replace('—', "");
        if !ratios.contains_key(&normalized) {
            order.push(normalized.clone());
        }
        ratios.entry(normalized).or_default().push(*ratio);
    }
    if ratios.values().any(|values| values.len() > 1) {
        let filtered: Vec<(String, f64)> = order
            .into_iter()
            .map(|language| {
                let best = ratios[&language]
                    .iter()
                    .copied()
                    .max_by(f64::total_cmp)
                    .unwrap_or(0.0);
                (language, best)
            })
            .collect();
        return Ok(PyList::new(py, filtered)?.into_any().unbind());
    }
    Ok(PyList::new(py, results)?.into_any().unbind())
}

#[pyfunction]
fn alpha_unicode_split(py: Python<'_>, decoded_sequence: &str) -> PyResult<Vec<String>> {
    let mut layers: Vec<(String, String)> = Vec::new();
    let mut classifications: HashMap<char, (bool, Option<String>)> = HashMap::new();
    let mut previous_range: Option<String> = None;
    let mut previous_target: Option<usize> = None;

    for character in decoded_sequence.chars() {
        let (alpha, range) = if let Some(cached) = classifications.get(&character) {
            cached.clone()
        } else {
            let text = character.to_string();
            let alpha: bool = pyo3::types::PyString::new(py, &text)
                .call_method0("isalpha")?
                .extract()?;
            let range = if alpha {
                unicode_range(py, &text)?
            } else {
                None
            };
            classifications.insert(character, (alpha, range.clone()));
            (alpha, range)
        };
        if !alpha {
            continue;
        }
        let Some(range) = range else { continue };
        if previous_range.as_ref() == Some(&range) {
            if let Some(target) = previous_target {
                layers[target].1.push(character);
            }
            continue;
        }

        let mut target = None;
        for (index, (discovered, _)) in layers.iter().enumerate() {
            let incompatible =
                suspicious_ranges_impl(py, Some(discovered.as_str()), Some(range.as_str()))?;
            if !incompatible {
                target = Some(index);
                break;
            }
        }
        let target = match target {
            Some(index) => index,
            None => {
                layers.push((range.clone(), String::new()));
                layers.len() - 1
            }
        };
        layers[target].1.push(character);
        previous_range = Some(range);
        previous_target = Some(target);
    }

    layers
        .into_iter()
        .map(|(_, layer)| {
            pyo3::types::PyString::new(py, &layer)
                .call_method0("lower")?
                .extract()
        })
        .collect()
}

#[pyfunction(signature = (decoded_sequence, threshold=0.1, lg_inclusion=None))]
fn coherence_ratio(
    py: Python<'_>,
    decoded_sequence: &str,
    threshold: f64,
    lg_inclusion: Option<&str>,
) -> PyResult<Py<PyAny>> {
    let mut results = Vec::<(String, f64)>::new();
    let mut inclusion: Vec<String> = lg_inclusion
        .map(|value| value.split(',').map(str::to_owned).collect())
        .unwrap_or_default();
    let ignore_non_latin = inclusion.iter().any(|language| language == "Latin Based");
    inclusion.retain(|language| language != "Latin Based");
    let mut sufficient = 0usize;
    let round = py.import("builtins")?.getattr("round")?;

    for layer in alpha_unicode_split(py, decoded_sequence)? {
        let mut counts = HashMap::<String, usize>::new();
        let mut order = Vec::<String>::new();
        for character in layer.chars() {
            let character = character.to_string();
            if !counts.contains_key(&character) {
                order.push(character.clone());
            }
            *counts.entry(character).or_default() += 1;
        }
        if layer.chars().count() <= 32 {
            continue;
        }
        order.sort_by(|a, b| counts[b].cmp(&counts[a]));
        let languages = if inclusion.is_empty() {
            alphabet_languages(py, order.clone(), ignore_non_latin)?
        } else {
            inclusion.clone()
        };
        for language in languages {
            let profile = frequency_profile(py, &language)?;
            let ratio = popularity_compare(&profile, &order);
            if ratio < threshold {
                continue;
            }
            if ratio >= 0.8 {
                sufficient += 1;
            }
            let rounded: f64 = round.call1((ratio, 4))?.extract()?;
            results.push((language, rounded));
            if sufficient >= 3 {
                break;
            }
        }
    }
    let filtered = filter_alt_coherence_matches(py, results)?;
    let mut filtered: Vec<(String, f64)> = filtered.bind(py).extract()?;
    filtered.sort_by(|a, b| b.1.total_cmp(&a.1));
    Ok(PyList::new(py, filtered)?.into_any().unbind())
}

#[pyfunction]
fn encoding_unicode_range(py: Python<'_>, encoding: &str) -> PyResult<Vec<String>> {
    let is_multibyte: bool = py
        .import("charset_normalizer.utils")?
        .getattr("is_multi_byte_encoding")?
        .call1((encoding,))?
        .extract()?;
    if is_multibyte {
        return Err(PyOSError::new_err(
            "Function not supported on multi-byte code page",
        ));
    }
    let decoder = py
        .import("importlib")?
        .getattr("import_module")?
        .call1((format!("encodings.{encoding}"),))?
        .getattr("IncrementalDecoder")?
        .call(
            (),
            Some(&{
                let kwargs = PyDict::new(py);
                kwargs.set_item("errors", "ignore")?;
                kwargs
            }),
        )?;
    let secondary = constants(py)?.getattr("_SECONDARY_RANGE_NAMES")?;
    let mut order = Vec::<String>::new();
    let mut counts = HashMap::<String, usize>::new();
    let mut character_count = 0usize;
    for byte in 0x40u8..0xffu8 {
        let chunk: String = decoder
            .call_method1("decode", (PyBytes::new(py, &[byte]),))?
            .extract()?;
        if chunk.is_empty() {
            continue;
        }
        one_codepoint(&chunk)?;
        let Some(range) = unicode_range(py, &chunk)? else {
            continue;
        };
        if secondary.contains(&range)? {
            continue;
        }
        if !counts.contains_key(&range) {
            order.push(range.clone());
        }
        *counts.entry(range).or_default() += 1;
        character_count += 1;
    }
    let mut result: Vec<String> = order
        .into_iter()
        .filter(|range| counts[range] as f64 / character_count as f64 >= 0.15)
        .collect();
    result.sort();
    Ok(result)
}

#[pyfunction]
fn unicode_range_languages(py: Python<'_>, primary_range: &str) -> PyResult<Vec<String>> {
    let frequencies = constants(py)?
        .getattr("FREQUENCIES")?
        .cast_into::<PyDict>()?;
    let mut languages = Vec::new();
    for (language, characters) in frequencies.iter() {
        let characters: Vec<String> = characters.extract()?;
        for character in characters {
            if unicode_range(py, &character)?.as_deref() == Some(primary_range) {
                languages.push(language.extract()?);
                break;
            }
        }
    }
    Ok(languages)
}

#[pyfunction]
fn encoding_languages(py: Python<'_>, encoding: &str) -> PyResult<Vec<String>> {
    let ranges = match encoding_unicode_range(py, encoding) {
        Ok(ranges) => ranges,
        Err(error) if error.is_instance_of::<PyImportError>(py) => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let Some(primary) = ranges.iter().find(|range| !range.contains("Latin")) else {
        return Ok(vec!["Latin Based".to_owned()]);
    };
    unicode_range_languages(py, primary)
}

fn is_multi_byte_encoding_impl(py: Python<'_>, name: &str) -> PyResult<bool> {
    let module = constants(py)?;
    if module.getattr("_KNOWN_MB_DECODERS")?.contains(name)? {
        return Ok(true);
    }
    let import_module = py.import("importlib")?.getattr("import_module")?;
    let providers = module.getattr("_KNOWN_MB_CLASSES")?;
    for provider in providers.try_iter()? {
        let provider: String = provider?.extract()?;
        let result = import_module
            .call1((&provider,))
            .and_then(|module| module.getattr("getcodec"))
            .and_then(|getcodec| getcodec.call1((name,)));
        match result {
            Ok(_) => return Ok(true),
            Err(error)
                if error.is_instance_of::<PyImportError>(py)
                    || error.is_instance_of::<PyAttributeError>(py)
                    || error.is_instance_of::<PyLookupError>(py) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

#[pyfunction]
fn is_multi_byte_encoding(py: Python<'_>, name: &str) -> PyResult<bool> {
    is_multi_byte_encoding_impl(py, name)
}

#[pyfunction]
fn cp_similarity(py: Python<'_>, encoding_a: &str, encoding_b: &str) -> PyResult<f64> {
    if is_multi_byte_encoding_impl(py, encoding_a)? || is_multi_byte_encoding_impl(py, encoding_b)?
    {
        return Ok(0.0);
    }
    let import_module = py.import("importlib")?.getattr("import_module")?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("errors", "ignore")?;
    let decoder_a = import_module
        .call1((format!("encodings.{encoding_a}"),))?
        .getattr("IncrementalDecoder")?
        .call((), Some(&kwargs))?;
    let decoder_b = import_module
        .call1((format!("encodings.{encoding_b}"),))?
        .getattr("IncrementalDecoder")?
        .call((), Some(&kwargs))?;
    let mut matches = 0usize;
    for byte in 0u8..=255 {
        let value = PyBytes::new(py, &[byte]);
        let a: String = decoder_a.call_method1("decode", (&value,))?.extract()?;
        let b: String = decoder_b.call_method1("decode", (&value,))?.extract()?;
        if a == b {
            matches += 1;
        }
    }
    Ok(matches as f64 / 256.0)
}

fn decode_slice(py: Python<'_>, bytes: &[u8], encoding: &str, errors: &str) -> PyResult<String> {
    PyBytes::new(py, bytes)
        .call_method1("decode", (encoding, errors))?
        .extract()
}

#[pyfunction]
#[allow(clippy::too_many_arguments)]
fn cut_sequence_chunks(
    py: Python<'_>,
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
        py,
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
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn cut_sequence_chunks_impl(
    py: Python<'_>,
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
) -> PyResult<Vec<String>> {
    let mut chunks = Vec::new();
    if let Some(decoded) = decoded_payload.filter(|_| encoding_iana.starts_with("iso2022_")) {
        let chars: Vec<char> = decoded.chars().collect();
        for offset in offsets {
            let decoded_offset = offset * chars.len() / sequences.len();
            let chunk: String = chars.iter().skip(decoded_offset).take(chunk_size).collect();
            if chunk.is_empty() {
                break;
            }
            chunks.push(chunk);
        }
    } else if let Some(decoded) = decoded_payload.filter(|_| !is_multi_byte_decoder) {
        let chars: Vec<char> = decoded.chars().collect();
        for offset in offsets {
            let chunk: String = chars.iter().skip(offset).take(chunk_size).collect();
            if chunk.is_empty() {
                break;
            }
            chunks.push(chunk);
        }
    } else if deferred_decoding {
        let base = if strip_sig_or_bom {
            &sequences[sig_payload.len()..]
        } else {
            sequences
        };
        for offset in offsets {
            let cut = &base[offset.min(base.len())..(offset + chunk_size).min(base.len())];
            if cut.is_empty() {
                break;
            }
            chunks.push(decode_slice(py, cut, encoding_iana, "strict")?);
        }
    } else {
        for offset in offsets {
            let chunk_end = offset + chunk_size;
            if chunk_end > sequences.len() + 8 {
                continue;
            }
            let end = chunk_end.min(sequences.len());
            let mut cut = sequences[offset.min(sequences.len())..end].to_vec();
            if bom_or_sig_available && !strip_sig_or_bom {
                let mut prefixed = sig_payload.to_vec();
                prefixed.extend(cut);
                cut = prefixed;
            }
            let mut chunk = decode_slice(
                py,
                &cut,
                encoding_iana,
                if is_multi_byte_decoder {
                    "ignore"
                } else {
                    "strict"
                },
            )?;
            if is_multi_byte_decoder && offset > 0 {
                let prefix: String = chunk.chars().take(chunk_size.min(16)).collect();
                if let Some(decoded) = decoded_payload {
                    let decoded_len = decoded.chars().count();
                    let expected = offset * decoded_len / sequences.len();
                    let radius: usize = constants(py)?
                        .getattr("_MULTIBYTE_SEARCH_RADIUS")?
                        .extract()?;
                    let start = expected.saturating_sub(radius);
                    let search_end = (expected + radius).min(decoded_len);
                    let found: isize = PyString::new(py, decoded)
                        .call_method1("find", (&prefix, start, search_end))?
                        .extract()?;
                    if found < 0 && !decoded.contains(&prefix) {
                        for delta in 0..4usize {
                            let signed_start = offset as isize - delta as isize;
                            let adjusted_start = if signed_start < 0 {
                                sequences.len().saturating_sub((-signed_start) as usize)
                            } else {
                                signed_start as usize
                            };
                            let mut adjusted =
                                sequences[adjusted_start.min(sequences.len())..end].to_vec();
                            if bom_or_sig_available && !strip_sig_or_bom {
                                let mut prefixed = sig_payload.to_vec();
                                prefixed.extend(adjusted);
                                adjusted = prefixed;
                            }
                            chunk = decode_slice(py, &adjusted, encoding_iana, "ignore")?;
                            let adjusted_prefix: String =
                                chunk.chars().take(chunk_size.min(16)).collect();
                            if decoded.contains(&adjusted_prefix) {
                                break;
                            }
                        }
                    }
                }
            }
            chunks.push(chunk);
        }
    }
    Ok(chunks)
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
    use super::popularity_compare;

    #[test]
    fn popularity_scores_ranked_input() {
        let profile = vec!["e", "t", "a", "o"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let ordered = vec!["e", "e", "t", "a"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(popularity_compare(&profile, &ordered), 1.0);
    }

    #[test]
    fn popularity_empty_input_matches_python_division_shape() {
        assert!(popularity_compare(&["e".to_owned()], &[]).is_nan());
    }
}

use std::collections::HashSet;

use rustc_hash::FxHashMap;

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyByteArray, PyBytes};

use super::codecs::{self, DecodeError, Errors};
use super::mess::mess_ratio_impl;
use super::models::{CharsetMatch, CharsetMatches};
use super::tables::{self, IANA_SUPPORTED_MB_FIRST, TOO_BIG_SEQUENCE, TOO_SMALL_SEQUENCE, TRACE};
use super::{
    coherence, iana_name_impl, log, mb_languages, merge_coherence, py_sum, should_strip_sig_or_bom,
    sig_or_bom, single_byte_languages, specified_encoding, ChunkCutter, DEBUG,
};

fn decode(bytes: &[u8], encoding: &str) -> Result<String, DecodeError> {
    codecs::decode(bytes, encoding, Errors::Strict)
}

#[allow(clippy::too_many_arguments)]
fn new_match(
    py: Python<'_>,
    payload: &Bound<'_, PyAny>,
    encoding: &str,
    chaos: f64,
    bom: bool,
    languages: Vec<(String, f64)>,
    decoded: Option<String>,
    declaration: Option<&str>,
) -> PyResult<Py<CharsetMatch>> {
    Py::new(
        py,
        CharsetMatch::create(
            py,
            payload.clone().unbind(),
            encoding.to_owned(),
            chaos,
            bom,
            languages,
            decoded,
            declaration.map(str::to_owned),
        )?,
    )
}

fn new_matches(py: Python<'_>, entries: Vec<Py<CharsetMatch>>) -> PyResult<Py<CharsetMatches>> {
    Py::new(py, CharsetMatches::from_results(py, entries)?)
}

#[pyfunction(signature = (sequences, steps=5, chunk_size=512, threshold=0.2, cp_isolation=None, cp_exclusion=None, preemptive_behaviour=true, explain=false, language_threshold=0.1, enable_fallback=true))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn from_bytes(
    py: Python<'_>,
    sequences: &Bound<'_, PyAny>,
    mut steps: usize,
    mut chunk_size: usize,
    threshold: f64,
    cp_isolation: Option<Vec<String>>,
    cp_exclusion: Option<Vec<String>>,
    preemptive_behaviour: bool,
    explain: bool,
    language_threshold: f64,
    enable_fallback: bool,
) -> PyResult<Py<CharsetMatches>> {
    if !sequences.is_instance_of::<PyBytes>() && !sequences.is_instance_of::<PyByteArray>() {
        return Err(PyTypeError::new_err(format!(
            "Expected object of type bytes or bytearray, got: {}",
            sequences.get_type()
        )));
    }
    let owned_bytes: Vec<u8>;
    let bytes: &[u8] = if let Ok(value) = sequences.cast::<PyBytes>() {
        value.as_bytes()
    } else {
        owned_bytes = sequences.extract()?;
        &owned_bytes
    };
    let length = bytes.len();

    if length == 0 {
        log(py, DEBUG, || {
            "Encoding detection on empty bytes, assuming utf_8 intention.".to_owned()
        })?;
        let entry = new_match(
            py,
            sequences,
            "utf_8",
            0.0,
            false,
            Vec::new(),
            Some(String::new()),
            None,
        )?;
        return new_matches(py, vec![entry]);
    }

    let normalize = |values: Option<Vec<String>>| -> PyResult<Vec<String>> {
        values
            .unwrap_or_default()
            .into_iter()
            .map(|value| iana_name_impl(&value, false))
            .collect()
    };
    let isolation = normalize(cp_isolation)?;
    let exclusion = normalize(cp_exclusion)?;

    if length <= chunk_size.saturating_mul(steps) {
        steps = 1;
        chunk_size = length;
    }
    if steps > 1 && length / steps < chunk_size {
        chunk_size = length / steps;
    }
    let is_too_small = length < TOO_SMALL_SEQUENCE;
    let is_too_large = length >= TOO_BIG_SEQUENCE;
    if is_too_small {
        log(py, TRACE, || {
            format!("Trying to detect encoding from a tiny portion of ({length}) byte(s).")
        })?;
    }

    let specified = if preemptive_behaviour {
        specified_encoding(bytes, 8192)
    } else {
        None
    };
    let mut prioritized = Vec::<&str>::new();
    if let Some(value) = specified {
        prioritized.push(value);
    }
    let (sig_encoding, sig) = sig_or_bom(bytes);
    if let Some(value) = sig_encoding {
        prioritized.insert(0, value);
    }
    prioritized.push("ascii");
    if !prioritized.contains(&"utf_8") {
        prioritized.push("utf_8");
    }
    prioritized.extend_from_slice(IANA_SUPPORTED_MB_FIRST);

    let mut results = CharsetMatches::from_results(py, Vec::new())?;
    let mut early_results = CharsetMatches::from_results(py, Vec::new())?;
    let mut tested = HashSet::<&str>::new();
    let mut soft_skip = HashSet::<&str>::new();
    let mut fallback_ascii: Option<Py<CharsetMatch>> = None;
    let mut fallback_utf8: Option<Py<CharsetMatch>> = None;
    let mut fallback_specified: Option<Py<CharsetMatch>> = None;
    let mut definitive = false;
    let mut definitive_languages = HashSet::<&str>::new();
    let mut post_definitive_success = 0usize;
    let mut multibyte_definitive = false;
    let mut mess_cache = FxHashMap::<String, f64>::default();
    // inclusion -> chunk -> coherence results
    let mut coherence_cache =
        FxHashMap::<String, FxHashMap<String, Vec<(&'static str, f64)>>>::default();
    let explain_mess = explain && (1..=2).contains(&isolation.len());

    for encoding in prioritized {
        if (!isolation.is_empty() && !isolation.iter().any(|value| value == encoding))
            || exclusion.iter().any(|value| value == encoding)
            || tested.contains(encoding)
        {
            continue;
        }
        tested.insert(encoding);
        let bom = sig_encoding == Some(encoding);
        let strip_bom = bom && should_strip_sig_or_bom(encoding);
        if matches!(encoding, "utf_16" | "utf_32" | "utf_7") && !bom {
            continue;
        }
        if soft_skip.contains(encoding) {
            continue;
        }
        if !codecs::is_known(encoding) {
            continue; // no codec available
        }
        let multibyte = tables::is_multi_byte_encoding(encoding);
        let target_languages: Vec<&str> = if multibyte {
            mb_languages(encoding)
        } else {
            single_byte_languages(encoding)
        };
        if definitive
            && !target_languages
                .iter()
                .any(|language| definitive_languages.contains(language))
        {
            continue;
        }
        if definitive && !multibyte && post_definitive_success >= 7 {
            continue;
        }
        if multibyte_definitive && !multibyte {
            continue;
        }

        let deferred = !multibyte && !is_too_large;
        let source = if strip_bom {
            &bytes[sig.len()..]
        } else {
            bytes
        };
        let mut decoded: Option<String> = None;
        let initial_decode = if is_too_large && !multibyte {
            decode(&source[..source.len().min(500_000)], encoding).map(|_| ())
        } else if !deferred {
            let decode_source = if encoding == "utf_7" && bom {
                bytes
            } else {
                source
            };
            decode(decode_source, encoding).map(|mut value| {
                if encoding == "utf_7" && bom && value.starts_with('\u{feff}') {
                    value.remove(0);
                }
                decoded = Some(value);
            })
        } else {
            Ok(())
        };
        if initial_decode.is_err() {
            continue;
        }

        let step = length / steps;
        let offset_start = if bom { sig.len() } else { 0 };
        let offsets: Vec<usize> = (offset_start..length).step_by(step).collect();
        let multibyte_bonus = multibyte
            && decoded
                .as_ref()
                .is_some_and(|value| value.chars().count() < length);
        let max_give_up = (offsets.len() / 4).max(2);
        let mut early_stop = 0usize;
        let mut cutter = ChunkCutter::new(
            bytes,
            encoding,
            offsets,
            chunk_size,
            bom,
            strip_bom,
            sig,
            multibyte,
            decoded.as_deref(),
            deferred,
        );
        if cutter.validate().is_err() {
            continue;
        }
        let mut chunks = Vec::new();
        let mut ratios = Vec::new();
        let mut failed = false;
        for chunk in cutter.by_ref() {
            let Ok(chunk) = chunk else {
                failed = true;
                break;
            };
            let ratio = match mess_cache.get(&chunk) {
                Some(value) => *value,
                None => {
                    let value = mess_ratio_impl(py, &chunk, threshold, explain_mess)?;
                    mess_cache.insert(chunk.clone(), value);
                    value
                }
            };
            chunks.push(chunk);
            ratios.push(ratio);
            if ratio >= threshold {
                early_stop += 1;
            }
            if early_stop >= max_give_up || (bom && !strip_bom) {
                break;
            }
        }
        drop(cutter);
        if failed {
            continue;
        }
        let mean = if ratios.is_empty() {
            0.0
        } else {
            py_sum(&ratios) / ratios.len() as f64
        };
        if is_too_large
            && !multibyte
            && mean < threshold
            && early_stop < max_give_up
            && decode(&bytes[50_000.min(length)..], encoding).is_err()
        {
            continue;
        }
        if mean >= threshold || early_stop >= max_give_up {
            soft_skip.extend(tables::similar_encodings(encoding).iter().copied());
            if enable_fallback
                && (encoding == "ascii"
                    || encoding == "utf_8"
                    || specified == Some(encoding)
                    || encoding == "utf_16"
                    || encoding == "utf_32")
            {
                if decoded.is_none() {
                    match decode(source, encoding) {
                        Ok(value) => decoded = if is_too_large { None } else { Some(value) },
                        Err(_) => continue,
                    }
                }
                let fallback = new_match(
                    py,
                    sequences,
                    encoding,
                    threshold,
                    bom,
                    Vec::new(),
                    decoded,
                    specified,
                )?;
                if specified == Some(encoding) {
                    fallback_specified = Some(fallback);
                } else if encoding == "ascii" {
                    fallback_ascii = Some(fallback);
                } else {
                    fallback_utf8 = Some(fallback);
                }
            }
            continue;
        }
        if deferred {
            match decode(source, encoding) {
                Ok(value) => decoded = Some(value),
                Err(_) => continue,
            }
        }

        let inclusion = target_languages.join(",");
        let mut coherence_results = Vec::new();
        if encoding != "ascii" {
            let cache = coherence_cache.entry(inclusion.clone()).or_default();
            for chunk in &chunks {
                let values = match cache.get(chunk.as_str()) {
                    Some(value) => value.clone(),
                    None => {
                        let value = coherence(
                            chunk,
                            language_threshold,
                            (!inclusion.is_empty()).then_some(inclusion.as_str()),
                        )?;
                        cache.insert(chunk.clone(), value.clone());
                        value
                    }
                };
                coherence_results.push(values);
            }
        }
        let merged = merge_coherence(coherence_results);
        let best_coherence = merged.iter().map(|item| item.1).fold(0.0, f64::max);
        let retained_decoded = if !is_too_large
            || specified == Some(encoding)
            || matches!(encoding, "ascii" | "utf_8")
        {
            decoded.clone()
        } else {
            None
        };
        let current = new_match(
            py,
            sequences,
            encoding,
            mean,
            bom,
            merged
                .iter()
                .map(|(language, ratio)| ((*language).to_owned(), *ratio))
                .collect(),
            retained_decoded,
            specified,
        )?;
        results.push(py, current.clone_ref(py))?;
        if definitive && !multibyte && mean < 0.02 {
            post_definitive_success += 1;
        }
        if (specified == Some(encoding) || matches!(encoding, "ascii" | "utf_8")) && mean < 0.1 {
            if mean == 0.0 {
                log(py, DEBUG, || {
                    format!("Encoding detection: {encoding} is most likely the one.")
                })?;
                return new_matches(py, vec![current]);
            }
            early_results.push(py, current.clone_ref(py))?;
        }
        if early_results.len() > 0
            && specified.is_none_or(|value| tested.contains(value))
            && tested.contains("ascii")
            && tested.contains("utf_8")
        {
            let best = early_results.best_native(py)?;
            return new_matches(py, best.into_iter().collect());
        }
        if !definitive
            && !multibyte
            && best_coherence >= 0.5
            && tested.contains("ascii")
            && tested.contains("utf_8")
        {
            definitive = true;
            definitive_languages.extend(target_languages.iter().copied());
        }
        if !multibyte_definitive
            && multibyte
            && multibyte_bonus
            && decoded
                .as_ref()
                .is_some_and(|value| (value.chars().count() as f64) < length as f64 * 0.98)
            && !matches!(
                encoding,
                "utf_8"
                    | "utf_8_sig"
                    | "utf_16"
                    | "utf_16_be"
                    | "utf_16_le"
                    | "utf_32"
                    | "utf_32_be"
                    | "utf_32_le"
                    | "utf_7"
            )
            && tested.contains("ascii")
            && tested.contains("utf_8")
        {
            multibyte_definitive = true;
        }
        if sig_encoding == Some(encoding) {
            return new_matches(py, vec![current]);
        }
    }

    if results.len() == 0 {
        if let Some(value) = fallback_specified.or(fallback_utf8).or(fallback_ascii) {
            results.push(py, value)?;
        }
    }
    if results.len() > 0 {
        let alternatives = results.len() - 1;
        if let Some(best) = results.best_native(py)? {
            let encoding = best.bind(py).borrow().encoding_name().to_owned();
            log(py, DEBUG, || {
                format!(
                    "Encoding detection: Found {encoding} as plausible (best-candidate) for content. With {alternatives} alternatives."
                )
            })?;
        }
    } else {
        log(py, DEBUG, || {
            "Encoding detection: Unable to determine any suitable charset.".to_owned()
        })?;
    }
    Py::new(py, results)
}

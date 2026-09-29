use std::collections::{HashMap, HashSet};

use pyo3::exceptions::{PyLookupError, PyTypeError, PyUnicodeDecodeError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyByteArray, PyBytes, PyDict, PyList};

use super::mess::mess_ratio;
use super::{
    coherence_ratio, constants, cut_sequence_chunks_impl, iana_name, identify_sig_or_bom,
    merge_coherence_ratios, should_strip_sig_or_bom,
};

fn decode(py: Python<'_>, bytes: &[u8], encoding: &str) -> PyResult<String> {
    PyBytes::new(py, bytes)
        .call_method1("decode", (encoding, "strict"))?
        .extract()
}

#[allow(clippy::too_many_arguments)]
fn new_match(
    _py: Python<'_>,
    models: &Bound<'_, PyAny>,
    payload: &Bound<'_, PyAny>,
    encoding: &str,
    chaos: f64,
    bom: bool,
    languages: Vec<(String, f64)>,
    decoded: Option<String>,
    declaration: Option<&str>,
) -> PyResult<Py<PyAny>> {
    Ok(models
        .getattr("CharsetMatch")?
        .call1((
            payload,
            encoding,
            chaos,
            bom,
            languages,
            decoded,
            declaration,
        ))?
        .unbind())
}

fn new_matches(
    py: Python<'_>,
    models: &Bound<'_, PyAny>,
    entries: Vec<Py<PyAny>>,
) -> PyResult<Py<PyAny>> {
    Ok(models
        .getattr("CharsetMatches")?
        .call1((PyList::new(py, entries)?,))?
        .unbind())
}

fn log_message(
    logger: &Bound<'_, PyAny>,
    level: &Bound<'_, PyAny>,
    message: impl Into<String>,
) -> PyResult<()> {
    logger.call_method1("log", (level, message.into()))?;
    Ok(())
}

fn is_decode_failure(py: Python<'_>, error: &PyErr) -> bool {
    error.is_instance_of::<PyUnicodeDecodeError>(py) || error.is_instance_of::<PyLookupError>(py)
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
) -> PyResult<Py<PyAny>> {
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
    let models = py.import("charset_normalizer.models")?.into_any();
    let cd_module = py.import("charset_normalizer.cd")?;
    let utils_module = py.import("charset_normalizer.utils")?;
    let logger = py
        .import("logging")?
        .getattr("getLogger")?
        .call1(("charset_normalizer",))?;
    let trace = constants(py)?.getattr("TRACE")?;

    if length == 0 {
        logger.call_method1(
            "debug",
            ("Encoding detection on empty bytes, assuming utf_8 intention.",),
        )?;
        let entry = new_match(
            py,
            &models,
            sequences,
            "utf_8",
            0.0,
            false,
            Vec::new(),
            Some(String::new()),
            None,
        )?;
        return new_matches(py, &models, vec![entry]);
    }

    let normalize = |values: Option<Vec<String>>| -> PyResult<Vec<String>> {
        values
            .unwrap_or_default()
            .into_iter()
            .map(|value| iana_name(py, &value, false))
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
    let constants_module = constants(py)?;
    let too_small: usize = constants_module.getattr("TOO_SMALL_SEQUENCE")?.extract()?;
    let too_big: usize = constants_module.getattr("TOO_BIG_SEQUENCE")?.extract()?;
    let is_too_small = length < too_small;
    let is_too_large = length >= too_big;
    if is_too_small {
        log_message(
            &logger,
            &trace,
            format!("Trying to detect encoding from a tiny portion of ({length}) byte(s)."),
        )?;
    }

    let specified = if preemptive_behaviour {
        super::any_specified_encoding(py, sequences, 8192)?
    } else {
        None
    };
    let mut prioritized = Vec::<String>::new();
    if let Some(value) = &specified {
        prioritized.push(value.clone());
    }
    let (sig_encoding, sig_payload) = identify_sig_or_bom(py, sequences)?;
    let sig: Vec<u8> = sig_payload.extract()?;
    if let Some(value) = &sig_encoding {
        prioritized.insert(0, value.clone());
    }
    prioritized.push("ascii".to_owned());
    if !prioritized.iter().any(|value| value == "utf_8") {
        prioritized.push("utf_8".to_owned());
    }
    let supported: Vec<String> = py
        .import("charset_normalizer.api")?
        .getattr("IANA_SUPPORTED_MB_FIRST")?
        .extract()?;
    prioritized.extend(supported);

    let models_class = models.getattr("CharsetMatches")?;
    let results = models_class.call0()?;
    let early_results = models_class.call0()?;
    let mut tested = HashSet::<String>::new();
    let mut soft_skip = HashSet::<String>::new();
    let similar = constants_module
        .getattr("IANA_SUPPORTED_SIMILAR")?
        .cast_into::<PyDict>()?;
    let mut fallback_ascii: Option<Py<PyAny>> = None;
    let mut fallback_utf8: Option<Py<PyAny>> = None;
    let mut fallback_specified: Option<Py<PyAny>> = None;
    let mut definitive = false;
    let mut definitive_languages = HashSet::<String>::new();
    let mut post_definitive_success = 0usize;
    let mut multibyte_definitive = false;
    let mut mess_cache = HashMap::<String, f64>::new();
    let mut coherence_cache = HashMap::<(String, String), Vec<(String, f64)>>::new();

    for encoding in prioritized {
        if (!isolation.is_empty() && !isolation.contains(&encoding))
            || exclusion.contains(&encoding)
            || tested.contains(&encoding)
        {
            continue;
        }
        tested.insert(encoding.clone());
        let bom = sig_encoding.as_deref() == Some(encoding.as_str());
        let strip_bom = bom && should_strip_sig_or_bom(&encoding);
        if matches!(encoding.as_str(), "utf_16" | "utf_32" | "utf_7") && !bom {
            continue;
        }
        if soft_skip.contains(&encoding) {
            continue;
        }
        let multibyte = match utils_module
            .getattr("is_multi_byte_encoding")?
            .call1((&encoding,))
            .and_then(|value| value.extract())
        {
            Ok(value) => value,
            Err(error)
                if error.is_instance_of::<pyo3::exceptions::PyImportError>(py)
                    || error.is_instance_of::<pyo3::exceptions::PyModuleNotFoundError>(py) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        let target_languages: Vec<String> = if multibyte {
            cd_module
                .getattr("mb_encoding_languages")?
                .call1((&encoding,))?
                .extract()?
        } else {
            cd_module
                .getattr("encoding_languages")?
                .call1((&encoding,))?
                .extract()?
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
            decode(py, &source[..source.len().min(500_000)], &encoding).map(|_| ())
        } else if !deferred {
            let decode_source = if encoding == "utf_7" && bom {
                bytes
            } else {
                source
            };
            decode(py, decode_source, &encoding).map(|mut value| {
                if encoding == "utf_7" && bom && value.starts_with('\u{feff}') {
                    value.remove(0);
                }
                decoded = Some(value);
            })
        } else {
            Ok(())
        };
        if let Err(error) = initial_decode {
            if is_decode_failure(py, &error) {
                continue;
            }
            return Err(error);
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
        let mut chunks = Vec::<String>::new();
        let sampled = cut_sequence_chunks_impl(
            py,
            bytes,
            &encoding,
            offsets,
            chunk_size,
            bom,
            strip_bom,
            &sig,
            multibyte,
            decoded.as_deref(),
            deferred,
        );
        let sampled = match sampled {
            Ok(value) => value,
            Err(error) if is_decode_failure(py, &error) => continue,
            Err(error) => return Err(error),
        };
        let mut ratios = Vec::new();
        for chunk in sampled {
            chunks.push(chunk.clone());
            let ratio = if let Some(value) = mess_cache.get(&chunk) {
                *value
            } else {
                let value = mess_ratio(
                    py,
                    &chunk,
                    threshold,
                    explain && (1..=2).contains(&isolation.len()),
                )?;
                mess_cache.insert(chunk, value);
                value
            };
            ratios.push(ratio);
            if ratio >= threshold {
                early_stop += 1;
            }
            if early_stop >= max_give_up || (bom && !strip_bom) {
                break;
            }
        }
        let mean = if ratios.is_empty() {
            0.0
        } else {
            let total: f64 = py
                .import("builtins")?
                .getattr("sum")?
                .call1((PyList::new(py, &ratios)?,))?
                .extract()?;
            total / ratios.len() as f64
        };
        if is_too_large && !multibyte && mean < threshold && early_stop < max_give_up {
            if let Err(error) = decode(py, &bytes[50_000.min(length)..], &encoding) {
                if is_decode_failure(py, &error) {
                    continue;
                }
                return Err(error);
            }
        }
        if mean >= threshold || early_stop >= max_give_up {
            if let Some(values) = similar.get_item(&encoding)? {
                for value in values.try_iter()? {
                    soft_skip.insert(value?.extract()?);
                }
            }
            if enable_fallback
                && (encoding == "ascii"
                    || encoding == "utf_8"
                    || specified.as_deref() == Some(encoding.as_str())
                    || encoding == "utf_16"
                    || encoding == "utf_32")
            {
                if decoded.is_none() {
                    match decode(py, source, &encoding) {
                        Ok(value) => decoded = if is_too_large { None } else { Some(value) },
                        Err(error) if is_decode_failure(py, &error) => continue,
                        Err(error) => return Err(error),
                    }
                }
                let fallback = new_match(
                    py,
                    &models,
                    sequences,
                    &encoding,
                    threshold,
                    bom,
                    Vec::new(),
                    decoded,
                    specified.as_deref(),
                )?;
                if specified.as_deref() == Some(encoding.as_str()) {
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
            decoded = match decode(py, source, &encoding) {
                Ok(value) => Some(value),
                Err(error) if is_decode_failure(py, &error) => continue,
                Err(error) => return Err(error),
            };
        }

        let inclusion = if target_languages.is_empty() {
            String::new()
        } else {
            target_languages.join(",")
        };
        let mut coherence_results = Vec::new();
        if encoding != "ascii" {
            for chunk in &chunks {
                let key = (chunk.clone(), inclusion.clone());
                let values = if let Some(value) = coherence_cache.get(&key) {
                    value.clone()
                } else {
                    let value = coherence_ratio(
                        py,
                        chunk,
                        language_threshold,
                        if inclusion.is_empty() {
                            None
                        } else {
                            Some(inclusion.as_str())
                        },
                    )?;
                    let value: Vec<(String, f64)> = value.bind(py).extract()?;
                    coherence_cache.insert(key, value.clone());
                    value
                };
                coherence_results.push(values);
            }
        }
        let merged = merge_coherence_ratios(py, coherence_results)?;
        let merged: Vec<(String, f64)> = merged.bind(py).extract()?;
        let retained_decoded = if !is_too_large
            || specified.as_deref() == Some(encoding.as_str())
            || matches!(encoding.as_str(), "ascii" | "utf_8")
        {
            decoded.clone()
        } else {
            None
        };
        let current = new_match(
            py,
            &models,
            sequences,
            &encoding,
            mean,
            bom,
            merged.clone(),
            retained_decoded,
            specified.as_deref(),
        )?;
        results.call_method1("append", (current.bind(py),))?;
        if definitive && !multibyte && mean < 0.02 {
            post_definitive_success += 1;
        }
        if (specified.as_deref() == Some(encoding.as_str())
            || matches!(encoding.as_str(), "ascii" | "utf_8"))
            && mean < 0.1
        {
            if mean == 0.0 {
                logger.call_method1(
                    "debug",
                    (format!(
                        "Encoding detection: {encoding} is most likely the one."
                    ),),
                )?;
                return new_matches(py, &models, vec![current]);
            }
            early_results.call_method1("append", (current.bind(py),))?;
        }
        let early_len = early_results.len()?;
        if early_len > 0
            && (specified.is_none()
                || specified
                    .as_ref()
                    .is_some_and(|value| tested.contains(value)))
            && tested.contains("ascii")
            && tested.contains("utf_8")
        {
            let best = early_results.call_method0("best")?;
            return new_matches(py, &models, vec![best.unbind()]);
        }
        if !definitive && !multibyte {
            let best_coherence = merged.iter().map(|item| item.1).fold(0.0, f64::max);
            if best_coherence >= 0.5 && tested.contains("ascii") && tested.contains("utf_8") {
                definitive = true;
                definitive_languages.extend(target_languages.iter().cloned());
            }
        }
        if !multibyte_definitive
            && multibyte
            && multibyte_bonus
            && decoded
                .as_ref()
                .is_some_and(|value| (value.chars().count() as f64) < length as f64 * 0.98)
            && !matches!(
                encoding.as_str(),
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
        if sig_encoding.as_deref() == Some(encoding.as_str()) {
            return new_matches(py, &models, vec![current]);
        }
    }

    if results.len()? == 0 {
        let fallback = fallback_specified.or(fallback_utf8).or(fallback_ascii);
        if let Some(value) = fallback {
            results.call_method1("append", (value.bind(py),))?;
        }
    }
    if results.len()? > 0 {
        let best = results.call_method0("best")?;
        let encoding: String = best.getattr("encoding")?.extract()?;
        logger.call_method1(
            "debug",
            (format!(
                "Encoding detection: Found {encoding} as plausible (best-candidate) for content. With {} alternatives.",
                results.len()? - 1
            ),),
        )?;
    } else {
        logger.call_method1(
            "debug",
            ("Encoding detection: Unable to determine any suitable charset.",),
        )?;
    }
    Ok(results.unbind())
}

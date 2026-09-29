//! Python bindings: exposes `charset-norm` as `charset_norm._native`.

use charset_norm::codecs::DecodeError;
use charset_norm::{Error, chunks, coherence, encoding, mess, unicode};
use pyo3::exceptions::{
    PyImportError, PyKeyError, PyLookupError, PyOSError, PyTypeError, PyUnicodeDecodeError,
    PyValueError, PyZeroDivisionError,
};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyByteArray, PyBytes};

mod api;
mod logging;
mod models;

use logging::PyLogger;

/// Detection allocates many short-lived strings; mimalloc handles that
/// pattern noticeably faster than the system allocators.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/* ---------------------------------------------------------------------- */
/* Conversions                                                             */
/* ---------------------------------------------------------------------- */

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

pub(crate) fn to_py_error(error: Error) -> PyErr {
    match error {
        Error::MultiByteEncoding(_) => PyOSError::new_err(error.to_string()),
        Error::UnknownRange(name) => PyKeyError::new_err(name),
        other => PyValueError::new_err(other.to_string()),
    }
}

/// Codec lookups used to import `encodings.<name>`; keep the exception type.
fn codec_error(error: Error) -> PyErr {
    match error {
        Error::UnknownEncoding(name) => {
            PyImportError::new_err(format!("No module named 'encodings.{name}'"))
        }
        other => to_py_error(other),
    }
}

fn bytes_of<'a>(sequence: &'a Bound<'_, PyAny>, owned: &'a mut Vec<u8>) -> PyResult<&'a [u8]> {
    if let Ok(value) = sequence.cast::<PyBytes>() {
        return Ok(value.as_bytes());
    }
    *owned = sequence.extract()?;
    Ok(owned)
}

/* ---------------------------------------------------------------------- */
/* utils.py                                                                */
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

#[pyfunction]
fn is_suspiciously_successive_range(
    unicode_range_a: Option<&str>,
    unicode_range_b: Option<&str>,
) -> PyResult<bool> {
    unicode::is_suspiciously_successive_range(unicode_range_a, unicode_range_b).map_err(to_py_error)
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
    Ok(encoding::any_specified_encoding(
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
    encoding::should_strip_sig_or_bom(iana_encoding)
}

#[pyfunction(signature = (cp_name, strict=true))]
fn iana_name(cp_name: &str, strict: bool) -> PyResult<String> {
    encoding::iana_name(cp_name, strict).map_err(to_py_error)
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
    let (encoding, mark) = encoding::identify_sig_or_bom(raw);
    Ok((encoding, PyBytes::new(py, mark)))
}

#[pyfunction]
fn is_cp_similar(iana_name_a: &str, iana_name_b: &str) -> bool {
    encoding::is_cp_similar(iana_name_a, iana_name_b)
}

#[pyfunction]
fn is_multi_byte_encoding(name: &str) -> bool {
    encoding::is_multi_byte_encoding(name)
}

#[pyfunction]
fn cp_similarity(encoding_a: &str, encoding_b: &str) -> PyResult<f64> {
    encoding::cp_similarity(encoding_a, encoding_b).map_err(codec_error)
}

#[pyfunction]
#[pyo3(signature = (sequences, encoding_iana, offsets, chunk_size, bom_or_sig_available, strip_sig_or_bom, sig_payload, is_multi_byte_decoder, decoded_payload=None, deferred_decoding=false))]
#[expect(
    clippy::too_many_arguments,
    clippy::fn_params_excessive_bools,
    reason = "mirrors the Python signature of utils.cut_sequence_chunks"
)]
fn cut_sequence_chunks(
    sequences: &Bound<'_, PyAny>,
    encoding_iana: &str,
    offsets: Vec<usize>,
    chunk_size: usize,
    bom_or_sig_available: bool,
    strip_sig_or_bom: bool,
    sig_payload: &Bound<'_, PyAny>,
    is_multi_byte_decoder: bool,
    decoded_payload: Option<&str>,
    deferred_decoding: bool,
) -> PyResult<Vec<String>> {
    let (mut owned_sequences, mut owned_sig) = (Vec::new(), Vec::new());
    let sequences = bytes_of(sequences, &mut owned_sequences)?;
    let sig_payload = bytes_of(sig_payload, &mut owned_sig)?;
    chunks::cut_sequence_chunks(
        sequences,
        encoding_iana,
        offsets,
        chunk_size,
        chunks::Signature::from_flags(bom_or_sig_available, strip_sig_or_bom, sig_payload),
        chunks::ChunkSource::select(
            encoding_iana,
            decoded_payload,
            is_multi_byte_decoder,
            deferred_decoding,
        ),
    )
    .map_err(|error| decode_error(encoding_iana, error))
}

/* ---------------------------------------------------------------------- */
/* cd.py                                                                   */
/* ---------------------------------------------------------------------- */

#[pyfunction]
fn mb_encoding_languages(iana_name: &str) -> Vec<&'static str> {
    encoding::mb_encoding_languages(iana_name)
}

#[pyfunction]
fn encoding_unicode_range(encoding: &str) -> PyResult<Vec<&'static str>> {
    encoding::encoding_unicode_range(encoding).map_err(codec_error)
}

#[pyfunction]
fn unicode_range_languages(primary_range: &str) -> Vec<&'static str> {
    encoding::unicode_range_languages(primary_range)
}

#[pyfunction]
fn encoding_languages(encoding: &str) -> PyResult<Vec<&'static str>> {
    encoding::encoding_languages(encoding).map_err(to_py_error)
}

#[pyfunction]
fn get_target_features(language: &str) -> PyResult<(bool, bool)> {
    coherence::get_target_features(language).map_err(to_py_error)
}

#[pyfunction(signature = (characters, ignore_non_latin=false))]
#[expect(
    clippy::needless_pass_by_value,
    reason = "PyO3 extracts sequence arguments as owned values"
)]
fn alphabet_languages(
    characters: Vec<String>,
    ignore_non_latin: bool,
) -> PyResult<Vec<&'static str>> {
    let characters = characters
        .iter()
        .map(|character| one_char(character))
        .collect::<PyResult<Vec<char>>>()?;
    Ok(coherence::alphabet_languages(&characters, ignore_non_latin))
}

#[pyfunction]
#[expect(
    clippy::needless_pass_by_value,
    reason = "PyO3 extracts sequence arguments as owned values"
)]
fn characters_popularity_compare(language: &str, ordered_characters: Vec<String>) -> PyResult<f64> {
    if ordered_characters.is_empty() {
        return Err(PyZeroDivisionError::new_err("division by zero"));
    }
    // Strings that are not a single character can never match a profile
    // entry; U+FFFF (a noncharacter) stands in for them.
    let ordered: Vec<char> = ordered_characters
        .iter()
        .map(|value| one_char(value).unwrap_or('\u{ffff}'))
        .collect();
    coherence::characters_popularity_compare(language, &ordered).map_err(to_py_error)
}

#[pyfunction]
fn merge_coherence_ratios(results: Vec<Vec<(String, f64)>>) -> Vec<(String, f64)> {
    coherence::merge_coherence_ratios(results)
}

#[pyfunction]
fn filter_alt_coherence_matches(results: Vec<(String, f64)>) -> Vec<(String, f64)> {
    coherence::filter_alt_coherence_matches(results)
}

#[pyfunction]
fn alpha_unicode_split(decoded_sequence: &str) -> Vec<String> {
    coherence::alpha_unicode_split(decoded_sequence)
}

#[pyfunction(signature = (decoded_sequence, threshold=0.1, lg_inclusion=None))]
fn coherence_ratio(
    decoded_sequence: &str,
    threshold: f64,
    lg_inclusion: Option<&str>,
) -> PyResult<Vec<(&'static str, f64)>> {
    coherence::coherence_ratio(decoded_sequence, threshold, lg_inclusion).map_err(to_py_error)
}

/* ---------------------------------------------------------------------- */
/* md.py                                                                   */
/* ---------------------------------------------------------------------- */

#[pyfunction(signature = (decoded_sequence, maximum_threshold=0.2, debug=false))]
fn mess_ratio(
    py: Python<'_>,
    decoded_sequence: &str,
    maximum_threshold: f64,
    debug: bool,
) -> PyResult<f64> {
    let logger = PyLogger::new(py)?;
    let ratio = mess::mess_ratio_with(decoded_sequence, maximum_threshold, debug, &logger);
    logger.finish()?;
    Ok(ratio)
}

#[pymodule(gil_used = false)]
mod _native {
    #[pymodule_export]
    use super::api::from_bytes;
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
        mb_encoding_languages, merge_coherence_ratios, mess_ratio, remove_accent,
        should_strip_sig_or_bom, unicode_range, unicode_range_languages,
    };

    #[pymodule_export]
    #[allow(non_upper_case_globals)]
    const __version__: &str = super::VERSION;
}

//! `charset_norm.api.from_bytes`, delegating to the core detector.

use charset_norm::DetectionOptions;
use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyByteArray, PyBytes};

use crate::logging::PyLogger;
use crate::models::{payload_bytes, CharsetMatches};

#[pyfunction(signature = (sequences, steps=5, chunk_size=512, threshold=0.2, cp_isolation=None, cp_exclusion=None, preemptive_behaviour=true, explain=false, language_threshold=0.1, enable_fallback=true))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn from_bytes(
    py: Python<'_>,
    sequences: &Bound<'_, PyAny>,
    steps: usize,
    chunk_size: usize,
    threshold: f64,
    cp_isolation: Option<Vec<String>>,
    cp_exclusion: Option<Vec<String>>,
    preemptive_behaviour: bool,
    explain: bool,
    language_threshold: f64,
    enable_fallback: bool,
) -> PyResult<CharsetMatches> {
    if !sequences.is_instance_of::<PyBytes>() && !sequences.is_instance_of::<PyByteArray>() {
        return Err(PyTypeError::new_err(format!(
            "Expected object of type bytes or bytearray, got: {}",
            sequences.get_type()
        )));
    }
    let options = DetectionOptions {
        steps,
        chunk_size,
        threshold,
        cp_isolation: cp_isolation.unwrap_or_default(),
        cp_exclusion: cp_exclusion.unwrap_or_default(),
        preemptive_behaviour,
        explain,
        language_threshold,
        enable_fallback,
    };
    let logger = PyLogger::new(py)?;
    let results = charset_norm::detect(payload_bytes(sequences)?, &options, &logger);
    logger.finish()?;
    CharsetMatches::wrap(py, results, sequences)
}

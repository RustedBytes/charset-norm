//! Python result classes, backed by the core `CharsetMatch`.

use std::fmt::Write as _;
use std::sync::Arc;

use charset_norm::codecs::{self, DecodeError};
use charset_norm::{OutputError, encoding, sort_by_rank};
use pyo3::basic::CompareOp;
use pyo3::exceptions::{PyIndexError, PyKeyError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyByteArray, PyBytes, PyDict, PyList, PyString};

use crate::{decode_error, to_py_error};

/// Copy a `bytes`/`bytearray` (or buffer) payload for the core.
pub(crate) fn payload_bytes(payload: &Bound<'_, PyAny>) -> PyResult<Arc<[u8]>> {
    if let Ok(value) = payload.cast::<PyBytes>() {
        return Ok(Arc::from(value.as_bytes()));
    }
    if let Ok(value) = payload.cast::<PyByteArray>() {
        return Ok(Arc::from(value.to_vec()));
    }
    let owned: Vec<u8> = payload.extract()?;
    Ok(Arc::from(owned))
}

#[pyclass(module = "charset_norm.models", subclass)]
pub(crate) struct CharsetMatch {
    inner: charset_norm::CharsetMatch,
    payload: Py<PyAny>,
    leaves: Vec<Py<CharsetMatch>>,
    output: Option<(String, Py<PyAny>)>,
}

impl CharsetMatch {
    /// Wrap a core match (and, recursively, its submatches).
    pub(crate) fn wrap(
        py: Python<'_>,
        mut inner: charset_norm::CharsetMatch,
        payload: &Bound<'_, PyAny>,
    ) -> PyResult<Py<CharsetMatch>> {
        let leaves = inner
            .take_submatches()
            .into_iter()
            .map(|leaf| Self::wrap(py, leaf, payload))
            .collect::<PyResult<Vec<_>>>()?;
        Py::new(
            py,
            Self {
                inner,
                payload: payload.clone().unbind(),
                leaves,
                output: None,
            },
        )
    }

    /// Make the decoded text available, falling back on Python's codecs for
    /// encodings without a native implementation.
    fn ensure_decoded(&mut self, py: Python<'_>) -> PyResult<()> {
        match self.inner.decoded() {
            Ok(_) => Ok(()),
            Err(DecodeError::Unknown) => {
                let text: String = self
                    .payload
                    .bind(py)
                    .call_method1("decode", (self.inner.encoding(), "strict"))?
                    .extract()?;
                self.inner.set_decoded(Some(text));
                Ok(())
            }
            Err(error) => Err(decode_error(self.inner.encoding(), error)),
        }
    }

    fn decoded(&mut self, py: Python<'_>) -> PyResult<&str> {
        self.ensure_decoded(py)?;
        Ok(self.inner.cached_decoded().unwrap_or_default())
    }

    fn fingerprint_value(&mut self, py: Python<'_>) -> PyResult<u64> {
        self.ensure_decoded(py)?;
        self.inner
            .fingerprint()
            .map_err(|error| decode_error(self.inner.encoding(), error))
    }

    fn could_be_from(&self, py: Python<'_>) -> Vec<String> {
        let mut values = vec![self.inner.encoding().to_owned()];
        for leaf in &self.leaves {
            values.push(leaf.bind(py).borrow().inner.encoding().to_owned());
        }
        values
    }

    /// Whether `other` has the same encoding and decoded text.
    fn same_as(&mut self, py: Python<'_>, other: &Bound<'_, CharsetMatch>) -> PyResult<bool> {
        let (other_encoding, other_fingerprint) = {
            let mut other = other.borrow_mut();
            let fingerprint = other.fingerprint_value(py)?;
            (other.inner.encoding().to_owned(), fingerprint)
        };
        Ok(self.inner.encoding() == other_encoding
            && self.fingerprint_value(py)? == other_fingerprint)
    }
}

/// Decode (through Python if needed) matches whose codec is not native, so
/// the core ranking can see their text.
fn prepare(py: Python<'_>, item: &Bound<'_, CharsetMatch>) -> PyResult<()> {
    let needs_python = {
        let value = item.borrow();
        value.inner.cached_decoded().is_none() && !codecs::is_known(value.inner.encoding())
    };
    if needs_python {
        item.borrow_mut().ensure_decoded(py)?;
    }
    Ok(())
}

/// `a < b` as `CharsetMatch.__lt__` defines it.
fn ranks_before(py: Python<'_>, a: &Py<CharsetMatch>, b: &Py<CharsetMatch>) -> PyResult<bool> {
    prepare(py, a.bind(py))?;
    prepare(py, b.bind(py))?;
    let a = a.bind(py).borrow();
    let b = b.bind(py).borrow();
    Ok(a.inner.ranks_before(&b.inner))
}

#[pymethods]
impl CharsetMatch {
    #[new]
    #[pyo3(signature = (payload, guessed_encoding, mean_mess_ratio, has_sig_or_bom, languages, decoded_payload=None, preemptive_declaration=None))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        payload: &Bound<'_, PyAny>,
        guessed_encoding: String,
        mean_mess_ratio: f64,
        has_sig_or_bom: bool,
        languages: Vec<(String, f64)>,
        decoded_payload: Option<String>,
        preemptive_declaration: Option<String>,
    ) -> PyResult<Self> {
        Ok(Self {
            inner: charset_norm::CharsetMatch::new(
                payload_bytes(payload)?,
                guessed_encoding,
                mean_mess_ratio,
                has_sig_or_bom,
                languages,
                decoded_payload,
                preemptive_declaration,
            ),
            payload: payload.clone().unbind(),
            leaves: Vec::new(),
            output: None,
        })
    }

    fn __str__(&mut self, py: Python<'_>) -> PyResult<String> {
        self.decoded(py).map(str::to_owned)
    }

    fn __repr__(&mut self, py: Python<'_>) -> PyResult<String> {
        let fingerprint = self.fingerprint_value(py)?;
        Ok(format!(
            "<CharsetMatch '{}' fp({})>",
            self.inner.encoding(),
            fingerprint
        ))
    }

    fn __richcmp__(
        slf: &Bound<'_, Self>,
        other: &Bound<'_, PyAny>,
        op: CompareOp,
    ) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let result = match op {
            CompareOp::Eq | CompareOp::Ne => {
                let equal = if let Ok(value) = other.extract::<String>() {
                    encoding::iana_name(&value, false).map_err(to_py_error)?
                        == slf.borrow().inner.encoding()
                } else if let Ok(other) = other.cast::<CharsetMatch>() {
                    other.is(slf) || slf.borrow_mut().same_as(py, other)?
                } else {
                    false
                };
                equal == matches!(op, CompareOp::Eq)
            }
            CompareOp::Lt => {
                let Ok(other) = other.cast::<CharsetMatch>() else {
                    return Err(PyValueError::new_err(""));
                };
                ranks_before(py, &slf.clone().unbind(), &other.clone().unbind())?
            }
            _ => return Ok(py.NotImplemented()),
        };
        Ok(result.into_pyobject(py)?.to_owned().into_any().unbind())
    }

    fn add_submatch(slf: &Bound<'_, Self>, other: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = slf.py();
        let submatch = match other.cast::<CharsetMatch>() {
            Ok(value) if !value.is(slf) && !slf.borrow_mut().same_as(py, value)? => value,
            _ => {
                let class = other.get_type();
                return Err(PyValueError::new_err(format!(
                    "Unable to add instance <{class}> as a submatch of a CharsetMatch"
                )));
            }
        };
        submatch.borrow_mut().inner.set_decoded(None);
        slf.borrow_mut().leaves.push(submatch.clone().unbind());
        Ok(())
    }

    #[getter]
    fn encoding(&self) -> &str {
        self.inner.encoding()
    }

    #[getter]
    fn encoding_aliases(&self) -> Vec<&'static str> {
        self.inner.encoding_aliases()
    }

    #[getter]
    fn bom(&self) -> bool {
        self.inner.has_sig_or_bom()
    }

    #[getter]
    fn byte_order_mark(&self) -> bool {
        self.inner.has_sig_or_bom()
    }

    #[getter]
    fn languages(&self) -> Vec<&str> {
        self.inner.languages()
    }

    #[getter]
    fn language(&self, py: Python<'_>) -> String {
        if self.inner.language_ratios().is_empty()
            && self.could_be_from(py).iter().any(|value| value == "ascii")
        {
            return "English".to_owned();
        }
        self.inner.language().to_owned()
    }

    #[getter]
    fn chaos(&self) -> f64 {
        self.inner.chaos()
    }

    #[getter]
    fn coherence(&self) -> f64 {
        self.inner.coherence()
    }

    #[getter]
    fn percent_chaos(&self) -> f64 {
        self.inner.percent_chaos()
    }

    #[getter]
    fn percent_coherence(&self) -> f64 {
        self.inner.percent_coherence()
    }

    #[getter]
    fn multi_byte_usage(&mut self, py: Python<'_>) -> PyResult<f64> {
        self.ensure_decoded(py)?;
        self.inner
            .multi_byte_usage()
            .map_err(|error| decode_error(self.inner.encoding(), error))
    }

    #[getter]
    fn raw(&self, py: Python<'_>) -> Py<PyAny> {
        self.payload.clone_ref(py)
    }

    #[getter]
    fn submatch(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        Ok(
            PyList::new(py, self.leaves.iter().map(|item| item.clone_ref(py)))?
                .into_any()
                .unbind(),
        )
    }

    #[getter]
    fn has_submatch(&self) -> bool {
        !self.leaves.is_empty()
    }

    #[getter]
    fn alphabets(&mut self, py: Python<'_>) -> PyResult<Vec<&'static str>> {
        self.ensure_decoded(py)?;
        self.inner
            .alphabets()
            .map(<[&str]>::to_vec)
            .map_err(|error| decode_error(self.inner.encoding(), error))
    }

    #[getter]
    fn could_be_from_charset(&self, py: Python<'_>) -> Vec<String> {
        self.could_be_from(py)
    }

    #[pyo3(signature = (encoding="utf_8"))]
    fn output(&mut self, py: Python<'_>, encoding: &str) -> PyResult<Py<PyAny>> {
        if let Some((cached, payload)) = &self.output
            && cached == encoding
        {
            return Ok(payload.clone_ref(py));
        }
        self.ensure_decoded(py)?;
        let text = self
            .inner
            .output_text(encoding)
            .map_err(|error| match error {
                OutputError::Decode(error) => decode_error(self.inner.encoding(), error),
                OutputError::Encoding(error) => to_py_error(error),
                other => PyValueError::new_err(other.to_string()),
            })?;
        let encoded = match codecs::encode(&text, encoding) {
            Some(bytes) => PyBytes::new(py, &bytes).into_any(),
            None => PyString::new(py, &text).call_method1("encode", (encoding, "replace"))?,
        };
        self.output = Some((encoding.to_owned(), encoded.clone().unbind()));
        Ok(encoded.unbind())
    }

    #[getter]
    fn fingerprint(&mut self, py: Python<'_>) -> PyResult<u64> {
        self.fingerprint_value(py)
    }

    #[getter]
    fn _string(&self) -> Option<String> {
        self.inner.cached_decoded().map(str::to_owned)
    }

    #[setter]
    fn set_string(&mut self, value: Option<String>) {
        self.inner.set_decoded(value);
    }
}

#[pyclass(module = "charset_norm.models", subclass)]
pub(crate) struct CharsetMatches {
    results: Vec<Py<CharsetMatch>>,
    sorted: bool,
}

impl CharsetMatches {
    /// Wrap core results, which are already in rank order.
    pub(crate) fn wrap(
        py: Python<'_>,
        results: charset_norm::CharsetMatches,
        payload: &Bound<'_, PyAny>,
    ) -> PyResult<Self> {
        let results = results
            .into_iter()
            .map(|item| CharsetMatch::wrap(py, item, payload))
            .collect::<PyResult<Vec<_>>>()?;
        Ok(Self {
            results,
            sorted: true,
        })
    }

    fn ensure_sorted(&mut self, py: Python<'_>) -> PyResult<()> {
        if self.sorted {
            return Ok(());
        }
        if self.results.len() >= 64 {
            // Keep CPython's exact (timsort) order for long lists.
            let list = PyList::new(py, self.results.iter().map(|item| item.clone_ref(py)))?;
            list.call_method0("sort")?;
            self.results = list.extract()?;
        } else {
            for item in &self.results {
                prepare(py, item.bind(py))?;
            }
            sort_by_rank(&mut self.results, |a, b| {
                a.bind(py)
                    .borrow()
                    .inner
                    .ranks_before(&b.bind(py).borrow().inner)
            });
        }
        self.sorted = true;
        Ok(())
    }

    fn push(&mut self, py: Python<'_>, item: Py<CharsetMatch>) -> PyResult<()> {
        prepare(py, item.bind(py))?;
        for existing in &self.results {
            let duplicate = {
                let candidate = item.bind(py).borrow();
                let existing = existing.bind(py).borrow();
                candidate
                    .inner
                    .is_duplicate_of(&existing.inner)
                    .map_err(|error| decode_error(candidate.inner.encoding(), error))?
            };
            if duplicate {
                item.bind(py).borrow_mut().inner.set_decoded(None);
                existing.bind(py).borrow_mut().leaves.push(item);
                return Ok(());
            }
        }
        self.results.push(item);
        self.sorted = false;
        Ok(())
    }
}

#[pymethods]
impl CharsetMatches {
    #[new]
    #[pyo3(signature = (results=None))]
    fn new(py: Python<'_>, results: Option<Vec<Py<CharsetMatch>>>) -> PyResult<Self> {
        let mut value = Self {
            results: results.unwrap_or_default(),
            sorted: false,
        };
        value.ensure_sorted(py)?;
        Ok(value)
    }

    fn __len__(&self) -> usize {
        self.results.len()
    }

    fn __bool__(&self) -> bool {
        !self.results.is_empty()
    }

    fn __iter__(&mut self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.ensure_sorted(py)?;
        Ok(
            PyList::new(py, self.results.iter().map(|item| item.clone_ref(py)))?
                .call_method0("__iter__")?
                .unbind(),
        )
    }

    fn __getitem__(
        &mut self,
        py: Python<'_>,
        item: &Bound<'_, PyAny>,
    ) -> PyResult<Py<CharsetMatch>> {
        if let Ok(index) = item.extract::<isize>() {
            self.ensure_sorted(py)?;
            let length = self.results.len();
            let position = if index < 0 {
                length.checked_sub(index.unsigned_abs())
            } else {
                usize::try_from(index).ok()
            };
            return match position.filter(|position| *position < length) {
                Some(position) => Ok(self.results[position].clone_ref(py)),
                None => Err(PyIndexError::new_err("list index out of range")),
            };
        }
        if let Ok(name) = item.extract::<String>() {
            let name = encoding::iana_name(&name, false).map_err(to_py_error)?;
            for result in &self.results {
                if result.bind(py).borrow().could_be_from(py).contains(&name) {
                    return Ok(result.clone_ref(py));
                }
            }
        }
        Err(PyKeyError::new_err(()))
    }

    fn append(&mut self, py: Python<'_>, item: &Bound<'_, PyAny>) -> PyResult<()> {
        let Ok(item) = item.cast::<CharsetMatch>() else {
            return Err(PyValueError::new_err(format!(
                "Cannot append instance '{}' to CharsetMatches",
                item.get_type()
            )));
        };
        self.push(py, item.clone().unbind())
    }

    fn best(&mut self, py: Python<'_>) -> PyResult<Option<Py<CharsetMatch>>> {
        self.ensure_sorted(py)?;
        Ok(self.results.first().map(|item| item.clone_ref(py)))
    }

    fn first(&mut self, py: Python<'_>) -> PyResult<Option<Py<CharsetMatch>>> {
        self.best(py)
    }
}

/// `json.dumps(..., ensure_ascii=True)` string escaping.
fn json_string(value: &str, out: &mut String) {
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(character),
            _ => {
                let mut units = [0u16; 2];
                for unit in character.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

/// `repr(float)` as `json.dumps` writes it.
fn json_float(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .to_owned();
    }
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let sign = if value < 0.0 { "-" } else { "" };
    if !(-4..16).contains(&exponent) {
        let mut mantissa = digits[..1].to_owned();
        if digits.len() > 1 {
            mantissa.push('.');
            mantissa.push_str(&digits[1..]);
        }
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        return format!("{sign}{mantissa}e{exponent_sign}{:02}", exponent.abs());
    }
    // Position of the decimal point relative to the digits (-3..=16 here).
    let point = exponent + 1;
    let body = if point <= 0 {
        let zeros = usize::try_from(-point).unwrap_or_default();
        format!("0.{}{digits}", "0".repeat(zeros))
    } else {
        let point = usize::try_from(point).unwrap_or_default();
        if point >= digits.len() {
            format!("{digits}{}.0", "0".repeat(point - digits.len()))
        } else {
            format!("{}.{}", &digits[..point], &digits[point..])
        }
    };
    format!("{sign}{body}")
}

#[pyclass(module = "charset_norm.models", get_all, set_all, subclass)]
pub(crate) struct CliDetectionResult {
    path: String,
    unicode_path: Option<String>,
    encoding: Option<String>,
    encoding_aliases: Vec<String>,
    alternative_encodings: Vec<String>,
    language: String,
    alphabets: Vec<String>,
    has_sig_or_bom: bool,
    chaos: f64,
    coherence: f64,
    is_preferred: bool,
}

#[pymethods]
impl CliDetectionResult {
    #[new]
    #[allow(clippy::too_many_arguments)]
    fn new(
        path: String,
        encoding: Option<String>,
        encoding_aliases: Vec<String>,
        alternative_encodings: Vec<String>,
        language: String,
        alphabets: Vec<String>,
        has_sig_or_bom: bool,
        chaos: f64,
        coherence: f64,
        unicode_path: Option<String>,
        is_preferred: bool,
    ) -> Self {
        Self {
            path,
            unicode_path,
            encoding,
            encoding_aliases,
            alternative_encodings,
            language,
            alphabets,
            has_sig_or_bom,
            chaos,
            coherence,
            is_preferred,
        }
    }
    #[getter]
    fn __dict__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let result = PyDict::new(py);
        result.set_item("path", &self.path)?;
        result.set_item("encoding", &self.encoding)?;
        result.set_item("encoding_aliases", &self.encoding_aliases)?;
        result.set_item("alternative_encodings", &self.alternative_encodings)?;
        result.set_item("language", &self.language)?;
        result.set_item("alphabets", &self.alphabets)?;
        result.set_item("has_sig_or_bom", self.has_sig_or_bom)?;
        result.set_item("chaos", self.chaos)?;
        result.set_item("coherence", self.coherence)?;
        result.set_item("unicode_path", &self.unicode_path)?;
        result.set_item("is_preferred", self.is_preferred)?;
        Ok(result.into_any().unbind())
    }

    /// Same output as `json.dumps(self.__dict__, ensure_ascii=True, indent=4)`.
    fn to_json(&self) -> String {
        fn string_or_null(value: Option<&String>, out: &mut String) {
            match value {
                Some(value) => json_string(value, out),
                None => out.push_str("null"),
            }
        }
        fn list(values: &[String], out: &mut String) {
            if values.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (index, value) in values.iter().enumerate() {
                out.push_str(if index == 0 {
                    "\n        "
                } else {
                    ",\n        "
                });
                json_string(value, out);
            }
            out.push_str("\n    ]");
        }

        let mut out = String::from("{");
        let key = |name: &str, out: &mut String, first: bool| {
            out.push_str(if first { "\n    " } else { ",\n    " });
            json_string(name, out);
            out.push_str(": ");
        };
        key("path", &mut out, true);
        json_string(&self.path, &mut out);
        key("encoding", &mut out, false);
        string_or_null(self.encoding.as_ref(), &mut out);
        key("encoding_aliases", &mut out, false);
        list(&self.encoding_aliases, &mut out);
        key("alternative_encodings", &mut out, false);
        list(&self.alternative_encodings, &mut out);
        key("language", &mut out, false);
        json_string(&self.language, &mut out);
        key("alphabets", &mut out, false);
        list(&self.alphabets, &mut out);
        key("has_sig_or_bom", &mut out, false);
        out.push_str(if self.has_sig_or_bom { "true" } else { "false" });
        key("chaos", &mut out, false);
        out.push_str(&json_float(self.chaos));
        key("coherence", &mut out, false);
        out.push_str(&json_float(self.coherence));
        key("unicode_path", &mut out, false);
        string_or_null(self.unicode_path.as_ref(), &mut out);
        key("is_preferred", &mut out, false);
        out.push_str(if self.is_preferred { "true" } else { "false" });
        out.push_str("\n}");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_repr_matches_python() {
        for (value, expected) in [
            (0.0, "0.0"),
            (10.0, "10.0"),
            (0.1, "0.1"),
            (1e-05, "1e-05"),
            (0.0001, "0.0001"),
            (1e16, "1e+16"),
            (123_456_789_012_345.6, "123456789012345.6"),
            (-2.5, "-2.5"),
            (99.123, "99.123"),
        ] {
            assert_eq!(json_float(value), expected);
        }
    }

    #[test]
    fn json_escaping() {
        let mut out = String::new();
        json_string("é\"\n😀\u{7f}", &mut out);
        assert_eq!(out, "\"\\u00e9\\\"\\n\\ud83d\\ude00\\u007f\"");
    }
}

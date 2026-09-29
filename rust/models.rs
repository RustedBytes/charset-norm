use std::hash::{DefaultHasher, Hasher};

use pyo3::basic::CompareOp;
use pyo3::exceptions::{PyIndexError, PyKeyError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyByteArray, PyBytes, PyDict, PyList, PyString};

use crate::codecs::{self, DecodeError, Errors};
use crate::tables::{self, ENCODING_ALIASES, TOO_BIG_SEQUENCE};
use crate::{decode_error, encoding_indication, iana_name_impl, py_round, unicode};

/// Run `f` over the payload bytes without copying `bytes` objects.
fn with_payload<R>(payload: &Bound<'_, PyAny>, f: impl FnOnce(&[u8]) -> R) -> PyResult<R> {
    if let Ok(value) = payload.cast::<PyBytes>() {
        return Ok(f(value.as_bytes()));
    }
    if let Ok(value) = payload.cast::<PyByteArray>() {
        return Ok(f(&value.to_vec()));
    }
    let owned: Vec<u8> = payload.extract()?;
    Ok(f(&owned))
}

/// `str(payload, encoding, "strict")`, natively when the codec is known.
fn decode_payload(payload: &Bound<'_, PyAny>, encoding: &str) -> PyResult<String> {
    match with_payload(payload, |bytes| {
        codecs::decode(bytes, encoding, Errors::Strict)
    })? {
        Ok(value) => Ok(value),
        Err(DecodeError::Unknown) => payload
            .call_method1("decode", (encoding, "strict"))?
            .extract(),
        Err(error) => Err(decode_error(encoding, error)),
    }
}

#[pyclass(module = "charset_normalizer.models", subclass)]
pub(crate) struct CharsetMatch {
    payload: Py<PyAny>,
    encoding: String,
    mean_mess_ratio: f64,
    languages: Vec<(String, f64)>,
    has_sig_or_bom: bool,
    unicode_ranges: Option<Vec<&'static str>>,
    leaves: Vec<Py<CharsetMatch>>,
    output_payload: Option<Py<PyAny>>,
    output_encoding: Option<String>,
    string: Option<String>,
    preemptive_declaration: Option<String>,
    fingerprint: Option<isize>,
    char_count: Option<usize>,
    raw_len: usize,
}

impl CharsetMatch {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create(
        py: Python<'_>,
        payload: Py<PyAny>,
        encoding: String,
        mean_mess_ratio: f64,
        has_sig_or_bom: bool,
        languages: Vec<(String, f64)>,
        decoded_payload: Option<String>,
        preemptive_declaration: Option<String>,
    ) -> PyResult<Self> {
        let raw_len = payload.bind(py).len()?;
        Ok(Self {
            payload,
            encoding,
            mean_mess_ratio,
            languages,
            has_sig_or_bom,
            unicode_ranges: None,
            leaves: Vec::new(),
            output_payload: None,
            output_encoding: None,
            string: decoded_payload,
            preemptive_declaration,
            fingerprint: None,
            char_count: None,
            raw_len,
        })
    }

    pub(crate) fn encoding_name(&self) -> &str {
        &self.encoding
    }

    fn decoded(&mut self, py: Python<'_>) -> PyResult<&str> {
        if self.string.is_none() {
            let mut decoded = decode_payload(self.payload.bind(py), &self.encoding)?;
            if self.has_sig_or_bom && self.encoding == "utf_7" && decoded.starts_with('\u{feff}') {
                decoded.remove(0);
            }
            self.string = Some(decoded);
        }
        Ok(self.string.as_deref().unwrap_or_default())
    }

    /// Stable hash of the decoded text (replaces the process-salted `hash(str)`).
    fn fingerprint_value(&mut self, py: Python<'_>) -> PyResult<isize> {
        if let Some(value) = self.fingerprint {
            return Ok(value);
        }
        let mut hasher = DefaultHasher::new();
        hasher.write(self.decoded(py)?.as_bytes());
        let value = hasher.finish() as isize;
        self.fingerprint = Some(value);
        Ok(value)
    }

    fn multi_byte_usage_value(&mut self, py: Python<'_>) -> PyResult<f64> {
        if self.raw_len == 0 {
            return Ok(0.0);
        }
        let count = match self.char_count {
            Some(count) => count,
            None => {
                let count = self.decoded(py)?.chars().count();
                self.char_count = Some(count);
                count
            }
        };
        Ok(1.0 - count as f64 / self.raw_len as f64)
    }

    fn coherence_value(&self) -> f64 {
        self.languages.first().map_or(0.0, |value| value.1)
    }

    fn same_as(&mut self, py: Python<'_>, other: &Bound<'_, CharsetMatch>) -> PyResult<bool> {
        let (other_encoding, other_fingerprint) = {
            let mut other = other.borrow_mut();
            let fingerprint = other.fingerprint_value(py)?;
            (other.encoding.clone(), fingerprint)
        };
        Ok(self.encoding == other_encoding && self.fingerprint_value(py)? == other_fingerprint)
    }
}

/// `a < b` for two matches, as `CharsetMatch.__lt__` defines it.
pub(crate) fn match_lt(
    py: Python<'_>,
    a: &Py<CharsetMatch>,
    b: &Py<CharsetMatch>,
) -> PyResult<bool> {
    let (a_chaos, a_coherence, a_len) = {
        let a = a.bind(py).borrow();
        (a.mean_mess_ratio, a.coherence_value(), a.raw_len)
    };
    let (b_chaos, b_coherence) = {
        let b = b.bind(py).borrow();
        (b.mean_mess_ratio, b.coherence_value())
    };
    let chaos_difference = (a_chaos - b_chaos).abs();
    let coherence_difference = (a_coherence - b_coherence).abs();
    if chaos_difference < 0.005 && coherence_difference > 0.02 {
        return Ok(a_coherence > b_coherence);
    }
    if chaos_difference < 0.005 && coherence_difference <= 0.02 {
        if a_len >= TOO_BIG_SEQUENCE {
            return Ok(a_chaos < b_chaos);
        }
        let a_usage = a.bind(py).borrow_mut().multi_byte_usage_value(py)?;
        let b_usage = b.bind(py).borrow_mut().multi_byte_usage_value(py)?;
        return Ok(a_usage > b_usage);
    }
    Ok(a_chaos < b_chaos)
}

#[pymethods]
impl CharsetMatch {
    #[new]
    #[pyo3(signature = (payload, guessed_encoding, mean_mess_ratio, has_sig_or_bom, languages, decoded_payload=None, preemptive_declaration=None))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        py: Python<'_>,
        payload: Py<PyAny>,
        guessed_encoding: String,
        mean_mess_ratio: f64,
        has_sig_or_bom: bool,
        languages: Vec<(String, f64)>,
        decoded_payload: Option<String>,
        preemptive_declaration: Option<String>,
    ) -> PyResult<Self> {
        Self::create(
            py,
            payload,
            guessed_encoding,
            mean_mess_ratio,
            has_sig_or_bom,
            languages,
            decoded_payload,
            preemptive_declaration,
        )
    }

    fn __str__(&mut self, py: Python<'_>) -> PyResult<String> {
        self.decoded(py).map(str::to_owned)
    }

    fn __repr__(&mut self, py: Python<'_>) -> PyResult<String> {
        let fingerprint = self.fingerprint_value(py)?;
        Ok(format!(
            "<CharsetMatch '{}' fp({})>",
            self.encoding, fingerprint
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
                    iana_name_impl(&value, false)? == slf.borrow().encoding
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
                match_lt(py, &slf.clone().unbind(), &other.clone().unbind())?
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
        submatch.borrow_mut().string = None;
        slf.borrow_mut().leaves.push(submatch.clone().unbind());
        Ok(())
    }

    #[getter]
    fn encoding(&self) -> &str {
        &self.encoding
    }

    #[getter]
    fn encoding_aliases(&self) -> Vec<&'static str> {
        let mut known = Vec::new();
        for &(alias, canonical) in ENCODING_ALIASES {
            if self.encoding == alias {
                known.push(canonical);
            } else if self.encoding == canonical {
                known.push(alias);
            }
        }
        known
    }

    #[getter]
    fn bom(&self) -> bool {
        self.has_sig_or_bom
    }
    #[getter]
    fn byte_order_mark(&self) -> bool {
        self.has_sig_or_bom
    }
    #[getter]
    fn languages(&self) -> Vec<String> {
        self.languages.iter().map(|item| item.0.clone()).collect()
    }

    #[getter]
    fn language(&self, py: Python<'_>) -> PyResult<String> {
        if let Some((language, _)) = self.languages.first() {
            return Ok(language.clone());
        }
        if self
            .could_be_from_charset(py)
            .iter()
            .any(|value| value == "ascii")
        {
            return Ok("English".to_owned());
        }
        let languages = if tables::is_multi_byte_encoding(&self.encoding) {
            crate::mb_languages(&self.encoding)
        } else {
            crate::single_byte_languages(&self.encoding)
        };
        if languages.is_empty() || languages.contains(&"Latin Based") {
            Ok("Unknown".to_owned())
        } else {
            Ok(languages[0].to_owned())
        }
    }

    #[getter]
    fn chaos(&self) -> f64 {
        self.mean_mess_ratio
    }
    #[getter]
    fn coherence(&self) -> f64 {
        self.coherence_value()
    }
    #[getter]
    fn percent_chaos(&self) -> f64 {
        py_round(self.mean_mess_ratio * 100.0, 3)
    }
    #[getter]
    fn percent_coherence(&self) -> f64 {
        py_round(self.coherence_value() * 100.0, 3)
    }
    #[getter]
    fn multi_byte_usage(&mut self, py: Python<'_>) -> PyResult<f64> {
        self.multi_byte_usage_value(py)
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
        if let Some(ranges) = &self.unicode_ranges {
            return Ok(ranges.clone());
        }
        let mut seen = vec![false; unicode::ranges().len()];
        for character in self.decoded(py)?.chars() {
            let index = unicode::props(character).range;
            if index != unicode::NO_RANGE {
                seen[index as usize] = true;
            }
        }
        let mut ranges: Vec<&'static str> = unicode::ranges()
            .iter()
            .zip(seen)
            .filter_map(|(range, seen)| seen.then_some(range.name))
            .collect();
        ranges.sort_unstable();
        self.unicode_ranges = Some(ranges.clone());
        Ok(ranges)
    }

    #[getter]
    fn could_be_from_charset(&self, py: Python<'_>) -> Vec<String> {
        let mut values = vec![self.encoding.clone()];
        for leaf in &self.leaves {
            values.push(leaf.bind(py).borrow().encoding.clone());
        }
        values
    }

    #[pyo3(signature = (encoding="utf_8"))]
    fn output(&mut self, py: Python<'_>, encoding: &str) -> PyResult<Py<PyAny>> {
        if self.output_encoding.as_deref() != Some(encoding) || self.output_payload.is_none() {
            let mut decoded = self.decoded(py)?.to_owned();
            if self.preemptive_declaration.as_ref().is_some_and(|value| {
                !matches!(value.to_lowercase().as_str(), "utf-8" | "utf8" | "utf_8")
            }) {
                let prefix_end = decoded
                    .char_indices()
                    .nth(8192)
                    .map_or(decoded.len(), |(offset, _)| offset);
                let found = encoding_indication()
                    .captures(&decoded[..prefix_end])
                    .and_then(|captures| {
                        Some((captures.get(0)?.range(), captures.get(1)?.range()))
                    });
                if let Some((full, group)) = found {
                    let replacement = iana_name_impl(encoding, true)?.replace('_', "-");
                    let patched = decoded[full.clone()].replace(&decoded[group], &replacement);
                    decoded = format!(
                        "{}{}{}",
                        &decoded[..full.start],
                        patched,
                        &decoded[full.end..]
                    );
                }
            }
            let encoded = match codecs::encode(&decoded, encoding) {
                Some(bytes) => PyBytes::new(py, &bytes).into_any(),
                None => {
                    PyString::new(py, &decoded).call_method1("encode", (encoding, "replace"))?
                }
            };
            self.output_encoding = Some(encoding.to_owned());
            self.output_payload = Some(encoded.unbind());
        }
        Ok(self
            .output_payload
            .as_ref()
            .expect("output payload initialized")
            .clone_ref(py))
    }

    #[getter]
    fn fingerprint(&mut self, py: Python<'_>) -> PyResult<isize> {
        self.fingerprint_value(py)
    }

    #[getter]
    fn _string(&self) -> Option<String> {
        self.string.clone()
    }
    #[setter]
    fn set_string(&mut self, value: Option<String>) {
        self.string = value;
    }
}

#[pyclass(module = "charset_normalizer.models", subclass)]
pub(crate) struct CharsetMatches {
    results: Vec<Py<CharsetMatch>>,
    sorted: bool,
}

/// CPython's `list.sort` for short lists: a natural run, then binary
/// insertion. Longer lists defer to `list.sort` itself so that ordering with
/// the (non-transitive) match comparison stays identical.
fn sort_matches(py: Python<'_>, items: &mut Vec<Py<CharsetMatch>>) -> PyResult<()> {
    let n = items.len();
    if n < 2 {
        return Ok(());
    }
    if n >= 64 {
        let list = PyList::new(py, items.iter().map(|item| item.clone_ref(py)))?;
        list.call_method0("sort")?;
        *items = list.extract()?;
        return Ok(());
    }
    // count_run
    let mut run = 2;
    if match_lt(py, &items[1], &items[0])? {
        while run < n && match_lt(py, &items[run], &items[run - 1])? {
            run += 1;
        }
        items[..run].reverse();
    } else {
        while run < n && !match_lt(py, &items[run], &items[run - 1])? {
            run += 1;
        }
    }
    // binarysort
    for start in run..n {
        let mut left = 0;
        let mut right = start;
        while left < right {
            let middle = left + ((right - left) >> 1);
            if match_lt(py, &items[start], &items[middle])? {
                right = middle;
            } else {
                left = middle + 1;
            }
        }
        let pivot = items.remove(start);
        items.insert(left, pivot);
    }
    Ok(())
}

impl CharsetMatches {
    pub(crate) fn from_results(py: Python<'_>, results: Vec<Py<CharsetMatch>>) -> PyResult<Self> {
        let mut value = Self {
            results,
            sorted: false,
        };
        value.ensure_sorted(py)?;
        Ok(value)
    }

    fn ensure_sorted(&mut self, py: Python<'_>) -> PyResult<()> {
        if !self.sorted {
            sort_matches(py, &mut self.results)?;
            self.sorted = true;
        }
        Ok(())
    }

    pub(crate) fn len(&self) -> usize {
        self.results.len()
    }

    pub(crate) fn push(&mut self, py: Python<'_>, item: Py<CharsetMatch>) -> PyResult<()> {
        let (raw_len, chaos) = {
            let value = item.bind(py).borrow();
            (value.raw_len, value.mean_mess_ratio)
        };
        if raw_len < TOO_BIG_SEQUENCE {
            let mut fingerprint = None;
            for existing in &self.results {
                // Compare the cheap chaos first; texts are hashed only on a tie.
                if existing.bind(py).borrow().mean_mess_ratio != chaos {
                    continue;
                }
                let fingerprint = match fingerprint {
                    Some(value) => value,
                    None => *fingerprint.insert(item.bind(py).borrow_mut().fingerprint_value(py)?),
                };
                let same = existing.bind(py).borrow_mut().fingerprint_value(py)? == fingerprint;
                if same {
                    item.bind(py).borrow_mut().string = None;
                    existing.bind(py).borrow_mut().leaves.push(item);
                    return Ok(());
                }
            }
        }
        self.results.push(item);
        self.sorted = false;
        Ok(())
    }

    pub(crate) fn best_native(&mut self, py: Python<'_>) -> PyResult<Option<Py<CharsetMatch>>> {
        self.ensure_sorted(py)?;
        Ok(self.results.first().map(|item| item.clone_ref(py)))
    }
}

#[pymethods]
impl CharsetMatches {
    #[new]
    #[pyo3(signature = (results=None))]
    fn new(py: Python<'_>, results: Option<Vec<Py<CharsetMatch>>>) -> PyResult<Self> {
        Self::from_results(py, results.unwrap_or_default())
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
            let index = if index < 0 {
                self.results.len() as isize + index
            } else {
                index
            };
            if index < 0 || index as usize >= self.results.len() {
                return Err(PyIndexError::new_err("list index out of range"));
            }
            return Ok(self.results[index as usize].clone_ref(py));
        }
        if let Ok(name) = item.extract::<String>() {
            let name = iana_name_impl(&name, false)?;
            for result in &self.results {
                if result
                    .bind(py)
                    .borrow()
                    .could_be_from_charset(py)
                    .contains(&name)
                {
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
        self.best_native(py)
    }
    fn first(&mut self, py: Python<'_>) -> PyResult<Option<Py<CharsetMatch>>> {
        self.best_native(py)
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
                    out.push_str(&format!("\\u{unit:04x}"));
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
    let point = exponent + 1;
    let body = if point <= 0 {
        format!("0.{}{}", "0".repeat((-point) as usize), digits)
    } else if point as usize >= digits.len() {
        format!("{}{}.0", digits, "0".repeat(point as usize - digits.len()))
    } else {
        format!(
            "{}.{}",
            &digits[..point as usize],
            &digits[point as usize..]
        )
    };
    format!("{sign}{body}")
}

#[pyclass(module = "charset_normalizer.models", get_all, set_all, subclass)]
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
        fn string_or_null(value: &Option<String>, out: &mut String) {
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
        string_or_null(&self.encoding, &mut out);
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
        string_or_null(&self.unicode_path, &mut out);
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
            (123456789012345.6, "123456789012345.6"),
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

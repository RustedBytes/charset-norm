use pyo3::basic::CompareOp;
use pyo3::exceptions::{PyIndexError, PyKeyError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyString};

use super::{iana_name, unicode_range};

fn byte_offset(value: &str, character_offset: usize) -> usize {
    value
        .char_indices()
        .nth(character_offset)
        .map_or(value.len(), |(offset, _)| offset)
}

#[pyclass(module = "charset_normalizer.models", subclass)]
pub(crate) struct CharsetMatch {
    payload: Py<PyAny>,
    encoding: String,
    mean_mess_ratio: f64,
    languages: Vec<(String, f64)>,
    has_sig_or_bom: bool,
    unicode_ranges: Option<Vec<String>>,
    leaves: Vec<Py<CharsetMatch>>,
    output_payload: Option<Py<PyAny>>,
    output_encoding: Option<String>,
    string: Option<String>,
    preemptive_declaration: Option<String>,
}

impl CharsetMatch {
    fn decoded(&mut self, py: Python<'_>) -> PyResult<String> {
        if self.string.is_none() {
            let mut decoded: String = py
                .import("builtins")?
                .getattr("str")?
                .call1((self.payload.bind(py), &self.encoding, "strict"))?
                .extract()?;
            if self.has_sig_or_bom && self.encoding == "utf_7" && decoded.starts_with('\u{feff}') {
                decoded.remove(0);
            }
            self.string = Some(decoded);
        }
        Ok(self.string.clone().unwrap_or_default())
    }

    fn fingerprint_value(&mut self, py: Python<'_>) -> PyResult<isize> {
        py.import("builtins")?
            .getattr("hash")?
            .call1((self.decoded(py)?,))?
            .extract()
    }

    fn raw_len(&self, py: Python<'_>) -> PyResult<usize> {
        self.payload.bind(py).len()
    }

    fn multi_byte_usage_value(&mut self, py: Python<'_>) -> PyResult<f64> {
        let raw_len = self.raw_len(py)?;
        if raw_len == 0 {
            return Ok(0.0);
        }
        Ok(1.0 - self.decoded(py)?.chars().count() as f64 / raw_len as f64)
    }
}

#[pymethods]
impl CharsetMatch {
    #[new]
    #[pyo3(signature = (payload, guessed_encoding, mean_mess_ratio, has_sig_or_bom, languages, decoded_payload=None, preemptive_declaration=None))]
    fn new(
        payload: Py<PyAny>,
        guessed_encoding: String,
        mean_mess_ratio: f64,
        has_sig_or_bom: bool,
        languages: Vec<(String, f64)>,
        decoded_payload: Option<String>,
        preemptive_declaration: Option<String>,
    ) -> Self {
        Self {
            payload,
            encoding: guessed_encoding,
            mean_mess_ratio,
            languages,
            has_sig_or_bom,
            unicode_ranges: None,
            leaves: Vec::new(),
            output_payload: None,
            output_encoding: None,
            string: decoded_payload,
            preemptive_declaration,
        }
    }

    fn __str__(&mut self, py: Python<'_>) -> PyResult<String> {
        self.decoded(py)
    }

    fn __repr__(&mut self, py: Python<'_>) -> PyResult<String> {
        let encoding = self.encoding.clone();
        let fingerprint = self.fingerprint_value(py)?;
        Ok(format!("<CharsetMatch '{}' fp({})>", encoding, fingerprint))
    }

    fn __richcmp__(
        mut slf: PyRefMut<'_, Self>,
        py: Python<'_>,
        other: &Bound<'_, PyAny>,
        op: CompareOp,
    ) -> PyResult<Py<PyAny>> {
        let result = match op {
            CompareOp::Eq | CompareOp::Ne => {
                let equal = if let Ok(value) = other.extract::<String>() {
                    iana_name(py, &value, false)? == slf.encoding
                } else if other.is_instance_of::<CharsetMatch>() {
                    if other.as_ptr() == slf.as_ptr() {
                        true
                    } else {
                        let other_encoding: String = other.getattr("encoding")?.extract()?;
                        let other_fingerprint: isize = other.getattr("fingerprint")?.extract()?;
                        slf.encoding == other_encoding
                            && slf.fingerprint_value(py)? == other_fingerprint
                    }
                } else {
                    false
                };
                if matches!(op, CompareOp::Eq) {
                    equal
                } else {
                    !equal
                }
            }
            CompareOp::Lt => {
                if !other.is_instance_of::<CharsetMatch>() {
                    return Err(PyValueError::new_err(""));
                }
                let other_chaos: f64 = other.getattr("chaos")?.extract()?;
                let other_coherence: f64 = other.getattr("coherence")?.extract()?;
                let chaos_difference = (slf.mean_mess_ratio - other_chaos).abs();
                let self_coherence = slf.languages.first().map_or(0.0, |value| value.1);
                let coherence_difference = (self_coherence - other_coherence).abs();
                if chaos_difference < 0.005 && coherence_difference > 0.02 {
                    self_coherence > other_coherence
                } else if chaos_difference < 0.005 && coherence_difference <= 0.02 {
                    let too_big: usize = py
                        .import("charset_normalizer.constant")?
                        .getattr("TOO_BIG_SEQUENCE")?
                        .extract()?;
                    if slf.raw_len(py)? >= too_big {
                        slf.mean_mess_ratio < other_chaos
                    } else {
                        let other_usage: f64 = other.getattr("multi_byte_usage")?.extract()?;
                        slf.multi_byte_usage_value(py)? > other_usage
                    }
                } else {
                    slf.mean_mess_ratio < other_chaos
                }
            }
            _ => return Ok(py.NotImplemented()),
        };
        Ok(result.into_pyobject(py)?.to_owned().into_any().unbind())
    }

    fn add_submatch(
        mut slf: PyRefMut<'_, Self>,
        py: Python<'_>,
        other: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let invalid = if !other.is_instance_of::<CharsetMatch>() || other.as_ptr() == slf.as_ptr() {
            true
        } else {
            let other_encoding: String = other.getattr("encoding")?.extract()?;
            let other_fingerprint: isize = other.getattr("fingerprint")?.extract()?;
            slf.encoding == other_encoding && slf.fingerprint_value(py)? == other_fingerprint
        };
        if invalid {
            let class = other.get_type();
            return Err(PyValueError::new_err(format!(
                "Unable to add instance <{class}> as a submatch of a CharsetMatch"
            )));
        }
        other.setattr("_string", py.None())?;
        slf.leaves.push(other.extract()?);
        Ok(())
    }

    #[getter]
    fn encoding(&self) -> &str {
        &self.encoding
    }

    #[getter]
    fn encoding_aliases(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        let aliases = py
            .import("encodings.aliases")?
            .getattr("aliases")?
            .cast_into::<PyDict>()?;
        let mut known = Vec::new();
        for (alias, canonical) in aliases.iter() {
            let alias: String = alias.extract()?;
            let canonical: String = canonical.extract()?;
            if self.encoding == alias {
                known.push(canonical);
            } else if self.encoding == canonical {
                known.push(alias);
            }
        }
        Ok(known)
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
        let charsets = self.could_be_from_charset(py)?;
        if charsets.iter().any(|encoding| encoding == "ascii") {
            return Ok("English".to_owned());
        }
        let utils = py.import("charset_normalizer.utils")?;
        let is_mb: bool = utils
            .getattr("is_multi_byte_encoding")?
            .call1((&self.encoding,))?
            .extract()?;
        let cd = py.import("charset_normalizer.cd")?;
        let languages: Vec<String> = if is_mb {
            cd.getattr("mb_encoding_languages")?
                .call1((&self.encoding,))?
                .extract()?
        } else {
            cd.getattr("encoding_languages")?
                .call1((&self.encoding,))?
                .extract()?
        };
        if languages.is_empty() || languages.iter().any(|value| value == "Latin Based") {
            Ok("Unknown".to_owned())
        } else {
            Ok(languages[0].clone())
        }
    }

    #[getter]
    fn chaos(&self) -> f64 {
        self.mean_mess_ratio
    }
    #[getter]
    fn coherence(&self) -> f64 {
        self.languages.first().map_or(0.0, |item| item.1)
    }
    #[getter]
    fn percent_chaos(&self, py: Python<'_>) -> PyResult<f64> {
        py.import("builtins")?
            .getattr("round")?
            .call1((self.mean_mess_ratio * 100.0, 3))?
            .extract()
    }
    #[getter]
    fn percent_coherence(&self, py: Python<'_>) -> PyResult<f64> {
        py.import("builtins")?
            .getattr("round")?
            .call1((self.coherence() * 100.0, 3))?
            .extract()
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
    fn alphabets(&mut self, py: Python<'_>) -> PyResult<Vec<String>> {
        if let Some(ranges) = &self.unicode_ranges {
            return Ok(ranges.clone());
        }
        let mut ranges = Vec::<String>::new();
        for character in self.decoded(py)?.chars() {
            if let Some(range) = unicode_range(py, &character.to_string())? {
                if !ranges.contains(&range) {
                    ranges.push(range);
                }
            }
        }
        ranges.sort();
        self.unicode_ranges = Some(ranges.clone());
        Ok(ranges)
    }

    #[getter]
    fn could_be_from_charset(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        let mut values = vec![self.encoding.clone()];
        for leaf in &self.leaves {
            values.push(leaf.bind(py).borrow().encoding.clone());
        }
        Ok(values)
    }

    #[pyo3(signature = (encoding="utf_8"))]
    fn output(&mut self, py: Python<'_>, encoding: &str) -> PyResult<Py<PyAny>> {
        if self.output_encoding.as_deref() != Some(encoding) {
            self.output_encoding = Some(encoding.to_owned());
            let mut decoded = self.decoded(py)?;
            if self.preemptive_declaration.as_ref().is_some_and(|value| {
                !matches!(value.to_lowercase().as_str(), "utf-8" | "utf8" | "utf_8")
            }) {
                let prefix: String = decoded.chars().take(8192).collect();
                let regex = py
                    .import("charset_normalizer.constant")?
                    .getattr("RE_POSSIBLE_ENCODING_INDICATION")?;
                if let Some(found) = regex
                    .call_method1("search", (&prefix,))?
                    .extract::<Option<Py<PyAny>>>()?
                {
                    let found = found.bind(py);
                    let start: usize = found.call_method0("start")?.extract()?;
                    let end: usize = found.call_method0("end")?.extract()?;
                    let group: String = found.call_method1("group", (1,))?.extract()?;
                    let start = byte_offset(&prefix, start);
                    let end = byte_offset(&prefix, end);
                    let full = &prefix[start..end];
                    let replacement = iana_name(py, encoding, true)?.replace('_', "-");
                    let patched = full.replace(&group, &replacement);
                    decoded = format!("{}{}{}", &prefix[..start], patched, &decoded[end..]);
                }
            }
            let encoded =
                PyString::new(py, &decoded).call_method1("encode", (encoding, "replace"))?;
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

impl CharsetMatches {
    fn ensure_sorted(&mut self, py: Python<'_>) -> PyResult<()> {
        if !self.sorted {
            let list = PyList::new(py, self.results.iter().map(|item| item.clone_ref(py)))?;
            list.call_method0("sort")?;
            self.results = list.extract()?;
            self.sorted = true;
        }
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
            let name = iana_name(py, &name, false)?;
            for result in &self.results {
                if result
                    .bind(py)
                    .borrow()
                    .could_be_from_charset(py)?
                    .contains(&name)
                {
                    return Ok(result.clone_ref(py));
                }
            }
        }
        Err(PyKeyError::new_err(()))
    }
    fn append(&mut self, py: Python<'_>, item: &Bound<'_, PyAny>) -> PyResult<()> {
        if !item.is_instance_of::<CharsetMatch>() {
            return Err(PyValueError::new_err(format!(
                "Cannot append instance '{}' to CharsetMatches",
                item.get_type()
            )));
        }
        let item_py: Py<CharsetMatch> = item.extract()?;
        let too_big: usize = py
            .import("charset_normalizer.constant")?
            .getattr("TOO_BIG_SEQUENCE")?
            .extract()?;
        let (raw_len, chaos) = {
            let value = item_py.bind(py).borrow();
            (value.raw_len(py)?, value.mean_mess_ratio)
        };
        if raw_len < too_big {
            let fingerprint = item_py.bind(py).borrow_mut().fingerprint_value(py)?;
            for existing in &self.results {
                let same = {
                    let mut value = existing.bind(py).borrow_mut();
                    value.fingerprint_value(py)? == fingerprint && value.mean_mess_ratio == chaos
                };
                if same {
                    item_py.bind(py).borrow_mut().string = None;
                    existing.bind(py).borrow_mut().leaves.push(item_py);
                    return Ok(());
                }
            }
        }
        self.results.push(item_py);
        self.sorted = false;
        Ok(())
    }
    fn best(&mut self, py: Python<'_>) -> PyResult<Option<Py<CharsetMatch>>> {
        self.ensure_sorted(py)?;
        Ok(self.results.first().map(|item| item.clone_ref(py)))
    }
    fn first(&mut self, py: Python<'_>) -> PyResult<Option<Py<CharsetMatch>>> {
        self.best(py)
    }
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
    fn to_json(&self, py: Python<'_>) -> PyResult<String> {
        let kwargs = PyDict::new(py);
        kwargs.set_item("ensure_ascii", true)?;
        kwargs.set_item("indent", 4)?;
        py.import("json")?
            .getattr("dumps")?
            .call((self.__dict__(py)?,), Some(&kwargs))?
            .extract()
    }
}

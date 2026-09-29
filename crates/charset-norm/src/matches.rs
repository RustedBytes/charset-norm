//! Detection results.

use std::hash::{DefaultHasher, Hasher};
use std::sync::{Arc, OnceLock};

use crate::codecs::{self, DecodeError, Errors};
use crate::encoding::{self, encoding_indication};
use crate::{pyfloat, unicode, Error, TOO_BIG_SEQUENCE};

/// One plausible encoding for a payload.
#[derive(Clone, Debug)]
pub struct CharsetMatch {
    payload: Arc<[u8]>,
    encoding: String,
    chaos: f64,
    has_sig_or_bom: bool,
    languages: Vec<(String, f64)>,
    decoded: OnceLock<Arc<str>>,
    preemptive_declaration: Option<String>,
    submatches: Vec<CharsetMatch>,
    fingerprint: OnceLock<u64>,
    char_count: OnceLock<usize>,
    alphabets: OnceLock<Vec<&'static str>>,
}

impl CharsetMatch {
    /// Build a match. `languages` holds `(language, coherence)` pairs, best
    /// first; `decoded` may carry the already decoded text.
    pub fn new(
        payload: impl Into<Arc<[u8]>>,
        encoding: impl Into<String>,
        chaos: f64,
        has_sig_or_bom: bool,
        languages: Vec<(String, f64)>,
        decoded: Option<String>,
        preemptive_declaration: Option<String>,
    ) -> Self {
        let decoded_cell = OnceLock::new();
        if let Some(text) = decoded {
            let _ = decoded_cell.set(Arc::from(text));
        }
        Self {
            payload: payload.into(),
            encoding: encoding.into(),
            chaos,
            has_sig_or_bom,
            languages,
            decoded: decoded_cell,
            preemptive_declaration,
            submatches: Vec::new(),
            fingerprint: OnceLock::new(),
            char_count: OnceLock::new(),
            alphabets: OnceLock::new(),
        }
    }

    /// Canonical (CPython) name of the encoding, e.g. `"cp1252"`.
    pub fn encoding(&self) -> &str {
        &self.encoding
    }

    /// Other names of [`encoding`](Self::encoding).
    pub fn encoding_aliases(&self) -> Vec<&'static str> {
        encoding::aliases(&self.encoding)
    }

    /// Mess ratio of the decoded text; lower is better.
    pub fn chaos(&self) -> f64 {
        self.chaos
    }

    /// [`chaos`](Self::chaos) as a percentage, rounded to 3 decimals.
    pub fn percent_chaos(&self) -> f64 {
        pyfloat::round(self.chaos * 100.0, 3)
    }

    /// Coherence of the best detected language, in `[0, 1]`.
    pub fn coherence(&self) -> f64 {
        self.languages.first().map_or(0.0, |value| value.1)
    }

    /// [`coherence`](Self::coherence) as a percentage, rounded to 3 decimals.
    pub fn percent_coherence(&self) -> f64 {
        pyfloat::round(self.coherence() * 100.0, 3)
    }

    /// Whether the payload starts with this encoding's signature or BOM.
    pub fn has_sig_or_bom(&self) -> bool {
        self.has_sig_or_bom
    }

    /// Detected languages with their coherence, best first.
    pub fn language_ratios(&self) -> &[(String, f64)] {
        &self.languages
    }

    /// Detected languages, best first.
    pub fn languages(&self) -> Vec<&str> {
        self.languages.iter().map(|item| item.0.as_str()).collect()
    }

    /// Most probable language, falling back on what the encoding suggests.
    pub fn language(&self) -> &str {
        if let Some((language, _)) = self.languages.first() {
            return language;
        }
        if self.could_be_from_charset().contains(&"ascii") {
            return "English";
        }
        let languages = encoding::target_languages(&self.encoding);
        match languages.first() {
            Some(first) if !languages.contains(&"Latin Based") => first,
            _ => "Unknown",
        }
    }

    /// The analysed payload.
    pub fn raw(&self) -> &[u8] {
        &self.payload
    }

    /// The analysed payload, shared.
    pub fn payload(&self) -> &Arc<[u8]> {
        &self.payload
    }

    /// Encoding declared inside the payload, if any was found.
    pub fn preemptive_declaration(&self) -> Option<&str> {
        self.preemptive_declaration.as_deref()
    }

    /// The payload decoded with [`encoding`](Self::encoding).
    pub fn decoded(&self) -> Result<&str, DecodeError> {
        if let Some(text) = self.decoded.get() {
            return Ok(text);
        }
        let mut text = codecs::decode(&self.payload, &self.encoding, Errors::Strict)?;
        if self.has_sig_or_bom && self.encoding == "utf_7" && text.starts_with('\u{feff}') {
            text.remove(0);
        }
        Ok(self.decoded.get_or_init(|| Arc::from(text)))
    }

    /// The decoded text if it is already available, without decoding.
    pub fn cached_decoded(&self) -> Option<&str> {
        self.decoded.get().map(|text| &**text)
    }

    /// Replace (or drop) the cached decoded text.
    pub fn set_decoded(&mut self, text: Option<String>) {
        self.decoded = OnceLock::new();
        if let Some(text) = text {
            let _ = self.decoded.set(Arc::from(text));
        }
    }

    /// Stable 64-bit hash of the decoded text.
    pub fn fingerprint(&self) -> Result<u64, DecodeError> {
        if let Some(value) = self.fingerprint.get() {
            return Ok(*value);
        }
        let mut hasher = DefaultHasher::new();
        hasher.write(self.decoded()?.as_bytes());
        Ok(*self.fingerprint.get_or_init(|| hasher.finish()))
    }

    /// Share of the payload's bytes spent on multi-byte sequences.
    pub fn multi_byte_usage(&self) -> Result<f64, DecodeError> {
        if self.payload.is_empty() {
            return Ok(0.0);
        }
        let count = match self.char_count.get() {
            Some(count) => *count,
            None => {
                let text = self.decoded()?;
                *self.char_count.get_or_init(|| text.chars().count())
            }
        };
        Ok(1.0 - count as f64 / self.payload.len() as f64)
    }

    /// Unicode ranges present in the decoded text, sorted by name.
    pub fn alphabets(&self) -> Result<&[&'static str], DecodeError> {
        if let Some(ranges) = self.alphabets.get() {
            return Ok(ranges);
        }
        let mut seen = vec![false; unicode::ranges().len()];
        for character in self.decoded()?.chars() {
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
        Ok(self.alphabets.get_or_init(|| ranges))
    }

    /// Other matches that decode to the very same text.
    pub fn submatches(&self) -> &[CharsetMatch] {
        &self.submatches
    }

    /// Detach and return the submatches.
    pub fn take_submatches(&mut self) -> Vec<CharsetMatch> {
        std::mem::take(&mut self.submatches)
    }

    /// Whether other encodings decode to the same text.
    pub fn has_submatch(&self) -> bool {
        !self.submatches.is_empty()
    }

    /// This match's encoding followed by those of its submatches.
    pub fn could_be_from_charset(&self) -> Vec<&str> {
        std::iter::once(self.encoding.as_str())
            .chain(self.submatches.iter().map(|item| item.encoding.as_str()))
            .collect()
    }

    /// Whether `other` has the same encoding and decoded text.
    pub fn same_as(&self, other: &CharsetMatch) -> Result<bool, DecodeError> {
        Ok(self.encoding == other.encoding && self.fingerprint()? == other.fingerprint()?)
    }

    /// Attach `other` as a submatch. Returns `false` (and drops it) when it
    /// is the same match as `self`.
    pub fn add_submatch(&mut self, mut other: CharsetMatch) -> Result<bool, DecodeError> {
        if self.same_as(&other)? {
            return Ok(false);
        }
        other.set_decoded(None);
        self.submatches.push(other);
        Ok(true)
    }

    /// Whether this match, if added to a result list holding `existing`,
    /// would be folded into it as a submatch (same text, same chaos).
    pub fn is_duplicate_of(&self, existing: &CharsetMatch) -> Result<bool, DecodeError> {
        if self.payload.len() >= TOO_BIG_SEQUENCE || existing.chaos != self.chaos {
            return Ok(false);
        }
        Ok(existing.fingerprint()? == self.fingerprint()?)
    }

    /// Whether this match ranks before `other` (lower chaos, then higher
    /// coherence, then heavier multi-byte usage).
    pub fn ranks_before(&self, other: &CharsetMatch) -> bool {
        let chaos_difference = (self.chaos - other.chaos).abs();
        let coherence_difference = (self.coherence() - other.coherence()).abs();
        if chaos_difference < 0.005 && coherence_difference > 0.02 {
            return self.coherence() > other.coherence();
        }
        if chaos_difference < 0.005 && coherence_difference <= 0.02 {
            if self.payload.len() >= TOO_BIG_SEQUENCE {
                return self.chaos < other.chaos;
            }
            return self.multi_byte_usage().unwrap_or(0.0)
                > other.multi_byte_usage().unwrap_or(0.0);
        }
        self.chaos < other.chaos
    }

    /// Decoded text with any in-document encoding declaration rewritten to
    /// `encoding`, ready to be re-encoded.
    pub fn output_text(&self, encoding: &str) -> Result<String, OutputError> {
        let mut decoded = self.decoded().map_err(OutputError::Decode)?.to_owned();
        let declared_non_utf8 = self.preemptive_declaration.as_ref().is_some_and(|value| {
            !matches!(value.to_lowercase().as_str(), "utf-8" | "utf8" | "utf_8")
        });
        if declared_non_utf8 {
            let prefix_end = decoded
                .char_indices()
                .nth(8192)
                .map_or(decoded.len(), |(offset, _)| offset);
            let found = encoding_indication()
                .captures(&decoded[..prefix_end])
                .and_then(|captures| Some((captures.get(0)?.range(), captures.get(1)?.range())));
            if let Some((full, group)) = found {
                let replacement = encoding::iana_name(encoding, true)
                    .map_err(OutputError::Encoding)?
                    .replace('_', "-");
                let patched = decoded[full.clone()].replace(&decoded[group], &replacement);
                decoded = format!(
                    "{}{}{}",
                    &decoded[..full.start],
                    patched,
                    &decoded[full.end..]
                );
            }
        }
        Ok(decoded)
    }

    /// The payload transcoded to `encoding` (unencodable characters become
    /// `?`), with in-document declarations updated.
    ///
    /// ```
    /// let results = charset_norm::from_bytes("Ça va très bien".as_bytes());
    /// let best = results.best().unwrap();
    /// assert_eq!(best.output("utf_8").unwrap(), "Ça va très bien".as_bytes());
    /// ```
    pub fn output(&self, encoding: &str) -> Result<Vec<u8>, OutputError> {
        let text = self.output_text(encoding)?;
        codecs::encode(&text, encoding).ok_or_else(|| OutputError::Unsupported(encoding.to_owned()))
    }
}

/// Why [`CharsetMatch::output`] failed.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum OutputError {
    /// The payload could not be decoded.
    Decode(DecodeError),
    /// The target encoding name is unknown.
    Encoding(Error),
    /// No native encoder exists for the target encoding.
    Unsupported(String),
}

impl std::fmt::Display for OutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputError::Decode(error) => error.fmt(f),
            OutputError::Encoding(error) => error.fmt(f),
            OutputError::Unsupported(name) => write!(f, "no native encoder for '{name}'"),
        }
    }
}

impl std::error::Error for OutputError {}

/// Sort `items` by a "ranks before" predicate, reproducing CPython's
/// `list.sort()` order for fewer than 64 items (the order is observable
/// because the predicate is not transitive). Larger inputs use a stable
/// binary insertion sort.
pub fn sort_by_rank<T>(items: &mut [T], mut lt: impl FnMut(&T, &T) -> bool) {
    let n = items.len();
    if n < 2 {
        return;
    }
    // CPython's count_run, only for short lists (a single run + binarysort).
    let mut run = 1;
    if n < 64 {
        run = 2;
        if lt(&items[1], &items[0]) {
            while run < n && lt(&items[run], &items[run - 1]) {
                run += 1;
            }
            items[..run].reverse();
        } else {
            while run < n && !lt(&items[run], &items[run - 1]) {
                run += 1;
            }
        }
    }
    for start in run..n {
        let mut left = 0;
        let mut right = start;
        while left < right {
            let middle = left + ((right - left) >> 1);
            if lt(&items[start], &items[middle]) {
                right = middle;
            } else {
                left = middle + 1;
            }
        }
        items[left..=start].rotate_right(1);
    }
}

/// Plausible encodings for a payload, best first.
#[derive(Clone, Debug, Default)]
pub struct CharsetMatches {
    results: Vec<CharsetMatch>,
}

impl CharsetMatches {
    /// Rank `results`, best first.
    pub fn new(mut results: Vec<CharsetMatch>) -> Self {
        sort_by_rank(&mut results, CharsetMatch::ranks_before);
        Self { results }
    }

    /// Wrap results that are already in rank order.
    pub(crate) fn from_sorted(results: Vec<CharsetMatch>) -> Self {
        Self { results }
    }

    /// Add a match. One decoding to the same text with the same chaos as an
    /// existing match becomes its submatch instead.
    pub fn push(&mut self, item: CharsetMatch) {
        for existing in &mut self.results {
            if item.is_duplicate_of(existing).unwrap_or(false) {
                let mut item = item;
                item.set_decoded(None);
                existing.submatches.push(item);
                return;
            }
        }
        self.results.push(item);
        sort_by_rank(&mut self.results, CharsetMatch::ranks_before);
    }

    /// The most probable match.
    pub fn best(&self) -> Option<&CharsetMatch> {
        self.results.first()
    }

    /// Number of distinct matches (submatches excluded).
    pub fn len(&self) -> usize {
        self.results.len()
    }

    /// Whether no encoding fits the payload (it is likely binary).
    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }

    /// Match at `index` in rank order.
    pub fn get(&self, index: usize) -> Option<&CharsetMatch> {
        self.results.get(index)
    }

    /// The match that could come from `encoding` (any alias), if any.
    pub fn get_by_encoding(&self, encoding: &str) -> Option<&CharsetMatch> {
        let name = encoding::iana_name(encoding, false).ok()?;
        self.results
            .iter()
            .find(|item| item.could_be_from_charset().contains(&name.as_str()))
    }

    /// Matches in rank order.
    pub fn iter(&self) -> std::slice::Iter<'_, CharsetMatch> {
        self.results.iter()
    }

    /// Matches in rank order, as a vector.
    pub fn into_vec(self) -> Vec<CharsetMatch> {
        self.results
    }
}

impl IntoIterator for CharsetMatches {
    type Item = CharsetMatch;
    type IntoIter = std::vec::IntoIter<CharsetMatch>;

    fn into_iter(self) -> Self::IntoIter {
        self.results.into_iter()
    }
}

impl<'a> IntoIterator for &'a CharsetMatches {
    type Item = &'a CharsetMatch;
    type IntoIter = std::slice::Iter<'a, CharsetMatch>;

    fn into_iter(self) -> Self::IntoIter {
        self.results.iter()
    }
}

/// Result list with the lazy sorting of the reference implementation: items
/// are ranked only when the best one is requested.
pub(crate) struct PendingMatches {
    results: Vec<CharsetMatch>,
    sorted: bool,
}

impl PendingMatches {
    pub(crate) fn new() -> Self {
        Self {
            results: Vec::new(),
            sorted: true,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.results.len()
    }

    pub(crate) fn push(&mut self, item: CharsetMatch) {
        for existing in &mut self.results {
            if item.is_duplicate_of(existing).unwrap_or(false) {
                let mut item = item;
                item.set_decoded(None);
                existing.submatches.push(item);
                return;
            }
        }
        self.results.push(item);
        self.sorted = false;
    }

    fn sort(&mut self) {
        if !self.sorted {
            sort_by_rank(&mut self.results, CharsetMatch::ranks_before);
            self.sorted = true;
        }
    }

    pub(crate) fn best(&mut self) -> Option<&CharsetMatch> {
        self.sort();
        self.results.first()
    }

    pub(crate) fn take_best(mut self) -> Option<CharsetMatch> {
        self.sort();
        self.results.into_iter().next()
    }

    pub(crate) fn into_matches(mut self) -> CharsetMatches {
        self.sort();
        CharsetMatches::from_sorted(self.results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(encoding: &str, chaos: f64, text: &str) -> CharsetMatch {
        CharsetMatch::new(
            text.as_bytes().to_vec(),
            encoding,
            chaos,
            false,
            Vec::new(),
            None,
            None,
        )
    }

    #[test]
    fn ranking_and_merging() {
        let mut matches = CharsetMatches::default();
        matches.push(sample("utf_8", 0.3, "hello"));
        matches.push(sample("ascii", 0.1, "hello"));
        matches.push(sample("latin_1", 0.1, "hello"));
        assert_eq!(matches.len(), 2);
        let best = matches.best().unwrap();
        assert_eq!(best.encoding(), "ascii");
        assert_eq!(best.could_be_from_charset(), ["ascii", "latin_1"]);
        assert_eq!(best.language(), "English");
        assert!(matches.get_by_encoding("latin-1").is_some());
    }

    #[test]
    fn short_sort_matches_cpython() {
        let mut values = vec![3, 1, 2, 5, 4];
        sort_by_rank(&mut values, |a, b| a < b);
        assert_eq!(values, [1, 2, 3, 4, 5]);
        let mut long: Vec<u32> = (0..100).rev().collect();
        sort_by_rank(&mut long, |a, b| a < b);
        assert!(long.windows(2).all(|pair| pair[0] <= pair[1]));
    }
}

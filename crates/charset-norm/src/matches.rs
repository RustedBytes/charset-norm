//! Detection results: [`CharsetMatches`] and [`CharsetMatch`], re-exported at
//! the crate root.

use std::borrow::Cow;
use std::hash::{DefaultHasher, Hasher};
use std::sync::{Arc, OnceLock};

use crate::codecs::{self, DecodeError, Errors};
use crate::encoding::{self, encoding_indication};
use crate::{Error, TOO_BIG_SEQUENCE, pyfloat, unicode};

/// One plausible encoding for a payload.
///
/// A match holds the analysed payload, the encoding, and the two scores the
/// verdict rests on:
///
/// - [`chaos`](Self::chaos): how noisy the decoded text looks, from `0.0`
///   (clean) upward. Lower is better.
/// - [`coherence`](Self::coherence): how well the text matches the letter
///   frequencies of its best language, in `[0, 1]`. Higher is better.
///
/// The decoded text is computed lazily and cached; cloning a match never
/// copies the payload or the text.
///
/// ```
/// use charset_norm::codecs;
///
/// let text = "Größere Änderungen an der Straßenführung sind für nächste Woche geplant.";
/// let payload = codecs::encode(text, "cp1252").unwrap();
///
/// let results = charset_norm::from_bytes(&payload);
/// let best = results.best().unwrap();
///
/// assert_eq!(best.decoded().unwrap(), text);
/// assert!(best.chaos() < 0.1);
/// assert!(best.coherence() > 0.0);
/// assert!(!best.has_sig_or_bom());
///
/// // Code pages that decode these bytes to the very same text are
/// // reported alongside the best match rather than as separate results.
/// assert!(best.could_be_from_charset().contains(&"cp1252"));
/// ```
#[derive(Clone, Debug)]
pub struct CharsetMatch {
    payload: Arc<[u8]>,
    encoding: Cow<'static, str>,
    chaos: f64,
    has_sig_or_bom: bool,
    languages: Arc<[(Cow<'static, str>, f64)]>,
    decoded: OnceLock<Arc<String>>,
    preemptive_declaration: Option<Cow<'static, str>>,
    submatches: Vec<CharsetMatch>,
    fingerprint: OnceLock<u64>,
    char_count: OnceLock<usize>,
    alphabets: OnceLock<Vec<&'static str>>,
}

impl CharsetMatch {
    /// Build a match. `languages` holds `(language, coherence)` pairs, best
    /// first; `decoded` may carry the already decoded text.
    ///
    /// Names are borrowed when static (as produced by detection) and owned
    /// otherwise; cloning a match never copies its payload or text.
    pub fn new(
        payload: impl Into<Arc<[u8]>>,
        encoding: impl Into<Cow<'static, str>>,
        chaos: f64,
        has_sig_or_bom: bool,
        languages: impl Into<Arc<[(Cow<'static, str>, f64)]>>,
        decoded: Option<String>,
        preemptive_declaration: Option<Cow<'static, str>>,
    ) -> Self {
        let decoded_cell = OnceLock::new();
        if let Some(text) = decoded {
            let _ = decoded_cell.set(Arc::new(text));
        }
        Self {
            payload: payload.into(),
            encoding: encoding.into(),
            chaos,
            has_sig_or_bom,
            languages: languages.into(),
            decoded: decoded_cell,
            preemptive_declaration,
            submatches: Vec::new(),
            fingerprint: OnceLock::new(),
            char_count: OnceLock::new(),
            alphabets: OnceLock::new(),
        }
    }

    /// Canonical (`CPython`) name of the encoding, e.g. `"cp1252"`.
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
    pub fn language_ratios(&self) -> &[(Cow<'static, str>, f64)] {
        &self.languages
    }

    /// Detected languages, best first.
    pub fn languages(&self) -> Vec<&str> {
        self.languages.iter().map(|item| &*item.0).collect()
    }

    /// Most probable language, falling back on what the encoding suggests.
    pub fn language(&self) -> &str {
        if let Some((language, _)) = self.languages.first() {
            return language;
        }
        if self.could_be_from("ascii") {
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
    ///
    /// # Errors
    ///
    /// [`DecodeError::Invalid`] when the payload is not valid for the encoding,
    /// [`DecodeError::Unknown`] when the encoding has no native codec.
    pub fn decoded(&self) -> Result<&str, DecodeError> {
        if let Some(text) = self.decoded.get() {
            return Ok(text);
        }
        let mut text = codecs::decode(&self.payload, &self.encoding, Errors::Strict)?;
        if self.has_sig_or_bom && self.encoding == "utf_7" && text.starts_with('\u{feff}') {
            text.remove(0);
        }
        Ok(self.decoded.get_or_init(|| Arc::new(text)))
    }

    /// The decoded text if it is already available, without decoding.
    pub fn cached_decoded(&self) -> Option<&str> {
        self.decoded.get().map(|text| text.as_str())
    }

    /// Replace (or drop) the cached decoded text.
    pub fn set_decoded(&mut self, text: Option<String>) {
        self.decoded = OnceLock::new();
        if let Some(text) = text {
            let _ = self.decoded.set(Arc::new(text));
        }
    }

    /// Stable 64-bit hash of the decoded text.
    ///
    /// # Errors
    ///
    /// A [`DecodeError`] when the payload cannot be decoded.
    pub fn fingerprint(&self) -> Result<u64, DecodeError> {
        if let Some(value) = self.fingerprint.get() {
            return Ok(*value);
        }
        let mut hasher = DefaultHasher::new();
        hasher.write(self.decoded()?.as_bytes());
        Ok(*self.fingerprint.get_or_init(|| hasher.finish()))
    }

    /// Share of the payload's bytes spent on multi-byte sequences.
    ///
    /// # Errors
    ///
    /// A [`DecodeError`] when the payload cannot be decoded.
    pub fn multi_byte_usage(&self) -> Result<f64, DecodeError> {
        if self.payload.is_empty() {
            return Ok(0.0);
        }
        let count = if let Some(count) = self.char_count.get() {
            *count
        } else {
            let text = self.decoded()?;
            *self.char_count.get_or_init(|| text.chars().count())
        };
        Ok(1.0 - count as f64 / self.payload.len() as f64)
    }

    /// Unicode ranges present in the decoded text, sorted by name.
    ///
    /// # Errors
    ///
    /// A [`DecodeError`] when the payload cannot be decoded.
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

    /// Other matches that decode to the very same text with the same chaos.
    ///
    /// Many code pages differ only in a few byte values, so a payload that
    /// avoids those bytes is equally valid in all of them. Such matches are
    /// grouped under the best-ranked one instead of being listed separately
    /// in [`CharsetMatches`]; [`could_be_from_charset`](Self::could_be_from_charset)
    /// lists every encoding of the group.
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

    /// Whether this match or one of its submatches uses `encoding`.
    fn could_be_from(&self, encoding: &str) -> bool {
        self.encoding == encoding || self.submatches.iter().any(|item| item.encoding == encoding)
    }

    /// This match's encoding followed by those of its submatches.
    pub fn could_be_from_charset(&self) -> Vec<&str> {
        std::iter::once(&*self.encoding)
            .chain(self.submatches.iter().map(|item| &*item.encoding))
            .collect()
    }

    /// Whether `other` has the same encoding and decoded text.
    ///
    /// # Errors
    ///
    /// A [`DecodeError`] when either payload cannot be decoded.
    pub fn same_as(&self, other: &CharsetMatch) -> Result<bool, DecodeError> {
        Ok(self.encoding == other.encoding && self.fingerprint()? == other.fingerprint()?)
    }

    /// Attach `other` as a submatch. Returns `false` (and drops it) when it
    /// is the same match as `self`.
    ///
    /// # Errors
    ///
    /// A [`DecodeError`] when either payload cannot be decoded.
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
    ///
    /// # Errors
    ///
    /// A [`DecodeError`] when either payload cannot be decoded.
    #[expect(
        clippy::float_cmp,
        reason = "only identical chaos values denote the same result"
    )]
    pub fn is_duplicate_of(&self, existing: &CharsetMatch) -> Result<bool, DecodeError> {
        if self.payload.len() >= TOO_BIG_SEQUENCE || existing.chaos != self.chaos {
            return Ok(false);
        }
        // Comparing the texts is exact, and cheaper than hashing both.
        Ok(existing.decoded()? == self.decoded()?)
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
    ///
    /// # Errors
    ///
    /// [`OutputError`] when the payload does not decode or, if a declaration
    /// must be rewritten, the target encoding is unknown.
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
    ///
    /// # Errors
    ///
    /// [`OutputError`] when the payload does not decode, the target encoding is
    /// unknown, or it has no native encoder.
    pub fn output(&self, encoding: &str) -> Result<Vec<u8>, OutputError> {
        let text = self.output_text(encoding)?;
        codecs::encode(&text, encoding).ok_or_else(|| OutputError::Unsupported(encoding.to_owned()))
    }
}

/// Why [`CharsetMatch::output`] or [`CharsetMatch::output_text`] failed.
///
/// ```
/// use charset_norm::OutputError;
///
/// let results = charset_norm::from_bytes("plain text".as_bytes());
/// let best = results.best().unwrap();
/// assert!(matches!(best.output("no-such-codec"), Err(OutputError::Unsupported(_))));
/// ```
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

/// Sort `items` by a "ranks before" predicate, reproducing `CPython`'s
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
///
/// Returned by [`from_bytes`](crate::from_bytes) and the other detection
/// functions. [`best`](Self::best) is the verdict; the rest are less likely
/// alternatives. Empty when no encoding fits, which usually means the payload
/// is binary.
///
/// ```
/// use charset_norm::codecs;
///
/// let payload = codecs::encode("Меня зовут Анна, я живу в Москве и работаю учителем.", "koi8_r").unwrap();
/// let results = charset_norm::from_bytes(&payload);
///
/// assert_eq!(results.best().unwrap().encoding(), "koi8_r");
///
/// for candidate in &results {
///     println!("{}: chaos {:.1}%", candidate.encoding(), candidate.percent_chaos());
/// }
///
/// // Look a match up by any alias of its encoding or of one of its submatches.
/// assert!(results.get_by_encoding("KOI8-R").is_some());
/// ```
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
    #[must_use]
    pub fn best(&self) -> Option<&CharsetMatch> {
        self.results.first()
    }

    /// Number of distinct matches (submatches excluded).
    #[must_use]
    pub fn len(&self) -> usize {
        self.results.len()
    }

    /// Whether no encoding fits the payload (it is likely binary).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }

    /// Match at `index` in rank order.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&CharsetMatch> {
        self.results.get(index)
    }

    /// The match that could come from `encoding` (any alias), if any.
    #[must_use]
    pub fn get_by_encoding(&self, encoding: &str) -> Option<&CharsetMatch> {
        let name = encoding::iana_name(encoding, false).ok()?;
        self.results.iter().find(|item| item.could_be_from(&name))
    }

    /// Matches in rank order.
    pub fn iter(&self) -> std::slice::Iter<'_, CharsetMatch> {
        self.results.iter()
    }

    /// Matches in rank order, as a vector.
    #[must_use]
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

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
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

    fn sample(encoding: &'static str, chaos: f64, text: &str) -> CharsetMatch {
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

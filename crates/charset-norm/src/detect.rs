//! The detector.

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;

use rustc_hash::FxHashMap;

use crate::chunks::{ChunkCutter, ChunkSource, Signature};
use crate::codecs::{self, DecodeError, Errors};
use crate::encoding::{self, any_specified_encoding, identify_sig_or_bom, should_strip_sig_or_bom};
use crate::log::{Level, Logger, NoLogger, emit};
use crate::matches::{CharsetMatch, CharsetMatches, PendingMatches};
use crate::{TOO_BIG_SEQUENCE, TOO_SMALL_SEQUENCE, coherence, mess, pyfloat};

/// Tuning knobs of [`from_bytes_with`]. The defaults match the reference
/// implementation.
#[derive(Clone, Debug, PartialEq)]
pub struct DetectionOptions {
    /// Number of chunks sampled from the payload.
    pub steps: usize,
    /// Size of each sampled chunk, in bytes.
    pub chunk_size: usize,
    /// Maximum acceptable mess ratio (chaos) for a candidate.
    pub threshold: f64,
    /// Only try these encodings (any alias) when not empty.
    pub cp_isolation: Vec<String>,
    /// Never try these encodings (any alias).
    pub cp_exclusion: Vec<String>,
    /// Favour an encoding declared inside the payload (e.g. HTML `charset`).
    pub preemptive_behaviour: bool,
    /// Trace the mess analysis when one or two encodings are isolated.
    pub explain: bool,
    /// Minimum coherence for a language to be reported.
    pub language_threshold: f64,
    /// Fall back on ASCII/UTF-8/declared encodings when nothing else fits.
    pub enable_fallback: bool,
}

impl Default for DetectionOptions {
    fn default() -> Self {
        Self {
            steps: 5,
            chunk_size: 512,
            threshold: 0.2,
            cp_isolation: Vec::new(),
            cp_exclusion: Vec::new(),
            preemptive_behaviour: true,
            explain: false,
            language_threshold: 0.1,
            enable_fallback: true,
        }
    }
}

fn decode(bytes: &[u8], encoding: &str) -> Result<String, DecodeError> {
    codecs::decode(bytes, encoding, Errors::Strict)
}

/// Detect the plausible encodings of `payload` with default options.
///
/// ```
/// use charset_norm::codecs;
///
/// let text = "Всеки човек има право на образование. Образованието трябва да бъде безплатно.";
/// let payload = codecs::encode(text, "cp1251").unwrap();
///
/// let results = charset_norm::from_bytes(&payload);
/// let best = results.best().unwrap();
/// assert_eq!(best.encoding(), "cp1251");
/// assert_eq!(best.decoded().unwrap(), text);
/// ```
#[must_use]
pub fn from_bytes(payload: &[u8]) -> CharsetMatches {
    from_bytes_with(payload, &DetectionOptions::default(), &NoLogger)
}

/// Read a file and detect its plausible encodings with default options.
///
/// # Errors
///
/// Any error reading the file.
pub fn from_path(path: impl AsRef<Path>) -> std::io::Result<CharsetMatches> {
    Ok(from_bytes(&std::fs::read(path)?))
}

/// Whether `payload` looks like binary data rather than text.
#[must_use]
pub fn is_binary(payload: &[u8]) -> bool {
    let options = DetectionOptions {
        enable_fallback: false,
        ..DetectionOptions::default()
    };
    from_bytes_with(payload, &options, &NoLogger).is_empty()
}

/// Detect the plausible encodings of `payload`, reporting progress to
/// `logger`.
#[must_use]
pub fn from_bytes_with(
    payload: &[u8],
    options: &DetectionOptions,
    logger: &dyn Logger,
) -> CharsetMatches {
    detect(&Arc::from(payload), options, logger)
}

/// [`from_bytes_with`] for a payload that is already shared; the matches
/// keep a reference to it instead of a copy.
#[must_use]
pub fn detect(
    payload: &Arc<[u8]>,
    options: &DetectionOptions,
    logger: &dyn Logger,
) -> CharsetMatches {
    if payload.is_empty() {
        emit(logger, Level::Debug, || {
            "Encoding detection on empty bytes, assuming utf_8 intention.".to_owned()
        });
        return CharsetMatches::from_sorted(vec![CharsetMatch::new(
            payload.clone(),
            "utf_8",
            0.0,
            false,
            Vec::new(),
            Some(String::new()),
            None,
        )]);
    }
    Detector::new(payload, options, logger).run()
}

/// Encodings that never carry a trustworthy signal of multi-byte usage.
fn is_unicode_family(encoding: &str) -> bool {
    matches!(
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
}

/// Coherence results per language inclusion list, then per chunk.
type CoherenceCache = FxHashMap<String, FxHashMap<String, Vec<(&'static str, f64)>>>;

/// Mess analysis of one candidate encoding.
struct Sample {
    chunks: Vec<String>,
    mean: f64,
    early_stop: usize,
    max_give_up: usize,
}

/// Facts about one candidate encoding for the current payload.
struct Candidate {
    encoding: &'static str,
    bom: bool,
    strip_bom: bool,
    multibyte: bool,
    languages: Vec<&'static str>,
}

/// State of one detection run (the reference algorithm's local variables).
struct Detector<'a> {
    payload: &'a Arc<[u8]>,
    options: &'a DetectionOptions,
    logger: &'a dyn Logger,
    steps: usize,
    chunk_size: usize,
    is_too_large: bool,
    specified: Option<&'static str>,
    sig_encoding: Option<&'static str>,
    sig: &'static [u8],
    isolation: Vec<String>,
    exclusion: Vec<String>,
    explain_mess: bool,
    results: PendingMatches,
    early_results: PendingMatches,
    tested: HashSet<&'static str>,
    soft_skip: HashSet<&'static str>,
    fallback_ascii: Option<CharsetMatch>,
    fallback_utf8: Option<CharsetMatch>,
    fallback_specified: Option<CharsetMatch>,
    /// Languages of the first coherent single-byte match, once found.
    definitive_languages: Option<HashSet<&'static str>>,
    post_definitive_success: usize,
    multibyte_definitive: bool,
    mess_cache: FxHashMap<String, f64>,
    coherence_cache: CoherenceCache,
}

impl<'a> Detector<'a> {
    fn new(payload: &'a Arc<[u8]>, options: &'a DetectionOptions, logger: &'a dyn Logger) -> Self {
        let length = payload.len();
        let mut steps = options.steps;
        let mut chunk_size = options.chunk_size;
        if length <= chunk_size.saturating_mul(steps) {
            steps = 1;
            chunk_size = length;
        }
        if steps > 1 && length / steps < chunk_size {
            chunk_size = length / steps;
        }
        if length < TOO_SMALL_SEQUENCE {
            emit(logger, Level::Trace, || {
                format!("Trying to detect encoding from a tiny portion of ({length}) byte(s).")
            });
        }
        let normalize = |values: &[String]| -> Vec<String> {
            values
                .iter()
                .map(|value| encoding::iana_name(value, false).unwrap_or_default())
                .collect()
        };
        let isolation = normalize(&options.cp_isolation);
        let exclusion = normalize(&options.cp_exclusion);
        let (sig_encoding, sig) = identify_sig_or_bom(payload);
        Self {
            payload,
            options,
            logger,
            steps,
            chunk_size,
            is_too_large: length >= TOO_BIG_SEQUENCE,
            specified: if options.preemptive_behaviour {
                any_specified_encoding(payload, 8192)
            } else {
                None
            },
            sig_encoding,
            sig,
            explain_mess: options.explain && (1..=2).contains(&isolation.len()),
            isolation,
            exclusion,
            results: PendingMatches::new(),
            early_results: PendingMatches::new(),
            tested: HashSet::new(),
            soft_skip: HashSet::new(),
            fallback_ascii: None,
            fallback_utf8: None,
            fallback_specified: None,
            definitive_languages: None,
            post_definitive_success: 0,
            multibyte_definitive: false,
            mess_cache: FxHashMap::default(),
            coherence_cache: FxHashMap::default(),
        }
    }

    fn run(mut self) -> CharsetMatches {
        for encoding in self.prioritized() {
            if let ControlFlow::Break(matches) = self.try_encoding(encoding) {
                return matches;
            }
        }
        self.finish()
    }

    /// Candidates in trial order: signature, declared encoding, ASCII, UTF-8,
    /// then every supported encoding (multi-byte first).
    fn prioritized(&self) -> Vec<&'static str> {
        let mut prioritized = Vec::new();
        if let Some(value) = self.specified {
            prioritized.push(value);
        }
        if let Some(value) = self.sig_encoding {
            prioritized.insert(0, value);
        }
        prioritized.push("ascii");
        if !prioritized.contains(&"utf_8") {
            prioritized.push("utf_8");
        }
        prioritized.extend_from_slice(encoding::supported_encodings());
        prioritized
    }

    fn new_match(
        &self,
        encoding: &str,
        chaos: f64,
        bom: bool,
        languages: Vec<(String, f64)>,
        decoded: Option<String>,
    ) -> CharsetMatch {
        CharsetMatch::new(
            self.payload.clone(),
            encoding,
            chaos,
            bom,
            languages,
            decoded,
            self.specified.map(str::to_owned),
        )
    }

    /// Whether `encoding` should be tried at all, recording it as tested.
    fn candidate(&mut self, encoding: &'static str) -> Option<Candidate> {
        if (!self.isolation.is_empty() && !self.isolation.iter().any(|value| value == encoding))
            || self.exclusion.iter().any(|value| value == encoding)
            || self.tested.contains(encoding)
        {
            return None;
        }
        self.tested.insert(encoding);
        let bom = self.sig_encoding == Some(encoding);
        if (matches!(encoding, "utf_16" | "utf_32" | "utf_7") && !bom)
            || self.soft_skip.contains(encoding)
            || !codecs::is_known(encoding)
        {
            return None;
        }
        let multibyte = encoding::is_multi_byte_encoding(encoding);
        let languages = encoding::target_languages(encoding);
        if let Some(definitive) = &self.definitive_languages
            && (!languages
                .iter()
                .any(|language| definitive.contains(language))
                || (!multibyte && self.post_definitive_success >= 7))
        {
            return None;
        }
        if self.multibyte_definitive && !multibyte {
            return None;
        }
        Some(Candidate {
            encoding,
            bom,
            strip_bom: bom && should_strip_sig_or_bom(encoding),
            multibyte,
            languages,
        })
    }

    /// Payload without a stripped signature.
    fn source(&self, candidate: &Candidate) -> &'a [u8] {
        let payload: &'a [u8] = self.payload;
        if candidate.strip_bom {
            &payload[self.sig.len()..]
        } else {
            payload
        }
    }

    /// Decode up front when the reference does (multi-byte codecs, or a
    /// prefix of large payloads). `Err` rejects the candidate.
    fn initial_decode(&self, candidate: &Candidate) -> Result<Option<String>, DecodeError> {
        let encoding = candidate.encoding;
        let source = self.source(candidate);
        if self.is_too_large && !candidate.multibyte {
            decode(&source[..source.len().min(500_000)], encoding)?;
            return Ok(None);
        }
        if candidate.multibyte {
            let bom_kept_utf7 = encoding == "utf_7" && candidate.bom;
            let payload: &[u8] = self.payload;
            let mut value = decode(if bom_kept_utf7 { payload } else { source }, encoding)?;
            if bom_kept_utf7 && value.starts_with('\u{feff}') {
                value.remove(0);
            }
            return Ok(Some(value));
        }
        Ok(None)
    }

    /// Measure the mess of sampled chunks; `None` when a chunk does not decode.
    fn sample(&mut self, candidate: &Candidate, decoded: Option<&str>) -> Option<Sample> {
        let length = self.payload.len();
        let deferred = !candidate.multibyte && !self.is_too_large;
        let offset_start = if candidate.bom { self.sig.len() } else { 0 };
        let offsets: Vec<usize> = (offset_start..length)
            .step_by(length / self.steps)
            .collect();
        let max_give_up = (offsets.len() / 4).max(2);
        let mut cutter = ChunkCutter::new(
            self.payload,
            candidate.encoding,
            offsets,
            self.chunk_size,
            Signature::from_flags(candidate.bom, candidate.strip_bom, self.sig),
            ChunkSource::select(candidate.encoding, decoded, candidate.multibyte, deferred),
        );
        cutter.validate().ok()?;
        let threshold = self.options.threshold;
        let mut chunks = Vec::new();
        let mut ratios = Vec::new();
        let mut early_stop = 0usize;
        for chunk in cutter.by_ref() {
            let chunk = chunk.ok()?;
            let ratio = if let Some(value) = self.mess_cache.get(&chunk) {
                *value
            } else {
                let value =
                    mess::mess_ratio_with(&chunk, threshold, self.explain_mess, self.logger);
                self.mess_cache.insert(chunk.clone(), value);
                value
            };
            chunks.push(chunk);
            ratios.push(ratio);
            if ratio >= threshold {
                early_stop += 1;
            }
            if early_stop >= max_give_up || (candidate.bom && !candidate.strip_bom) {
                break;
            }
        }
        let mean = if ratios.is_empty() {
            0.0
        } else {
            pyfloat::sum(&ratios) / ratios.len() as f64
        };
        Some(Sample {
            chunks,
            mean,
            early_stop,
            max_give_up,
        })
    }

    /// Keep a rejected ASCII/UTF-8/declared candidate as a last resort.
    fn record_fallback(&mut self, candidate: &Candidate, mut decoded: Option<String>) {
        let encoding = candidate.encoding;
        let eligible = matches!(encoding, "ascii" | "utf_8" | "utf_16" | "utf_32")
            || self.specified == Some(encoding);
        if !self.options.enable_fallback || !eligible {
            return;
        }
        if decoded.is_none() {
            match decode(self.source(candidate), encoding) {
                Ok(value) => decoded = (!self.is_too_large).then_some(value),
                Err(_) => return,
            }
        }
        let fallback = self.new_match(
            encoding,
            self.options.threshold,
            candidate.bom,
            Vec::new(),
            decoded,
        );
        if self.specified == Some(encoding) {
            self.fallback_specified = Some(fallback);
        } else if encoding == "ascii" {
            self.fallback_ascii = Some(fallback);
        } else {
            self.fallback_utf8 = Some(fallback);
        }
    }

    /// Languages coherent with the sampled chunks, merged across chunks.
    fn coherence(&mut self, candidate: &Candidate, chunks: &[String]) -> Vec<(&'static str, f64)> {
        if candidate.encoding == "ascii" {
            return Vec::new();
        }
        let inclusion = candidate.languages.join(",");
        let language_threshold = self.options.language_threshold;
        let cache = self.coherence_cache.entry(inclusion.clone()).or_default();
        let results = chunks
            .iter()
            .map(|chunk| {
                if let Some(value) = cache.get(chunk.as_str()) {
                    return value.clone();
                }
                // Inclusion lists only name profiled languages.
                let value = coherence::coherence_ratio(
                    chunk,
                    language_threshold,
                    (!inclusion.is_empty()).then_some(inclusion.as_str()),
                )
                .unwrap_or_default();
                cache.insert(chunk.clone(), value.clone());
                value
            })
            .collect();
        coherence::merge_coherence_ratios(results)
    }

    /// Try one encoding; `Break` ends detection with a final answer.
    fn try_encoding(&mut self, encoding: &'static str) -> ControlFlow<CharsetMatches> {
        let Some(candidate) = self.candidate(encoding) else {
            return ControlFlow::Continue(());
        };
        let Ok(mut decoded) = self.initial_decode(&candidate) else {
            return ControlFlow::Continue(());
        };
        let multibyte_bonus = candidate.multibyte
            && decoded
                .as_ref()
                .is_some_and(|value| value.chars().count() < self.payload.len());
        let Some(sample) = self.sample(&candidate, decoded.as_deref()) else {
            return ControlFlow::Continue(());
        };
        let threshold = self.options.threshold;
        let rejected = sample.mean >= threshold || sample.early_stop >= sample.max_give_up;
        if self.is_too_large
            && !candidate.multibyte
            && !rejected
            && decode(&self.payload[50_000.min(self.payload.len())..], encoding).is_err()
        {
            return ControlFlow::Continue(());
        }
        if rejected {
            self.soft_skip
                .extend(encoding::similar_encodings(encoding).iter().copied());
            self.record_fallback(&candidate, decoded);
            return ControlFlow::Continue(());
        }
        if !candidate.multibyte && !self.is_too_large {
            match decode(self.source(&candidate), encoding) {
                Ok(value) => decoded = Some(value),
                Err(_) => return ControlFlow::Continue(()),
            }
        }
        let merged = self.coherence(&candidate, &sample.chunks);
        self.accept(&candidate, &sample, &merged, decoded, multibyte_bonus)
    }

    /// Record a plausible candidate and apply the reference's early exits.
    fn accept(
        &mut self,
        candidate: &Candidate,
        sample: &Sample,
        merged: &[(&'static str, f64)],
        decoded: Option<String>,
        multibyte_bonus: bool,
    ) -> ControlFlow<CharsetMatches> {
        let encoding = candidate.encoding;
        let mean = sample.mean;
        let best_coherence = merged.iter().map(|item| item.1).fold(0.0, f64::max);
        let preferred = self.specified == Some(encoding) || matches!(encoding, "ascii" | "utf_8");
        let mostly_multi_byte = decoded
            .as_ref()
            .is_some_and(|value| (value.chars().count() as f64) < self.payload.len() as f64 * 0.98);
        let retained = if !self.is_too_large || preferred {
            decoded
        } else {
            None
        };
        let languages = merged
            .iter()
            .map(|(language, ratio)| ((*language).to_owned(), *ratio))
            .collect();
        let current = self.new_match(encoding, mean, candidate.bom, languages, retained);
        self.results.push(current.clone());
        if self.definitive_languages.is_some() && !candidate.multibyte && mean < 0.02 {
            self.post_definitive_success += 1;
        }
        if preferred && mean < 0.1 {
            if mean == 0.0 {
                emit(self.logger, Level::Debug, || {
                    format!("Encoding detection: {encoding} is most likely the one.")
                });
                return ControlFlow::Break(CharsetMatches::from_sorted(vec![current]));
            }
            self.early_results.push(current.clone());
        }
        let baseline_tested = self.tested.contains("ascii") && self.tested.contains("utf_8");
        if self.early_results.len() > 0
            && self
                .specified
                .is_none_or(|value| self.tested.contains(value))
            && baseline_tested
        {
            let early = std::mem::replace(&mut self.early_results, PendingMatches::new());
            return ControlFlow::Break(CharsetMatches::from_sorted(
                early.take_best().into_iter().collect(),
            ));
        }
        if self.definitive_languages.is_none()
            && !candidate.multibyte
            && best_coherence >= 0.5
            && baseline_tested
        {
            self.definitive_languages = Some(candidate.languages.iter().copied().collect());
        }
        if !self.multibyte_definitive
            && candidate.multibyte
            && multibyte_bonus
            && mostly_multi_byte
            && !is_unicode_family(encoding)
            && baseline_tested
        {
            self.multibyte_definitive = true;
        }
        if self.sig_encoding == Some(encoding) {
            return ControlFlow::Break(CharsetMatches::from_sorted(vec![current]));
        }
        ControlFlow::Continue(())
    }

    fn finish(mut self) -> CharsetMatches {
        if self.results.len() == 0
            && let Some(value) = self
                .fallback_specified
                .take()
                .or(self.fallback_utf8.take())
                .or(self.fallback_ascii.take())
        {
            self.results.push(value);
        }
        if self.results.len() > 0 {
            let alternatives = self.results.len() - 1;
            if let Some(best) = self.results.best() {
                let encoding = best.encoding().to_owned();
                emit(self.logger, Level::Debug, || {
                    format!(
                        "Encoding detection: Found {encoding} as plausible (best-candidate) for content. With {alternatives} alternatives."
                    )
                });
            }
        } else {
            emit(self.logger, Level::Debug, || {
                "Encoding detection: Unable to determine any suitable charset.".to_owned()
            });
        }
        self.results.into_matches()
    }
}

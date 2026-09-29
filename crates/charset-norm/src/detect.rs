//! The detector.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use rustc_hash::FxHashMap;

use crate::chunks::ChunkCutter;
use crate::codecs::{self, DecodeError, Errors};
use crate::encoding::{self, any_specified_encoding, identify_sig_or_bom, should_strip_sig_or_bom};
use crate::log::{emit, Level, Logger, NoLogger};
use crate::matches::{CharsetMatch, CharsetMatches, PendingMatches};
use crate::{coherence, mess, pyfloat, TOO_BIG_SEQUENCE, TOO_SMALL_SEQUENCE};

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
pub fn from_bytes(payload: &[u8]) -> CharsetMatches {
    from_bytes_with(payload, &DetectionOptions::default(), &NoLogger)
}

/// Read a file and detect its plausible encodings with default options.
pub fn from_path(path: impl AsRef<Path>) -> std::io::Result<CharsetMatches> {
    Ok(from_bytes(&std::fs::read(path)?))
}

/// Whether `payload` looks like binary data rather than text.
pub fn is_binary(payload: &[u8]) -> bool {
    let options = DetectionOptions {
        enable_fallback: false,
        ..DetectionOptions::default()
    };
    from_bytes_with(payload, &options, &NoLogger).is_empty()
}

/// Detect the plausible encodings of `payload`, reporting progress to
/// `logger`.
pub fn from_bytes_with(
    payload: &[u8],
    options: &DetectionOptions,
    logger: &dyn Logger,
) -> CharsetMatches {
    detect(Arc::from(payload), options, logger)
}

/// [`from_bytes_with`] for a payload that is already shared.
pub fn detect(
    payload: Arc<[u8]>,
    options: &DetectionOptions,
    logger: &dyn Logger,
) -> CharsetMatches {
    let bytes: &[u8] = &payload;
    let length = bytes.len();
    let threshold = options.threshold;
    let new_match = |encoding: &str,
                     chaos: f64,
                     bom: bool,
                     languages: Vec<(String, f64)>,
                     decoded: Option<String>,
                     declaration: Option<&str>| {
        CharsetMatch::new(
            payload.clone(),
            encoding,
            chaos,
            bom,
            languages,
            decoded,
            declaration.map(str::to_owned),
        )
    };

    if length == 0 {
        emit(logger, Level::Debug, || {
            "Encoding detection on empty bytes, assuming utf_8 intention.".to_owned()
        });
        return CharsetMatches::from_sorted(vec![new_match(
            "utf_8",
            0.0,
            false,
            Vec::new(),
            Some(String::new()),
            None,
        )]);
    }

    let normalize = |values: &[String]| -> Vec<String> {
        values
            .iter()
            .map(|value| encoding::iana_name(value, false).unwrap_or_default())
            .collect()
    };
    let isolation = normalize(&options.cp_isolation);
    let exclusion = normalize(&options.cp_exclusion);

    let mut steps = options.steps;
    let mut chunk_size = options.chunk_size;
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
        emit(logger, Level::Trace, || {
            format!("Trying to detect encoding from a tiny portion of ({length}) byte(s).")
        });
    }

    let specified = if options.preemptive_behaviour {
        any_specified_encoding(bytes, 8192)
    } else {
        None
    };
    let mut prioritized = Vec::<&str>::new();
    if let Some(value) = specified {
        prioritized.push(value);
    }
    let (sig_encoding, sig) = identify_sig_or_bom(bytes);
    if let Some(value) = sig_encoding {
        prioritized.insert(0, value);
    }
    prioritized.push("ascii");
    if !prioritized.contains(&"utf_8") {
        prioritized.push("utf_8");
    }
    prioritized.extend_from_slice(encoding::supported_encodings());

    let mut results = PendingMatches::new();
    let mut early_results = PendingMatches::new();
    let mut tested = HashSet::<&str>::new();
    let mut soft_skip = HashSet::<&str>::new();
    let mut fallback_ascii: Option<CharsetMatch> = None;
    let mut fallback_utf8: Option<CharsetMatch> = None;
    let mut fallback_specified: Option<CharsetMatch> = None;
    let mut definitive = false;
    let mut definitive_languages = HashSet::<&str>::new();
    let mut post_definitive_success = 0usize;
    let mut multibyte_definitive = false;
    let mut mess_cache = FxHashMap::<String, f64>::default();
    // inclusion -> chunk -> coherence results
    let mut coherence_cache =
        FxHashMap::<String, FxHashMap<String, Vec<(&'static str, f64)>>>::default();
    let explain_mess = options.explain && (1..=2).contains(&isolation.len());

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
        let multibyte = encoding::is_multi_byte_encoding(encoding);
        let target_languages = encoding::target_languages(encoding);
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
                    let value = mess::mess_ratio_with(&chunk, threshold, explain_mess, logger);
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
            pyfloat::sum(&ratios) / ratios.len() as f64
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
            soft_skip.extend(encoding::similar_encodings(encoding).iter().copied());
            if options.enable_fallback
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
                let fallback = new_match(encoding, threshold, bom, Vec::new(), decoded, specified);
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
                        // Inclusion lists only name profiled languages.
                        let value = coherence::coherence_ratio(
                            chunk,
                            options.language_threshold,
                            (!inclusion.is_empty()).then_some(inclusion.as_str()),
                        )
                        .unwrap_or_default();
                        cache.insert(chunk.clone(), value.clone());
                        value
                    }
                };
                coherence_results.push(values);
            }
        }
        let merged = coherence::merge_coherence_ratios(coherence_results);
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
            encoding,
            mean,
            bom,
            merged
                .iter()
                .map(|(language, ratio)| ((*language).to_owned(), *ratio))
                .collect(),
            retained_decoded,
            specified,
        );
        results.push(current.clone());
        if definitive && !multibyte && mean < 0.02 {
            post_definitive_success += 1;
        }
        if (specified == Some(encoding) || matches!(encoding, "ascii" | "utf_8")) && mean < 0.1 {
            if mean == 0.0 {
                emit(logger, Level::Debug, || {
                    format!("Encoding detection: {encoding} is most likely the one.")
                });
                return CharsetMatches::from_sorted(vec![current]);
            }
            early_results.push(current.clone());
        }
        if early_results.len() > 0
            && specified.is_none_or(|value| tested.contains(value))
            && tested.contains("ascii")
            && tested.contains("utf_8")
        {
            return CharsetMatches::from_sorted(early_results.take_best().into_iter().collect());
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
            return CharsetMatches::from_sorted(vec![current]);
        }
    }

    if results.len() == 0 {
        if let Some(value) = fallback_specified.or(fallback_utf8).or(fallback_ascii) {
            results.push(value);
        }
    }
    if results.len() > 0 {
        let alternatives = results.len() - 1;
        if let Some(best) = results.best() {
            let encoding = best.encoding().to_owned();
            emit(logger, Level::Debug, || {
                format!(
                    "Encoding detection: Found {encoding} as plausible (best-candidate) for content. With {alternatives} alternatives."
                )
            });
        }
    } else {
        emit(logger, Level::Debug, || {
            "Encoding detection: Unable to determine any suitable charset.".to_owned()
        });
    }
    results.into_matches()
}

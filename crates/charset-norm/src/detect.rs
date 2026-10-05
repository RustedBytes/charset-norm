//! The detector: [`from_bytes`] and friends, re-exported at the crate root.

use std::borrow::Cow;
use std::hash::{Hash, Hasher};
use std::iter::StepBy;
use std::ops::{ControlFlow, Range};
use std::path::Path;
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet, FxHasher};

use crate::chunks::{ChunkCutter, ChunkSource, Signature};
use crate::codecs::{self, DecodeError, Errors};
use crate::encoding::{self, any_specified_encoding, identify_sig_or_bom, should_strip_sig_or_bom};
use crate::log::{Level, Logger, NoLogger, emit};
use crate::matches::{CharsetMatch, CharsetMatches, PendingMatches};
use crate::{TOO_BIG_SEQUENCE, TOO_SMALL_SEQUENCE, coherence, mess, pyfloat};

/// Tuning knobs of [`from_bytes_with`] and [`detect`].
///
/// The [`Default`] values match the Python package's `from_bytes`. Override
/// only the fields you need with struct update syntax:
///
/// ```
/// use charset_norm::DetectionOptions;
///
/// let options = DetectionOptions {
///     // Never consider these code pages, whatever the payload looks like.
///     cp_exclusion: vec!["utf_7".into(), "hz".into()],
///     // Look at more of the payload.
///     steps: 10,
///     ..DetectionOptions::default()
/// };
/// assert_eq!(options.chunk_size, 512);
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct DetectionOptions {
    /// Number of chunks sampled from the payload. Default: `5`.
    ///
    /// Chunks are spread evenly over the payload. When the payload is not
    /// longer than `steps * chunk_size` bytes, it is analysed whole, as a
    /// single chunk.
    pub steps: usize,
    /// Size of each sampled chunk, in bytes. Default: `512`.
    ///
    /// Shrunk automatically when the payload is too short to hold `steps`
    /// chunks of this size.
    pub chunk_size: usize,
    /// Maximum acceptable mean chaos (see [`mess::mess_ratio`]) for a
    /// candidate encoding. Default: `0.2`.
    ///
    /// Lower values reject more candidates; raise it to accept noisier text.
    pub threshold: f64,
    /// When not empty, only these encodings are tried. Default: empty.
    ///
    /// Names may be any alias (`"windows-1252"`, `"UTF-8"`, ...).
    pub cp_isolation: Vec<String>,
    /// Encodings never tried, by any alias. Default: empty.
    pub cp_exclusion: Vec<String>,
    /// Favour an encoding declared inside the payload, such as
    /// `<meta charset="...">` in HTML or a `# coding: ...` comment in Python
    /// sources. Default: `true`.
    ///
    /// Only the first 8 KiB are searched. A declared encoding is tried right
    /// after any BOM-announced one, and detection stops there when it
    /// decodes the payload without any chaos.
    pub preemptive_behaviour: bool,
    /// Log the chaos breakdown of every sampled chunk at [`Level::Trace`].
    /// Default: `false`.
    ///
    /// Only takes effect when [`cp_isolation`](Self::cp_isolation) names one
    /// or two encodings, to keep the output readable.
    pub explain: bool,
    /// Minimum coherence (in `[0, 1]`) for a language to be reported.
    /// Default: `0.1`.
    pub language_threshold: f64,
    /// Fall back on ASCII, UTF-8 or a declared encoding when every candidate
    /// is rejected but one of those still decodes the payload. Default:
    /// `true`.
    ///
    /// Without the fallback, text that is merely noisy can come back as no
    /// match at all (see [`is_binary`]).
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

fn decode_into(bytes: &[u8], encoding: &str, out: &mut String) -> Result<(), DecodeError> {
    codecs::decode_into(bytes, encoding, Errors::Strict, out)
}

/// Detect the plausible encodings of `payload` with default options.
///
/// Results are ranked best first; [`CharsetMatches::best`] is the verdict.
/// An empty result means no encoding fits, which usually means `payload` is
/// binary. An empty payload yields a single `"utf_8"` match.
///
/// Use [`from_bytes_with`] to tune detection or collect diagnostics.
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
/// The whole file is read into memory; payloads of [`TOO_BIG_SEQUENCE`]
/// bytes or more are sampled rather than fully decoded.
///
/// ```no_run
/// let results = charset_norm::from_path("subtitles.srt")?;
/// if let Some(best) = results.best() {
///     println!("{} ({})", best.encoding(), best.language());
/// }
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// # Errors
///
/// Any error reading the file.
pub fn from_path(path: impl AsRef<Path>) -> std::io::Result<CharsetMatches> {
    Ok(from_bytes(&std::fs::read(path)?))
}

/// Whether `payload` looks like binary data rather than text.
///
/// This runs a full detection with
/// [`enable_fallback`](DetectionOptions::enable_fallback) turned off and
/// reports whether no encoding fits.
///
/// ```
/// assert!(!charset_norm::is_binary("Plain old text, nothing to see here.".as_bytes()));
/// assert!(charset_norm::is_binary(b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x01\x00\x00\x00\x01\x00\x08\x06\x00\x00\x00\x5c\x72\xa8\x66\x00\x00\x00\x04\x73\x42\x49\x54\x08\x08"));
/// ```
#[must_use]
pub fn is_binary(payload: &[u8]) -> bool {
    let options = DetectionOptions {
        enable_fallback: false,
        ..DetectionOptions::default()
    };
    from_bytes_with(payload, &options, &NoLogger).is_empty()
}

/// Detect the plausible encodings of `payload` with custom `options`,
/// reporting progress to `logger`.
///
/// Pass [`NoLogger`] to discard diagnostics.
///
/// ```
/// use charset_norm::{DetectionOptions, NoLogger, codecs, from_bytes_with};
///
/// let payload = codecs::encode("Le cœur a ses raisons que la raison ne connaît point.", "cp1252").unwrap();
/// let options = DetectionOptions {
///     cp_isolation: vec!["windows-1252".into(), "iso-8859-15".into()],
///     ..DetectionOptions::default()
/// };
///
/// let results = from_bytes_with(&payload, &options, &NoLogger);
/// let best = results.best().unwrap();
/// assert_eq!(best.encoding(), "cp1252");
/// ```
#[must_use]
pub fn from_bytes_with(
    payload: &[u8],
    options: &DetectionOptions,
    logger: &dyn Logger,
) -> CharsetMatches {
    detect(&Arc::from(payload), options, logger)
}

/// [`from_bytes_with`] for a payload that is already shared: the matches keep
/// a reference to it instead of a copy.
///
/// ```
/// use std::sync::Arc;
/// use charset_norm::{DetectionOptions, NoLogger};
///
/// let payload: Arc<[u8]> = Arc::from("Shared, not copied.".as_bytes());
/// let results = charset_norm::detect(&payload, &DetectionOptions::default(), &NoLogger);
/// assert!(Arc::ptr_eq(results.best().unwrap().payload(), &payload));
/// ```
#[must_use]
#[cfg_attr(feature = "hotpath", hotpath::measure)]
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

/// Every distinct chunk text seen during one detection, stored once (in a
/// single arena) with its mess ratio. Chunks are identified by their index.
#[derive(Default)]
struct ChunkStore {
    arena: String,
    /// (start, end) of each chunk in `arena`.
    spans: Vec<(usize, usize)>,
    mess: Vec<f64>,
    /// Text hash -> most recent chunk with that hash.
    heads: FxHashMap<u64, u32>,
    /// Chunk -> previous chunk with the same hash (`u32::MAX` ends the chain).
    next: Vec<u32>,
}

impl ChunkStore {
    fn hash(text: &str) -> u64 {
        let mut hasher = FxHasher::default();
        text.hash(&mut hasher);
        hasher.finish()
    }

    fn text(&self, id: u32) -> &str {
        let (start, end) = self.spans[id as usize];
        &self.arena[start..end]
    }

    fn find(&self, hash: u64, text: &str) -> Option<u32> {
        let mut current = *self.heads.get(&hash)?;
        while current != u32::MAX {
            if self.text(current) == text {
                return Some(current);
            }
            current = self.next[current as usize];
        }
        None
    }

    /// The id of `text`, storing it (and computing its mess) on first sight.
    fn intern(&mut self, text: &str, mess: impl FnOnce() -> f64) -> u32 {
        let hash = Self::hash(text);
        if let Some(id) = self.find(hash, text) {
            return id;
        }
        let id = u32::try_from(self.spans.len()).expect("fewer than 2^32 chunks");
        let start = self.arena.len();
        self.arena.push_str(text);
        self.spans.push((start, self.arena.len()));
        self.mess.push(mess());
        self.next
            .push(self.heads.insert(hash, id).unwrap_or(u32::MAX));
        id
    }

    fn clear(&mut self) {
        self.arena.clear();
        self.spans.clear();
        self.mess.clear();
        self.heads.clear();
        self.next.clear();
    }
}

/// Span in `Buffers::coherence_values` of the results per (inclusion list,
/// chunk).
type CoherenceCache = FxHashMap<(&'static [&'static str], u32), (usize, usize)>;

/// Working memory of a detection, kept per thread between detections.
#[derive(Default)]
struct Buffers {
    chunks: ChunkStore,
    coherence_cache: CoherenceCache,
    coherence_values: Vec<(&'static str, f64)>,
    coherence_scratch: coherence::Scratch,
    merged: Vec<(&'static str, f64)>,
    merge_groups: coherence::Groups,
    /// Chunks sampled for the current candidate.
    chunk_ids: Vec<u32>,
    /// Decoded payload of the current candidate.
    text: String,
    chunk: String,
    tested: FxHashSet<&'static str>,
    soft_skip: FxHashSet<&'static str>,
}

impl Buffers {
    /// Largest buffer kept for the next detection.
    const KEEP_LIMIT: usize = 1 << 20;

    thread_local! {
        static POOL: std::cell::RefCell<Option<Buffers>> = const { std::cell::RefCell::new(None) };
    }

    fn take() -> Self {
        Self::POOL
            .try_with(|pool| pool.borrow_mut().take())
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// Clear and return to the pool, dropping oversized allocations.
    fn recycle(mut self) {
        self.chunks.clear();
        self.coherence_cache.clear();
        self.coherence_values.clear();
        self.chunk_ids.clear();
        self.tested.clear();
        self.soft_skip.clear();
        if self.text.capacity() > Self::KEEP_LIMIT {
            self.text = String::new();
        }
        if self.chunks.arena.capacity() > Self::KEEP_LIMIT {
            self.chunks = ChunkStore::default();
        }
        let _ = Self::POOL.try_with(|pool| *pool.borrow_mut() = Some(self));
    }
}

/// Mess analysis of one candidate encoding (sampled chunks are left in
/// `Detector::chunk_ids`).
struct Sample {
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
    languages: &'static [&'static str],
}

/// State of one detection run (the reference algorithm's local variables),
/// plus buffers reused from one candidate to the next.
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
    fallback_ascii: Option<CharsetMatch>,
    fallback_utf8: Option<CharsetMatch>,
    fallback_specified: Option<CharsetMatch>,
    /// Languages of the first coherent single-byte match, once found.
    definitive_languages: Option<FxHashSet<&'static str>>,
    post_definitive_success: usize,
    multibyte_definitive: bool,
    buffers: Buffers,
}

impl<'a> Detector<'a> {
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
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
            fallback_ascii: None,
            fallback_utf8: None,
            fallback_specified: None,
            definitive_languages: None,
            post_definitive_success: 0,
            multibyte_definitive: false,
            buffers: Buffers::take(),
        }
    }

    fn run(mut self) -> CharsetMatches {
        let mut answer = None;
        for encoding in self.prioritized() {
            if let ControlFlow::Break(matches) = self.try_encoding(encoding) {
                answer = Some(matches);
                break;
            }
        }
        let matches = answer.unwrap_or_else(|| self.finish());
        self.buffers.recycle();
        matches
    }

    /// Candidates in trial order: signature, declared encoding, ASCII, UTF-8,
    /// then every supported encoding (multi-byte first).
    fn prioritized(&self) -> impl Iterator<Item = &'static str> + use<> {
        let mut first: [Option<&'static str>; 4] = [self.sig_encoding, self.specified, None, None];
        if first[0].is_none() {
            first.rotate_left(1);
        }
        let mut count = first.iter().flatten().count();
        first[count] = Some("ascii");
        count += 1;
        if !first[..count].contains(&Some("utf_8")) {
            first[count] = Some("utf_8");
        }
        first
            .into_iter()
            .flatten()
            .chain(encoding::supported_encodings().iter().copied())
    }

    fn new_match(
        &self,
        encoding: &'static str,
        chaos: f64,
        bom: bool,
        languages: impl Into<Arc<[(Cow<'static, str>, f64)]>>,
        decoded: Option<String>,
    ) -> CharsetMatch {
        CharsetMatch::new(
            self.payload.clone(),
            encoding,
            chaos,
            bom,
            languages,
            decoded,
            self.specified.map(Cow::Borrowed),
        )
    }

    /// Whether `encoding` should be tried at all, recording it as tested.
    fn candidate(&mut self, encoding: &'static str) -> Option<Candidate> {
        if (!self.isolation.is_empty() && !self.isolation.iter().any(|value| value == encoding))
            || self.exclusion.iter().any(|value| value == encoding)
            || !self.buffers.tested.insert(encoding)
        {
            return None;
        }
        let bom = self.sig_encoding == Some(encoding);
        if (matches!(encoding, "utf_16" | "utf_32" | "utf_7") && !bom)
            || self.buffers.soft_skip.contains(encoding)
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

    /// ASCII, UTF-8 and the declared encoding: their matches end detection
    /// early and keep their decoded text.
    fn is_preferred(&self, encoding: &str) -> bool {
        self.specified == Some(encoding) || matches!(encoding, "ascii" | "utf_8")
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
    /// prefix of large payloads). Returns whether `text` now holds the whole
    /// decoded payload; `Err` rejects the candidate.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn initial_decode(
        &self,
        candidate: &Candidate,
        text: &mut String,
    ) -> Result<bool, DecodeError> {
        let encoding = candidate.encoding;
        let source = self.source(candidate);
        if self.is_too_large && !candidate.multibyte {
            decode_into(&source[..source.len().min(500_000)], encoding, text)?;
            return Ok(false);
        }
        if candidate.multibyte {
            let bom_kept_utf7 = encoding == "utf_7" && candidate.bom;
            let payload: &[u8] = self.payload;
            decode_into(if bom_kept_utf7 { payload } else { source }, encoding, text)?;
            if bom_kept_utf7 && text.starts_with('\u{feff}') {
                text.remove(0);
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// Measure the mess of sampled chunks, recording them in `chunk_ids`;
    /// `None` when a chunk does not decode.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn sample(&mut self, candidate: &Candidate, decoded: Option<&str>) -> Option<Sample> {
        let length = self.payload.len();
        let deferred = !candidate.multibyte && !self.is_too_large;
        let offset_start = if candidate.bom { self.sig.len() } else { 0 };
        let offsets: StepBy<Range<usize>> = (offset_start..length).step_by(length / self.steps);
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
        let (explain, logger) = (self.explain_mess, self.logger);
        let buffers = &mut self.buffers;
        buffers.chunk_ids.clear();
        let mut total = pyfloat::Sum::default();
        let mut early_stop = 0usize;
        while let Some(result) = cutter.next_into(&mut buffers.chunk) {
            result.ok()?;
            let chunk = &buffers.chunk;
            let id = buffers.chunks.intern(chunk, || {
                mess::mess_ratio_with(chunk, threshold, explain, logger)
            });
            let ratio = buffers.chunks.mess[id as usize];
            buffers.chunk_ids.push(id);
            total.add(ratio);
            if ratio >= threshold {
                early_stop += 1;
            }
            if early_stop >= max_give_up || (candidate.bom && !candidate.strip_bom) {
                break;
            }
        }
        let mean = if buffers.chunk_ids.is_empty() {
            0.0
        } else {
            total.value() / buffers.chunk_ids.len() as f64
        };
        Some(Sample {
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

    /// Languages coherent with the sampled chunks, merged across chunks,
    /// left in `Buffers::merged`.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn coherence(&mut self, candidate: &Candidate) {
        let buffers = &mut self.buffers;
        buffers.merged.clear();
        if candidate.encoding == "ascii" {
            return;
        }
        let languages = candidate.languages;
        for &id in &buffers.chunk_ids {
            if !buffers.coherence_cache.contains_key(&(languages, id)) {
                let start = buffers.coherence_values.len();
                // Inclusion lists only name profiled languages.
                if coherence::coherence_into(
                    buffers.chunks.text(id),
                    self.options.language_threshold,
                    languages,
                    &mut buffers.coherence_scratch,
                    &mut buffers.coherence_values,
                )
                .is_err()
                {
                    buffers.coherence_values.truncate(start);
                }
                let end = buffers.coherence_values.len();
                buffers
                    .coherence_cache
                    .insert((languages, id), (start, end));
            }
        }
        let values = &buffers.coherence_values;
        let cache = &buffers.coherence_cache;
        coherence::merge_into(
            buffers.chunk_ids.iter().map(|id| {
                let (start, end) = cache[&(languages, *id)];
                &values[start..end]
            }),
            &mut buffers.merge_groups,
            &mut buffers.merged,
        );
    }

    /// Try one encoding; `Break` ends detection with a final answer.
    fn try_encoding(&mut self, encoding: &'static str) -> ControlFlow<CharsetMatches> {
        let Some(candidate) = self.candidate(encoding) else {
            return ControlFlow::Continue(());
        };
        let mut text = std::mem::take(&mut self.buffers.text);
        let flow = self.evaluate(&candidate, &mut text);
        self.buffers.text = text;
        flow
    }

    /// Evaluate a candidate, using `text` for its decoded payload.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn evaluate(
        &mut self,
        candidate: &Candidate,
        text: &mut String,
    ) -> ControlFlow<CharsetMatches> {
        let encoding = candidate.encoding;
        let Ok(mut decoded) = self.initial_decode(candidate, text) else {
            return ControlFlow::Continue(());
        };
        // Character count of the up-front decode (multi-byte candidates).
        let decoded_chars = decoded.then(|| text.chars().count());
        let Some(sample) = self.sample(candidate, decoded.then_some(text.as_str())) else {
            return ControlFlow::Continue(());
        };
        let rejected =
            sample.mean >= self.options.threshold || sample.early_stop >= sample.max_give_up;
        if self.is_too_large
            && !candidate.multibyte
            && !rejected
            && decode_into(
                &self.payload[50_000.min(self.payload.len())..],
                encoding,
                text,
            )
            .is_err()
        {
            return ControlFlow::Continue(());
        }
        if rejected {
            self.buffers
                .soft_skip
                .extend(encoding::similar_encodings(encoding).iter().copied());
            let decoded_text = decoded.then(|| std::mem::take(text));
            self.record_fallback(candidate, decoded_text);
            return ControlFlow::Continue(());
        }
        if !candidate.multibyte && !self.is_too_large {
            // The whole payload must decode. Only the text of a preferred
            // encoding is kept up front (as the reference does for large
            // payloads); other code pages are checked against their table
            // and decode lazily, as most matches are never read.
            if self.is_preferred(encoding) {
                if decode_into(self.source(candidate), encoding, text).is_err() {
                    return ControlFlow::Continue(());
                }
                decoded = true;
            } else if !codecs::is_valid(self.source(candidate), encoding).unwrap_or(false) {
                return ControlFlow::Continue(());
            }
        }
        self.coherence(candidate);
        self.accept(candidate, &sample, decoded.then_some(text), decoded_chars)
    }

    /// Record a plausible candidate and apply the reference's early exits.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn accept(
        &mut self,
        candidate: &Candidate,
        sample: &Sample,
        decoded: Option<&mut String>,
        decoded_chars: Option<usize>,
    ) -> ControlFlow<CharsetMatches> {
        let encoding = candidate.encoding;
        let length = self.payload.len();
        let mean = sample.mean;
        let merged = &self.buffers.merged;
        let best_coherence = merged.iter().map(|item| item.1).fold(0.0, f64::max);
        let preferred = self.is_preferred(encoding);
        let retained = if !self.is_too_large || preferred {
            decoded.map(std::mem::take)
        } else {
            None
        };
        let languages: Arc<[(Cow<'static, str>, f64)]> = merged
            .iter()
            .map(|&(language, ratio)| (Cow::Borrowed(language), ratio))
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
        let tested = &self.buffers.tested;
        let baseline_tested = tested.contains("ascii") && tested.contains("utf_8");
        if self.early_results.len() > 0
            && self.specified.is_none_or(|value| tested.contains(value))
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
        // Multi-byte candidates were decoded up front.
        let multibyte_bonus = decoded_chars.is_some_and(|count| count < length);
        let mostly_multi_byte =
            decoded_chars.is_some_and(|count| (count as f64) < length as f64 * 0.98);
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

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn finish(&mut self) -> CharsetMatches {
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
        std::mem::replace(&mut self.results, PendingMatches::new()).into_matches()
    }
}

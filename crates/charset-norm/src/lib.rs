//! Universal character encoding detector.
//!
//! `charset-norm` is a Rust implementation of
//! [charset_normalizer](https://github.com/jawah/charset_normalizer). Give it
//! bytes of unknown origin and it tells you which encodings they were
//! plausibly written in, which language the text is in, and how confident it
//! is. It knows 98 code pages, from UTF-8 and the Windows/ISO-8859 families to
//! the CJK and ISO-2022 encodings.
//!
//! Decoding reproduces `CPython`'s codecs byte for byte, so verdicts agree with
//! the Python package.
//!
//! # Quick start
//!
//! ```
//! let results = charset_norm::from_bytes("Ça va très bien, merci.".as_bytes());
//! let best = results.best().expect("text payload");
//! assert_eq!(best.encoding(), "utf_8");
//! assert_eq!(best.decoded().unwrap(), "Ça va très bien, merci.");
//! ```
//!
//! [`from_bytes`] returns [`CharsetMatches`], the plausible encodings best
//! first. It is empty when nothing fits, which usually means the payload is
//! binary. Each [`CharsetMatch`] exposes the encoding, the decoded text, the
//! detected languages and the scores behind the verdict:
//!
//! ```
//! use charset_norm::codecs;
//!
//! let text = "Всеки човек има право на образование. Образованието трябва да бъде безплатно.";
//! let payload = codecs::encode(text, "cp1251").unwrap();
//!
//! let results = charset_norm::from_bytes(&payload);
//! let best = results.best().unwrap();
//! assert_eq!(best.encoding(), "cp1251");
//! assert!(best.chaos() < 0.1);
//!
//! // Languages come ranked by coherence. On one short sentence, Cyrillic
//! // languages are hard to tell apart, so look at the whole list.
//! assert!(best.languages().contains(&"Bulgarian"));
//!
//! // Transcode to UTF-8 (in-document charset declarations are rewritten too).
//! assert_eq!(best.output("utf_8").unwrap(), text.as_bytes());
//! ```
//!
//! Files are read with [`from_path`]; [`is_binary`] answers the text vs.
//! binary question alone.
//!
//! # Tuning detection
//!
//! [`from_bytes_with`] takes [`DetectionOptions`] to restrict the candidate
//! encodings, change how much of the payload is sampled, or set how noisy a
//! decoding may look before it is rejected. The defaults match the Python
//! package.
//!
//! ```
//! use charset_norm::{DetectionOptions, NoLogger, codecs, from_bytes_with};
//!
//! let payload = codecs::encode("Příliš žluťoučký kůň úpěl ďábelské ódy.", "cp1250").unwrap();
//! let options = DetectionOptions {
//!     cp_isolation: vec!["cp1250".into(), "cp1252".into()],
//!     ..DetectionOptions::default()
//! };
//! let results = from_bytes_with(&payload, &options, &NoLogger);
//! assert_eq!(results.best().unwrap().encoding(), "cp1250");
//! ```
//!
//! # How it works
//!
//! Candidates are tried in order: the encoding announced by a byte order
//! mark, the one declared in the document (`<meta charset>`, `# coding:`),
//! ASCII, UTF-8, then every other code page. For each one, the detector:
//!
//! 1. Skips it when the payload cannot be decoded with it, or when a
//!    near-identical code page (see [`encoding::is_cp_similar`]) was already
//!    rejected.
//! 2. Decodes [`steps`](DetectionOptions::steps) chunks of
//!    [`chunk_size`](DetectionOptions::chunk_size) bytes spread over the
//!    payload.
//! 3. Scores each chunk's **chaos** with [`mess::mess_ratio`]: suspicious
//!    character successions, unexpected accents, unprintable characters and
//!    so on. Candidates whose mean chaos reaches
//!    [`threshold`](DetectionOptions::threshold) are dropped.
//! 4. Scores the survivors' **coherence** with
//!    [`coherence::coherence_ratio`]: how well letter frequencies match known
//!    languages.
//! 5. Ranks the matches: lower chaos first, then higher coherence. Encodings
//!    that produce the very same text become
//!    [submatches](CharsetMatch::submatches) of one another.
//!
//! Detection stops early once a preferred candidate (declared, ASCII or
//! UTF-8) decodes cleanly. When no candidate survives, ASCII, UTF-8 or a
//! declared encoding may still
//! be returned as a fallback (see
//! [`enable_fallback`](DetectionOptions::enable_fallback)).
//!
//! # Modules
//!
//! The top-level functions cover most needs. The building blocks are public
//! for tools that want them:
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`codecs`] | `CPython`-compatible decoders and encoders |
//! | [`encoding`] | Code page names and aliases, BOMs, declared charsets, code page languages |
//! | [`mess`] | Chaos (noise) scoring of decoded text |
//! | [`coherence`] | Language detection from letter frequencies |
//! | [`unicode`] | Character properties (`unicodedata` semantics) |
//! | [`chunks`] | Sampling of decoded chunks, as the detector does it |
//! | [`log`] | Diagnostics emitted while detecting |
//!
//! # Encoding names
//!
//! Encodings are reported by their canonical `CPython` names: `"utf_8"`,
//! `"cp1252"`, `"shift_jis"`, `"iso8859_5"`. Wherever a name is accepted,
//! aliases such as `"UTF-8"`, `"windows-1252"` or `"latin-1"` work too; see
//! [`encoding::iana_name`] and [`encoding::supported_encodings`].
//!
//! # Diagnostics
//!
//! Detection reports what it tries and why it rejects candidates through a
//! [`Logger`]. [`NoLogger`] discards everything; implement the trait to
//! collect messages yourself, or enable the `log` feature and pass
//! `log::LogCrate` to forward them to the [`log`](https://docs.rs/log)
//! crate.
//!
//! # Feature flags
//!
//! - `log` (off by default): adds `log::LogCrate`, a [`Logger`] that forwards
//!   diagnostics to the `log` crate under the `charset_norm` target.
//!
//! # Minimum supported Rust version
//!
//! Rust 1.98.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod chunks;
pub mod codecs;
pub mod coherence;
mod detect;
pub mod encoding;
mod error;
pub mod log;
mod matches;
pub mod mess;
mod pyfloat;
mod stackfmt;
mod tables;
pub mod unicode;

pub use detect::{DetectionOptions, detect, from_bytes, from_bytes_with, from_path, is_binary};
pub use error::Error;
pub use log::{Level, Logger, NoLogger};
pub use matches::{CharsetMatch, CharsetMatches, OutputError, sort_by_rank};

/// Payloads shorter than this many bytes are considered too small for
/// reliable detection.
///
/// They are still analysed, but as a single chunk, and verdicts on so little
/// text are best treated as guesses.
pub const TOO_SMALL_SEQUENCE: usize = 32;

/// Payloads of at least this many bytes are sampled rather than fully decoded.
///
/// For such payloads, [`CharsetMatch::decoded`] decodes on demand instead of
/// keeping the text detection produced, and duplicate matches are no longer
/// folded into submatches.
pub const TOO_BIG_SEQUENCE: usize = 10_000_000;

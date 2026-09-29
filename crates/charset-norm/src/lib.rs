//! Universal character encoding detector.
//!
//! `charset-norm` is a Rust implementation of
//! [charset_normalizer](https://github.com/jawah/charset_normalizer). It
//! finds the encodings a payload was plausibly written in by decoding it with
//! every supported code page, measuring how noisy the result looks ("chaos")
//! and how well it matches known languages ("coherence").
//!
//! Decoding reproduces `CPython`'s codecs byte for byte, so results agree with
//! the Python package.
//!
//! ```
//! let results = charset_norm::from_bytes("Ça va très bien, merci.".as_bytes());
//! let best = results.best().expect("text payload");
//! assert_eq!(best.encoding(), "utf_8");
//! assert_eq!(best.decoded().unwrap(), "Ça va très bien, merci.");
//! ```
//!
//! Tune detection with [`DetectionOptions`] and [`from_bytes_with`]; route
//! diagnostics through a [`Logger`] (enable the `log` feature to forward
//! them to the `log` crate).

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

/// Payloads shorter than this are considered too small for reliable detection.
pub const TOO_SMALL_SEQUENCE: usize = 32;

/// Payloads at least this long are sampled rather than fully decoded.
pub const TOO_BIG_SEQUENCE: usize = 10_000_000;

//! Diagnostics emitted while detecting.
//!
//! Detection reports its progress through a [`Logger`]: which encodings it
//! tries, why it rejects them, and which one it settles on. Nothing is
//! emitted by default ([`NoLogger`]). Implement the trait to route messages
//! elsewhere, or enable the `log` feature and use `LogCrate` to forward them
//! to the [`log`](https://docs.rs/log) facade.
//!
//! Messages are formatted only when [`Logger::enabled`] returns `true` for
//! their level, so a logger that ignores a level costs nothing for it.
//!
//! ```
//! use std::cell::RefCell;
//! use charset_norm::{DetectionOptions, Level, Logger, from_bytes_with};
//!
//! #[derive(Default)]
//! struct Collect(RefCell<Vec<String>>);
//!
//! impl Logger for Collect {
//!     fn enabled(&self, level: Level) -> bool {
//!         level >= Level::Debug
//!     }
//!
//!     fn log(&self, _level: Level, message: &str) {
//!         self.0.borrow_mut().push(message.to_owned());
//!     }
//! }
//!
//! let logger = Collect::default();
//! from_bytes_with("Hello, world! Plain ASCII text.".as_bytes(), &DetectionOptions::default(), &logger);
//! assert!(!logger.0.borrow().is_empty());
//! ```

/// Severity of a diagnostic message.
///
/// Levels are ordered: `Trace < Debug`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// Detailed tracing of the detection process: every candidate tried,
    /// and the chaos breakdown when
    /// [`DetectionOptions::explain`](crate::DetectionOptions::explain) is set.
    Trace,
    /// Notable detection outcomes, such as the encoding settled on.
    Debug,
}

impl Level {
    /// The equivalent numeric level of Python's `logging` module.
    #[must_use]
    pub fn python_level(self) -> i32 {
        match self {
            Level::Trace => 5,
            Level::Debug => 10,
        }
    }
}

/// Receiver for detection diagnostics.
///
/// Implementations must be cheap to call from the detector's loop. See the
/// [module documentation](self) for an example.
pub trait Logger {
    /// Whether messages at `level` are wanted. Messages are only formatted
    /// when this returns `true`.
    fn enabled(&self, level: Level) -> bool;

    /// Record one message.
    fn log(&self, level: Level, message: &str);
}

/// A logger that discards everything.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoLogger;

impl Logger for NoLogger {
    fn enabled(&self, _level: Level) -> bool {
        false
    }

    fn log(&self, _level: Level, _message: &str) {}
}

/// Forwards diagnostics to the [`log`](https://docs.rs/log) facade under the
/// `charset_norm` target.
///
/// [`Level::Trace`] maps to `log::Level::Trace` and [`Level::Debug`] to
/// `log::Level::Debug`; the facade's filter decides what is emitted.
///
/// ```
/// use charset_norm::{DetectionOptions, from_bytes_with, log::LogCrate};
///
/// let results = from_bytes_with(b"Hello, world!", &DetectionOptions::default(), &LogCrate);
/// # assert!(results.best().is_some());
/// ```
#[cfg(feature = "log")]
#[cfg_attr(docsrs, doc(cfg(feature = "log")))]
#[derive(Clone, Copy, Debug, Default)]
pub struct LogCrate;

#[cfg(feature = "log")]
impl LogCrate {
    fn level(level: Level) -> log::Level {
        match level {
            Level::Trace => log::Level::Trace,
            Level::Debug => log::Level::Debug,
        }
    }
}

#[cfg(feature = "log")]
impl Logger for LogCrate {
    fn enabled(&self, level: Level) -> bool {
        log::log_enabled!(target: "charset_norm", Self::level(level))
    }

    fn log(&self, level: Level, message: &str) {
        log::log!(target: "charset_norm", Self::level(level), "{message}");
    }
}

/// Format and emit `message` only when `level` is enabled.
pub(crate) fn emit(logger: &dyn Logger, level: Level, message: impl FnOnce() -> String) {
    if logger.enabled(level) {
        logger.log(level, &message());
    }
}

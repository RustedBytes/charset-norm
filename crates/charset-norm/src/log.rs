//! Diagnostics emitted while detecting.
//!
//! Detection reports its progress through a [`Logger`]. Nothing is emitted by
//! default; implement the trait to route messages elsewhere, or enable the
//! `log` feature and use [`LogCrate`] to forward them to the `log` facade.

/// Severity of a diagnostic message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// Detailed tracing of the detection process.
    Trace,
    /// Notable detection outcomes.
    Debug,
}

impl Level {
    /// The equivalent numeric level of Python's `logging` module.
    pub fn python_level(self) -> i32 {
        match self {
            Level::Trace => 5,
            Level::Debug => 10,
        }
    }
}

/// Receiver for detection diagnostics.
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
#[cfg(feature = "log")]
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

//! Bridge from the core's [`Logger`] to Python's `charset_norm` logger.
//!
//! Detection runs without the GIL, so diagnostics are buffered and replayed
//! to Python's `logging` once detection is over.

use std::sync::Mutex;

use charset_norm::{Level, Logger};
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;

fn python_logger(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    static LOGGER: PyOnceLock<Py<PyAny>> = PyOnceLock::new();
    LOGGER
        .get_or_try_init(py, || {
            py.import("logging")?
                .getattr("getLogger")?
                .call1(("charset_norm",))
                .map(Bound::unbind)
        })
        .map(|logger| logger.bind(py).clone())
}

/// Collects diagnostics for the levels `logging.getLogger("charset_norm")`
/// had enabled when detection started.
pub(crate) struct BufferedLogger {
    trace: bool,
    debug: bool,
    records: Mutex<Vec<(Level, String)>>,
}

impl BufferedLogger {
    /// Snapshot which levels are enabled (requires the GIL).
    pub(crate) fn capture(py: Python<'_>) -> PyResult<Self> {
        let logger = python_logger(py)?;
        let enabled = |level: Level| -> PyResult<bool> {
            logger
                .call_method1("isEnabledFor", (level.python_level(),))?
                .is_truthy()
        };
        Ok(Self {
            trace: enabled(Level::Trace)?,
            debug: enabled(Level::Debug)?,
            records: Mutex::new(Vec::new()),
        })
    }

    /// Emit the buffered records to Python's `logging`, in order.
    pub(crate) fn replay(self, py: Python<'_>) -> PyResult<()> {
        let records = self
            .records
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if records.is_empty() {
            return Ok(());
        }
        let logger = python_logger(py)?;
        for (level, message) in records {
            logger.call_method1("log", (level.python_level(), message))?;
        }
        Ok(())
    }
}

impl Logger for BufferedLogger {
    fn enabled(&self, level: Level) -> bool {
        match level {
            Level::Trace => self.trace,
            Level::Debug => self.debug,
        }
    }

    fn log(&self, level: Level, message: &str) {
        if let Ok(mut records) = self.records.lock() {
            records.push((level, message.to_owned()));
        }
    }
}

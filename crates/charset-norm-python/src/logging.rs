//! Bridge from the core's [`Logger`] to Python's `charset_norm` logger.

use std::cell::RefCell;

use charset_norm::{Level, Logger};
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;

/// Forwards detection diagnostics to `logging.getLogger("charset_norm")`.
///
/// Python errors raised by logging calls are kept and surfaced by
/// [`PyLogger::finish`], since the core's logger interface is infallible.
pub(crate) struct PyLogger<'py> {
    logger: Bound<'py, PyAny>,
    error: RefCell<Option<PyErr>>,
}

impl<'py> PyLogger<'py> {
    pub(crate) fn new(py: Python<'py>) -> PyResult<Self> {
        static LOGGER: PyOnceLock<Py<PyAny>> = PyOnceLock::new();
        let logger = LOGGER.get_or_try_init(py, || {
            py.import("logging")?
                .getattr("getLogger")?
                .call1(("charset_norm",))
                .map(Bound::unbind)
        })?;
        Ok(Self {
            logger: logger.bind(py).clone(),
            error: RefCell::new(None),
        })
    }

    /// Report the first error a logging call raised, if any.
    pub(crate) fn finish(self) -> PyResult<()> {
        match self.error.into_inner() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn keep(&self, error: PyErr) {
        self.error.borrow_mut().get_or_insert(error);
    }
}

impl Logger for PyLogger<'_> {
    fn enabled(&self, level: Level) -> bool {
        if self.error.borrow().is_some() {
            return false;
        }
        match self
            .logger
            .call_method1("isEnabledFor", (level.python_level(),))
            .and_then(|value| value.is_truthy())
        {
            Ok(enabled) => enabled,
            Err(error) => {
                self.keep(error);
                false
            }
        }
    }

    fn log(&self, level: Level, message: &str) {
        if let Err(error) = self
            .logger
            .call_method1("log", (level.python_level(), message))
        {
            self.keep(error);
        }
    }
}

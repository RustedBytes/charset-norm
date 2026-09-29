//! Float helpers reproducing `CPython`'s results bit for bit.

use std::fmt;

use crate::stackfmt::StackStr;

/// `round(value, digits)` with `CPython`'s correctly-rounded semantics.
pub(crate) fn round(value: f64, digits: usize) -> f64 {
    const POWERS: [f64; 9] = [1.0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8];
    if !value.is_finite() {
        return value;
    }
    if digits < POWERS.len() {
        // Below 1e9 the scaled product is within 1.2e-7 of the exact value, so
        // away from a decimal tie it selects the same integer k as CPython's
        // correctly-rounded path; k / 10^d is then that decimal's nearest double.
        let scale = POWERS[digits];
        let scaled = value * scale;
        let fraction = scaled - scaled.floor();
        if scaled.abs() < 1e9 && (fraction - 0.5).abs() > 1e-6 {
            return scaled.round() / scale;
        }
    }
    // Correctly rounded decimal text, parsed back: CPython's own method.
    let mut text = StackStr::<352>::new();
    match fmt::write(&mut text, format_args!("{value:.digits$}")) {
        Ok(()) => text.as_str().parse().unwrap_or(value),
        Err(_) => format!("{value:.digits$}").parse().unwrap_or(value),
    }
}

/// Running `sum()` over floats as `CPython` 3.12+ computes it (Neumaier
/// compensation), fed one value at a time.
#[derive(Clone, Copy, Default)]
pub(crate) struct Sum {
    total: f64,
    compensation: f64,
}

impl Sum {
    pub(crate) fn add(&mut self, value: f64) {
        let next = self.total + value;
        if self.total.abs() >= value.abs() {
            self.compensation += (self.total - next) + value;
        } else {
            self.compensation += (value - next) + self.total;
        }
        self.total = next;
    }

    pub(crate) fn value(self) -> f64 {
        if self.compensation != 0.0 && self.compensation.is_finite() {
            self.total + self.compensation
        } else {
            self.total
        }
    }
}

/// `sum()` over floats as `CPython` 3.12+ computes it.
#[cfg(test)]
pub(crate) fn sum(values: &[f64]) -> f64 {
    let mut total = Sum::default();
    for &value in values {
        total.add(value);
    }
    total.value()
}

#[cfg(test)]
#[expect(clippy::float_cmp, reason = "results must match CPython exactly")]
mod tests {
    use super::*;

    #[test]
    fn matches_python() {
        assert_eq!(round(0.125, 2), 0.12);
        assert_eq!(round(2.675, 2), 2.67);
        assert_eq!(sum(&[0.1; 10]), 1.0);
    }
}

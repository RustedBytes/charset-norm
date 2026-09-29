//! Formatting into a fixed stack buffer, for short strings built in hot paths.

use std::fmt;

/// A string of at most `N` bytes formatted without heap allocation.
pub(crate) struct StackStr<const N: usize> {
    buffer: [u8; N],
    length: usize,
}

impl<const N: usize> StackStr<N> {
    pub(crate) fn new() -> Self {
        Self {
            buffer: [0; N],
            length: 0,
        }
    }

    /// Format `value`; `None` when it does not fit in `N` bytes.
    pub(crate) fn format(value: impl fmt::Display) -> Option<Self> {
        let mut out = Self::new();
        fmt::write(&mut out, format_args!("{value}")).ok()?;
        Some(out)
    }

    pub(crate) fn as_str(&self) -> &str {
        // Only whole `&str`s are ever appended, so this cannot fail.
        std::str::from_utf8(&self.buffer[..self.length]).unwrap_or_default()
    }
}

impl<const N: usize> fmt::Write for StackStr<N> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self.length + text.len();
        if end > N {
            return Err(fmt::Error);
        }
        self.buffer[self.length..end].copy_from_slice(text.as_bytes());
        self.length = end;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::StackStr;

    #[test]
    fn formats_or_reports_overflow() {
        assert_eq!(StackStr::<16>::format(12.5).unwrap().as_str(), "12.5");
        assert!(StackStr::<2>::format("too long").is_none());
    }
}

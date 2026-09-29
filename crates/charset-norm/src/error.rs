use std::fmt;

/// Errors reported by the lookup helpers of this crate.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The name does not resolve to a supported code page.
    UnknownEncoding(String),
    /// The language has no frequency profile.
    UnknownLanguage(String),
    /// The operation only applies to single-byte code pages.
    MultiByteEncoding(String),
    /// The name is not a known Unicode range.
    UnknownRange(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::UnknownEncoding(name) => write!(f, "Unable to retrieve IANA for '{name}'"),
            Error::UnknownLanguage(name) => write!(f, "{name} not available"),
            Error::MultiByteEncoding(_) => {
                f.write_str("Function not supported on multi-byte code page")
            }
            Error::UnknownRange(name) => write!(f, "unknown Unicode range '{name}'"),
        }
    }
}

impl std::error::Error for Error {}

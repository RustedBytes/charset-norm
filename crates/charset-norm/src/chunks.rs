//! Sampling of decoded chunks, the unit the detector measures.
//!
//! This is a low-level building block of [`crate::from_bytes`]; most users
//! do not need it.

use std::borrow::Cow;

use crate::codecs::{self, DecodeError, Errors};

/// How the payload's signature (BOM) is treated when cutting chunks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signature<'a> {
    /// No signature handling.
    None,
    /// The signature belongs to the text: it is prepended to every byte
    /// chunk so the decoder sees it.
    Kept(&'a [u8]),
    /// A signature of this many bytes is dropped: strictly decoded chunks
    /// are cut after it.
    Stripped(usize),
}

impl<'a> Signature<'a> {
    /// Build from the flags of the reference implementation.
    #[must_use]
    pub fn from_flags(sig_available: bool, strip_sig: bool, sig_payload: &'a [u8]) -> Self {
        if strip_sig {
            Signature::Stripped(sig_payload.len())
        } else if sig_available {
            Signature::Kept(sig_payload)
        } else {
            Signature::None
        }
    }
}

/// Where chunks come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkSource<'a> {
    /// Slices of the fully decoded text, byte offsets mapped proportionally
    /// onto characters (stateful ISO-2022 codecs).
    ScaledText(&'a str),
    /// Slices of the fully decoded text, offsets taken as character positions.
    Text(&'a str),
    /// Each byte chunk decoded strictly.
    Deferred,
    /// Byte chunks decoded leniently (multi-byte decoders) or strictly, and
    /// realigned against the full decoded text when it is given.
    Bytes {
        /// Skip invalid sequences instead of failing.
        lenient: bool,
        /// Full decoded text, used to realign chunks cut mid-character.
        decoded: Option<&'a str>,
    },
}

impl<'a> ChunkSource<'a> {
    /// Select the source the reference implementation uses for `encoding`.
    #[must_use]
    pub fn select(
        encoding: &str,
        decoded: Option<&'a str>,
        multi_byte_decoder: bool,
        deferred_decoding: bool,
    ) -> Self {
        match decoded {
            Some(text) if encoding.starts_with("iso2022_") => ChunkSource::ScaledText(text),
            Some(text) if !multi_byte_decoder => ChunkSource::Text(text),
            _ if deferred_decoding => ChunkSource::Deferred,
            _ => ChunkSource::Bytes {
                lenient: multi_byte_decoder,
                decoded,
            },
        }
    }
}

fn char_slice(value: &str, start: usize, count: usize) -> String {
    value.chars().skip(start).take(count).collect()
}

/// `prefix in decoded`, probing first around where a chunk starting at byte
/// `offset` of the payload is expected to land (any hit there is a hit).
fn contains_near(decoded: &str, prefix: &str, offset: usize, payload_len: usize) -> bool {
    const RADIUS: usize = 32768 * 4;
    let expected = offset.saturating_mul(decoded.len()) / payload_len.max(1);
    let mut start = expected.saturating_sub(RADIUS).min(decoded.len());
    let mut end = expected
        .saturating_add(RADIUS + prefix.len())
        .min(decoded.len());
    while !decoded.is_char_boundary(start) {
        start -= 1;
    }
    while !decoded.is_char_boundary(end) {
        end += 1;
    }
    decoded[start..end].contains(prefix) || decoded.contains(prefix)
}

/// Chunk sampler for one candidate encoding. Chunks are produced lazily so
/// the detector can stop decoding as soon as it has seen enough of them.
pub struct ChunkCutter<'a> {
    sequences: &'a [u8],
    encoding: &'a str,
    offsets: std::vec::IntoIter<usize>,
    chunk_size: usize,
    signature: Signature<'a>,
    source: ChunkSource<'a>,
    decoded_len: Option<usize>,
    done: bool,
}

impl<'a> ChunkCutter<'a> {
    /// Sample chunks of `chunk_size` from `sequences`, decoded as
    /// `encoding`, starting at each of `offsets`.
    #[must_use]
    pub fn new(
        sequences: &'a [u8],
        encoding: &'a str,
        offsets: Vec<usize>,
        chunk_size: usize,
        signature: Signature<'a>,
        source: ChunkSource<'a>,
    ) -> Self {
        Self {
            sequences,
            encoding,
            offsets: offsets.into_iter(),
            chunk_size,
            signature,
            source,
            decoded_len: None,
            done: false,
        }
    }

    /// Bytes strictly decoded chunks are cut from (after a stripped signature).
    fn deferred_base(&self) -> &'a [u8] {
        match self.signature {
            Signature::Stripped(length) => &self.sequences[length..],
            _ => self.sequences,
        }
    }

    fn deferred_cut(&self, offset: usize) -> &'a [u8] {
        let base = self.deferred_base();
        &base[offset.min(base.len())..(offset + self.chunk_size).min(base.len())]
    }

    /// `sequences[start:end]`, with the signature prepended when it is kept.
    fn cut(&self, start: usize, end: usize) -> Cow<'a, [u8]> {
        let cut = &self.sequences[start.min(end)..end];
        match self.signature {
            Signature::Kept(signature) => {
                let mut prefixed = signature.to_vec();
                prefixed.extend_from_slice(cut);
                Cow::Owned(prefixed)
            }
            _ => Cow::Borrowed(cut),
        }
    }

    /// End of the byte chunk starting at `offset`, or `None` when that
    /// chunk would overrun the payload and is skipped.
    fn chunk_end(&self, offset: usize) -> Option<usize> {
        let end = offset + self.chunk_size;
        (end <= self.sequences.len() + 8).then(|| end.min(self.sequences.len()))
    }

    /// Check up front that every chunk a full pass would strictly decode is
    /// valid, so stopping early never hides a decoding failure.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Invalid`] when a chunk does not decode, or
    /// [`DecodeError::Unknown`] for an unsupported encoding.
    pub fn validate(&self) -> Result<(), DecodeError> {
        let offsets = self.offsets.as_slice();
        match self.source {
            // Decoded text and lenient decoding cannot fail.
            ChunkSource::ScaledText(_)
            | ChunkSource::Text(_)
            | ChunkSource::Bytes { lenient: true, .. } => {}
            ChunkSource::Deferred => {
                for &offset in offsets {
                    let cut = self.deferred_cut(offset);
                    if cut.is_empty() {
                        break;
                    }
                    if !codecs::is_valid(cut, self.encoding)? {
                        return Err(DecodeError::Invalid);
                    }
                }
            }
            ChunkSource::Bytes { lenient: false, .. } => {
                for &offset in offsets {
                    if let Some(end) = self.chunk_end(offset)
                        && !codecs::is_valid(&self.cut(offset, end), self.encoding)?
                    {
                        return Err(DecodeError::Invalid);
                    }
                }
            }
        }
        Ok(())
    }

    fn next_chunk(&mut self) -> Option<Result<String, DecodeError>> {
        loop {
            let offset = self.offsets.next()?;
            let (lenient, decoded) = match self.source {
                ChunkSource::ScaledText(text) => {
                    let decoded_len = *self.decoded_len.get_or_insert_with(|| text.chars().count());
                    let chunk = char_slice(
                        text,
                        offset * decoded_len / self.sequences.len(),
                        self.chunk_size,
                    );
                    return (!chunk.is_empty()).then_some(Ok(chunk));
                }
                ChunkSource::Text(text) => {
                    let chunk = char_slice(text, offset, self.chunk_size);
                    return (!chunk.is_empty()).then_some(Ok(chunk));
                }
                ChunkSource::Deferred => {
                    let cut = self.deferred_cut(offset);
                    return (!cut.is_empty())
                        .then(|| codecs::decode(cut, self.encoding, Errors::Strict));
                }
                ChunkSource::Bytes { lenient, decoded } => (lenient, decoded),
            };
            let Some(end) = self.chunk_end(offset) else {
                continue;
            };
            let errors = if lenient {
                Errors::Ignore
            } else {
                Errors::Strict
            };
            let chunk = codecs::decode(&self.cut(offset, end), self.encoding, errors);
            return Some(match (chunk, decoded) {
                (Ok(chunk), Some(decoded)) if lenient && offset > 0 => {
                    self.realign(chunk, decoded, offset, end)
                }
                (chunk, _) => chunk,
            });
        }
    }

    /// When a lenient chunk was cut mid-character, back up (at most three
    /// bytes, wrapping like a negative Python index) until its beginning
    /// appears in the full text.
    fn realign(
        &self,
        mut chunk: String,
        decoded: &str,
        offset: usize,
        end: usize,
    ) -> Result<String, DecodeError> {
        let head =
            |chunk: &str| -> String { chunk.chars().take(self.chunk_size.min(16)).collect() };
        if contains_near(decoded, &head(&chunk), offset, self.sequences.len()) {
            return Ok(chunk);
        }
        for delta in 0..4usize {
            let start = if delta <= offset {
                offset - delta
            } else {
                self.sequences.len().saturating_sub(delta - offset)
            };
            chunk = codecs::decode(&self.cut(start, end), self.encoding, Errors::Ignore)?;
            if decoded.contains(&head(&chunk)) {
                break;
            }
        }
        Ok(chunk)
    }
}

impl Iterator for ChunkCutter<'_> {
    type Item = Result<String, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let item = self.next_chunk();
        if item.is_none() {
            self.done = true;
        }
        item
    }
}

/// Decode every sample chunk eagerly; see [`ChunkCutter::new`].
///
/// # Errors
///
/// The first [`DecodeError`] raised while decoding a chunk.
pub fn cut_sequence_chunks(
    sequences: &[u8],
    encoding: &str,
    offsets: Vec<usize>,
    chunk_size: usize,
    signature: Signature<'_>,
    source: ChunkSource<'_>,
) -> Result<Vec<String>, DecodeError> {
    ChunkCutter::new(sequences, encoding, offsets, chunk_size, signature, source).collect()
}

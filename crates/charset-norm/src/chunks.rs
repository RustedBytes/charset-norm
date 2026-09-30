//! Sampling of decoded chunks, the unit the detector measures.
//!
//! This is a low-level building block of [`crate::from_bytes`]; most users
//! do not need it. It is public for tools that want to reproduce the
//! detector's measurements on a given encoding.
//!
//! ```
//! use charset_norm::chunks::{ChunkSource, Signature, cut_sequence_chunks};
//!
//! let payload = "abcdefghij".repeat(10);
//! let chunks = cut_sequence_chunks(
//!     payload.as_bytes(),
//!     "ascii",
//!     (0..payload.len()).step_by(25).collect(),
//!     25,
//!     Signature::None,
//!     ChunkSource::Deferred,
//! )
//! .unwrap();
//! assert_eq!(chunks.len(), 4);
//! assert!(chunks.iter().all(|chunk| chunk.len() == 25));
//! ```

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

/// First `count` characters of `value` from character `start`, into `out`.
fn char_slice_into(value: &str, start: usize, count: usize, out: &mut String) {
    out.clear();
    out.extend(value.chars().skip(start).take(count));
}

/// The first `count` characters of `text`, borrowed.
fn head(text: &str, count: usize) -> &str {
    text.char_indices()
        .nth(count)
        .map_or(text, |(offset, _)| &text[..offset])
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
pub struct ChunkCutter<'a, I = std::vec::IntoIter<usize>> {
    sequences: &'a [u8],
    encoding: &'a str,
    offsets: I,
    chunk_size: usize,
    signature: Signature<'a>,
    source: ChunkSource<'a>,
    decoded_len: Option<usize>,
    /// Scratch for chunks prefixed with a kept signature.
    prefixed: Vec<u8>,
    done: bool,
}

impl<'a, I: Iterator<Item = usize> + Clone> ChunkCutter<'a, I> {
    /// Sample chunks of `chunk_size` from `sequences`, decoded as
    /// `encoding`, starting at each of `offsets`.
    #[must_use]
    pub fn new(
        sequences: &'a [u8],
        encoding: &'a str,
        offsets: impl IntoIterator<IntoIter = I>,
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
            prefixed: Vec::new(),
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

    /// Decode `sequences[start:end]` (prefixed with a kept signature).
    fn decode_cut(
        &mut self,
        start: usize,
        end: usize,
        errors: Errors,
        out: &mut String,
    ) -> Result<(), DecodeError> {
        let cut = &self.sequences[start.min(end)..end];
        if let Signature::Kept(signature) = self.signature {
            self.prefixed.clear();
            self.prefixed.extend_from_slice(signature);
            self.prefixed.extend_from_slice(cut);
            codecs::decode_into(&self.prefixed, self.encoding, errors, out)
        } else {
            codecs::decode_into(cut, self.encoding, errors, out)
        }
    }

    /// Whether `sequences[start:end]` (with a kept signature) decodes strictly.
    fn valid_cut(&self, start: usize, end: usize) -> Result<bool, DecodeError> {
        let cut = &self.sequences[start.min(end)..end];
        match self.signature {
            Signature::Kept(signature) => {
                let mut prefixed = Vec::with_capacity(signature.len() + cut.len());
                prefixed.extend_from_slice(signature);
                prefixed.extend_from_slice(cut);
                codecs::is_valid(&prefixed, self.encoding)
            }
            _ => codecs::is_valid(cut, self.encoding),
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
        match self.source {
            // Decoded text and lenient decoding cannot fail.
            ChunkSource::ScaledText(_)
            | ChunkSource::Text(_)
            | ChunkSource::Bytes { lenient: true, .. } => {}
            ChunkSource::Deferred => {
                for offset in self.offsets.clone() {
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
                for offset in self.offsets.clone() {
                    if let Some(end) = self.chunk_end(offset)
                        && !self.valid_cut(offset, end)?
                    {
                        return Err(DecodeError::Invalid);
                    }
                }
            }
        }
        Ok(())
    }

    /// Decode the next chunk into `out` (replacing its contents).
    /// `None` once every chunk has been produced.
    pub fn next_into(&mut self, out: &mut String) -> Option<Result<(), DecodeError>> {
        if self.done {
            return None;
        }
        let item = self.next_chunk(out);
        if item.is_none() {
            self.done = true;
        }
        item
    }

    fn next_chunk(&mut self, out: &mut String) -> Option<Result<(), DecodeError>> {
        loop {
            let offset = self.offsets.next()?;
            let (lenient, decoded) = match self.source {
                ChunkSource::ScaledText(text) => {
                    let decoded_len = *self.decoded_len.get_or_insert_with(|| text.chars().count());
                    char_slice_into(
                        text,
                        offset * decoded_len / self.sequences.len(),
                        self.chunk_size,
                        out,
                    );
                    return (!out.is_empty()).then_some(Ok(()));
                }
                ChunkSource::Text(text) => {
                    char_slice_into(text, offset, self.chunk_size, out);
                    return (!out.is_empty()).then_some(Ok(()));
                }
                ChunkSource::Deferred => {
                    let cut = self.deferred_cut(offset);
                    return (!cut.is_empty())
                        .then(|| codecs::decode_into(cut, self.encoding, Errors::Strict, out));
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
            let result = self.decode_cut(offset, end, errors, out);
            return Some(match (result, decoded) {
                (Ok(()), Some(decoded)) if lenient && offset > 0 => {
                    self.realign(out, decoded, offset, end)
                }
                (result, _) => result,
            });
        }
    }

    /// When a lenient chunk was cut mid-character, back up (at most three
    /// bytes, wrapping like a negative Python index) until its beginning
    /// appears in the full text.
    fn realign(
        &mut self,
        chunk: &mut String,
        decoded: &str,
        offset: usize,
        end: usize,
    ) -> Result<(), DecodeError> {
        let prefix_chars = self.chunk_size.min(16);
        if contains_near(
            decoded,
            head(chunk, prefix_chars),
            offset,
            self.sequences.len(),
        ) {
            return Ok(());
        }
        for delta in 0..4usize {
            let start = if delta <= offset {
                offset - delta
            } else {
                self.sequences.len().saturating_sub(delta - offset)
            };
            self.decode_cut(start, end, Errors::Ignore, chunk)?;
            if decoded.contains(head(chunk, prefix_chars)) {
                break;
            }
        }
        Ok(())
    }
}

impl<I: Iterator<Item = usize> + Clone> Iterator for ChunkCutter<'_, I> {
    type Item = Result<String, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut chunk = String::new();
        self.next_into(&mut chunk)
            .map(|result| result.map(|()| chunk))
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

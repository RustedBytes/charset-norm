//! Sampling of decoded chunks, the unit the detector measures.
//!
//! This is a low-level building block of [`crate::from_bytes`]; most users
//! do not need it.

use crate::codecs::{self, DecodeError, Errors};

fn char_slice(value: &str, start: usize, count: usize) -> String {
    value.chars().skip(start).take(count).collect()
}

/// `prefix in decoded`, probing first around where a chunk starting at byte
/// `offset` of the payload is expected to land (any hit there is a hit).
fn contains_near(decoded: &str, prefix: &str, offset: usize, payload_len: usize) -> bool {
    const RADIUS: usize = 32768 * 4;
    let expected = (offset as u128 * decoded.len() as u128 / payload_len.max(1) as u128) as usize;
    let mut start = expected.saturating_sub(RADIUS).min(decoded.len());
    let mut end = (expected + RADIUS + prefix.len()).min(decoded.len());
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
    bom_or_sig_available: bool,
    strip_sig_or_bom: bool,
    sig_payload: &'a [u8],
    is_multi_byte_decoder: bool,
    decoded_payload: Option<&'a str>,
    deferred_decoding: bool,
    decoded_len: Option<usize>,
    done: bool,
}

impl<'a> ChunkCutter<'a> {
    /// Sample `sequences` decoded as `encoding_iana` at `offsets`.
    ///
    /// When `decoded_payload` holds the full decoded text, chunks are taken
    /// from it; with `deferred_decoding` each byte chunk is decoded strictly;
    /// otherwise chunks are decoded from bytes (leniently for multi-byte
    /// decoders, realigned against `decoded_payload` when given).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sequences: &'a [u8],
        encoding: &'a str,
        offsets: Vec<usize>,
        chunk_size: usize,
        bom_or_sig_available: bool,
        strip_sig_or_bom: bool,
        sig_payload: &'a [u8],
        is_multi_byte_decoder: bool,
        decoded_payload: Option<&'a str>,
        deferred_decoding: bool,
    ) -> Self {
        Self {
            sequences,
            encoding,
            offsets: offsets.into_iter(),
            chunk_size,
            bom_or_sig_available,
            strip_sig_or_bom,
            sig_payload,
            is_multi_byte_decoder,
            decoded_payload,
            deferred_decoding,
            decoded_len: None,
            done: false,
        }
    }

    fn iso2022_decoded(&self) -> Option<&'a str> {
        self.decoded_payload
            .filter(|_| self.encoding.starts_with("iso2022_"))
    }

    fn single_byte_decoded(&self) -> Option<&'a str> {
        self.decoded_payload.filter(|_| !self.is_multi_byte_decoder)
    }

    fn deferred_base(&self) -> &'a [u8] {
        if self.strip_sig_or_bom {
            &self.sequences[self.sig_payload.len()..]
        } else {
            self.sequences
        }
    }

    /// `sequences[start:end]`, with the signature prepended when it is kept.
    fn cut(&self, start: usize, end: usize) -> std::borrow::Cow<'a, [u8]> {
        let cut = &self.sequences[start.min(end)..end];
        if self.bom_or_sig_available && !self.strip_sig_or_bom {
            let mut prefixed = self.sig_payload.to_vec();
            prefixed.extend_from_slice(cut);
            std::borrow::Cow::Owned(prefixed)
        } else {
            std::borrow::Cow::Borrowed(cut)
        }
    }

    /// Check up front that every chunk a full pass would strictly decode is
    /// valid, so stopping early never hides a decoding failure.
    pub fn validate(&self) -> Result<(), DecodeError> {
        if self.iso2022_decoded().is_some() || self.single_byte_decoded().is_some() {
            return Ok(());
        }
        let offsets = self.offsets.as_slice();
        if self.deferred_decoding {
            let base = self.deferred_base();
            for &offset in offsets {
                let cut = &base[offset.min(base.len())..(offset + self.chunk_size).min(base.len())];
                if cut.is_empty() {
                    break;
                }
                if !codecs::is_valid(cut, self.encoding)? {
                    return Err(DecodeError::Invalid);
                }
            }
        } else if !self.is_multi_byte_decoder {
            for &offset in offsets {
                let chunk_end = offset + self.chunk_size;
                if chunk_end > self.sequences.len() + 8 {
                    continue;
                }
                let end = chunk_end.min(self.sequences.len());
                if !codecs::is_valid(&self.cut(offset, end), self.encoding)? {
                    return Err(DecodeError::Invalid);
                }
            }
        }
        Ok(())
    }

    fn next_chunk(&mut self) -> Option<Result<String, DecodeError>> {
        loop {
            let offset = self.offsets.next()?;
            if let Some(decoded) = self.iso2022_decoded() {
                let decoded_len = *self
                    .decoded_len
                    .get_or_insert_with(|| decoded.chars().count());
                let decoded_offset = offset * decoded_len / self.sequences.len();
                let chunk = char_slice(decoded, decoded_offset, self.chunk_size);
                return (!chunk.is_empty()).then_some(Ok(chunk));
            }
            if let Some(decoded) = self.single_byte_decoded() {
                let chunk = char_slice(decoded, offset, self.chunk_size);
                return (!chunk.is_empty()).then_some(Ok(chunk));
            }
            if self.deferred_decoding {
                let base = self.deferred_base();
                let cut = &base[offset.min(base.len())..(offset + self.chunk_size).min(base.len())];
                if cut.is_empty() {
                    return None;
                }
                return Some(codecs::decode(cut, self.encoding, Errors::Strict));
            }
            let errors = if self.is_multi_byte_decoder {
                Errors::Ignore
            } else {
                Errors::Strict
            };
            let chunk_end = offset + self.chunk_size;
            if chunk_end > self.sequences.len() + 8 {
                continue;
            }
            let end = chunk_end.min(self.sequences.len());
            let mut chunk = match codecs::decode(&self.cut(offset, end), self.encoding, errors) {
                Ok(chunk) => chunk,
                Err(error) => return Some(Err(error)),
            };
            if self.is_multi_byte_decoder && offset > 0 {
                if let Some(decoded) = self.decoded_payload {
                    let prefix: String = chunk.chars().take(self.chunk_size.min(16)).collect();
                    if !contains_near(decoded, &prefix, offset, self.sequences.len()) {
                        for delta in 0..4usize {
                            let signed_start = offset as isize - delta as isize;
                            let adjusted_start = if signed_start < 0 {
                                self.sequences
                                    .len()
                                    .saturating_sub((-signed_start) as usize)
                            } else {
                                signed_start as usize
                            };
                            chunk = match codecs::decode(
                                &self.cut(adjusted_start, end),
                                self.encoding,
                                Errors::Ignore,
                            ) {
                                Ok(chunk) => chunk,
                                Err(error) => return Some(Err(error)),
                            };
                            let adjusted_prefix: String =
                                chunk.chars().take(self.chunk_size.min(16)).collect();
                            if decoded.contains(&adjusted_prefix) {
                                break;
                            }
                        }
                    }
                }
            }
            return Some(Ok(chunk));
        }
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

/// Decode every sample chunk eagerly; see [`ChunkCutter::new`] for the
/// meaning of the arguments.
#[allow(clippy::too_many_arguments)]
pub fn cut_sequence_chunks(
    sequences: &[u8],
    encoding_iana: &str,
    offsets: Vec<usize>,
    chunk_size: usize,
    bom_or_sig_available: bool,
    strip_sig_or_bom: bool,
    sig_payload: &[u8],
    is_multi_byte_decoder: bool,
    decoded_payload: Option<&str>,
    deferred_decoding: bool,
) -> Result<Vec<String>, DecodeError> {
    ChunkCutter::new(
        sequences,
        encoding_iana,
        offsets,
        chunk_size,
        bom_or_sig_available,
        strip_sig_or_bom,
        sig_payload,
        is_multi_byte_decoder,
        decoded_payload,
        deferred_decoding,
    )
    .collect()
}

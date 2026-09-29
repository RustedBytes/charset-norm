//! Stateless CJK codecs driven by tables probed from `CPython`.

use std::collections::HashMap;
use std::sync::OnceLock;

use super::single_byte::{ascii_prefix, push_ascii};
use super::{DecodeError, Errors};

/* ---------------------------------------------------------------------- */
/* Table-driven CJK codecs                                                 */
/* ---------------------------------------------------------------------- */

pub(super) const CJK_NAMES: [&str; 16] = [
    "big5",
    "big5hkscs",
    "cp932",
    "cp949",
    "cp950",
    "euc_jis_2004",
    "euc_jisx0213",
    "euc_jp",
    "euc_kr",
    "gb18030",
    "gb2312",
    "gbk",
    "johab",
    "shift_jis",
    "shift_jis_2004",
    "shift_jisx0213",
];

const NONE: u32 = 0xFF_FFFF;
const PAIR_BASE: u32 = 0x11_0000;
/// Marks an invalid sequence spanning `value - ERROR_BASE` bytes.
const ERROR_BASE: u32 = 0xFF_FFF0;

struct Row {
    first: u8,
    values: Vec<u32>,
}

/// Rows keyed by their first byte, each spanning a contiguous trail range.
pub(super) struct Rows {
    rows: Vec<Option<Row>>,
}

impl Rows {
    pub(super) fn get(&self, first: u8, second: u8) -> Option<u32> {
        let row = self.rows[first as usize].as_ref()?;
        let offset = second.checked_sub(row.first)? as usize;
        row.values
            .get(offset)
            .copied()
            .filter(|value| *value != NONE)
    }
}

pub(super) struct CjkCodec {
    /// Bytes below 0x80 decode to themselves.
    ascii_identity: bool,
    need: [u8; 256],
    single: [u32; 256],
    double: Rows,
    triple_prefix: u8,
    triple: Rows,
}

pub(super) struct CjkData {
    codecs: HashMap<&'static str, CjkCodec>,
    pub(super) iso2022: HashMap<&'static str, Rows>,
    gb18030_ranges: Vec<(u32, u32)>,
    pub(super) pairs: Vec<(char, char)>,
}

impl CjkData {
    pub(super) fn codec(&self, name: &str) -> &CjkCodec {
        &self.codecs[name]
    }
}

static CJK_BLOB: &[u8] = include_bytes!("../generated/cjk.bin");

struct Reader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> &'a [u8] {
        let slice = &self.data[self.position..self.position + length];
        self.position += length;
        slice
    }
    fn u8(&mut self) -> u8 {
        self.take(1)[0]
    }
    fn u16(&mut self) -> u16 {
        let bytes = self.take(2);
        u16::from_le_bytes([bytes[0], bytes[1]])
    }
    fn u24(&mut self) -> u32 {
        let bytes = self.take(3);
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0])
    }
    fn u32(&mut self) -> u32 {
        let bytes = self.take(4);
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    }
    fn rows(&mut self) -> Rows {
        let mut rows: Vec<Option<Row>> = (0..256).map(|_| None).collect();
        for _ in 0..self.u16() {
            let lead = self.u8();
            let first = self.u8();
            let count = self.u16() as usize;
            let values = (0..count).map(|_| self.u24()).collect();
            rows[lead as usize] = Some(Row { first, values });
        }
        Rows { rows }
    }
}

pub(super) fn cjk() -> &'static CjkData {
    static DATA: OnceLock<CjkData> = OnceLock::new();
    DATA.get_or_init(|| {
        let mut reader = Reader {
            data: CJK_BLOB,
            position: 0,
        };
        assert_eq!(reader.take(6), b"CNCJK1", "corrupted CJK tables");
        let mut data = CjkData {
            codecs: HashMap::new(),
            iso2022: HashMap::new(),
            gb18030_ranges: Vec::new(),
            pairs: Vec::new(),
        };
        for _ in 0..reader.u16() {
            let length = reader.u8() as usize;
            let name = std::str::from_utf8(reader.take(length)).unwrap_or_default();
            let size = reader.u32() as usize;
            let mut section = Reader {
                data: reader.take(size),
                position: 0,
            };
            if let Some(&codec) = CJK_NAMES.iter().find(|value| **value == name) {
                let mut need = [0u8; 256];
                need.copy_from_slice(section.take(256));
                let mut single = [NONE; 256];
                for value in &mut single {
                    *value = section.u24();
                }
                let double = section.rows();
                let triple_prefix = section.u8();
                let triple = section.rows();
                let ascii_identity = (0u8..0x80).all(|byte| {
                    need[usize::from(byte)] == 1 && single[usize::from(byte)] == u32::from(byte)
                });
                data.codecs.insert(
                    codec,
                    CjkCodec {
                        ascii_identity,
                        need,
                        single,
                        double,
                        triple_prefix,
                        triple,
                    },
                );
            } else if name == "gb18030_ranges" {
                for _ in 0..section.u16() {
                    let index = section.u32();
                    let codepoint = section.u32();
                    data.gb18030_ranges.push((index, codepoint));
                }
            } else if name == "pairs" {
                for _ in 0..section.u16() {
                    let first = char::from_u32(section.u32()).unwrap_or('\u{fffd}');
                    let second = char::from_u32(section.u32()).unwrap_or('\u{fffd}');
                    data.pairs.push((first, second));
                }
            } else {
                let name: &'static str = match name {
                    "jisx0208" => "jisx0208",
                    "jisx0212" => "jisx0212",
                    "ksx1001" => "ksx1001",
                    "gb2312_7bit" => "gb2312_7bit",
                    "jisx0201_r" => "jisx0201_r",
                    "jisx0201_k" => "jisx0201_k",
                    "jisx0213_2000_1" => "jisx0213_2000_1",
                    "jisx0213_2000_2" => "jisx0213_2000_2",
                    "jisx0213_2004_1" => "jisx0213_2004_1",
                    "jisx0213_2004_2" => "jisx0213_2004_2",
                    _ => continue,
                };
                data.iso2022.insert(name, section.rows());
            }
        }
        data
    })
}

#[inline]
pub(super) fn push_value(
    out: &mut String,
    value: u32,
    pairs: &[(char, char)],
) -> Result<(), DecodeError> {
    if value >= PAIR_BASE {
        let (first, second) = pairs
            .get((value - PAIR_BASE) as usize)
            .ok_or(DecodeError::Invalid)?;
        out.push(*first);
        out.push(*second);
    } else {
        out.push(char::from_u32(value).ok_or(DecodeError::Invalid)?);
    }
    Ok(())
}

const CGK2U_CHOSEONG: [u8; 30] = [
    0, 1, 127, 2, 127, 127, 3, 4, 5, 127, 127, 127, 127, 127, 127, 127, 6, 7, 8, 127, 9, 10, 11,
    12, 13, 14, 15, 16, 17, 18,
];
const CGK2U_JONGSEONG: [u8; 30] = [
    1, 2, 3, 4, 5, 6, 7, 127, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 127, 18, 19, 20, 21, 22, 127,
    23, 24, 25, 26, 27,
];

/// KS X 1001:1998 Annex 3 make-up sequence (`A4 D4 A4 xx A4 xx A4 xx`).
fn euc_kr_makeup(data: &[u8]) -> Option<char> {
    if data[2] != 0xA4 || data[4] != 0xA4 || data[6] != 0xA4 {
        return None;
    }
    // Leading consonant, vowel and trailing consonant of the syllable.
    let lead = match data[3] {
        value @ 0xA1..=0xBE => CGK2U_CHOSEONG[usize::from(value - 0xA1)],
        _ => 127,
    };
    let vowel = match data[5] {
        value @ 0xBF..=0xD3 => value - 0xBF,
        _ => 127,
    };
    let tail = match data[7] {
        0xD4 => 0,
        value @ 0xA1..=0xBE => CGK2U_JONGSEONG[usize::from(value - 0xA1)],
        _ => 127,
    };
    if lead == 127 || vowel == 127 || tail == 127 {
        return None;
    }
    char::from_u32(0xAC00 + u32::from(lead) * 588 + u32::from(vowel) * 28 + u32::from(tail))
}

#[derive(Clone, Copy)]
pub(super) enum Step {
    /// Consumed this many bytes.
    Ok(usize),
    /// Invalid sequence of this many bytes.
    Error(usize),
    /// Not enough input left for the sequence.
    Incomplete,
}

/// Move past a decoding step. Returns `Ok(false)` when decoding should stop
/// (incomplete trailing sequence under `errors="ignore"`).
pub(super) fn advance(
    step: Step,
    errors: Errors,
    position: &mut usize,
) -> Result<bool, DecodeError> {
    match step {
        Step::Error(_) | Step::Incomplete if errors == Errors::Strict => Err(DecodeError::Invalid),
        Step::Ok(length) | Step::Error(length) => {
            *position += length;
            Ok(true)
        }
        Step::Incomplete => Ok(false),
    }
}

fn gb18030_four_byte(data: &[u8], out: &mut String, ranges: &[(u32, u32)]) -> Step {
    if data.len() < 4 {
        return Step::Incomplete;
    }
    let (c1, c2, c3, c4) = (data[0], data[1], data[2], data[3]);
    if !(0x81..=0xFE).contains(&c1) || !(0x81..=0xFE).contains(&c3) || !(0x30..=0x39).contains(&c4)
    {
        return Step::Error(1);
    }
    let (c1, c2, c3, c4) = (
        u32::from(c1 - 0x81),
        u32::from(c2 - 0x30),
        u32::from(c3 - 0x81),
        u32::from(c4 - 0x30),
    );
    if c1 < 4 {
        let index = (c1 * 10 + c2) * 1260 + c3 * 10 + c4;
        if index < 39420 {
            let run = ranges.partition_point(|entry| entry.0 <= index) - 1;
            let (base, first) = ranges[run];
            if let Some(character) = char::from_u32(first + index - base) {
                out.push(character);
                return Step::Ok(4);
            }
        }
    } else if c1 >= 15 {
        let value = 0x10000 + ((c1 - 15) * 10 + c2) * 1260 + c3 * 10 + c4;
        if let Some(character) = char::from_u32(value) {
            out.push(character);
            return Step::Ok(4);
        }
    }
    Step::Error(1)
}

fn table_step(
    value: Option<u32>,
    width: usize,
    out: &mut String,
    pairs: &[(char, char)],
) -> Result<Step, DecodeError> {
    Ok(match value {
        Some(value) if value >= ERROR_BASE => Step::Error((value - ERROR_BASE) as usize),
        Some(value) => {
            push_value(out, value, pairs)?;
            Step::Ok(width)
        }
        None => Step::Error(1),
    })
}

pub(super) fn decode_cjk(
    codec: &CjkCodec,
    data: &[u8],
    errors: Errors,
) -> Result<String, DecodeError> {
    let tables = cjk();
    let is_euc_kr = std::ptr::eq(codec, tables.codec("euc_kr"));
    let is_gb18030 = std::ptr::eq(codec, tables.codec("gb18030"));
    let mut out = String::with_capacity(data.len() * 2);
    let mut position = 0usize;
    while position < data.len() {
        if codec.ascii_identity {
            let run = ascii_prefix(&data[position..]);
            push_ascii(&mut out, &data[position..position + run]);
            position += run;
            if position == data.len() {
                break;
            }
        }
        let rest = &data[position..];
        let lead = rest[0];
        let step = match codec.need[lead as usize] {
            1 => {
                push_value(&mut out, codec.single[lead as usize], &tables.pairs)?;
                Step::Ok(1)
            }
            2 if rest.len() < 2 => Step::Incomplete,
            2 if is_euc_kr && lead == 0xA4 && rest[1] == 0xD4 => {
                if rest.len() < 8 {
                    Step::Incomplete
                } else if let Some(character) = euc_kr_makeup(rest) {
                    out.push(character);
                    Step::Ok(8)
                } else {
                    Step::Error(1)
                }
            }
            2 if is_gb18030 && (0x30..=0x39).contains(&rest[1]) => {
                gb18030_four_byte(rest, &mut out, &tables.gb18030_ranges)
            }
            2 => table_step(codec.double.get(lead, rest[1]), 2, &mut out, &tables.pairs)?,
            3 if rest.len() < 3 => Step::Incomplete,
            3 if lead == codec.triple_prefix => table_step(
                codec.triple.get(rest[1], rest[2]),
                3,
                &mut out,
                &tables.pairs,
            )?,
            _ => Step::Error(1),
        };
        if !advance(step, errors, &mut position)? {
            break;
        }
    }
    Ok(out)
}

//! Stateful HZ and ISO-2022 decoders (ports of `CPython`'s).

use super::cjk::{Step, advance, cjk, push_value};
use super::{DecodeError, Errors, Iso2022Variant};

/* ---------------------------------------------------------------------- */
/* HZ and ISO-2022 (ports of CPython's stateful decoders)                  */
/* ---------------------------------------------------------------------- */

pub(super) fn decode_hz(data: &[u8], errors: Errors, out: &mut String) -> Result<(), DecodeError> {
    let tables = cjk();
    let gb2312 = &tables.iso2022["gb2312_7bit"];
    out.reserve(data.len());
    let mut position = 0usize;
    let mut gb_mode = false;
    while position < data.len() {
        let rest = &data[position..];
        let byte = rest[0];
        let step = if byte == b'~' {
            if rest.len() < 2 {
                Step::Incomplete
            } else {
                match (rest[1], gb_mode) {
                    (b'~', false) => {
                        out.push('~');
                        Step::Ok(2)
                    }
                    (b'{', false) => {
                        gb_mode = true;
                        Step::Ok(2)
                    }
                    (b'\n', false) => Step::Ok(2),
                    (b'}', true) => {
                        gb_mode = false;
                        Step::Ok(2)
                    }
                    _ => Step::Error(1),
                }
            }
        } else if byte & 0x80 != 0 {
            Step::Error(1)
        } else if !gb_mode {
            out.push(byte as char);
            Step::Ok(1)
        } else if rest.len() < 2 {
            Step::Incomplete
        } else {
            match gb2312.get(byte, rest[1]) {
                Some(value) => {
                    push_value(out, value, &tables.pairs)?;
                    Step::Ok(2)
                }
                None => Step::Error(1),
            }
        };
        if !advance(step, errors, &mut position)? {
            break;
        }
    }
    Ok(())
}

const DBCS: u8 = 0x80;
const CHARSET_ASCII: u8 = b'B';
const CHARSET_ISO8859_1: u8 = b'A';
const CHARSET_ISO8859_7: u8 = b'F';

#[derive(Clone, Copy)]
enum Designation {
    /// Double-byte charset decoded through an ISO-2022 table.
    Double(&'static str),
    /// Single-byte charset decoded through an ISO-2022 table.
    Single(&'static str),
    /// Charsets only usable through single shifts (G2); direct use fails.
    Dummy,
}

struct Iso2022Config {
    no_shift: bool,
    use_g2: bool,
    jisx0208_ext: bool,
    designations: &'static [(u8, Designation)],
}

const JISX0208: (u8, Designation) = (b'B' | DBCS, Designation::Double("jisx0208"));
const JISX0208_O: (u8, Designation) = (b'@' | DBCS, Designation::Double("jisx0208"));
const JISX0212: (u8, Designation) = (b'D' | DBCS, Designation::Double("jisx0212"));
const KSX1001: (u8, Designation) = (b'C' | DBCS, Designation::Double("ksx1001"));
const GB2312: (u8, Designation) = (b'A' | DBCS, Designation::Double("gb2312_7bit"));
const JISX0201_R: (u8, Designation) = (b'J', Designation::Single("jisx0201_r"));
const JISX0201_K: (u8, Designation) = (b'I', Designation::Single("jisx0201_k"));
const ISO8859_1: (u8, Designation) = (CHARSET_ISO8859_1, Designation::Dummy);
const ISO8859_7: (u8, Designation) = (CHARSET_ISO8859_7, Designation::Dummy);

fn iso2022_config(variant: Iso2022Variant) -> Iso2022Config {
    match variant {
        Iso2022Variant::Kr => Iso2022Config {
            no_shift: false,
            use_g2: false,
            jisx0208_ext: false,
            designations: &[KSX1001],
        },
        Iso2022Variant::Jp => Iso2022Config {
            no_shift: true,
            use_g2: false,
            jisx0208_ext: true,
            designations: &[JISX0208, JISX0201_R, JISX0208_O],
        },
        Iso2022Variant::Jp1 => Iso2022Config {
            no_shift: true,
            use_g2: false,
            jisx0208_ext: true,
            designations: &[JISX0208, JISX0212, JISX0201_R, JISX0208_O],
        },
        Iso2022Variant::Jp2 => Iso2022Config {
            no_shift: true,
            use_g2: true,
            jisx0208_ext: true,
            designations: &[
                JISX0208, JISX0212, KSX1001, GB2312, JISX0201_R, JISX0208_O, ISO8859_1, ISO8859_7,
            ],
        },
        Iso2022Variant::Jp2004 => Iso2022Config {
            no_shift: true,
            use_g2: false,
            jisx0208_ext: true,
            designations: &[
                (b'Q' | DBCS, Designation::Double("jisx0213_2004_1")),
                JISX0208,
                (b'P' | DBCS, Designation::Double("jisx0213_2004_2")),
            ],
        },
        Iso2022Variant::Jp3 => Iso2022Config {
            no_shift: true,
            use_g2: false,
            jisx0208_ext: true,
            designations: &[
                (b'O' | DBCS, Designation::Double("jisx0213_2000_1")),
                JISX0208,
                (b'P' | DBCS, Designation::Double("jisx0213_2000_2")),
            ],
        },
        Iso2022Variant::JpExt => Iso2022Config {
            no_shift: true,
            use_g2: false,
            jisx0208_ext: true,
            designations: &[JISX0208, JISX0212, JISX0201_R, JISX0201_K, JISX0208_O],
        },
    }
}

fn is_escape_end(byte: u8) -> bool {
    byte.is_ascii_uppercase() || byte == b'@'
}

/// `iso2022processesc`: returns the designation to apply or the error step.
fn iso2022_escape(config: &Iso2022Config, data: &[u8]) -> Result<(usize, usize, u8), Step> {
    let mut escape_length = 0usize;
    let mut index = 1usize;
    while index < 16 {
        if index >= data.len() {
            return Err(Step::Incomplete);
        }
        if is_escape_end(data[index]) {
            escape_length = index + 1;
            break;
        } else if config.jisx0208_ext
            && index + 1 < data.len()
            && data[index] == b'&'
            && data[index + 1] == b'@'
        {
            index += 2;
        }
        index += 1;
    }
    let (charset, designation) = match escape_length {
        0 => return Err(Step::Error(1)),
        3 => {
            if data[1] == b'$' {
                (data[2] | DBCS, 0)
            } else {
                let designation = match data[1] {
                    b'(' => 0,
                    b')' => 1,
                    b'.' if config.use_g2 => 2,
                    _ => return Err(Step::Error(3)),
                };
                (data[2], designation)
            }
        }
        4 => {
            if data[1] != b'$' {
                return Err(Step::Error(4));
            }
            let designation = match data[2] {
                b'(' => 0,
                b')' => 1,
                _ => return Err(Step::Error(4)),
            };
            (data[3] | DBCS, designation)
        }
        6 => {
            if config.jisx0208_ext && data[3] == 0x1B && data[4] == b'$' && data[5] == b'B' {
                (b'B' | DBCS, 0)
            } else {
                return Err(Step::Error(6));
            }
        }
        length => return Err(Step::Error(length)),
    };
    if charset != CHARSET_ASCII && !config.designations.iter().any(|(mark, _)| *mark == charset) {
        return Err(Step::Error(escape_length));
    }
    Ok((escape_length, designation, charset))
}

fn iso8859_7_decode(byte: u8) -> Option<char> {
    let c = u32::from(byte);
    let value = if c < 0xA0 || (c < 0xC0 && (0x288f_3bc9u32 & (1u32 << (c - 0xA0))) != 0) {
        c
    } else if (0xB4..=0xFE).contains(&c)
        && (c >= 0xD4 || (0xbfff_fd77u32 & (1u32 << (c - 0xB4))) != 0)
    {
        0x02D0 + c
    } else if c == 0xA1 {
        0x2018
    } else if c == 0xA2 {
        0x2019
    } else if c == 0xAF {
        0x2015
    } else {
        return None;
    };
    char::from_u32(value)
}

/// `ESC N x`: one character from the G2 charset (ISO-2022-JP-2 only).
fn single_shift(g2: u8, rest: &[u8], out: &mut String) -> Step {
    if rest.len() < 3 {
        return Step::Incomplete;
    }
    let value = rest[2];
    let decoded = match g2 {
        CHARSET_ISO8859_1 => (value < 0x80).then(|| char::from(value + 0x80)),
        CHARSET_ISO8859_7 => iso8859_7_decode(value ^ 0x80),
        CHARSET_ASCII => (value & 0x80 == 0).then_some(char::from(value)),
        _ => None, // CPython raises an internal codec error
    };
    match decoded {
        Some(character) => {
            out.push(character);
            Step::Ok(3)
        }
        None => Step::Error(3),
    }
}

/// One character of the charset currently designated to G0/G1.
fn designated(
    config: &Iso2022Config,
    charset: u8,
    rest: &[u8],
    out: &mut String,
) -> Result<Step, DecodeError> {
    let tables = cjk();
    let designation = config
        .designations
        .iter()
        .find(|(mark, _)| *mark == charset)
        .map_or(Designation::Dummy, |(_, designation)| *designation);
    Ok(match designation {
        Designation::Double(_) if rest.len() < 2 => Step::Incomplete,
        Designation::Double(table) => match tables.iso2022[table].get(rest[0], rest[1]) {
            Some(value) => {
                push_value(out, value, &tables.pairs)?;
                Step::Ok(2)
            }
            None => Step::Error(2),
        },
        Designation::Single(table) => match tables.iso2022[table].get(0, rest[0]) {
            Some(value) => {
                push_value(out, value, &tables.pairs)?;
                Step::Ok(1)
            }
            None => Step::Error(1),
        },
        Designation::Dummy => Step::Error(1),
    })
}

pub(super) fn decode_iso2022(
    variant: Iso2022Variant,
    data: &[u8],
    errors: Errors,
    out: &mut String,
) -> Result<(), DecodeError> {
    const ESC: u8 = 0x1B;
    const SO: u8 = 0x0E;
    const SI: u8 = 0x0F;
    const LF: u8 = 0x0A;

    let config = iso2022_config(variant);
    out.reserve(data.len());
    let mut g = [CHARSET_ASCII; 4];
    let mut shifted = false;
    let mut escape_throughout = false;
    let mut position = 0usize;

    while position < data.len() {
        let rest = &data[position..];
        let byte = rest[0];
        if escape_throughout {
            out.push(char::from(byte)); // assume ISO-8859-1
            position += 1;
            if is_escape_end(byte) {
                escape_throughout = false;
            }
            continue;
        }
        let bypass = |out: &mut String| {
            out.push(char::from(byte));
            Step::Ok(1)
        };
        let step = match byte {
            ESC if rest.len() < 2 => Step::Incomplete,
            ESC if matches!(rest[1], b'(' | b')' | b'$' | b'.' | b'&') => {
                match iso2022_escape(&config, rest) {
                    Ok((length, designation, charset)) => {
                        g[designation] = charset;
                        Step::Ok(length)
                    }
                    Err(step) => step,
                }
            }
            ESC if config.use_g2 && rest[1] == b'N' => single_shift(g[2], rest, out),
            ESC => {
                out.push(char::from(ESC));
                escape_throughout = true;
                Step::Ok(1)
            }
            SI | SO if config.no_shift => bypass(out),
            SI => {
                shifted = false;
                Step::Ok(1)
            }
            SO => {
                shifted = true;
                Step::Ok(1)
            }
            LF => {
                shifted = false;
                out.push('\n');
                Step::Ok(1)
            }
            _ if byte < 0x20 => bypass(out),
            _ if byte >= 0x80 => Step::Error(1),
            _ => {
                let charset = if shifted { g[1] } else { g[0] };
                if charset == CHARSET_ASCII {
                    bypass(out)
                } else {
                    designated(&config, charset, rest, out)?
                }
            }
        };
        if !advance(step, errors, &mut position)? {
            break;
        }
    }
    Ok(())
}

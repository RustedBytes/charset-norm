//! Character properties computed natively (no `unicodedata` round-trips).
//!
//! Python semantics are reproduced on top of the `unicode_names2`,
//! `unicode-general-category` and `unicode-normalization` crates. Results
//! are memoized per code point in a lock-free table, so hot loops pay for
//! a name lookup at most once per character for the process lifetime.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use unicode_general_category::{get_general_category, GeneralCategory};

use crate::tables::{
    ACCENT_KEYWORDS, BASIC_LATIN_COMPATIBLE_RANGE_FAMILIES, COMMON_CJK_CHARACTERS,
    COMMON_SAFE_ASCII_CHARACTERS, COMPATIBLE_RANGE_FAMILIES, COMPATIBLE_WITH_ANY_RANGE_FAMILIES,
    NON_DECIMAL_DIGITS, UNICODE_RANGES,
};

pub const LATIN: u16 = 1;
pub const ACCENTUATED: u16 = 1 << 1;
pub const CJK: u16 = 1 << 2;
pub const HANGUL: u16 = 1 << 3;
pub const KATAKANA: u16 = 1 << 4;
pub const HIRAGANA: u16 = 1 << 5;
pub const THAI: u16 = 1 << 6;
pub const ARABIC: u16 = 1 << 7;
pub const ARABIC_ISOLATED_FORM: u16 = 1 << 8;
pub const HALFWIDTH_KATAKANA: u16 = 1 << 9;
pub const LIGATURE: u16 = 1 << 10;
pub const SUPERSCRIPT: u16 = 1 << 11;
pub const SENTENCE_OPEN_PUNCTUATION: u16 = 1 << 12;

/// Two-letter general category, as `unicodedata.category` reports it.
pub fn category(character: char) -> &'static str {
    use GeneralCategory::*;
    match get_general_category(character) {
        UppercaseLetter => "Lu",
        LowercaseLetter => "Ll",
        TitlecaseLetter => "Lt",
        ModifierLetter => "Lm",
        OtherLetter => "Lo",
        NonspacingMark => "Mn",
        SpacingMark => "Mc",
        EnclosingMark => "Me",
        DecimalNumber => "Nd",
        LetterNumber => "Nl",
        OtherNumber => "No",
        ConnectorPunctuation => "Pc",
        DashPunctuation => "Pd",
        OpenPunctuation => "Ps",
        ClosePunctuation => "Pe",
        InitialPunctuation => "Pi",
        FinalPunctuation => "Pf",
        OtherPunctuation => "Po",
        MathSymbol => "Sm",
        CurrencySymbol => "Sc",
        ModifierSymbol => "Sk",
        OtherSymbol => "So",
        SpaceSeparator => "Zs",
        LineSeparator => "Zl",
        ParagraphSeparator => "Zp",
        Control => "Cc",
        Format => "Cf",
        Surrogate => "Cs",
        PrivateUse => "Co",
        Unassigned => "Cn",
        _ => "Cn",
    }
}

/// `str.isspace()`: bidirectional class WS/B/S or category Zs.
pub fn is_space(character: char) -> bool {
    character.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&character)
}

/// `str.isdigit()`: Numeric_Type Decimal or Digit.
pub fn is_digit(character: char) -> bool {
    category(character) == "Nd"
        || NON_DECIMAL_DIGITS
            .binary_search(&(character as u32))
            .is_ok()
}

fn flags_from_name(character: char) -> u16 {
    let Some(name) = unicode_names2::name(character) else {
        return 0;
    };
    let description = name.to_string();
    let mut flags = 0u16;
    if description.contains("LATIN") {
        flags |= LATIN;
    }
    if description.contains("CJK") {
        flags |= CJK;
    }
    if description.contains("HANGUL") {
        flags |= HANGUL;
    }
    if description.contains("KATAKANA") {
        flags |= KATAKANA;
        if description.contains("HALFWIDTH") {
            flags |= HALFWIDTH_KATAKANA;
        }
    }
    if description.contains("HIRAGANA") {
        flags |= HIRAGANA;
    }
    if description.contains("THAI") {
        flags |= THAI;
    }
    if description.contains("ARABIC") {
        flags |= ARABIC;
        if description.contains("ISOLATED FORM") {
            flags |= ARABIC_ISOLATED_FORM;
        }
    }
    if description.contains("LIGATURE") || description.ends_with("LETTER AE") {
        flags |= LIGATURE;
    }
    if description.contains("SUPERSCRIPT") {
        flags |= SUPERSCRIPT;
    }
    if description == "INVERTED QUESTION MARK" || description == "INVERTED EXCLAMATION MARK" {
        flags |= SENTENCE_OPEN_PUNCTUATION;
    }
    if ACCENT_KEYWORDS
        .iter()
        .any(|keyword| description.contains(keyword))
    {
        flags |= ACCENTUATED;
    }
    flags
}

/// First code point of the one-step canonical decomposition, like
/// `chr(int(unicodedata.decomposition(c).split()[0], 16))`.
pub fn remove_accent(character: char) -> char {
    let mut parts = Vec::with_capacity(4);
    unicode_normalization::char::decompose_canonical(character, |part| parts.push(part));
    if parts.len() <= 1 {
        return parts.first().copied().unwrap_or(character);
    }
    // The full decomposition is recursive; recompose everything but the last
    // mark to recover the head of the single-step mapping.
    let last = parts[parts.len() - 1];
    let head = parts[1..parts.len() - 1]
        .iter()
        .try_fold(parts[0], |base, &mark| {
            unicode_normalization::char::compose(base, mark)
        });
    if let Some(head) = head {
        if unicode_normalization::char::compose(head, last) == Some(character) {
            return head;
        }
    }
    // Singleton or composition-excluded mapping.
    let mut composed = parts[0];
    for &mark in &parts[1..] {
        match unicode_normalization::char::compose(composed, mark) {
            Some(value) => composed = value,
            None => return composed,
        }
    }
    composed
}

pub struct RangeInfo {
    pub name: &'static str,
    pub family: &'static str,
    pub secondary: bool,
    pub punctuation: bool,
    pub forms: bool,
    pub emoticon: bool,
}

pub fn ranges() -> &'static [RangeInfo] {
    static RANGES: OnceLock<Vec<RangeInfo>> = OnceLock::new();
    RANGES.get_or_init(|| {
        UNICODE_RANGES
            .iter()
            .map(|&(_, _, name, family, secondary)| RangeInfo {
                name,
                family,
                secondary,
                punctuation: name.contains("Punctuation"),
                forms: name.contains("Forms"),
                emoticon: name.contains("Emoticons") || name.contains("Pictographs"),
            })
            .collect()
    })
}

pub const NO_RANGE: u16 = 0x3FF;

fn range_index_uncached(codepoint: u32) -> u16 {
    let index = UNICODE_RANGES.partition_point(|entry| entry.0 <= codepoint);
    if index == 0 {
        return NO_RANGE;
    }
    let (start, stop, ..) = UNICODE_RANGES[index - 1];
    if start <= codepoint && codepoint < stop {
        (index - 1) as u16
    } else {
        NO_RANGE
    }
}

pub fn range_of(character: char) -> Option<&'static RangeInfo> {
    let index = props(character).range;
    (index != NO_RANGE).then(|| &ranges()[index as usize])
}

pub fn unicode_range(character: char) -> Option<&'static str> {
    range_of(character).map(|range| range.name)
}

pub fn range_index(name: &str) -> Option<usize> {
    ranges().iter().position(|range| range.name == name)
}

fn compatible_families(a: &str, b: &str) -> bool {
    let pair = if a <= b { (a, b) } else { (b, a) };
    COMPATIBLE_RANGE_FAMILIES.contains(&pair)
}

/// `is_suspiciously_successive_range` on range table entries.
pub fn suspicious_ranges(a: Option<&RangeInfo>, b: Option<&RangeInfo>) -> bool {
    let (Some(a), Some(b)) = (a, b) else {
        return true;
    };
    if a.family == b.family {
        return false;
    }
    if COMPATIBLE_WITH_ANY_RANGE_FAMILIES.contains(&a.family)
        || COMPATIBLE_WITH_ANY_RANGE_FAMILIES.contains(&b.family)
    {
        return false;
    }
    if compatible_families(a.family, b.family) {
        return false;
    }
    if a.name == "Basic Latin" {
        return !BASIC_LATIN_COMPATIBLE_RANGE_FAMILIES.contains(&b.family);
    }
    if b.name == "Basic Latin" {
        return !BASIC_LATIN_COMPATIBLE_RANGE_FAMILIES.contains(&a.family);
    }
    true
}

/// `suspicious_ranges` by range-table index (`NO_RANGE` for none), memoized
/// as a matrix over every pair of ranges.
pub fn suspicious_range_indices(a: u16, b: u16) -> bool {
    static MATRIX: OnceLock<(usize, Vec<bool>)> = OnceLock::new();
    if a == NO_RANGE || b == NO_RANGE {
        return true;
    }
    let (size, matrix) = MATRIX.get_or_init(|| {
        let table = ranges();
        let size = table.len();
        let mut matrix = Vec::with_capacity(size * size);
        for first in table {
            for second in table {
                matrix.push(suspicious_ranges(Some(first), Some(second)));
            }
        }
        (size, matrix)
    });
    matrix[a as usize * size + b as usize]
}

/// Memoized per-character properties.
#[derive(Clone, Copy)]
pub struct Props {
    pub category: &'static str,
    pub upper: bool,
    pub lower: bool,
    pub space: bool,
    pub digit: bool,
    pub flags: u16,
    pub range: u16,
    pub unaccented: char,
}

impl Props {
    pub fn alpha(&self) -> bool {
        self.category.starts_with('L')
    }

    pub fn printable(&self, character: char) -> bool {
        character == ' '
            || !matches!(
                self.category,
                "Cc" | "Cf" | "Cs" | "Co" | "Cn" | "Zl" | "Zp" | "Zs"
            )
    }

    pub fn range(&self) -> Option<&'static RangeInfo> {
        (self.range != NO_RANGE).then(|| &ranges()[self.range as usize])
    }
}

const CATEGORIES: [&str; 30] = [
    "Lu", "Ll", "Lt", "Lm", "Lo", "Mn", "Mc", "Me", "Nd", "Nl", "No", "Pc", "Pd", "Ps", "Pe", "Pi",
    "Pf", "Po", "Sm", "Sc", "Sk", "So", "Zs", "Zl", "Zp", "Cc", "Cf", "Cs", "Co", "Cn",
];

const CACHE_SIZE: usize = 0x30000;
const COMPUTED: u64 = 1 << 63;
static CACHE: [AtomicU64; CACHE_SIZE] = [const { AtomicU64::new(0) }; CACHE_SIZE];

fn compute(character: char) -> Props {
    let flags = if character.is_ascii() {
        if character.is_ascii_alphabetic() {
            LATIN
        } else {
            0
        }
    } else {
        flags_from_name(character)
    };
    let unaccented = if flags & LATIN != 0 && flags & ACCENTUATED != 0 {
        remove_accent(character)
    } else {
        character
    };
    Props {
        category: category(character),
        upper: character.is_uppercase(),
        lower: character.is_lowercase(),
        space: is_space(character),
        digit: is_digit(character),
        flags,
        range: range_index_uncached(character as u32),
        unaccented,
    }
}

fn pack(props: &Props) -> u64 {
    let category = CATEGORIES
        .iter()
        .position(|value| *value == props.category)
        .unwrap_or(29) as u64;
    COMPUTED
        | category
        | (props.upper as u64) << 5
        | (props.lower as u64) << 6
        | (props.space as u64) << 7
        | (props.digit as u64) << 8
        | (props.flags as u64) << 9
        | (props.range as u64) << 22
        | (props.unaccented as u64) << 32
}

fn unpack(bits: u64) -> Props {
    Props {
        category: CATEGORIES[(bits & 0x1F) as usize],
        upper: bits & (1 << 5) != 0,
        lower: bits & (1 << 6) != 0,
        space: bits & (1 << 7) != 0,
        digit: bits & (1 << 8) != 0,
        flags: ((bits >> 9) & 0x1FFF) as u16,
        range: ((bits >> 22) & 0x3FF) as u16,
        unaccented: char::from_u32(((bits >> 32) & 0x1F_FFFF) as u32).unwrap_or('\0'),
    }
}

pub fn props(character: char) -> Props {
    let codepoint = character as usize;
    if codepoint >= CACHE_SIZE {
        return compute(character);
    }
    let bits = CACHE[codepoint].load(Ordering::Relaxed);
    if bits & COMPUTED != 0 {
        return unpack(bits);
    }
    let value = compute(character);
    CACHE[codepoint].store(pack(&value), Ordering::Relaxed);
    value
}

pub fn character_flags(character: char) -> u16 {
    props(character).flags
}

pub fn is_safe_ascii(character: char) -> bool {
    character.is_ascii() && COMMON_SAFE_ASCII_CHARACTERS.contains(&character)
}

pub fn is_common_cjk(character: char) -> bool {
    COMMON_CJK_CHARACTERS.binary_search(&character).is_ok()
}

pub fn is_punctuation(character: char) -> bool {
    let props = props(character);
    props.category.starts_with('P') || props.range().is_some_and(|range| range.punctuation)
}

pub fn is_symbol(character: char) -> bool {
    let props = props(character);
    props.category.starts_with('S')
        || props.category.starts_with('N')
        || (props.range().is_some_and(|range| range.forms) && props.category != "Lo")
}

pub fn is_emoticon(character: char) -> bool {
    range_of(character).is_some_and(|range| range.emoticon)
}

pub fn is_separator(character: char) -> bool {
    let props = props(character);
    props.space
        || matches!(character, '｜' | '+' | '<' | '>')
        || props.category.starts_with('Z')
        || matches!(props.category, "Po" | "Pd" | "Pc")
}

pub fn is_case_variable(character: char) -> bool {
    let props = props(character);
    props.lower != props.upper
}

pub fn is_unprintable(character: char) -> bool {
    let props = props(character);
    !props.space && !props.printable(character) && character != '\u{1a}' && character != '\u{feff}'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packing_round_trips() {
        for character in ['a', 'É', 'ấ', '中', 'ｶ', '😀', '\u{10ffff}'] {
            let direct = compute(character);
            let cached = unpack(pack(&direct));
            assert_eq!(direct.category, cached.category);
            assert_eq!(direct.flags, cached.flags);
            assert_eq!(direct.range, cached.range);
            assert_eq!(direct.unaccented, cached.unaccented);
        }
    }

    #[test]
    fn one_step_decomposition() {
        assert_eq!(remove_accent('é'), 'e');
        assert_eq!(remove_accent('Ấ'), 'Â');
        assert_eq!(remove_accent('\u{212b}'), 'Å');
        assert_eq!(remove_accent('a'), 'a');
    }
}

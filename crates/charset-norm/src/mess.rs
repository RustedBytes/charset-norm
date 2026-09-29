//! Mess (noise) detection: how implausible decoded text looks.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::log::{Level, Logger, NoLogger, emit};
use crate::pyfloat;
use crate::unicode::{
    self, ACCENTUATED, ARABIC, ARABIC_ISOLATED_FORM, CJK, HALFWIDTH_KATAKANA, HANGUL, HIRAGANA,
    KATAKANA, LATIN, LIGATURE, RangeInfo, SENTENCE_OPEN_PUNCTUATION, SUPERSCRIPT, THAI,
    is_common_cjk, is_safe_ascii,
};

const GLYPH_MASK: u16 = CJK | HANGUL | KATAKANA | HIRAGANA | THAI;

const CACHE_SIZE: usize = 0x30000;
const COMPUTED: u64 = 1 << 63;
static CACHE: [AtomicU64; CACHE_SIZE] = [const { AtomicU64::new(0) }; CACHE_SIZE];

const PRINTABLE: u64 = 1 << 13;
const ALPHA: u64 = 1 << 14;
const UPPER: u64 = 1 << 15;
const LOWER: u64 = 1 << 16;
const SPACE: u64 = 1 << 17;
const DIGIT: u64 = 1 << 18;
const ASCII: u64 = 1 << 19;
const PUNCT: u64 = 1 << 20;
const SYMBOL: u64 = 1 << 21;
const SEPARATOR: u64 = 1 << 22;
const EMOTICON: u64 = 1 << 23;
const SAFE: u64 = 1 << 24;
const COMMON_CJK: u64 = 1 << 25;

/// Packed mess-detector view of a character: bits 0-12 hold the Unicode
/// name flags, 13-25 the boolean properties, 26-35 the range index and
/// 36-56 the unaccented character; bit 63 marks a computed cache entry.
#[derive(Clone, Copy)]
struct CharInfo(u64);

macro_rules! bit_accessors {
    ($($name:ident => $bit:expr),* $(,)?) => {
        $(#[inline] fn $name(self) -> bool { self.0 & $bit != 0 })*
    };
}

macro_rules! flag_accessors {
    ($($name:ident => $flag:expr),* $(,)?) => {
        $(#[inline] fn $name(self) -> bool { self.flags() & $flag != 0 })*
    };
}

impl CharInfo {
    bit_accessors! {
        printable => PRINTABLE,
        alpha => ALPHA,
        upper => UPPER,
        lower => LOWER,
        space => SPACE,
        digit => DIGIT,
        ascii => ASCII,
        punct => PUNCT,
        symbol => SYMBOL,
        separator => SEPARATOR,
        emoticon => EMOTICON,
        safe => SAFE,
        common_cjk => COMMON_CJK,
    }
    flag_accessors! {
        accentuated => ACCENTUATED,
        latin => LATIN,
        cjk => CJK,
        katakana => KATAKANA,
        halfwidth_katakana => HALFWIDTH_KATAKANA,
        arabic => ARABIC,
        arabic_isolated_form => ARABIC_ISOLATED_FORM,
        ligature => LIGATURE,
        superscript => SUPERSCRIPT,
        sentence_open_punctuation => SENTENCE_OPEN_PUNCTUATION,
        glyph => GLYPH_MASK,
    }

    #[inline]
    fn flags(self) -> u16 {
        (self.0 & 0x1FFF) as u16
    }

    #[inline]
    fn case_variable(self) -> bool {
        self.lower() != self.upper()
    }

    #[inline]
    fn range(self) -> u16 {
        ((self.0 >> 26) & 0x3FF) as u16
    }

    #[inline]
    fn unaccented(self) -> u32 {
        ((self.0 >> 36) & 0x1F_FFFF) as u32
    }
}

/// `(isalpha, unicode range index)` for coherence splitting.
#[inline]
pub(crate) fn alpha_range(character: char) -> (bool, u16) {
    let info = char_info(character);
    (info.alpha(), info.range())
}

/// Mess-detector view of a character, memoized per code point.
#[inline]
fn char_info(character: char) -> CharInfo {
    let codepoint = character as usize;
    if codepoint >= CACHE_SIZE {
        return CharInfo(compute_char_info(character));
    }
    let bits = CACHE[codepoint].load(Ordering::Relaxed);
    if bits & COMPUTED != 0 {
        return CharInfo(bits);
    }
    let bits = compute_char_info(character);
    CACHE[codepoint].store(bits, Ordering::Relaxed);
    CharInfo(bits)
}

fn compute_char_info(character: char) -> u64 {
    let props = unicode::props(character);
    let ascii = character.is_ascii();
    let printable = props.printable(character);
    let alpha = props.alpha();
    let category = props.category;
    let range: Option<&RangeInfo> = props.range();
    let punct =
        printable && (category.starts_with('P') || range.is_some_and(|range| range.punctuation));
    let symbol = printable
        && (category.starts_with('S')
            || (!ascii
                && (category.starts_with('N')
                    || (range.is_some_and(|range| range.forms) && category != "Lo"))));
    let separator = props.space
        || matches!(character, '｜' | '+' | '<' | '>')
        || category.starts_with('Z')
        || matches!(category, "Po" | "Pd" | "Pc");
    let flags = props.flags;

    let mut bits = COMPUTED
        | u64::from(flags)
        | u64::from(props.range) << 26
        | u64::from(u32::from(props.unaccented)) << 36;
    for (set, bit) in [
        (printable, PRINTABLE),
        (alpha, ALPHA),
        (props.upper, UPPER),
        (props.lower, LOWER),
        (props.space, SPACE),
        (props.digit, DIGIT),
        (ascii, ASCII),
        (punct, PUNCT),
        (symbol, SYMBOL),
        (separator, SEPARATOR),
        (
            !alpha && range.is_some_and(|range| range.emoticon),
            EMOTICON,
        ),
        (is_safe_ascii(character), SAFE),
        (flags & CJK != 0 && is_common_cjk(character), COMMON_CJK),
    ] {
        if set {
            bits |= bit;
        }
    }
    bits
}

#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "running state of the reference plugins, one flag per condition"
)]
struct Detectors {
    punctuation: usize,
    symbols: usize,
    printable_count: usize,
    last_printable: Option<char>,
    alpha_count: usize,
    accents: usize,
    unprintable: usize,
    all_count: usize,
    has_escape: bool,
    duplicate_count: usize,
    latin_count: usize,
    last_latin: Option<(bool, bool, u32)>,
    suspicious_ranges: usize,
    range_count: usize,
    last_range: Option<u16>,
    word_count: usize,
    foreign_long_count: usize,
    character_count: usize,
    bad_character_count: usize,
    buffer_length: usize,
    buffer_last_upper: bool,
    buffer_last_accent: bool,
    buffer_accents: usize,
    buffer_glyphs: usize,
    buffer_uppers: usize,
    buffer_first_lower: bool,
    buffer_non_ascii: bool,
    buffer_last_ligature: bool,
    buffer_internal_ligature: bool,
    current_bad: bool,
    current_invalid: bool,
    invalid_words: usize,
    foreign_watch: bool,
    cjk_count: usize,
    uncommon_cjk: usize,
    katakana_count: usize,
    halfwidth_katakana: usize,
    katakana_cjk_count: usize,
    katakana_uncommon_cjk: usize,
    archaic_buf: bool,
    archaic_chunk_count: usize,
    archaic_current: usize,
    archaic_final: usize,
    archaic_count: usize,
    archaic_last_upper: bool,
    archaic_last_lower: bool,
    archaic_ascii_only: bool,
    arabic_count: usize,
    isolated_arabic: usize,
}

impl Detectors {
    fn new() -> Self {
        Self {
            archaic_ascii_only: true,
            ..Self::default()
        }
    }

    fn feed_always(&mut self, ch: char, i: CharInfo) {
        if ch == '\u{1b}' {
            self.has_escape = true;
        }
        if !i.printable() && !i.space() && ch != '\u{1a}' && ch != '\u{feff}' {
            self.unprintable += 1;
        }
        self.all_count += 1;
        self.feed_word(ch, i);
    }

    fn feed_printable(&mut self, ch: char, i: CharInfo) {
        self.printable_count += 1;
        if self.last_printable != Some(ch) && !i.safe() {
            if i.punct() {
                self.punctuation += 1;
            } else if !i.digit() && i.symbol() && !i.emoticon() {
                self.symbols += 2;
            }
        }
        self.last_printable = Some(ch);

        self.range_count += 1;
        if i.space() || i.punct() || i.safe() {
            self.last_range = None;
            return;
        }
        if let Some(previous) = self.last_range
            && (previous != i.range() || previous == unicode::NO_RANGE)
            && unicode::suspicious_range_indices(previous, i.range())
        {
            self.suspicious_ranges += 1;
        }
        self.last_range = Some(i.range());
    }

    fn feed_alpha(&mut self, i: CharInfo) {
        self.alpha_count += 1;
        if i.accentuated() {
            self.accents += 1;
        }
        if i.latin() {
            self.latin_count += 1;
            if let Some((upper, accent, unaccented)) = &self.last_latin
                && i.accentuated()
                && *accent
            {
                if i.upper() && *upper {
                    self.duplicate_count += 1;
                }
                if i.unaccented() == *unaccented {
                    self.duplicate_count += 1;
                }
            }
            self.last_latin = Some((i.upper(), i.accentuated(), i.unaccented()));
        }
        if i.cjk() {
            self.cjk_count += 1;
            if !i.common_cjk() {
                self.uncommon_cjk += 1;
            }
        }
        if i.cjk() || i.katakana() {
            if i.katakana() {
                self.katakana_count += 1;
                if i.halfwidth_katakana() {
                    self.halfwidth_katakana += 1;
                }
            } else {
                self.katakana_cjk_count += 1;
                if !i.common_cjk() {
                    self.katakana_uncommon_cjk += 1;
                }
            }
        }
        if i.arabic() {
            self.arabic_count += 1;
            if i.arabic_isolated_form() {
                self.isolated_arabic += 1;
            }
        }
    }

    fn feed_archaic(&mut self, i: CharInfo) {
        let concerned = i.alpha() && i.case_variable();
        if !concerned && self.archaic_chunk_count > 0 {
            if self.archaic_chunk_count <= 64 && !i.digit() && !self.archaic_ascii_only {
                self.archaic_final += self.archaic_current;
            }
            self.archaic_current = 0;
            self.archaic_chunk_count = 0;
            self.archaic_buf = false;
            self.archaic_count += 1;
            self.archaic_ascii_only = true;
            return;
        }
        if self.archaic_ascii_only && !i.ascii() {
            self.archaic_ascii_only = false;
        }
        if self.archaic_chunk_count > 0 {
            if (i.upper() && self.archaic_last_lower) || (i.lower() && self.archaic_last_upper) {
                if self.archaic_buf {
                    self.archaic_current += 2;
                    self.archaic_buf = false;
                } else {
                    self.archaic_buf = true;
                }
            } else {
                self.archaic_buf = false;
            }
        }
        self.archaic_count += 1;
        self.archaic_chunk_count += 1;
        self.archaic_last_upper = i.upper();
        self.archaic_last_lower = i.lower();
    }

    fn feed_word(&mut self, ch: char, i: CharInfo) {
        if i.alpha() {
            if self.buffer_last_ligature {
                self.buffer_internal_ligature = true;
            }
            self.buffer_last_ligature = i.ligature();
            if self.buffer_length == 0 {
                self.buffer_first_lower = i.lower();
            }
            self.buffer_length += 1;
            self.buffer_last_upper = i.upper();
            if i.upper() {
                self.buffer_uppers += 1;
            }
            if !i.ascii() {
                self.buffer_non_ascii = true;
            }
            self.buffer_last_accent = i.accentuated();
            if i.accentuated() {
                self.buffer_accents += 1;
            }
            if i.glyph() {
                self.buffer_glyphs += 1;
            } else if !self.foreign_watch && (!i.latin() || i.accentuated()) {
                self.foreign_watch = true;
            }
            return;
        }
        if self.buffer_length == 0 {
            return;
        }
        if i.sentence_open_punctuation() || (i.superscript() && self.buffer_internal_ligature) {
            self.current_bad = true;
            self.current_invalid = true;
        }
        if i.space() || i.punct() || i.separator() {
            self.word_count += 1;
            let length = self.buffer_length;
            self.character_count += length;
            if length >= 4 {
                if self.buffer_accents as f64 / length as f64 >= 0.5 {
                    self.current_bad = true;
                } else if self.buffer_last_accent
                    && self.buffer_last_upper
                    && self.buffer_uppers != length
                {
                    self.foreign_long_count += 1;
                    self.current_bad = true;
                } else if self.buffer_glyphs == 1 {
                    self.current_bad = true;
                    self.foreign_long_count += 1;
                } else if self.buffer_non_ascii
                    && self.buffer_first_lower
                    && self.buffer_uppers == length - 1
                {
                    self.foreign_long_count += 1;
                    self.current_bad = true;
                }
            }
            if length >= 24 && self.foreign_watch {
                let camel =
                    self.buffer_uppers > 0 && self.buffer_uppers as f64 / length as f64 <= 0.3;
                if !camel {
                    self.foreign_long_count += 1;
                    self.current_bad = true;
                }
            }
            if self.current_bad {
                self.bad_character_count += length;
            }
            if self.current_invalid {
                self.invalid_words += 1;
            }
            self.current_bad = false;
            self.current_invalid = false;
            self.foreign_watch = false;
            self.buffer_length = 0;
            self.buffer_last_accent = false;
            self.buffer_accents = 0;
            self.buffer_glyphs = 0;
            self.buffer_uppers = 0;
            self.buffer_first_lower = false;
            self.buffer_non_ascii = false;
            self.buffer_last_ligature = false;
            self.buffer_internal_ligature = false;
        } else if !matches!(ch, '<' | '>' | '-' | '=' | '~' | '|' | '_') && !i.digit() && i.symbol()
        {
            self.current_bad = true;
            self.buffer_length += 1;
            self.buffer_last_accent = false;
        }
    }

    fn ratios(&self) -> [f64; 10] {
        let sp = if self.printable_count == 0 {
            0.0
        } else {
            let r = (self.punctuation + self.symbols) as f64 / self.printable_count as f64;
            if r >= 0.3 { r } else { 0.0 }
        };
        let ta = if self.alpha_count < 8 {
            0.0
        } else {
            let r = self.accents as f64 / self.alpha_count as f64;
            if r >= 0.35 { r } else { 0.0 }
        };
        let up = if self.all_count == 0 {
            0.0
        } else if self.has_escape {
            1.0
        } else {
            (self.unprintable * 8) as f64 / self.all_count as f64
        };
        let sda = if self.latin_count == 0 {
            0.0
        } else {
            (self.duplicate_count * 2) as f64 / self.latin_count as f64
        };
        let sr = if self.range_count <= 13 {
            0.0
        } else {
            (self.suspicious_ranges * 2) as f64 / self.range_count as f64
        };
        let sw = if self.invalid_words > 0 {
            1.0
        } else if self.word_count <= 10 && self.foreign_long_count == 0 {
            0.0
        } else {
            self.bad_character_count as f64 / self.character_count as f64
        };
        let cu = if self.cjk_count < 4 {
            0.0
        } else {
            ((2.0 * self.uncommon_cjk as f64 - self.cjk_count as f64)
                / (5 * self.cjk_count.max(16)) as f64)
                .max(0.0)
        };
        let sk = if self.halfwidth_katakana >= 4
            && self.halfwidth_katakana == self.katakana_count
            && self.katakana_cjk_count >= 3
            && self.katakana_cjk_count == self.katakana_uncommon_cjk
        {
            1.0
        } else {
            0.0
        };
        let au = if self.archaic_count == 0 {
            0.0
        } else {
            self.archaic_final as f64 / self.archaic_count as f64
        };
        let ai = if self.arabic_count < 8 {
            0.0
        } else {
            self.isolated_arabic as f64 / self.arabic_count as f64
        };
        [sp, ta, up, sda, sr, sw, cu, sk, au, ai]
    }
}

/// Mess ratio of decoded text: 0 for clean text, growing with noise.
///
/// Analysis stops early once the ratio reaches `maximum_threshold`.
///
/// ```
/// assert_eq!(charset_norm::mess::mess_ratio("plain text", 0.2), 0.0);
/// ```
#[must_use]
pub fn mess_ratio(decoded_sequence: &str, maximum_threshold: f64) -> f64 {
    mess_ratio_with(decoded_sequence, maximum_threshold, false, &NoLogger)
}

/// [`mess_ratio`], optionally tracing the per-detector breakdown to `logger`.
pub fn mess_ratio_with(
    decoded_sequence: &str,
    maximum_threshold: f64,
    debug: bool,
    logger: &dyn Logger,
) -> f64 {
    let length = decoded_sequence.chars().count();
    let step = if length < 511 {
        32
    } else if length < 1024 {
        64
    } else {
        128
    };
    let pure_ascii = decoded_sequence.is_ascii();
    let mut detectors = Detectors::new();
    let mut mean = 0.0;
    let mut completed = true;
    let mut characters = decoded_sequence.chars();
    let mut remaining = length;
    while remaining > 0 {
        let block = step.min(remaining);
        remaining -= block;
        for ch in characters.by_ref().take(block) {
            let info = char_info(ch);
            detectors.feed_always(ch, info);
            if pure_ascii {
                if info.printable() {
                    detectors.feed_printable(ch, info);
                }
                continue;
            }
            detectors.feed_archaic(info);
            if info.printable() {
                detectors.feed_printable(ch, info);
            }
            if info.alpha() {
                detectors.feed_alpha(info);
            }
        }
        mean = detectors.ratios().iter().sum();
        if mean >= maximum_threshold {
            completed = false;
            break;
        }
    }
    if completed {
        let newline = char_info('\n');
        detectors.feed_word('\n', newline);
        if !pure_ascii {
            detectors.feed_archaic(newline);
        }
        if !newline.printable() && !newline.space() {
            detectors.unprintable += 1;
        }
        detectors.all_count += 1;
        mean = detectors.ratios().iter().sum();
    }
    if debug {
        emit(logger, Level::Trace, || {
            format!(
                "Mess-detector extended-analysis start. intermediary_mean_mess_ratio_calc={step} mean_mess_ratio={mean:?} maximum_threshold={maximum_threshold:?}"
            )
        });
        if length > 16 {
            emit(logger, Level::Trace, || {
                format!(
                    "Starting with: {}",
                    decoded_sequence.chars().take(16).collect::<String>()
                )
            });
            emit(logger, Level::Trace, || {
                format!(
                    "Ending with: {}",
                    decoded_sequence
                        .chars()
                        .skip(length - 16)
                        .collect::<String>()
                )
            });
        }
        let names = [
            "TooManySymbolOrPunctuationPlugin",
            "TooManyAccentuatedPlugin",
            "UnprintablePlugin",
            "SuspiciousDuplicateAccentPlugin",
            "SuspiciousRange",
            "SuperWeirdWordPlugin",
            "CjkUncommonPlugin",
            "SuspiciousKatakanaPlugin",
            "ArchaicUpperLowerPlugin",
            "ArabicIsolatedFormPlugin",
        ];
        for (name, ratio) in names.into_iter().zip(detectors.ratios()) {
            emit(logger, Level::Trace, || {
                format!("<class 'charset_norm.md.{name}'>: {ratio:?}")
            });
        }
    }
    pyfloat::round(mean, 3)
}

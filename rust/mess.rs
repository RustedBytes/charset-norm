use std::collections::{HashMap, HashSet};

use pyo3::prelude::*;
use pyo3::types::PyString;

use super::{character_flags, constants, suspicious_ranges_impl, unicode_range};
use super::{
    ACCENTUATED, ARABIC, ARABIC_ISOLATED_FORM, CJK, HALFWIDTH_KATAKANA, HANGUL, HIRAGANA, KATAKANA,
    LATIN, LIGATURE, SENTENCE_OPEN_PUNCTUATION, SUPERSCRIPT, THAI,
};

const GLYPH_MASK: u16 = CJK | HANGUL | KATAKANA | HIRAGANA | THAI;

#[derive(Clone)]
struct CharInfo {
    printable: bool,
    alpha: bool,
    upper: bool,
    lower: bool,
    space: bool,
    digit: bool,
    ascii: bool,
    case_variable: bool,
    flags: u16,
    accentuated: bool,
    latin: bool,
    cjk: bool,
    katakana: bool,
    halfwidth_katakana: bool,
    arabic: bool,
    ligature: bool,
    superscript: bool,
    sentence_open_punctuation: bool,
    glyph: bool,
    punct: bool,
    symbol: bool,
    range: Option<String>,
    separator: bool,
    emoticon: bool,
    safe: bool,
    common_cjk: bool,
    unaccented: String,
}

fn python_predicate(value: &Bound<'_, PyString>, method: &str) -> PyResult<bool> {
    value.call_method0(method)?.extract()
}

fn char_info(
    py: Python<'_>,
    character: char,
    safe_ascii: &HashSet<char>,
    common_cjk: &HashSet<char>,
) -> PyResult<CharInfo> {
    let text = character.to_string();
    let ascii = character.is_ascii();
    let py_char = PyString::new(py, &text);
    let printable = python_predicate(&py_char, "isprintable")?;
    let alpha = python_predicate(&py_char, "isalpha")?;
    let upper = python_predicate(&py_char, "isupper")?;
    let lower = python_predicate(&py_char, "islower")?;
    let space = python_predicate(&py_char, "isspace")?;
    let digit = python_predicate(&py_char, "isdigit")?;
    let flags = if ascii {
        if character.is_ascii_alphabetic() {
            LATIN
        } else {
            0
        }
    } else {
        character_flags(py, &text)?
    };
    let category: String = py
        .import("unicodedata")?
        .getattr("category")?
        .call1((&text,))?
        .extract()?;
    let range = unicode_range(py, &text)?;
    let punct = printable
        && (category.contains('P')
            || range
                .as_deref()
                .is_some_and(|name| name.contains("Punctuation")));
    let symbol = printable
        && (category.contains('S')
            || (!ascii
                && (category.contains('N')
                    || (range.as_deref().is_some_and(|name| name.contains("Forms"))
                        && category != "Lo"))));
    let separator = space
        || matches!(character, '｜' | '+' | '<' | '>')
        || category.contains('Z')
        || matches!(category.as_str(), "Po" | "Pd" | "Pc");
    let accentuated = flags & ACCENTUATED != 0;
    let latin = flags & LATIN != 0;
    let cjk = flags & CJK != 0;
    let utils = py.import("charset_normalizer.utils")?;
    let emoticon = if alpha {
        false
    } else {
        utils.getattr("is_emoticon")?.call1((&text,))?.extract()?
    };
    let unaccented = if latin && accentuated {
        utils.getattr("remove_accent")?.call1((&text,))?.extract()?
    } else {
        text.clone()
    };

    Ok(CharInfo {
        printable,
        alpha,
        upper,
        lower,
        space,
        digit,
        ascii,
        case_variable: lower != upper,
        flags,
        accentuated,
        latin,
        cjk,
        katakana: flags & KATAKANA != 0,
        halfwidth_katakana: flags & HALFWIDTH_KATAKANA != 0,
        arabic: flags & ARABIC != 0,
        ligature: flags & LIGATURE != 0,
        superscript: flags & SUPERSCRIPT != 0,
        sentence_open_punctuation: flags & SENTENCE_OPEN_PUNCTUATION != 0,
        glyph: flags & GLYPH_MASK != 0,
        punct,
        symbol,
        range,
        separator,
        emoticon,
        safe: ascii && safe_ascii.contains(&character),
        common_cjk: cjk && common_cjk.contains(&character),
        unaccented,
    })
}

#[derive(Default)]
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
    last_latin: Option<(bool, bool, String)>,
    suspicious_ranges: usize,
    range_count: usize,
    last_range: Option<Option<String>>,
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

    fn feed_always(&mut self, ch: char, i: &CharInfo) {
        if ch == '\u{1b}' {
            self.has_escape = true;
        }
        if !i.printable && !i.space && ch != '\u{1a}' && ch != '\u{feff}' {
            self.unprintable += 1;
        }
        self.all_count += 1;
        self.feed_word(ch, i);
    }

    fn feed_printable(&mut self, ch: char, i: &CharInfo, py: Python<'_>) -> PyResult<()> {
        self.printable_count += 1;
        if self.last_printable != Some(ch) && !i.safe {
            if i.punct {
                self.punctuation += 1;
            } else if !i.digit && i.symbol && !i.emoticon {
                self.symbols += 2;
            }
        }
        self.last_printable = Some(ch);

        self.range_count += 1;
        if i.space || i.punct || i.safe {
            self.last_range = None;
            return Ok(());
        }
        if let Some(previous) = &self.last_range {
            if previous != &i.range || previous.is_none() {
                let suspicious =
                    suspicious_ranges_impl(py, previous.as_deref(), i.range.as_deref())?;
                if suspicious {
                    self.suspicious_ranges += 1;
                }
            }
        }
        self.last_range = Some(i.range.clone());
        Ok(())
    }

    fn feed_alpha(&mut self, i: &CharInfo) {
        self.alpha_count += 1;
        if i.accentuated {
            self.accents += 1;
        }
        if i.latin {
            self.latin_count += 1;
            if let Some((upper, accent, unaccented)) = &self.last_latin {
                if i.accentuated && *accent {
                    if i.upper && *upper {
                        self.duplicate_count += 1;
                    }
                    if i.unaccented == *unaccented {
                        self.duplicate_count += 1;
                    }
                }
            }
            self.last_latin = Some((i.upper, i.accentuated, i.unaccented.clone()));
        }
        if i.cjk {
            self.cjk_count += 1;
            if !i.common_cjk {
                self.uncommon_cjk += 1;
            }
        }
        if i.cjk || i.katakana {
            if i.katakana {
                self.katakana_count += 1;
                if i.halfwidth_katakana {
                    self.halfwidth_katakana += 1;
                }
            } else {
                self.katakana_cjk_count += 1;
                if !i.common_cjk {
                    self.katakana_uncommon_cjk += 1;
                }
            }
        }
        if i.arabic {
            self.arabic_count += 1;
            if i.flags & ARABIC_ISOLATED_FORM != 0 {
                self.isolated_arabic += 1;
            }
        }
    }

    fn feed_archaic(&mut self, i: &CharInfo) {
        let concerned = i.alpha && i.case_variable;
        if !concerned && self.archaic_chunk_count > 0 {
            if self.archaic_chunk_count <= 64 && !i.digit && !self.archaic_ascii_only {
                self.archaic_final += self.archaic_current;
            }
            self.archaic_current = 0;
            self.archaic_chunk_count = 0;
            self.archaic_buf = false;
            self.archaic_count += 1;
            self.archaic_ascii_only = true;
            return;
        }
        if self.archaic_ascii_only && !i.ascii {
            self.archaic_ascii_only = false;
        }
        if self.archaic_chunk_count > 0 {
            if (i.upper && self.archaic_last_lower) || (i.lower && self.archaic_last_upper) {
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
        self.archaic_last_upper = i.upper;
        self.archaic_last_lower = i.lower;
    }

    fn feed_word(&mut self, ch: char, i: &CharInfo) {
        if i.alpha {
            if self.buffer_last_ligature {
                self.buffer_internal_ligature = true;
            }
            self.buffer_last_ligature = i.ligature;
            if self.buffer_length == 0 {
                self.buffer_first_lower = i.lower;
            }
            self.buffer_length += 1;
            self.buffer_last_upper = i.upper;
            if i.upper {
                self.buffer_uppers += 1;
            }
            if !i.ascii {
                self.buffer_non_ascii = true;
            }
            self.buffer_last_accent = i.accentuated;
            if i.accentuated {
                self.buffer_accents += 1;
            }
            if i.glyph {
                self.buffer_glyphs += 1;
            } else if !self.foreign_watch && (!i.latin || i.accentuated) {
                self.foreign_watch = true;
            }
            return;
        }
        if self.buffer_length == 0 {
            return;
        }
        if i.sentence_open_punctuation || (i.superscript && self.buffer_internal_ligature) {
            self.current_bad = true;
            self.current_invalid = true;
        }
        if i.space || i.punct || i.separator {
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
        } else if !matches!(ch, '<' | '>' | '-' | '=' | '~' | '|' | '_') && !i.digit && i.symbol {
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
            if r >= 0.3 {
                r
            } else {
                0.0
            }
        };
        let ta = if self.alpha_count < 8 {
            0.0
        } else {
            let r = self.accents as f64 / self.alpha_count as f64;
            if r >= 0.35 {
                r
            } else {
                0.0
            }
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

#[pyfunction(signature = (decoded_sequence, maximum_threshold=0.2, debug=false))]
pub(crate) fn mess_ratio(
    py: Python<'_>,
    decoded_sequence: &str,
    maximum_threshold: f64,
    debug: bool,
) -> PyResult<f64> {
    let constants = constants(py)?;
    let safe_ascii: HashSet<char> = constants
        .getattr("COMMON_SAFE_ASCII_CHARACTERS")?
        .extract::<HashSet<String>>()?
        .into_iter()
        .filter_map(|s| s.chars().next())
        .collect();
    let common_cjk: HashSet<char> = constants
        .getattr("COMMON_CJK_CHARACTERS")?
        .extract::<HashSet<String>>()?
        .into_iter()
        .filter_map(|s| s.chars().next())
        .collect();
    let chars: Vec<char> = decoded_sequence.chars().collect();
    let step = if chars.len() < 511 {
        32
    } else if chars.len() < 1024 {
        64
    } else {
        128
    };
    let pure_ascii = decoded_sequence.is_ascii();
    let mut cache = HashMap::<char, CharInfo>::new();
    let mut detectors = Detectors::new();
    let mut mean = 0.0;
    let mut completed = true;
    for block in chars.chunks(step) {
        for &ch in block {
            if let std::collections::hash_map::Entry::Vacant(entry) = cache.entry(ch) {
                entry.insert(char_info(py, ch, &safe_ascii, &common_cjk)?);
            }
            let info = &cache[&ch];
            detectors.feed_always(ch, info);
            if pure_ascii {
                if info.printable {
                    detectors.feed_printable(ch, info, py)?;
                }
                continue;
            }
            detectors.feed_archaic(info);
            if info.printable {
                detectors.feed_printable(ch, info, py)?;
            }
            if info.alpha {
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
        let newline = char_info(py, '\n', &safe_ascii, &common_cjk)?;
        detectors.feed_word('\n', &newline);
        if !pure_ascii {
            detectors.feed_archaic(&newline);
        }
        if !newline.printable && !newline.space {
            detectors.unprintable += 1;
        }
        detectors.all_count += 1;
        mean = detectors.ratios().iter().sum();
    }
    if debug {
        let logger = py
            .import("logging")?
            .getattr("getLogger")?
            .call1(("charset_normalizer",))?;
        let trace = constants.getattr("TRACE")?;
        logger.call_method1("log", (trace.clone(), format!("Mess-detector extended-analysis start. intermediary_mean_mess_ratio_calc={step} mean_mess_ratio={mean:?} maximum_threshold={maximum_threshold:?}")))?;
        if chars.len() > 16 {
            let start: String = chars.iter().take(16).collect();
            let end: String = chars[chars.len() - 16..].iter().collect();
            logger.call_method1("log", (trace.clone(), format!("Starting with: {start}")))?;
            logger.call_method1("log", (trace.clone(), format!("Ending with: {end}")))?;
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
            logger.call_method1(
                "log",
                (
                    trace.clone(),
                    format!("<class 'charset_normalizer.md.{name}'>: {ratio:?}"),
                ),
            )?;
        }
    }
    py.import("builtins")?
        .getattr("round")?
        .call1((mean, 3))?
        .extract()
}

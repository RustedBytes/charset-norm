from __future__ import annotations

from functools import lru_cache

from . import _native
from .constant import (
    _ACCENTUATED,
    _ARABIC,
    _CJK,
    _HALFWIDTH_KATAKANA,
    _HANGUL,
    _HIRAGANA,
    _KATAKANA,
    _LATIN,
    _LIGATURE,
    _SENTENCE_OPEN_PUNCTUATION,
    _SUPERSCRIPT,
    _THAI,
    COMMON_CJK_CHARACTERS,
    COMMON_SAFE_ASCII_CHARACTERS,
)
from .utils import (
    _character_flags,
    is_emoticon,
    is_punctuation,
    is_separator,
    is_symbol,
    remove_accent,
    unicode_range,
)

# Combined bitmask for CJK/Hangul/Katakana/Hiragana/Thai glyph detection.
_GLYPH_MASK: int = _CJK | _HANGUL | _KATAKANA | _HIRAGANA | _THAI


class CharInfo:
    """Pre-computed character properties shared across all detectors."""

    __slots__ = (
        "accentuated",
        "alpha",
        "case_variable",
        "character",
        "common_cjk",
        "digit",
        "emoticon",
        "flags",
        "is_arabic",
        "is_ascii",
        "is_cjk",
        "is_glyph",
        "is_halfwidth_katakana",
        "is_katakana",
        "is_ligature",
        "is_sentence_open_punctuation",
        "is_superscript",
        "latin",
        "lower",
        "printable",
        "punct",
        "range",
        "safe",
        "sep",
        "space",
        "sym",
        "unaccented",
        "upper",
    )

    character: str
    printable: bool
    alpha: bool
    upper: bool
    lower: bool
    space: bool
    digit: bool
    is_ascii: bool
    case_variable: bool
    flags: int
    accentuated: bool
    latin: bool
    is_cjk: bool
    is_katakana: bool
    is_halfwidth_katakana: bool
    is_arabic: bool
    is_ligature: bool
    is_superscript: bool
    is_sentence_open_punctuation: bool
    is_glyph: bool
    punct: bool
    sym: bool
    range: str | None
    sep: bool
    emoticon: bool
    safe: bool
    common_cjk: bool
    unaccented: str

    def __init__(self, character: str) -> None:
        """Compute all properties for *character* (built once per codepoint,
        every branch assigns every slot)."""
        self.character = character

        # ASCII fast-path: for characters with ord < 128, we can skip
        # _character_flags() entirely and derive most properties from ord.
        o: int = ord(character)
        if o < 128:
            self.is_ascii = True
            self.accentuated = False
            self.unaccented = character
            self.emoticon = False
            self.common_cjk = False
            self.safe = character in COMMON_SAFE_ASCII_CHARACTERS
            self.is_cjk = False
            self.is_katakana = False
            self.is_halfwidth_katakana = False
            self.is_arabic = False
            self.is_ligature = False
            self.is_superscript = False
            self.is_sentence_open_punctuation = False
            self.is_glyph = False
            # ASCII alpha: a-z (97-122) or A-Z (65-90)
            if 65 <= o <= 90:
                # Uppercase ASCII letter
                self.alpha = True
                self.upper = True
                self.lower = False
                self.space = False
                self.digit = False
                self.printable = True
                self.case_variable = True
                self.flags = _LATIN
                self.latin = True
                self.punct = False
                self.sym = False
            elif 97 <= o <= 122:
                # Lowercase ASCII letter
                self.alpha = True
                self.upper = False
                self.lower = True
                self.space = False
                self.digit = False
                self.printable = True
                self.case_variable = True
                self.flags = _LATIN
                self.latin = True
                self.punct = False
                self.sym = False
            elif 48 <= o <= 57:
                # ASCII digit 0-9
                self.alpha = False
                self.upper = False
                self.lower = False
                self.space = False
                self.digit = True
                self.printable = True
                self.case_variable = False
                self.flags = 0
                self.latin = False
                self.punct = False
                self.sym = False
            elif o == 32 or (9 <= o <= 13):
                # Space, tab, newline, etc.
                self.alpha = False
                self.upper = False
                self.lower = False
                self.space = True
                self.digit = False
                self.printable = o == 32
                self.case_variable = False
                self.flags = 0
                self.latin = False
                self.punct = False
                self.sym = False
            else:
                # Other ASCII (punctuation, symbols, control chars)
                self.printable = character.isprintable()
                self.alpha = False
                self.upper = False
                self.lower = False
                self.space = False
                self.digit = False
                self.case_variable = False
                self.flags = 0
                self.latin = False
                self.punct = is_punctuation(character) if self.printable else False
                self.sym = is_symbol(character) if self.printable else False
        else:
            # Non-ASCII path
            self.is_ascii = False
            self.safe = False
            self.printable = character.isprintable()
            self.alpha = character.isalpha()
            self.upper = character.isupper()
            self.lower = character.islower()
            self.space = character.isspace()
            self.digit = character.isdigit()
            self.case_variable = self.lower != self.upper

            # Flag-based classification (single unicodedata.name() call, lru-cached)
            flags: int = _character_flags(character)
            if self.alpha:
                self.emoticon = False
            else:
                self.emoticon = is_emoticon(character)
            self.flags = flags
            self.accentuated = bool(flags & _ACCENTUATED)
            self.latin = bool(flags & _LATIN)
            self.is_cjk = bool(flags & _CJK)
            self.is_katakana = bool(flags & _KATAKANA)
            self.is_halfwidth_katakana = bool(flags & _HALFWIDTH_KATAKANA)
            self.is_arabic = bool(flags & _ARABIC)
            self.is_ligature = bool(flags & _LIGATURE)
            self.is_superscript = bool(flags & _SUPERSCRIPT)
            self.is_sentence_open_punctuation = bool(flags & _SENTENCE_OPEN_PUNCTUATION)
            self.is_glyph = bool(flags & _GLYPH_MASK)

            if self.latin and self.accentuated:
                self.unaccented = remove_accent(character)
            else:
                self.unaccented = character

            self.common_cjk = self.is_cjk and character in COMMON_CJK_CHARACTERS

            # Eagerly compute punct and sym (avoids property dispatch overhead
            # on 300K+ accesses in the hot loop).
            if self.printable:
                self.punct = is_punctuation(character)
                self.sym = is_symbol(character)
            else:
                self.punct = False
                self.sym = False

        self.range = unicode_range(character)
        self.sep = is_separator(character)


# Per-codepoint cache of CharInfo instances
# At most UTF-8 size allocated.
@lru_cache(maxsize=None)
def _char_info(character: str) -> CharInfo:
    """Build (once per codepoint) and cache the CharInfo for *character*."""
    return CharInfo(character)


@lru_cache(maxsize=None)
def is_suspiciously_successive_range(
    unicode_range_a: str | None, unicode_range_b: str | None
) -> bool:
    """
    Determine if two Unicode ranges seen next to each other can be considered suspicious.
    """
    return _native.is_suspiciously_successive_range(unicode_range_a, unicode_range_b)


def mess_ratio(
    decoded_sequence: str, maximum_threshold: float = 0.2, debug: bool = False
) -> float:
    """
    Compute a mess ratio given a decoded bytes sequence. The maximum threshold does stop the computation earlier.
    """
    return _native.mess_ratio(decoded_sequence, maximum_threshold, debug)

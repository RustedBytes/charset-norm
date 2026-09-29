from __future__ import annotations

import logging
from functools import lru_cache
from typing import Generator

from . import _native
from .constant import (
    COMMON_CJK_CHARACTERS,
    _ACCENTUATED,
    _ARABIC,
    _ARABIC_ISOLATED_FORM,
    _CJK,
    _HANGUL,
    _HIRAGANA,
    _KATAKANA,
    _LATIN,
    _SECONDARY_RANGE_NAMES,
    _THAI,
)


def _character_flags(character: str) -> int:
    return _native.character_flags(character)


def is_accentuated(character: str) -> bool:
    return bool(_character_flags(character) & _ACCENTUATED)


def remove_accent(character: str) -> str:
    return _native.remove_accent(character)


def unicode_range(character: str) -> str | None:
    return _native.unicode_range(character)


def is_latin(character: str) -> bool:
    return bool(_character_flags(character) & _LATIN)


def is_punctuation(character: str) -> bool:
    return _native.is_punctuation(character)


def is_symbol(character: str) -> bool:
    return _native.is_symbol(character)


def is_emoticon(character: str) -> bool:
    return _native.is_emoticon(character)


def is_separator(character: str) -> bool:
    return _native.is_separator(character)


def is_case_variable(character: str) -> bool:
    return _native.is_case_variable(character)


def is_cjk(character: str) -> bool:
    return bool(_character_flags(character) & _CJK)


def is_hiragana(character: str) -> bool:
    return bool(_character_flags(character) & _HIRAGANA)


def is_katakana(character: str) -> bool:
    return bool(_character_flags(character) & _KATAKANA)


def is_hangul(character: str) -> bool:
    return bool(_character_flags(character) & _HANGUL)


def is_thai(character: str) -> bool:
    return bool(_character_flags(character) & _THAI)


def is_arabic(character: str) -> bool:
    return bool(_character_flags(character) & _ARABIC)


def is_arabic_isolated_form(character: str) -> bool:
    return bool(_character_flags(character) & _ARABIC_ISOLATED_FORM)


def is_cjk_uncommon(character: str) -> bool:
    return character not in COMMON_CJK_CHARACTERS


def is_unicode_range_secondary(range_name: str) -> bool:
    return range_name in _SECONDARY_RANGE_NAMES


def is_unprintable(character: str) -> bool:
    return _native.is_unprintable(character)


def any_specified_encoding(
    sequence: bytes | bytearray, search_zone: int = 8192
) -> str | None:
    return _native.any_specified_encoding(sequence, search_zone)


@lru_cache(maxsize=None)
def is_multi_byte_encoding(name: str) -> bool:
    return _native.is_multi_byte_encoding(name)


def identify_sig_or_bom(sequence: bytes | bytearray) -> tuple[str | None, bytes]:
    return _native.identify_sig_or_bom(sequence)


def should_strip_sig_or_bom(iana_encoding: str) -> bool:
    return _native.should_strip_sig_or_bom(iana_encoding)


def iana_name(cp_name: str, strict: bool = True) -> str:
    return _native.iana_name(cp_name, strict)


def cp_similarity(iana_name_a: str, iana_name_b: str) -> float:
    return _native.cp_similarity(iana_name_a, iana_name_b)


def is_cp_similar(iana_name_a: str, iana_name_b: str) -> bool:
    return _native.is_cp_similar(iana_name_a, iana_name_b)


def set_logging_handler(
    name: str = "charset_normalizer",
    level: int = logging.INFO,
    format_string: str = "%(asctime)s | %(levelname)s | %(message)s",
) -> None:
    logger = logging.getLogger(name)
    logger.setLevel(level)
    handler = logging.StreamHandler()
    handler.setFormatter(logging.Formatter(format_string))
    logger.addHandler(handler)


def cut_sequence_chunks(
    sequences: bytes | bytearray,
    encoding_iana: str,
    offsets: range,
    chunk_size: int,
    bom_or_sig_available: bool,
    strip_sig_or_bom: bool,
    sig_payload: bytes,
    is_multi_byte_decoder: bool,
    decoded_payload: str | None = None,
    deferred_decoding: bool = False,
) -> Generator[str, None, None]:
    yield from _native.cut_sequence_chunks(
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

from __future__ import annotations

from functools import lru_cache

from . import _native
from .constant import LANGUAGE_SUPPORTED_COUNT
from .models import CoherenceMatches

# Explicit re-export (PEP 484 alias form).
from .utils import is_multi_byte_encoding as is_multi_byte_encoding  # noqa: PLC0414


def encoding_unicode_range(iana_name: str) -> list[str]:
    return _native.encoding_unicode_range(iana_name)


def unicode_range_languages(primary_range: str) -> list[str]:
    return _native.unicode_range_languages(primary_range)


@lru_cache
def encoding_languages(iana_name: str) -> list[str]:
    return _native.encoding_languages(iana_name)


@lru_cache
def mb_encoding_languages(iana_name: str) -> list[str]:
    return _native.mb_encoding_languages(iana_name)


@lru_cache(maxsize=LANGUAGE_SUPPORTED_COUNT)
def get_target_features(language: str) -> tuple[bool, bool]:
    return _native.get_target_features(language)


def alphabet_languages(
    characters: list[str], ignore_non_latin: bool = False
) -> list[str]:
    return _native.alphabet_languages(characters, ignore_non_latin)


def characters_popularity_compare(
    language: str, ordered_characters: list[str]
) -> float:
    return _native.characters_popularity_compare(language, ordered_characters)


def alpha_unicode_split(decoded_sequence: str) -> list[str]:
    return _native.alpha_unicode_split(decoded_sequence)


def merge_coherence_ratios(results: list[CoherenceMatches]) -> CoherenceMatches:
    return _native.merge_coherence_ratios(results)


def filter_alt_coherence_matches(results: CoherenceMatches) -> CoherenceMatches:
    return _native.filter_alt_coherence_matches(results)


def coherence_ratio(
    decoded_sequence: str, threshold: float = 0.1, lg_inclusion: str | None = None
) -> CoherenceMatches:
    return _native.coherence_ratio(decoded_sequence, threshold, lg_inclusion)

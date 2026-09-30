from __future__ import annotations

import pytest

from charset_norm.constant import IANA_SUPPORTED, CompatibleFamillyRange
from charset_norm.utils import (
    cp_similarity,
    cut_sequence_chunks,
    iana_name,
    is_accentuated,
    is_arabic,
    is_arabic_isolated_form,
    is_case_variable,
    is_cjk,
    is_cjk_uncommon,
    is_hangul,
    is_hiragana,
    is_katakana,
    is_latin,
    is_thai,
    is_unicode_range_secondary,
)


@pytest.mark.parametrize(
    "character, expected_is_accentuated",
    [
        ("é", True),
        ("è", True),
        ("à", True),
        ("À", True),
        ("Ù", True),
        ("ç", True),
        ("a", False),
        ("€", False),
        ("&", False),
        ("Ö", True),
        ("ü", True),
        ("ê", True),
        ("Ñ", True),
        ("Ý", True),
        ("Ω", False),
        ("ø", False),
        ("Ё", False),
    ],
)
def test_is_accentuated(character, expected_is_accentuated):
    assert is_accentuated(character) is expected_is_accentuated, (
        "is_accentuated behavior incomplete"
    )


@pytest.mark.parametrize(
    "cp_name_a, cp_name_b, expected_is_similar",
    [
        ("cp1026", "cp1140", True),
        ("cp1140", "cp1026", True),
        ("latin_1", "cp1252", True),
        ("latin_1", "iso8859_4", True),
        ("latin_1", "cp1251", False),
        ("cp1251", "mac_turkish", False),
    ],
)
def test_cp_similarity(cp_name_a, cp_name_b, expected_is_similar):
    is_similar = cp_similarity(cp_name_a, cp_name_b) >= 0.8

    assert is_similar is expected_is_similar, "cp_similarity is broken"


@pytest.mark.parametrize("cp_name", IANA_SUPPORTED)
def test_iana_name_resolves_every_supported_encoding(cp_name):
    assert iana_name(cp_name) == cp_name, "IANA_SUPPORTED entry is unresolvable"


@pytest.mark.parametrize(
    "predicate, matching, other",
    [
        (is_latin, "a", "я"),
        (is_case_variable, "A", "1"),
        (is_cjk, "中", "a"),
        (is_hiragana, "あ", "ア"),
        (is_katakana, "ア", "あ"),
        (is_hangul, "한", "中"),
        (is_thai, "ก", "a"),
        (is_arabic, "ع", "a"),
        (is_arabic_isolated_form, "\ufe8d", "ع"),
        (is_cjk_uncommon, "黵", "的"),
    ],
)
def test_character_predicates(predicate, matching, other):
    assert predicate(matching) is True
    assert predicate(other) is False


def test_is_unicode_range_secondary():
    assert is_unicode_range_secondary("Latin-1 Supplement") is True
    assert is_unicode_range_secondary("Basic Latin") is False


def test_cut_sequence_chunks():
    payload = b"abcdefghij" * 10
    chunks = list(
        cut_sequence_chunks(
            payload, "ascii", range(0, 100, 25), 25, False, False, b"", False
        )
    )
    assert chunks == ["abcdefghij" * 2 + "abcde", "fghij" + "abcdefghij" * 2] * 2


def test_compatible_family_range_is_unordered():
    pair = CompatibleFamillyRange("Latin", "Greek")
    assert pair == CompatibleFamillyRange("Greek", "Latin")
    assert hash(pair) == hash(CompatibleFamillyRange("Greek", "Latin"))
    assert pair != CompatibleFamillyRange("Latin", "Cyrillic")
    assert pair != ("Latin", "Greek")

from __future__ import annotations

import inspect

from charset_normalizer import _native
from charset_normalizer.cd import (
    alpha_unicode_split,
    alphabet_languages,
    characters_popularity_compare,
    coherence_ratio,
    filter_alt_coherence_matches,
    get_target_features,
    mb_encoding_languages,
    merge_coherence_ratios,
)
from charset_normalizer.md import mess_ratio
from charset_normalizer.models import CharsetMatch, CharsetMatches, CliDetectionResult
from charset_normalizer.utils import (
    any_specified_encoding,
    iana_name,
    identify_sig_or_bom,
    is_cp_similar,
    is_emoticon,
    is_punctuation,
    is_separator,
    is_symbol,
    is_unprintable,
    remove_accent,
    should_strip_sig_or_bom,
)


def test_native_backend_is_required() -> None:
    assert _native.backend_name() == "rust-pyo3"
    assert _native.__version__ == "3.5.1"
    best = _native.from_bytes(b"plain ASCII").best()
    assert best is not None
    assert best.encoding == "ascii"


def test_python_facades_keep_their_signatures() -> None:
    assert str(inspect.signature(iana_name)) == (
        "(cp_name: 'str', strict: 'bool' = True) -> 'str'"
    )
    assert str(inspect.signature(alphabet_languages)) == (
        "(characters: 'list[str]', ignore_non_latin: 'bool' = False) -> 'list[str]'"
    )


def test_native_utils_contract() -> None:
    assert iana_name("UTF-8") == "utf_8"
    assert iana_name("not-real", False) == "not_real"
    assert identify_sig_or_bom(b"\xef\xbb\xbfhello") == ("utf_8", b"\xef\xbb\xbf")
    assert should_strip_sig_or_bom("utf_8") is True
    assert should_strip_sig_or_bom("utf_16") is False
    assert is_cp_similar("latin_1", "cp1252") is True


def test_native_coherence_helpers_contract() -> None:
    assert mb_encoding_languages("shift_jis") == ["Japanese"]
    assert get_target_features("English") == (False, True)
    assert "English" in alphabet_languages(list("etaoinshrdlu"), True)
    assert characters_popularity_compare("English", ["e", "e", "t", "a"]) == 0.25
    assert merge_coherence_ratios(
        [[("English", 0.2)], [("English", 0.4), ("French", 0.5)]]
    ) == [("French", 0.5), ("English", 0.3)]
    assert filter_alt_coherence_matches([("English", 0.8), ("English—", 0.9)]) == [
        ("English", 0.9)
    ]


def test_native_character_and_detection_contract() -> None:
    assert remove_accent("é") == "e"
    assert is_punctuation("!") is True
    assert is_symbol("²") is True
    assert is_emoticon("😀") is True
    assert is_separator("+") is True
    assert is_unprintable("\x00") is True
    assert any_specified_encoding(b'<meta charset="windows-1252">') == "cp1252"
    assert alpha_unicode_split("Hello العربية") == ["hello", "العربية"]
    assert coherence_ratio(
        "This is a sufficiently long English sentence for detection."
    )
    assert mess_ratio("plain text") == 0.0


def test_native_result_models_keep_python_contract() -> None:
    match = CharsetMatch(b"caf\xe9", "latin_1", 0.1, False, [("French", 0.8)])
    assert type(match).__module__ == "charset_normalizer.models"
    assert str(match) == "café"
    assert match.language == "French"
    assert match.output() == "café".encode()

    matches = CharsetMatches([match])
    assert matches.best() is match
    assert matches["latin-1"] is match

    cli_result = CliDetectionResult(
        "sample.txt",
        "latin_1",
        [],
        [],
        "French",
        ["Basic Latin"],
        False,
        10.0,
        80.0,
        None,
        True,
    )
    assert '"encoding": "latin_1"' in cli_result.to_json()

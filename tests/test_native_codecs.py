from __future__ import annotations

import random
import subprocess
import sys
from glob import glob
from os.path import dirname, join, realpath

import pytest

from charset_norm import _native
from charset_norm.constant import IANA_SUPPORTED

DIR_PATH = join(dirname(realpath(__file__)), "..", "data")

SAMPLE_TEXTS = [
    "Hello, world! ~{ }~ +- \\ ¥",
    "Всеки човек има право на образование. Ελληνικά κείμενα.",
    "中文测试，繁體字 テスト ｶﾀｶﾅ 한국어 텍스트",
    "Ça va très bien, merci — «guillemets» ÀÉÎõü",
]


def _native_decode(payload: bytes, encoding: str, ignore: bool) -> str | None:
    try:
        chunks = _native.cut_sequence_chunks(
            payload,
            encoding,
            [0],
            len(payload),
            False,
            False,
            b"",
            ignore,
            None,
            not ignore,
        )
    except UnicodeDecodeError:
        return None
    return chunks[0] if chunks else ""


def _python_decode(payload: bytes, encoding: str, ignore: bool) -> str | None:
    try:
        return payload.decode(encoding, "ignore" if ignore else "strict")
    except UnicodeDecodeError:
        return None


def _payloads(encoding: str) -> list[bytes]:
    rng = random.Random(encoding)
    payloads = [text.encode(encoding, "ignore") for text in SAMPLE_TEXTS]
    for path in sorted(glob(join(DIR_PATH, "*.txt"))):
        with open(path, "rb") as fp:
            payloads.append(fp.read()[:2048])
    payloads.extend(
        bytes(rng.randrange(256) for _ in range(rng.randrange(1, 48)))
        for _ in range(64)
    )
    return payloads


@pytest.mark.parametrize("encoding", sorted(set(IANA_SUPPORTED)))
def test_native_decoding_matches_cpython(encoding: str) -> None:
    try:
        _native.cut_sequence_chunks(b"a", encoding, [0], 1, False, False, b"", False)
    except UnicodeDecodeError:
        pass
    except LookupError:
        pytest.skip(f"{encoding} has no native codec")

    modes = (False, True) if _native.is_multi_byte_encoding(encoding) else (False,)
    for payload in _payloads(encoding):
        for ignore in modes:
            expected = _python_decode(payload, encoding, ignore)
            if expected is not None and any(
                0xD800 <= ord(c) < 0xE000 for c in expected
            ):
                continue  # lone surrogates cannot be represented natively
            assert _native_decode(payload, encoding, ignore) == expected, (
                encoding,
                ignore,
                payload[:40],
            )


def test_detection_uses_no_python_codecs_or_unicodedata() -> None:
    script = """
import sys
import charset_norm

payloads = [
    (text * 8).encode(encoding)
    for text, encoding in (
        ("Всеки човек има право на образование.", "cp1251"),
        ("中文测试，繁體字 这是一个用于检测的句子。", "gb18030"),
        ("日本語のテキストを検出するための文章です。", "shift_jis"),
        ("Ceci est un texte français accentué: élève, château.", "latin_1"),
    )
]
# Encoding the samples above loads those codecs; forget them so any codec
# module detection pulls in shows up below.
for name in list(sys.modules):
    if name.startswith(("encodings.", "_codecs_", "_multibytecodec")):
        if name not in ("encodings.aliases", "encodings.utf_8"):
            del sys.modules[name]

before = set(sys.modules)
for payload in payloads:
    best = charset_norm.from_bytes(payload).best()
    assert best is not None
    str(best)
loaded = sorted(
    name
    for name in set(sys.modules) - before
    if name.startswith(("encodings.", "_codecs_", "_multibytecodec"))
    or name == "unicodedata"
)
assert not loaded, loaded
"""
    subprocess.run([sys.executable, "-c", script], check=True)

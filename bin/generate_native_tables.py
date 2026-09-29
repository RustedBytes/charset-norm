"""
Generate the static tables used by the native (Rust) core.

The extension must not call back into Python at runtime, so every constant
and every codec it relies on is materialized here once, from the reference
implementation (``charset_normalizer.constant`` and CPython's codecs):

* ``rust/generated/constants.rs`` - detection constants, Unicode ranges,
  language frequencies, IANA aliases and single-byte code pages.
* ``rust/generated/cjk.bin`` - decoding tables for CPython's CJK codecs,
  obtained by exhaustively probing each codec.

Run from the repository root with the interpreter whose codecs are the
reference: ``python bin/generate_native_tables.py``.
"""

from __future__ import annotations

import codecs
import importlib.util
import struct
import sys
import unicodedata
from encodings.aliases import aliases
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "rust" / "generated"

# Load constant.py directly: importing the package would require the
# extension module we are generating tables for.
_spec = importlib.util.spec_from_file_location(
    "cn_constant", ROOT / "src" / "charset_normalizer" / "constant.py"
)
assert _spec is not None and _spec.loader is not None
constant = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(constant)

MULTI_BYTE_CJK = [
    "big5",
    "big5hkscs",
    "cp932",
    "cp949",
    "cp950",
    "euc_jis_2004",
    "euc_jisx0213",
    "euc_jp",
    "euc_kr",
    "gb18030",
    "gb2312",
    "gbk",
    "johab",
    "shift_jis",
    "shift_jis_2004",
    "shift_jisx0213",
]
STATEFUL_CJK = [
    "hz",
    "iso2022_jp",
    "iso2022_jp_1",
    "iso2022_jp_2",
    "iso2022_jp_2004",
    "iso2022_jp_3",
    "iso2022_jp_ext",
    "iso2022_kr",
]
UNICODE_MB = [
    "utf_8",
    "utf_8_sig",
    "utf_16",
    "utf_16_be",
    "utf_16_le",
    "utf_32",
    "utf_32_be",
    "utf_32_le",
    "utf_7",
]
MULTI_BYTE = set(MULTI_BYTE_CJK + STATEFUL_CJK + UNICODE_MB)

# Stateful ISO-2022 designations: (table name, codec, escape prefix, width).
ISO2022_CHARSETS = [
    ("jisx0208", "iso2022_jp", b"\x1b$B", 2),
    ("jisx0212", "iso2022_jp_1", b"\x1b$(D", 2),
    ("ksx1001", "iso2022_jp_2", b"\x1b$(C", 2),
    ("gb2312_7bit", "iso2022_jp_2", b"\x1b$A", 2),
    ("jisx0201_r", "iso2022_jp", b"\x1b(J", 1),
    ("jisx0201_k", "iso2022_jp_ext", b"\x1b(I", 1),
    ("jisx0213_2000_1", "iso2022_jp_3", b"\x1b$(O", 2),
    ("jisx0213_2000_2", "iso2022_jp_3", b"\x1b$(P", 2),
    ("jisx0213_2004_1", "iso2022_jp_2004", b"\x1b$(Q", 2),
    ("jisx0213_2004_2", "iso2022_jp_2004", b"\x1b$(P", 2),
]

NONE = 0xFFFFFF
PAIR_BASE = 0x110000
# Invalid sequence whose error spans more than one byte (CPython skips that
# many bytes under errors="ignore"): ERROR_BASE + length.
ERROR_BASE = 0xFFFFF0


def rust_str(value: str) -> str:
    out = []
    for ch in value:
        if ch in '\\"':
            out.append("\\" + ch)
        elif 0x20 <= ord(ch) < 0x7F:
            out.append(ch)
        else:
            out.append(f"\\u{{{ord(ch):x}}}")
    return '"' + "".join(out) + '"'


def rust_char(value: str) -> str:
    assert len(value) == 1
    return "'" + rust_str(value)[1:-1].replace("'", "\\'") + "'"


def try_decode(data: bytes, encoding: str) -> tuple[str | None, str | None]:
    decoded, error, _ = try_decode_span(data, encoding)
    return decoded, error


def try_decode_span(data: bytes, encoding: str) -> tuple[str | None, str | None, int]:
    """Decode strictly; on failure also report the length of the error."""
    try:
        return data.decode(encoding), None, 0
    except UnicodeDecodeError as error:
        incomplete = error.reason.startswith("incomplete") and error.end == len(data)
        return None, "incomplete" if incomplete else "illegal", error.end - error.start


class PairTable:
    def __init__(self) -> None:
        self.pairs: list[tuple[int, int]] = []
        self.index: dict[tuple[int, int], int] = {}

    def value(self, decoded: str) -> int:
        if len(decoded) == 1:
            return ord(decoded)
        assert len(decoded) == 2, decoded
        key = (ord(decoded[0]), ord(decoded[1]))
        if key not in self.index:
            self.index[key] = len(self.pairs)
            self.pairs.append(key)
        return PAIR_BASE + self.index[key]


def encode_rows(rows: dict[int, dict[int, int]]) -> bytes:
    """Serialize ``{lead: {trail: value}}`` as contiguous rows of u24 values."""
    blob = bytearray(struct.pack("<H", len(rows)))
    for lead in sorted(rows):
        trails = rows[lead]
        first, last = min(trails), max(trails)
        blob += struct.pack("<BBH", lead, first, last - first + 1)
        for trail in range(first, last + 1):
            blob += (trails.get(trail, NONE)).to_bytes(3, "little")
    return bytes(blob)


def probe_cjk(encoding: str, pairs: PairTable) -> bytes:
    """
    Probe a stateless CJK codec. Layout:
    ``need[256]`` (0 invalid, 1 single, 2 double, 3 triple),
    ``single[256]`` u24 values, the 2-byte rows, then the 3-byte prefix and rows.
    """
    need = [0] * 256
    single = [NONE] * 256
    double: dict[int, dict[int, int]] = {}
    triple: dict[int, dict[int, int]] = {}
    triple_prefix = 0

    for lead in range(256):
        decoded, error = try_decode(bytes([lead]), encoding)
        if decoded is not None:
            assert len(decoded) == 1
            need[lead] = 1
            single[lead] = ord(decoded)
            continue
        if error != "incomplete":
            continue
        need[lead] = 2
        if encoding == "gb18030":
            for trail in range(256):
                if 0x30 <= trail <= 0x39:
                    continue  # four-byte form, handled algorithmically
                decoded, _ = try_decode(bytes([lead, trail]), encoding)
                if decoded is not None:
                    double.setdefault(lead, {})[trail] = pairs.value(decoded)
            continue
        for trail in range(256):
            decoded, error, span = try_decode_span(bytes([lead, trail]), encoding)
            if decoded is not None:
                double.setdefault(lead, {})[trail] = pairs.value(decoded)
            elif error == "illegal" and span > 1:
                double.setdefault(lead, {})[trail] = ERROR_BASE + span
            elif error == "incomplete":
                if encoding == "euc_kr" and (lead, trail) == (0xA4, 0xD4):
                    continue  # KS X 1001 make-up sequence, decoded natively
                need[lead] = 3
                assert triple_prefix in (0, lead)
                triple_prefix = lead
                for third in range(256):
                    decoded, error, span = try_decode_span(
                        bytes([lead, trail, third]), encoding
                    )
                    if decoded is not None:
                        triple.setdefault(trail, {})[third] = pairs.value(decoded)
                    elif error == "illegal" and span > 1:
                        triple.setdefault(trail, {})[third] = ERROR_BASE + span

    for lead in range(256):
        assert not (need[lead] == 3 and lead in double), (encoding, lead)

    blob = bytearray(bytes(need))
    for value in single:
        blob += value.to_bytes(3, "little")
    blob += encode_rows(double)
    blob += bytes([triple_prefix])
    blob += encode_rows(triple)
    return bytes(blob)


def probe_iso2022(codec: str, prefix: bytes, width: int, pairs: PairTable) -> bytes:
    rows: dict[int, dict[int, int]] = {}
    for first in range(0x20, 0x80):
        if width == 1:
            decoded, _ = try_decode(prefix + bytes([first]), codec)
            if decoded is not None:
                rows.setdefault(0, {})[first] = pairs.value(decoded)
            continue
        if codec == "hz" and first == ord("~"):
            continue  # escape character, not a GB2312 row
        for second in range(256):
            decoded, _ = try_decode(prefix + bytes([first, second]), codec)
            if decoded is not None:
                rows.setdefault(first, {})[second] = pairs.value(decoded)
    return encode_rows(rows)


def gb18030_ranges() -> list[tuple[int, int]]:
    """Runs of (four-byte linear index, first code point) for the BMP area."""
    runs: list[tuple[int, int]] = []
    for index in range(39420):
        b1, rest = divmod(index, 12600)
        b2, rest = divmod(rest, 1260)
        b3, b4 = divmod(rest, 10)
        seq = bytes([0x81 + b1, 0x30 + b2, 0x81 + b3, 0x30 + b4])
        decoded = seq.decode("gb18030")
        assert len(decoded) == 1
        codepoint = ord(decoded)
        if not runs or runs[-1][1] - runs[-1][0] != codepoint - index:
            runs.append((index, codepoint))
    return runs


def single_byte_table(encoding: str) -> list[int]:
    table = []
    for byte in range(256):
        decoded, _ = try_decode(bytes([byte]), encoding)
        if decoded is None:
            table.append(0xFFFE)
        else:
            assert (
                len(decoded) == 1 and ord(decoded) < 0x10000 and ord(decoded) != 0xFFFE
            )
            table.append(ord(decoded))
    return table


def write_constants(single_byte: dict[str, list[int]]) -> None:
    lines = [
        "// @generated by bin/generate_native_tables.py -- do not edit.",
        f"// Reference interpreter: Python {sys.version.split()[0]}",
        "",
    ]
    emit = lines.append

    families = constant._RANGE_FAMILIES
    secondary = constant._SECONDARY_RANGE_NAMES
    ranges = sorted(
        constant.UNICODE_RANGES_COMBINED.items(), key=lambda item: item[1].start
    )
    emit("/// (start, stop, name, family, secondary)")
    emit("pub static UNICODE_RANGES: &[(u32, u32, &str, &str, bool)] = &[")
    for name, rng in ranges:
        emit(
            f"    ({rng.start:#x}, {rng.stop:#x}, {rust_str(name)}, "
            f"{rust_str(families[name])}, {'true' if name in secondary else 'false'}),"
        )
    emit("];")
    emit("")

    compatible = sorted(
        tuple(sorted(pair._families)) for pair in constant._COMPATIBLE_RANGE_FAMILIES
    )
    emit("/// Unordered compatible family pairs, each stored sorted.")
    emit("pub static COMPATIBLE_RANGE_FAMILIES: &[(&str, &str)] = &[")
    for a, b in compatible:
        emit(f"    ({rust_str(a)}, {rust_str(b)}),")
    emit("];")
    for name, values in (
        (
            "COMPATIBLE_WITH_ANY_RANGE_FAMILIES",
            constant._COMPATIBLE_WITH_ANY_RANGE_FAMILIES,
        ),
        (
            "BASIC_LATIN_COMPATIBLE_RANGE_FAMILIES",
            constant._BASIC_LATIN_COMPATIBLE_RANGE_FAMILIES,
        ),
        ("ACCENT_KEYWORDS", constant._ACCENT_KEYWORDS),
    ):
        items = values if isinstance(values, tuple) else sorted(values)
        emit(
            f"pub static {name}: &[&str] = &[{', '.join(rust_str(v) for v in items)}];"
        )
    emit("")

    emit("/// Language -> characters ordered from most to least frequent.")
    emit("pub static FREQUENCIES: &[(&str, &str)] = &[")
    for language, characters in constant.FREQUENCIES.items():
        assert all(len(c) == 1 for c in characters)
        emit(f"    ({rust_str(language)}, {rust_str(''.join(characters))}),")
    emit("];")
    emit("")

    emit(
        "pub static COMMON_SAFE_ASCII_CHARACTERS: &[char] = &["
        + ", ".join(rust_char(c) for c in sorted(constant.COMMON_SAFE_ASCII_CHARACTERS))
        + "];"
    )
    emit(
        "pub static COMMON_CJK_CHARACTERS: &[char] = &["
        + ", ".join(rust_char(c) for c in sorted(constant.COMMON_CJK_CHARACTERS))
        + "];"
    )
    digits = [
        cp
        for cp in range(0x110000)
        if chr(cp).isdigit() and unicodedata.category(chr(cp)) != "Nd"
    ]
    emit("/// Code points with Numeric_Type=Digit (str.isdigit() beyond category Nd).")
    emit(
        "pub static NON_DECIMAL_DIGITS: &[u32] = &["
        + ", ".join(f"{cp:#x}" for cp in digits)
        + "];"
    )
    emit("")

    emit(
        "pub static ZH_NAMES: &[&str] = &["
        + ", ".join(rust_str(v) for v in sorted(constant.ZH_NAMES))
        + "];"
    )
    emit(
        "pub static KO_NAMES: &[&str] = &["
        + ", ".join(rust_str(v) for v in sorted(constant.KO_NAMES))
        + "];"
    )
    emit(
        "pub static MULTI_BYTE_ENCODINGS: &[&str] = &["
        + ", ".join(rust_str(v) for v in sorted(MULTI_BYTE))
        + "];"
    )
    emit("")

    emit("/// Sorted normalized name -> canonical codec name (constant._IANA_NAMES).")
    emit("pub static IANA_NAMES: &[(&str, &str)] = &[")
    for key in sorted(constant._IANA_NAMES):
        emit(f"    ({rust_str(key)}, {rust_str(constant._IANA_NAMES[key])}),")
    emit("];")
    emit("")

    emit("/// encodings.aliases.aliases, in dictionary order.")
    emit("pub static ENCODING_ALIASES: &[(&str, &str)] = &[")
    for key, value in aliases.items():
        emit(f"    ({rust_str(key)}, {rust_str(value)}),")
    emit("];")
    emit("")

    supported = constant.IANA_SUPPORTED
    mb_first = sorted(supported, key=lambda encoding: encoding not in MULTI_BYTE)
    emit("/// IANA_SUPPORTED ordered multi-byte first (stable).")
    emit(
        "pub static IANA_SUPPORTED_MB_FIRST: &[&str] = &["
        + ", ".join(rust_str(v) for v in mb_first)
        + "];"
    )
    emit("pub static IANA_SUPPORTED_SIMILAR: &[(&str, &[&str])] = &[")
    for key in sorted(constant.IANA_SUPPORTED_SIMILAR):
        values = ", ".join(rust_str(v) for v in constant.IANA_SUPPORTED_SIMILAR[key])
        emit(f"    ({rust_str(key)}, &[{values}]),")
    emit("];")
    emit("")

    emit("/// Single-byte code pages; 0xFFFE marks an undefined byte.")
    emit("pub static SINGLE_BYTE_CODECS: &[(&str, [u16; 256])] = &[")
    for name in sorted(single_byte):
        table = single_byte[name]
        body = ", ".join(f"{v:#06x}" for v in table)
        emit(f"    ({rust_str(name)}, [{body}]),")
    emit("];")
    emit("")

    (OUT / "constants.rs").write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)

    candidates = set(constant.IANA_SUPPORTED) | {"latin_1", "ascii"}
    single_byte = {}
    for encoding in sorted(candidates):
        if encoding in MULTI_BYTE:
            continue
        try:
            codecs.lookup(encoding)
        except LookupError:
            continue
        single_byte[encoding] = single_byte_table(encoding)
    write_constants(single_byte)

    pairs = PairTable()
    sections: list[tuple[str, bytes]] = []
    for encoding in MULTI_BYTE_CJK:
        sections.append((encoding, probe_cjk(encoding, pairs)))
    for name, codec, prefix, width in ISO2022_CHARSETS:
        sections.append((name, probe_iso2022(codec, prefix, width, pairs)))
    # HZ and ISO-2022-KR reuse the ISO-2022 GB2312/KSX1001 tables; make sure.
    assert probe_iso2022("hz", b"~{", 2, pairs) == dict(sections)["gb2312_7bit"]
    assert (
        probe_iso2022("iso2022_kr", b"\x1b$)C\x0e", 2, pairs)
        == dict(sections)["ksx1001"]
    )

    runs = gb18030_ranges()
    ranges_blob = bytearray(struct.pack("<H", len(runs)))
    for index, codepoint in runs:
        ranges_blob += struct.pack("<II", index, codepoint)
    sections.append(("gb18030_ranges", bytes(ranges_blob)))

    pairs_blob = bytearray(struct.pack("<H", len(pairs.pairs)))
    for first, second in pairs.pairs:
        pairs_blob += struct.pack("<II", first, second)
    sections.append(("pairs", bytes(pairs_blob)))

    blob = bytearray(b"CNCJK1")
    blob += struct.pack("<H", len(sections))
    for name, data in sections:
        encoded = name.encode()
        blob += struct.pack("<B", len(encoded)) + encoded + struct.pack("<I", len(data))
        blob += data
    (OUT / "cjk.bin").write_bytes(bytes(blob))
    print(f"constants.rs and cjk.bin ({len(blob)} bytes) written to {OUT}")


if __name__ == "__main__":
    main()

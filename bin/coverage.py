from __future__ import annotations

import argparse
from codecs import lookup
from glob import glob
from os import sep
from os.path import isdir
from sys import argv

from chardet import __version__ as chardet_version
from chardet import detect as chardet_detect

from charset_normalizer import __version__, from_bytes
from charset_normalizer.utils import iana_name


def calc_equivalence(content: bytes, cp_a: str, cp_b: str):
    str_a = content.decode(cp_a)
    str_b = content.decode(cp_b)

    character_count = len(str_a)
    diff_character_count = sum(chr_a != chr_b for chr_a, chr_b in zip(str_a, str_b))

    return 1.0 - (diff_character_count / character_count)


def normalize_encoding(encoding: str) -> str:
    try:
        return iana_name(encoding)
    except ValueError:
        return lookup(encoding).name.replace("-", "_")


def evaluate_result(
    content: bytes, expected_encoding: str, detected_encoding: str | None
) -> tuple[bool, str]:
    if expected_encoding == "None":
        return (
            (True, "binary")
            if detected_encoding is None
            else (False, f"got '{detected_encoding}'")
        )

    if detected_encoding is None:
        return False, "nothing"

    normalized_detected = normalize_encoding(detected_encoding)
    normalized_expected = normalize_encoding(expected_encoding)
    if (
        detected_encoding == expected_encoding
        or normalized_detected == normalized_expected
        or {normalized_detected, normalized_expected} <= {"utf_8", "utf_8_sig"}
    ):
        return True, detected_encoding

    try:
        equivalence = calc_equivalence(content, expected_encoding, detected_encoding)
    except (LookupError, UnicodeError, ZeroDivisionError):
        return False, f"got '{detected_encoding}'"

    if equivalence >= 0.98:
        return (
            True,
            f"got '{detected_encoding}', equivalence {equivalence * 100.0:.3f} %",
        )

    return False, f"got '{detected_encoding}'"


def cli_coverage(arguments: list[str]):
    parser = argparse.ArgumentParser(
        description="Embedded detection success coverage script checker for Charset-Normalizer"
    )

    parser.add_argument(
        "-p",
        "--with-preemptive",
        action="store_true",
        default=False,
        dest="preemptive",
        help="Enable the preemptive scan behaviour during coverage check",
    )
    parser.add_argument(
        "-c",
        "--coverage",
        action="store",
        default=90,
        type=int,
        dest="coverage",
        help="Define the minimum acceptable coverage to succeed",
    )
    parser.add_argument(
        "-q",
        "--quiet",
        action="store_true",
        help="Only print detector versions and final coverage totals",
    )

    args = parser.parse_args(arguments)

    if not isdir("./char-dataset"):
        print(
            "This script require https://github.com/Ousret/char-dataset to be cloned on package root directory"
        )
        exit(1)

    print(f"> using charset-normalizer {__version__} and chardet {chardet_version}")

    charset_normalizer_success_count = 0
    chardet_success_count = 0
    total_count = 0

    for tbt_path in sorted(glob("./char-dataset/**/*.*")):
        expected_encoding = tbt_path.split(sep)[-2]
        total_count += 1

        with open(tbt_path, "rb") as fp:
            content = fp.read()

        matches = from_bytes(content, preemptive_behaviour=args.preemptive)
        best_match = matches.best()
        charset_normalizer_encoding = (
            best_match.encoding if best_match is not None else None
        )
        chardet_encoding = chardet_detect(content)["encoding"]

        charset_normalizer_success, charset_normalizer_detail = evaluate_result(
            content, expected_encoding, charset_normalizer_encoding
        )
        chardet_success, chardet_detail = evaluate_result(
            content, expected_encoding, chardet_encoding
        )

        charset_normalizer_success_count += charset_normalizer_success
        chardet_success_count += chardet_success
        charset_normalizer_mark = "✅" if charset_normalizer_success else "⚡"
        chardet_mark = "✅" if chardet_success else "⚡"
        if not args.quiet:
            print(
                f"{tbt_path}: CN {charset_normalizer_mark} "
                f"({charset_normalizer_detail}) | "
                f"Chardet {chardet_mark} ({chardet_detail})"
            )

    charset_normalizer_success_ratio = (
        charset_normalizer_success_count / total_count * 100.0
    )
    chardet_success_ratio = chardet_success_count / total_count * 100.0

    print(
        "Charset-Normalizer coverage = "
        f"{charset_normalizer_success_ratio:.3f} % "
        f"({charset_normalizer_success_count} / {total_count} files)"
    )
    print(
        f"Chardet coverage = {chardet_success_ratio:.3f} % "
        f"({chardet_success_count} / {total_count} files)"
    )

    return 0 if charset_normalizer_success_ratio >= args.coverage else 1


if __name__ == "__main__":
    exit(cli_coverage(argv[1:]))

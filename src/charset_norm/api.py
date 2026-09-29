from __future__ import annotations

import logging
from os import PathLike
from typing import BinaryIO, TextIO

from . import _native
from .constant import IANA_SUPPORTED, TRACE
from .models import CharsetMatches
from .utils import is_multi_byte_encoding

logger: logging.Logger = logging.getLogger("charset_norm")
explain_handler: logging.StreamHandler[TextIO] = logging.StreamHandler()
explain_handler.setFormatter(
    logging.Formatter("%(asctime)s | %(levelname)s | %(message)s")
)

# Kept as a public compatibility constant. The native detector applies this
# same stable ordering internally.
IANA_SUPPORTED_MB_FIRST: list[str] = sorted(
    IANA_SUPPORTED, key=lambda encoding: not is_multi_byte_encoding(encoding)
)


def from_bytes(
    sequences: bytes | bytearray,
    steps: int = 5,
    chunk_size: int = 512,
    threshold: float = 0.2,
    cp_isolation: list[str] | None = None,
    cp_exclusion: list[str] | None = None,
    preemptive_behaviour: bool = True,
    explain: bool = False,
    language_threshold: float = 0.1,
    enable_fallback: bool = True,
) -> CharsetMatches:
    """
    Detect plausible encodings for a byte sequence.

    The detector samples ``steps`` blocks of ``chunk_size`` bytes, rejects
    candidates at or above ``threshold``, and returns matches ordered from most
    to least probable. Encoding isolation/exclusion, declarative hints, verbose
    analysis, language thresholds, and Unicode fallbacks retain their historic
    behavior.
    """
    previous_logger_level: int | None = None
    if explain:
        previous_logger_level = logger.level
        logger.addHandler(explain_handler)
        logger.setLevel(TRACE)
    try:
        return _native.from_bytes(
            sequences,
            steps,
            chunk_size,
            threshold,
            cp_isolation,
            cp_exclusion,
            preemptive_behaviour,
            explain,
            language_threshold,
            enable_fallback,
        )
    finally:
        if explain:
            logger.removeHandler(explain_handler)
            logger.setLevel(previous_logger_level)  # type: ignore[arg-type]


def from_fp(
    fp: BinaryIO,
    steps: int = 5,
    chunk_size: int = 512,
    threshold: float = 0.20,
    cp_isolation: list[str] | None = None,
    cp_exclusion: list[str] | None = None,
    preemptive_behaviour: bool = True,
    explain: bool = False,
    language_threshold: float = 0.1,
    enable_fallback: bool = True,
) -> CharsetMatches:
    """Detect plausible encodings from an open binary file object."""
    return from_bytes(
        fp.read(),
        steps,
        chunk_size,
        threshold,
        cp_isolation,
        cp_exclusion,
        preemptive_behaviour,
        explain,
        language_threshold,
        enable_fallback,
    )


def from_path(
    path: str | bytes | PathLike[str] | PathLike[bytes],
    steps: int = 5,
    chunk_size: int = 512,
    threshold: float = 0.20,
    cp_isolation: list[str] | None = None,
    cp_exclusion: list[str] | None = None,
    preemptive_behaviour: bool = True,
    explain: bool = False,
    language_threshold: float = 0.1,
    enable_fallback: bool = True,
) -> CharsetMatches:
    """Open a path in binary mode and detect its plausible encodings."""
    with open(path, "rb") as fp:
        return from_fp(
            fp,
            steps,
            chunk_size,
            threshold,
            cp_isolation,
            cp_exclusion,
            preemptive_behaviour,
            explain,
            language_threshold,
            enable_fallback,
        )


def is_binary(
    fp_or_path_or_payload: PathLike[str] | PathLike[bytes] | str | BinaryIO | bytes,
    steps: int = 5,
    chunk_size: int = 512,
    threshold: float = 0.20,
    cp_isolation: list[str] | None = None,
    cp_exclusion: list[str] | None = None,
    preemptive_behaviour: bool = True,
    explain: bool = False,
    language_threshold: float = 0.1,
    enable_fallback: bool = False,
) -> bool:
    """Return whether a payload, file object, or path appears to be binary."""
    kwargs = {
        "steps": steps,
        "chunk_size": chunk_size,
        "threshold": threshold,
        "cp_isolation": cp_isolation,
        "cp_exclusion": cp_exclusion,
        "preemptive_behaviour": preemptive_behaviour,
        "explain": explain,
        "language_threshold": language_threshold,
        "enable_fallback": enable_fallback,
    }
    if isinstance(fp_or_path_or_payload, (str, PathLike)):
        guesses = from_path(fp_or_path_or_payload, **kwargs)
    elif isinstance(fp_or_path_or_payload, (bytes, bytearray)):
        guesses = from_bytes(fp_or_path_or_payload, **kwargs)
    else:
        guesses = from_fp(fp_or_path_or_payload, **kwargs)
    return not guesses

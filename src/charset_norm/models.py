from __future__ import annotations

from typing import List, Tuple

from ._native import CharsetMatch, CharsetMatches, CliDetectionResult

CoherenceMatch = Tuple[str, float]
CoherenceMatches = List[CoherenceMatch]

__all__ = [
    "CharsetMatch",
    "CharsetMatches",
    "CliDetectionResult",
    "CoherenceMatch",
    "CoherenceMatches",
]

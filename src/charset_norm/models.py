from __future__ import annotations

from ._native import CharsetMatch, CharsetMatches, CliDetectionResult

CoherenceMatch = tuple[str, float]
CoherenceMatches = list[CoherenceMatch]

__all__ = [
    "CharsetMatch",
    "CharsetMatches",
    "CliDetectionResult",
    "CoherenceMatch",
    "CoherenceMatches",
]

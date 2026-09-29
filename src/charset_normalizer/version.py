"""
Expose version
"""

from __future__ import annotations

from ._native import __version__

VERSION = __version__.split(".")

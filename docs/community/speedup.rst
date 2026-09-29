Rust extension
==============

Why?
----

Charset-normalizer uses a Rust core exposed through PyO3. Published wheels contain
the extension and preserve the existing Python API and import paths.

How?
----

If your platform or architecture is not served by a wheel, compile the extension
locally with Rust 1.83 or newer:

  ::

    pip install charset-normalizer --no-binary charset-normalizer


There is no pure-Python fallback. An installation that cannot load the native
module fails immediately instead of silently changing implementation.


Native data
-----------

The extension does not call back into Python while detecting. Unicode character
properties come from Rust crates, while detection constants and the codec tables
(single-byte code pages and the CJK codecs) are generated from the reference
implementation so that decoding matches CPython byte for byte. After changing
``charset_normalizer/constant.py``, regenerate them:

  ::

    python bin/generate_native_tables.py

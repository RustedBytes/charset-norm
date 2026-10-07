# charset-norm

[![PyPI - Version](https://img.shields.io/pypi/v/charset-norm)](https://pypi.org/project/charset-norm/)
[![Crates.io Version](https://img.shields.io/crates/v/charset-norm)](https://crates.io/crates/charset-norm)
[![CI](https://github.com/RustedBytes/charset-norm/actions/workflows/ci.yml/badge.svg)](https://github.com/RustedBytes/charset-norm/actions/workflows/ci.yml)
[![PyPI Downloads](https://static.pepy.tech/personalized-badge/charset-norm?period=total&units=INTERNATIONAL_SYSTEM&left_color=BLACK&right_color=GREEN&left_text=downloads)](https://pepy.tech/projects/charset-norm)

A Rust rewrite of [charset_normalizer](https://github.com/jawah/charset_normalizer),
the universal charset detector for Python.

The detection core (codecs, Unicode tables, mess and coherence analysis) is
the [`charset-norm`](crates/charset-norm) Rust crate, usable on its own; the
`charset_norm` Python package wraps it through PyO3 and keeps the original
charset_normalizer API.

| Path | What |
|------|------|
| `crates/charset-norm` | Rust library (publishable to crates.io) |
| `crates/charset-norm-python` | PyO3 bindings built as `charset_norm._native` |
| `src/charset_norm` | Python package |

## Installation

Requires Python 3.9+ and, when building from source, Rust 1.98+.

```sh
pip install charset-norm

# or using uv
uv add charset-norm
```

Prebuilt wheels are available on [PyPI](https://pypi.org/project/charset-norm/4.2.0/#files)
for the following interpreters:

| Interpreter | Wheel tags | Supported platforms |
|-------------|------------|---------------------|
| CPython 3.9+ (GIL) | `cp39-abi3` | Linux x86_64/ARM64 (glibc 2.17+), Windows x64, macOS ARM64 (11+) |
| Free-threaded CPython 3.14t | `cp314-cp314t` | Linux x86_64/ARM64 (glibc 2.28+), Windows x64, macOS ARM64 (11+) |
| CPython 3.15+ (GIL and free-threaded 3.15t+) | `cp315-abi3.abi3t` | Linux x86_64/ARM64 (glibc 2.28+), Windows x64, macOS ARM64 (11+) |
| PyPy 3.11 / 3.12 | `pp311-pypy311_pp80` / `pp312-pypy312_pp80` | Linux x86_64/ARM64 (glibc 2.28+), Windows x64, macOS ARM64 (11+) |

Starting with v4.2.0, one shared stable-ABI wheel per platform supports both
regular and free-threaded CPython 3.15+. Python 3.14t continues to use its
interpreter-specific wheels; it cannot use the Python 3.15+ shared wheels.
See the [4.2.0 changelog](CHANGELOG.md#420-2026-10-07) for packaging details.

Other platforms build from the source distribution and require Rust 1.98+.
To install the latest code:

```sh
pip install git+https://github.com/RustedBytes/charset-norm.git
```

## Usage

```python
from charset_norm import from_bytes, from_path

best = from_path("./my_subtitle.srt").best()
print(best.encoding, str(best))

payload = "Всеки човек има право на образование.".encode("cp1251")
print(from_bytes(payload).best().encoding)  # cp1251
```

A chardet-compatible `detect()` is also available:

```python
from charset_norm import detect
```

Command line:

```sh
normalizer ./data/sample-french.txt
```

## Rust

```toml
[dependencies]
charset-norm = "4.1"
```

```rust
let results = charset_norm::from_bytes(&payload);
if let Some(best) = results.best() {
    println!("{} {}", best.encoding(), best.decoded()?);
}
```

See the [crate README](crates/charset-norm/README.md) for more.

## Development

```sh
pip install maturin
maturin develop --release
pip install --group dev
pytest
cargo test --workspace --all-features
```

## License

MIT. Based on [charset_normalizer](https://github.com/jawah/charset_normalizer)
by Ahmed TAHRI ([@Ousret](https://github.com/Ousret)).

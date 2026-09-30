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

Prebuilt wheels cover Linux (x86_64, arm64), Windows and macOS (arm64) for
CPython 3.9+ (abi3), free-threaded CPython 3.14 and PyPy 3.11/3.12. Other
platforms build from the source distribution. To install the latest code:

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
charset-norm = "4.0"
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

# charset-norm

A Rust rewrite of [charset_normalizer](https://github.com/jawah/charset_normalizer),
the universal charset detector for Python.

The detection core (codecs, Unicode tables, mess and coherence analysis) is
implemented in Rust and exposed through PyO3, while the Python package keeps the
original `charset_normalizer` import path and API.

## Installation

The distribution is named `charset-norm` and provides the `charset_normalizer`
module, so do not install it alongside the original `charset-normalizer`.

Requires Python 3.8+ and, when building from source, Rust 1.83+.

```sh
pip install git+https://github.com/RustedBytes/charset-norm.git
```

## Usage

```python
from charset_normalizer import from_bytes, from_path

best = from_path("./my_subtitle.srt").best()
print(best.encoding, str(best))

payload = "Всеки човек има право на образование.".encode("cp1251")
print(from_bytes(payload).best().encoding)  # cp1251
```

A chardet-compatible `detect()` is also available:

```python
from charset_normalizer import detect
```

Command line:

```sh
normalizer ./data/sample-french.txt
```

## Development

```sh
pip install maturin
maturin develop --release
pip install --group dev
pytest
```

## License

MIT. Based on [charset_normalizer](https://github.com/jawah/charset_normalizer)
by Ahmed TAHRI ([@Ousret](https://github.com/Ousret)).

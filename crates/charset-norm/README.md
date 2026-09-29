# charset-norm

Universal character encoding detector for Rust: a Rust implementation of
[charset_normalizer](https://github.com/jawah/charset_normalizer).

It decodes a payload with every supported code page, measures how noisy each
result looks (*chaos*) and how well it matches known languages (*coherence*),
and ranks the plausible encodings. Decoding reproduces CPython's codecs byte
for byte (99 code pages, including the CJK and ISO-2022 families), so results
agree with the Python package.

## Usage

```toml
[dependencies]
charset-norm = "3.5"
```

```rust
let payload = std::fs::read("subtitles.srt")?;
let results = charset_norm::from_bytes(&payload);

match results.best() {
    Some(best) => {
        println!("{} ({})", best.encoding(), best.language());
        let text = best.decoded()?;
        let utf8 = best.output("utf_8")?;
    }
    None => println!("probably binary"),
}
```

Detection can be tuned and observed:

```rust
use charset_norm::{from_bytes_with, DetectionOptions, NoLogger};

let options = DetectionOptions {
    cp_isolation: vec!["cp1252".into(), "cp1251".into()],
    ..DetectionOptions::default()
};
let results = from_bytes_with(&payload, &options, &NoLogger);
```

The building blocks are public too: `codecs` (CPython-compatible decoders and
encoders), `encoding` (names, BOMs, declared charsets), `mess`, `coherence`
and `unicode`.

## Features

- `log`: forward detection diagnostics to the [`log`](https://docs.rs/log)
  crate through `charset_norm::log::LogCrate`.

## License

MIT. Based on [charset_normalizer](https://github.com/jawah/charset_normalizer)
by Ahmed TAHRI.

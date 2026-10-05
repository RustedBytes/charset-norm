//! Profile detection with hotpath over a directory of samples:
//!
//! ```text
//! cargo run --release -p charset-norm --example profile --features hotpath -- DIR [ROUNDS]
//! ```
//!
//! With `--features hotpath-alloc`, the report lists allocations instead of
//! timings. `DIR` is searched recursively; `ROUNDS` (default 3) repeats the
//! whole corpus so per-thread buffers are warm.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

fn collect(dir: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path
            .file_name()
            .is_some_and(|name| name.as_encoded_bytes().starts_with(b"."))
        {
            continue;
        }
        if path.is_dir() {
            collect(&path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

#[hotpath::main(percentiles = [50, 95, 99])]
fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(dir) = args.next() else {
        eprintln!("usage: profile DIR [ROUNDS]");
        return ExitCode::FAILURE;
    };
    let rounds: usize = args.next().map_or(3, |value| value.parse().unwrap_or(3));
    let mut files = Vec::new();
    if let Err(error) = collect(Path::new(&dir), &mut files) {
        eprintln!("{dir}: {error}");
        return ExitCode::FAILURE;
    }
    files.sort();
    let payloads: Vec<Vec<u8>> = files
        .iter()
        .filter_map(|path| std::fs::read(path).ok())
        .collect();
    let bytes: usize = payloads.iter().map(Vec::len).sum();

    let start = Instant::now();
    let mut found = 0usize;
    for _ in 0..rounds {
        for payload in &payloads {
            found += usize::from(charset_norm::from_bytes(payload).best().is_some());
        }
    }
    let elapsed = start.elapsed();
    eprintln!(
        "{} files ({bytes} bytes) x {rounds} rounds: {elapsed:.2?}, {:.2?} per file, {found} detections",
        payloads.len(),
        elapsed
            / u32::try_from(payloads.len() * rounds)
                .unwrap_or(u32::MAX)
                .max(1),
    );
    ExitCode::SUCCESS
}

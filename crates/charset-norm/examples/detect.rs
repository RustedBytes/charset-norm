//! Print the plausible encodings of files: `cargo run --example detect -- FILE...`

use std::process::ExitCode;

fn main() -> ExitCode {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("usage: detect FILE...");
        return ExitCode::FAILURE;
    }
    for path in paths {
        match charset_norm::from_path(&path) {
            Ok(results) => match results.best() {
                Some(best) => println!(
                    "{path}: {} (language: {}, chaos: {:.1}%, coherence: {:.1}%)",
                    best.encoding(),
                    best.language(),
                    best.percent_chaos(),
                    best.percent_coherence()
                ),
                None => println!("{path}: binary or unknown"),
            },
            Err(error) => {
                eprintln!("{path}: {error}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

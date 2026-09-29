//! End-to-end detection through the public API.

use charset_norm::codecs::{self, Errors};
use charset_norm::{DetectionOptions, Level, Logger, from_bytes, from_bytes_with, is_binary};
use std::cell::RefCell;

fn detect(text: &str, encoding: &str) -> String {
    let payload = codecs::encode(text, encoding).expect("native encoder");
    let results = from_bytes(&payload);
    let best = results.best().expect("a match");
    assert_eq!(
        best.decoded().unwrap(),
        codecs::decode(&payload, best.encoding(), Errors::Strict).unwrap()
    );
    best.encoding().to_owned()
}

#[test]
fn detects_common_single_byte_pages() {
    assert_eq!(
        detect(
            "Всеки човек има право на образование. Образованието трябва да бъде безплатно.",
            "cp1251"
        ),
        "cp1251"
    );
    assert_eq!(
        detect(
            "Ελληνικά κείμενα για τον έλεγχο της ανίχνευσης κωδικοσελίδας και γλώσσας.",
            "cp1253"
        ),
        "cp1253"
    );
}

#[test]
fn utf8_and_bom() {
    let text = "Ça va très bien, merci beaucoup. Nous avons visité le château et goûté la cuisine régionale pendant les vacances d'été.";
    let results = from_bytes(text.as_bytes());
    let best = results.best().unwrap();
    assert_eq!(best.encoding(), "utf_8");
    assert_eq!(best.language(), "French");

    let results = from_bytes(b"\xef\xbb\xbfhello world");
    let best = results.best().unwrap();
    assert!(best.has_sig_or_bom());
    assert_eq!(best.decoded().unwrap(), "hello world");
}

#[test]
fn empty_and_binary_payloads() {
    let results = from_bytes(b"");
    assert_eq!(results.best().unwrap().encoding(), "utf_8");
    assert!(is_binary(&[
        0u8, 159, 146, 150, 0, 1, 2, 255, 254, 0, 0, 7, 3, 0, 200, 201
    ]));
    assert!(!is_binary(b"just some ascii text"));
}

#[test]
fn isolation_and_logging() {
    struct Collect(RefCell<Vec<String>>);
    impl Logger for Collect {
        fn enabled(&self, _level: Level) -> bool {
            true
        }
        fn log(&self, _level: Level, message: &str) {
            self.0.borrow_mut().push(message.to_owned());
        }
    }

    let payload = codecs::encode("Grüße aus München, schöne Grüße!", "cp1252").unwrap();
    let options = DetectionOptions {
        cp_isolation: vec!["latin-1".into()],
        ..DetectionOptions::default()
    };
    let logger = Collect(RefCell::new(Vec::new()));
    let results = from_bytes_with(&payload, &options, &logger);
    assert_eq!(results.best().unwrap().encoding(), "latin_1");
    assert!(
        logger
            .0
            .borrow()
            .iter()
            .any(|message| message.starts_with("Encoding detection:"))
    );
}

#[test]
fn transcoding_output() {
    let payload = codecs::encode("<meta charset=\"windows-1252\"> café", "cp1252").unwrap();
    let results = from_bytes(&payload);
    let best = results.best().unwrap();
    let output = String::from_utf8(best.output("utf_8").unwrap()).unwrap();
    assert!(output.contains("charset=\"utf-8\""));
    assert!(output.contains("café"));
}

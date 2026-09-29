//! Static detection data, generated from `charset_norm.constant`.

use std::sync::OnceLock;

use rustc_hash::FxHashMap;

include!("generated/constants.rs");

fn sorted_lookup<V: Copy>(table: &[(&'static str, V)], key: &str) -> Option<V> {
    table
        .binary_search_by(|(name, _)| (*name).cmp(key))
        .ok()
        .map(|index| table[index].1)
}

/// Resolve a normalized codec name the way `constant._IANA_NAMES` does.
pub fn iana_lookup(normalized: &str) -> Option<&'static str> {
    sorted_lookup(IANA_NAMES, normalized)
}

pub fn similar_encodings(encoding: &str) -> &'static [&'static str] {
    sorted_lookup(IANA_SUPPORTED_SIMILAR, encoding).unwrap_or(&[])
}

pub fn is_multi_byte_encoding(encoding: &str) -> bool {
    MULTI_BYTE_ENCODINGS.binary_search(&encoding).is_ok()
}

pub struct Language {
    pub name: &'static str,
    pub characters: Vec<char>,
    pub ranks: FxHashMap<char, usize>,
    pub has_accents: bool,
    pub pure_latin: bool,
}

/// FREQUENCIES with pre-computed per-language lookups, in declaration order.
pub fn languages() -> &'static [Language] {
    static LANGUAGES: OnceLock<Vec<Language>> = OnceLock::new();
    LANGUAGES.get_or_init(|| {
        FREQUENCIES
            .iter()
            .map(|(name, characters)| {
                let characters: Vec<char> = characters.chars().collect();
                let mut has_accents = false;
                let mut pure_latin = true;
                for &character in &characters {
                    let flags = crate::unicode::character_flags(character);
                    has_accents |= flags & crate::unicode::ACCENTUATED != 0;
                    pure_latin &= flags & crate::unicode::LATIN != 0;
                }
                let ranks = characters
                    .iter()
                    .enumerate()
                    .map(|(rank, &character)| (character, rank))
                    .collect();
                Language {
                    name,
                    characters,
                    ranks,
                    has_accents,
                    pure_latin,
                }
            })
            .collect()
    })
}

/// Bit `i` is set when `languages()[i]` lists the character.
pub fn language_mask(character: char) -> u64 {
    static MASKS: OnceLock<FxHashMap<char, u64>> = OnceLock::new();
    let masks = MASKS.get_or_init(|| {
        let languages = languages();
        assert!(
            languages.len() <= 64,
            "language masks hold at most 64 languages"
        );
        let mut masks = FxHashMap::default();
        for (index, language) in languages.iter().enumerate() {
            for &character in &language.characters {
                *masks.entry(character).or_insert(0u64) |= 1 << index;
            }
        }
        masks
    });
    masks.get(&character).copied().unwrap_or(0)
}

pub fn language(name: &str) -> Option<&'static Language> {
    languages().iter().find(|language| language.name == name)
}

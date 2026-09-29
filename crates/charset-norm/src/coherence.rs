//! Language coherence: how well decoded text matches known letter frequencies.

use std::hash::Hash;

use rustc_hash::FxHashMap;

use crate::tables::{self, Language};
use crate::{mess, pyfloat, unicode, Error};

/// Names of every language with a frequency profile.
pub fn languages() -> impl Iterator<Item = &'static str> {
    tables::languages().iter().map(|language| language.name)
}

fn language(name: &str) -> Result<&'static Language, Error> {
    tables::language(name).ok_or_else(|| Error::UnknownLanguage(name.to_owned()))
}

/// Whether a language's alphabet has accented letters, and whether it is
/// written with Latin letters only.
pub fn get_target_features(name: &str) -> Result<(bool, bool), Error> {
    let language = language(name)?;
    Ok((language.has_accents, language.pure_latin))
}

/// Languages whose alphabet covers at least 20% of `characters`, best first.
pub fn alphabet_languages(characters: &[char], ignore_non_latin: bool) -> Vec<&'static str> {
    let source_has_accents = characters
        .iter()
        .any(|&character| unicode::character_flags(character) & unicode::ACCENTUATED != 0);
    // Count, per language, how many distinct source characters it lists.
    let mut unique: Vec<char> = characters.to_vec();
    unique.sort_unstable();
    unique.dedup();
    let mut counts = [0usize; 64];
    for &character in &unique {
        let mut mask = tables::language_mask(character);
        while mask != 0 {
            counts[mask.trailing_zeros() as usize] += 1;
            mask &= mask - 1;
        }
    }
    let mut matches = Vec::new();
    for (index, language) in tables::languages().iter().enumerate() {
        if (ignore_non_latin && !language.pure_latin)
            || (!language.has_accents && source_has_accents)
        {
            continue;
        }
        let ratio = counts[index] as f64 / language.characters.len() as f64;
        if ratio >= 0.2 {
            matches.push((language.name, ratio));
        }
    }
    matches.sort_by(|a, b| b.1.total_cmp(&a.1));
    matches.into_iter().map(|item| item.0).collect()
}

fn popularity_compare(language: &Language, ordered: &[char]) -> f64 {
    let target_count = language.characters.len();
    if ordered.is_empty() {
        return f64::NAN;
    }
    let large_alphabet = target_count > 26;
    let large_threshold = target_count as f64 / 3.0;
    let projection_ratio = target_count as f64 / ordered.len() as f64;
    let common: Vec<(usize, usize)> = ordered
        .iter()
        .enumerate()
        .filter_map(|(popularity_rank, character)| {
            language
                .ranks
                .get(character)
                .map(|&language_rank| (language_rank, popularity_rank))
        })
        .collect();

    let mut approved = 0usize;
    for &(language_rank, popularity_rank) in &common {
        let projected = (popularity_rank as f64 * projection_ratio) as usize;
        let distance = projected.abs_diff(language_rank);

        if !large_alphabet && distance > 4 {
            continue;
        }
        if large_alphabet && (distance as f64) < large_threshold {
            approved += 1;
            continue;
        }
        if language_rank == 0 {
            approved += 1;
            continue;
        }

        let after_len = target_count - language_rank;
        let mut before = 0usize;
        let mut after = 0usize;
        for &(other_language_rank, other_popularity_rank) in &common {
            if other_language_rank < language_rank {
                if other_popularity_rank < popularity_rank {
                    before += 1;
                    if 5 * before >= 2 * language_rank {
                        approved += 1;
                        break;
                    }
                }
            } else if other_popularity_rank >= popularity_rank {
                after += 1;
                if 5 * after >= 2 * after_len {
                    approved += 1;
                    break;
                }
            }
        }
    }

    approved as f64 / ordered.len() as f64
}

/// Share of `ordered` (characters sorted from most to least frequent) whose
/// rank agrees with the language's frequency profile. `NaN` when empty.
pub fn characters_popularity_compare(language_name: &str, ordered: &[char]) -> Result<f64, Error> {
    Ok(popularity_compare(language(language_name)?, ordered))
}

/// Average each language's ratios across chunks, rounded, best first.
pub fn merge_coherence_ratios<K: Clone + Eq + Hash>(results: Vec<Vec<(K, f64)>>) -> Vec<(K, f64)> {
    let mut order = Vec::new();
    let mut ratios: FxHashMap<K, Vec<f64>> = FxHashMap::default();
    for result in results {
        for (language, ratio) in result {
            ratios
                .entry(language)
                .or_insert_with_key(|language| {
                    order.push(language.clone());
                    Vec::new()
                })
                .push(ratio);
        }
    }
    let mut merged: Vec<(K, f64)> = order
        .into_iter()
        .map(|language| {
            let values = &ratios[&language];
            let mean = values.iter().sum::<f64>() / values.len() as f64;
            (language, pyfloat::round(mean, 4))
        })
        .collect();
    merged.sort_by(|a, b| b.1.total_cmp(&a.1));
    merged
}

/// Fold alternative profiles (`"English—"`) into their base language,
/// keeping the best ratio, when any language appears more than once.
fn filter_alt_by<K: Clone + Eq + Hash>(
    results: Vec<(K, f64)>,
    normalize: impl Fn(&K) -> K,
) -> Vec<(K, f64)> {
    let mut order = Vec::new();
    let mut ratios: FxHashMap<K, Vec<f64>> = FxHashMap::default();
    for (language, ratio) in &results {
        ratios
            .entry(normalize(language))
            .or_insert_with_key(|normalized| {
                order.push(normalized.clone());
                Vec::new()
            })
            .push(*ratio);
    }
    if !ratios.values().any(|values| values.len() > 1) {
        return results;
    }
    order
        .into_iter()
        .map(|language| {
            let best = ratios[&language]
                .iter()
                .copied()
                .max_by(f64::total_cmp)
                .unwrap_or(0.0);
            (language, best)
        })
        .collect()
}

/// Fold alternative language profiles (names suffixed with `—`) into their
/// base language when a language is reported more than once.
pub fn filter_alt_coherence_matches(results: Vec<(String, f64)>) -> Vec<(String, f64)> {
    filter_alt_by(results, |language| language.replace('—', ""))
}

/// Split text into lowercase layers of letters from compatible Unicode ranges.
pub fn alpha_unicode_split(decoded: &str) -> Vec<String> {
    let mut layers: Vec<(u16, String)> = Vec::new();
    let mut previous: Option<(u16, usize)> = None;

    for character in decoded.chars() {
        let (alpha, range) = mess::alpha_range(character);
        if !alpha || range == unicode::NO_RANGE {
            continue;
        }
        if let Some((previous_range, target)) = previous {
            if previous_range == range {
                layers[target].1.push(character);
                continue;
            }
        }
        let target = layers
            .iter()
            .position(|(discovered, _)| !unicode::suspicious_range_indices(*discovered, range))
            .unwrap_or_else(|| {
                layers.push((range, String::new()));
                layers.len() - 1
            });
        layers[target].1.push(character);
        previous = Some((range, target));
    }

    layers
        .into_iter()
        .map(|(_, layer)| layer.to_lowercase())
        .collect()
}

/// Languages the text plausibly is, with a coherence ratio in `[0, 1]`,
/// best first.
///
/// `lg_inclusion` is a comma-separated list restricting the candidate
/// languages; `"Latin Based"` limits automatic candidates to Latin alphabets.
///
/// ```
/// let text = "The quick brown fox jumps over the lazy dog and keeps running far away.";
/// let languages = charset_norm::coherence::coherence_ratio(text, 0.1, None).unwrap();
/// assert_eq!(languages[0].0, "English");
/// ```
pub fn coherence_ratio(
    decoded: &str,
    threshold: f64,
    lg_inclusion: Option<&str>,
) -> Result<Vec<(&'static str, f64)>, Error> {
    let mut results = Vec::<(&'static str, f64)>::new();
    let mut inclusion: Vec<&str> = lg_inclusion
        .map(|value| value.split(',').collect())
        .unwrap_or_default();
    let ignore_non_latin = inclusion.contains(&"Latin Based");
    inclusion.retain(|language| *language != "Latin Based");
    let mut sufficient = 0usize;

    for layer in alpha_unicode_split(decoded) {
        let mut counts = FxHashMap::<char, usize>::default();
        let mut order = Vec::<char>::new();
        let mut length = 0usize;
        for character in layer.chars() {
            length += 1;
            let count = counts.entry(character).or_default();
            if *count == 0 {
                order.push(character);
            }
            *count += 1;
        }
        if length <= 32 {
            continue;
        }
        order.sort_by(|a, b| counts[b].cmp(&counts[a]));
        let detected;
        let candidates: &[&str] = if inclusion.is_empty() {
            detected = alphabet_languages(&order, ignore_non_latin);
            &detected
        } else {
            &inclusion
        };
        for &name in candidates {
            let language = language(name)?;
            let ratio = popularity_compare(language, &order);
            if ratio < threshold {
                continue;
            }
            if ratio >= 0.8 {
                sufficient += 1;
            }
            results.push((language.name, pyfloat::round(ratio, 4)));
            if sufficient >= 3 {
                break;
            }
        }
    }
    // Alternative profile names only carry trailing em dashes, so trimming
    // yields the (static) base name.
    let mut filtered = filter_alt_by(results, |language| language.trim_end_matches('—'));
    filtered.sort_by(|a, b| b.1.total_cmp(&a.1));
    Ok(filtered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popularity_scores_ranked_input() {
        assert_eq!(
            characters_popularity_compare("English", &['e', 'e', 't', 'a']).unwrap(),
            0.25
        );
        assert!(characters_popularity_compare("English", &[])
            .unwrap()
            .is_nan());
        assert!(characters_popularity_compare("Klingon", &['a']).is_err());
    }

    #[test]
    fn merges_and_filters() {
        assert_eq!(
            merge_coherence_ratios(vec![
                vec![("English", 0.2)],
                vec![("English", 0.4), ("French", 0.5)]
            ]),
            vec![("French", 0.5), ("English", 0.3)]
        );
        assert_eq!(
            filter_alt_coherence_matches(vec![
                ("English".to_owned(), 0.8),
                ("English—".to_owned(), 0.9)
            ]),
            vec![("English".to_owned(), 0.9)]
        );
        assert_eq!(
            alpha_unicode_split("Hello العربية"),
            vec!["hello".to_owned(), "العربية".to_owned()]
        );
    }
}

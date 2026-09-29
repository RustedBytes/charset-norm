//! Language coherence: how well decoded text matches known letter frequencies.

use rustc_hash::FxHashMap;

use crate::tables::{self, Language};
use crate::{Error, mess, pyfloat, unicode};

/// Names of every language with a frequency profile.
pub fn languages() -> impl Iterator<Item = &'static str> {
    tables::languages().iter().map(|language| language.name)
}

fn language(name: &str) -> Result<&'static Language, Error> {
    tables::language(name).ok_or_else(|| Error::UnknownLanguage(name.to_owned()))
}

/// Whether a language's alphabet has accented letters, and whether it is
/// written with Latin letters only.
///
/// # Errors
///
/// [`Error::UnknownLanguage`] when the language has no frequency profile.
pub fn get_target_features(name: &str) -> Result<(bool, bool), Error> {
    let language = language(name)?;
    Ok((language.has_accents, language.pure_latin))
}

/// Languages covering at least 20% of `unique` (distinct characters), best
/// first, written to `out`.
fn alphabet_candidates(
    unique: &[char],
    ignore_non_latin: bool,
    out: &mut Vec<(&'static Language, f64)>,
) {
    out.clear();
    let source_has_accents = unique
        .iter()
        .any(|&character| unicode::character_flags(character) & unicode::ACCENTUATED != 0);
    let mut counts = [0usize; 64];
    for &character in unique {
        let mut mask = tables::language_mask(character);
        while mask != 0 {
            counts[mask.trailing_zeros() as usize] += 1;
            mask &= mask - 1;
        }
    }
    for (index, language) in tables::languages().iter().enumerate() {
        if (ignore_non_latin && !language.pure_latin)
            || (!language.has_accents && source_has_accents)
        {
            continue;
        }
        let ratio = counts[index] as f64 / language.characters.len() as f64;
        if ratio >= 0.2 {
            out.push((language, ratio));
        }
    }
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
}

/// Languages whose alphabet covers at least 20% of `characters`, best first.
#[must_use]
pub fn alphabet_languages(characters: &[char], ignore_non_latin: bool) -> Vec<&'static str> {
    let mut unique: Vec<char> = characters.to_vec();
    unique.sort_unstable();
    unique.dedup();
    let mut candidates = Vec::new();
    alphabet_candidates(&unique, ignore_non_latin, &mut candidates);
    candidates.into_iter().map(|item| item.0.name).collect()
}

/// Share of the `ordered_len` popular characters whose rank agrees with the
/// language profile, given the `(language rank, popularity rank)` pairs of
/// the characters both share. The result does not depend on pair order.
fn popularity_score(target_count: usize, ordered_len: usize, common: &[(usize, usize)]) -> f64 {
    if ordered_len == 0 {
        return f64::NAN;
    }
    let large_alphabet = target_count > 26;
    let large_threshold = target_count as f64 / 3.0;
    let projection_ratio = target_count as f64 / ordered_len as f64;

    let mut approved = 0usize;
    for &(language_rank, popularity_rank) in common {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "non-negative; truncation is the reference's int()"
        )]
        let projected = (popularity_rank as f64 * projection_ratio) as usize;
        let distance = projected.abs_diff(language_rank);

        if !large_alphabet && distance > 4 {
            continue;
        }
        if (large_alphabet && (distance as f64) < large_threshold) || language_rank == 0 {
            approved += 1;
            continue;
        }

        let after_len = target_count - language_rank;
        let mut before = 0usize;
        let mut after = 0usize;
        for &(other_language_rank, other_popularity_rank) in common {
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

    approved as f64 / ordered_len as f64
}

/// Share of `ordered` (characters sorted from most to least frequent) whose
/// rank agrees with the language's frequency profile. `NaN` when empty.
///
/// # Errors
///
/// [`Error::UnknownLanguage`] when the language has no frequency profile.
pub fn characters_popularity_compare(language_name: &str, ordered: &[char]) -> Result<f64, Error> {
    let language = language(language_name)?;
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
    Ok(popularity_score(
        language.characters.len(),
        ordered.len(),
        &common,
    ))
}

/// Average each language's ratios, rounded, best first. Groups keep the
/// order in which languages first appear.
fn merge_by<'k, K: Clone + PartialEq + 'k>(
    results: impl IntoIterator<Item = impl IntoIterator<Item = &'k (K, f64)>>,
) -> Vec<(K, f64)> {
    // (language, running sum, count); a handful of languages at most.
    let mut groups: Vec<(K, f64, usize)> = Vec::new();
    for result in results {
        for (language, ratio) in result {
            match groups.iter_mut().find(|group| group.0 == *language) {
                Some(group) => {
                    group.1 += ratio;
                    group.2 += 1;
                }
                None => groups.push((language.clone(), *ratio, 1)),
            }
        }
    }
    let mut merged: Vec<(K, f64)> = groups
        .into_iter()
        .map(|(language, sum, count)| (language, pyfloat::round(sum / count as f64, 4)))
        .collect();
    merged.sort_by(|a, b| b.1.total_cmp(&a.1));
    merged
}

/// Average each language's ratios across chunks, rounded, best first.
#[must_use]
pub fn merge_coherence_ratios<K: Clone + PartialEq>(results: &[Vec<(K, f64)>]) -> Vec<(K, f64)> {
    merge_by(results)
}

/// (language, running sum or best ratio, count) while grouping results.
pub(crate) type Groups = Vec<(&'static str, f64, usize)>;

/// [`merge_coherence_ratios`] over borrowed per-chunk results, written to
/// `out` (replacing its contents); `groups` is scratch space.
pub(crate) fn merge_into<'r>(
    results: impl IntoIterator<Item = &'r [(&'static str, f64)]>,
    groups: &mut Groups,
    out: &mut Vec<(&'static str, f64)>,
) {
    groups.clear();
    for result in results {
        for &(language, ratio) in result {
            match groups.iter_mut().find(|group| group.0 == language) {
                Some(group) => {
                    group.1 += ratio;
                    group.2 += 1;
                }
                None => groups.push((language, ratio, 1)),
            }
        }
    }
    out.clear();
    out.extend(
        groups
            .iter()
            .map(|&(language, sum, count)| (language, pyfloat::round(sum / count as f64, 4))),
    );
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
}

/// Fold alternative profiles (`"English—"`) into their base language,
/// keeping the best ratio, when any language appears more than once.
fn filter_alt_by<K: Clone + PartialEq>(
    results: Vec<(K, f64)>,
    normalize: impl Fn(&K) -> K,
) -> Vec<(K, f64)> {
    // (normalized language, best ratio, count), in order of appearance.
    let mut groups: Vec<(K, f64, usize)> = Vec::new();
    for (language, ratio) in &results {
        let normalized = normalize(language);
        match groups.iter_mut().find(|group| group.0 == normalized) {
            Some(group) => {
                group.1 = group.1.max(*ratio);
                group.2 += 1;
            }
            None => groups.push((normalized, *ratio, 1)),
        }
    }
    if groups.iter().all(|group| group.2 == 1) {
        return results;
    }
    groups
        .into_iter()
        .map(|(language, best, _)| (language, best))
        .collect()
}

/// Fold alternative language profiles (names suffixed with `—`) into their
/// base language when a language is reported more than once.
#[must_use]
pub fn filter_alt_coherence_matches(results: Vec<(String, f64)>) -> Vec<(String, f64)> {
    filter_alt_by(results, |language| language.replace('—', ""))
}

/// Reusable buffers for coherence analysis.
#[derive(Default)]
pub(crate) struct Scratch {
    /// (unicode range, letters) per layer; only `layer_count` are in use.
    layers: Vec<Layer>,
    layer_count: usize,
    /// BMP character -> its lowercase plus one when that is a single
    /// character, `MULTI_LOWER` otherwise, 0 when not computed yet.
    lowercase: Vec<u32>,
    /// BMP character -> position in `tally` (then popularity rank), or
    /// `NO_SLOT`; allocated on first use.
    slots: Vec<u32>,
    /// The same for characters beyond the BMP.
    index: FxHashMap<char, usize>,
    /// (character, count, first position) in order of first appearance.
    tally: Vec<(char, usize, usize)>,
    ordered: Vec<char>,
    candidates: Vec<(&'static Language, f64)>,
    common: Vec<(usize, usize)>,
    results: Vec<(&'static str, f64)>,
    groups: Groups,
}

impl Scratch {
    /// Split `decoded` into layers of letters from compatible Unicode ranges.
    fn split_layers(&mut self, decoded: &str) {
        self.layer_count = 0;
        let mut previous: Option<(u16, usize)> = None;
        for character in decoded.chars() {
            let (alpha, range) = mess::alpha_range(character);
            if !alpha || range == unicode::NO_RANGE {
                continue;
            }
            if let Some((previous_range, target)) = previous
                && previous_range == range
            {
                self.layers[target].push(character);
                continue;
            }
            let target = self.layers[..self.layer_count]
                .iter()
                .position(|layer| !unicode::suspicious_range_indices(layer.range, range))
                .unwrap_or_else(|| {
                    if self.layer_count == self.layers.len() {
                        self.layers.push(Layer::default());
                    }
                    self.layers[self.layer_count].reset(range);
                    self.layer_count += 1;
                    self.layer_count - 1
                });
            self.layers[target].push(character);
            previous = Some((range, target));
        }
    }

    /// Forget the characters of the previous tally.
    fn reset_slots(&mut self) {
        if self.slots.is_empty() {
            self.slots = vec![NO_SLOT; 0x1_0000];
        }
        for &(character, ..) in &self.tally {
            if let Some(slot) = self.slots.get_mut(character as usize) {
                *slot = NO_SLOT;
            }
        }
        self.index.clear();
        self.tally.clear();
    }

    /// Count the lowercased letters of a layer; returns their total.
    fn tally_layer(&mut self, layer: usize) -> usize {
        self.reset_slots();
        if self.lowercase.is_empty() {
            self.lowercase = vec![0; 0x1_0000];
        }
        let layer = &self.layers[layer];
        let (slots, index, tally) = (&mut self.slots, &mut self.index, &mut self.tally);
        let mut length = 0usize;
        let mut add = |character: char| {
            let position = match slots.get_mut(character as usize) {
                Some(slot) if *slot != NO_SLOT => *slot as usize,
                Some(slot) => {
                    *slot = u32::try_from(tally.len()).unwrap_or(NO_SLOT);
                    tally.push((character, 0, length));
                    tally.len() - 1
                }
                None => *index.entry(character).or_insert_with(|| {
                    tally.push((character, 0, length));
                    tally.len() - 1
                }),
            };
            tally[position].1 += 1;
            length += 1;
        };
        if layer.capital_sigma {
            // Only the capital sigma lowercases differently in context.
            let text: String = layer.letters.iter().collect();
            text.to_lowercase().chars().for_each(&mut add);
        } else {
            let lowercase = &mut self.lowercase;
            for &character in &layer.letters {
                match lowercase.get_mut(character as usize) {
                    Some(cached) if *cached != 0 && *cached != MULTI_LOWER => {
                        add(char::from_u32(*cached - 1).unwrap_or(character));
                    }
                    Some(cached) if *cached == 0 => {
                        let mut lower = character.to_lowercase();
                        match (lower.next(), lower.len()) {
                            (Some(single), 0) => {
                                *cached = u32::from(single) + 1;
                                add(single);
                            }
                            (first, _) => {
                                *cached = MULTI_LOWER;
                                first.into_iter().chain(lower).for_each(&mut add);
                            }
                        }
                    }
                    _ => character.to_lowercase().for_each(&mut add),
                }
            }
        }
        length
    }

    /// Letters of the tallied layer, most frequent first (ties in order of
    /// first appearance); slots now map each letter to its popularity rank.
    fn order_tally(&mut self) {
        self.tally
            .sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
        self.ordered.clear();
        self.index.clear();
        for (rank, &(character, ..)) in self.tally.iter().enumerate() {
            self.ordered.push(character);
            match self.slots.get_mut(character as usize) {
                Some(slot) => *slot = u32::try_from(rank).unwrap_or(NO_SLOT),
                None => {
                    self.index.insert(character, rank);
                }
            }
        }
    }

    /// Popularity rank of a letter of the ordered tally.
    fn rank(&self, character: char) -> Option<usize> {
        match self.slots.get(character as usize) {
            Some(&slot) => (slot != NO_SLOT).then_some(slot as usize),
            None => self.index.get(&character).copied(),
        }
    }

    /// [`popularity_score`] of the ordered tally against `language`.
    fn popularity(&mut self, language: &Language) -> f64 {
        let mut common = std::mem::take(&mut self.common);
        common.clear();
        common.extend(language.characters.iter().enumerate().filter_map(
            |(language_rank, &character)| {
                self.rank(character)
                    .map(|popularity_rank| (language_rank, popularity_rank))
            },
        ));
        let score = popularity_score(language.characters.len(), self.ordered.len(), &common);
        self.common = common;
        score
    }
}

/// Empty entry of `Scratch::slots`.
const NO_SLOT: u32 = u32::MAX;

/// `Scratch::lowercase` entry of a character lowercasing to several.
const MULTI_LOWER: u32 = u32::MAX;

/// Letters from compatible Unicode ranges, in text order.
#[derive(Default)]
struct Layer {
    range: u16,
    letters: Vec<char>,
    /// Whether the layer holds a capital sigma, whose lowercase depends on
    /// its neighbours.
    capital_sigma: bool,
}

impl Layer {
    fn reset(&mut self, range: u16) {
        self.range = range;
        self.letters.clear();
        self.capital_sigma = false;
    }

    fn push(&mut self, character: char) {
        self.capital_sigma |= character == 'Σ';
        self.letters.push(character);
    }
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
///
/// # Errors
///
/// [`Error::UnknownLanguage`] when `lg_inclusion` names a language without
/// a frequency profile.
pub fn coherence_ratio(
    decoded: &str,
    threshold: f64,
    lg_inclusion: Option<&str>,
) -> Result<Vec<(&'static str, f64)>, Error> {
    let inclusion: Vec<&str> = lg_inclusion
        .map(|value| value.split(',').collect())
        .unwrap_or_default();
    let mut out = Vec::new();
    coherence_into(
        decoded,
        threshold,
        &inclusion,
        &mut Scratch::default(),
        &mut out,
    )?;
    Ok(out)
}

/// [`coherence_ratio`] with the inclusion list already split, reusing
/// `scratch`; the results are appended to `out`.
pub(crate) fn coherence_into(
    decoded: &str,
    threshold: f64,
    inclusion: &[&str],
    scratch: &mut Scratch,
    out: &mut Vec<(&'static str, f64)>,
) -> Result<(), Error> {
    let ignore_non_latin = inclusion.contains(&"Latin Based");
    let restricted = inclusion.iter().any(|name| *name != "Latin Based");
    let mut results = std::mem::take(&mut scratch.results);
    results.clear();
    let mut sufficient = 0usize;

    scratch.split_layers(decoded);
    for layer in 0..scratch.layer_count {
        if scratch.tally_layer(layer) <= 32 {
            continue;
        }
        scratch.order_tally();
        if !restricted {
            alphabet_candidates(&scratch.ordered, ignore_non_latin, &mut scratch.candidates);
        }
        let mut consider = |language: &'static Language, scratch: &mut Scratch| {
            let ratio = scratch.popularity(language);
            if ratio < threshold {
                return false;
            }
            if ratio >= 0.8 {
                sufficient += 1;
            }
            results.push((language.name, pyfloat::round(ratio, 4)));
            sufficient >= 3
        };
        if restricted {
            for name in inclusion.iter().filter(|name| **name != "Latin Based") {
                match language(name) {
                    Ok(language) => {
                        if consider(language, scratch) {
                            break;
                        }
                    }
                    Err(error) => {
                        scratch.results = results;
                        return Err(error);
                    }
                }
            }
        } else {
            for index in 0..scratch.candidates.len() {
                let language = scratch.candidates[index].0;
                if consider(language, scratch) {
                    break;
                }
            }
        }
    }
    // Fold alternative profiles ("English—") into their base language when
    // a language appears more than once; their names only carry trailing em
    // dashes, so trimming yields the (static) base name.
    let groups = &mut scratch.groups;
    groups.clear();
    for &(language, ratio) in &results {
        let normalized = language.trim_end_matches('—');
        match groups.iter_mut().find(|group| group.0 == normalized) {
            Some(group) => {
                group.1 = group.1.max(ratio);
                group.2 += 1;
            }
            None => groups.push((normalized, ratio, 1)),
        }
    }
    let start = out.len();
    if groups.iter().all(|group| group.2 == 1) {
        out.extend_from_slice(&results);
    } else {
        out.extend(groups.iter().map(|&(language, best, _)| (language, best)));
    }
    out[start..].sort_by(|a, b| b.1.total_cmp(&a.1));
    scratch.results = results;
    Ok(())
}

/// Split text into lowercase layers of letters from compatible Unicode ranges.
#[must_use]
pub fn alpha_unicode_split(decoded: &str) -> Vec<String> {
    let mut scratch = Scratch::default();
    scratch.split_layers(decoded);
    scratch.layers[..scratch.layer_count]
        .iter()
        .map(|layer| layer.letters.iter().collect::<String>().to_lowercase())
        .collect()
}

#[cfg(test)]
#[expect(clippy::float_cmp, reason = "ratios must match the reference exactly")]
mod tests {
    use super::*;

    #[test]
    fn popularity_scores_ranked_input() {
        assert_eq!(
            characters_popularity_compare("English", &['e', 'e', 't', 'a']).unwrap(),
            0.25
        );
        assert!(
            characters_popularity_compare("English", &[])
                .unwrap()
                .is_nan()
        );
        assert!(characters_popularity_compare("Klingon", &['a']).is_err());
    }

    #[test]
    fn merges_and_filters() {
        assert_eq!(
            merge_coherence_ratios(&[
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

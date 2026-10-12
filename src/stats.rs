//! Persistent per-letter mistake tracking used to bias future tests toward
//! the letters the user struggles with.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::{fs, io};

/// How much old data is discounted every time a new test is recorded.
/// 0.9 means a mistake made ten tests ago counts for roughly a third of a
/// mistake made just now, so the drill follows your *current* weaknesses.
const DECAY: f64 = 0.9;

/// Bayesian-style smoothing so that a single miss on a rarely seen letter
/// does not dominate everything.
const PRIOR_MISSES: f64 = 0.5;
const PRIOR_ATTEMPTS: f64 = 5.0;

#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LetterStat {
    pub attempts: f64,
    pub misses: f64,
}

impl LetterStat {
    /// Smoothed probability of missing this letter.
    pub fn miss_rate(&self) -> f64 {
        (self.misses + PRIOR_MISSES) / (self.attempts + PRIOR_ATTEMPTS)
    }

    /// Raw accuracy, or `None` if the letter has never been attempted.
    pub fn accuracy(&self) -> Option<f64> {
        if self.attempts > 0.0 {
            Some(1.0 - self.misses / self.attempts)
        } else {
            None
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Stats {
    pub tests: u64,
    pub letters: BTreeMap<String, LetterStat>,
}

impl Stats {
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let body = toml::to_string(self).map_err(|e| io::Error::other(e))?;
        fs::write(path, body)
    }

    /// Fold the results of one test into the history.
    /// `results` is a list of (expected letter, was it typed correctly).
    pub fn record(&mut self, results: &[(char, bool)]) {
        if results.is_empty() {
            return;
        }
        self.tests += 1;
        for stat in self.letters.values_mut() {
            stat.attempts *= DECAY;
            stat.misses *= DECAY;
        }
        for &(c, correct) in results {
            let stat = self.letters.entry(c.to_string()).or_default();
            stat.attempts += 1.0;
            if !correct {
                stat.misses += 1.0;
            }
        }
    }

    pub fn letter(&self, c: char) -> LetterStat {
        let mut buf = [0u8; 4];
        self.letters
            .get(c.encode_utf8(&mut buf) as &str)
            .copied()
            .unwrap_or_default()
    }

    /// Weight for a candidate word: higher when it contains letters that are
    /// often missed. `strength` scales how aggressive the bias is (0 = none).
    pub fn word_weight(&self, word: &str, strength: f64) -> f64 {
        let base = 1.0;
        if strength <= 0.0 {
            return base;
        }
        let miss_sum: f64 = word
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| self.letter(c).miss_rate())
            .sum();
        base + strength * miss_sum
    }

    /// Letters sorted from worst to best accuracy, excluding ones with
    /// perfect (or no) history.
    pub fn weakest(&self, n: usize) -> Vec<(char, f64)> {
        let mut v: Vec<(char, f64)> = self
            .letters
            .iter()
            .filter_map(|(k, s)| {
                let c = k.chars().next()?;
                let acc = s.accuracy()?;
                (acc < 1.0 && s.attempts >= 1.0).then_some((c, acc))
            })
            .collect();
        v.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        v.truncate(n);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_counts_and_decays() {
        let mut s = Stats::default();
        s.record(&[('a', true), ('a', false), ('b', true)]);
        assert_eq!(s.tests, 1);
        assert_eq!(s.letter('a').attempts, 2.0);
        assert_eq!(s.letter('a').misses, 1.0);
        s.record(&[('b', false)]);
        assert!((s.letter('a').attempts - 2.0 * DECAY).abs() < 1e-9);
        assert_eq!(s.letter('b').misses, 1.0);
    }

    #[test]
    fn missed_letters_make_words_heavier() {
        let mut s = Stats::default();
        s.record(&[('q', false), ('q', false), ('e', true), ('e', true)]);
        assert!(s.word_weight("quiz", 10.0) > s.word_weight("tree", 10.0));
        assert_eq!(s.word_weight("quiz", 0.0), s.word_weight("tree", 0.0));
    }

    #[test]
    fn weakest_sorted_and_filtered() {
        let mut s = Stats::default();
        s.record(&[('a', false), ('b', true), ('b', false), ('c', true)]);
        let w = s.weakest(10);
        assert_eq!(w.iter().map(|x| x.0).collect::<Vec<_>>(), vec!['a', 'b']);
    }

    #[test]
    fn roundtrip_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.toml");
        let mut s = Stats::default();
        s.record(&[('ž', false)]);
        s.save(&path).unwrap();
        let loaded = Stats::load(&path);
        assert_eq!(loaded.letter('ž').misses, 1.0);
        assert_eq!(loaded.tests, 1);
    }
}

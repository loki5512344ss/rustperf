//! A word counter, written the slow way on purpose.
//!
//! This is the same `count_words` as `testdata/demo`, kept here so the PGO
//! reproduction in this directory is self-contained.

use std::collections::HashMap;

/// Returns how many times each lowercase word appears in `s`.
///
/// ```
/// let counts = pgo_demo::count_words("the quick brown the");
/// assert_eq!(counts["the"], 2);
/// assert_eq!(counts["quick"], 1);
/// ```
pub fn count_words(s: &str) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for field in s.split_whitespace() {
        let mut word = String::new();
        for c in field.chars() {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                // Quadratic on purpose: rebuilds the string every character.
                word = word + &c.to_string();
            }
        }
        if !word.is_empty() {
            *counts.entry(word).or_insert(0) += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_repeated_words() {
        let got = count_words("the quick brown the");
        assert_eq!(got["the"], 2);
        assert_eq!(got["quick"], 1);
        assert_eq!(got["brown"], 1);
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn strips_punctuation_and_folds_case() {
        let got = count_words("Hello, WORLD! hello?");
        assert_eq!(got["hello"], 2);
        assert_eq!(got["world"], 1);
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn digits_are_kept_and_empty_input_yields_nothing() {
        let got = count_words("abc 123 !!!");
        assert_eq!(got["abc"], 1);
        assert_eq!(got["123"], 1);
        assert_eq!(got.len(), 2);
        assert!(count_words("").is_empty());
    }
}

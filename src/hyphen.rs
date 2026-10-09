//! Hyphenation for the paperback PDF: Frank Liang's algorithm, the one TeX
//! uses, with the American English patterns from hyph-utf8
//! (`data/hyphenation/`, see its README for the copyright notice).
//!
//! Patterns like `.ach4` or `hy3ph` give scores between letters; a break
//! is allowed where the highest score is odd. [`Hyphenator::soft_hyphens`]
//! marks those places with U+00AD, which Pango shows as a hyphen only when
//! it breaks the line there.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Letters that must stay before and after a break (TeX's hyphenmins for
/// American English).
const LEFT: usize = 2;
const RIGHT: usize = 3;
/// Shorter words stay whole in the book: "ta-ble" is legal but looks poor.
const SHORTEST: usize = 6;

pub struct Hyphenator {
    patterns: HashMap<String, Vec<u8>>,
    longest: usize,
    exceptions: HashMap<String, Vec<usize>>,
}

pub fn en_us() -> &'static Hyphenator {
    static H: OnceLock<Hyphenator> = OnceLock::new();
    H.get_or_init(|| {
        Hyphenator::new(
            include_str!("../data/hyphenation/hyph-en-us.pat.txt"),
            include_str!("../data/hyphenation/hyph-en-us.hyp.txt"),
        )
    })
}

impl Hyphenator {
    pub fn new(patterns: &str, exceptions: &str) -> Hyphenator {
        let mut map = HashMap::new();
        let mut longest = 0;
        for pat in patterns.split_whitespace() {
            let mut letters = String::new();
            let mut scores = vec![0u8];
            for c in pat.chars() {
                match c.to_digit(10) {
                    Some(d) => {
                        let slot = scores
                            .last_mut()
                            .expect("pattern scores start with a slot and only grow");
                        *slot = d as u8;
                    }
                    None => {
                        letters.push(c);
                        scores.push(0);
                    }
                }
            }
            longest = longest.max(letters.chars().count());
            map.insert(letters, scores);
        }
        let exceptions = exceptions
            .split_whitespace()
            .map(|w| {
                let mut points = Vec::new();
                let mut n = 0;
                for c in w.chars() {
                    if c == '-' {
                        points.push(n);
                    } else {
                        n += 1;
                    }
                }
                (w.replace('-', ""), points)
            })
            .collect();
        Hyphenator {
            patterns: map,
            longest,
            exceptions,
        }
    }

    /// Where `word` may break: `i` means between its `i-1`th and `i`th
    /// letter. `word` must be lowercase letters.
    pub fn points(&self, word: &str) -> Vec<usize> {
        let n = word.chars().count();
        if n < LEFT + RIGHT {
            return Vec::new();
        }
        if let Some(p) = self.exceptions.get(word) {
            return p.clone();
        }
        let padded: Vec<char> = std::iter::once('.')
            .chain(word.chars())
            .chain(std::iter::once('.'))
            .collect();
        let mut scores = vec![0u8; padded.len() + 1];
        let mut key = String::new();
        for i in 0..padded.len() {
            key.clear();
            for &c in padded.iter().skip(i).take(self.longest) {
                key.push(c);
                if let Some(s) = self.patterns.get(&key) {
                    for (k, v) in s.iter().enumerate() {
                        scores[i + k] = scores[i + k].max(*v);
                    }
                }
            }
        }
        // scores[i + 1] sits between word letters i-1 and i (the leading
        // dot shifts everything by one).
        (LEFT..=n - RIGHT)
            .filter(|&i| scores[i + 1] % 2 == 1)
            .collect()
    }

    /// `text` with soft hyphens (U+00AD) at every allowed break. Only
    /// lowercase words of plain letters, six or more, are touched: names, acronyms and
    /// words already hyphenated are left whole, as a careful typesetter
    /// would.
    pub fn soft_hyphens(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len() + text.len() / 8);
        let mut word = String::new();
        let flush = |word: &mut String, out: &mut String| {
            if word.len() >= SHORTEST && word.chars().all(|c| c.is_ascii_lowercase()) {
                let points = self.points(word);
                for (i, c) in word.chars().enumerate() {
                    if points.contains(&i) {
                        out.push('\u{ad}');
                    }
                    out.push(c);
                }
            } else {
                out.push_str(word);
            }
            word.clear();
        };
        for c in text.chars() {
            if c.is_alphabetic() {
                word.push(c);
            } else {
                flush(&mut word, &mut out);
                out.push(c);
            }
        }
        flush(&mut word, &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hy(word: &str) -> String {
        let h = en_us();
        let points = h.points(word);
        word.chars()
            .enumerate()
            .flat_map(|(i, c)| {
                if points.contains(&i) {
                    vec!['-', c]
                } else {
                    vec![c]
                }
            })
            .collect()
    }

    #[test]
    fn knuths_examples() {
        assert_eq!(hy("hyphenation"), "hy-phen-ation");
        // "put-er" would leave two letters; three must follow a break.
        assert_eq!(hy("computer"), "com-puter");
        assert_eq!(hy("algorithm"), "al-go-rithm");
        assert_eq!(hy("possibilities"), "pos-si-bil-i-ties");
        assert_eq!(hy("windshield"), "wind-shield");
        // An exception from the .hyp list.
        assert_eq!(hy("associate"), "as-so-ciate");
        // TeX allows this; the paperback skips words this short.
        assert_eq!(hy("table"), "ta-ble");
        assert_eq!(en_us().soft_hyphens("a table"), "a table");
        assert!(!hy("unflinching").starts_with("u-"));
    }

    #[test]
    fn soft_hyphens_only_in_plain_words() {
        let out =
            en_us().soft_hyphens("The windshield wipers cleared Nick's CONCENTRATION, well-known.");
        assert_eq!(
            out,
            "The wind\u{ad}shield wipers cleared Nick's CONCENTRATION, well-known."
        );
        assert_eq!(
            out.replace('\u{ad}', ""),
            "The windshield wipers cleared Nick's CONCENTRATION, well-known."
        );
    }
}

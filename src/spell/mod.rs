//! Nothing here opens a project or writes a file. A word the author is still typing
//! is not a mark, and neither is any word while spelling is off or while
//! composition mode is hiding marks.

use std::ops::Range;

mod scan;

pub use scan::{
    LineMark, Scan, Slice, byte_of_chars, line_anchor_after_edit, next_scan, scan_index_after_edit,
    slice_window, take_slice,
};
/// How many characters a single lookup will accept.
const MAX_WORD: usize = 80;

/// How far a keystroke looks back across whitespace for the word just left.
const MAX_GAP: usize = 4_000;

/// What the window is doing right now, plus the choices remembered in settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub enabled: bool,
    pub composing: bool,
    /// When composing, marks stay hidden unless this is on.
    pub show_in_compose: bool,
    /// The word at the cursor stays unmarked until the cursor leaves it.
    pub hide_word_at_cursor: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: true,
            composing: false,
            show_in_compose: false,
            hide_word_at_cursor: true,
        }
    }
}

/// Marks are drawn only when spelling is on, and composition mode hides
/// them unless the author asked to see them there.
pub fn shown(policy: &Policy) -> bool {
    policy.enabled && (!policy.composing || policy.show_in_compose)
}

/// A word a keystroke should repaint, and the slices inside it that are misspelled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Touch {
    pub start: usize,
    pub end: usize,
    pub bad: Vec<Range<usize>>,
}

/// Apostrophes that can sit inside a word. U+2019 is also the right single quote.
fn is_apostrophe(c: char) -> bool {
    matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{02BC}')
}

fn is_hyphen(c: char) -> bool {
    matches!(c, '-' | '\u{2010}' | '\u{2011}')
}

fn is_combining_mark(c: char) -> bool {
    // `char::is_alphabetic` is false for nonspacing marks, so a decomposed
    // letter would otherwise split around the accent.
    matches!(
        c,
        '\u{0300}'..='\u{036F}'
            | '\u{0591}'..='\u{05BD}'
            | '\u{05BF}'
            | '\u{05C1}'..='\u{05C2}'
            | '\u{05C4}'..='\u{05C5}'
            | '\u{05C7}'
            | '\u{064B}'..='\u{065F}'
            | '\u{0670}'
            | '\u{1AB0}'..='\u{1AFF}'
            | '\u{1DC0}'..='\u{1DFF}'
            | '\u{20D0}'..='\u{20F0}'
            | '\u{3099}'..='\u{309A}'
            | '\u{FE20}'..='\u{FE2F}'
    )
}

pub fn is_word_char(c: char) -> bool {
    c.is_alphabetic()
        || c.is_ascii_digit()
        || is_apostrophe(c)
        || is_hyphen(c)
        || c == '\u{00AD}'
        || is_combining_mark(c)
}

fn floor(text: &str, index: usize) -> usize {
    let mut i = index.min(text.len());
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn char_before(text: &str, index: usize) -> Option<(usize, char)> {
    let index = floor(text, index);
    if index == 0 {
        return None;
    }
    let mut i = index - 1;
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    text[i..index].chars().next().map(|c| (i, c))
}

fn is_edge(c: char) -> bool {
    is_apostrophe(c) || is_hyphen(c) || c == '\u{00AD}'
}

fn trim_token(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    let mut s = start;
    let mut e = end.min(text.len());
    while s < e {
        let c = text[s..e].chars().next()?;
        if !is_edge(c) {
            break;
        }
        s += c.len_utf8();
    }
    while s < e {
        let c = text[s..e].chars().next_back()?;
        if !is_edge(c) {
            break;
        }
        e -= c.len_utf8();
    }
    if s >= e || !text[s..e].chars().any(char::is_alphabetic) {
        return None;
    }
    Some((s, e))
}

/// The word that begins at `cursor`, when the caret is on its first letter.
fn token_starting(text: &str, cursor: usize) -> Option<(usize, usize)> {
    let cursor = floor(text, cursor);
    let c = text[cursor..].chars().next()?;
    if !is_word_char(c) {
        return None;
    }
    token_touching(text, cursor + c.len_utf8())
}

/// The word that ends at `cursor` or contains it, including a trailing
/// apostrophe the author is still typing. `cursor` is a byte offset.
pub fn raw_word(text: &str, cursor: usize) -> Option<(usize, usize)> {
    let cursor = floor(text, cursor);
    if let Some(span) = raw_ending(text, cursor) {
        return Some(span);
    }
    let c = text[cursor..].chars().next()?;
    if !is_word_char(c) {
        return None;
    }
    raw_ending(text, cursor + c.len_utf8())
}

fn raw_ending(text: &str, cursor: usize) -> Option<(usize, usize)> {
    let cursor = floor(text, cursor);
    let (mut start, _) = char_before(text, cursor).filter(|(_, c)| is_word_char(*c))?;
    let mut chars = 1usize;
    while chars <= MAX_WORD {
        let Some((i, c)) = char_before(text, start) else {
            break;
        };
        if !is_word_char(c) {
            break;
        }
        start = i;
        chars += 1;
    }
    if chars > MAX_WORD {
        return None;
    }
    let mut end = cursor;
    while chars <= MAX_WORD {
        let Some(c) = text[end..].chars().next() else {
            break;
        };
        if !is_word_char(c) {
            break;
        }
        end += c.len_utf8();
        chars += 1;
    }
    if chars > MAX_WORD {
        return None;
    }
    Some((start, end))
}

/// The word that ends at `cursor` or contains it, with edge punctuation removed.
fn token_touching(text: &str, cursor: usize) -> Option<(usize, usize)> {
    let (start, end) = raw_ending(text, cursor)?;
    trim_token(text, start, end)
}

fn token_before(text: &str, before: usize) -> Option<(usize, usize)> {
    let mut pos = floor(text, before);
    let mut skipped = 0usize;
    loop {
        let (i, c) = char_before(text, pos)?;
        if is_word_char(c) {
            return token_touching(text, pos);
        }
        pos = i;
        skipped += 1;
        if skipped > MAX_GAP {
            return None;
        }
    }
}

/// The form handed to a dictionary: straight apostrophes, soft hyphens removed.
pub fn dictionary_form(word: &str) -> String {
    lookup(word, 0, word.len())
}

/// The key stored for Add to Dictionary. A possessive stores the bare word,
/// because a later `Zorvath` has to match the `Zorvath's` the author added.
pub fn dictionary_key(word: &str) -> String {
    let lower = dictionary_form(word).to_lowercase();
    match lower.strip_suffix("'s") {
        Some(stem) if stem.chars().any(char::is_alphabetic) => stem.to_string(),
        _ => lower,
    }
}

fn lookup(text: &str, start: usize, end: usize) -> String {
    text[start..end]
        .chars()
        .filter_map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{02BC}' => Some('\''),
            '\u{00AD}' => None,
            c => Some(c),
        })
        .collect()
}

fn accepts(word: &str, known: &mut impl FnMut(&str) -> bool) -> bool {
    if word.chars().count() > MAX_WORD {
        return true;
    }
    if word.is_empty() {
        return true;
    }
    if known(word) {
        return true;
    }
    let lower = word.to_lowercase();
    if lower != word && known(&lower) {
        return true;
    }
    if let Some(stem) = word
        .strip_suffix("'s")
        .or_else(|| word.strip_suffix("'S"))
        .filter(|stem| stem.chars().any(char::is_alphabetic))
    {
        return accepts(stem, known);
    }
    false
}

fn pieces(text: &str, start: usize, end: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut part = start;
    let mut i = start;
    while i < end {
        let Some(c) = text[i..end].chars().next() else {
            break;
        };
        let n = c.len_utf8();
        if is_hyphen(c) {
            if let Some((a, b)) = trim_token(text, part, i) {
                out.push((a, b));
            }
            part = i + n;
        }
        i += n;
    }
    if let Some((a, b)) = trim_token(text, part, end) {
        out.push((a, b));
    }
    out
}

fn bad_pieces(
    text: &str,
    start: usize,
    end: usize,
    known: &mut impl FnMut(&str) -> bool,
) -> Vec<Range<usize>> {
    let whole = lookup(text, start, end);
    if accepts(&whole, known) {
        return Vec::new();
    }
    let parts = pieces(text, start, end);
    if parts.len() <= 1 {
        return vec![Range { start, end }];
    }
    let mut bad = Vec::new();
    for (a, b) in parts {
        if !accepts(&lookup(text, a, b), known) {
            bad.push(a..b);
        }
    }
    bad
}

fn is_current(start: usize, end: usize, cursor: Option<usize>) -> bool {
    let Some(cursor) = cursor else {
        return false;
    };
    // The caret sits on the boundary before a letter and still belongs to that word.
    start <= cursor && cursor <= end
}

/// Misspelled slices in `start..end` (byte offsets). The walk starts at
/// `start`, rewinding at most one word, and stops at `end`.
pub fn bad_ranges(
    text: &str,
    start: usize,
    end: usize,
    cursor: Option<usize>,
    policy: &Policy,
    known: &mut impl FnMut(&str) -> bool,
) -> Vec<Range<usize>> {
    if !shown(policy) {
        return Vec::new();
    }
    let end = end.min(text.len());
    let mut i = token_touching(text, floor(text, start))
        .map(|(s, _)| s)
        .unwrap_or_else(|| floor(text, start));
    if i > end {
        i = floor(text, start);
    }
    let mut out = Vec::new();
    while i < end {
        let Some(c) = text[i..].chars().next() else {
            break;
        };
        if !is_word_char(c) {
            i += c.len_utf8();
            continue;
        }
        let raw = i;
        let mut chars = 0usize;
        let mut j = i;
        while j < text.len() && chars <= MAX_WORD {
            let Some(ch) = text[j..].chars().next() else {
                break;
            };
            if !is_word_char(ch) {
                break;
            }
            j += ch.len_utf8();
            chars += 1;
        }
        if chars > MAX_WORD {
            while j < text.len() {
                let Some(ch) = text[j..].chars().next() else {
                    break;
                };
                if !is_word_char(ch) {
                    break;
                }
                j += ch.len_utf8();
            }
            i = j;
            continue;
        }
        i = j;
        let Some((a, b)) = trim_token(text, raw, j) else {
            continue;
        };
        if a >= end {
            break;
        }
        let hide = policy.hide_word_at_cursor && is_current(raw, j, cursor);
        if hide {
            continue;
        }
        out.extend(bad_pieces(text, a, b, known));
    }
    out
}

/// The word at the cursor and the word just before it.
///
/// This walks back from `cursor` only, and looks at the word that begins
/// there when the caret is on its first letter. A document of any length
/// costs the same as those words.
pub fn keystroke_marks(
    text: &str,
    cursor: usize,
    policy: &Policy,
    known: &mut impl FnMut(&str) -> bool,
) -> Vec<Touch> {
    if !shown(policy) {
        return Vec::new();
    }
    let cursor = floor(text, cursor);
    let current_raw = raw_word(text, cursor);
    let current = current_raw.and_then(|(start, end)| trim_token(text, start, end));
    let previous = match current {
        Some((start, _)) => token_before(text, start),
        None => token_before(text, cursor),
    };
    let mut spans = Vec::new();
    for span in [previous, current, token_starting(text, cursor)]
        .into_iter()
        .flatten()
    {
        if !spans.contains(&span) {
            spans.push(span);
        }
    }
    let mut out = Vec::new();
    for (start, end) in spans {
        let hide = policy.hide_word_at_cursor
            && current_raw.is_some_and(|(raw_start, raw_end)| {
                is_current(raw_start, raw_end, Some(cursor)) && start >= raw_start && end <= raw_end
            });
        let bad = if hide {
            Vec::new()
        } else {
            bad_pieces(text, start, end, known)
        };
        out.push(Touch { start, end, bad });
    }
    out
}

/// `en_US.UTF-8` and `en-GB` become the tag Enchant asks a provider for.
pub fn tag_from_locale(raw: &str) -> String {
    let base = raw.split(['.', '@']).next().unwrap_or("").trim();
    if base.is_empty() || base == "C" || base == "POSIX" {
        return "en_US".to_string();
    }
    base.replace('-', "_")
}

/// Empty means the process locale (`LC_ALL`, then `LC_MESSAGES`, then `LANG`).
fn nonempty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

/// `setting` wins. Otherwise the first non-empty locale string, in the
/// order `LC_ALL`, `LC_MESSAGES`, `LANG`. An empty value is unset.
pub fn language_from_sources(setting: &str, locales: &[&str]) -> String {
    if let Some(raw) = nonempty(setting) {
        return tag_from_locale(raw);
    }
    for value in locales {
        if let Some(raw) = nonempty(value) {
            return tag_from_locale(raw);
        }
    }
    tag_from_locale("")
}

pub fn language_tag(setting: &str) -> String {
    let locales = [
        std::env::var("LC_ALL").unwrap_or_default(),
        std::env::var("LC_MESSAGES").unwrap_or_default(),
        std::env::var("LANG").unwrap_or_default(),
    ];
    let borrowed: Vec<&str> = locales.iter().map(String::as_str).collect();
    language_from_sources(setting, &borrowed)
}

/// Tags to try, most specific first, with English as the last resort.
pub fn dictionary_tags(tag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |s: &str| {
        if !s.is_empty() && !out.iter().any(|e: &String| e == s) {
            out.push(s.to_string());
        }
    };
    push(tag);
    if let Some((lang, _)) = tag.split_once('_') {
        push(lang);
    }
    push("en_US");
    push("en");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known_word(w: &str) -> bool {
        matches!(
            w,
            "the"
                | "don't"
                | "it's"
                | "mara"
                | "well"
                | "known"
                | "hello"
                | "world"
                | "writers"
                | "writer"
                | "n"
                | "roll"
                | "lighthouse"
                | "alpha"
        )
    }

    fn marked(text: &str, cursor: Option<usize>, policy: &Policy) -> Vec<String> {
        bad_ranges(text, 0, text.len(), cursor, policy, &mut known_word)
            .into_iter()
            .map(|r| text[r].to_string())
            .collect()
    }

    #[test]
    fn word_rules_keep_apostrophes_quotes_and_hyphens() {
        let policy = Policy {
            hide_word_at_cursor: false,
            ..Policy::default()
        };
        assert_eq!(marked("The helo.", None, &policy), vec!["helo"]);
        assert_eq!(marked("don\u{2019}t", None, &policy), Vec::<String>::new());
        assert_eq!(marked("it\u{2019}s", None, &policy), Vec::<String>::new());
        assert_eq!(
            marked("\u{201C}hello\u{201D}", None, &policy),
            Vec::<String>::new()
        );
        assert_eq!(marked("\u{201C}helo\u{201D}", None, &policy), vec!["helo"]);
        assert_eq!(marked("well-known", None, &policy), Vec::<String>::new());
        assert_eq!(marked("well-knwon", None, &policy), vec!["knwon"]);
        assert_eq!(marked("writers'", None, &policy), Vec::<String>::new());
        assert_eq!(marked("writer's", None, &policy), Vec::<String>::new());
        assert_eq!(marked("helo 'n' roll", None, &policy), vec!["helo"]);
        assert_eq!(marked("hel\u{00AD}lo", None, &policy), Vec::<String>::new());
        assert_eq!(marked("hello\u{2014}helo", None, &policy), vec!["helo"]);
        assert_eq!(marked("hello\u{2013}helo", None, &policy), vec!["helo"]);
    }

    #[test]
    fn an_ignored_word_is_not_a_misspelling() {
        let policy = Policy {
            hide_word_at_cursor: false,
            ..Policy::default()
        };
        let mut known = |w: &str| known_word(w) || w.eq_ignore_ascii_case("xyzzy");
        let bad = bad_ranges("the xyzzy", 0, 9, None, &policy, &mut known);
        assert!(bad.is_empty());
    }

    #[test]
    fn a_token_past_the_lookup_limit_is_not_marked() {
        let policy = Policy {
            hide_word_at_cursor: false,
            ..Policy::default()
        };
        let word = "q".repeat(90);
        assert_eq!(marked(&word, None, &policy), Vec::<String>::new());
        let sentence = format!("the {word} well");
        assert_eq!(marked(&sentence, None, &policy), Vec::<String>::new());
    }

    #[test]
    fn a_decomposed_letter_stays_inside_the_word() {
        let policy = Policy {
            hide_word_at_cursor: false,
            ..Policy::default()
        };
        assert_eq!(
            marked("nai\u{0308}ve", None, &policy),
            vec!["nai\u{0308}ve"]
        );
        assert_eq!(marked("cafe\u{0301}", None, &policy), vec!["cafe\u{0301}"]);
        assert_eq!(
            marked("\u{0643}\u{064E}\u{062A}\u{0628}", None, &policy),
            vec!["\u{0643}\u{064E}\u{062A}\u{0628}"]
        );
    }

    #[test]
    fn the_word_at_the_caret_includes_the_position_before_its_first_letter() {
        let hide = Policy::default();
        assert_eq!(marked("wrld", Some(0), &hide), Vec::<String>::new());
        assert_eq!(marked("the wrld", Some(4), &hide), Vec::<String>::new());
        assert_eq!(marked("the wrld", Some(8), &hide), Vec::<String>::new());
        assert_eq!(marked("the wrld", Some(3), &hide), vec!["wrld"]);
        let touches = keystroke_marks("wrld", 0, &hide, &mut known_word);
        assert_eq!(
            touches,
            vec![Touch {
                start: 0,
                end: 4,
                bad: Vec::new(),
            }]
        );
    }

    #[test]
    fn a_long_line_paints_whole_words_and_skips_an_overlong_token() {
        let policy = Policy {
            hide_word_at_cursor: false,
            ..Policy::default()
        };
        let q = "q".repeat(200);
        let text = format!("{}{q} {}", "helo ".repeat(280), "helo");
        let mut marks = Vec::new();
        let mut index = 0usize;
        let mut steps = 0usize;
        while let Some(scan) = next_scan(&text, index, 1_500) {
            steps += 1;
            assert!(steps < 20, "scan did not advance from {index}");
            assert!(scan.resume > index, "{scan:?} from {index}");
            assert!(scan.start <= scan.end && scan.end <= scan.resume);
            if scan.end > scan.start {
                let slice = &text[scan.start..scan.end];
                assert!(!slice.contains(&q));
                for range in bad_ranges(slice, 0, slice.len(), None, &policy, &mut known_word) {
                    let word = slice[range].to_string();
                    assert!(!word.contains('q'), "{word}");
                    marks.push(word);
                }
            }
            index = scan.resume;
        }
        assert_eq!(index, text.len());
        assert_eq!(marks.iter().filter(|m| *m == "helo").count(), 281);

        let mid = next_scan(&q, 10, 1_500).expect("a mid-token start still resumes");
        assert_eq!(mid.end, mid.start);
        assert_eq!(mid.resume, q.len());
    }

    #[test]
    fn the_word_being_typed_stays_clear_until_the_cursor_leaves() {
        let text = "helo world";
        let hide = Policy::default();
        let show = Policy {
            hide_word_at_cursor: false,
            ..Policy::default()
        };
        assert_eq!(marked(text, Some(4), &hide), Vec::<String>::new());
        assert_eq!(marked(text, Some(2), &hide), Vec::<String>::new());
        assert_eq!(marked(text, Some(5), &hide), vec!["helo"]);
        assert_eq!(marked(text, Some(4), &show), vec!["helo"]);
    }

    #[test]
    fn composition_hides_marks_unless_the_author_asks() {
        let mut policy = Policy::default();
        assert!(shown(&policy));
        policy.composing = true;
        assert!(!shown(&policy));
        assert!(marked("helo", None, &policy).is_empty());
        policy.show_in_compose = true;
        assert!(shown(&policy));
        assert_eq!(
            marked(
                "helo",
                None,
                &Policy {
                    hide_word_at_cursor: false,
                    ..policy
                }
            ),
            vec!["helo"]
        );
        policy.enabled = false;
        policy.composing = false;
        assert!(!shown(&policy));
        assert!(marked("helo", None, &policy).is_empty());
    }

    #[test]
    fn a_keystroke_checks_the_words_next_to_the_cursor() {
        let prefix = "alpha ".repeat(100_000);
        let text = format!("{prefix}xyzzy");
        let cursor = text.len();
        let calls = std::cell::Cell::new(0usize);
        let mut known = |w: &str| {
            calls.set(calls.get() + 1);
            w == "alpha" || w == "xyzzy"
        };
        let hide = Policy::default();
        let touches = keystroke_marks(&text, cursor, &hide, &mut known);
        assert!(
            touches.iter().all(|t| t.bad.is_empty()),
            "the word at the cursor stays clear"
        );
        assert!(calls.get() <= 4, "looked up {} words", calls.get());

        let text = format!("{prefix}xyzzy ");
        calls.set(0);
        let mut known = |w: &str| {
            calls.set(calls.get() + 1);
            w == "alpha"
        };
        let touches = keystroke_marks(&text, text.len(), &hide, &mut known);
        let bad: Vec<&str> = touches
            .iter()
            .flat_map(|t| t.bad.iter().map(|r| &text[r.clone()]))
            .collect();
        assert_eq!(bad, vec!["xyzzy"]);
        assert!(calls.get() <= 4, "looked up {} words", calls.get());

        let mut times = Vec::new();
        for _ in 0..21 {
            let start = std::time::Instant::now();
            let touches = keystroke_marks(&text, text.len(), &hide, &mut known);
            times.push(start.elapsed());
            assert!(
                touches
                    .iter()
                    .any(|t| t.bad.iter().any(|r| &text[r.clone()] == "xyzzy"))
            );
        }
        times.sort();
        let median = times[10];
        eprintln!(
            "spell keystroke median {median:?} min {:?} max {:?}",
            times[0], times[20]
        );
        assert!(
            median.as_micros() < 5_000,
            "keystroke median {median:?} on a 100k-word document"
        );
    }

    #[test]
    fn locale_tags_drop_the_encoding_and_keep_the_language() {
        assert_eq!(tag_from_locale("en_US.UTF-8"), "en_US");
        assert_eq!(tag_from_locale("en-GB"), "en_GB");
        assert_eq!(tag_from_locale("fr_FR@euro"), "fr_FR");
        assert_eq!(tag_from_locale("C"), "en_US");
        assert_eq!(tag_from_locale(""), "en_US");
        assert_eq!(language_tag("en_GB"), "en_GB");
        assert_eq!(
            dictionary_tags("en_GB"),
            vec!["en_GB".to_string(), "en".to_string(), "en_US".to_string()]
        );
        assert_eq!(
            dictionary_tags("de_DE"),
            vec![
                "de_DE".to_string(),
                "de".to_string(),
                "en_US".to_string(),
                "en".to_string()
            ]
        );
        assert_eq!(language_from_sources("", &["", "de_DE.UTF-8"]), "de_DE");
        assert_eq!(language_from_sources("en-GB", &[""]), "en_GB");
    }

    #[test]
    fn an_ordinal_is_looked_up_whole() {
        let policy = Policy {
            hide_word_at_cursor: false,
            ..Policy::default()
        };
        let mut known = |w: &str| {
            matches!(
                w.to_lowercase().as_str(),
                "on" | "the" | "and" | "of" | "may" | "4th" | "22nd" | "3rd" | "21st"
            )
        };
        let text = "On the 4th and 22nd of May, the 3rd and 21st.";
        let bad: Vec<&str> = bad_ranges(text, 0, text.len(), None, &policy, &mut known)
            .into_iter()
            .map(|range| &text[range])
            .collect();
        assert!(bad.is_empty(), "{bad:?}");
    }

    #[test]
    fn a_trailing_apostrophe_is_still_the_word_being_typed() {
        let hide = Policy::default();
        assert_eq!(marked("isn'", Some(4), &hide), Vec::<String>::new());
        assert_eq!(marked("isn' well", Some(5), &hide), vec!["isn"]);
    }

    #[test]
    fn a_possessive_added_to_the_dictionary_stores_the_bare_word() {
        assert_eq!(dictionary_key("Zorvath's"), "zorvath");
        assert_eq!(dictionary_key("Zorvath\u{2019}s"), "zorvath");
        assert_eq!(dictionary_key("Zorvath"), "zorvath");
        let policy = Policy {
            hide_word_at_cursor: false,
            ..Policy::default()
        };
        let mut known = |w: &str| w.eq_ignore_ascii_case("zorvath");
        assert!(bad_ranges("Zorvath", 0, 7, None, &policy, &mut known).is_empty());
        assert!(bad_ranges("Zorvath's", 0, 9, None, &policy, &mut known).is_empty());
    }
}

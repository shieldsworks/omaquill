//! Idle slices over one line.
//!
//! The scan index moves back to the word around an edit. It does not move
//! forward, so a keystroke ahead of the scan cannot skip unchecked text.

#[cfg(test)]
use std::cell::Cell;

use super::{MAX_WORD, char_before, floor, is_word_char};

/// One idle pass over a line. `start..end` holds only whole tokens.
/// `resume` is the next byte index, past an overlong token when one was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scan {
    pub start: usize,
    pub end: usize,
    pub resume: usize,
}

/// `(start, end, chars)` of the token that contains `at`, if `at` is inside one.
fn token_extent(text: &str, at: usize) -> Option<(usize, usize, usize)> {
    let at = floor(text, at);
    let inside = char_before(text, at).is_some_and(|(_, c)| is_word_char(c))
        || text[at..].chars().next().is_some_and(is_word_char);
    if !inside {
        return None;
    }
    let mut start = at;
    while let Some((i, _)) = char_before(text, start).filter(|(_, c)| is_word_char(*c)) {
        start = i;
    }
    let mut chars = 0usize;
    let mut end = start;
    while end < text.len() {
        let Some(ch) = text[end..].chars().next() else {
            break;
        };
        if !is_word_char(ch) {
            break;
        }
        end += ch.len_utf8();
        chars += 1;
    }
    (chars > 0).then_some((start, end, chars))
}

/// The next piece of `text` to paint, starting at byte `index`.
///
/// A pass stops on a token boundary after about `budget` characters. A token
/// longer than the lookup limit is not painted, and `resume` lands after it.
pub fn next_scan(text: &str, index: usize, budget: usize) -> Option<Scan> {
    let origin = floor(text, index);
    if origin >= text.len() || budget == 0 {
        return None;
    }
    let mut index = origin;
    if let Some((start, end, chars)) = token_extent(text, origin)
        && start < origin
        && origin < end
    {
        if chars > MAX_WORD {
            return Some(Scan {
                start: origin,
                end: origin,
                resume: end,
            });
        }
        index = start;
    }
    let start = index;
    let mut end = index;
    let mut counted = 0usize;
    let mut i = index;
    while i < text.len() && counted < budget {
        let Some(c) = text[i..].chars().next() else {
            break;
        };
        if !is_word_char(c) {
            i += c.len_utf8();
            end = i;
            counted += 1;
            continue;
        }
        let Some((token_start, token_end, chars)) = token_extent(text, i) else {
            break;
        };
        if chars > MAX_WORD {
            return Some(Scan {
                start,
                end,
                resume: token_end,
            });
        }
        if counted > 0 && counted + chars > budget {
            return Some(Scan {
                start,
                end,
                resume: token_start,
            });
        }
        end = token_end;
        counted += chars;
        i = token_end;
    }
    let resume = i.max(end);
    if resume <= origin {
        return None;
    }
    Some(Scan { start, end, resume })
}

#[cfg(test)]
thread_local! {
    static MARK_VISITS: Cell<u64> = const { Cell::new(0) };
}

fn note_mark(n: u64) {
    #[cfg(test)]
    MARK_VISITS.with(|visits| visits.set(visits.get().saturating_add(n)));
    #[cfg(not(test))]
    let _ = n;
}

#[cfg(test)]
fn reset_mark_visits() {
    MARK_VISITS.with(|visits| visits.set(0));
}

#[cfg(test)]
fn mark_visits() -> u64 {
    MARK_VISITS.with(|visits| visits.get())
}

/// Character index of the word that contains an edit at `edit_at`, minus
/// one word of context. `delta` is the change in character count: an
/// insertion's new characters sit just before the caret, and a deletion
/// or a same-length replacement sits at the caret.
///
/// This is not the scan index. [`scan_index_after_edit`] moves the scan
/// backward to this anchor and never forward past unchecked text.
pub fn line_anchor_after_edit(edit_at: i32, delta: i32) -> i32 {
    let edit_at = edit_at.max(0);
    let touched = if delta > 0 {
        edit_at.saturating_sub(delta)
    } else {
        edit_at
    };
    let word = i32::try_from(MAX_WORD).unwrap_or(i32::MAX);
    touched.saturating_sub(word).max(0)
}

/// Where the idle scan resumes after an edit on its current line.
///
/// `scan_at` is the first character that has not been checked. The index
/// moves back to the word around the edit. It does not move forward, so a
/// keystroke ahead of the scan cannot skip the unchecked range between them.
pub fn scan_index_after_edit(scan_at: i32, edit_at: i32, delta: i32) -> i32 {
    let scan_at = scan_at.max(0);
    scan_at.min(line_anchor_after_edit(edit_at, delta))
}

/// Character range to copy for one idle slice. `index` is the character to
/// resume from. The range keeps `MAX_WORD` characters of the token that
/// may start before `index` or end after the budget.
pub fn slice_window(line_chars: usize, index: usize, budget: usize) -> (usize, usize) {
    let index = index.min(line_chars);
    let start = index.saturating_sub(MAX_WORD);
    let end = index
        .saturating_add(budget)
        .saturating_add(MAX_WORD)
        .min(line_chars);
    (start, end)
}

/// Byte offset of character `chars` in `text`.
pub fn byte_of_chars(text: &str, chars: usize) -> usize {
    let mut byte = 0usize;
    for (n, ch) in text.chars().enumerate() {
        if n >= chars {
            break;
        }
        byte += ch.len_utf8();
    }
    byte
}

/// Char and byte offsets into one line. `Session::pump` keeps a mark and
/// moves it forward, so the next idle slice does not reread the prefix.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LineMark {
    chars: usize,
    byte: usize,
}

impl LineMark {
    pub fn chars(self) -> usize {
        self.chars
    }

    pub fn byte(self) -> usize {
        self.byte
    }

    pub fn resume(chars: usize, byte: usize) -> Self {
        Self { chars, byte }
    }

    fn byte_for(&mut self, text: &str, chars: usize) -> usize {
        if chars < self.chars || self.byte > text.len() {
            self.chars = 0;
            self.byte = 0;
        }
        let mut byte = self.byte;
        let mut have = self.chars;
        while have < chars {
            let Some(ch) = text[byte..].chars().next() else {
                break;
            };
            byte += ch.len_utf8();
            have += 1;
            note_mark(1);
        }
        self.chars = have;
        self.byte = byte;
        byte
    }

    fn chars_for(&mut self, text: &str, byte: usize) -> usize {
        let byte = floor(text, byte);
        if byte < self.byte {
            self.chars = 0;
            self.byte = 0;
        }
        let mut at = self.byte;
        let mut chars = self.chars;
        while at < byte {
            let Some(ch) = text[at..].chars().next() else {
                break;
            };
            let next = at + ch.len_utf8();
            if next > byte {
                break;
            }
            at = next;
            chars += 1;
            note_mark(1);
        }
        self.byte = at;
        self.chars = chars;
        chars
    }
}

/// Offsets of one idle slice. `start`, `end`, and `resume` are character
/// offsets into `text`, matching [`Scan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slice {
    pub start_chars: usize,
    pub end_chars: usize,
    pub resume_chars: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub resume_byte: usize,
}

/// The next idle slice of `text`, starting where `mark` left off.
pub fn take_slice(text: &str, mark: &mut LineMark, budget: usize) -> Option<Slice> {
    let origin = mark.byte_for(text, mark.chars);
    let scan = next_scan(text, origin, budget)?;
    let start_chars = mark.chars_for(text, scan.start);
    let end_chars = mark.chars_for(text, scan.end);
    let resume_chars = mark.chars_for(text, scan.resume);
    Some(Slice {
        start_chars,
        end_chars,
        resume_chars,
        start_byte: scan.start,
        end_byte: scan.end,
        resume_byte: scan.resume,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_line_slices_read_each_character_once() {
        let text = "helo ".repeat(40_000);
        reset_mark_visits();
        let started = std::time::Instant::now();
        let mut mark = LineMark::default();
        let mut slices = 0usize;
        let mut previous = 0usize;
        while let Some(slice) = take_slice(&text, &mut mark, 1_500) {
            assert!(slice.resume_byte >= previous);
            assert_eq!(slice.resume_chars, mark.chars());
            previous = slice.resume_byte;
            slices += 1;
            assert!(slices <= 400, "slice {slices} did not reach the end");
        }
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(mark.byte(), text.len());
        assert_eq!(mark.chars(), text.chars().count());
        let visits = mark_visits();
        let len = u64::try_from(text.len()).unwrap();
        assert!(
            visits <= len.saturating_mul(4),
            "visited {visits} of {len} across {slices} slices"
        );
        eprintln!("spell visits={visits} chars={len} slices={slices} ms={ms:.1}");
    }

    #[test]
    fn carried_offsets_match_a_count_from_the_start() {
        let text = "héllo — wörld ".repeat(40);
        let mut mark = LineMark::default();
        let mut seen = 0usize;
        while let Some(slice) = take_slice(&text, &mut mark, 17) {
            let start = text[..slice.start_byte].chars().count();
            let end = text[..slice.end_byte].chars().count();
            let resume = text[..slice.resume_byte].chars().count();
            assert_eq!(slice.start_chars, start);
            assert_eq!(slice.end_chars, end);
            assert_eq!(slice.resume_chars, resume);
            assert_eq!(mark.chars(), resume);
            assert_eq!(mark.byte(), slice.resume_byte);
            seen += 1;
            assert!(seen < 10_000);
        }
        assert_eq!(mark.byte(), text.len());
    }

    #[test]
    fn a_keystroke_ahead_of_the_scan_does_not_skip_unchecked_text() {
        let scan_at = 100;
        let ahead = scan_index_after_edit(scan_at, 4_000, 1);
        assert!(
            ahead <= scan_at,
            "a keystroke at 4000 moved the scan from {scan_at} to {ahead}"
        );
        let behind = scan_index_after_edit(4_000, 500, 1);
        assert!(behind < 4_000);
        assert_eq!(behind, line_anchor_after_edit(500, 1));
        assert_eq!(scan_index_after_edit(0, 4_000, 1), 0);
    }

    #[test]
    fn a_keystroke_anchors_near_the_edit() {
        let word = i32::try_from(MAX_WORD).unwrap();
        let inserted = line_anchor_after_edit(4_000, 1);
        assert_ne!(inserted, 0, "a keystroke restarted the line");
        assert_eq!(inserted, 4_000 - 1 - word);
        assert_eq!(line_anchor_after_edit(4_000, 0), 4_000 - word);
        assert_eq!(line_anchor_after_edit(10, 1), 0);

        // The stored byte belongs to the previous text. A leading `é`
        // shifts every later byte, so the resume byte is counted on the
        // new line at the anchor, not reused from the old scan.
        let line = format!("é{}", "alpha ".repeat(4_000));
        let anchor = usize::try_from(inserted).unwrap();
        let byte = byte_of_chars(&line, anchor);
        assert_ne!(
            byte, 4_000,
            "byte 4000 is not character {anchor} after a leading é"
        );
        reset_mark_visits();
        let started = std::time::Instant::now();
        let mut from_anchor = LineMark::resume(anchor, byte);
        let got = take_slice(&line, &mut from_anchor, 1_500).expect("anchor slice");
        assert!(
            got.start_chars + MAX_WORD >= anchor && got.start_chars <= anchor,
            "slice started at {} for an edit near {anchor}",
            got.start_chars
        );
        assert!(
            got.start_chars > 0,
            "the slice reread the line from the start"
        );
        let mut stale = LineMark::resume(4_000, 4_000);
        let wrong = take_slice(&line, &mut stale, 1_500).expect("stale slice");
        let at = line[..wrong.start_byte].chars().count();
        assert_ne!(at, 4_000, "byte 4000 is character {at} after a leading é");
        let visits = mark_visits();
        let len = u64::try_from(line.chars().count()).unwrap();
        assert!(visits <= len.saturating_mul(4), "visited {visits} of {len}");
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        eprintln!("anchor visits={visits} chars={len} ms={ms:.1}");
    }

    #[test]
    fn long_line_windows_copy_a_linear_amount() {
        let words = "alpha ".repeat(40);
        let chars: Vec<char> = words.chars().collect();
        let index = 100usize;
        let (start, end) = slice_window(chars.len(), index, 40);
        let window: String = chars[start..end].iter().collect();
        let local = index - start;
        let mut mark = LineMark::resume(local, byte_of_chars(&window, local));
        let slice = take_slice(&window, &mut mark, 40).expect("window slice");
        let painted: String = window
            .chars()
            .skip(slice.start_chars)
            .take(slice.end_chars.saturating_sub(slice.start_chars))
            .collect();
        assert!(
            painted.starts_with("alpha"),
            "window dropped the token around index {index}: {painted}"
        );

        let line_chars = 200_000usize;
        let budget = 1_500usize;
        let mut index = 0usize;
        let mut copied = 0usize;
        let mut slices = 0usize;
        let started = std::time::Instant::now();
        while index < line_chars {
            let (start, end) = slice_window(line_chars, index, budget);
            let window = end.saturating_sub(start);
            assert!(
                window <= budget + 2 * MAX_WORD,
                "window {window} on slice {slices}"
            );
            copied += window;
            let next = index.saturating_add(budget).min(line_chars);
            assert!(next > index, "slice {slices} did not advance");
            index = next;
            slices += 1;
            assert!(slices <= line_chars, "slice {slices} did not reach the end");
        }
        let len = u64::try_from(line_chars).unwrap();
        let copied = u64::try_from(copied).unwrap();
        assert!(
            copied <= len.saturating_mul(4),
            "copied {copied} of {len} across {slices} slices"
        );
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        eprintln!("window copied={copied} chars={len} slices={slices} ms={ms:.1}");
    }
}

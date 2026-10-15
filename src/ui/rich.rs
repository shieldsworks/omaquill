//! [`RichText`] into a `GtkTextBuffer` and back.
//!
//! Formatting lives in text tags whose *names* carry the exact values from
//! the file: `c:size=24`, `p:fi=720`, `c:font=Palatino`. Saving reads the
//! names, never the on-screen properties, so zoom and font substitution can
//! change how text looks without changing what gets written.
//!
//! Tags starting `c:` are character formatting; `p:` tags cover a whole
//! paragraph, its newline included, so they follow the paragraph as it's
//! split and joined.

use gtk::prelude::*;
use gtk::{TextBuffer, TextIter, TextTag, pango};
use omaquill::rtf::{Align, CharStyle, LineSpacing, ParaStyle, Paragraph, RichText, Run, Script};
#[cfg(test)]
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

#[cfg(test)]
thread_local! {
    static INSERTS: Cell<u64> = const { Cell::new(0) };
}

fn note_insert(n: u64) {
    #[cfg(test)]
    INSERTS.with(|inserts| inserts.set(inserts.get().saturating_add(n)));
    #[cfg(not(test))]
    let _ = n;
}

#[cfg(test)]
pub(crate) fn reset_inserts() {
    INSERTS.with(|inserts| inserts.set(0));
}

#[cfg(test)]
pub(crate) fn inserts() -> u64 {
    INSERTS.with(|inserts| inserts.get())
}

thread_local! {
    /// How tags are drawn: zoom and which installed font stands in for each
    /// family. Shared by every buffer so a preference change reaches all.
    static LOOK: RefCell<Look> = RefCell::new(Look::default());
}

#[derive(Clone)]
pub struct Look {
    pub zoom: f64,
    /// Show every run in this family instead of the document's fonts.
    pub font_override: Option<String>,
    /// Family name → the installed family drawn in its place.
    pub substitutes: HashMap<String, String>,
}

impl Default for Look {
    fn default() -> Self {
        Look {
            zoom: 1.0,
            font_override: None,
            substitutes: HashMap::new(),
        }
    }
}

pub fn set_look(look: Look) {
    LOOK.with(|l| *l.borrow_mut() = look);
}

pub fn look() -> Look {
    LOOK.with(|l| l.borrow().clone())
}

fn luminance(c: &gtk::gdk::RGBA) -> f32 {
    0.2126 * c.red() + 0.7152 * c.green() + 0.0722 * c.blue()
}

fn px(twips: i32, zoom: f64) -> i32 {
    // 20 twips a point, 96/72 pixels a point.
    (twips as f64 / 15.0 * zoom).round() as i32
}

/// Sets a tag's display properties from its name.
fn style_tag(tag: &TextTag, name: &str, look: &Look) {
    let zoom = look.zoom;
    let Some((kind, rest)) = name.split_once(':') else {
        return;
    };
    let (key, value) = rest.split_once('=').unwrap_or((rest, ""));
    let int = || value.parse::<i32>().unwrap_or(0);
    match (kind, key) {
        ("c", "b") => tag.set_weight(700),
        ("c", "i") => tag.set_style(pango::Style::Italic),
        ("c", "u") => tag.set_underline(pango::Underline::Single),
        ("c", "s") => tag.set_strikethrough(true),
        ("c", "sup") => {
            tag.set_rise((4.0 * zoom * pango::SCALE as f64) as i32);
            tag.set_scale(0.7);
        }
        ("c", "sub") => {
            tag.set_rise((-3.0 * zoom * pango::SCALE as f64) as i32);
            tag.set_scale(0.7);
        }
        ("c", "font") => {
            let family = look
                .font_override
                .clone()
                .or_else(|| look.substitutes.get(value).cloned())
                .unwrap_or_else(|| value.to_string());
            tag.set_family(Some(&family));
        }
        ("c", "size") => tag.set_size_points(int() as f64 / 2.0 * zoom),
        // Black text and white highlights are what a Mac page looks like;
        // on screen they'd vanish or glare in a dark theme, so they're kept
        // in the tag name for the file but not drawn.
        ("c", "fg") => {
            if let Ok(c) = gtk::gdk::RGBA::parse(value)
                && luminance(&c) > 0.2
            {
                tag.set_foreground_rgba(Some(&c));
            }
        }
        ("c", "bg") => {
            if let Ok(c) = gtk::gdk::RGBA::parse(value)
                && luminance(&c) < 0.9
            {
                let mut c = c;
                c.set_alpha(0.45);
                tag.set_background_rgba(Some(&c));
            }
        }
        // Links show underlined; a Scrivener comment or footnote anchor
        // shows as a highlight, as it does in Scrivener.
        ("c", "link") => {
            if value.starts_with("scrivcmt:") {
                if let Ok(c) = gtk::gdk::RGBA::parse("rgba(224, 175, 104, 0.35)") {
                    tag.set_background_rgba(Some(&c));
                }
            } else {
                tag.set_underline(pango::Underline::Single);
            }
        }
        ("p", "tabs") => {
            let stops: Vec<i32> = value
                .split(',')
                .filter_map(|t| t.get(1..).and_then(|n| n.parse().ok()))
                .collect();
            let mut tabs = pango::TabArray::new(stops.len() as i32, true);
            for (i, t) in stops.iter().enumerate() {
                tabs.set_tab(i as i32, pango::TabAlign::Left, px(*t, zoom));
            }
            tag.set_tabs(Some(&tabs));
        }
        ("p", "align") => tag.set_justification(match value {
            "center" => gtk::Justification::Center,
            "right" => gtk::Justification::Right,
            "justify" => gtk::Justification::Fill,
            _ => gtk::Justification::Left,
        }),
        ("p", "fi") => tag.set_indent(px(int(), zoom)),
        ("p", "li") => tag.set_left_margin(px(int(), zoom)),
        ("p", "ri") => tag.set_right_margin(px(int(), zoom)),
        ("p", "sb") => tag.set_pixels_above_lines(px(int(), zoom)),
        ("p", "sa") => tag.set_pixels_below_lines(px(int(), zoom)),
        ("p", "ls") => {
            // m1.5: a multiple. a/e: at least / exactly N twips, shown as the
            // multiple of 12 pt it amounts to.
            let (mode, n) = value.split_at(1);
            let n: f64 = n.parse().unwrap_or(1.0);
            let multiple = match mode {
                "m" => n,
                _ => n / 20.0 / (12.0 * 1.2),
            };
            tag.set_line_height(multiple.clamp(0.5, 4.0) as f32);
        }
        _ => {}
    }
}

/// The tag called `name`, made on first use.
pub fn tag(buffer: &TextBuffer, name: &str) -> TextTag {
    let table = buffer.tag_table();
    if let Some(t) = table.lookup(name) {
        return t;
    }
    let t = TextTag::new(Some(name));
    style_tag(&t, name, &look());
    table.add(&t);
    t
}

/// Re-applies zoom and fonts to every tag in the buffer.
pub fn restyle(buffer: &TextBuffer) {
    let look = look();
    let mut tags = Vec::new();
    buffer.tag_table().foreach(|t| tags.push(t.clone()));
    for t in tags {
        if let Some(name) = t.name() {
            let name = name.to_string();
            if name.starts_with("c:") || name.starts_with("p:") {
                // Clear first: a property set before stays set otherwise.
                for prop in [
                    "family-set",
                    "size-set",
                    "indent-set",
                    "left-margin-set",
                    "right-margin-set",
                    "pixels-above-lines-set",
                    "pixels-below-lines-set",
                    "line-height-set",
                    "tabs-set",
                    "underline-set",
                    "background-set",
                    "rise-set",
                ] {
                    if t.find_property(prop).is_some() {
                        t.set_property(prop, false);
                    }
                }
                style_tag(&t, &name, &look);
            }
        }
    }
}

fn hex(c: (u8, u8, u8)) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

fn unhex(s: &str) -> Option<(u8, u8, u8)> {
    let s = s.strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(s, 16).ok()?;
    Some(((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

pub fn char_tags(s: &CharStyle) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(f) = &s.font {
        out.push(format!("c:font={f}"));
    }
    out.push(format!("c:size={}", s.size));
    if s.bold {
        out.push("c:b".into());
    }
    if s.italic {
        out.push("c:i".into());
    }
    if s.underline {
        out.push("c:u".into());
    }
    if s.strike {
        out.push("c:s".into());
    }
    match s.script {
        Script::Super => out.push("c:sup".into()),
        Script::Sub => out.push("c:sub".into()),
        Script::Normal => {}
    }
    if let Some(c) = s.color {
        out.push(format!("c:fg={}", hex(c)));
    }
    if let Some(c) = s.highlight {
        out.push(format!("c:bg={}", hex(c)));
    }
    if let Some(l) = &s.link {
        out.push(format!("c:link={l}"));
    }
    out
}

pub fn para_tags(s: &ParaStyle) -> Vec<String> {
    let mut out = Vec::new();
    match s.align {
        Align::Natural => {}
        Align::Left => out.push("p:align=left".into()),
        Align::Center => out.push("p:align=center".into()),
        Align::Right => out.push("p:align=right".into()),
        Align::Justify => out.push("p:align=justify".into()),
    }
    for (key, v) in [
        ("fi", s.first_indent),
        ("li", s.left_indent),
        ("ri", s.right_indent),
        ("sb", s.space_before),
        ("sa", s.space_after),
    ] {
        if v != 0 {
            out.push(format!("p:{key}={v}"));
        }
    }
    match s.line_spacing {
        LineSpacing::Single => {}
        LineSpacing::Multiple(m) => out.push(format!("p:ls=m{m}")),
        LineSpacing::AtLeast(t) => out.push(format!("p:ls=a{t}")),
        LineSpacing::Exactly(t) => out.push(format!("p:ls=e{t}")),
    }
    if !s.tabs.is_empty() {
        let stops: Vec<String> = s.tabs.iter().map(|(at, k)| format!("{k}{at}")).collect();
        out.push(format!("p:tabs={}", stops.join(",")));
    }
    out
}

fn shared_link(links: &mut HashMap<String, Rc<str>>, value: &str) -> Rc<str> {
    if let Some(link) = links.get(value) {
        return Rc::clone(link);
    }
    let link = Rc::<str>::from(value);
    links.insert(value.to_string(), Rc::clone(&link));
    link
}

fn char_style(names: &[String], links: &mut HashMap<String, Rc<str>>) -> CharStyle {
    let mut s = CharStyle::default();
    for n in names {
        let Some(rest) = n.strip_prefix("c:") else {
            continue;
        };
        let (key, value) = rest.split_once('=').unwrap_or((rest, ""));
        match key {
            "b" => s.bold = true,
            "i" => s.italic = true,
            "u" => s.underline = true,
            "s" => s.strike = true,
            "sup" => s.script = Script::Super,
            "sub" => s.script = Script::Sub,
            "font" => s.font = Some(value.to_string()),
            "size" => s.size = value.parse().unwrap_or(24),
            "fg" => s.color = unhex(value),
            "bg" => s.highlight = unhex(value),
            "link" => s.link = Some(shared_link(links, value)),
            _ => {}
        }
    }
    s
}

fn para_style(names: &[String]) -> ParaStyle {
    let mut s = ParaStyle::default();
    for n in names {
        let Some(rest) = n.strip_prefix("p:") else {
            continue;
        };
        let Some((key, value)) = rest.split_once('=') else {
            continue;
        };
        let int = || value.parse().unwrap_or(0);
        match key {
            "align" => {
                s.align = match value {
                    "left" => Align::Left,
                    "center" => Align::Center,
                    "right" => Align::Right,
                    "justify" => Align::Justify,
                    _ => Align::Natural,
                }
            }
            "fi" => s.first_indent = int(),
            "li" => s.left_indent = int(),
            "ri" => s.right_indent = int(),
            "sb" => s.space_before = int(),
            "sa" => s.space_after = int(),
            "tabs" => {
                s.tabs = value
                    .split(',')
                    .filter_map(|t| {
                        let kind = t.chars().next()?;
                        Some((t[kind.len_utf8()..].parse().ok()?, kind))
                    })
                    .collect();
            }
            "ls" => {
                let (mode, n) = value.split_at(1);
                s.line_spacing = match mode {
                    "m" => LineSpacing::Multiple(n.parse().unwrap_or(1.0)),
                    "a" => LineSpacing::AtLeast(n.parse().unwrap_or(0)),
                    "e" => LineSpacing::Exactly(n.parse().unwrap_or(0)),
                    _ => LineSpacing::Single,
                }
            }
            _ => {}
        }
    }
    s
}

pub fn tag_names(iter: &TextIter) -> Vec<String> {
    iter.tags()
        .iter()
        .filter_map(|t| t.name().map(|n| n.to_string()))
        .collect()
}

/// Tag names for one character style. The `c:link=` name is built once per
/// shared target and reused for every later run.
fn style_names(style: &CharStyle, links: &mut HashMap<*const str, Rc<str>>) -> Vec<Rc<str>> {
    let link = style.link.clone();
    let mut bare = style.clone();
    bare.link = None;
    let mut names: Vec<Rc<str>> = char_tags(&bare).into_iter().map(Rc::from).collect();
    if let Some(link) = link {
        let key = Rc::as_ptr(&link);
        let name = links
            .entry(key)
            .or_insert_with(|| {
                let mut tagged = String::from("c:link=");
                tagged.push_str(&link);
                Rc::from(tagged)
            })
            .clone();
        names.push(name);
    }
    names
}

fn forward(iter: &mut TextIter, mut n: usize) {
    while n > 0 {
        let step = n.min(i32::MAX as usize);
        iter.forward_chars(step as i32);
        n -= step;
    }
}

/// The buffer's visible text. Character tags change formatting only, so a
/// paragraph stays one line for find and for spell.
pub fn logical_plain(buffer: &TextBuffer) -> String {
    let (start, end) = buffer.bounds();
    buffer.text(&start, &end, false).into()
}

/// How many character-tag applies a document needs after adjacent runs with
/// the same style have been merged. `c:size` is always one of them.
pub(crate) fn style_tag_count(style: &CharStyle) -> usize {
    1 + usize::from(style.font.is_some())
        + usize::from(style.bold)
        + usize::from(style.italic)
        + usize::from(style.underline)
        + usize::from(style.strike)
        + usize::from(style.script != Script::Normal)
        + usize::from(style.color.is_some())
        + usize::from(style.highlight.is_some())
        + usize::from(style.link.is_some())
}

/// On-screen tagging of 1,000 alternating runs, about 2,500 tag applies on
/// one line, took 159 ms and grew with the square of that count. A paragraph
/// under this many applies is styled before the window returns to the main
/// loop. Above it, applying the tags and laying the line out again froze the
/// UI thread: 10,000 runs kept a visible editor busy for 560 s, and one main
/// loop turn took 1.3 s. The editor then shows the words and does not tag.
pub(crate) const SYNC_TAG_APPLIES: usize = 800;

/// Word-char wrap of one unspaced line is laid out on the next main-loop
/// turn. A visible 800×600 editor, GTK 4.14.5, took 20.9 ms at 2,048
/// characters, 24.4 at 4,096, 73.5 at 8,192, and 101.8 at 10,000.
/// `set_text` itself stayed under 1 ms. The run count does not matter:
/// uniform runs merge to one range and still produce that line. A longer
/// breakless run is shown plain and read-only, with character wrap, so the
/// line stays inside the column. Ordinary prose breaks on spaces, so a long
/// manuscript of short words stays editable. A keystroke in an editable
/// document is not held: that layout is the writer's own line.
pub(crate) const SYNC_BREAKLESS_CHARS: usize = 8_192;

/// A no-break space glues the characters on either side. Unicode `White_Space`
/// includes it, and a line break does not.
fn is_no_break_space(ch: char) -> bool {
    matches!(ch, '\u{00A0}' | '\u{202F}' | '\u{2007}' | '\u{FEFF}')
}

/// CJK, kana, and hangul break between characters. A long run of them is
/// not one unbreakable line.
fn is_cjk_break(ch: char) -> bool {
    matches!(
        ch,
        '\u{1100}'..='\u{11FF}'
            | '\u{2E80}'..='\u{A4CF}'
            | '\u{A960}'..='\u{A97F}'
            | '\u{AC00}'..='\u{D7AF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{FF00}'..='\u{FFEF}'
    )
}

pub(crate) fn breakless_len(chars: impl Iterator<Item = char>) -> usize {
    let mut heavy = 0usize;
    let mut run = 0usize;
    for ch in chars {
        if is_cjk_break(ch) {
            heavy = heavy.max(run).max(1);
            run = 0;
            continue;
        }
        // U+200B is a break opportunity. Pango wraps there.
        if ch == '\u{200B}' || (ch.is_whitespace() && !is_no_break_space(ch)) {
            heavy = heavy.max(run);
            run = 0;
            continue;
        }
        run += 1;
    }
    heavy.max(run)
}

/// Characters in the longest stretch that word-char wrap cannot break.
/// A style change does not break the stretch. A no-break space does not
/// either. CJK does.
pub(crate) fn breakless_chars(text: &RichText) -> usize {
    let mut heavy = 0usize;
    for paragraph in &text.paragraphs {
        let chars = paragraph.runs.iter().flat_map(|piece| piece.text.chars());
        heavy = heavy.max(breakless_len(chars));
    }
    heavy
}

/// The plain read-only view. Either the heaviest paragraph needs too many
/// tag applies, or one unspaced run is long enough that word-char layout
/// blocks the UI thread.
pub(crate) fn needs_plain_view(text: &RichText) -> bool {
    heavy_applies(text) > SYNC_TAG_APPLIES || breakless_chars(text) > SYNC_BREAKLESS_CHARS
}

/// Tag applies in the paragraph that has the most, after adjacent runs with
/// the same style have been merged. Paragraphs do not add together.
pub(crate) fn heavy_applies(text: &RichText) -> usize {
    let mut heavy = 0usize;
    let n = text.paragraphs.len();
    for (i, p) in text.paragraphs.iter().enumerate() {
        let mut para_applies = 0usize;
        let mut span: Option<&CharStyle> = None;
        let mut span_chars = 0usize;
        let close =
            |span: &mut Option<&CharStyle>, span_chars: &mut usize, para_applies: &mut usize| {
                if let Some(style) = span.take()
                    && *span_chars > 0
                {
                    *para_applies += style_tag_count(style);
                }
                *span_chars = 0;
            };
        for run in &p.runs {
            if run.text.is_empty() {
                continue;
            }
            let same = span.is_some_and(|style| style == &run.style);
            if !same {
                close(&mut span, &mut span_chars, &mut para_applies);
                span = Some(&run.style);
            }
            span_chars += run.text.chars().count();
        }
        let fallback = CharStyle::default();
        if i + 1 < n {
            let newline_style = p.runs.last().map(|run| &run.style).unwrap_or(&fallback);
            let same = span.is_some_and(|style| style == newline_style);
            if !same {
                close(&mut span, &mut span_chars, &mut para_applies);
                span = Some(newline_style);
            }
            span_chars += 1;
        }
        close(&mut span, &mut span_chars, &mut para_applies);
        para_applies += para_tags(&p.style).len();
        heavy = heavy.max(para_applies);
    }
    heavy
}

/// Tags still to apply after [`Paint::begin`] has inserted the characters.
/// [`Paint::pump`] with an unlimited budget tags every range before the
/// caller returns to the main loop.
pub(crate) struct Paint {
    marks: Vec<(usize, usize, CharStyle)>,
    paras: Vec<(usize, usize, ParaStyle)>,
    index: usize,
    pos: usize,
    place: gtk::TextMark,
    links: HashMap<*const str, Rc<str>>,
    char_tags: HashMap<CharStyle, Vec<TextTag>>,
    para_i: usize,
    chars_done: bool,
    closed: bool,
}

impl Paint {
    /// Inserts `text` with one `set_text`. Adjacent runs that share a style
    /// become one range. The newline keeps the paragraph's last character
    /// formatting, so typing at the end of a line continues in that style.
    pub(crate) fn begin(buffer: &TextBuffer, text: &RichText) -> Self {
        buffer.begin_irreversible_action();
        let mut body = String::new();
        let mut marks = Vec::new();
        let mut paras = Vec::new();
        let mut heavy = 0usize;
        let mut chars = 0usize;
        let n = text.paragraphs.len();
        for (i, p) in text.paragraphs.iter().enumerate() {
            let para_start = chars;
            let mut para_applies = 0usize;
            let mut span_style: Option<CharStyle> = None;
            let mut span_start = chars;
            // Equal `CharStyle` values produce the same GTK tag set, so
            // adjacent equal runs are one range. Alternating bold does not
            // merge: each run keeps its own tags.
            let close_span = |span_style: &mut Option<CharStyle>,
                              span_start: usize,
                              chars: usize,
                              marks: &mut Vec<(usize, usize, CharStyle)>,
                              para_applies: &mut usize| {
                if let Some(style) = span_style.take()
                    && span_start < chars
                {
                    *para_applies += style_tag_count(&style);
                    marks.push((span_start, chars, style));
                }
            };
            for run in &p.runs {
                if run.text.is_empty() {
                    continue;
                }
                let same = span_style.as_ref().is_some_and(|style| style == &run.style);
                if !same {
                    close_span(
                        &mut span_style,
                        span_start,
                        chars,
                        &mut marks,
                        &mut para_applies,
                    );
                    span_style = Some(run.style.clone());
                    span_start = chars;
                }
                body.push_str(&run.text);
                chars += run.text.chars().count();
            }
            if i + 1 < n {
                let newline_style = p
                    .runs
                    .last()
                    .map(|run| run.style.clone())
                    .unwrap_or_default();
                let same = span_style
                    .as_ref()
                    .is_some_and(|style| style == &newline_style);
                if !same {
                    close_span(
                        &mut span_style,
                        span_start,
                        chars,
                        &mut marks,
                        &mut para_applies,
                    );
                    span_style = Some(newline_style);
                    span_start = chars;
                }
                body.push('\n');
                chars += 1;
            }
            close_span(
                &mut span_style,
                span_start,
                chars,
                &mut marks,
                &mut para_applies,
            );
            para_applies += para_tags(&p.style).len();
            heavy = heavy.max(para_applies);
            paras.push((para_start, chars, p.style.clone()));
        }
        note_insert(1);
        buffer.set_text(&body);
        let place = buffer.create_mark(None, &buffer.start_iter(), true);
        buffer.place_cursor(&buffer.start_iter());
        buffer.set_modified(false);
        debug_assert_eq!(heavy, heavy_applies(text));
        Self {
            marks,
            paras,
            index: 0,
            pos: 0,
            place,
            links: HashMap::new(),
            char_tags: HashMap::new(),
            para_i: 0,
            chars_done: false,
            closed: false,
        }
    }

    /// Applies ranges until `budget_ms` has passed. One range may run past
    /// the budget. Returns true when every range has been tagged.
    pub(crate) fn pump(&mut self, buffer: &TextBuffer, budget_ms: f64) -> bool {
        if self.closed {
            return true;
        }
        let started = std::time::Instant::now();
        let mut worked = false;
        loop {
            if self.chars_done && self.para_i >= self.paras.len() {
                return true;
            }
            if worked && started.elapsed().as_secs_f64() * 1000.0 >= budget_ms {
                return false;
            }
            if !self.chars_done {
                self.paint_range(buffer);
            } else {
                self.paint_paragraph(buffer);
            }
            worked = true;
        }
    }

    fn paint_range(&mut self, buffer: &TextBuffer) {
        if self.index >= self.marks.len() {
            self.chars_done = true;
            return;
        }
        let (start, end, style) = self.marks[self.index].clone();
        let mut cursor = buffer.iter_at_mark(&self.place);
        forward(&mut cursor, start.saturating_sub(self.pos));
        buffer.move_mark(&self.place, &cursor);
        let mut stop = cursor;
        forward(&mut stop, end.saturating_sub(start));
        let stop_mark = buffer.create_mark(None, &stop, false);
        let tags = cached_char_tags(&mut self.char_tags, buffer, &style, &mut self.links).to_vec();
        for text_tag in &tags {
            let head = buffer.iter_at_mark(&self.place);
            let tail = buffer.iter_at_mark(&stop_mark);
            buffer.apply_tag(text_tag, &head, &tail);
        }
        buffer.move_mark(&self.place, &buffer.iter_at_mark(&stop_mark));
        buffer.delete_mark(&stop_mark);
        self.pos = end;
        self.index += 1;
        if self.index >= self.marks.len() {
            self.chars_done = true;
            self.pos = 0;
            buffer.move_mark(&self.place, &buffer.start_iter());
        }
    }

    fn paint_paragraph(&mut self, buffer: &TextBuffer) {
        if self.para_i >= self.paras.len() {
            return;
        }
        let (start, end, style) = self.paras[self.para_i].clone();
        self.para_i += 1;
        let mut cursor = buffer.iter_at_mark(&self.place);
        forward(&mut cursor, start.saturating_sub(self.pos));
        buffer.move_mark(&self.place, &cursor);
        if start == end {
            self.pos = end;
            return;
        }
        let mut stop = buffer.iter_at_mark(&self.place);
        forward(&mut stop, end.saturating_sub(start));
        let head = buffer.create_mark(None, &buffer.iter_at_mark(&self.place), true);
        let tail = buffer.create_mark(None, &stop, false);
        for name in para_tags(&style) {
            buffer.apply_tag(
                &tag(buffer, &name),
                &buffer.iter_at_mark(&head),
                &buffer.iter_at_mark(&tail),
            );
        }
        buffer.move_mark(&self.place, &buffer.iter_at_mark(&tail));
        self.pos = end;
        buffer.delete_mark(&head);
        buffer.delete_mark(&tail);
    }

    pub(crate) fn finish(&mut self, buffer: &TextBuffer) {
        self.close(buffer);
        buffer.set_modified(false);
        buffer.place_cursor(&buffer.start_iter());
    }

    fn close(&mut self, buffer: &TextBuffer) {
        if self.closed {
            return;
        }
        self.closed = true;
        buffer.delete_mark(&self.place);
        buffer.end_irreversible_action();
    }
}

/// Replaces the buffer's contents with `text`. Not undoable.
#[cfg(test)]
pub(crate) fn load(buffer: &TextBuffer, text: &RichText) {
    let mut paint = Paint::begin(buffer, text);
    while !paint.pump(buffer, f64::MAX) {}
    paint.finish(buffer);
}

fn cached_char_tags<'a>(
    cache: &'a mut HashMap<CharStyle, Vec<TextTag>>,
    buffer: &TextBuffer,
    style: &CharStyle,
    links: &mut HashMap<*const str, Rc<str>>,
) -> &'a [TextTag] {
    if !cache.contains_key(style) {
        let built = style_names(style, links)
            .iter()
            .map(|name| tag(buffer, name))
            .collect();
        cache.insert(style.clone(), built);
    }
    cache.get(style).expect("the style was stored in the cache")
}

/// Reads the buffer back into [`RichText`].
pub fn save(buffer: &TextBuffer) -> RichText {
    let mut paragraphs: Vec<Paragraph> = Vec::new();
    let mut links = HashMap::new();
    // By line number: forward_line() won't step onto an empty last line.
    for i in 0..buffer.line_count() {
        let Some(line) = buffer.iter_at_line(i) else {
            break;
        };
        let mut end = line;
        if !end.ends_line() {
            end.forward_to_line_end();
        }
        let style = para_style(&tag_names(&line));
        let mut p = Paragraph {
            style,
            runs: Vec::new(),
        };
        let mut at = line;
        while at < end {
            let mut next = at;
            next.forward_to_tag_toggle(None::<&TextTag>);
            if next > end || next <= at {
                next = end;
            }
            let text: String = buffer
                .text(&at, &next, true)
                .chars()
                .filter(|&c| c != '\u{fffc}')
                .collect();
            let s = char_style(&tag_names(&at), &mut links);
            match p.runs.last_mut() {
                Some(last) if last.style == s => last.text.push_str(&text),
                _ => {
                    if !text.is_empty() {
                        p.runs.push(Run { text, style: s })
                    }
                }
            }
            at = next;
        }
        paragraphs.push(p);
    }
    RichText {
        paragraphs,
        lossy: Vec::new(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use omaquill::rtf;
    use std::path::Path;

    /// Paragraph lists compare equal if they only differ in the style of a
    /// trailing empty paragraph, which has no character to hold its tags.
    fn same(a: &[Paragraph], b: &[Paragraph]) -> bool {
        a.len() == b.len()
            && a.iter().zip(b).enumerate().all(|(i, (x, y))| {
                let last_empty = i + 1 == a.len() && x.runs.is_empty() && y.runs.is_empty();
                x.runs == y.runs && (last_empty || x.style == y.style)
            })
    }

    fn rtf_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                rtf_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rtf") {
                out.push(p);
            }
        }
    }

    #[test]
    fn heavy_applies_uses_the_worst_paragraph_and_merges_equal_runs() {
        let alternating = toggled(4);
        // Even runs are size, bold, and link. Odd runs are size and link.
        assert_eq!(heavy_applies(&alternating), 2 * 3 + 2 * 2);
        let mut two = alternating.clone();
        two.paragraphs.push(alternating.paragraphs[0].clone());
        assert_eq!(heavy_applies(&two), heavy_applies(&alternating));
        let mut paragraph = Paragraph::default();
        for _ in 0..10 {
            paragraph.runs.push(Run {
                text: "a".into(),
                style: CharStyle {
                    bold: true,
                    ..CharStyle::default()
                },
            });
        }
        let uniform = RichText {
            paragraphs: vec![paragraph],
            lossy: Vec::new(),
        };
        assert_eq!(heavy_applies(&uniform), 2);
        assert!(!needs_plain_view(&uniform));
        let long = RichText::from_plain(&"a".repeat(SYNC_BREAKLESS_CHARS + 1));
        assert!(heavy_applies(&long) <= SYNC_TAG_APPLIES);
        assert!(needs_plain_view(&long));
        let spaced = RichText::from_plain(&"aa ".repeat(10_000));
        assert!(!needs_plain_view(&spaced));
        assert!(breakless_chars(&spaced) < SYNC_BREAKLESS_CHARS);
    }

    #[test]
    fn breakless_chars_keeps_a_no_break_space_and_splits_cjk() {
        let glued = "a\u{00A0}".repeat(100);
        assert_eq!(breakless_chars(&RichText::from_plain(&glued)), 200);
        assert!(needs_plain_view(&RichText::from_plain(&format!(
            "a\u{00A0}{}",
            "b".repeat(SYNC_BREAKLESS_CHARS)
        ))));
        let cjk = "漢".repeat(10_000);
        assert_eq!(breakless_chars(&RichText::from_plain(&cjk)), 1);
        assert!(!needs_plain_view(&RichText::from_plain(&cjk)));
        let broken = "a\u{200B}".repeat(100);
        assert_eq!(breakless_chars(&RichText::from_plain(&broken)), 1);
        assert!(!needs_plain_view(&RichText::from_plain(&broken)));
    }

    #[test]
    fn style_tag_count_matches_the_tag_list() {
        let full = CharStyle {
            font: Some("Palatino".into()),
            size: 24,
            bold: true,
            italic: true,
            underline: true,
            strike: true,
            color: Some((1, 2, 3)),
            highlight: Some((4, 5, 6)),
            script: Script::Super,
            link: Some(Rc::from("https://example.test/chapter")),
        };
        assert_eq!(style_tag_count(&full), char_tags(&full).len());
        let plain = CharStyle::default();
        assert_eq!(char_tags(&plain), vec!["c:size=24".to_string()]);
        assert_eq!(style_tag_count(&plain), 1);
        let sub = CharStyle {
            script: Script::Sub,
            ..CharStyle::default()
        };
        assert_eq!(style_tag_count(&sub), char_tags(&sub).len());
    }

    #[test]
    fn save_reuses_one_link_for_every_run() {
        let mut links = HashMap::new();
        let names = vec![
            "c:b".to_string(),
            "c:link=https://example.test/chapter".to_string(),
        ];
        let first = char_style(&names, &mut links);
        let second = char_style(&names, &mut links);
        let left = first.link.clone().expect("first link");
        let right = second.link.clone().expect("second link");
        assert!(Rc::ptr_eq(&left, &right), "each run allocated its own link");
    }

    #[test]
    fn a_shared_link_builds_one_tag_name() {
        let link = "https://example.test/".repeat(80);
        let toggles = 400usize;
        let mut src = format!(
            "{{\\rtf1\\ansi{{\\fonttbl\\f0 Palatino-Roman;}}\\f0{{\\field{{\\*\\fldinst{{HYPERLINK \"{link}\"}}}}{{\\fldrslt "
        );
        for i in 0..toggles {
            if i % 2 == 0 {
                src.push_str("\\b a\\b0 ");
            } else {
                src.push('a');
            }
        }
        src.push_str("}}}");
        let text = rtf::parse(src.as_bytes());
        let mut links = HashMap::new();
        let mut held = Vec::new();
        let mut runs = 0usize;
        for paragraph in &text.paragraphs {
            for run in &paragraph.runs {
                runs += 1;
                held.extend(style_names(&run.style, &mut links));
            }
        }
        let mut seen = std::collections::HashSet::<*const str>::new();
        let mut bytes = 0usize;
        for name in &held {
            let Some(target) = name.strip_prefix("c:link=") else {
                continue;
            };
            if seen.insert(Rc::as_ptr(name)) {
                bytes += target.len();
            }
        }
        let one = text.paragraphs[0].runs[0]
            .style
            .link
            .as_ref()
            .expect("the run keeps the link")
            .len();
        assert!(runs >= toggles / 2, "runs {runs}");
        assert_eq!(
            bytes, one,
            "tag name bytes {bytes} across {runs} runs, one target is {one}"
        );
    }

    /// Every document survives the trip into a GTK buffer and back. Needs
    /// a display; skipped without one. Set OMAQUILL_REAL_PROJECT to also
    /// run a real project's documents through it.
    pub(crate) fn buffer_round_trip() {
        let mut files = Vec::new();
        rtf_files(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"),
            &mut files,
        );
        if let Some(real) = std::env::var_os("OMAQUILL_REAL_PROJECT") {
            rtf_files(Path::new(&real), &mut files);
        }
        assert!(!files.is_empty());
        let buffer = TextBuffer::new(None);
        for f in &files {
            let text = rtf::parse(&std::fs::read(f).unwrap());
            load(&buffer, &text);
            let back = save(&buffer);
            assert!(
                same(&back.paragraphs, &text.paragraphs),
                "{}\nwant {:#?}\ngot {:#?}",
                f.display(),
                text.paragraphs,
                back.paragraphs
            );
            // And the RTF written from the buffer reads back the same.
            let again = rtf::parse(rtf::write(&back).as_bytes());
            assert!(same(&again.paragraphs, &text.paragraphs), "{}", f.display());
        }
        eprintln!("{} documents through a GTK buffer", files.len());
    }

    fn toggled(runs: usize) -> RichText {
        let link: Rc<str> = Rc::from("https://example.test/chapter");
        let mut paragraph = Paragraph::default();
        for i in 0..runs {
            paragraph.runs.push(Run {
                text: "a".into(),
                style: CharStyle {
                    bold: i % 2 == 0,
                    link: Some(Rc::clone(&link)),
                    ..CharStyle::default()
                },
            });
        }
        RichText {
            paragraphs: vec![paragraph],
            lossy: Vec::new(),
        }
    }

    /// One buffer insert, then one tag range per style run, on the paragraph's
    /// own line. A newline between runs would hide a find match and split a
    /// spelling line.
    pub(crate) fn load_inserts_the_text_once() {
        let runs = 500usize;
        let text = toggled(runs);
        let buffer = TextBuffer::new(None);
        reset_inserts();
        let started = std::time::Instant::now();
        load(&buffer, &text);
        let small_ms = started.elapsed().as_secs_f64() * 1000.0;
        let wrote = inserts();
        assert_eq!(wrote, 1, "inserted {wrote} times for {runs} runs");
        assert_eq!(buffer.line_count(), 1);
        let back = save(&buffer);
        assert!(
            same(&back.paragraphs, &text.paragraphs),
            "round trip dropped a run"
        );
        eprintln!(
            "runs={runs} inserts={wrote} lines={} ms={small_ms:.1}",
            buffer.line_count()
        );
    }
}

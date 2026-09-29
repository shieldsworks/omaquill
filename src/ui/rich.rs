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
use std::cell::RefCell;
use std::collections::HashMap;

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

fn char_style(names: &[String]) -> CharStyle {
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
            "link" => s.link = Some(value.to_string()),
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

/// Replaces the buffer's contents with `text`. Not undoable.
pub fn load(buffer: &TextBuffer, text: &RichText) {
    buffer.begin_irreversible_action();
    buffer.set_text("");
    let n = text.paragraphs.len();
    for (i, p) in text.paragraphs.iter().enumerate() {
        let start = buffer.end_iter().offset();
        for run in &p.runs {
            let tags: Vec<TextTag> = char_tags(&run.style)
                .iter()
                .map(|n| tag(buffer, n))
                .collect();
            let tag_refs: Vec<&TextTag> = tags.iter().collect();
            let mut end = buffer.end_iter();
            buffer.insert_with_tags(&mut end, &run.text, &tag_refs);
        }
        if i + 1 < n {
            // The newline takes the paragraph's last character formatting,
            // so typing at the end of a line continues in that style.
            let tags: Vec<TextTag> = p
                .runs
                .last()
                .map(|r| char_tags(&r.style))
                .unwrap_or_else(|| char_tags(&CharStyle::default()))
                .iter()
                .map(|n| tag(buffer, n))
                .collect();
            let tag_refs: Vec<&TextTag> = tags.iter().collect();
            let mut end = buffer.end_iter();
            buffer.insert_with_tags(&mut end, "\n", &tag_refs);
        }
        let s = buffer.iter_at_offset(start);
        let e = buffer.end_iter();
        for name in para_tags(&p.style) {
            buffer.apply_tag(&tag(buffer, &name), &s, &e);
        }
    }
    buffer.end_irreversible_action();
    buffer.set_modified(false);
    buffer.place_cursor(&buffer.start_iter());
}

/// Reads the buffer back into [`RichText`].
pub fn save(buffer: &TextBuffer) -> RichText {
    let mut paragraphs = Vec::new();
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
            let s = char_style(&tag_names(&at));
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
}

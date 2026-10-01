//! Compile to PDF, laid out by Pango and written by Cairo (the text engine
//! and PDF writer GTK itself uses), so real fonts are embedded and text
//! is shaped, justified (and in the paperback, hyphenated) and
//! searchable.
//!
//! Two layouts:
//! - **Manuscript** (`Options::manuscript`): US Letter, 1" margins, Times
//!   12 pt double spaced, half-inch indents, a title page with the author
//!   and word count, and a "Surname / TITLE / page" header.
//! - **Book**: a 6×9" paperback page with mirrored margins, the book's own
//!   typeface at 11 pt, justified, chapters opening a third of the way down,
//!   running heads and page numbers.
//!
//! Lines are placed one at a time on a fixed leading, so a paragraph can
//! break across pages anywhere.

use crate::compile::{Options, Section, about_words, is_blank, surname};
use crate::project::{Error, Result};
use crate::rtf::{Align, Paragraph, Script};
use pango::prelude::*;
use std::collections::HashMap;

/// Page geometry in points (1/72").
struct Geometry {
    width: f64,
    height: f64,
    top: f64,
    bottom: f64,
    /// Margin on the binding side; equal to `outer` for a manuscript.
    inner: f64,
    outer: f64,
    /// Baseline-to-baseline distance for body text.
    leading: f64,
    size: f64,
    indent: f64,
}

impl Geometry {
    fn manuscript() -> Geometry {
        Geometry {
            width: 612.0,
            height: 792.0,
            top: 72.0,
            bottom: 72.0,
            inner: 72.0,
            outer: 72.0,
            leading: 24.0,
            size: 12.0,
            indent: 36.0,
        }
    }

    fn book() -> Geometry {
        Geometry {
            width: 432.0,
            height: 648.0,
            top: 54.0,
            bottom: 58.0,
            inner: 58.0,
            outer: 44.0,
            leading: 14.5,
            size: 11.0,
            indent: 16.0,
        }
    }

    fn text_width(&self) -> f64 {
        self.width - self.inner - self.outer
    }
}

struct Doc<'a> {
    cr: cairo::Context,
    surface: cairo::PdfSurface,
    ctx: pango::Context,
    g: Geometry,
    book: bool,
    family: String,
    opts: &'a Options,
    page: u32,
    y: f64,
    /// No running head or folio on this page (title, chapter openings).
    plain_page: bool,
}

fn err(e: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("PDF: {e}"))
}

/// Installed font families, as Pango sees them.
fn installed() -> Vec<String> {
    pangocairo::FontMap::default()
        .list_families()
        .iter()
        .map(|f| f.name().to_string())
        .collect()
}

/// An installed family to draw `wanted` with.
fn resolve(wanted: &str, installed: &[String], subs: &HashMap<String, String>) -> String {
    if installed.iter().any(|f| f.eq_ignore_ascii_case(wanted)) {
        return wanted.to_string();
    }
    subs.get(wanted)
        .cloned()
        .unwrap_or_else(|| "serif".to_string())
}

/// The family most of the manuscript's text is set in.
fn book_family(sections: &[Section]) -> Option<String> {
    let mut chars: HashMap<&str, usize> = HashMap::new();
    for s in sections {
        for scene in &s.scenes {
            for p in &scene.paragraphs {
                for r in &p.runs {
                    if let Some(f) = &r.style.font {
                        *chars.entry(f.as_str()).or_default() += r.text.len();
                    }
                }
            }
        }
    }
    chars
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(f, _)| f.to_string())
}

pub fn pdf(sections: &[Section], opts: &Options) -> Result<Vec<u8>> {
    let book = !opts.manuscript;
    let g = if book {
        Geometry::book()
    } else {
        Geometry::manuscript()
    };
    let fonts = installed();
    let subs = crate::fonts::find_substitutes(&fonts);
    let wanted = if book {
        book_family(sections).unwrap_or_else(|| "Palatino".to_string())
    } else {
        "Times New Roman".to_string()
    };
    let family = resolve(&wanted, &fonts, &subs);

    let surface =
        cairo::PdfSurface::for_stream(g.width, g.height, Vec::<u8>::new()).map_err(err)?;
    let _ = surface.set_metadata(cairo::PdfMetadata::Title, &opts.title);
    if !opts.author.is_empty() {
        let _ = surface.set_metadata(cairo::PdfMetadata::Author, &opts.author);
    }
    let _ = surface.set_metadata(cairo::PdfMetadata::Creator, "omaquill");
    let cr = cairo::Context::new(&surface).map_err(err)?;
    let ctx = pangocairo::functions::create_context(&cr);
    // One Pango unit of size is then one point, the PDF's own unit.
    pangocairo::functions::context_set_resolution(&ctx, 72.0);

    let mut doc = Doc {
        cr,
        surface,
        ctx,
        g,
        book,
        family,
        opts,
        page: 0,
        y: 0.0,
        plain_page: true,
    };
    doc.title_page(sections)?;
    for s in sections {
        doc.chapter(s)?;
    }
    doc.finish_page()?;
    let stream = doc.surface.finish_output_stream().map_err(err)?;
    stream
        .downcast::<Vec<u8>>()
        .map(|b| *b)
        .map_err(|_| err("unexpected output stream"))
}

impl Doc<'_> {
    fn font(&self, size: f64) -> pango::FontDescription {
        let mut f = pango::FontDescription::new();
        f.set_family(&self.family);
        f.set_size((size * pango::SCALE as f64) as i32);
        f
    }

    fn left(&self) -> f64 {
        // Page 1 is a right-hand page: its binding is on the left.
        if self.book && self.page.is_multiple_of(2) {
            self.g.outer
        } else {
            self.g.inner
        }
    }

    fn bottom_limit(&self) -> f64 {
        self.g.height - self.g.bottom
    }

    fn finish_page(&mut self) -> Result<()> {
        if self.page == 0 {
            return Ok(());
        }
        if !self.plain_page {
            self.furniture()?;
        }
        self.cr.show_page().map_err(err)
    }

    fn new_page(&mut self, plain: bool) -> Result<()> {
        self.finish_page()?;
        self.page += 1;
        self.plain_page = plain;
        self.y = self.g.top;
        Ok(())
    }

    /// Running head and page number.
    fn furniture(&self) -> Result<()> {
        let o = self.opts;
        if self.book {
            // Folio at the foot, centered; the head names the author on
            // left pages and the title on right ones.
            let folio = self.page.to_string();
            self.line_at(
                &folio,
                self.g.size * 0.85,
                Align::Center,
                self.g.height - self.g.bottom + 26.0,
            )?;
            let head = if self.page.is_multiple_of(2) && !o.author.is_empty() {
                o.author.to_uppercase()
            } else {
                o.title.to_uppercase()
            };
            let spaced: String = head.chars().flat_map(|c| [c, '\u{2009}']).collect();
            self.line_at(
                spaced.trim_end(),
                self.g.size * 0.75,
                Align::Center,
                self.g.top - 22.0,
            )?;
        } else {
            let head = format!(
                "{} / {} / {}",
                surname(&o.author),
                o.title.to_uppercase(),
                self.page
            );
            let head = head.trim_start_matches(" / ").to_string();
            self.line_at(&head, self.g.size, Align::Right, self.g.top / 2.0 + 8.0)?;
        }
        Ok(())
    }

    fn layout(&self, text: &str, size: f64, align: Align, width: f64) -> pango::Layout {
        let layout = pango::Layout::new(&self.ctx);
        layout.set_font_description(Some(&self.font(size)));
        layout.set_width((width * pango::SCALE as f64) as i32);
        layout.set_wrap(pango::WrapMode::WordChar);
        layout.set_alignment(match align {
            Align::Center => pango::Alignment::Center,
            Align::Right => pango::Alignment::Right,
            _ => pango::Alignment::Left,
        });
        layout.set_justify(align == Align::Justify);
        layout.set_text(text);
        layout
    }

    /// One line of text with its baseline at `baseline`, across the text
    /// block.
    fn line_at(&self, text: &str, size: f64, align: Align, baseline: f64) -> Result<()> {
        let layout = self.layout(text, size, align, self.g.text_width());
        let first = layout.baseline() as f64 / pango::SCALE as f64;
        self.cr.move_to(self.left(), baseline - first);
        pangocairo::functions::show_layout(&self.cr, &layout);
        Ok(())
    }

    /// Lays out a paragraph and places it line by line, breaking pages as
    /// needed. `indent`: the first line's indent in points.
    fn paragraph(
        &mut self,
        p: &Paragraph,
        size: f64,
        leading: f64,
        align: Align,
        indent: f64,
    ) -> Result<()> {
        // The paperback's justified text is hyphenated (soft hyphens,
        // shown only where a line actually breaks); a manuscript never is.
        let mut texts: Vec<String> = p
            .runs
            .iter()
            .map(|r| {
                let text = keep_dashes_attached(&r.text);
                if self.book && align == Align::Justify {
                    crate::hyphen::en_us().soft_hyphens(&text)
                } else {
                    text
                }
            })
            .collect();
        let layout = loop {
            let text: String = texts.concat();
            let layout = self.layout(&text, size, align, self.g.text_width());
            layout.set_indent((indent * pango::SCALE as f64) as i32);
            layout.set_attributes(Some(&attributes(p, &texts)));
            // At most two hyphenated lines in a row: the word that would
            // make a third loses its break points and moves down whole.
            match third_hyphen(&layout, &text) {
                Some(at) if unhyphenate_word(&mut texts, at) => continue,
                _ => break layout,
            }
        };
        let mut iter = layout.iter();
        loop {
            if self.y + leading > self.bottom_limit() + 0.01 {
                self.new_page(false)?;
            }
            let line = iter.line_readonly();
            let (_, logical) = iter.line_extents();
            let base_in_line = (iter.baseline() - logical.y()) as f64 / pango::SCALE as f64;
            let height = logical.height() as f64 / pango::SCALE as f64;
            // Center the line's own height in its slot of leading.
            let baseline = self.y + (leading - height) / 2.0 + base_in_line;
            if let Some(line) = line {
                self.cr.move_to(
                    self.left() + logical.x() as f64 / pango::SCALE as f64,
                    baseline,
                );
                pangocairo::functions::show_layout_line(&self.cr, &line);
            }
            self.y += leading;
            if !iter.next_line() {
                break;
            }
        }
        Ok(())
    }

    fn blank_lines(&mut self, n: f64) {
        self.y += self.g.leading * n;
    }

    fn title_page(&mut self, sections: &[Section]) -> Result<()> {
        self.new_page(true)?;
        let o = self.opts;
        let title = if o.title.is_empty() {
            "Untitled"
        } else {
            &o.title
        };
        if self.book {
            let top = self.g.height * 0.32;
            self.line_at(title, 26.0, Align::Center, top)?;
            if !o.author.is_empty() {
                self.line_at(&o.author, 14.0, Align::Center, top + 52.0)?;
            }
            return Ok(());
        }
        let size = self.g.size;
        let first = self.g.top + size;
        if !o.author.is_empty() {
            self.line_at(&o.author, size, Align::Left, first)?;
        }
        let words = crate::compile::word_count(sections);
        self.line_at(&about_words(words), size, Align::Right, first)?;
        let middle = self.g.height * 0.45;
        self.line_at(&title.to_uppercase(), size, Align::Center, middle)?;
        if !o.author.is_empty() {
            self.line_at(
                &format!("by {}", o.author),
                size,
                Align::Center,
                middle + self.g.leading,
            )?;
        }
        Ok(())
    }

    fn chapter(&mut self, s: &Section) -> Result<()> {
        // A book's chapter opening has no running head; a manuscript keeps
        // its header on every page but the title page.
        self.new_page(self.book)?;
        // Chapters open a third of the way down the text block.
        self.y = self.g.top + (self.bottom_limit() - self.g.top) / 3.0;
        if let Some(h) = &s.heading {
            let size = if self.book { 17.0 } else { self.g.size };
            let base = self.y + size;
            self.line_at(h, size, Align::Center, base)?;
            self.y = base;
            self.blank_lines(if self.book { 2.5 } else { 2.0 });
        }
        let mut first = true;
        for (i, scene) in s.scenes.iter().enumerate() {
            if i > 0 {
                self.separator()?;
                first = true;
            }
            for p in scene.paragraphs.iter().filter(|p| !is_blank(p)) {
                let centered = matches!(p.style.align, Align::Center | Align::Right);
                let align = if centered {
                    p.style.align
                } else if self.book {
                    Align::Justify
                } else {
                    Align::Left
                };
                // Books don't indent a chapter's or scene's first line.
                let indent = if centered || (self.book && first) {
                    0.0
                } else {
                    self.g.indent
                };
                self.paragraph(p, self.g.size, self.g.leading, align, indent)?;
                first = false;
            }
        }
        Ok(())
    }

    fn separator(&mut self) -> Result<()> {
        if self.y + self.g.leading * 3.0 > self.bottom_limit() {
            self.new_page(false)?;
        } else {
            self.blank_lines(if self.book { 0.5 } else { 0.0 });
        }
        let sep = if self.opts.scene_separator.trim().is_empty() {
            "#".to_string()
        } else {
            self.opts.scene_separator.clone()
        };
        let base = self.y + self.g.leading * 0.75;
        self.line_at(&sep, self.g.size, Align::Center, base)?;
        self.y += self.g.leading;
        self.blank_lines(if self.book { 0.5 } else { 0.0 });
        Ok(())
    }
}

/// Emphasis from the runs as Pango attributes. Fonts and sizes come from
/// the layout, not the draft: a compiled book has one typeface.
/// A word joiner (U+2060) before each em or en dash that follows a
/// letter: line breaking may otherwise start a line with the dash, which
/// books never do. It's invisible and doesn't change the text.
fn keep_dashes_attached(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev: Option<char> = None;
    for c in text.chars() {
        if matches!(c, '\u{2014}' | '\u{2013}') && prev.is_some_and(|p| !p.is_whitespace()) {
            out.push('\u{2060}');
        }
        out.push(c);
        prev = Some(c);
    }
    out
}

/// Most hyphenated lines allowed in a row.
const MAX_HYPHENS_IN_A_ROW: usize = 2;

/// The byte offset (in `text`) of the soft hyphen ending a line that
/// would be the third hyphenated line in a row, if any.
fn third_hyphen(layout: &pango::Layout, text: &str) -> Option<usize> {
    let mut run = 0;
    for line in layout.lines_readonly() {
        let end = (line.start_index() + line.length()) as usize;
        if text.get(..end).is_some_and(|t| t.ends_with('\u{ad}')) {
            run += 1;
            if run > MAX_HYPHENS_IN_A_ROW {
                return Some(end - '\u{ad}'.len_utf8());
            }
        } else {
            run = 0;
        }
    }
    None
}

/// Removes the soft hyphens from the word around byte `at` of the joined
/// `texts`. False if there was nothing to remove.
fn unhyphenate_word(texts: &mut [String], at: usize) -> bool {
    let mut start = 0;
    for t in texts.iter_mut() {
        if at < start + t.len() {
            let local = at - start;
            let in_word = |c: char| c.is_alphabetic() || c == '\u{ad}';
            let from = t[..local]
                .char_indices()
                .rev()
                .find(|&(_, c)| !in_word(c))
                .map_or(0, |(i, c)| i + c.len_utf8());
            let to = t[local..]
                .char_indices()
                .find(|&(_, c)| !in_word(c))
                .map_or(t.len(), |(i, _)| local + i);
            let word: String = t[from..to].chars().filter(|&c| c != '\u{ad}').collect();
            if word.len() == to - from {
                return false;
            }
            t.replace_range(from..to, &word);
            return true;
        }
        start += t.len();
    }
    false
}

/// `texts`: each run's text as laid out (hyphenated or not), for offsets.
fn attributes(p: &Paragraph, texts: &[String]) -> pango::AttrList {
    let list = pango::AttrList::new();
    let mut at = 0u32;
    for (r, text) in p.runs.iter().zip(texts) {
        let end = at + text.len() as u32;
        let add = |mut a: pango::Attribute| {
            a.set_start_index(at);
            a.set_end_index(end);
            list.insert(a);
        };
        let s = &r.style;
        if s.bold {
            add(pango::AttrInt::new_weight(pango::Weight::Bold).upcast());
        }
        if s.italic {
            add(pango::AttrInt::new_style(pango::Style::Italic).upcast());
        }
        if s.underline {
            add(pango::AttrInt::new_underline(pango::Underline::Single).upcast());
        }
        if s.strike {
            add(pango::AttrInt::new_strikethrough(true).upcast());
        }
        match s.script {
            Script::Super => {
                add(pango::AttrInt::new_rise(3 * pango::SCALE).upcast());
                add(pango::AttrFloat::new_scale(0.7).upcast());
            }
            Script::Sub => {
                add(pango::AttrInt::new_rise(-2 * pango::SCALE).upcast());
                add(pango::AttrFloat::new_scale(0.7).upcast());
            }
            Script::Normal => {}
        }
        at = end;
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashes_stay_with_the_word_before() {
        assert_eq!(
            keep_dashes_attached("ago\u{2014}never"),
            "ago\u{2060}\u{2014}never"
        );
        assert_eq!(keep_dashes_attached("a \u{2013} b"), "a \u{2013} b");
    }

    #[test]
    fn unhyphenates_one_word() {
        let mut texts = vec![
            "a con\u{ad}cen\u{ad}trated ".to_string(),
            "sto\u{ad}ry".to_string(),
        ];
        assert!(unhyphenate_word(&mut texts, "a con\u{ad}cen".len()));
        assert_eq!(texts[0], "a concentrated ");
        assert_eq!(texts[1], "sto\u{ad}ry");
        assert!(!unhyphenate_word(&mut texts, 3));
    }

    /// A narrow column of long words would hyphenate line after line;
    /// never more than two in a row.
    #[test]
    fn at_most_two_hyphenated_lines_in_a_row() {
        let ctx = pangocairo::FontMap::default().create_context();
        let words = "concentrated uncomfortable illuminated anticipation \
                     investigations transportation extraordinary \
                     metronome interrogation neighborhood ";
        let base = words.repeat(6);
        let mut texts = vec![crate::hyphen::en_us().soft_hyphens(&base)];
        let mut rounds = 0;
        let layout = loop {
            let text = texts.concat();
            let layout = pango::Layout::new(&ctx);
            layout.set_width(90 * pango::SCALE);
            layout.set_justify(true);
            layout.set_text(&text);
            match third_hyphen(&layout, &text) {
                Some(at) if unhyphenate_word(&mut texts, at) => rounds += 1,
                _ => break layout,
            }
        };
        assert!(rounds > 0, "the test column never needed the cap");
        let text = texts.concat();
        assert_eq!(third_hyphen(&layout, &text), None);
        assert_eq!(text.replace('\u{ad}', ""), base);
    }
}

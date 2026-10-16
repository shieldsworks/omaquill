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
    /// The same setup as `ctx`, with the base direction of the paragraph
    /// a window is cut from.
    windows: pango::Context,
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
    let windows = pangocairo::functions::create_context(&cr);
    pangocairo::functions::context_set_resolution(&windows, 72.0);

    let mut doc = Doc {
        cr,
        surface,
        ctx,
        windows,
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

    /// One line of text with its baseline at `baseline`, across the text
    /// block.
    fn line_at(&self, text: &str, size: f64, align: Align, baseline: f64) -> Result<()> {
        let mut texts = [text.to_string()];
        breakable(&mut texts);
        let layout = layout(
            &self.ctx,
            &self.font(size),
            self.g.text_width(),
            align,
            &texts[0],
        );
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
        breakable(&mut texts);
        let setting = Setting {
            ctx: &self.ctx,
            windows: &self.windows,
            font: self.font(size),
            width: self.g.text_width(),
            align,
            indent,
        };
        let layout = setting.paragraph(p, texts);
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

/// Characters of one stretch with no break opportunity before a zero-width
/// space is added. Pango's word-char wrap of such a stretch takes time that
/// grows with its square: 20,000 letters took 0.18 s to compile and 80,000
/// took 2.1 s, against 0.25 s with a zero-width space every 64. A line of
/// the book holds fewer characters than this, and no word is this long.
const BREAKLESS: usize = 64;

/// Adds a zero-width space after every [`BREAKLESS`] characters that have
/// no break between them, counted across runs. It is not drawn. Han, Kana,
/// Hangul, and Thai break between their own letters, so they end a stretch.
/// The space waits for a boundary between two clusters, since Pango breaks
/// a line after a zero-width space even before a mark.
fn breakable(texts: &mut [String]) {
    let mut run = 0;
    let mut prev = None;
    for t in texts.iter_mut() {
        let mut out = String::with_capacity(t.len());
        for c in t.chars() {
            if run >= BREAKLESS && prev.is_some_and(|p| !leads(p) && !follows(c)) {
                out.push('\u{200b}');
                run = 0;
            }
            out.push(c);
            run = if breaks(c) { 0 } else { run + 1 };
            prev = Some(c);
        }
        *t = out;
    }
}

fn breaks(c: char) -> bool {
    use pango::glib::{Unichar, UnicodeBreakType as B};
    (c.is_whitespace() && !matches!(c, '\u{a0}' | '\u{202f}' | '\u{2007}' | '\u{feff}'))
        || matches!(c, '\u{200b}' | '\u{ad}')
        || matches!(
            c.break_type(),
            B::Ideographic | B::HangulLvSyllable | B::HangulLvtSyllable | B::ComplexContext
        )
}

/// Whether `c` joins the cluster of the character before it.
fn follows(c: char) -> bool {
    use pango::glib::{Unichar, UnicodeBreakType as B};
    c.is_mark()
        || matches!(
            c.break_type(),
            B::CombiningMark
                | B::ZeroWidthJoiner
                | B::EmojiModifier
                | B::RegionalIndicator
                | B::HangulVJamo
                | B::HangulTJamo
        )
}

/// Whether `c` joins the cluster of the character after it. A virama
/// joins the next consonant into one conjunct.
fn leads(c: char) -> bool {
    use pango::glib::{Unichar, UnicodeBreakType as B};
    c.combining_class() == 9
        || matches!(
            c.break_type(),
            B::ZeroWidthJoiner | B::RegionalIndicator | B::HangulLJamo | B::HangulVJamo
        )
}

fn layout(
    ctx: &pango::Context,
    font: &pango::FontDescription,
    width: f64,
    align: Align,
    text: &str,
) -> pango::Layout {
    let layout = pango::Layout::new(ctx);
    layout.set_font_description(Some(font));
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

/// Most hyphenated lines allowed in a row.
const MAX_HYPHENS_IN_A_ROW: usize = 2;

/// Bytes of a paragraph laid out at once while the line after a third
/// hyphenated one is found again. Each removal of a word's soft hyphens
/// lays out this much, not the whole paragraph, so the work stays linear
/// in the paragraph's length.
const WINDOW: usize = 1024;

/// Bytes of the paragraph, at least, that a window lays out before the
/// line it sets. Pango shapes and itemizes a line with the text before it.
const CONTEXT: usize = 64;

/// Bytes a window reaches back at most: to the start of its Pango
/// paragraph, or for that text.
const REACH: usize = 4 * WINDOW;

/// Characters before a Pango paragraph that a window starting there lays
/// out too: as many as HarfBuzz reads before a run it shapes.
const BEFORE_PARAGRAPH: usize = 5;

/// How a paragraph is set: everything but its text.
struct Setting<'a> {
    ctx: &'a pango::Context,
    windows: &'a pango::Context,
    font: pango::FontDescription,
    width: f64,
    align: Align,
    /// The first line's indent in points.
    indent: f64,
}

/// Bytes the cap of a paragraph lays out before it starts no more windows,
/// over every window and every layout of the whole paragraph: this many
/// times the paragraph's length, plus [`SPARE`].
const BUDGET: usize = 16;

/// Bytes of a paragraph's budget besides [`BUDGET`] times its length, for
/// the windows near a Pango paragraph's start, which lay out from there.
const SPARE: usize = 64 * WINDOW;

/// How the cap of a paragraph ended.
#[derive(Debug, PartialEq)]
enum Outcome {
    /// No line ends a third hyphenated one in a row, after this many
    /// rounds of windows.
    Capped(usize),
    /// The budget ran out. From the last line checked on, the lines are
    /// drawn as the whole layout breaks them, with no more soft hyphens
    /// taken out.
    Stopped,
}

/// A window's layout and the lines it sets.
struct Window {
    slice: Slice,
    layout: pango::Layout,
    /// The index of the line at the byte the window sets lines from.
    first: usize,
    /// Where the window's text ends in the paragraph, and whether it ends
    /// at a paragraph delimiter before the paragraph's end.
    to: usize,
    ends: bool,
    /// Bytes laid out to find it.
    cost: usize,
}

impl Setting<'_> {
    fn lay_out(&self, ctx: &pango::Context, slice: &Slice) -> pango::Layout {
        let layout = layout(ctx, &self.font, self.width, self.align, &slice.text);
        layout.set_indent((self.indent * pango::SCALE as f64) as i32);
        layout.set_attributes(Some(&slice.attrs));
        layout
    }

    /// `p` laid out from `texts`, its runs' text. At most two hyphenated
    /// lines in a row: the word that would make a third loses its break
    /// points and moves down whole. A word that starts on an earlier line
    /// is longer than a line and cannot move down whole, so it keeps them.
    ///
    /// Pango breaks lines first-fit, so the lines above the one that loses
    /// a word stay put. After each removal only the text from that line on
    /// is laid out again, a [`WINDOW`] at a time, and the lines it sets are
    /// kept. One layout of the whole paragraph then draws it. Where its
    /// lines start as the kept ones do, each word lost its break points on
    /// the lines this layout has, as when the whole paragraph is laid out
    /// again after each removal. Else the words the windows took them out
    /// of on the first line that ends otherwise, or later, get them back,
    /// and windows set the lines from there again.
    ///
    /// The cap starts no window once what it has laid out comes within two
    /// lengths of the paragraph of [`BUDGET`] times its length, plus
    /// [`SPARE`], and then lays the whole paragraph out at most twice more.
    /// A window lays out a few times its own bytes, so the work stays linear
    /// in the length whatever the text. Where the budget runs out the cap
    /// ends: the lines checked keep their removals, and the rest of the
    /// paragraph is drawn as the whole layout breaks it.
    fn paragraph(&self, p: &Paragraph, texts: Vec<String>) -> pango::Layout {
        let len: usize = texts.iter().map(String::len).sum();
        self.capped(p, texts, BUDGET * len + SPARE).0
    }

    /// [`Setting::paragraph`] within `budget` bytes laid out, and how the
    /// cap ended.
    fn capped(
        &self,
        p: &Paragraph,
        texts: Vec<String>,
        mut budget: usize,
    ) -> (pango::Layout, Outcome) {
        let mut joined = Joined::new(texts);
        let len = joined.text.len();
        let mut rounds = 0;
        let mut kept: Option<(Vec<usize>, bool, usize)> = None;
        let mut stop = false;
        loop {
            let slice = joined.slice(p, 0..len, None);
            let layout = self.lay_out(self.ctx, &slice);
            budget = budget.saturating_sub(len);
            if stop {
                return (layout, Outcome::Stopped);
            }
            let lines = layout.lines_readonly();
            let at = |line: &pango::LayoutLine| slice.src[line.start_index() as usize];
            if let Some((kept, done, decided)) = kept.take() {
                let same = lines
                    .iter()
                    .zip(&kept)
                    .take_while(|&(line, &start)| at(line) == start)
                    .count();
                if same == kept.len() && (!done || same == lines.len()) {
                    let outcome = if done {
                        Outcome::Capped(rounds)
                    } else {
                        Outcome::Stopped
                    };
                    return (layout, outcome);
                }
                // Both start at byte 0, so the line before ends otherwise.
                joined.restore(decided, lines.get(same.saturating_sub(1)).map_or(0, at));
                stop = !done || budget < 2 * len;
                continue;
            }
            let Err(line) = cap_lines(&lines, &slice.text, 0, |start, end| {
                joined.unhyphenate_word(slice.src[start], slice.src[end])
            }) else {
                return (layout, Outcome::Capped(rounds));
            };
            rounds += 1;
            let decided = joined.taken.len();
            let mut starts = lines[..line].iter().map(at).collect();
            let done = self.cap_from(p, &mut joined, &mut starts, at(&lines[line]), &mut budget);
            kept = Some((starts, done, decided));
        }
    }

    /// Caps the hyphenated lines of `joined` from byte `from`, the start of
    /// a line that just lost a word, so two hyphenated lines are before it.
    /// `starts` holds the start of every line before it, and gets the start
    /// of every line set after. Each window's bytes come out of `budget`,
    /// which keeps two layouts of the whole paragraph: the check, and one
    /// more should the check put soft hyphens back. False when the budget
    /// ran out before the end.
    fn cap_from(
        &self,
        p: &Paragraph,
        joined: &mut Joined,
        starts: &mut Vec<usize>,
        mut from: usize,
        budget: &mut usize,
    ) -> bool {
        let len = joined.text.len();
        let directions = directions(&joined.text, self.ctx.base_dir());
        let mut run = MAX_HYPHENS_IN_A_ROW;
        let mut size = WINDOW;
        while from < len {
            if *budget < 2 * len {
                return false;
            }
            let paragraph = directions[directions.partition_point(|&(s, _)| s <= from) - 1];
            let window = self.window(p, joined, starts, paragraph, from, size);
            *budget = budget.saturating_sub(window.cost);
            let lines = window.layout.lines_readonly();
            // Text past `to` could still move the last lines' breaks, and
            // a cut paragraph leaves an empty line after its delimiter.
            let keep = if window.to == len {
                lines.len()
            } else if window.ends {
                lines.len() - 1
            } else {
                lines.len().saturating_sub(2)
            };
            if keep <= window.first {
                if window.to == len {
                    return true;
                }
                size *= 2;
                continue;
            }
            size = WINDOW;
            let src = &window.slice.src;
            let at = |line: &pango::LayoutLine| src[line.start_index() as usize];
            let kept = &lines[window.first..keep];
            let capped = cap_lines(kept, &window.slice.text, run, |start, end| {
                joined.unhyphenate_word(src[start], src[end])
            });
            let done = match capped {
                Ok(after) => {
                    run = after;
                    from = lines.get(keep).map_or(window.to, at);
                    kept
                }
                Err(line) => {
                    run = MAX_HYPHENS_IN_A_ROW;
                    from = at(&kept[line]);
                    &kept[..line]
                }
            };
            starts.extend(done.iter().map(at));
        }
        true
    }

    /// The window that sets the lines from byte `from`, a line's start, in
    /// the Pango paragraph that starts at byte `paragraph.0` with base
    /// direction `paragraph.1`: about `size` bytes of them. `starts` holds
    /// the start of every line before.
    ///
    /// Pango lays out a paragraph line by line from its start, and some of
    /// what it works out holds for the rest of it: the width of a tab, from
    /// the font at the layout's first byte; the width of a hyphen, from the
    /// font where it first weighs a break at a soft hyphen; the direction
    /// of the text, from the letters before. So within [`REACH`] bytes of
    /// the paragraph's start a window lays the paragraph out from there.
    /// Further on it lays out the text from a line at least [`CONTEXT`]
    /// bytes before `from`, after a line separator in the style of the
    /// text's first byte for the tabs, and serves only if it breaks every
    /// line before `from` where `starts` has them; else it reaches further
    /// back, up to [`REACH`] bytes, and then starts at `from`.
    /// [`Setting::paragraph`] checks what it sets.
    fn window(
        &self,
        p: &Paragraph,
        joined: &Joined,
        starts: &[usize],
        (paragraph, direction): (usize, pango::Direction),
        from: usize,
        size: usize,
    ) -> Window {
        self.windows.set_base_dir(direction);
        let (to, ends) = joined.window_end(from, size);
        let lead = p
            .runs
            .get(joined.ends.partition_point(|&e| e == 0))
            .map(|r| &r.style);
        let before = joined.text[..paragraph]
            .char_indices()
            .rev()
            .nth(BEFORE_PARAGRAPH - 1)
            .map_or(0, |(i, _)| i);
        let mut back = CONTEXT;
        let mut cost = 0;
        loop {
            let start = if from - paragraph <= REACH {
                paragraph
            } else {
                match starts[..starts.partition_point(|&s| s + back <= from)].last() {
                    Some(&s) if s > paragraph && back <= REACH && from - s <= REACH => s,
                    _ => from,
                }
            };
            let range = if start == paragraph {
                before..to
            } else {
                start..to
            };
            let lead = lead.filter(|_| range.start > 0);
            let lead_len = lead.map_or(0, |_| '\u{2028}'.len_utf8());
            let slice = joined.slice(p, range.clone(), lead);
            cost += slice.text.len();
            let layout = self.lay_out(self.windows, &slice);
            layout.set_auto_dir(false);
            let lines = layout.lines_readonly();
            let mut expect = starts[starts.partition_point(|&s| s < start)..].iter();
            let source = |line: &pango::LayoutLine| {
                let at = line.start_index() as usize;
                (at >= lead_len)
                    .then(|| slice.src[at])
                    .filter(|&s| s >= paragraph)
            };
            let first = lines.iter().enumerate().find_map(|(i, line)| {
                let s = source(line)?;
                if s >= from {
                    return Some((s == from && expect.next().is_none()).then_some(i));
                }
                (expect.next() != Some(&s)).then_some(None)
            });
            let first = match first {
                Some(Some(first)) => first,
                _ if start == from || start == paragraph => lines
                    .iter()
                    .position(|l| source(l).is_some_and(|s| s >= from))
                    .unwrap_or(lines.len()),
                _ => {
                    back *= 4;
                    continue;
                }
            };
            return Window {
                slice,
                layout,
                first,
                to,
                ends,
                cost,
            };
        }
    }
}

/// Walks `lines` counting hyphenated lines in a row, from `run` before
/// the first. At each one past the limit, `unhyphenate` gets the line's
/// first byte and its last, the soft hyphen, in `text`, and says whether
/// it took that word's soft hyphens out. Err: the line that lost a word.
/// Ok: the run after the last line.
fn cap_lines(
    lines: &[pango::LayoutLine],
    text: &str,
    mut run: usize,
    mut unhyphenate: impl FnMut(usize, usize) -> bool,
) -> std::result::Result<usize, usize> {
    for (i, line) in lines.iter().enumerate() {
        let start = line.start_index() as usize;
        let end = start + line.length() as usize;
        if !text.get(..end).is_some_and(|t| t.ends_with('\u{ad}')) {
            run = 0;
            continue;
        }
        run += 1;
        if run > MAX_HYPHENS_IN_A_ROW && unhyphenate(start, end - '\u{ad}'.len_utf8()) {
            return Err(i);
        }
    }
    Ok(run)
}

/// Pango starts a paragraph after each of these, and after "\r\n".
fn delimits(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2029}')
}

/// Where each of Pango's paragraphs in `text` starts, with the base
/// direction a layout of all of `text` gives it: its first strong
/// letter's, else the paragraph before's, else the whole text's first
/// strong letter's, else `base`, the context's.
fn directions(text: &str, base: pango::Direction) -> Vec<(usize, pango::Direction)> {
    let strong =
        |t: &str| Some(pango::find_base_dir(t)).filter(|&d| d != pango::Direction::Neutral);
    let mut before = strong(text).unwrap_or(base);
    let mut out = Vec::new();
    let mut start = 0;
    loop {
        let end = text[start..]
            .find(delimits)
            .map_or(text.len(), |i| start + i);
        before = strong(&text[start..end]).unwrap_or(before);
        out.push((start, before));
        let Some(c) = text[end..].chars().next() else {
            return out;
        };
        start = end + c.len_utf8();
        if c == '\r' && text[start..].starts_with('\n') {
            start += 1;
        }
    }
}

/// A paragraph's runs joined into one string, with the soft hyphens taken
/// out so far marked, not yet removed.
struct Joined {
    text: String,
    /// Run `i` ends at byte `ends[i]` and starts where run `i - 1` ends.
    ends: Vec<usize>,
    /// True at the first byte of each soft hyphen taken out.
    gone: Vec<bool>,
    /// Each soft hyphen taken out, by first byte, after the first byte of
    /// the line that lost it, in the order taken.
    taken: Vec<(usize, usize)>,
}

/// Part of a [`Joined`] as Pango lays it out, without the soft hyphens
/// taken out.
struct Slice {
    text: String,
    /// The `Joined` byte for each byte of `text`, then the slice's end.
    src: Vec<usize>,
    attrs: pango::AttrList,
}

impl Joined {
    fn new(texts: Vec<String>) -> Joined {
        let ends = texts
            .iter()
            .scan(0, |end, t| {
                *end += t.len();
                Some(*end)
            })
            .collect();
        let text = texts.concat();
        let gone = vec![false; text.len()];
        Joined {
            text,
            ends,
            gone,
            taken: Vec::new(),
        }
    }

    fn run_start(&self, i: usize) -> usize {
        if i == 0 { 0 } else { self.ends[i - 1] }
    }

    /// `range` of the text with the attributes of every run that touches
    /// it as the whole paragraph's layout gets them. With `lead`, a line
    /// separator in that style comes first.
    fn slice(
        &self,
        p: &Paragraph,
        range: std::ops::Range<usize>,
        lead: Option<&crate::rtf::CharStyle>,
    ) -> Slice {
        let len = range.len() + 3;
        let mut text = String::with_capacity(len);
        let mut src = Vec::with_capacity(len + 1);
        let attrs = pango::AttrList::new();
        if let Some(style) = lead {
            text.push('\u{2028}');
            src.resize(text.len(), range.start);
            style_attributes(&attrs, style, 0, text.len() as u32);
        }
        self.push(p, range.clone(), &mut text, &mut src, &attrs);
        src.push(range.end);
        Slice { text, src, attrs }
    }

    /// Appends `range` of the text, without the soft hyphens taken out, to
    /// `text`, with each byte's place in `src` and the runs' attributes.
    fn push(
        &self,
        p: &Paragraph,
        range: std::ops::Range<usize>,
        text: &mut String,
        src: &mut Vec<usize>,
        attrs: &pango::AttrList,
    ) {
        let first = self.ends.partition_point(|&e| e < range.start);
        for (i, r) in p.runs.iter().enumerate().skip(first) {
            let start = self.run_start(i);
            if start > range.end {
                break;
            }
            let (from, to) = (start.max(range.start), self.ends[i].min(range.end));
            let at = text.len();
            for (j, c) in self.text[from..to].char_indices() {
                if c == '\u{ad}' && self.gone[from + j] {
                    continue;
                }
                text.push(c);
                src.extend(from + j..from + j + c.len_utf8());
            }
            style_attributes(attrs, &r.style, at as u32, text.len() as u32);
        }
    }

    /// Where a window from `from` ends: about `size` bytes on, the end of
    /// the text, or just past the first paragraph delimiter. True when it
    /// ends at a delimiter before the end of the text.
    fn window_end(&self, from: usize, size: usize) -> (usize, bool) {
        let len = self.text.len();
        let mut to = (from + size).min(len);
        while !self.text.is_char_boundary(to) {
            to += 1;
        }
        let Some((i, c)) = self.text[from..to]
            .char_indices()
            .find(|&(_, c)| delimits(c))
        else {
            return (to, false);
        };
        let mut end = from + i + c.len_utf8();
        if c == '\r' && self.text[end..].starts_with('\n') {
            end += 1;
        }
        (end, end < len)
    }

    /// Takes out the soft hyphens of the word around byte `at`, within the
    /// run that holds it. A word that starts before byte `line`, on an
    /// earlier line, or that has more than [`BREAKLESS`] letters cannot
    /// move down whole and keeps them. False if it took nothing out.
    fn unhyphenate_word(&mut self, line: usize, at: usize) -> bool {
        let run = self.ends.partition_point(|&e| e <= at);
        let Some(&end) = self.ends.get(run) else {
            return false;
        };
        let start = self.run_start(run).max(line);
        let in_word = |c: char| c.is_alphabetic() || c == '\u{ad}';
        let from = match self.text[start..at]
            .char_indices()
            .rev()
            .find(|&(_, c)| !in_word(c))
        {
            Some((i, c)) => start + i + c.len_utf8(),
            None if start == line && self.text[..line].chars().next_back().is_some_and(in_word) => {
                return false;
            }
            None => start,
        };
        let mut letters = self.text[from..at]
            .chars()
            .filter(|&c| c != '\u{ad}')
            .count();
        let mut to = end;
        for (i, c) in self.text[at..end].char_indices() {
            if !in_word(c) {
                to = at + i;
                break;
            }
            if c != '\u{ad}' {
                letters += 1;
            }
            if letters > BREAKLESS {
                return false;
            }
        }
        let mut any = false;
        for (i, c) in self.text[from..to].char_indices() {
            if c == '\u{ad}' && !self.gone[from + i] {
                self.gone[from + i] = true;
                self.taken.push((line, from + i));
                any = true;
            }
        }
        any
    }

    /// Puts back the soft hyphens taken out after the first `decided`,
    /// where lines from byte `line` on lost a word.
    fn restore(&mut self, decided: usize, line: usize) {
        for (l, at) in self.taken.split_off(decided) {
            if l < line {
                self.taken.push((l, at));
            } else {
                self.gone[at] = false;
            }
        }
    }
}

/// Emphasis from a run as Pango attributes over `start..end`. Fonts and
/// sizes come from the layout, not the draft: a compiled book has one
/// typeface.
fn style_attributes(list: &pango::AttrList, s: &crate::rtf::CharStyle, start: u32, end: u32) {
    let add = |mut a: pango::Attribute| {
        a.set_start_index(start);
        a.set_end_index(end);
        list.insert(a);
    };
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtf::CharStyle;

    #[test]
    fn dashes_stay_with_the_word_before() {
        assert_eq!(
            keep_dashes_attached("ago\u{2014}never"),
            "ago\u{2060}\u{2014}never"
        );
        assert_eq!(keep_dashes_attached("a \u{2013} b"), "a \u{2013} b");
    }

    #[test]
    fn a_long_stretch_without_breaks_gets_one_every_breakless_chars() {
        let mut texts = vec!["a".repeat(100), format!("{} b", "a".repeat(40))];
        breakable(&mut texts);
        let zw = |n| format!("{}\u{200b}", "a".repeat(n));
        assert_eq!(texts[0], format!("{}{}", zw(BREAKLESS), "a".repeat(36)));
        assert_eq!(texts[1], format!("{}{} b", zw(28), "a".repeat(12)));
        let mut prose = vec!["a\u{a0}".repeat(40), "word ".repeat(40)];
        breakable(&mut prose);
        assert_eq!(prose[0].matches('\u{200b}').count(), 1);
        assert!(!prose[1].contains('\u{200b}'));
    }

    #[test]
    fn breakable_leaves_clusters_and_scripts_without_spaces_whole() {
        let thai = "ภาษาไทยเป็นภาษาที่มีระดับเสียงของคำแน่นอน".repeat(8);
        let han = "吾輩は猫である。名前はまだ無い。".repeat(8);
        let flags = "\u{1f1fa}\u{1f1f8}".repeat(100);
        let mut texts = vec![thai.clone(), han.clone(), flags.clone()];
        breakable(&mut texts);
        assert_eq!(texts, [thai, han, flags]);
        for unit in ["e\u{301}", "\u{915}\u{94d}\u{937}", "\u{633}\u{64e}"] {
            let mut texts = vec![unit.repeat(200)];
            breakable(&mut texts);
            let pieces: Vec<&str> = texts[0].split('\u{200b}').collect();
            assert!(pieces.len() > 1, "no break in {unit:?}");
            assert!(
                pieces.iter().all(|p| p.len() % unit.len() == 0),
                "a break inside {unit:?}"
            );
        }
    }

    #[test]
    fn unhyphenates_one_word() {
        let mut joined = Joined::new(vec![
            "a con\u{ad}cen\u{ad}trated ".to_string(),
            "sto\u{ad}ry".to_string(),
        ]);
        let run = || crate::rtf::Run {
            text: String::new(),
            style: Default::default(),
        };
        let p = Paragraph {
            runs: vec![run(), run()],
            ..Default::default()
        };
        let text = |j: &Joined| j.slice(&p, 0..j.text.len(), None).text;
        assert!(joined.unhyphenate_word(2, "a con\u{ad}cen".len()));
        assert_eq!(text(&joined), "a concentrated sto\u{ad}ry");
        assert!(!joined.unhyphenate_word(2, "a con\u{ad}cen".len()));
        assert!(!joined.unhyphenate_word(0, 3));
        let mut started = Joined::new(vec!["con\u{ad}cen\u{ad}trated".to_string()]);
        assert!(!started.unhyphenate_word("con\u{ad}".len(), "con\u{ad}cen".len()));
        let mut split = Joined::new(vec!["con\u{ad}".to_string(), "cen\u{ad}trated".to_string()]);
        assert!(split.unhyphenate_word(0, "con\u{ad}cen".len()));
        assert_eq!(text(&split), "con\u{ad}centrated");
        let huge = crate::hyphen::en_us().soft_hyphens(&"concentrated".repeat(6));
        let mut huge = Joined::new(vec![huge]);
        assert!(!huge.unhyphenate_word(0, "con".len()));
    }

    /// A context as `pdf` makes one, on a PDF surface.
    fn context() -> pango::Context {
        let surface = cairo::PdfSurface::for_stream(432.0, 648.0, Vec::<u8>::new()).unwrap();
        let cr = cairo::Context::new(&surface).unwrap();
        let ctx = pangocairo::functions::create_context(&cr);
        pangocairo::functions::context_set_resolution(&ctx, 72.0);
        ctx
    }

    fn setting<'a>(
        ctx: &'a pango::Context,
        windows: &'a pango::Context,
        width: f64,
        indent: f64,
    ) -> Setting<'a> {
        Setting {
            ctx,
            windows,
            font: pango::FontDescription::from_string("serif 11"),
            width,
            align: Align::Justify,
            indent,
        }
    }

    /// Runs as the paperback prepares them, bold on every other one.
    fn prepared(runs: &[&str]) -> (Paragraph, Vec<String>) {
        styled(&bold_every_other(runs))
    }

    fn bold_every_other<'a>(runs: &[&'a str]) -> Vec<(&'a str, CharStyle)> {
        let bold = |i: usize| CharStyle {
            bold: i % 2 == 1,
            ..Default::default()
        };
        runs.iter()
            .enumerate()
            .map(|(i, &t)| (t, bold(i)))
            .collect()
    }

    /// Runs in the given styles as the paperback prepares them.
    fn styled(runs: &[(&str, CharStyle)]) -> (Paragraph, Vec<String>) {
        let p = Paragraph {
            runs: runs
                .iter()
                .map(|(t, style)| crate::rtf::Run {
                    text: t.to_string(),
                    style: style.clone(),
                })
                .collect(),
            ..Default::default()
        };
        let mut texts: Vec<String> = runs
            .iter()
            .map(|(t, _)| crate::hyphen::en_us().soft_hyphens(&keep_dashes_attached(t)))
            .collect();
        breakable(&mut texts);
        (p, texts)
    }

    /// Each line's text and logical extents.
    fn lines(layout: &pango::Layout) -> Vec<(String, i32, i32, i32)> {
        let text = layout.text();
        let mut out = Vec::new();
        let mut iter = layout.iter();
        loop {
            let line = iter.line_readonly().expect("an iterator stands on a line");
            let (_, logical) = iter.line_extents();
            let start = line.start_index() as usize;
            let end = start + line.length() as usize;
            out.push((
                text[start..end].to_string(),
                logical.x(),
                logical.width(),
                iter.baseline(),
            ));
            if !iter.next_line() {
                return out;
            }
        }
    }

    /// Whether a third hyphenated line in a row ends in a word that
    /// starts on that line.
    fn third_in_a_row(layout: &pango::Layout) -> bool {
        let text = layout.text();
        let mut joined = Joined::new(vec![text.to_string()]);
        cap_lines(&layout.lines_readonly(), &text, 0, |start, at| {
            joined.unhyphenate_word(start, at)
        })
        .is_err()
    }

    /// A narrow column of long words would hyphenate line after line;
    /// never more than two in a row.
    #[test]
    fn at_most_two_hyphenated_lines_in_a_row() {
        let (ctx, windows) = (context(), context());
        let words = "concentrated uncomfortable illuminated anticipation \
                     investigations transportation extraordinary \
                     metronome interrogation neighborhood ";
        let base = words.repeat(6);
        let (p, texts) = prepared(&[&base]);
        let hyphens = texts[0].matches('\u{ad}').count();
        let layout = setting(&ctx, &windows, 90.0, 0.0).paragraph(&p, texts);
        let text = layout.text();
        assert!(
            text.matches('\u{ad}').count() < hyphens,
            "the test column never needed the cap"
        );
        assert!(!third_in_a_row(&layout));
        assert_eq!(text.replace('\u{ad}', ""), base);
    }

    /// A word longer than a line cannot move down whole, so it keeps its
    /// break points rather than run on unbroken.
    #[test]
    fn a_word_longer_than_a_line_keeps_its_hyphens() {
        let (ctx, windows) = (context(), context());
        let word = "concentrated".repeat(200);
        let (p, texts) = prepared(&[&format!("{} {word}", "incomprehensibilities ".repeat(4))]);
        let hyphens = texts[0].matches('\u{ad}').count();
        let layout = setting(&ctx, &windows, 330.0, 0.0).paragraph(&p, texts);
        assert_eq!(layout.text().matches('\u{ad}').count(), hyphens);
        assert!(layout.line_count() > 20);
    }

    /// The cap as one loop over the whole paragraph: lay out, take the
    /// soft hyphens out of the word ending the first third hyphenated
    /// line, and lay out again.
    /// The bytes of `text` whose soft hyphens `laid`, the same text with
    /// some of them taken out, lacks.
    fn removed(text: &str, laid: &str) -> Vec<usize> {
        let mut laid = laid.chars().peekable();
        text.char_indices()
            .filter(|&(_, c)| {
                let kept = laid.peek() == Some(&c);
                if kept {
                    laid.next();
                }
                !kept
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn capped_whole(s: &Setting, p: &Paragraph, texts: Vec<String>) -> (pango::Layout, usize) {
        let mut joined = Joined::new(texts);
        let mut rounds = 0;
        loop {
            let slice = joined.slice(p, 0..joined.text.len(), None);
            let layout = s.lay_out(s.ctx, &slice);
            let capped = cap_lines(&layout.lines_readonly(), &slice.text, 0, |start, at| {
                joined.unhyphenate_word(slice.src[start], slice.src[at])
            });
            if capped.is_ok() {
                return (layout, rounds);
            }
            rounds += 1;
        }
    }

    const LONG: &str = "incomprehensibilities uncharacteristically \
                        internationalization counterrevolutionaries \
                        electroencephalography ";

    /// The long words with a tab after every seventh, 400 of them.
    fn tabbed() -> String {
        let words: Vec<&str> = LONG.split_whitespace().collect();
        let mut body = String::new();
        for i in 0..400 {
            body.push_str(words[i % words.len()]);
            body.push(if i % 7 == 6 { '\t' } else { ' ' });
        }
        body
    }

    /// Lines that start with a combining mark, after a space and after a
    /// paragraph delimiter.
    fn marked() -> String {
        let words = LONG.split_whitespace().collect::<Vec<_>>().join(" \u{301}");
        let paragraph = format!("{words} \u{301}").repeat(40);
        format!("{paragraph}\r\n\u{301}{paragraph}\n\u{301}{paragraph}")
    }

    /// Text in a font and setting, and the cases each test lays out.
    struct Case<'a> {
        name: &'a str,
        runs: Vec<(&'a str, CharStyle)>,
        font: &'a str,
        settings: &'a [(f64, f64)],
    }

    /// The reviewer's cases: tabs after a lead-in in another style, and
    /// lines that start with a combining mark.
    fn context_cases<'a>(tabbed: &'a str, marked: &'a str) -> Vec<Case<'a>> {
        let lead = |style: CharStyle| vec![("Lo ", style), (tabbed, CharStyle::default())];
        let bold = CharStyle {
            bold: true,
            ..Default::default()
        };
        let sup = CharStyle {
            script: Script::Super,
            ..Default::default()
        };
        vec![
            Case {
                name: "tabs after bold",
                runs: lead(bold.clone()),
                font: "DejaVu Serif 11",
                settings: &[(330.0, 16.0), (468.0, 16.0)],
            },
            Case {
                name: "tabs after bold, EB Garamond",
                runs: lead(bold),
                font: "EB Garamond 11",
                settings: &[(330.0, 16.0)],
            },
            Case {
                name: "tabs after superscript",
                runs: lead(sup),
                font: "P052 11",
                settings: &[(330.0, 16.0)],
            },
            Case {
                name: "marks at line starts",
                runs: bold_every_other(&[marked]),
                font: "P052 11",
                settings: &[(330.0, 16.0), (150.0, 0.0)],
            },
        ]
    }

    /// Laying out a window at a time after each removal breaks every line
    /// where the whole-paragraph loop does, with the same extents, and the
    /// check of the windows' lines finds nothing to set again.
    #[test]
    fn windows_set_lines_as_the_whole_paragraph_does() {
        let (ctx, windows) = (context(), context());
        let long = LONG;
        let english = long.repeat(50);
        let mix = |other: &str| format!("{}{other} ", long.repeat(3)).repeat(15);
        let thai = mix("ภาษาไทยเป็นภาษาที่มีระดับเสียงของคำแน่นอน");
        let japanese = mix("吾輩は猫である。名前はまだ無い。");
        let hebrew = mix("שלום עולם, \u{5de}\u{5d4} \u{5e9}\u{5dc}\u{5d5}\u{5de}\u{5da}?");
        let arabic = mix("\u{627}\u{644}\u{633}\u{644}\u{627}\u{645}");
        let marks = mix("e\u{301}te\u{301} ago\u{2014}never \u{915}\u{94d}\u{937}");
        let paragraphs = format!(
            "{}\n{}\u{2029}{}\r\n{}\r{}\u{2028}{}",
            long.repeat(8),
            long.repeat(10),
            long.repeat(6),
            long.repeat(9),
            long.repeat(7),
            long.repeat(6)
        );
        let rtl = format!("שלום {}", format!("{long}שלום 😀. {long}").repeat(15));
        let split: Vec<&str> = english.split_inclusive("ties").collect();
        let widths = &[(330.0, 16.0), (150.0, 0.0), (90.0, 0.0)];
        let mut cases: Vec<Case> = [
            ("english", vec![english.as_str()]),
            ("runs", split),
            ("thai", vec![&thai]),
            ("japanese", vec![&japanese]),
            ("hebrew", vec![&hebrew]),
            ("arabic", vec![&arabic]),
            ("marks", vec![&marks]),
            ("paragraphs", vec![&paragraphs]),
            ("rtl", vec![&rtl]),
        ]
        .into_iter()
        .map(|(name, runs)| Case {
            name,
            runs: bold_every_other(&runs),
            font: "serif 11",
            settings: widths,
        })
        .collect();
        let (tabbed, marked) = (tabbed(), marked());
        cases.extend(context_cases(&tabbed, &marked));
        for case in &cases {
            let name = case.name;
            for &(width, indent) in case.settings {
                let mut s = setting(&ctx, &windows, width, indent);
                s.font = pango::FontDescription::from_string(case.font);
                let (p, texts) = styled(&case.runs);
                assert!(
                    texts.concat().len() > 3 * WINDOW,
                    "{name}: shorter than three windows"
                );
                let (whole, rounds) = capped_whole(&s, &p, texts.clone());
                assert!(rounds > 3, "{name} at {width}: {rounds} rounds");
                let budget = BUDGET * texts.concat().len() + SPARE;
                let (windowed, outcome) = s.capped(&p, texts, budget);
                assert_eq!(outcome, Outcome::Capped(1), "{name} at {width}");
                assert_eq!(windowed.text(), whole.text(), "{name} at {width}");
                assert_eq!(lines(&windowed), lines(&whole), "{name} at {width}");
            }
        }
    }

    /// Soft hyphens the windows took out on lines a layout of the whole
    /// paragraph breaks otherwise are put back, and when the budget runs
    /// out the paragraph keeps all its text, keeps only removals the whole
    /// paragraph laid out again after each one also makes, and is drawn as
    /// the whole layout breaks it. Windows laid out at another resolution
    /// break the lines otherwise.
    #[test]
    fn a_paragraph_the_windows_set_wrong_keeps_its_text() {
        let (ctx, windows) = (context(), context());
        pangocairo::functions::context_set_resolution(&windows, 80.0);
        let s = setting(&ctx, &windows, 330.0, 16.0);
        let (p, texts) = prepared(&[&LONG.repeat(50)]);
        let text = texts.concat();
        let (drawn, outcome) = s.capped(&p, texts.clone(), BUDGET * text.len() + SPARE);
        assert_eq!(outcome, Outcome::Stopped);
        assert_eq!(
            drawn.text().replace('\u{ad}', ""),
            text.replace('\u{ad}', "")
        );
        let (whole, _) = capped_whole(&s, &p, texts.clone());
        let kept = removed(&text, &drawn.text());
        let all = removed(&text, &whole.text());
        assert!(!kept.is_empty() && kept.iter().all(|i| all.contains(i)));
        let fresh = layout(&ctx, &s.font, s.width, s.align, &drawn.text());
        fresh.set_indent((s.indent * pango::SCALE as f64) as i32);
        assert_eq!(lines(&drawn), lines(&fresh));
    }

    /// A paragraph whose budget runs out keeps all its text, keeps the
    /// removals made before, and stops taking soft hyphens out.
    #[test]
    fn a_paragraph_out_of_budget_keeps_its_text() {
        let (ctx, windows) = (context(), context());
        let s = setting(&ctx, &windows, 330.0, 16.0);
        let (p, texts) = prepared(&[&LONG.repeat(50)]);
        let text = texts.concat();
        let (whole, _) = capped_whole(&s, &p, texts.clone());
        let all = removed(&text, &whole.text());
        let mut counts = Vec::new();
        for budget in [0, 4 * text.len(), 8 * text.len()] {
            let (layout, outcome) = s.capped(&p, texts.clone(), budget);
            assert_eq!(outcome, Outcome::Stopped, "budget {budget}");
            let laid = layout.text();
            assert_eq!(laid.replace('\u{ad}', ""), text.replace('\u{ad}', ""));
            let kept = removed(&text, &laid);
            assert!(kept.iter().all(|i| all.contains(i)), "budget {budget}");
            counts.push(kept.len());
        }
        assert!(
            0 < counts[0] && counts[0] < counts[1] && counts[1] < counts[2],
            "{counts:?}"
        );
        let (capped, outcome) = s.capped(&p, texts, BUDGET * text.len() + SPARE);
        assert!(matches!(outcome, Outcome::Capped(_)));
        assert_eq!(lines(&capped), lines(&whole));
    }

    /// Putting soft hyphens back leaves out the ones taken out first.
    #[test]
    fn restoring_keeps_the_soft_hyphens_taken_out_first() {
        let text = "il\u{ad}lu\u{ad}mi\u{ad}nat\u{ad}ed con\u{ad}cen\u{ad}trat\u{ad}ed";
        let mut joined = Joined::new(vec![text.into()]);
        assert!(joined.unhyphenate_word(0, 2));
        let first = joined.taken.len();
        assert!(joined.unhyphenate_word(0, text.find("con").unwrap() + 3));
        joined.restore(first, 0);
        let left: String = text
            .char_indices()
            .filter(|&(i, _)| !joined.gone[i])
            .map(|(_, c)| c)
            .collect();
        assert_eq!(left, "illuminated con\u{ad}cen\u{ad}trat\u{ad}ed");
    }

    /// A window set for any line of a paragraph, from a line before it
    /// rather than the paragraph's start, breaks the lines it keeps where a
    /// layout of the whole paragraph does: the text before the window
    /// changes neither tab stops nor shaping.
    #[test]
    fn a_window_from_any_line_breaks_as_the_whole_paragraph_does() {
        let (ctx, windows) = (context(), context());
        let (tabbed, marked) = (tabbed(), marked());
        let mut cases = context_cases(&tabbed, &marked);
        let devanagari = "a \u{3bd}\u{3b9}\u{3ba}\u{3ac}\u{3b5}\u{3bb}\u{3bb}\u{3b7} \
            \u{3bb}\u{3b7}\u{3bd}\u{3b9}\u{3ba}\u{3ac}\u{3b5}\u{3bb}\u{3bb}\u{3b7}\u{3bd}\u{3b9} \
            \u{e1b}\u{e19}\u{e20}\u{e29}\u{e32}\u{e20}\u{e32}\u{e29}\u{e32}\u{e44}\u{924}\
            \u{435}\u{442}\u{41f}\u{440}\u{438}\u{432} ete \u{432}\u{435}\u{442}\u{41f}\u{440}\
            \u{438}\u{432}\u{435}\u{442} \u{915}\u{937}\u{924}\u{930}\u{92f}\u{915}\u{94d}\
            \u{627}\u{644}\u{645}\u{e9}na\u{ef}v \u{442}\u{41f}\u{440}\u{438}\u{432}\u{435}\
            \u{442}\u{41f}\u{440}\u{438} \u{ef}vet\u{e9}na\u{ef} \u{3ac}\u{3b5}\u{3bb}\u{3bb}\
            \u{3b7}\u{3bd}\u{3b9}\u{3b7} \u{5da}\u{5dc}\u{5d5}\u{5dd}\u{5e2} \u{5dc}\u{5d5}\
            \u{5de}\u{5da}\u{5e9}\u{5dc}\u{5d5}\u{5dd}\u{5e2}\u{5d5}\u{5dc}\u{5dd}\u{5de}\u{5d4}\
            \u{5e9}\u{5dc}\u{5d5} \u{e32}\u{e20}\u{e32}\u{e29} \u{924}\u{94d}\u{94d}\u{937}\
            \u{924}\u{915}\u{915}";
        let virama = [devanagari; 16].join(" ");
        cases.push(Case {
            name: "virama",
            runs: vec![(&virama, CharStyle::default())],
            font: "monospace 13",
            settings: &[(354.0, 36.0)],
        });
        for case in &cases {
            let name = case.name;
            for &(width, indent) in case.settings {
                let mut s = setting(&ctx, &windows, width, indent);
                s.font = pango::FontDescription::from_string(case.font);
                let (p, texts) = styled(&case.runs);
                let joined = Joined::new(texts);
                let slice = joined.slice(&p, 0..joined.text.len(), None);
                let whole = s.lay_out(s.ctx, &slice);
                let lines = whole.lines_readonly();
                let starts: Vec<usize> = lines.iter().map(|l| l.start_index() as usize).collect();
                assert!(starts.len() > 3, "{name}: {} lines", starts.len());
                let directions = directions(&joined.text, ctx.base_dir());
                let mut tried = 0;
                for (k, &from) in starts.iter().enumerate().skip(1) {
                    let paragraph = directions[directions.partition_point(|&(s, _)| s <= from) - 1];
                    if from - paragraph.0 <= REACH {
                        continue;
                    }
                    tried += 1;
                    let window = s.window(&p, &joined, &starts[..k], paragraph, from, WINDOW);
                    let kept: Vec<usize> = window.layout.lines_readonly()[window.first..]
                        .iter()
                        .map(|l| window.slice.src[l.start_index() as usize])
                        .collect();
                    let keep = if window.to == joined.text.len() {
                        kept.len()
                    } else {
                        kept.len().saturating_sub(2)
                    };
                    assert_eq!(
                        kept[..keep],
                        starts[k..(k + keep).min(starts.len())],
                        "{name} at {width}, window for line {k}"
                    );
                }
                assert!(tried > 0, "{name}: no line past the reach");
            }
        }
    }
}

//! The rich-text editor: a `GtkTextView` over a buffer of [`super::rich`]
//! tags, with typing that carries formatting forward the way a word
//! processor does (GTK leaves new text unformatted by default).

use super::rich;
use gtk::glib;
use gtk::prelude::*;
use omaquill::rtf::{Align, RichText};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Default, Clone, Copy, PartialEq)]
pub struct Formatting {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub align: Align,
}

type FormattingCallback = Box<dyn Fn(Formatting)>;

pub struct Editor {
    pub root: gtk::Box,
    pub view: gtk::TextView,
    pub buffer: gtk::TextBuffer,
    pub clamp: adw::Clamp,
    pub search_bar: gtk::SearchBar,
    search_entry: gtk::SearchEntry,
    /// Formatting switched on or off with nothing selected, for the next
    /// characters typed: (tag name, on).
    pending: RefCell<Vec<(String, bool)>>,
    /// Tags for text typed where there's nothing to take formatting from
    /// (a new, empty document).
    default_tags: RefCell<Vec<String>>,
    loading: Cell<bool>,
    inserting: Cell<bool>,
    changed: RefCell<Vec<Box<dyn Fn()>>>,
    formatting: RefCell<Vec<FormattingCallback>>,
}

impl Editor {
    /// `page` is the main editor: centered column, find bar. Otherwise a
    /// compact editor for notes.
    pub fn new(page: bool) -> Rc<Editor> {
        let buffer = gtk::TextBuffer::new(None);
        let view = gtk::TextView::builder()
            .buffer(&buffer)
            .wrap_mode(gtk::WrapMode::WordChar)
            .accepts_tab(true)
            .build();
        view.add_css_class("omaquill-text");
        let clamp = adw::Clamp::builder()
            .maximum_size(760)
            .tightening_threshold(600)
            .build();
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Find in document")
            .build();
        let search_bar = gtk::SearchBar::builder()
            .child(&search_entry)
            .show_close_button(true)
            .build();
        search_bar.connect_entry(&search_entry);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        if page {
            view.add_css_class("omaquill-page");
            view.set_top_margin(48);
            view.set_bottom_margin(240);
            view.set_left_margin(24);
            view.set_right_margin(24);
            clamp.set_child(Some(&view));
            scrolled.set_child(Some(&clamp));
            root.append(&search_bar);
        } else {
            view.add_css_class("omaquill-notes");
            view.set_top_margin(8);
            view.set_bottom_margin(8);
            view.set_left_margin(8);
            view.set_right_margin(8);
            scrolled.set_child(Some(&view));
        }
        root.append(&scrolled);

        let ed = Rc::new(Editor {
            root,
            view,
            buffer,
            clamp,
            search_bar,
            search_entry,
            pending: RefCell::new(Vec::new()),
            default_tags: RefCell::new(Vec::new()),
            loading: Cell::new(false),
            inserting: Cell::new(false),
            changed: RefCell::new(Vec::new()),
            formatting: RefCell::new(Vec::new()),
        });
        ed.wire();
        ed
    }

    fn wire(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.buffer.connect_closure(
            "insert-text",
            true,
            glib::closure_local!(move |buffer: gtk::TextBuffer,
                                       end: gtk::TextIter,
                                       text: &str,
                                       _len: i32| {
                if let Some(ed) = weak.upgrade() {
                    ed.after_insert(&buffer, &end, text);
                }
            }),
        );
        // One value per property: applying `c:size=40` (a paste brings its
        // own tags) takes any other `c:size=` off that stretch first.
        self.buffer.connect_apply_tag(|buffer, tag, s, e| {
            let Some(name) = tag.name() else { return };
            let Some((key, _)) = name.split_once('=') else {
                return;
            };
            let prefix = format!("{key}=");
            let mut others = Vec::new();
            buffer.tag_table().foreach(|t| {
                if let Some(n) = t.name()
                    && n.starts_with(&prefix)
                    && n != name
                {
                    others.push(t.clone());
                }
            });
            for t in others {
                buffer.remove_tag(&t, s, e);
            }
        });
        let weak = Rc::downgrade(self);
        self.buffer.connect_changed(move |_| {
            if let Some(ed) = weak.upgrade()
                && !ed.loading.get()
            {
                for f in ed.changed.borrow().iter() {
                    f();
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.buffer.connect_mark_set(move |_, _, mark| {
            if let Some(ed) = weak.upgrade()
                && mark.name().as_deref() == Some("insert")
                && !ed.inserting.get()
            {
                ed.pending.borrow_mut().clear();
                ed.report();
            }
        });
        let weak = Rc::downgrade(self);
        self.search_entry.connect_activate(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.find(true);
            }
        });
        let weak = Rc::downgrade(self);
        self.search_entry.connect_search_changed(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.find_from_start();
            }
        });
        let weak = Rc::downgrade(self);
        self.search_entry.connect_next_match(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.find(true);
            }
        });
        let weak = Rc::downgrade(self);
        self.search_entry.connect_previous_match(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.find(false);
            }
        });
        let weak = Rc::downgrade(self);
        self.search_entry.connect_stop_search(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.search_bar.set_search_mode(false);
                ed.view.grab_focus();
            }
        });
    }

    pub fn connect_changed(&self, f: impl Fn() + 'static) {
        self.changed.borrow_mut().push(Box::new(f));
    }

    /// Called with the formatting at the cursor whenever it moves.
    pub fn connect_formatting(&self, f: impl Fn(Formatting) + 'static) {
        self.formatting.borrow_mut().push(Box::new(f));
    }

    pub fn load(&self, text: &RichText) {
        self.loading.set(true);
        rich::load(&self.buffer, text);
        self.loading.set(false);
        self.pending.borrow_mut().clear();
        self.report();
    }

    /// The formatting for text typed into an empty document.
    pub fn set_default_style(
        &self,
        style: Option<(omaquill::rtf::ParaStyle, omaquill::rtf::CharStyle)>,
    ) {
        *self.default_tags.borrow_mut() = match style {
            Some((p, c)) => {
                let mut c = c;
                c.link = None;
                rich::char_tags(&c)
                    .into_iter()
                    .chain(rich::para_tags(&p))
                    .collect()
            }
            None => Vec::new(),
        };
    }

    pub fn text(&self) -> RichText {
        rich::save(&self.buffer)
    }

    pub fn plain_text(&self) -> String {
        let (s, e) = self.buffer.bounds();
        self.buffer.text(&s, &e, false).to_string()
    }

    pub fn word_count(&self) -> usize {
        omaquill::rtf::word_count(&self.plain_text())
    }

    /// Tags new text should carry, from the text it was typed next to.
    fn source_tags(&self, start: &gtk::TextIter, end: &gtk::TextIter) -> Vec<String> {
        // Character formatting: at the start of a paragraph, from the text
        // that follows (if the paragraph has any); otherwise from the
        // character before.
        let mut before = *start;
        let from_before = if start.starts_line() && !end.ends_line() {
            false
        } else {
            before.backward_char()
        };
        let mut chars = if from_before {
            rich::tag_names(&before)
        } else if !end.is_end() {
            rich::tag_names(end)
        } else {
            Vec::new()
        };
        if chars.is_empty() && !from_before {
            let mut b = *start;
            if b.backward_char() {
                chars = rich::tag_names(&b);
            }
        }
        // Paragraph formatting: always the paragraph's own, which the
        // character after the insert (its text or its newline) carries.
        // Only at the very end of the text is there nothing after.
        let para = if !end.is_end() {
            rich::tag_names(end)
        } else {
            chars.clone()
        };
        chars
            .into_iter()
            .filter(|n| n.starts_with("c:"))
            .chain(para.into_iter().filter(|n| n.starts_with("p:")))
            .collect()
    }

    fn after_insert(&self, buffer: &gtk::TextBuffer, end: &gtk::TextIter, text: &str) {
        if self.loading.get() || self.inserting.get() {
            return;
        }
        self.inserting.set(true);
        let mut start = *end;
        start.backward_chars(text.chars().count() as i32);
        let mut names = self.source_tags(&start, end);
        // Nothing to inherit from (every formatted character carries at
        // least a size): an empty document takes the project's default.
        if names.is_empty() {
            names = self.default_tags.borrow().clone();
        }
        for (name, on) in self.pending.borrow().iter() {
            names.retain(|n| n != name);
            if *on {
                names.push(name.clone());
            }
        }
        // Drop what the insert inherited and apply the chosen set.
        let current = rich::tag_names(&start);
        for n in current
            .iter()
            .filter(|n| n.starts_with("c:") || n.starts_with("p:"))
        {
            buffer.remove_tag(&rich::tag(buffer, n), &start, end);
        }
        for n in &names {
            buffer.apply_tag(&rich::tag(buffer, n), &start, end);
        }
        self.inserting.set(false);
    }

    /// Formatting at the cursor (or the start of the selection).
    pub fn formatting(&self) -> Formatting {
        let buffer = &self.buffer;
        let mut at = buffer.iter_at_mark(&buffer.get_insert());
        let para = rich::tag_names(&{
            let mut p = at;
            p.set_line_offset(0);
            p
        });
        if !buffer.has_selection() {
            at.backward_char();
        } else if let Some((s, _)) = buffer.selection_bounds() {
            at = s;
        }
        let mut names = rich::tag_names(&at);
        for (name, on) in self.pending.borrow().iter() {
            names.retain(|n| n != name);
            if *on {
                names.push(name.clone());
            }
        }
        let has = |n: &str| names.iter().any(|x| x == n);
        let align = if para.iter().any(|n| n == "p:align=center") {
            Align::Center
        } else if para.iter().any(|n| n == "p:align=right") {
            Align::Right
        } else if para.iter().any(|n| n == "p:align=justify") {
            Align::Justify
        } else {
            Align::Left
        };
        Formatting {
            bold: has("c:b"),
            italic: has("c:i"),
            underline: has("c:u"),
            strike: has("c:s"),
            align,
        }
    }

    fn report(&self) {
        let f = self.formatting();
        for cb in self.formatting.borrow().iter() {
            cb(f);
        }
    }

    /// Bold, italic, underline or strikethrough (`c:b`, `c:i`, `c:u`,
    /// `c:s`) on the selection, or for what's typed next.
    pub fn toggle(&self, name: &str) {
        let buffer = &self.buffer;
        let tag = rich::tag(buffer, name);
        match buffer.selection_bounds() {
            Some((s, e)) => {
                // All of it has the tag: take it off. Otherwise put it on.
                let mut next = s;
                let all = s.has_tag(&tag) && {
                    next.forward_to_tag_toggle(Some(&tag));
                    next >= e
                };
                if all {
                    buffer.remove_tag(&tag, &s, &e);
                } else {
                    buffer.apply_tag(&tag, &s, &e);
                }
                // Tag changes don't emit `changed`; the document did change.
                for f in self.changed.borrow().iter() {
                    f();
                }
            }
            None => {
                let on = !self.formatting_has(name);
                let mut pending = self.pending.borrow_mut();
                pending.retain(|(n, _)| n != name);
                pending.push((name.to_string(), on));
            }
        }
        self.report();
    }

    fn formatting_has(&self, name: &str) -> bool {
        let f = self.formatting();
        match name {
            "c:b" => f.bold,
            "c:i" => f.italic,
            "c:u" => f.underline,
            "c:s" => f.strike,
            _ => false,
        }
    }

    /// Aligns every paragraph the selection touches.
    pub fn set_align(&self, align: Align) {
        let buffer = &self.buffer;
        let (mut s, mut e) = buffer.selection_bounds().unwrap_or_else(|| {
            let i = buffer.iter_at_mark(&buffer.get_insert());
            (i, i)
        });
        s.set_line_offset(0);
        // A selection ending at the start of a line (a triple-click) doesn't
        // take that next paragraph along.
        if e > s && e.starts_line() {
            e.backward_char();
        }
        if !e.ends_line() {
            e.forward_to_line_end();
        }
        e.forward_char(); // the newline
        for n in [
            "p:align=left",
            "p:align=center",
            "p:align=right",
            "p:align=justify",
        ] {
            buffer.remove_tag(&rich::tag(buffer, n), &s, &e);
        }
        let name = match align {
            Align::Natural | Align::Left => None,
            Align::Center => Some("p:align=center"),
            Align::Right => Some("p:align=right"),
            Align::Justify => Some("p:align=justify"),
        };
        if let Some(n) = name {
            buffer.apply_tag(&rich::tag(buffer, n), &s, &e);
        }
        for f in self.changed.borrow().iter() {
            f();
        }
        self.report();
    }

    pub fn show_find(&self) {
        self.search_bar.set_search_mode(true);
        if let Some((s, e)) = self.buffer.selection_bounds() {
            let text = self.buffer.text(&s, &e, false);
            if !text.contains('\n') {
                self.search_entry.set_text(&text);
            }
        }
        self.search_entry.grab_focus();
    }

    fn find_from_start(&self) {
        let mut at = self.buffer.iter_at_mark(&self.buffer.get_insert());
        if let Some((s, _)) = self.buffer.selection_bounds() {
            at = s;
        }
        self.buffer.place_cursor(&at);
        self.find(true);
    }

    pub fn find(&self, forward: bool) {
        let needle = self.search_entry.text();
        if needle.is_empty() {
            return;
        }
        let buffer = &self.buffer;
        let flags = gtk::TextSearchFlags::CASE_INSENSITIVE | gtk::TextSearchFlags::TEXT_ONLY;
        let (sel_start, sel_end) = buffer.selection_bounds().unwrap_or_else(|| {
            let i = buffer.iter_at_mark(&buffer.get_insert());
            (i, i)
        });
        let found = if forward {
            sel_end
                .forward_search(&needle, flags, None)
                .or_else(|| buffer.start_iter().forward_search(&needle, flags, None))
        } else {
            sel_start
                .backward_search(&needle, flags, None)
                .or_else(|| buffer.end_iter().backward_search(&needle, flags, None))
        };
        match found {
            Some((s, e)) => {
                self.search_entry.remove_css_class("error");
                buffer.select_range(&s, &e);
                let mut s = s;
                self.view.scroll_to_iter(&mut s, 0.1, true, 0.0, 0.35);
            }
            None => self.search_entry.add_css_class("error"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omaquill::rtf::{self, Align};

    fn type_at(ed: &Editor, offset: i32, text: &str) {
        let mut at = ed.buffer.iter_at_offset(offset);
        ed.buffer.insert(&mut at, text);
    }

    /// Typed text takes the formatting around it, and Ctrl+B with nothing
    /// selected applies to what's typed next. Needs a display.
    #[test]
    fn empty_paragraphs_keep_their_style_and_lines_align_alone() {
        if adw::init().is_err() {
            return;
        }
        let ed = Editor::new(true);
        let src = r"{\rtf1\pard one\
\pard\qc \
\pard three}";
        ed.load(&rtf::parse(src.as_bytes()));
        let empty = ed.plain_text().find('\n').unwrap() as i32 + 1;
        type_at(&ed, empty, "two");
        let t = ed.text();
        assert_eq!(t.paragraphs[1].text(), "two");
        assert_eq!(t.paragraphs[1].style.align, Align::Center);
        assert_eq!(t.paragraphs[0].style.align, Align::Natural);
        // Select line one as a triple-click does (up to the next line).
        let s = ed.buffer.iter_at_offset(0);
        let e = ed.buffer.iter_at_offset(4);
        ed.buffer.select_range(&s, &e);
        ed.set_align(Align::Right);
        let t = ed.text();
        assert_eq!(t.paragraphs[0].style.align, Align::Right);
        assert_eq!(t.paragraphs[1].style.align, Align::Center);
    }

    #[test]
    fn typing_carries_formatting() {
        if adw::init().is_err() {
            eprintln!("no display; skipped");
            return;
        }
        let ed = Editor::new(true);
        let src = r"{\rtf1\ansi{\fonttbl\f0 Palatino-Roman;}\pard\qc\fi720\f0\fs26 plain {\i slanted} end\
next}";
        ed.load(&rtf::parse(src.as_bytes()));
        // Inside the italic run: italic, same font and size.
        type_at(&ed, 9, "XX");
        // At the start of the second paragraph: its own style.
        let second = ed.plain_text().find("next").unwrap() as i32;
        type_at(&ed, second, "Y");
        let t = ed.text();
        let slanted: Vec<_> = t.paragraphs[0]
            .runs
            .iter()
            .filter(|r| r.style.italic)
            .collect();
        assert_eq!(slanted.len(), 1);
        assert_eq!(slanted[0].text, "slaXXnted");
        assert_eq!(slanted[0].style.size, 26);
        assert_eq!(slanted[0].style.font.as_deref(), Some("Palatino"));
        assert_eq!(t.paragraphs[1].text(), "Ynext");
        assert!(
            t.paragraphs[1]
                .runs
                .iter()
                .all(|r| !r.style.italic && r.style.size == 26)
        );

        // Enter at the end of a paragraph keeps the paragraph's style.
        let end = ed.plain_text().find(" end").unwrap() as i32 + 4;
        ed.buffer.place_cursor(&ed.buffer.iter_at_offset(end));
        type_at(&ed, end, "\nnew");
        let t = ed.text();
        assert_eq!(t.paragraphs[1].text(), "new");
        assert_eq!(t.paragraphs[1].style.align, Align::Center);
        assert_eq!(t.paragraphs[1].style.first_indent, 720);

        // Bold toggled with no selection: the next characters are bold.
        let at = ed.plain_text().find("new").unwrap() as i32 + 3;
        ed.buffer.place_cursor(&ed.buffer.iter_at_offset(at));
        ed.toggle("c:b");
        type_at(&ed, at, "B");
        let t = ed.text();
        let bold: Vec<_> = t.paragraphs[1]
            .runs
            .iter()
            .filter(|r| r.style.bold)
            .collect();
        assert_eq!(bold.len(), 1);
        assert_eq!(bold[0].text, "B");

        // A paste brings its own size; the text keeps one size, not two.
        let s0 = ed.buffer.iter_at_offset(0);
        let s1 = ed.buffer.iter_at_offset(2);
        ed.buffer
            .apply_tag(&rich::tag(&ed.buffer, "c:size=40"), &s0, &s1);
        let names = rich::tag_names(&ed.buffer.iter_at_offset(0));
        assert_eq!(names.iter().filter(|n| n.starts_with("c:size=")).count(), 1);

        // Bold on a selection, then off again.
        let s = ed.buffer.iter_at_offset(0);
        let e = ed.buffer.iter_at_offset(5);
        ed.buffer.select_range(&s, &e);
        ed.toggle("c:b");
        assert!(ed.text().paragraphs[0].runs[0].style.bold);
        assert_eq!(ed.text().paragraphs[0].runs[0].text, "plain");
        ed.toggle("c:b");
        assert!(!ed.text().paragraphs[0].runs[0].style.bold);
    }
}

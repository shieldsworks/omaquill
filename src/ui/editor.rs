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
    /// Features the file on disk would lose if this buffer were written back.
    lossy: RefCell<Vec<&'static str>>,
    changed: RefCell<Vec<Box<dyn Fn()>>>,
    formatting: RefCell<Vec<FormattingCallback>>,
    cursor: RefCell<Vec<Box<dyn Fn()>>>,
    /// Set when a paragraph has too many style runs to tag on the UI thread.
    /// The buffer then holds the plain words, and [`Self::text`] returns this
    /// document so a save cannot strip the formatting.
    painted_source: RefCell<Option<RichText>>,
    /// Typing is allowed when the buffer is fully tagged. A held document
    /// stays read-only.
    edit_when_ready: Cell<bool>,
    /// Scrolls a held document that is longer than [`HELD_WINDOW`].
    held_bar: gtk::Scrollbar,
    /// Character index of the first buffer character in that document.
    held_origin: Cell<usize>,
    /// The bar is changing the buffer, so its own signal does not run again.
    held_adjusting: Cell<bool>,
}

/// Character wrap of one unspaced line on a visible 800×600 editor, GTK
/// 4.14.5, debug build. `place_cursor` was about 3 ms at 10,000 characters,
/// 33 ms at 20,000, 85 ms at 50,000, and 165 ms at 100,000. A held document
/// longer than this is shown in a window of this many of its own characters.
const HELD_WINDOW: usize = 10_000;

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
        let held_bar = gtk::Scrollbar::new(
            gtk::Orientation::Vertical,
            Some(&gtk::Adjustment::new(0.0, 0.0, 0.0, 1.0, 1.0, 0.0)),
        );
        held_bar.set_visible(false);
        held_bar.set_halign(gtk::Align::End);
        held_bar.set_valign(gtk::Align::Fill);
        // The bar overlays the column. A sibling would shrink the text view
        // and lay the line out a second time when the bar appears.
        let overlay = gtk::Overlay::new();
        overlay.set_vexpand(true);
        scrolled.set_hexpand(true);
        overlay.set_child(Some(&scrolled));
        overlay.add_overlay(&held_bar);
        root.append(&overlay);

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
            lossy: RefCell::new(Vec::new()),
            changed: RefCell::new(Vec::new()),
            formatting: RefCell::new(Vec::new()),
            cursor: RefCell::new(Vec::new()),
            painted_source: RefCell::new(None),
            edit_when_ready: Cell::new(true),
            held_bar,
            held_origin: Cell::new(0),
            held_adjusting: Cell::new(false),
        });
        ed.wire();
        ed
    }

    fn wire(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.buffer
            .connect_insert_text(move |buffer, _iter, _text| {
                if weak.upgrade().is_some_and(|ed| ed.blocks_signal()) {
                    buffer.stop_signal_emission_by_name("insert-text");
                }
            });
        let weak = Rc::downgrade(self);
        self.buffer
            .connect_delete_range(move |buffer, _start, _end| {
                if weak.upgrade().is_some_and(|ed| ed.blocks_signal()) {
                    buffer.stop_signal_emission_by_name("delete-range");
                }
            });
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
        // Loading applies a finished document, so that scan stays off until
        // the buffer is back on screen.
        let weak = Rc::downgrade(self);
        self.buffer.connect_apply_tag(move |buffer, tag, s, e| {
            if weak.upgrade().is_some_and(|ed| ed.loading.get()) {
                return;
            }
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
                && ed.allows_edits()
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
                if !ed.loading.get() {
                    for f in ed.cursor.borrow().iter() {
                        f();
                    }
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.held_bar
            .adjustment()
            .connect_value_changed(move |adj| {
                let Some(ed) = weak.upgrade() else {
                    return;
                };
                if ed.held_adjusting.get() || ed.painted_source.borrow().is_none() {
                    return;
                }
                let origin = bar_chars(adj.value()) * HELD_WINDOW;
                if origin == ed.held_origin.get() {
                    return;
                }
                ed.held_origin.set(origin);
                ed.fill_held_window();
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

    /// Called when the cursor moves and the buffer is not loading.
    pub fn connect_cursor(&self, f: impl Fn() + 'static) {
        self.cursor.borrow_mut().push(Box::new(f));
    }

    /// `text` moves in. A held document keeps that value for a later save.
    pub fn load(&self, text: RichText) {
        self.release_plain();
        *self.lossy.borrow_mut() = text.lossy.clone();
        self.loading.set(true);
        let edit = self.edit_when_ready.get();
        // A paragraph under the threshold is tagged before this returns.
        // A heavier one is shown as plain words. Tagging it, then laying
        // that line out, blocked the UI thread for longer than a slice
        // can hide.
        if !rich::needs_plain_view(&text) {
            self.view.set_wrap_mode(gtk::WrapMode::WordChar);
            let mut paint = rich::Paint::begin(&self.buffer, &text);
            while !paint.pump(&self.buffer, f64::MAX) {}
            paint.finish(&self.buffer);
            self.loading.set(false);
            self.view.set_editable(edit);
            self.view.set_cursor_visible(edit);
            self.pending.borrow_mut().clear();
            self.report();
            return;
        }
        // Character wrap is GTK's own break. The buffer holds the file's
        // characters and no others. A line longer than [`HELD_WINDOW`] is
        // shown one window at a time, because wrapping the whole line at
        // 100,000 characters took about 170 ms.
        self.view.set_wrap_mode(gtk::WrapMode::Char);
        let plain = text.plain_text();
        let total = plain.chars().count();
        self.painted_source.borrow_mut().replace(text);
        self.held_origin.set(0);
        if total > HELD_WINDOW {
            self.fill_held_window();
            self.place_held_bar(total, 0);
        } else {
            self.held_bar.set_visible(false);
            self.buffer.begin_irreversible_action();
            self.buffer.set_text(&plain);
            self.buffer.end_irreversible_action();
            self.buffer.place_cursor(&self.buffer.start_iter());
        }
        self.buffer.set_modified(false);
        self.view.set_editable(false);
        self.view.set_cursor_visible(true);
        self.loading.set(false);
        self.pending.borrow_mut().clear();
        self.report();
    }

    /// Typing, tags, paste, drop, and undo may change the buffer.
    pub fn allows_edits(&self) -> bool {
        !self.is_too_formatted() && !self.loading.get() && self.view.is_editable()
    }

    /// `insert-text` and `delete-range` while a document is loading are the
    /// load itself. After that, a held document and a read-only view refuse
    /// every insert and delete, including paste, drop, and undo.
    fn blocks_signal(&self) -> bool {
        !self.loading.get() && !self.allows_edits()
    }

    /// Whether typing is allowed. A document held for too much formatting
    /// stays read-only even when `on` is true.
    pub fn set_editing(&self, on: bool) {
        self.edit_when_ready.set(on);
        if !self.is_too_formatted() {
            self.view.set_editable(on);
            self.view.set_cursor_visible(on);
        }
    }

    /// The buffer is the plain words of a document with too much inline
    /// formatting to tag safely. [`Self::text`] still returns that document.
    pub fn is_too_formatted(&self) -> bool {
        self.painted_source.borrow().is_some()
    }

    /// Held because one unbreakable run is long. The formatting banner would
    /// name the wrong reason, including when the tag count is also over the cap.
    pub fn held_for_length(&self) -> bool {
        self.painted_source
            .borrow()
            .as_ref()
            .is_some_and(|text| rich::breakless_chars(text) > rich::SYNC_BREAKLESS_CHARS)
    }

    /// Drops a plain-only hold. The next [`Self::load`] replaces the buffer.
    pub fn release_plain(&self) {
        let held = self.painted_source.borrow().is_some();
        self.painted_source.borrow_mut().take();
        self.held_origin.set(0);
        self.held_bar.set_visible(false);
        self.loading.set(false);
        if held {
            self.view.set_wrap_mode(gtk::WrapMode::WordChar);
            let edit = self.edit_when_ready.get();
            self.view.set_editable(edit);
            self.view.set_cursor_visible(edit);
        }
    }

    /// The buffer shows one window of a long held document. The characters
    /// are the file's, starting at [`Self::held_origin`].
    fn fill_held_window(&self) {
        let Some(plain) = self
            .painted_source
            .borrow()
            .as_ref()
            .map(|text| text.plain_text())
        else {
            return;
        };
        let origin = self.held_origin.get();
        let slice: String = plain.chars().skip(origin).take(HELD_WINDOW).collect();
        let was_loading = self.loading.get();
        self.loading.set(true);
        self.buffer.begin_irreversible_action();
        self.buffer.set_text(&slice);
        self.buffer.end_irreversible_action();
        self.buffer.place_cursor(&self.buffer.start_iter());
        self.loading.set(was_loading);
    }

    fn place_held_bar(&self, total: usize, origin: usize) {
        let pages = total.div_ceil(HELD_WINDOW).max(1);
        let page = origin / HELD_WINDOW;
        let adj = self.held_bar.adjustment();
        self.held_adjusting.set(true);
        adj.set_lower(0.0);
        adj.set_upper(pages as f64);
        adj.set_page_size(1.0);
        adj.set_page_increment(1.0);
        adj.set_step_increment(1.0);
        let max = (pages as f64 - 1.0).max(0.0);
        adj.set_value((page as f64).min(max));
        self.held_adjusting.set(false);
        self.held_bar.set_visible(pages > 1);
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
        if let Some(source) = self.painted_source.borrow().as_ref() {
            return source.clone();
        }
        rich::save(&self.buffer)
    }

    /// Content the loaded file had that this buffer does not.
    pub fn lossy(&self) -> Vec<&'static str> {
        self.lossy.borrow().clone()
    }

    pub fn plain_text(&self) -> String {
        if let Some(source) = self.painted_source.borrow().as_ref() {
            return source.plain_text();
        }
        rich::logical_plain(&self.buffer)
    }

    pub fn word_count(&self) -> usize {
        omaquill::rtf::word_count(&self.plain_text())
    }

    /// Replace All for this buffer. A held document is reported the way an
    /// image is: the plan is refused, and nothing is written.
    pub fn plan_replace(
        &self,
        query: &omaquill::replace::Query,
    ) -> Result<omaquill::replace::EditPlan, omaquill::replace::ReplaceBlock> {
        if self.is_too_formatted() {
            return Err(omaquill::replace::ReplaceBlock::Lossy(vec![
                "inline formatting",
            ]));
        }
        omaquill::replace::plan_document(&self.lossy(), &self.text(), query)
    }

    pub fn apply_edits(
        &self,
        plan: &omaquill::replace::EditPlan,
    ) -> Option<omaquill::replace::Cursor> {
        if plan.is_empty() || !self.allows_edits() {
            return None;
        }
        self.pending.borrow_mut().clear();
        self.buffer.begin_user_action();
        self.inserting.set(true);
        let edits = plan.edits();
        let mut caret = 0usize;
        for (i, edit) in edits.iter().enumerate().rev() {
            let start = edit.range().start();
            let end = edit.range().end();
            let mut from = self.buffer.iter_at_offset(text_offset(start));
            let mut to = self.buffer.iter_at_offset(text_offset(end));
            self.buffer.delete(&mut from, &mut to);
            let tags = tags_for(&self.buffer, edit.char_style());
            let mut at = self.buffer.iter_at_offset(text_offset(start));
            let origin = at.offset();
            for (part_ix, part) in edit.replacement().split('\n').enumerate() {
                let refs: Vec<&gtk::TextTag> = tags.iter().collect();
                if part_ix > 0 {
                    self.buffer.insert_with_tags(&mut at, "\n", &refs);
                }
                if !part.is_empty() {
                    let refs: Vec<&gtk::TextTag> = tags.iter().collect();
                    self.buffer.insert_with_tags(&mut at, part, &refs);
                }
            }
            paint_new_lines(&self.buffer, origin, at.offset(), edit.para_style());
            let written = edit.replacement().chars().count();
            if i + 1 == edits.len() {
                caret = start + written;
            } else {
                let delta = text_len(written) - text_len(end - start);
                caret = usize::try_from(text_len(caret) + delta)
                    .expect("the caret stays inside the document");
            }
        }
        self.inserting.set(false);
        self.buffer.end_user_action();
        let mut caret_iter = self.buffer.iter_at_offset(text_offset(caret));
        self.buffer.place_cursor(&caret_iter);
        self.view
            .scroll_to_iter(&mut caret_iter, 0.1, false, 0.0, 0.0);
        Some(omaquill::replace::Cursor::new(caret))
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
        if !self.allows_edits() {
            return;
        }
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
        if !self.allows_edits() {
            return;
        }
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
        if self.held_bar.is_visible() {
            self.find_in_held(forward);
            return;
        }
        let buffer = &self.buffer;
        let (sel_start, sel_end) = buffer.selection_bounds().unwrap_or_else(|| {
            let i = buffer.iter_at_mark(&buffer.get_insert());
            (i, i)
        });
        let flags = gtk::TextSearchFlags::CASE_INSENSITIVE | gtk::TextSearchFlags::TEXT_ONLY;
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

    /// Find in a held document whose buffer is one window of the file text.
    fn find_in_held(&self, forward: bool) {
        let needle = self.search_entry.text();
        let Some(plain) = self
            .painted_source
            .borrow()
            .as_ref()
            .map(|text| text.plain_text())
        else {
            return;
        };
        let total = plain.chars().count();
        let origin = self.held_origin.get();
        let (sel_start, sel_end) = self.buffer.selection_bounds().unwrap_or_else(|| {
            let i = self.buffer.iter_at_mark(&self.buffer.get_insert());
            (i, i)
        });
        let from = if forward {
            origin + sel_end.offset().max(0) as usize
        } else {
            origin + sel_start.offset().max(0) as usize
        };
        let hit = find_ci(&plain, needle.as_str(), forward, from).or_else(|| {
            let wrap = if forward { 0 } else { total };
            find_ci(&plain, needle.as_str(), forward, wrap)
        });
        let Some((start, end)) = hit else {
            self.search_entry.add_css_class("error");
            return;
        };
        let window_end = origin + HELD_WINDOW;
        if start < origin || end > window_end {
            let next = start / HELD_WINDOW * HELD_WINDOW;
            self.held_origin.set(next);
            self.fill_held_window();
            self.place_held_bar(total, next);
        }
        let origin = self.held_origin.get();
        let s = self
            .buffer
            .iter_at_offset(text_offset(start.saturating_sub(origin)));
        let e = self
            .buffer
            .iter_at_offset(text_offset(end.saturating_sub(origin)));
        self.search_entry.remove_css_class("error");
        self.buffer.select_range(&s, &e);
        let mut s = s;
        self.view.scroll_to_iter(&mut s, 0.1, true, 0.0, 0.35);
    }
}

fn bar_chars(value: f64) -> usize {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    // The bar's value is a page index. A page is [`HELD_WINDOW`]
    // characters, and the page count fits in the exact integer range of f64.
    value as usize
}

fn eq_ci(a: char, b: char) -> bool {
    if a == b || a.eq_ignore_ascii_case(&b) {
        return true;
    }
    let mut ac = a.to_lowercase();
    let mut bc = b.to_lowercase();
    loop {
        match (ac.next(), bc.next()) {
            (None, None) => return true,
            (Some(x), Some(y)) if x == y => {}
            _ => return false,
        }
    }
}

/// Character offsets of `needle` in `hay`. `from_char` is where the scan
/// starts. A forward scan begins there. A backward scan begins on the
/// character before it.
fn find_ci(hay: &str, needle: &str, forward: bool, from_char: usize) -> Option<(usize, usize)> {
    let hay: Vec<char> = hay.chars().collect();
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    let last = hay.len() - needle.len();
    let matches = |i: usize| {
        hay[i..i + needle.len()]
            .iter()
            .zip(needle.iter())
            .all(|(a, b)| eq_ci(*a, *b))
    };
    if forward {
        let start = from_char.min(hay.len());
        if start > last {
            return None;
        }
        return (start..=last)
            .find(|&i| matches(i))
            .map(|i| (i, i + needle.len()));
    }
    if from_char == 0 {
        return None;
    }
    let start = from_char.saturating_sub(1).min(last);
    (0..=start)
        .rev()
        .find(|&i| matches(i))
        .map(|i| (i, i + needle.len()))
}

fn text_offset(n: usize) -> i32 {
    i32::try_from(n).expect("a text offset fits in i32")
}

fn text_len(n: usize) -> isize {
    isize::try_from(n).expect("a text length fits in isize")
}

fn tags_for(buffer: &gtk::TextBuffer, style: &omaquill::rtf::CharStyle) -> Vec<gtk::TextTag> {
    rich::char_tags(style)
        .iter()
        .map(|name| rich::tag(buffer, name))
        .collect()
}

fn paint_new_lines(
    buffer: &gtk::TextBuffer,
    origin: i32,
    end: i32,
    style: &omaquill::rtf::ParaStyle,
) {
    if origin >= end {
        return;
    }
    let mut line = buffer.iter_at_offset(origin);
    if !line.starts_line() && !line.forward_line() {
        return;
    }
    while line.offset() < end {
        let start = line;
        let mut next = line;
        if !next.forward_line() {
            next = buffer.end_iter();
        }
        let mut drop = Vec::new();
        buffer.tag_table().foreach(|tag| {
            if tag.name().is_some_and(|name| name.starts_with("p:")) {
                drop.push(tag.clone());
            }
        });
        for tag in drop {
            buffer.remove_tag(&tag, &start, &next);
        }
        for name in rich::para_tags(style) {
            buffer.apply_tag(&rich::tag(buffer, &name), &start, &next);
        }
        if next.is_end() {
            break;
        }
        line = next;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use omaquill::rtf::{self, Align, CharStyle, Paragraph, RichText, Run};
    use std::cell::Cell;
    use std::rc::Rc;

    fn type_at(ed: &Editor, offset: i32, text: &str) {
        let mut at = ed.buffer.iter_at_offset(offset);
        ed.buffer.insert(&mut at, text);
    }

    /// Typed text takes the formatting around it, and Ctrl+B with nothing
    /// selected applies to what's typed next. Needs a display.
    pub(crate) fn empty_paragraphs_keep_their_style_and_lines_align_alone() {
        let ed = Editor::new(true);
        let src = r"{\rtf1\pard one\
\pard\qc \
\pard three}";
        ed.load(rtf::parse(src.as_bytes()));
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

    pub(crate) fn typing_carries_formatting() {
        let ed = Editor::new(true);
        let src = r"{\rtf1\ansi{\fonttbl\f0 Palatino-Roman;}\pard\qc\fi720\f0\fs26 plain {\i slanted} end\
next}";
        ed.load(rtf::parse(src.as_bytes()));
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

        // Bold on a selection, then off again.
        let s = ed.buffer.iter_at_offset(0);
        let e = ed.buffer.iter_at_offset(5);
        ed.buffer.select_range(&s, &e);
        ed.toggle("c:b");
        assert!(ed.text().paragraphs[0].runs[0].style.bold);
        assert_eq!(ed.text().paragraphs[0].runs[0].text, "plain");
        ed.toggle("c:b");
        assert!(!ed.text().paragraphs[0].runs[0].style.bold);

        // A paste brings its own size; the text keeps one size, not two.
        let s0 = ed.buffer.iter_at_offset(0);
        let s1 = ed.buffer.iter_at_offset(2);
        ed.buffer
            .apply_tag(&rich::tag(&ed.buffer, "c:size=40"), &s0, &s1);
        let names = rich::tag_names(&ed.buffer.iter_at_offset(0));
        assert_eq!(names.iter().filter(|n| n.starts_with("c:size=")).count(), 1);
    }

    /// A spelling underline is a tag the file does not store.
    pub(crate) fn a_spelling_mark_is_not_saved() {
        let ed = Editor::new(true);
        ed.load(rtf::parse(br"{\rtf1 hello helo}"));
        let before = ed.text();
        let tag = gtk::TextTag::new(Some("spell"));
        tag.set_underline(gtk::pango::Underline::Error);
        ed.buffer.tag_table().add(&tag);
        let start = ed.buffer.iter_at_offset(6);
        let end = ed.buffer.iter_at_offset(10);
        ed.buffer.apply_tag(&tag, &start, &end);
        assert_eq!(ed.text(), before);
        assert_eq!(ed.plain_text(), "hello helo");
    }

    pub(crate) fn replace_all_is_one_undo_and_keeps_the_run_style() {
        let ed = Editor::new(true);
        let src =
            r"{\rtf1\ansi{\fonttbl\f0 Palatino-Roman;}\pard\f0\fs24 Say hello and {\i hello}.}";
        ed.load(rtf::parse(src.as_bytes()));
        let query = omaquill::replace::query(
            "hello",
            "hi",
            omaquill::replace::Options {
                case: omaquill::replace::Case::Sensitive,
                word: omaquill::replace::Word::Any,
                mode: omaquill::replace::Mode::Literal,
            },
        )
        .unwrap();
        let plan = omaquill::replace::plan_all(&ed.text(), &query);
        ed.apply_edits(&plan);
        let text = ed.text();
        let plain = text.plain_text();
        assert_eq!(plain, "Say hi and hi.");
        let styled: Vec<bool> = text.paragraphs[0]
            .runs
            .iter()
            .flat_map(|run| run.text.chars().map(|_| run.style.italic))
            .collect();
        let first = plain.find("hi").unwrap();
        let second = plain.rfind("hi").unwrap();
        assert!(styled[first..first + 2].iter().all(|italic| !italic));
        assert!(styled[second..second + 2].iter().all(|italic| *italic));
        assert!(ed.buffer.can_undo());
        ed.buffer.undo();
        assert_eq!(ed.plain_text(), "Say hello and hello.");
        assert!(!ed.buffer.can_undo());
    }

    pub(crate) fn replace_all_refuses_a_loaded_image() {
        let ed = Editor::new(true);
        let image = rtf::parse(br"{\rtf1\ansi\pard alpha {\pict\pngblip deadbeef} beta}");
        ed.load(image);
        assert_eq!(ed.lossy(), ["images"]);
        let query = omaquill::replace::query(
            "alpha",
            "omega",
            omaquill::replace::Options {
                case: omaquill::replace::Case::Sensitive,
                word: omaquill::replace::Word::Any,
                mode: omaquill::replace::Mode::Literal,
            },
        )
        .unwrap();
        let blocked = omaquill::replace::plan_document(&ed.lossy(), &ed.text(), &query);
        assert!(
            matches!(
                blocked,
                Err(omaquill::replace::ReplaceBlock::Lossy(ref reason))
                    if reason.as_slice() == ["images"]
            ),
            "{blocked:?}"
        );
        assert!(ed.plain_text().contains("alpha"));
        ed.load(rtf::parse(br"{\rtf1\ansi\pard alpha beta}"));
        assert!(ed.lossy().is_empty());
        let plan = omaquill::replace::plan_document(&ed.lossy(), &ed.text(), &query).unwrap();
        ed.apply_edits(&plan).unwrap();
        assert_eq!(ed.plain_text(), "omega beta");
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

    /// A style change is not a line break. Find uses the buffer's own
    /// search, so a needle that sits on two runs has to be one span of text.
    pub(crate) fn find_matches_across_a_style_boundary() {
        let mut paragraph = Paragraph::default();
        for i in 0..32 {
            paragraph.runs.push(Run {
                text: "q".into(),
                style: CharStyle {
                    bold: i % 2 == 0,
                    ..CharStyle::default()
                },
            });
        }
        paragraph.runs.push(Run {
            text: "z".into(),
            style: CharStyle {
                italic: true,
                ..CharStyle::default()
            },
        });
        paragraph.runs.push(Run {
            text: "z".into(),
            style: CharStyle::default(),
        });
        let text = RichText {
            paragraphs: vec![paragraph],
            lossy: Vec::new(),
        };
        let ed = Editor::new(true);
        ed.load(text);
        assert_eq!(
            ed.buffer.line_count(),
            1,
            "one paragraph became {} lines",
            ed.buffer.line_count()
        );
        ed.search_entry.set_text("qz");
        ed.find(true);
        let (start, end) = ed
            .buffer
            .selection_bounds()
            .expect("find missed qz where a style run changes");
        assert_eq!(ed.buffer.text(&start, &end, false).as_str(), "qz");
        assert!(!ed.search_entry.has_css_class("error"));
    }

    /// The editor window opens a run-heavy document. The view is on screen,
    /// which is where a per-run insert relayouts.
    pub(crate) fn large_documents_open_in_the_window() {
        let ed = Editor::new(true);
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&ed.root));
        window.present();
        let context = gtk::glib::MainContext::default();
        for _ in 0..20 {
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
        let runs = 64usize;
        let text = toggled(runs);
        let started = std::time::Instant::now();
        ed.load(text);
        let mut shown_ms = started.elapsed().as_secs_f64() * 1000.0;
        for _ in 0..30 {
            if ed.view.iter_location(&ed.buffer.start_iter()).height() > 0 {
                shown_ms = started.elapsed().as_secs_f64() * 1000.0;
                break;
            }
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
        eprintln!("gtk open runs={runs} ms={shown_ms:.1}");
        assert_eq!(ed.buffer.line_count(), 1);
        let names = rich::tag_names(&ed.buffer.start_iter());
        assert!(names.iter().any(|name| name == "c:b"));
        assert!(names.iter().any(|name| name.starts_with("c:link=")));
        let back = ed.text();
        assert_eq!(back.paragraphs.len(), 1);
        assert_eq!(back.paragraphs[0].runs.len(), runs);
        window.close();
    }

    /// A paragraph GTK cannot tag without a long block shows its words at
    /// once, on one line, without the style tags. Find works on those words.
    /// The saved form stays the source document, so a save cannot strip the
    /// formatting. The view stays read-only.
    pub(crate) fn a_hostile_paragraph_opens_plain_and_read_only() {
        let mut text = toggled(2_000);
        text.paragraphs[0].runs[1_000].text = "q".into();
        text.paragraphs[0].runs[1_001].text = "z".into();
        let ed = Editor::new(true);
        let owned = text.clone();
        let started = std::time::Instant::now();
        ed.load(owned);
        let open_ms = started.elapsed().as_secs_f64() * 1000.0;
        eprintln!("hostile open ms={open_ms:.1}");
        assert!(
            open_ms < 100.0,
            "open blocked the thread for {open_ms:.1} ms"
        );
        assert!(ed.is_too_formatted());
        assert!(!ed.held_for_length());
        assert!(!ed.view.is_editable());
        assert_eq!(ed.view.wrap_mode(), gtk::WrapMode::Char);
        assert_eq!(ed.buffer.line_count(), 1);
        assert_eq!(ed.text(), text);
        assert!(ed.plain_text().contains("qz"));
        let names = rich::tag_names(&ed.buffer.start_iter());
        assert!(
            !names.iter().any(|name| name == "c:b"),
            "plain open applied a bold tag: {names:?}"
        );
        ed.search_entry.set_text("qz");
        ed.find(true);
        let (start, end) = ed
            .buffer
            .selection_bounds()
            .expect("find missed qz in the plain paragraph");
        assert_eq!(ed.buffer.text(&start, &end, false).as_str(), "qz");
        let context = gtk::glib::MainContext::default();
        for _ in 0..10 {
            if !context.iteration(false) {
                break;
            }
        }
        assert!(ed.is_too_formatted());
        assert!(!ed.view.is_editable());
        assert_eq!(ed.text(), text);
        assert_eq!(ed.buffer.line_count(), 1);
    }

    /// Loading another document drops the plain hold. Later idle turns must
    /// not write the old paragraph back.
    pub(crate) fn switching_mid_paint_leaves_the_new_document() {
        let big = toggled(2_000);
        let ed = Editor::new(true);
        ed.load(big.clone());
        assert!(ed.is_too_formatted());
        assert_eq!(ed.text(), big);
        let hello = rtf::RichText::from_plain("Hello");
        ed.load(hello);
        assert!(!ed.is_too_formatted());
        assert!(ed.view.is_editable());
        assert_eq!(ed.plain_text(), "Hello");
        assert_eq!(ed.text().plain_text(), "Hello");
        let context = gtk::glib::MainContext::default();
        for _ in 0..40 {
            if !context.iteration(false) {
                break;
            }
        }
        assert!(!ed.is_too_formatted());
        assert_eq!(ed.plain_text(), "Hello");
        assert_eq!(ed.text().plain_text(), "Hello");
    }

    /// A held document accepts no edit. Alignment, paste, undo, and replace
    /// leave the buffer and the source alone, and replace is reported the
    /// way an image document is.
    pub(crate) fn a_held_document_refuses_every_edit() {
        let mut text = toggled(2_000);
        text.paragraphs[0].style.align = Align::Center;
        text.paragraphs[0].runs[10].text = " helo ".into();
        let ed = Editor::new(true);
        ed.load(text.clone());
        assert!(ed.is_too_formatted());
        assert!(!ed.held_for_length());
        assert!(!ed.loading.get());
        assert!(!ed.allows_edits());
        assert!(!ed.view.is_editable());
        let plain = ed.plain_text();
        let dirtied = Rc::new(Cell::new(false));
        let flag = dirtied.clone();
        ed.connect_changed(move || flag.set(true));
        ed.set_align(Align::Left);
        ed.toggle("c:b");
        assert!(!dirtied.get());
        assert_eq!(ed.text(), text);
        assert_eq!(ed.text().paragraphs[0].style.align, Align::Center);
        let mut at = ed.buffer.start_iter();
        ed.buffer.insert(&mut at, "ZZ");
        assert_eq!(ed.plain_text(), plain, "insert changed a held buffer");
        assert!(!dirtied.get());
        ed.buffer.undo();
        assert_eq!(ed.plain_text(), plain);
        let query = omaquill::replace::query(
            "helo",
            "hello",
            omaquill::replace::Options {
                case: omaquill::replace::Case::Sensitive,
                word: omaquill::replace::Word::Any,
                mode: omaquill::replace::Mode::Literal,
            },
        )
        .unwrap();
        let blocked = ed.plan_replace(&query);
        assert!(
            matches!(
                blocked,
                Err(omaquill::replace::ReplaceBlock::Lossy(ref reason))
                    if reason.as_slice() == ["inline formatting"]
            ),
            "{blocked:?}"
        );
        let plan = omaquill::replace::plan_document(&[], &text, &query).unwrap();
        assert!(ed.apply_edits(&plan).is_none());
        assert_eq!(ed.plain_text(), plain);
        assert_eq!(ed.text(), text);
        assert!(!dirtied.get());
    }

    /// Adjacent runs with the same style are one GTK range, so a uniform
    /// paragraph under the line-length cap is styled before load returns.
    pub(crate) fn identical_runs_open_without_an_idle_pass() {
        let runs = 5_000usize;
        let mut paragraph = Paragraph::default();
        for _ in 0..runs {
            paragraph.runs.push(Run {
                text: "a".into(),
                style: CharStyle {
                    bold: true,
                    ..CharStyle::default()
                },
            });
        }
        let text = RichText {
            paragraphs: vec![paragraph],
            lossy: Vec::new(),
        };
        let ed = Editor::new(true);
        ed.load(text);
        assert!(!ed.is_too_formatted());
        assert!(ed.view.is_editable());
        assert_eq!(ed.buffer.line_count(), 1);
        let names = rich::tag_names(&ed.buffer.start_iter());
        assert!(names.iter().any(|name| name == "c:b"));
        let back = ed.text();
        assert_eq!(back.paragraphs.len(), 1);
        assert_eq!(back.paragraphs[0].runs.len(), 1);
        assert_eq!(back.paragraphs[0].runs[0].text.len(), runs);
    }

    /// One uniform style merges to a couple of tag applies, so the tag cap
    /// does not hold it. The line is one unspaced run, and word-char layout
    /// of that run is what blocks the UI thread.
    pub(crate) fn a_long_uniform_line_opens_plain() {
        let runs = 10_000usize;
        let mut paragraph = Paragraph::default();
        for _ in 0..runs {
            paragraph.runs.push(Run {
                text: "a".into(),
                style: CharStyle {
                    bold: true,
                    ..CharStyle::default()
                },
            });
        }
        let text = RichText {
            paragraphs: vec![paragraph],
            lossy: Vec::new(),
        };
        assert!(rich::heavy_applies(&text) <= rich::SYNC_TAG_APPLIES);
        assert!(rich::needs_plain_view(&text));
        let ed = Editor::new(true);
        let owned = text.clone();
        let started = std::time::Instant::now();
        ed.load(owned);
        let open_ms = started.elapsed().as_secs_f64() * 1000.0;
        eprintln!("uniform plain runs={runs} open_ms={open_ms:.1}");
        assert!(
            open_ms < 100.0,
            "open blocked the thread for {open_ms:.1} ms"
        );
        assert!(ed.is_too_formatted());
        assert!(ed.held_for_length());
        assert!(!ed.loading.get());
        assert!(!ed.view.is_editable());
        assert_eq!(ed.view.wrap_mode(), gtk::WrapMode::Char);
        assert_eq!(ed.buffer.line_count(), 1);
        assert_eq!(ed.text(), text);
        assert_eq!(ed.plain_text().chars().count(), runs);
        let names = rich::tag_names(&ed.buffer.start_iter());
        assert!(
            !names.iter().any(|name| name == "c:b"),
            "plain open applied a bold tag: {names:?}"
        );
        let plain = ed.plain_text();
        let mut at = ed.buffer.start_iter();
        ed.buffer.insert(&mut at, "ZZ");
        assert_eq!(ed.plain_text(), plain);
    }

    /// Many ordinary paragraphs stay fully styled. The plain hold is for one
    /// paragraph with a large tag count or one long unspaced run, not for a
    /// long manuscript.
    pub(crate) fn many_plain_paragraphs_stay_editable() {
        let mut paragraphs = Vec::new();
        for _ in 0..2_000 {
            paragraphs.push(Paragraph {
                runs: vec![Run {
                    text: "Hello".into(),
                    style: CharStyle::default(),
                }],
                ..Paragraph::default()
            });
        }
        let ed = Editor::new(true);
        ed.load(RichText {
            paragraphs,
            lossy: Vec::new(),
        });
        assert!(!ed.is_too_formatted());
        assert!(ed.view.is_editable());
        assert_eq!(ed.buffer.line_count(), 2_000);
    }

    fn uniform(runs: usize) -> RichText {
        let mut paragraph = Paragraph::default();
        for _ in 0..runs {
            paragraph.runs.push(Run {
                text: "a".into(),
                style: CharStyle {
                    bold: true,
                    ..CharStyle::default()
                },
            });
        }
        RichText {
            paragraphs: vec![paragraph],
            lossy: Vec::new(),
        }
    }

    fn one_line(chars: usize) -> RichText {
        RichText {
            paragraphs: vec![Paragraph {
                runs: vec![Run {
                    text: "a".repeat(chars),
                    style: CharStyle::default(),
                }],
                ..Paragraph::default()
            }],
            lossy: Vec::new(),
        }
    }

    fn time_open(ed: &Editor, context: &gtk::glib::MainContext, label: &str, text: &RichText) {
        let chars = text.plain_text().chars().count();
        // The copy stays here so the timed load matches the window, which
        // moves the document it already owns.
        let owned = text.clone();
        let started = std::time::Instant::now();
        ed.load(owned);
        let open_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(ed.buffer.line_count(), 1);
        assert!(ed.is_too_formatted());
        assert!(!ed.view.is_editable());
        assert!(!ed.loading.get());
        let mut max_iter_ms = 0.0f64;
        let mut shown = false;
        for _ in 0..40 {
            let step = std::time::Instant::now();
            let ran = context.iteration(false);
            let iter_ms = step.elapsed().as_secs_f64() * 1000.0;
            if ran {
                max_iter_ms = max_iter_ms.max(iter_ms);
            }
            if ed.view.iter_location(&ed.buffer.start_iter()).height() > 0 {
                shown = true;
            }
            if shown && !context.pending() {
                break;
            }
        }
        eprintln!(
            "gtk plain shape={label} chars={chars} buf={} lines={} open_ms={open_ms:.1} max_iter_ms={max_iter_ms:.1} shown={shown} bar={}",
            ed.buffer.char_count(),
            ed.buffer.line_count(),
            ed.held_bar.is_visible()
        );
        assert!(
            open_ms < 100.0,
            "{label} open blocked {open_ms:.1} ms at {chars}"
        );
        assert!(
            max_iter_ms < 100.0,
            "{label} main-loop turn blocked {max_iter_ms:.1} ms at {chars}"
        );
        assert!(shown, "{label} did not get a line height");
        assert_eq!(ed.view.wrap_mode(), gtk::WrapMode::Char);
        let start_y = ed.view.iter_location(&ed.buffer.start_iter()).y();
        let mut last = ed.buffer.end_iter();
        assert!(last.backward_char(), "{label} was empty");
        let end_y = ed.view.iter_location(&last).y();
        assert!(
            end_y > start_y,
            "{label} did not wrap the line, start {start_y} end {end_y}"
        );
        assert_eq!(ed.plain_text(), text.plain_text());
        assert_eq!(ed.plain_text().chars().count(), chars);
        assert_eq!(ed.text(), *text);
        let (buf_start, buf_end) = ed.buffer.bounds();
        let shown = ed.buffer.text(&buf_start, &buf_end, false);
        assert!(
            text.plain_text().starts_with(shown.as_str()),
            "{label} buffer is not a prefix of the file"
        );
        assert_eq!(shown.chars().count(), chars.min(HELD_WINDOW));
        if chars > HELD_WINDOW {
            assert!(ed.held_bar.is_visible());
            let adj = ed.held_bar.adjustment();
            let started = std::time::Instant::now();
            adj.set_value(adj.upper());
            let scroll_ms = started.elapsed().as_secs_f64() * 1000.0;
            let mut scroll_iter = 0.0f64;
            for _ in 0..20 {
                let step = std::time::Instant::now();
                if !context.iteration(false) && !context.pending() {
                    break;
                }
                scroll_iter = scroll_iter.max(step.elapsed().as_secs_f64() * 1000.0);
            }
            eprintln!(
                "gtk plain scroll={label} chars={chars} scroll_ms={scroll_ms:.1} max_iter_ms={scroll_iter:.1}"
            );
            assert!(
                scroll_ms < 100.0,
                "{label} scroll blocked {scroll_ms:.1} ms at {chars}"
            );
            assert!(
                scroll_iter < 100.0,
                "{label} scroll turn blocked {scroll_iter:.1} ms at {chars}"
            );
            let (tail_start, tail_end) = ed.buffer.bounds();
            let tail = ed.buffer.text(&tail_start, &tail_end, false);
            assert!(
                text.plain_text().ends_with(tail.as_str()),
                "{label} tail is not the file text"
            );
            assert_eq!(tail.chars().count(), HELD_WINDOW.min(chars));
        } else {
            assert!(!ed.held_bar.is_visible());
        }
        let names = rich::tag_names(&ed.buffer.start_iter());
        assert!(
            !names.iter().any(|name| name == "c:b"),
            "{label} applied a bold tag"
        );
    }

    /// Word-char layout cost of one unspaced line, before the plain-view
    /// cap. Set OMAQUILL_GTK_OPEN=probe. This is the cost the cap avoids.
    fn probe_word_char(ed: &Editor, context: &gtk::glib::MainContext) {
        ed.view.set_wrap_mode(gtk::WrapMode::WordChar);
        for chars in [2_048usize, 4_096, 8_192, 10_000] {
            let body = "a".repeat(chars);
            let started = std::time::Instant::now();
            ed.buffer.set_text(&body);
            let open_ms = started.elapsed().as_secs_f64() * 1000.0;
            let mut max_iter_ms = 0.0f64;
            for _ in 0..20 {
                let step = std::time::Instant::now();
                if !context.iteration(false) && !context.pending() {
                    break;
                }
                max_iter_ms = max_iter_ms.max(step.elapsed().as_secs_f64() * 1000.0);
            }
            eprintln!(
                "gtk wordchar chars={chars} open_ms={open_ms:.1} max_iter_ms={max_iter_ms:.1} lines={}",
                ed.buffer.line_count()
            );
        }
    }

    /// A held unspaced line wraps on characters inside the column. The buffer
    /// is the file text, and find matches that text.
    pub(crate) fn a_held_long_line_wraps_on_characters() {
        let ed = Editor::new(true);
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&ed.root));
        window.present();
        let chars = 10_000usize;
        let body = "a".repeat(chars);
        let started = std::time::Instant::now();
        ed.load(one_line(chars));
        let open_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert!(open_ms < 100.0, "open blocked {open_ms:.1} ms");
        assert!(ed.is_too_formatted());
        assert!(ed.held_for_length());
        assert!(!ed.view.is_editable());
        assert_eq!(ed.view.wrap_mode(), gtk::WrapMode::Char);
        assert_eq!(ed.plain_text(), body);
        let context = gtk::glib::MainContext::default();
        let mut max_iter = 0.0f64;
        let mut shown = false;
        for _ in 0..40 {
            let step = std::time::Instant::now();
            let ran = context.iteration(false);
            if ran {
                max_iter = max_iter.max(step.elapsed().as_secs_f64() * 1000.0);
            }
            if ed.view.iter_location(&ed.buffer.start_iter()).height() > 0 {
                shown = true;
            }
            if shown && !context.pending() {
                break;
            }
        }
        assert!(shown, "the line was not allocated");
        assert!(max_iter < 100.0, "main-loop turn blocked {max_iter:.1} ms");
        let start_y = ed.view.iter_location(&ed.buffer.start_iter()).y();
        let mut last = ed.buffer.end_iter();
        assert!(last.backward_char());
        let end_y = ed.view.iter_location(&last).y();
        assert!(
            end_y > start_y,
            "the line did not wrap, start {start_y} end {end_y}"
        );
        ed.search_entry.set_text(&"a".repeat(100));
        ed.find(true);
        let (start, end) = ed.buffer.selection_bounds().expect("find missed the run");
        assert_eq!(
            ed.buffer.text(&start, &end, false).as_str(),
            "a".repeat(100)
        );
        window.close();
    }

    /// A zero-width space in the file is text. Open, an edit beside it, and
    /// replace keep that character. The saved RTF matches a write of that
    /// document, and the replace starts on the character after the space.
    pub(crate) fn a_zero_width_space_survives_edit_and_replace() {
        let source = "pre\u{200B}post";
        let file = rtf::write(&RichText::from_plain(source));
        assert!(
            file.contains("\\uc0\\u8203 "),
            "the file dropped U+200B: {file}"
        );
        let parsed = rtf::parse(file.as_bytes());
        assert_eq!(parsed.plain_text(), source);
        assert_eq!(parsed.paragraphs[0].runs.len(), 1);
        let ed = Editor::new(true);
        ed.load(parsed.clone());
        assert!(!ed.is_too_formatted());
        assert!(ed.allows_edits());
        assert_eq!(ed.view.wrap_mode(), gtk::WrapMode::WordChar);
        assert_eq!(ed.plain_text(), source);
        let (start, end) = ed.buffer.bounds();
        assert_eq!(ed.buffer.text(&start, &end, false).as_str(), source);
        assert_eq!(
            rtf::write(&ed.text()).as_bytes(),
            rtf::write(&parsed).as_bytes()
        );

        let mut at = ed.buffer.iter_at_offset(3);
        ed.buffer.insert(&mut at, "X");
        assert_eq!(ed.plain_text(), "preX\u{200B}post");
        let mut expected = parsed.clone();
        expected.paragraphs[0].runs[0].text = "preX\u{200B}post".into();
        assert_eq!(
            rtf::write(&ed.text()).as_bytes(),
            rtf::write(&expected).as_bytes()
        );

        ed.load(parsed.clone());
        assert_eq!(ed.plain_text(), source);
        let query = omaquill::replace::query(
            "post",
            "end",
            omaquill::replace::Options {
                case: omaquill::replace::Case::Sensitive,
                word: omaquill::replace::Word::Any,
                mode: omaquill::replace::Mode::Literal,
            },
        )
        .unwrap();
        let plan = ed.plan_replace(&query).unwrap();
        let edit = plan.edits().first().expect("post was not found");
        assert_eq!(edit.range().start(), 4);
        ed.apply_edits(&plan).unwrap();
        assert_eq!(ed.plain_text(), "pre\u{200B}end");
        expected.paragraphs[0].runs[0].text = "pre\u{200B}end".into();
        let saved = rtf::write(&ed.text());
        assert!(saved.contains("\\uc0\\u8203 "));
        assert_eq!(saved.as_bytes(), rtf::write(&expected).as_bytes());
    }

    /// A match past the first window moves that window. The selected text is
    /// the file's own characters.
    pub(crate) fn find_in_a_long_held_line_reaches_the_tail() {
        let ed = Editor::new(true);
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&ed.root));
        window.present();
        let mut body = "a".repeat(HELD_WINDOW);
        body.push_str("TAIL");
        ed.load(RichText::from_plain(&body));
        assert!(ed.is_too_formatted());
        assert!(ed.held_bar.is_visible());
        let (start, end) = ed.buffer.bounds();
        assert!(!ed.buffer.text(&start, &end, false).contains("TAIL"));
        ed.search_entry.set_text("TAIL");
        ed.find(true);
        let (start, end) = ed.buffer.selection_bounds().expect("find missed the tail");
        assert_eq!(ed.buffer.text(&start, &end, false).as_str(), "TAIL");
        assert!(ed.plain_text().ends_with("TAIL"));
        window.close();
    }

    /// Open times for one paragraph on a visible view. Uniform runs,
    /// alternating runs, and one unspaced line, at 10k, 50k, and 100k.
    /// Set OMAQUILL_GTK_OPEN to one of those counts to time only that size.
    /// Set it to `probe` to time word-char layout under the cap. A held
    /// paragraph stays one buffer line and wraps on characters.
    pub(crate) fn time_styled_paragraph_open() {
        let which = std::env::var("OMAQUILL_GTK_OPEN").unwrap_or_default();
        let ed = Editor::new(true);
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&ed.root));
        window.present();
        let context = gtk::glib::MainContext::default();
        for _ in 0..20 {
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
        if which == "probe" {
            probe_word_char(&ed, &context);
            window.close();
            return;
        }
        let sizes: Vec<usize> = if which.is_empty() || which == "1" {
            vec![10_000, 50_000, 100_000]
        } else if let Ok(n) = which.parse() {
            vec![n]
        } else {
            vec![10_000, 50_000, 100_000]
        };
        for chars in sizes {
            time_open(&ed, &context, "uniform", &uniform(chars));
            time_open(&ed, &context, "alternating", &toggled(chars));
            time_open(&ed, &context, "line", &one_line(chars));
        }
        window.close();
    }
}

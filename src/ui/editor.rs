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
    pub scrolled: gtk::ScrolledWindow,
    page: bool,
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
    /// The plain buffer contains [`rich::DISPLAY_BREAK`] so an unbreakable
    /// run can wrap. Those characters are not part of the document.
    display_breaks: Cell<bool>,
    /// Called when an insert would make an unbreakable run longer than the
    /// layout cap. The insert does not land.
    line_limit: RefCell<Vec<Box<dyn Fn()>>>,
    /// The view is the scrolled window's child, not the column clamp.
    side_scroll: Cell<bool>,
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
            scrolled,
            page,
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
            display_breaks: Cell::new(false),
            line_limit: RefCell::new(Vec::new()),
            side_scroll: Cell::new(false),
        });
        ed.wire();
        ed
    }

    fn wire(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.buffer.connect_insert_text(move |buffer, iter, text| {
            let Some(ed) = weak.upgrade() else {
                return;
            };
            if ed.blocks_signal() || ed.refuses_line(iter, text) {
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
        self.display_breaks.set(false);
        if !rich::needs_plain_view(&text) {
            // The previous document may have been held, which scrolls sideways
            // or uses word wrap.
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
        // Word-char wrap of an unspaced line of 10,000 characters took
        // 101.8 ms on the next main-loop turn. A line over the cap scrolls
        // sideways: word wrap of a display break every 80 characters laid
        // the paragraph out inside `place_cursor`, about 80 ms at 100,000
        // characters. A shorter unbreakable run, inside a document held for
        // its formatting, still wraps in the column. Those breaks are not
        // part of `text`.
        let plain = text.plain_text();
        let long = rich::breakless_str(&plain) > rich::SYNC_BREAKLESS_CHARS;
        let shown = if long {
            self.display_breaks.set(false);
            self.set_side_scroll(true);
            plain
        } else {
            self.set_side_scroll(false);
            let shown = rich::with_display_breaks(&plain);
            self.display_breaks.set(shown != plain);
            self.view.set_wrap_mode(gtk::WrapMode::Word);
            shown
        };
        self.buffer.begin_irreversible_action();
        self.buffer.set_text(&shown);
        self.buffer.end_irreversible_action();
        self.buffer.place_cursor(&self.buffer.start_iter());
        self.buffer.set_modified(false);
        self.painted_source.borrow_mut().replace(text);
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

    /// An insert that would leave an unbreakable run longer than
    /// [`rich::SYNC_BREAKLESS_CHARS`]. Word-char layout of that run blocks
    /// the UI thread. The insert is stopped, and the callbacks run so the
    /// window can say why.
    fn refuses_line(&self, at: &gtk::TextIter, text: &str) -> bool {
        if self.loading.get() || self.inserting.get() || !self.allows_edits() {
            return false;
        }
        let mut start = *at;
        start.set_line_offset(0);
        let mut end = *at;
        if !end.ends_line() {
            end.forward_to_line_end();
        }
        let before = self.buffer.text(&start, at, false);
        let after = self.buffer.text(at, &end, false);
        let mut combined = String::with_capacity(before.len() + text.len() + after.len());
        combined.push_str(before.as_str());
        combined.push_str(text);
        combined.push_str(after.as_str());
        if rich::breakless_str(&combined) <= rich::SYNC_BREAKLESS_CHARS {
            return false;
        }
        for f in self.line_limit.borrow().iter() {
            f();
        }
        true
    }

    /// Fired when [`Self::refuses_line`] stops an insert.
    pub fn connect_line_limit(&self, f: impl Fn() + 'static) {
        self.line_limit.borrow_mut().push(Box::new(f));
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

    /// A line over the breakless cap is one unwrapped row. The page editor's
    /// clamp would clip that row, and a bar on the scrolled window cannot
    /// scroll a child that stays inside the clamp. The view becomes the
    /// scrolled window's child for that document, and goes back in the clamp
    /// for every other one.
    fn set_side_scroll(&self, on: bool) {
        if self.side_scroll.get() == on {
            return;
        }
        self.side_scroll.set(on);
        if on {
            self.view.set_wrap_mode(gtk::WrapMode::None);
            self.scrolled
                .set_hscrollbar_policy(gtk::PolicyType::Automatic);
            if self.page {
                self.clamp.set_child(None::<&gtk::Widget>);
                self.scrolled.set_child(Some(&self.view));
            }
        } else if self.page {
            self.scrolled.set_hscrollbar_policy(gtk::PolicyType::Never);
            self.clamp.set_maximum_size(760);
            self.clamp.set_tightening_threshold(600);
            self.scrolled.set_child(None::<&gtk::Widget>);
            self.clamp.set_child(Some(&self.view));
            self.scrolled.set_child(Some(&self.clamp));
        } else {
            self.scrolled.set_hscrollbar_policy(gtk::PolicyType::Never);
        }
    }

    /// Drops a plain-only hold. The next [`Self::load`] replaces the buffer.
    pub fn release_plain(&self) {
        let held = self.painted_source.borrow().is_some();
        self.painted_source.borrow_mut().take();
        self.display_breaks.set(false);
        self.set_side_scroll(false);
        self.loading.set(false);
        if held {
            let edit = self.edit_when_ready.get();
            self.view.set_editable(edit);
            self.view.set_cursor_visible(edit);
        }
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
        let buffer = &self.buffer;
        let (sel_start, sel_end) = buffer.selection_bounds().unwrap_or_else(|| {
            let i = buffer.iter_at_mark(&buffer.get_insert());
            (i, i)
        });
        let found = if self.display_breaks.get() {
            self.find_across_breaks(
                forward,
                needle.as_str(),
                sel_start.offset(),
                sel_end.offset(),
            )
        } else {
            let flags = gtk::TextSearchFlags::CASE_INSENSITIVE | gtk::TextSearchFlags::TEXT_ONLY;
            if forward {
                sel_end
                    .forward_search(&needle, flags, None)
                    .or_else(|| buffer.start_iter().forward_search(&needle, flags, None))
            } else {
                sel_start
                    .backward_search(&needle, flags, None)
                    .or_else(|| buffer.end_iter().backward_search(&needle, flags, None))
            }
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

    /// Find in a plain view. Display breaks are not characters of the
    /// document, so a match may cross one.
    fn find_across_breaks(
        &self,
        forward: bool,
        needle: &str,
        sel_start: i32,
        sel_end: i32,
    ) -> Option<(gtk::TextIter, gtk::TextIter)> {
        let (raw_start, raw_end) = self.buffer.bounds();
        let raw = self.buffer.text(&raw_start, &raw_end, false);
        let mut logical = String::new();
        let mut map = Vec::new();
        let mut off = 0i32;
        for ch in raw.chars() {
            if ch != rich::DISPLAY_BREAK {
                logical.push(ch);
                map.push(off);
            }
            off += 1;
        }
        let from = if forward { sel_end } else { sel_start };
        let logical_from = map
            .iter()
            .position(|offset| *offset >= from)
            .unwrap_or(map.len());
        let hit = find_ci(logical.as_str(), needle, forward, logical_from).or_else(|| {
            let wrap = if forward { 0 } else { logical.chars().count() };
            find_ci(logical.as_str(), needle, forward, wrap)
        })?;
        let (start, end) = hit;
        let start_off = *map.get(start)?;
        let end_off = map.get(end).copied().unwrap_or(off);
        Some((
            self.buffer.iter_at_offset(start_off),
            self.buffer.iter_at_offset(end_off),
        ))
    }
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

/// Character offsets of `needle` in `hay`, ignoring ASCII case. `from_char`
/// is where the scan starts. A forward scan begins there. A backward scan
/// begins on the character before it.
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
        assert!(!ed.view.is_editable());
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
            "gtk plain shape={label} chars={chars} lines={} open_ms={open_ms:.1} max_iter_ms={max_iter_ms:.1} shown={shown}",
            ed.buffer.line_count()
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
        assert_eq!(ed.view.wrap_mode(), gtk::WrapMode::None);
        assert!(!ed.display_breaks.get());
        let start_x = ed.view.iter_location(&ed.buffer.start_iter()).x();
        let mut last = ed.buffer.end_iter();
        assert!(last.backward_char(), "{label} was empty");
        let end_x = ed.view.iter_location(&last).x();
        assert!(
            end_x > start_x,
            "{label} did not lay the line out, start {start_x} end {end_x}"
        );
        let adj = ed.scrolled.hadjustment();
        assert!(
            adj.upper() > adj.page_size() + 1.0,
            "{label} has nothing to scroll, upper {} page {}",
            adj.upper(),
            adj.page_size()
        );
        assert_eq!(ed.plain_text().chars().count(), chars);
        assert_eq!(ed.text(), *text);
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

    /// A plain unspaced line scrolls sideways. Word wrap of display breaks
    /// laid the paragraph out inside `place_cursor`. The file text does not
    /// gain break characters, and find matches the run.
    pub(crate) fn a_plain_unspaced_line_scrolls_sideways() {
        let ed = Editor::new(true);
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&ed.root));
        window.present();
        let chars = 10_000usize;
        let text = one_line(chars);
        let started = std::time::Instant::now();
        ed.load(text);
        let open_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert!(open_ms < 100.0, "open blocked {open_ms:.1} ms");
        assert!(ed.is_too_formatted());
        assert!(ed.held_for_length());
        assert_eq!(ed.view.wrap_mode(), gtk::WrapMode::None);
        assert!(!ed.display_breaks.get());
        assert_eq!(ed.scrolled.hscrollbar_policy(), gtk::PolicyType::Automatic);
        assert_eq!(ed.plain_text().chars().count(), chars);
        assert!(!ed.text().plain_text().contains(rich::DISPLAY_BREAK));
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
        let start_x = ed.view.iter_location(&ed.buffer.start_iter()).x();
        let mut last = ed.buffer.end_iter();
        assert!(last.backward_char());
        let end_x = ed.view.iter_location(&last).x();
        assert!(
            end_x > start_x,
            "the line did not extend, start {start_x} end {end_x}"
        );
        let adj = ed.scrolled.hadjustment();
        assert!(
            adj.upper() > adj.page_size() + 1.0,
            "nothing to scroll, upper {} page {}",
            adj.upper(),
            adj.page_size()
        );
        adj.set_value(adj.upper() - adj.page_size());
        for _ in 0..10 {
            if !context.iteration(false) && !context.pending() {
                break;
            }
        }
        let visible = ed.view.visible_rect();
        let end_loc = ed.view.iter_location(&last);
        assert!(
            end_loc.x() < visible.x() + visible.width(),
            "scrolling did not reach the tail, end {} visible {}+{}",
            end_loc.x(),
            visible.x(),
            visible.width()
        );
        ed.search_entry.set_text(&"a".repeat(100));
        ed.find(true);
        let (start, end) = ed
            .buffer
            .selection_bounds()
            .expect("find missed a run that crosses a display break");
        let selected = ed.buffer.text(&start, &end, false);
        assert_eq!(
            rich::strip_display_breaks(selected.as_str()),
            "a".repeat(100)
        );
        window.close();
    }

    /// Typing or pasting past the unspaced cap on an editable document used
    /// to leave word-char wrap in place. The insert is refused, so the next
    /// main-loop turn does not lay out that line.
    pub(crate) fn an_edit_past_the_unspaced_cap_is_refused() {
        let ed = Editor::new(true);
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&ed.root));
        window.present();
        let body = "a".repeat(rich::SYNC_BREAKLESS_CHARS);
        ed.load(RichText::from_plain(&body));
        assert!(!rich::needs_plain_view(&RichText::from_plain(&body)));
        assert!(!ed.is_too_formatted());
        assert!(ed.allows_edits());
        assert_eq!(ed.view.wrap_mode(), gtk::WrapMode::WordChar);
        let context = gtk::glib::MainContext::default();
        let mut shown = false;
        for _ in 0..40 {
            let ran = context.iteration(false);
            if ed.view.iter_location(&ed.buffer.start_iter()).height() > 0 {
                shown = true;
            }
            if shown && !ran && !context.pending() {
                break;
            }
        }
        assert!(shown, "the line under the cap was not allocated");
        let hit = Rc::new(Cell::new(false));
        let flag = hit.clone();
        ed.connect_line_limit(move || flag.set(true));
        let mut end = ed.buffer.end_iter();
        ed.buffer.insert(&mut end, "a");
        assert!(hit.get(), "the length limit was silent");
        assert_eq!(ed.plain_text().chars().count(), rich::SYNC_BREAKLESS_CHARS);
        assert_eq!(ed.view.wrap_mode(), gtk::WrapMode::WordChar);
        let mut end = ed.buffer.end_iter();
        let started = std::time::Instant::now();
        ed.buffer.insert(&mut end, &"b".repeat(20_000));
        let paste_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert!(
            paste_ms < 100.0,
            "refusing the paste blocked for {paste_ms:.1} ms"
        );
        assert_eq!(ed.plain_text().chars().count(), rich::SYNC_BREAKLESS_CHARS);
        assert!(!ed.plain_text().contains('b'));
        let mut max_iter = 0.0f64;
        for _ in 0..20 {
            let step = std::time::Instant::now();
            if !context.iteration(false) && !context.pending() {
                break;
            }
            max_iter = max_iter.max(step.elapsed().as_secs_f64() * 1000.0);
        }
        assert!(
            max_iter < 100.0,
            "main-loop turn blocked {max_iter:.1} ms after a refused paste"
        );
        let mut end = ed.buffer.end_iter();
        ed.buffer.insert(&mut end, " hello");
        assert!(
            ed.plain_text().ends_with(" hello"),
            "a spaced word was refused: {}",
            ed.plain_text()
        );
        window.close();
    }

    /// Open times for one paragraph on a visible view. Uniform runs,
    /// alternating runs, and one unspaced line, at 10k, 50k, and 100k.
    /// Set OMAQUILL_GTK_OPEN to one of those counts to time only that size.
    /// Set it to `probe` to time word-char layout under the cap. The
    /// paragraph stays one plain line, and the line scrolls sideways.
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

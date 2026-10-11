//! Spelling marks on the manuscript editor.
//!
//! The marks are a `spell` text tag. `rich::save` only reads tags named
//! `c:` and `p:`, so the tag never becomes part of the `.scriv` file.
//! Words the author adds, and words they ignore, are files under omaquill's
//! state directory.

use super::editor::Editor;
use gtk::glib;
use gtk::prelude::*;
use gtk::{gdk, pango};
use omaquill::enchant::Broker;
use omaquill::spell::{self, Policy, Touch};
use omaquill::state::{self, Settings};
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

const TAG: &str = "spell";

thread_local! {
    static SQUIGGLE: RefCell<Option<gdk::RGBA>> = const { RefCell::new(None) };
    static TAGS: RefCell<Vec<gtk::TextTag>> = const { RefCell::new(Vec::new()) };
}

pub fn set_squiggle_color(hex: Option<&str>) {
    let rgba = hex.and_then(|h| gdk::RGBA::parse(h).ok());
    SQUIGGLE.with(|c| *c.borrow_mut() = rgba);
    if let Some(rgba) = rgba {
        TAGS.with(|tags| {
            for tag in tags.borrow().iter() {
                tag.set_underline_rgba(Some(&rgba));
            }
        });
    }
}

pub struct Session {
    editor: Rc<Editor>,
    policy: RefCell<Policy>,
    language: RefCell<String>,
    broker: RefCell<Option<Broker>>,
    dictionary: RefCell<BTreeSet<String>>,
    ignored: RefCell<BTreeSet<String>>,
    ignore_path: RefCell<Option<PathBuf>>,
    popover: gtk::Popover,
    rows: gtk::Box,
    word_start: Cell<i32>,
    word_end: Cell<i32>,
    line: Cell<i32>,
    index: Cell<i32>,
    idle: RefCell<Option<glib::SourceId>>,
    me: RefCell<std::rc::Weak<Session>>,
}

impl Session {
    pub fn new(editor: &Rc<Editor>, settings: &Settings) -> Rc<Session> {
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
        rows.add_css_class("menu");
        let popover = gtk::Popover::new();
        popover.set_child(Some(&rows));
        popover.set_position(gtk::PositionType::Bottom);
        let tag = spell::language_tag(&settings.spell_language);
        let session = Rc::new(Session {
            editor: editor.clone(),
            policy: RefCell::new(Policy {
                enabled: settings.spellcheck,
                composing: false,
                show_in_compose: settings.spell_in_compose,
                hide_word_at_cursor: settings.spell_hide_current,
            }),
            broker: RefCell::new(Broker::open(&tag)),
            language: RefCell::new(tag),
            dictionary: RefCell::new(state::load_words(&state::dictionary_path())),
            ignored: RefCell::new(BTreeSet::new()),
            ignore_path: RefCell::new(None),
            popover,
            rows,
            word_start: Cell::new(0),
            word_end: Cell::new(0),
            line: Cell::new(0),
            index: Cell::new(0),
            idle: RefCell::new(None),
            me: RefCell::new(std::rc::Weak::new()),
        });
        *session.me.borrow_mut() = Rc::downgrade(&session);
        session.wire_click();
        session.wire_history();
        session
    }

    pub fn has_dictionary(&self) -> bool {
        self.broker.borrow().is_some()
    }

    pub fn set_project(&self, project: &Path) {
        let path = state::ignore_path(project);
        *self.ignored.borrow_mut() = state::load_words(&path);
        *self.ignore_path.borrow_mut() = Some(path);
        self.rescan();
    }

    pub fn set_enabled(&self, on: bool) {
        self.policy.borrow_mut().enabled = on;
        self.rescan();
    }

    pub fn set_composing(&self, on: bool) {
        self.policy.borrow_mut().composing = on;
        self.rescan();
    }

    pub fn apply_settings(&self, settings: &Settings, composing: bool) {
        *self.policy.borrow_mut() = Policy {
            enabled: settings.spellcheck,
            composing,
            show_in_compose: settings.spell_in_compose,
            hide_word_at_cursor: settings.spell_hide_current,
        };
        let tag = spell::language_tag(&settings.spell_language);
        if tag != *self.language.borrow() {
            *self.language.borrow_mut() = tag.clone();
            *self.broker.borrow_mut() = Broker::open(&tag);
        }
        self.rescan();
    }

    pub fn refresh_cursor(&self) {
        let policy = *self.policy.borrow();
        if !spell::shown(&policy) {
            return;
        }
        let Some((origin, text, cursor)) = self.window_around_cursor() else {
            return;
        };
        let mut known = |w: &str| self.known(w);
        let touches = spell::keystroke_marks(&text, cursor, &policy, &mut known);
        self.paint_touches(&origin, &text, &touches);
    }

    pub fn rescan(&self) {
        self.stop();
        if !spell::shown(&self.policy.borrow()) {
            self.clear_all();
            return;
        }
        self.line.set(0);
        self.index.set(0);
        self.arm();
    }

    fn wire_history(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.editor.buffer.connect_undo(move |_| {
            if let Some(session) = weak.upgrade() {
                session.rescan();
            }
        });
        let weak = Rc::downgrade(self);
        self.editor.buffer.connect_redo(move |_| {
            if let Some(session) = weak.upgrade() {
                session.rescan();
            }
        });
    }

    fn wire_click(self: &Rc<Self>) {
        let click = gtk::GestureClick::builder().button(3).build();
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |gesture, _, x, y| {
            let Some(session) = weak.upgrade() else {
                return;
            };
            if session.popup(x, y) {
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        });
        self.editor.view.add_controller(click);
    }

    fn popup(&self, x: f64, y: f64) -> bool {
        let mut policy = *self.policy.borrow();
        policy.hide_word_at_cursor = false;
        if !spell::shown(&policy) || !self.ensure_menu() {
            return false;
        }
        let (bx, by) = self.editor.view.window_to_buffer_coords(
            gtk::TextWindowType::Widget,
            widget_coord(x),
            widget_coord(y),
        );
        let Some(click) = self.editor.view.iter_at_location(bx, by) else {
            return false;
        };
        let mut origin = click;
        origin.backward_chars(4_200);
        let mut end = click;
        end.forward_chars(80);
        let text = self.editor.buffer.text(&origin, &end, false).to_string();
        let at = byte_between(&text, click.offset() - origin.offset());
        let at = if spell::is_word_char(click.char()) {
            let ch = click.char();
            at.saturating_add(ch.len_utf8()).min(text.len())
        } else {
            at
        };
        let mut known = |w: &str| self.known(w);
        let touches = spell::keystroke_marks(&text, at, &policy, &mut known);
        let Some(touch) = touches.into_iter().find(|t| t.start < at && at <= t.end) else {
            return false;
        };
        let Some(bad) = touch
            .bad
            .iter()
            .find(|r| r.start < at && at <= r.end)
            .or(touch.bad.first())
        else {
            return false;
        };
        let word = text[bad.clone()].to_string();
        let form = spell::dictionary_form(&word);
        let suggestions = self
            .broker
            .borrow()
            .as_ref()
            .map(|b| b.suggest(&form))
            .unwrap_or_default();
        self.word_start.set(
            origin
                .offset()
                .saturating_add(chars_before(&text, bad.start)),
        );
        self.word_end
            .set(origin.offset().saturating_add(chars_before(&text, bad.end)));
        self.fill_menu(&word, &suggestions);
        self.popover.set_pointing_to(Some(&gdk::Rectangle::new(
            widget_coord(x),
            widget_coord(y),
            1,
            1,
        )));
        self.popover.popup();
        true
    }

    fn fill_menu(&self, word: &str, suggestions: &[String]) {
        while let Some(child) = self.rows.first_child() {
            self.rows.remove(&child);
        }
        if suggestions.is_empty() {
            self.rows.append(
                &gtk::Label::builder()
                    .label("No suggestions")
                    .css_classes(["dim-label"])
                    .margin_top(6)
                    .margin_bottom(6)
                    .margin_start(12)
                    .margin_end(12)
                    .build(),
            );
        } else {
            for suggestion in suggestions.iter().take(8) {
                let button = gtk::Button::builder()
                    .label(suggestion)
                    .has_frame(false)
                    .build();
                let replacement = suggestion.clone();
                let weak_editor = Rc::downgrade(&self.editor);
                let start = self.word_start.get();
                let end = self.word_end.get();
                let pop = self.popover.clone();
                button.connect_clicked(move |_| {
                    if let Some(editor) = weak_editor.upgrade() {
                        replace_word(&editor, start, end, &replacement);
                    }
                    pop.popdown();
                });
                self.rows.append(&button);
            }
        }
        self.rows
            .append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let add = gtk::Button::builder()
            .label("Add to Dictionary")
            .has_frame(false)
            .build();
        let ignore = gtk::Button::builder()
            .label("Ignore")
            .has_frame(false)
            .build();
        let weak = self.me.borrow().clone();
        let added = word.to_string();
        let pop = self.popover.clone();
        add.connect_clicked(move |_| {
            if let Some(session) = weak.upgrade() {
                session.remember(&added, true);
                session.rescan();
            }
            pop.popdown();
        });
        let weak = self.me.borrow().clone();
        let ignored = word.to_string();
        let pop = self.popover.clone();
        ignore.connect_clicked(move |_| {
            if let Some(session) = weak.upgrade() {
                session.remember(&ignored, false);
                session.rescan();
            }
            pop.popdown();
        });
        self.rows.append(&add);
        self.rows.append(&ignore);
    }

    /// A popover parented before the text view has a window makes GTK 4.14
    /// report that the popover is not a child, and layout then repeats that
    /// report. The menu is attached on the first click that can show it.
    fn ensure_menu(&self) -> bool {
        if self.popover.parent().is_some() {
            return true;
        }
        if self.editor.view.root().is_none() {
            return false;
        }
        self.popover.set_parent(&self.editor.view);
        true
    }

    fn known(&self, word: &str) -> bool {
        let key = word.to_lowercase();
        if self.dictionary.borrow().contains(&key) || self.ignored.borrow().contains(&key) {
            return true;
        }
        match self.broker.borrow().as_ref() {
            Some(broker) => broker.check(word),
            None => true,
        }
    }

    fn window_around_cursor(&self) -> Option<(gtk::TextIter, String, usize)> {
        let buffer = &self.editor.buffer;
        let cursor = buffer.iter_at_mark(&buffer.get_insert());
        let mut origin = cursor;
        origin.backward_chars(4_200);
        let mut end = cursor;
        end.forward_chars(80);
        let text = buffer.text(&origin, &end, false).to_string();
        let at = byte_between(&text, cursor.offset() - origin.offset());
        Some((origin, text, at))
    }

    fn paint_touches(&self, origin: &gtk::TextIter, text: &str, touches: &[Touch]) {
        let tag = self.tag();
        let n = self.editor.buffer.tag_table().size();
        if n > 0 {
            tag.set_priority(n - 1);
        }
        for touch in touches {
            let start = iter_at(origin, text, touch.start);
            let end = iter_at(origin, text, touch.end);
            self.editor.buffer.remove_tag(&tag, &start, &end);
            for bad in &touch.bad {
                let start = iter_at(origin, text, bad.start);
                let end = iter_at(origin, text, bad.end);
                self.editor.buffer.apply_tag(&tag, &start, &end);
            }
        }
    }

    fn tag(&self) -> gtk::TextTag {
        let buffer = &self.editor.buffer;
        if let Some(tag) = buffer.tag_table().lookup(TAG) {
            return tag;
        }
        let tag = gtk::TextTag::new(Some(TAG));
        tag.set_underline(pango::Underline::Error);
        SQUIGGLE.with(|c| {
            if let Some(rgba) = c.borrow().as_ref() {
                tag.set_underline_rgba(Some(rgba));
            }
        });
        buffer.tag_table().add(&tag);
        let n = buffer.tag_table().size();
        if n > 0 {
            tag.set_priority(n - 1);
        }
        TAGS.with(|tags| tags.borrow_mut().push(tag.clone()));
        tag
    }

    fn clear_all(&self) {
        let tag = self.tag();
        let (start, end) = self.editor.buffer.bounds();
        self.editor.buffer.remove_tag(&tag, &start, &end);
    }

    fn stop(&self) {
        if let Some(id) = self.idle.borrow_mut().take() {
            id.remove();
        }
    }

    fn arm(&self) {
        if self.idle.borrow().is_some() {
            return;
        }
        let weak = self.me.borrow().clone();
        let id = glib::idle_add_local(move || {
            let Some(session) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            session.pump()
        });
        *self.idle.borrow_mut() = Some(id);
    }

    fn pump(&self) -> glib::ControlFlow {
        if !spell::shown(&self.policy.borrow()) {
            self.idle.borrow_mut().take();
            return glib::ControlFlow::Break;
        }
        let buffer = &self.editor.buffer;
        let line = self.line.get();
        if line >= buffer.line_count() {
            self.idle.borrow_mut().take();
            return glib::ControlFlow::Break;
        }
        let Some(line_start) = buffer.iter_at_line(line) else {
            self.line.set(line + 1);
            self.index.set(0);
            return glib::ControlFlow::Continue;
        };
        let mut line_end = line_start;
        if !line_end.ends_line() {
            line_end.forward_to_line_end();
        }
        let chars = line_end.offset() - line_start.offset();
        let index = self.index.get().clamp(0, chars.max(0));
        if chars <= 2_000 {
            self.paint_slice(&line_start, &line_end);
            self.line.set(line + 1);
            self.index.set(0);
            return glib::ControlFlow::Continue;
        }
        if index >= chars {
            self.line.set(line + 1);
            self.index.set(0);
            return glib::ControlFlow::Continue;
        }
        let text = buffer.text(&line_start, &line_end, false).to_string();
        let Some(scan) = spell::next_scan(&text, byte_between(&text, index), 1_500) else {
            self.line.set(line + 1);
            self.index.set(0);
            return glib::ControlFlow::Continue;
        };
        if scan.end > scan.start {
            let start = iter_at(&line_start, &text, scan.start);
            let end = iter_at(&line_start, &text, scan.end);
            self.paint_slice(&start, &end);
        }
        if scan.resume > scan.end {
            let start = iter_at(&line_start, &text, scan.end);
            let end = iter_at(&line_start, &text, scan.resume);
            buffer.remove_tag(&self.tag(), &start, &end);
        }
        let next = chars_before(&text, scan.resume);
        if next >= chars {
            self.line.set(line + 1);
            self.index.set(0);
        } else {
            self.index.set(next);
        }
        glib::ControlFlow::Continue
    }

    fn paint_slice(&self, start: &gtk::TextIter, end: &gtk::TextIter) {
        let policy = *self.policy.borrow();
        if !spell::shown(&policy) {
            return;
        }
        let text = self.editor.buffer.text(start, end, false).to_string();
        let cursor = self.cursor_byte(start, end, &text);
        let mut known = |w: &str| self.known(w);
        let bad = spell::bad_ranges(&text, 0, text.len(), cursor, &policy, &mut known);
        let tag = self.tag();
        self.editor.buffer.remove_tag(&tag, start, end);
        for range in bad {
            let a = iter_at(start, &text, range.start);
            let b = iter_at(start, &text, range.end);
            self.editor.buffer.apply_tag(&tag, &a, &b);
        }
    }

    fn cursor_byte(&self, start: &gtk::TextIter, end: &gtk::TextIter, text: &str) -> Option<usize> {
        let cursor = self
            .editor
            .buffer
            .iter_at_mark(&self.editor.buffer.get_insert());
        if cursor < *start || cursor > *end {
            return None;
        }
        let delta = cursor.offset() - start.offset();
        if delta < 0 {
            return None;
        }
        Some(byte_between(text, delta))
    }

    fn remember(&self, word: &str, dictionary: bool) {
        let key = spell::dictionary_form(word).to_lowercase();
        if key.is_empty() {
            return;
        }
        if dictionary {
            self.dictionary.borrow_mut().insert(key);
            let words = self.dictionary.borrow().clone();
            let _ = state::save_words(&state::dictionary_path(), &words);
        } else {
            self.ignored.borrow_mut().insert(key);
            if let Some(path) = self.ignore_path.borrow().clone() {
                let words = self.ignored.borrow().clone();
                let _ = state::save_words(&path, &words);
            }
        }
    }
}

fn replace_word(editor: &Editor, start: i32, end: i32, word: &str) {
    let buffer = &editor.buffer;
    let mut s = buffer.iter_at_offset(start);
    let mut e = buffer.iter_at_offset(end);
    buffer.begin_user_action();
    buffer.delete(&mut s, &mut e);
    buffer.insert(&mut s, word);
    buffer.end_user_action();
}

fn iter_at(origin: &gtk::TextIter, text: &str, byte: usize) -> gtk::TextIter {
    let mut iter = *origin;
    let chars = chars_before(text, byte);
    iter.forward_chars(chars);
    iter
}

fn chars_before(text: &str, byte: usize) -> i32 {
    let mut byte = byte.min(text.len());
    while byte > 0 && !text.is_char_boundary(byte) {
        byte -= 1;
    }
    i32::try_from(text[..byte].chars().count()).unwrap_or_default()
}

fn byte_between(text: &str, chars: i32) -> usize {
    if chars <= 0 {
        return 0;
    }
    let Ok(chars) = usize::try_from(chars) else {
        return text.len();
    };
    text.chars().take(chars).map(char::len_utf8).sum()
}

fn widget_coord(n: f64) -> i32 {
    let n = n.round();
    if !n.is_finite() || n < f64::from(i32::MIN) || n > f64::from(i32::MAX) {
        return 0;
    }
    n as i32
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn keystroke_on_a_long_buffer() {
        let editor = Editor::new(true);
        let mut text = String::with_capacity(100_000 * 6 + 8);
        for n in 0..100_000 {
            if n > 0 && n % 80 == 0 {
                text.push('\n');
            }
            text.push_str("alpha ");
        }
        text.push_str("xyzzy ");
        editor.buffer.set_text(&text);
        let session = Session::new(&editor, &Settings::default());
        if !session.has_dictionary() {
            eprintln!("no spelling dictionary; long-buffer check skipped");
            return;
        }
        let end = editor.buffer.end_iter();
        editor.buffer.place_cursor(&end);
        session.refresh_cursor();
        let mut times = Vec::new();
        for _ in 0..21 {
            let start = std::time::Instant::now();
            session.refresh_cursor();
            times.push(start.elapsed());
        }
        times.sort();
        eprintln!(
            "editor keystroke spell median {:?} min {:?} max {:?}",
            times[10], times[0], times[20]
        );
        let (start, _) = end
            .backward_search("xyzzy", gtk::TextSearchFlags::TEXT_ONLY, None)
            .expect("planted word");
        let marked = start
            .tags()
            .iter()
            .any(|tag| tag.name().as_deref() == Some(TAG));
        assert!(
            marked,
            "xyzzy should carry the spelling mark once the cursor has left it"
        );
        assert!(
            times[10].as_millis() < 16,
            "median keystroke {:?} on a 100k-word buffer",
            times[10]
        );
    }

    fn marked_words(buffer: &gtk::TextBuffer) -> Vec<String> {
        let Some(tag) = buffer.tag_table().lookup(TAG) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut iter = buffer.start_iter();
        loop {
            if !iter.starts_tag(Some(&tag)) && !iter.forward_to_tag_toggle(Some(&tag)) {
                break;
            }
            if !iter.starts_tag(Some(&tag)) {
                continue;
            }
            let start = iter;
            if !iter.forward_to_tag_toggle(Some(&tag)) {
                break;
            }
            out.push(buffer.text(&start, &iter, false).to_string());
        }
        out
    }

    fn drain(session: &Session) {
        while let glib::ControlFlow::Continue = session.pump() {}
    }

    /// The caret at the first letter is still in the word, so the mark waits.
    pub(crate) fn the_caret_hides_the_word_it_is_in() {
        let editor = Editor::new(true);
        let session = Session::new(&editor, &Settings::default());
        if !session.has_dictionary() {
            eprintln!("no spelling dictionary; caret check skipped");
            return;
        }
        editor.buffer.set_text("helo storm");
        editor.buffer.place_cursor(&editor.buffer.start_iter());
        session.rescan();
        drain(&session);
        assert_eq!(
            marked_words(&editor.buffer),
            Vec::<String>::new(),
            "the word under the caret stays clear"
        );
        editor.buffer.place_cursor(&editor.buffer.iter_at_offset(5));
        session.refresh_cursor();
        assert_eq!(marked_words(&editor.buffer), vec!["helo".to_string()]);
    }

    /// A line the idle walk splits must not spellcheck the tail of a token
    /// that is longer than the lookup limit.
    pub(crate) fn a_long_line_does_not_mark_the_tail_of_an_overlong_token() {
        let editor = Editor::new(true);
        let session = Session::new(&editor, &Settings::default());
        if !session.has_dictionary() {
            eprintln!("no spelling dictionary; long-line check skipped");
            return;
        }
        let mut text = "helo ".repeat(280);
        text.push_str(&"q".repeat(200));
        text.push_str(&" helo".repeat(200));
        editor.buffer.set_text(&text);
        editor.buffer.place_cursor(&editor.buffer.start_iter());
        session.rescan();
        drain(&session);
        let marks = marked_words(&editor.buffer);
        assert!(
            marks.iter().all(|word| !word.contains('q')),
            "overlong token was marked: {marks:?}"
        );
        assert!(
            marks.iter().any(|word| word == "helo"),
            "a real misspelling on the long line was missed"
        );
    }

    /// Undo puts the previous letters back. The mark has to match those
    /// letters, because a tag applied after the edit is not an undo entry.
    pub(crate) fn undo_drops_the_mark_when_the_word_is_whole_again() {
        let editor = Editor::new(true);
        let session = Session::new(
            &editor,
            &Settings {
                spell_hide_current: false,
                ..Settings::default()
            },
        );
        if !session.has_dictionary() {
            eprintln!("no spelling dictionary; undo check skipped");
            return;
        }
        let buffer = &editor.buffer;
        buffer.set_text("hello storm");
        buffer.place_cursor(&buffer.iter_at_offset(3));
        buffer.begin_user_action();
        let mut start = buffer.iter_at_offset(2);
        let mut end = buffer.iter_at_offset(3);
        buffer.delete(&mut start, &mut end);
        buffer.end_user_action();
        session.rescan();
        drain(&session);
        assert_eq!(
            buffer.text(&buffer.start_iter(), &buffer.end_iter(), false),
            "helo storm"
        );
        assert_eq!(marked_words(buffer), vec!["helo".to_string()]);
        buffer.undo();
        drain(&session);
        assert_eq!(
            buffer.text(&buffer.start_iter(), &buffer.end_iter(), false),
            "hello storm"
        );
        assert_eq!(
            marked_words(buffer),
            Vec::<String>::new(),
            "undo of the deleted letter clears the squiggle"
        );
        buffer.redo();
        drain(&session);
        assert_eq!(
            buffer.text(&buffer.start_iter(), &buffer.end_iter(), false),
            "helo storm"
        );
        assert_eq!(marked_words(buffer), vec!["helo".to_string()]);
    }

    pub(crate) fn the_squiggle_uses_the_theme_red() {
        set_squiggle_color(Some("#c45c26"));
        let editor = Editor::new(true);
        let session = Session::new(&editor, &Settings::default());
        editor.buffer.set_text("helo");
        session.rescan();
        drain(&session);
        let tag = editor
            .buffer
            .tag_table()
            .lookup(TAG)
            .expect("rescan creates the spell tag");
        let rgba = tag.underline_rgba().expect("theme red is on the tag");
        assert!((rgba.red() - 196.0 / 255.0).abs() < 0.004);
        assert!((rgba.green() - 92.0 / 255.0).abs() < 0.004);
        assert!((rgba.blue() - 38.0 / 255.0).abs() < 0.004);
        set_squiggle_color(Some("#112233"));
        let rgba = tag.underline_rgba().expect("a later theme red replaces it");
        assert!((rgba.red() - 17.0 / 255.0).abs() < 0.004);
        assert!((rgba.green() - 34.0 / 255.0).abs() < 0.004);
        assert!((rgba.blue() - 51.0 / 255.0).abs() < 0.004);
    }
}

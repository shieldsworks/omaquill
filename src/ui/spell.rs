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
    line: Cell<i32>,
    index: Cell<i32>,
    /// Byte offset of `index` in the current line. Idle slices advance it
    /// instead of recounting the line prefix.
    byte: Cell<usize>,
    /// Line-relative character offset of `scan_mark`, so the next slice
    /// walks forward from there.
    scan_at: Cell<i32>,
    scan_mark: RefCell<Option<gtk::TextMark>>,
    idle: RefCell<Option<glib::SourceId>>,
    me: RefCell<std::rc::Weak<Session>>,
    /// Buffer offsets of the word the caret was in, so leaving it can mark it.
    left: Cell<Option<(i32, i32)>>,
    chars: Cell<i32>,
}

impl Session {
    pub fn new(editor: &Rc<Editor>, settings: &Settings) -> Rc<Session> {
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
            line: Cell::new(0),
            index: Cell::new(0),
            byte: Cell::new(0),
            scan_at: Cell::new(0),
            scan_mark: RefCell::new(None),
            idle: RefCell::new(None),
            me: RefCell::new(std::rc::Weak::new()),
            left: Cell::new(None),
            chars: Cell::new(0),
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
        let next = Policy {
            enabled: settings.spellcheck,
            composing,
            show_in_compose: settings.spell_in_compose,
            hide_word_at_cursor: settings.spell_hide_current,
        };
        let tag = spell::language_tag(&settings.spell_language);
        if next == *self.policy.borrow() && tag == *self.language.borrow() {
            return;
        }
        *self.policy.borrow_mut() = next;
        if tag != *self.language.borrow() {
            *self.language.borrow_mut() = tag.clone();
            *self.broker.borrow_mut() = Broker::open(&tag);
        }
        self.rescan();
    }

    /// A paste, or any larger change, checks the whole buffer. A
    /// one-character edit on the line being scanned resumes within a word
    /// of that edit. The stored byte belonged to the previous text, so the
    /// new byte is counted from the current line.
    pub fn after_edit(&self) {
        let n = self.editor.buffer.char_count();
        let prev = self.chars.replace(n);
        let delta = n - prev;
        if delta.abs() > 1 {
            self.left.set(None);
            self.rescan();
            return;
        }
        let buffer = &self.editor.buffer;
        let cursor = buffer.iter_at_mark(&buffer.get_insert());
        if cursor.line() == self.line.get() {
            let index = spell::line_anchor_after_edit(cursor.line_offset(), delta);
            self.index.set(index);
            let mut start = cursor;
            start.set_line_offset(0);
            let mut end = start;
            if !end.ends_line() {
                end.forward_to_line_end();
            }
            let line = buffer.text(&start, &end, false);
            let at = usize::try_from(index.max(0)).unwrap_or(0);
            self.byte.set(spell::byte_of_chars(line.as_str(), at));
            self.clear_scan_mark();
        }
        self.refresh_cursor();
    }

    pub fn refresh_cursor(&self) {
        let policy = *self.policy.borrow();
        if !spell::shown(&policy) {
            return;
        }
        let buffer = &self.editor.buffer;
        let cursor = buffer.iter_at_mark(&buffer.get_insert()).offset();
        if let Some((start, end)) = self.left.get()
            && (cursor < start || cursor > end)
        {
            self.paint_slice(&buffer.iter_at_offset(start), &buffer.iter_at_offset(end));
        }
        let Some((origin, text, at)) = self.window_around_cursor() else {
            return;
        };
        let mut known = |w: &str| self.known(w);
        let touches = spell::keystroke_marks(&text, at, &policy, &mut known);
        self.paint_touches(&origin, &text, &touches);
        self.remember_cursor_word(&origin, &text, at);
    }

    pub fn rescan(&self) {
        self.stop();
        self.clear_scan_mark();
        self.left.set(None);
        self.chars.set(self.editor.buffer.char_count());
        if !spell::shown(&self.policy.borrow()) {
            self.clear_all();
            return;
        }
        self.line.set(0);
        self.index.set(0);
        self.byte.set(0);
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
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |_, _, x, y| {
            let Some(session) = weak.upgrade() else {
                return;
            };
            session.prepare_click(x, y);
        });
        self.editor.view.add_controller(click);
    }

    fn prepare_click(&self, x: f64, y: f64) {
        let (bx, by) = self.editor.view.window_to_buffer_coords(
            gtk::TextWindowType::Widget,
            widget_coord(x),
            widget_coord(y),
        );
        let Some(click) = self.editor.view.iter_at_location(bx, by) else {
            self.editor.view.set_extra_menu(None::<&gtk::gio::Menu>);
            return;
        };
        self.prepare_at(&click);
    }

    /// Suggestions, Add, and Ignore are items on the text view's own menu.
    /// A second popover would sit under that menu.
    fn prepare_at(&self, click: &gtk::TextIter) -> bool {
        let mut policy = *self.policy.borrow();
        policy.hide_word_at_cursor = false;
        if !spell::shown(&policy) {
            self.editor.view.set_extra_menu(None::<&gtk::gio::Menu>);
            return false;
        }
        let mut origin = *click;
        origin.backward_chars(4_200);
        let mut end = *click;
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
            self.editor.view.set_extra_menu(None::<&gtk::gio::Menu>);
            return false;
        };
        let Some(bad) = touch
            .bad
            .iter()
            .find(|r| r.start < at && at <= r.end)
            .or(touch.bad.first())
        else {
            self.editor.view.set_extra_menu(None::<&gtk::gio::Menu>);
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
        let start = origin
            .offset()
            .saturating_add(chars_before(&text, bad.start));
        let end = origin.offset().saturating_add(chars_before(&text, bad.end));
        self.install_menu(&word, &suggestions, start, end);
        true
    }

    fn install_menu(&self, word: &str, suggestions: &[String], start: i32, end: i32) {
        let menu = gtk::gio::Menu::new();
        let group = gtk::gio::SimpleActionGroup::new();
        for (i, suggestion) in suggestions.iter().take(8).enumerate() {
            let name = format!("s{i}");
            menu.append(Some(suggestion), Some(&format!("spell.{name}")));
            let action = gtk::gio::SimpleAction::new(&name, None);
            let replacement = suggestion.clone();
            let weak_editor = Rc::downgrade(&self.editor);
            action.connect_activate(move |_, _| {
                if let Some(editor) = weak_editor.upgrade() {
                    replace_word(&editor, start, end, &replacement);
                }
            });
            group.add_action(&action);
        }
        menu.append(Some("Add to Dictionary"), Some("spell.add"));
        menu.append(Some("Ignore"), Some("spell.ignore"));
        let add = gtk::gio::SimpleAction::new("add", None);
        let ignore = gtk::gio::SimpleAction::new("ignore", None);
        let weak = self.me.borrow().clone();
        let added = word.to_string();
        add.connect_activate(move |_, _| {
            if let Some(session) = weak.upgrade() {
                session.remember(&added, true);
                session.rescan();
            }
        });
        let weak = self.me.borrow().clone();
        let ignored = word.to_string();
        ignore.connect_activate(move |_, _| {
            if let Some(session) = weak.upgrade() {
                session.remember(&ignored, false);
                session.rescan();
            }
        });
        group.add_action(&add);
        group.add_action(&ignore);
        self.editor.view.insert_action_group("spell", Some(&group));
        self.editor.view.set_extra_menu(Some(&menu));
    }

    fn remember_cursor_word(&self, origin: &gtk::TextIter, text: &str, cursor: usize) {
        let Some((start, end)) = spell::raw_word(text, cursor) else {
            self.left.set(None);
            return;
        };
        let base = origin.offset();
        self.left.set(Some((
            base.saturating_add(chars_before(text, start)),
            base.saturating_add(chars_before(text, end)),
        )));
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
            self.advance_line(line);
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
            self.advance_line(line);
            return glib::ControlFlow::Continue;
        }
        if index >= chars {
            self.advance_line(line);
            return glib::ControlFlow::Continue;
        }
        let line_chars = usize::try_from(chars).unwrap_or(0);
        let index_at = usize::try_from(index).unwrap_or(0).min(line_chars);
        let (win_start, win_end) = spell::slice_window(line_chars, index_at, 1_500);
        if win_end <= win_start {
            self.advance_line(line);
            return glib::ControlFlow::Continue;
        }
        let origin = self.iter_on_line(&line_start, win_start);
        let mut win_end_iter = origin;
        let win_chars = i32::try_from(win_end - win_start).unwrap_or(i32::MAX);
        win_end_iter.forward_chars(win_chars);
        let text = buffer.text(&origin, &win_end_iter, false).to_string();
        let local = index_at - win_start;
        let mut mark = spell::LineMark::resume(local, spell::byte_of_chars(&text, local));
        let Some(slice) = spell::take_slice(&text, &mut mark, 1_500) else {
            self.advance_line(line);
            return glib::ControlFlow::Continue;
        };
        if slice.end_byte > slice.start_byte {
            let start = iter_chars(&origin, slice.start_chars);
            let end = iter_chars(&origin, slice.end_chars);
            self.paint_slice(&start, &end);
        }
        if slice.resume_byte > slice.end_byte {
            let start = iter_chars(&origin, slice.end_chars);
            let end = iter_chars(&origin, slice.resume_chars);
            buffer.remove_tag(&self.tag(), &start, &end);
        }
        let next_chars = win_start.saturating_add(slice.resume_chars);
        let next = i32::try_from(next_chars).unwrap_or(i32::MAX);
        if next >= chars {
            self.advance_line(line);
        } else {
            self.index.set(next);
            self.byte.set(0);
        }
        glib::ControlFlow::Continue
    }

    fn advance_line(&self, line: i32) {
        self.line.set(line + 1);
        self.index.set(0);
        self.byte.set(0);
        self.clear_scan_mark();
    }

    fn clear_scan_mark(&self) {
        if let Some(mark) = self.scan_mark.borrow_mut().take() {
            self.editor.buffer.delete_mark(&mark);
        }
        self.scan_at.set(0);
    }

    /// An iterator `chars` into the current line. The mark stays on the
    /// buffer so the next slice moves by the gap, not from the line start.
    fn iter_on_line(&self, line_start: &gtk::TextIter, chars: usize) -> gtk::TextIter {
        let buffer = &self.editor.buffer;
        let target = i32::try_from(chars).unwrap_or(i32::MAX);
        if let Some(mark) = self.scan_mark.borrow().clone() {
            let mut iter = buffer.iter_at_mark(&mark);
            let delta = target - self.scan_at.get();
            if delta > 0 {
                iter.forward_chars(delta);
            } else if delta < 0 {
                iter.backward_chars(delta.saturating_neg());
            }
            buffer.move_mark(&mark, &iter);
            self.scan_at.set(iter.offset() - line_start.offset());
            return iter;
        }
        let iter = iter_chars(line_start, chars);
        let mark = buffer.create_mark(None, &iter, true);
        self.scan_at.set(target);
        *self.scan_mark.borrow_mut() = Some(mark);
        iter
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
        if let Some(at) = cursor {
            self.remember_cursor_word(start, &text, at);
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
        let key = spell::dictionary_key(word);
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
    if !editor.allows_edits() {
        return;
    }
    let buffer = &editor.buffer;
    let mut s = buffer.iter_at_offset(start);
    let mut e = buffer.iter_at_offset(end);
    buffer.begin_user_action();
    buffer.delete(&mut s, &mut e);
    buffer.insert(&mut s, word);
    buffer.end_user_action();
}

fn iter_chars(origin: &gtk::TextIter, chars: usize) -> gtk::TextIter {
    let mut iter = *origin;
    iter.forward_chars(i32::try_from(chars).unwrap_or(i32::MAX));
    iter
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
        let mut settings = Settings::default();
        settings.spell_hide_current = false;
        let session = Session::new(&editor, &settings);
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

    pub(crate) fn leaving_the_word_by_a_jump_marks_it() {
        let editor = Editor::new(true);
        let session = Session::new(&editor, &Settings::default());
        if !session.has_dictionary() {
            eprintln!("no spelling dictionary; jump check skipped");
            return;
        }
        editor.buffer.set_text("helo storm and more words");
        editor.buffer.place_cursor(&editor.buffer.start_iter());
        session.rescan();
        drain(&session);
        assert_eq!(marked_words(&editor.buffer), Vec::<String>::new());
        editor.buffer.place_cursor(&editor.buffer.end_iter());
        session.refresh_cursor();
        assert_eq!(marked_words(&editor.buffer), vec!["helo".to_string()]);
    }

    pub(crate) fn a_paste_marks_every_new_word() {
        let editor = Editor::new(true);
        let session = Session::new(&editor, &Settings::default());
        if !session.has_dictionary() {
            eprintln!("no spelling dictionary; paste check skipped");
            return;
        }
        editor.buffer.set_text("storm");
        editor.buffer.place_cursor(&editor.buffer.end_iter());
        session.rescan();
        drain(&session);
        let mut end = editor.buffer.end_iter();
        editor.buffer.insert(&mut end, " the helo wrld end");
        session.after_edit();
        drain(&session);
        let marks = marked_words(&editor.buffer);
        assert!(
            marks.iter().any(|word| word == "helo"),
            "pasted helo was not marked: {marks:?}"
        );
        assert!(
            marks.iter().any(|word| word == "wrld"),
            "pasted wrld was not marked: {marks:?}"
        );
    }

    pub(crate) fn suggestions_share_the_text_view_menu() {
        let editor = Editor::new(true);
        let session = Session::new(&editor, &Settings::default());
        if !session.has_dictionary() {
            eprintln!("no spelling dictionary; menu check skipped");
            return;
        }
        editor.buffer.set_text("helo storm");
        let inside = editor.buffer.iter_at_offset(2);
        assert!(session.prepare_at(&inside));
        let model = editor
            .view
            .extra_menu()
            .expect("suggestions are on the text view menu");
        let menu = model
            .downcast::<gtk::gio::Menu>()
            .expect("extra menu is a menu");
        let mut labels = Vec::new();
        for i in 0..menu.n_items() {
            if let Some(label) = menu
                .item_attribute_value(i, gtk::gio::MENU_ATTRIBUTE_LABEL, None)
                .and_then(|value| value.get::<String>())
            {
                labels.push(label);
            }
        }
        assert!(
            labels
                .iter()
                .any(|label| label.eq_ignore_ascii_case("hello")),
            "suggestions: {labels:?}"
        );
        assert!(labels.iter().any(|label| label == "Add to Dictionary"));
        assert!(labels.iter().any(|label| label == "Ignore"));
    }

    /// A suggestion replaces the buffer even when the view is not editable,
    /// unless the held state refuses it. `text()` would still return the
    /// source, so the buffer and the file would disagree.
    pub(crate) fn a_suggestion_does_not_change_a_held_document() {
        let mut paragraph = omaquill::rtf::Paragraph::default();
        paragraph.runs.push(omaquill::rtf::Run {
            text: format!("{} helo", "a".repeat(10_000)),
            style: omaquill::rtf::CharStyle::default(),
        });
        let text = omaquill::rtf::RichText {
            paragraphs: vec![paragraph],
            lossy: Vec::new(),
        };
        let editor = Editor::new(true);
        editor.load(&text);
        assert!(editor.is_too_formatted());
        let plain = editor.plain_text();
        let at = i32::try_from(plain.find("helo").expect("helo")).unwrap_or(0);
        replace_word(&editor, at, at + 4, "hello");
        assert_eq!(editor.plain_text(), plain);
        assert_eq!(editor.text().plain_text(), plain);
    }

    /// Spell walks `line_count`. One paragraph is one line, including when
    /// its runs would have been split after 32 style spans.
    pub(crate) fn one_paragraph_is_one_spelling_line() {
        let editor = Editor::new(true);
        let mut paragraph = omaquill::rtf::Paragraph::default();
        for i in 0..32 {
            paragraph.runs.push(omaquill::rtf::Run {
                text: "q".into(),
                style: omaquill::rtf::CharStyle {
                    bold: i % 2 == 0,
                    ..omaquill::rtf::CharStyle::default()
                },
            });
        }
        paragraph.runs.push(omaquill::rtf::Run {
            text: "zz ".into(),
            style: omaquill::rtf::CharStyle {
                italic: true,
                ..omaquill::rtf::CharStyle::default()
            },
        });
        let word = format!("{}zz", "q".repeat(32));
        editor.load(&omaquill::rtf::RichText {
            paragraphs: vec![paragraph],
            lossy: Vec::new(),
        });
        assert_eq!(
            editor.buffer.line_count(),
            1,
            "spell would check {} lines",
            editor.buffer.line_count()
        );
        editor.buffer.place_cursor(&editor.buffer.end_iter());
        let session = Session::new(&editor, &Settings::default());
        session.rescan();
        drain(&session);
        assert_eq!(session.line.get(), editor.buffer.line_count());
        assert_eq!(editor.buffer.line_count(), 1);
        if session.has_dictionary() {
            assert_eq!(marked_words(&editor.buffer), vec![word]);
        } else {
            eprintln!("no spelling dictionary; the line count still stands");
        }
    }

    pub(crate) fn the_same_spelling_settings_do_not_restart_the_scan() {
        let editor = Editor::new(true);
        let session = Session::new(&editor, &Settings::default());
        editor.buffer.set_text("helo storm");
        session.rescan();
        assert_eq!(session.line.get(), 0);
        session.pump();
        assert_eq!(session.line.get(), 1);
        session.apply_settings(&Settings::default(), false);
        assert_eq!(session.line.get(), 1);
    }

    pub(crate) fn activating_spellcheck_flips_its_state() {
        let action = gtk::gio::SimpleAction::new_stateful("spellcheck", None, &true.to_variant());
        action.connect_change_state(|action, state| {
            if let Some(state) = state {
                action.set_state(state);
            }
        });
        action.connect_activate(|action, _| {
            let on = action
                .state()
                .and_then(|state| state.get::<bool>())
                .unwrap_or(false);
            action.change_state(&(!on).to_variant());
        });
        action.activate(None);
        assert_eq!(
            action.state().and_then(|state| state.get::<bool>()),
            Some(false)
        );
    }
}

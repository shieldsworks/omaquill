//! Suggestions, Add, and Ignore live on the text view's own menu.
//! A second popover would sit under that menu.

use gtk::prelude::*;
use omaquill::spell;
use std::rc::Rc;

use super::{Session, byte_between, chars_before, replace_word, widget_coord};

impl Session {
    pub(super) fn prepare_click(&self, x: f64, y: f64) {
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
    pub(super) fn prepare_at(&self, click: &gtk::TextIter) -> bool {
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

    pub(super) fn install_menu(&self, word: &str, suggestions: &[String], start: i32, end: i32) {
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
}

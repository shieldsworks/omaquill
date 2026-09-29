//! The inspector, on the right: the selected item's title, label, status,
//! compile switch, synopsis (its index card) and notes.

use super::Win;
use super::editor::Editor;
use adw::prelude::*;
use omaquill::project::{Item, Tag};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub struct Inspector {
    pub root: gtk::ScrolledWindow,
    title: adw::EntryRow,
    label: adw::ComboRow,
    status: adw::ComboRow,
    compile: adw::SwitchRow,
    synopsis: gtk::TextView,
    label_ids: RefCell<Vec<i32>>,
    status_ids: RefCell<Vec<i32>>,
    /// Set while filling the widgets, so that isn't taken as an edit.
    filling: Cell<bool>,
    uuid: RefCell<Option<String>>,
}

fn heading(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(["heading"])
        .margin_top(12)
        .build()
}

impl Inspector {
    pub fn new(notes: &Editor) -> Inspector {
        let title = adw::EntryRow::builder()
            .title("Title")
            .show_apply_button(true)
            .build();
        let label = adw::ComboRow::builder().title("Label").build();
        let status = adw::ComboRow::builder().title("Status").build();
        let compile = adw::SwitchRow::builder()
            .title("Include in Compile")
            .build();
        let group = adw::PreferencesGroup::new();
        group.add(&title);
        group.add(&label);
        group.add(&status);
        group.add(&compile);

        let synopsis = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(8)
            .right_margin(8)
            .height_request(110)
            .build();
        let synopsis_frame = gtk::Frame::builder().child(&synopsis).build();
        synopsis_frame.add_css_class("view");

        notes.root.set_height_request(240);
        let notes_frame = gtk::Frame::builder()
            .child(&notes.root)
            .vexpand(true)
            .build();

        let col = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        col.append(&group);
        col.append(&heading("Synopsis"));
        col.append(&synopsis_frame);
        col.append(&heading("Notes"));
        col.append(&notes_frame);
        let root = gtk::ScrolledWindow::builder()
            .child(&col)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        Inspector {
            root,
            title,
            label,
            status,
            compile,
            synopsis,
            label_ids: RefCell::new(Vec::new()),
            status_ids: RefCell::new(Vec::new()),
            filling: Cell::new(false),
            uuid: RefCell::new(None),
        }
    }

    pub fn set_tags(&self, labels: &[Tag], statuses: &[Tag]) {
        self.filling.set(true);
        for (row, tags, ids) in [
            (&self.label, labels, &self.label_ids),
            (&self.status, statuses, &self.status_ids),
        ] {
            let names: Vec<&str> = tags.iter().map(|t| t.name.as_str()).collect();
            row.set_model(Some(&gtk::StringList::new(&names)));
            *ids.borrow_mut() = tags.iter().map(|t| t.id).collect();
        }
        self.filling.set(false);
    }

    pub fn set_title(&self, title: &str) {
        self.filling.set(true);
        self.title.set_text(title);
        self.filling.set(false);
    }

    /// The item the inspector (notes, synopsis) belongs to.
    pub fn uuid(&self) -> Option<String> {
        self.uuid.borrow().clone()
    }

    /// The title typed into the field for its item, if not yet applied.
    pub fn pending_title(&self) -> Option<(String, String)> {
        let uuid = self.uuid()?;
        let text = self.title.text().trim().to_string();
        (!text.is_empty()).then_some((uuid, text))
    }

    /// Detaches from any item (after its project closes or it's deleted).
    pub fn forget(&self) {
        self.filling.set(true);
        *self.uuid.borrow_mut() = None;
        self.title.set_text("");
        self.synopsis.buffer().set_text("");
        self.filling.set(false);
    }

    pub fn synopsis_text(&self) -> String {
        let b = self.synopsis.buffer();
        let (s, e) = b.bounds();
        b.text(&s, &e, false).to_string()
    }
}

impl Win {
    pub(super) fn wire_inspector(self: &Rc<Self>) {
        let i = &self.inspector;
        let weak = Rc::downgrade(self);
        i.title.connect_apply(move |row| {
            let Some(win) = weak.upgrade() else { return };
            let Some(uuid) = win.inspector.uuid.borrow().clone() else {
                return;
            };
            let text = row.text().to_string();
            win.set_title(&uuid, text.trim());
        });
        let weak = Rc::downgrade(self);
        i.label.connect_selected_notify(move |row| {
            let Some(win) = weak.upgrade() else { return };
            if win.inspector.filling.get() {
                return;
            }
            let Some(uuid) = win.inspector.uuid.borrow().clone() else {
                return;
            };
            let id = win
                .inspector
                .label_ids
                .borrow()
                .get(row.selected() as usize)
                .copied();
            match win.with_project(|p| p.set_label(&uuid, id).and_then(|_| p.save())) {
                Some(Err(e)) => win.toast(&format!("Couldn't set the label: {e}")),
                _ => {
                    win.refresh_binder(None);
                    win.refresh_board();
                }
            }
        });
        let weak = Rc::downgrade(self);
        i.status.connect_selected_notify(move |row| {
            let Some(win) = weak.upgrade() else { return };
            if win.inspector.filling.get() {
                return;
            }
            let Some(uuid) = win.inspector.uuid.borrow().clone() else {
                return;
            };
            let id = win
                .inspector
                .status_ids
                .borrow()
                .get(row.selected() as usize)
                .copied();
            if let Some(Err(e)) =
                win.with_project(|p| p.set_status(&uuid, id).and_then(|_| p.save()))
            {
                win.toast(&format!("Couldn't set the status: {e}"));
            }
            win.refresh_board();
        });
        let weak = Rc::downgrade(self);
        i.compile.connect_active_notify(move |row| {
            let Some(win) = weak.upgrade() else { return };
            if win.inspector.filling.get() {
                return;
            }
            let Some(uuid) = win.inspector.uuid.borrow().clone() else {
                return;
            };
            let on = row.is_active();
            if let Some(Err(e)) =
                win.with_project(|p| p.set_include_in_compile(&uuid, on).and_then(|_| p.save()))
            {
                win.toast(&format!("Couldn't change that: {e}"));
            }
            win.update_totals();
        });
        let weak = Rc::downgrade(self);
        i.synopsis.buffer().connect_changed(move |_| {
            let Some(win) = weak.upgrade() else { return };
            if win.inspector.filling.get() {
                return;
            }
            win.dirty_synopsis.set(true);
            win.schedule_save();
        });
    }

    pub(super) fn show_inspector(&self, item: &Item) {
        let i = &self.inspector;
        i.filling.set(true);
        *i.uuid.borrow_mut() = Some(item.uuid.clone());
        i.title.set_text(&item.title);
        let pick = |ids: &RefCell<Vec<i32>>, id: Option<i32>| {
            let ids = ids.borrow();
            let id = id.unwrap_or(-1);
            ids.iter()
                .position(|&x| x == id)
                .or_else(|| ids.iter().position(|&x| x == -1))
                .unwrap_or(0) as u32
        };
        i.label.set_selected(pick(&i.label_ids, item.label));
        i.status.set_selected(pick(&i.status_ids, item.status));
        i.compile.set_active(item.include_in_compile);
        // The root folders aren't compiled themselves.
        i.compile.set_visible(!item.kind.is_root_folder());
        let (synopsis, notes) = match self.project.borrow().as_ref() {
            Some(p) => (
                p.synopsis(&item.uuid),
                p.notes(&item.uuid).unwrap_or_default(),
            ),
            None => (String::new(), Default::default()),
        };
        i.synopsis.buffer().set_text(&synopsis);
        self.notes.load(&notes);
        self.dirty_notes.set(false);
        self.dirty_synopsis.set(false);
        i.filling.set(false);
    }
}

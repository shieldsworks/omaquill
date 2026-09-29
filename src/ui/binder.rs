//! The binder: the project's tree of folders and documents, on the left.
//!
//! Rows are `GtkStringObject`s holding UUIDs, in a `GtkTreeListModel`
//! whose children come from a map rebuilt on every change. Drag a row onto
//! another's top or bottom edge to put it before or after, onto the middle
//! to put it inside. With the binder focused, j/k move and h/l fold.

use super::{Win, dialogs};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use omaquill::project::{Item, Kind};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone)]
pub struct RowInfo {
    pub title: String,
    pub kind: Kind,
    pub label: Option<i32>,
    pub has_children: bool,
}

pub struct Binder {
    pub root: gtk::Box,
    pub list: gtk::ListView,
    pub search: gtk::SearchEntry,
    /// One right-click menu for every row (a popover per row would leak
    /// as rows are recycled).
    menu: gtk::PopoverMenu,
    store: gio::ListStore,
    tree: gtk::TreeListModel,
    selection: gtk::SingleSelection,
    children: Rc<RefCell<HashMap<String, Vec<String>>>>,
    info: Rc<RefCell<HashMap<String, RowInfo>>>,
    searching: Rc<Cell<bool>>,
    /// Set while the model is rebuilt, so selection changes don't reopen.
    refreshing: Cell<bool>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Drop {
    Before,
    Into,
    After,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Nudge {
    Up,
    Down,
    In,
    Out,
}

fn icon(kind: Kind, has_children: bool) -> &'static str {
    match kind {
        Kind::Draft => "document-edit-symbolic",
        Kind::Research => "folder-documents-symbolic",
        Kind::Trash => "user-trash-symbolic",
        Kind::Folder => "folder-symbolic",
        Kind::Image => "image-x-generic-symbolic",
        Kind::Pdf => "x-office-document-symbolic",
        Kind::Text if has_children => "view-paged-symbolic",
        _ => "text-x-generic-symbolic",
    }
}

fn uuid_of(obj: &glib::Object) -> Option<String> {
    let row = obj.downcast_ref::<gtk::TreeListRow>()?;
    let item = row.item()?.downcast::<gtk::StringObject>().ok()?;
    Some(item.string().to_string())
}

impl Binder {
    pub fn new() -> Binder {
        let store = gio::ListStore::new::<gtk::StringObject>();
        let children: Rc<RefCell<HashMap<String, Vec<String>>>> = Rc::default();
        let searching: Rc<Cell<bool>> = Rc::default();
        let tree = {
            let children = children.clone();
            let searching = searching.clone();
            gtk::TreeListModel::new(store.clone(), false, false, move |obj| {
                if searching.get() {
                    return None;
                }
                let uuid = obj.downcast_ref::<gtk::StringObject>()?.string();
                let kids = children.borrow().get(uuid.as_str()).cloned()?;
                if kids.is_empty() {
                    return None;
                }
                let model = gio::ListStore::new::<gtk::StringObject>();
                for k in kids {
                    model.append(&gtk::StringObject::new(&k));
                }
                Some(model.upcast())
            })
        };
        let selection = gtk::SingleSelection::builder()
            .model(&tree)
            .autoselect(false)
            .can_unselect(true)
            .build();
        let list = gtk::ListView::builder()
            .model(&selection)
            .css_classes(["navigation-sidebar", "omaquill-binder"])
            .build();
        let scrolled = gtk::ScrolledWindow::builder()
            .child(&list)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search project")
            .margin_start(8)
            .margin_end(8)
            .margin_top(8)
            .margin_bottom(4)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&search);
        root.append(&scrolled);
        let menu = gtk::PopoverMenu::from_model(None::<&gio::MenuModel>);
        menu.set_has_arrow(false);
        menu.set_halign(gtk::Align::Start);
        Binder {
            root,
            list,
            search,
            menu,
            store,
            tree,
            selection,
            children,
            info: Rc::default(),
            searching,
            refreshing: Cell::new(false),
        }
    }

    /// UUIDs of expanded folders, top to bottom.
    pub fn expanded(&self) -> Vec<String> {
        let mut out = Vec::new();
        for i in 0..self.tree.n_items() {
            if let Some(row) = self.tree.row(i)
                && row.is_expanded()
                && let Some(u) = uuid_of(row.upcast_ref())
            {
                out.push(u);
            }
        }
        out
    }

    fn row_of(&self, uuid: &str) -> Option<(u32, gtk::TreeListRow)> {
        (0..self.tree.n_items()).find_map(|i| {
            let row = self.tree.row(i)?;
            (uuid_of(row.upcast_ref())?.eq_ignore_ascii_case(uuid)).then_some((i, row))
        })
    }

    /// Selects `uuid`'s row (or nothing) without opening it.
    pub fn select_quietly(&self, uuid: Option<&str>) {
        self.refreshing.set(true);
        match uuid.and_then(|u| self.row_of(u)) {
            Some((i, _)) => self.selection.set_selected(i),
            None => self.selection.set_selected(gtk::INVALID_LIST_POSITION),
        }
        self.refreshing.set(false);
    }

    pub fn selected(&self) -> Option<String> {
        self.selection.selected_item().and_then(|o| uuid_of(&o))
    }
}

impl Win {
    pub(super) fn wire_binder(self: &Rc<Self>) {
        let b = &self.binder;
        let factory = gtk::SignalListItemFactory::new();
        let menu_model = {
            let m = gio::Menu::new();
            let add = gio::Menu::new();
            add.append(Some("New Text"), Some("win.new-text"));
            add.append(Some("New Folder"), Some("win.new-folder"));
            m.append_section(None, &add);
            let edit = gio::Menu::new();
            edit.append(Some("Rename…"), Some("win.rename"));
            edit.append(Some("Move to Trash"), Some("win.trash"));
            m.append_section(None, &edit);
            let trash = gio::Menu::new();
            trash.append(Some("Empty Trash…"), Some("win.empty-trash"));
            m.append_section(None, &trash);
            m
        };
        b.menu.set_menu_model(Some(&menu_model));
        b.menu.set_parent(&b.root);
        let weak = Rc::downgrade(self);
        factory.connect_setup(move |_, obj| {
            let Some(li) = obj.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let icon = gtk::Image::new();
            let title = gtk::Label::builder()
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            let dot = gtk::Box::builder()
                .css_classes(["omaquill-dot"])
                .valign(gtk::Align::Center)
                .visible(false)
                .build();
            let row = gtk::Box::builder().spacing(8).build();
            row.append(&icon);
            row.append(&title);
            row.append(&dot);
            let expander = gtk::TreeExpander::builder().child(&row).build();
            li.set_child(Some(&expander));

            // Drag source.
            let drag = gtk::DragSource::new();
            drag.set_actions(gdk::DragAction::MOVE);
            let li_weak = li.downgrade();
            let w = weak.clone();
            drag.connect_prepare(move |_, _, _| {
                let li = li_weak.upgrade()?;
                let uuid = uuid_of(&li.item()?)?;
                let win = w.upgrade()?;
                let kind = win.binder.info.borrow().get(&uuid)?.kind;
                if kind.is_root_folder() || win.binder.searching.get() {
                    return None;
                }
                Some(gdk::ContentProvider::for_value(&uuid.to_value()))
            });
            let row_weak = row.downgrade();
            drag.connect_drag_begin(move |src, _| {
                if let Some(row) = row_weak.upgrade() {
                    src.set_icon(Some(&gtk::WidgetPaintable::new(Some(&row))), 0, 0);
                }
            });
            expander.add_controller(drag);

            // Drop target.
            let drop = gtk::DropTarget::new(String::static_type(), gdk::DragAction::MOVE);
            let exp_weak = expander.downgrade();
            let li_weak = li.downgrade();
            let w = weak.clone();
            let zone = move |y: f64| -> Option<Drop> {
                let exp = exp_weak.upgrade()?;
                let li = li_weak.upgrade()?;
                let uuid = uuid_of(&li.item()?)?;
                let win = w.upgrade()?;
                let kind = win.binder.info.borrow().get(&uuid)?.kind;
                let h = exp.height().max(1) as f64;
                let can_hold = !matches!(
                    kind,
                    Kind::Image | Kind::Pdf | Kind::WebArchive | Kind::Other
                );
                Some(if kind.is_root_folder() {
                    Drop::Into
                } else if y < h * 0.3 {
                    Drop::Before
                } else if y > h * 0.7 || !can_hold {
                    if y < h * 0.5 {
                        Drop::Before
                    } else {
                        Drop::After
                    }
                } else {
                    Drop::Into
                })
            };
            let zone = Rc::new(zone);
            let exp_weak = expander.downgrade();
            let z = zone.clone();
            drop.connect_motion(move |_, _, y| {
                if let Some(exp) = exp_weak.upgrade() {
                    for c in ["drop-before", "drop-into", "drop-after"] {
                        exp.remove_css_class(c);
                    }
                    exp.add_css_class(match z(y) {
                        Some(Drop::Before) => "drop-before",
                        Some(Drop::After) => "drop-after",
                        _ => "drop-into",
                    });
                }
                gdk::DragAction::MOVE
            });
            let exp_weak = expander.downgrade();
            drop.connect_leave(move |_| {
                if let Some(exp) = exp_weak.upgrade() {
                    for c in ["drop-before", "drop-into", "drop-after"] {
                        exp.remove_css_class(c);
                    }
                }
            });
            let li_weak = li.downgrade();
            let w = weak.clone();
            let exp_weak = expander.downgrade();
            drop.connect_drop(move |_, value, _, y| {
                if let Some(exp) = exp_weak.upgrade() {
                    for c in ["drop-before", "drop-into", "drop-after"] {
                        exp.remove_css_class(c);
                    }
                }
                let (Some(win), Some(li), Ok(dragged)) =
                    (w.upgrade(), li_weak.upgrade(), value.get::<String>())
                else {
                    return false;
                };
                let Some(target) = li.item().and_then(|o| uuid_of(&o)) else {
                    return false;
                };
                let Some(z) = zone(y) else { return false };
                // Let the drag finish before the model is rebuilt under it.
                let win2 = win.clone();
                glib::idle_add_local_once(move || win2.drop_item(&dragged, &target, z));
                true
            });
            expander.add_controller(drop);

            // Right-click: select the row, then the binder's shared menu.
            let click = gtk::GestureClick::builder().button(3).build();
            let li_weak = li.downgrade();
            let w = weak.clone();
            let exp_weak = expander.downgrade();
            click.connect_pressed(move |_, _, x, y| {
                let (Some(win), Some(li), Some(exp)) =
                    (w.upgrade(), li_weak.upgrade(), exp_weak.upgrade())
                else {
                    return;
                };
                win.binder.selection.set_selected(li.position());
                let at = exp
                    .compute_point(
                        &win.binder.root,
                        &gtk::graphene::Point::new(x as f32, y as f32),
                    )
                    .unwrap_or_else(|| gtk::graphene::Point::new(x as f32, y as f32));
                let popover = &win.binder.menu;
                popover.set_pointing_to(Some(&gdk::Rectangle::new(
                    at.x() as i32,
                    at.y() as i32,
                    1,
                    1,
                )));
                popover.popup();
            });
            expander.add_controller(click);
        });
        let weak = Rc::downgrade(self);
        factory.connect_bind(move |_, obj| {
            let Some(li) = obj.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(win) = weak.upgrade() else { return };
            let Some(expander) = li.child().and_downcast::<gtk::TreeExpander>() else {
                return;
            };
            let row = li.item().and_downcast::<gtk::TreeListRow>();
            expander.set_list_row(row.as_ref());
            let Some(uuid) = li.item().and_then(|o| uuid_of(&o)) else {
                return;
            };
            let info = win.binder.info.borrow().get(&uuid).cloned();
            let Some(info) = info else { return };
            let Some(row_box) = expander.child().and_downcast::<gtk::Box>() else {
                return;
            };
            let icon = row_box.first_child().and_downcast::<gtk::Image>();
            let title = icon
                .as_ref()
                .and_then(|i| i.next_sibling())
                .and_downcast::<gtk::Label>();
            let dot = title
                .as_ref()
                .and_then(|t| t.next_sibling())
                .and_downcast::<gtk::Box>();
            if let Some(i) = icon {
                i.set_icon_name(Some(icon_for(&info)));
            }
            if let Some(t) = title {
                t.set_text(if info.title.is_empty() {
                    "Untitled"
                } else {
                    &info.title
                });
                if info.kind.is_root_folder() {
                    t.add_css_class("heading");
                } else {
                    t.remove_css_class("heading");
                }
            }
            if let Some(d) = dot {
                for c in d.css_classes() {
                    if c.starts_with("omaquill-label-") {
                        d.remove_css_class(&c);
                    }
                }
                match info.label.filter(|&l| l >= 0) {
                    Some(l) => {
                        d.add_css_class(&format!("omaquill-label-{l}"));
                        d.set_visible(true);
                    }
                    None => d.set_visible(false),
                }
            }
        });
        b.list.set_factory(Some(&factory));

        let weak = Rc::downgrade(self);
        b.selection.connect_selected_item_notify(move |sel| {
            let Some(win) = weak.upgrade() else { return };
            if win.binder.refreshing.get() {
                return;
            }
            if let Some(uuid) = sel.selected_item().and_then(|o| uuid_of(&o)) {
                win.show_item(&uuid);
            }
        });
        let weak = Rc::downgrade(self);
        b.list.connect_activate(move |_, _| {
            if let Some(win) = weak.upgrade()
                && win.stack.visible_child_name().as_deref() == Some("editor")
            {
                win.editor.view.grab_focus();
            }
        });

        // hjkl and Delete while the binder has focus.
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let Some(win) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if !mods.is_empty() && mods != gdk::ModifierType::SHIFT_MASK {
                return glib::Propagation::Proceed;
            }
            let b = &win.binder;
            let sel = b.selection.selected();
            let n = b.tree.n_items();
            let pick = |i: u32| {
                b.selection.set_selected(i);
                b.list.scroll_to(i, gtk::ListScrollFlags::FOCUS, None);
            };
            match key {
                gdk::Key::j if n > 0 => pick(if sel == gtk::INVALID_LIST_POSITION {
                    0
                } else {
                    (sel + 1).min(n - 1)
                }),
                gdk::Key::k if n > 0 => pick(if sel == gtk::INVALID_LIST_POSITION {
                    0
                } else {
                    sel.saturating_sub(1)
                }),
                gdk::Key::l | gdk::Key::h => {
                    let Some(row) = b.tree.row(sel) else {
                        return glib::Propagation::Stop;
                    };
                    if key == gdk::Key::l {
                        if row.is_expandable() && !row.is_expanded() {
                            row.set_expanded(true);
                        } else if row.is_expanded() {
                            pick(sel + 1);
                        }
                    } else if row.is_expanded() {
                        row.set_expanded(false);
                    } else if let Some(parent) = row.parent() {
                        pick(parent.position());
                    }
                }
                gdk::Key::Delete | gdk::Key::KP_Delete => win.trash_selected(),
                gdk::Key::slash => {
                    b.search.grab_focus();
                }
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        b.list.add_controller(keys);

        let weak = Rc::downgrade(self);
        b.search.connect_search_changed(move |s| {
            if let Some(win) = weak.upgrade() {
                win.search_binder(&s.text());
            }
        });
        let weak = Rc::downgrade(self);
        b.search.connect_stop_search(move |s| {
            s.set_text("");
            if let Some(win) = weak.upgrade() {
                win.binder.list.grab_focus();
            }
        });
        let weak = Rc::downgrade(self);
        b.search.connect_activate(move |_| {
            if let Some(win) = weak.upgrade() {
                // Enter opens the first hit.
                if win.binder.tree.n_items() > 0 {
                    win.binder.selection.set_selected(0);
                    win.binder.list.grab_focus();
                }
            }
        });
    }

    /// Rebuilds the binder from the project. `expanded`: folders to open;
    /// None keeps what's open now.
    pub(super) fn refresh_binder(&self, expanded: Option<&[String]>) {
        let b = &self.binder;
        let keep = match expanded {
            Some(e) => e.to_vec(),
            None => b.expanded(),
        };
        let items = match self.project.borrow().as_ref() {
            Some(p) => p.items(),
            None => Vec::new(),
        };
        let mut children = HashMap::new();
        let mut info = HashMap::new();
        fn walk(
            items: &[Item],
            children: &mut HashMap<String, Vec<String>>,
            info: &mut HashMap<String, RowInfo>,
        ) {
            for i in items {
                children.insert(
                    i.uuid.clone(),
                    i.children.iter().map(|c| c.uuid.clone()).collect(),
                );
                info.insert(
                    i.uuid.clone(),
                    RowInfo {
                        title: i.title.clone(),
                        kind: i.kind,
                        label: i.label,
                        has_children: !i.children.is_empty(),
                    },
                );
                walk(&i.children, children, info);
            }
        }
        walk(&items, &mut children, &mut info);
        *b.children.borrow_mut() = children;
        *b.info.borrow_mut() = info;
        b.refreshing.set(true);
        b.searching.set(false);
        let tops: Vec<gtk::StringObject> = items
            .iter()
            .map(|i| gtk::StringObject::new(&i.uuid))
            .collect();
        b.store.splice(0, b.store.n_items(), &tops);
        let mut i = 0;
        while i < b.tree.n_items() {
            if let Some(row) = b.tree.row(i)
                && let Some(u) = uuid_of(row.upcast_ref())
                && keep.iter().any(|k| k.eq_ignore_ascii_case(&u))
            {
                row.set_expanded(true);
            }
            i += 1;
        }
        // Clone first: a Ref held through set_selected would be a panic
        // waiting for a handler that borrows `current`.
        let cur = self.current.borrow().clone();
        if let Some(cur) = cur
            && let Some((i, _)) = b.row_of(&cur)
        {
            b.selection.set_selected(i);
        } else {
            b.selection.set_selected(gtk::INVALID_LIST_POSITION);
        }
        b.refreshing.set(false);
        if !b.search.text().is_empty() {
            self.search_binder(&b.search.text());
        }
    }

    fn search_binder(&self, query: &str) {
        let b = &self.binder;
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            if b.searching.get() {
                b.searching.set(false);
                self.refresh_binder(None);
            }
            return;
        }
        let hits: Vec<gtk::StringObject> = {
            let text = self.search_text.borrow();
            let info = b.info.borrow();
            let order: Vec<String> = match self.project.borrow().as_ref() {
                Some(p) => {
                    fn flat(items: &[Item], out: &mut Vec<String>) {
                        for i in items {
                            out.push(i.uuid.clone());
                            flat(&i.children, out);
                        }
                    }
                    let mut out = Vec::new();
                    flat(&p.items(), &mut out);
                    out
                }
                None => Vec::new(),
            };
            order
                .into_iter()
                .filter(|u| !info.get(u).is_some_and(|i| i.kind.is_root_folder()))
                .filter(|u| {
                    text.get(u)
                        .is_some_and(|t| q.split_whitespace().all(|w| t.contains(w)))
                })
                .map(|u| gtk::StringObject::new(&u))
                .collect()
        };
        b.refreshing.set(true);
        b.searching.set(true);
        b.store.splice(0, b.store.n_items(), &hits);
        b.refreshing.set(false);
    }

    /// Selects `uuid` in the binder (opening its folders), which shows it.
    pub(super) fn select(self: &Rc<Self>, uuid: &str) {
        let b = &self.binder;
        if b.searching.get() {
            b.search.set_text("");
        }
        let ancestors = self
            .project
            .borrow()
            .as_ref()
            .map(|p| p.ancestors(uuid))
            .unwrap_or_default();
        for a in ancestors {
            if let Some((_, row)) = b.row_of(&a) {
                row.set_expanded(true);
            }
        }
        match b.row_of(uuid) {
            Some((i, _)) => {
                if b.selection.selected() == i {
                    self.show_item(uuid);
                } else {
                    b.selection.set_selected(i);
                }
                b.list.scroll_to(i, gtk::ListScrollFlags::NONE, None);
            }
            None => self.show_item(uuid),
        }
    }

    fn target(&self) -> Option<String> {
        self.binder
            .selected()
            .or_else(|| self.current.borrow().clone())
    }

    pub(super) fn add_item(self: &Rc<Self>, kind: Kind) {
        self.save_now();
        let placed = {
            let p = self.project.borrow();
            let Some(p) = p.as_ref() else { return };
            let draft = p.root_folder(Kind::Draft).map(|d| d.uuid);
            let sel = self.target().and_then(|u| p.item(&u));
            match sel {
                Some(s) if matches!(s.kind, Kind::Draft | Kind::Research) => Some((s.uuid, None)),
                Some(s) if s.kind == Kind::Trash || p.is_in_trash(&s.uuid) => {
                    draft.map(|d| (d, None))
                }
                Some(s) => {
                    let parent = p.ancestors(&s.uuid).last().cloned().unwrap_or_default();
                    Some((parent, Some(s.uuid)))
                }
                None => draft.map(|d| (d, None)),
            }
        };
        let Some((parent, after)) = placed else {
            return;
        };
        let title = if kind == Kind::Folder {
            "New Folder"
        } else {
            "Untitled"
        };
        let result = self.with_project(|p| {
            let uuid = p.add_item(kind, title, &parent, after.as_deref())?;
            p.save()?;
            Ok::<_, omaquill::project::Error>(uuid)
        });
        match result {
            Some(Ok(uuid)) => {
                self.counts.borrow_mut().insert(uuid.clone(), 0);
                self.search_text
                    .borrow_mut()
                    .insert(uuid.clone(), title.to_lowercase());
                self.refresh_binder(None);
                self.select(&uuid);
                dialogs::rename(self, &uuid);
            }
            Some(Err(e)) => self.toast(&format!("Couldn't add: {e}")),
            None => {}
        }
    }

    pub(super) fn rename_selected(self: &Rc<Self>) {
        if let Some(uuid) = self.target() {
            dialogs::rename(self, &uuid);
        }
    }

    pub(super) fn set_title(self: &Rc<Self>, uuid: &str, title: &str) {
        match self.with_project(|p| p.set_title(uuid, title).and_then(|_| p.save())) {
            Some(Err(e)) => self.toast(&format!("Couldn't rename: {e}")),
            None => {}
            Some(Ok(())) => {
                if let Some(i) = self.binder.info.borrow_mut().get_mut(uuid) {
                    i.title = title.to_string();
                }
                if self.current.borrow().as_deref() == Some(uuid) {
                    self.title.set_subtitle(title);
                    self.inspector.set_title(title);
                }
                self.refresh_binder(None);
                self.refresh_board();
            }
        }
    }

    pub(super) fn trash_selected(self: &Rc<Self>) {
        let Some(uuid) = self.target() else { return };
        let Some(item) = self.item(&uuid) else { return };
        if item.kind.is_root_folder() {
            self.toast(&format!("{} can't be moved to the Trash", item.title));
            return;
        }
        if self
            .project
            .borrow()
            .as_ref()
            .is_some_and(|p| p.is_in_trash(&uuid))
        {
            self.toast("Already in the Trash. Empty the Trash to delete it for good.");
            return;
        }
        self.save_now();
        // Select a neighbor first so the editor doesn't hold a trashed doc.
        let next = self.neighbor(&uuid);
        match self.with_project(|p| p.trash(&uuid).and_then(|_| p.save())) {
            Some(Ok(())) => {
                self.refresh_binder(None);
                if let Some(n) = next {
                    self.select(&n);
                }
                self.update_totals();
                self.toast(&format!("Moved “{}” to the Trash", item.title));
            }
            Some(Err(e)) => self.toast(&format!("Couldn't move to the Trash: {e}")),
            None => {}
        }
    }

    fn neighbor(&self, uuid: &str) -> Option<String> {
        let p = self.project.borrow();
        let p = p.as_ref()?;
        let parent = p.ancestors(uuid).last().cloned();
        let siblings: Vec<String> = match &parent {
            Some(par) => p.item(par)?.children.into_iter().map(|c| c.uuid).collect(),
            None => p.items().into_iter().map(|c| c.uuid).collect(),
        };
        let at = siblings.iter().position(|s| s == uuid)?;
        siblings
            .get(at + 1)
            .or_else(|| at.checked_sub(1).and_then(|i| siblings.get(i)))
            .cloned()
            .or(parent)
    }

    pub(super) fn empty_trash(self: &Rc<Self>) {
        let count = self
            .project
            .borrow()
            .as_ref()
            .and_then(|p| p.root_folder(Kind::Trash))
            .map_or(0, |t| t.children.len());
        if count == 0 {
            self.toast("The Trash is empty");
            return;
        }
        let dialog = adw::AlertDialog::new(
            Some("Empty the Trash?"),
            Some(
                "Everything in the Trash will be deleted for good. Your latest backup still has it.",
            ),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("empty", "Empty Trash")]);
        dialog.set_response_appearance("empty", adw::ResponseAppearance::Destructive);
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, r| {
            let Some(win) = weak.upgrade() else { return };
            if r != "empty" {
                return;
            }
            win.save_now();
            let current_in_trash = win.current.borrow().as_ref().is_some_and(|c| {
                win.project
                    .borrow()
                    .as_ref()
                    .is_some_and(|p| p.is_in_trash(c))
            });
            if current_in_trash {
                // Its text is being deleted on purpose; let it go.
                *win.current.borrow_mut() = None;
                *win.editor_uuid.borrow_mut() = None;
                win.dirty_text.set(false);
                win.dirty_notes.set(false);
                win.dirty_synopsis.set(false);
                win.inspector.forget();
                win.notes.load(&Default::default());
                win.stack.set_visible_child_name("empty");
            }
            match win.with_project(|p| p.empty_trash().and_then(|n| p.save().map(|_| n))) {
                Some(Ok(n)) => {
                    win.refresh_binder(None);
                    win.toast(&format!(
                        "Deleted {n} item{}",
                        if n == 1 { "" } else { "s" }
                    ));
                }
                Some(Err(e)) => win.toast(&format!("Couldn't empty the Trash: {e}")),
                None => {}
            }
        });
        dialog.present(Some(&self.window));
    }

    pub(super) fn drop_item(self: &Rc<Self>, dragged: &str, target: &str, zone: Drop) {
        if dragged.eq_ignore_ascii_case(target) {
            return;
        }
        self.save_now();
        let placed = {
            let p = self.project.borrow();
            let Some(p) = p.as_ref() else { return };
            match zone {
                Drop::Into => {
                    let n = p.item(target).map_or(0, |t| t.children.len());
                    Some((Some(target.to_string()), n))
                }
                Drop::Before | Drop::After => {
                    let parent = p.ancestors(target).last().cloned();
                    let siblings: Vec<String> = match &parent {
                        Some(par) => p
                            .item(par)
                            .map(|i| i.children.into_iter().map(|c| c.uuid).collect())
                            .unwrap_or_default(),
                        None => p.items().into_iter().map(|c| c.uuid).collect(),
                    };
                    siblings
                        .iter()
                        .position(|s| s.eq_ignore_ascii_case(target))
                        .map(|i| (parent, if zone == Drop::After { i + 1 } else { i }))
                }
            }
        };
        let Some((parent, index)) = placed else {
            return;
        };
        match self.with_project(|p| {
            p.move_item(dragged, parent.as_deref(), index)
                .and_then(|_| p.save())
        }) {
            Some(Ok(())) => {
                let mut keep = self.binder.expanded();
                if zone == Drop::Into {
                    keep.push(target.to_string());
                }
                self.refresh_binder(Some(&keep));
                self.select(dragged);
                self.update_totals();
                self.refresh_board();
            }
            Some(Err(e)) => self.toast(&e.to_string()),
            None => {}
        }
    }

    pub(super) fn nudge(self: &Rc<Self>, how: Nudge) {
        let Some(uuid) = self.target() else { return };
        self.save_now();
        let placed = {
            let p = self.project.borrow();
            let Some(p) = p.as_ref() else { return };
            let ancestors = p.ancestors(&uuid);
            let parent = ancestors.last().cloned();
            let siblings: Vec<Item> = match &parent {
                Some(par) => p.item(par).map(|i| i.children).unwrap_or_default(),
                None => p.items(),
            };
            let Some(at) = siblings.iter().position(|s| s.uuid == uuid) else {
                return;
            };
            match how {
                Nudge::Up if at > 0 => Some((parent, at - 1)),
                Nudge::Down if at + 1 < siblings.len() => Some((parent, at + 2)),
                Nudge::In if at > 0 => {
                    let prev = &siblings[at - 1];
                    if matches!(
                        prev.kind,
                        Kind::Image | Kind::Pdf | Kind::WebArchive | Kind::Other
                    ) {
                        None
                    } else {
                        Some((Some(prev.uuid.clone()), prev.children.len()))
                    }
                }
                Nudge::Out => {
                    let parent = parent.clone();
                    let grand = ancestors.len().checked_sub(2).map(|i| ancestors[i].clone());
                    let par = parent.as_ref().and_then(|u| p.item(u));
                    if par.as_ref().is_some_and(|i| i.kind.is_root_folder()) {
                        // Out of Draft/Research would put it loose at the top.
                        None
                    } else {
                        let aunts: Vec<String> = match &grand {
                            Some(g) => p
                                .item(g)
                                .map(|i| i.children.into_iter().map(|c| c.uuid).collect())
                                .unwrap_or_default(),
                            None => p.items().into_iter().map(|c| c.uuid).collect(),
                        };
                        parent
                            .and_then(|par| aunts.iter().position(|a| *a == par))
                            .map(|i| (grand, i + 1))
                    }
                }
                _ => None,
            }
        };
        let Some((parent, index)) = placed else {
            return;
        };
        match self.with_project(|p| {
            p.move_item(&uuid, parent.as_deref(), index)
                .and_then(|_| p.save())
        }) {
            Some(Ok(())) => {
                let mut keep = self.binder.expanded();
                if let Some(par) = parent {
                    keep.push(par);
                }
                self.refresh_binder(Some(&keep));
                self.select(&uuid);
                self.binder.list.grab_focus();
                self.update_totals();
                self.refresh_board();
            }
            Some(Err(e)) => self.toast(&e.to_string()),
            None => {}
        }
    }
}

fn icon_for(info: &RowInfo) -> &'static str {
    icon(info.kind, info.has_children)
}

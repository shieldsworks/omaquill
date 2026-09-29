//! What a folder shows: the corkboard (index cards) or the outliner, plus
//! the pages for images and other files.

use super::{Win, group};
use adw::prelude::*;
use gtk::gio;
use omaquill::project::Item;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub struct Board {
    pub cork_page: gtk::ScrolledWindow,
    flow: gtk::FlowBox,
    pub outline_page: gtk::ScrolledWindow,
    outline: gtk::ListBox,
    pub media_page: gtk::ScrolledWindow,
    picture: gtk::Picture,
    pub file_page: adw::StatusPage,
    file_path: Rc<RefCell<Option<PathBuf>>>,
    /// UUIDs of the cards or rows on show, in order.
    shown: RefCell<Vec<String>>,
}

impl Board {
    pub fn new() -> Board {
        let flow = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .activate_on_single_click(false)
            .homogeneous(true)
            .min_children_per_line(1)
            .max_children_per_line(8)
            .row_spacing(18)
            .column_spacing(18)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .valign(gtk::Align::Start)
            .build();
        let cork_page = gtk::ScrolledWindow::builder()
            .child(&flow)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let outline = gtk::ListBox::builder()
            .css_classes(["boxed-list"])
            .selection_mode(gtk::SelectionMode::Single)
            .build();
        let clamp = adw::Clamp::builder()
            .maximum_size(1100)
            .child(&outline)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        let outline_page = gtk::ScrolledWindow::builder()
            .child(&clamp)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .can_shrink(true)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();
        let media_page = gtk::ScrolledWindow::builder().child(&picture).build();
        let file_path: Rc<RefCell<Option<PathBuf>>> = Rc::default();
        let open = gtk::Button::builder()
            .label("Open in Default App")
            .css_classes(["pill", "suggested-action"])
            .halign(gtk::Align::Center)
            .build();
        {
            let file_path = file_path.clone();
            open.connect_clicked(move |_| {
                if let Some(p) = file_path.borrow().as_ref() {
                    let _ = gio::AppInfo::launch_default_for_uri(
                        &gio::File::for_path(p).uri(),
                        gio::AppLaunchContext::NONE,
                    );
                }
            });
        }
        let file_page = adw::StatusPage::builder()
            .icon_name("x-office-document-symbolic")
            .child(&open)
            .build();
        Board {
            cork_page,
            flow,
            outline_page,
            outline,
            media_page,
            picture,
            file_page,
            file_path,
            shown: RefCell::new(Vec::new()),
        }
    }

    pub fn show_image(&self, path: Option<&Path>) {
        match path {
            Some(p) => self.picture.set_filename(Some(p)),
            None => self.picture.set_paintable(gtk::gdk::Paintable::NONE),
        }
    }

    pub fn show_file(&self, title: &str, path: Option<&Path>) {
        self.file_page.set_title(title);
        self.file_page.set_description(Some(match path {
            Some(_) => "omaquill doesn't display this kind of file itself.",
            None => "The file for this item is missing from the project.",
        }));
        *self.file_path.borrow_mut() = path.map(Path::to_path_buf);
        if let Some(button) = self.file_page.child() {
            button.set_visible(path.is_some());
        }
    }
}

fn clear<W: IsA<gtk::Widget>>(container: &W, remove: impl Fn(&gtk::Widget)) {
    while let Some(c) = container.as_ref().first_child() {
        remove(&c);
    }
}

/// A card's text: the synopsis, or failing that the start of the text.
fn card_text(win: &Win, item: &Item) -> (String, bool) {
    let p = win.project.borrow();
    let Some(p) = p.as_ref() else {
        return (String::new(), false);
    };
    let synopsis = p.synopsis(&item.uuid);
    if !synopsis.trim().is_empty() {
        return (synopsis, true);
    }
    if item.kind.has_text() {
        let text = p
            .text(&item.uuid)
            .map(|t| t.plain_text())
            .unwrap_or_default();
        let snippet: String = text
            .split_whitespace()
            .take(60)
            .collect::<Vec<_>>()
            .join(" ");
        return (snippet, false);
    }
    (String::new(), false)
}

impl Win {
    pub(super) fn wire_board(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.board.flow.connect_child_activated(move |_, child| {
            let Some(win) = weak.upgrade() else { return };
            let uuid = win
                .board
                .shown
                .borrow()
                .get(child.index() as usize)
                .cloned();
            if let Some(uuid) = uuid {
                win.select(&uuid);
            }
        });
        let weak = Rc::downgrade(self);
        self.board.outline.connect_row_activated(move |_, row| {
            let Some(win) = weak.upgrade() else { return };
            // Row 0 is the column header.
            let i = row.index() as usize;
            let uuid = i
                .checked_sub(1)
                .and_then(|i| win.board.shown.borrow().get(i).cloned());
            if let Some(uuid) = uuid {
                win.select(&uuid);
            }
        });
    }

    pub(super) fn show_corkboard(self: &Rc<Self>, folder: &Item) {
        let b = &self.board;
        clear(&b.flow, |c| b.flow.remove(c));
        let mut shown = Vec::new();
        for child in &folder.children {
            b.flow.append(&self.card(child));
            shown.push(child.uuid.clone());
        }
        *b.shown.borrow_mut() = shown;
        if folder.children.is_empty() {
            let empty = gtk::Label::builder()
                .label("This folder is empty. Add a document with Ctrl+N.")
                .css_classes(["dim-label"])
                .build();
            b.flow.append(&empty);
        }
        self.stack.set_visible_child_name("corkboard");
    }

    fn card(&self, item: &Item) -> gtk::Widget {
        let (text, is_synopsis) = card_text(self, item);
        let stripe = gtk::Box::builder().css_classes(["omaquill-stripe"]).build();
        if let Some(l) = item.label.filter(|&l| l >= 0) {
            stripe.add_css_class(&format!("omaquill-label-{l}"));
        }
        let title = gtk::Label::builder()
            .label(if item.title.is_empty() {
                "Untitled"
            } else {
                &item.title
            })
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(18)
            .css_classes(["title"])
            .build();
        let body = gtk::Label::builder()
            .label(&text)
            .xalign(0.0)
            .yalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .lines(6)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            // Without a width cap a wrapping label asks for its whole text
            // on one line, and the board fits one card per row.
            .max_width_chars(20)
            .width_chars(18)
            .vexpand(true)
            .build();
        if !is_synopsis {
            body.add_css_class("dim-label");
        }
        let inner = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_top(8)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        let head = gtk::Box::builder().spacing(6).build();
        if item.kind.is_folder() {
            head.append(&gtk::Image::from_icon_name("folder-symbolic"));
        }
        head.append(&title);
        inner.append(&head);
        inner.append(&body);
        let card = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["card", "omaquill-card"])
            .width_request(220)
            .height_request(160)
            .build();
        card.append(&stripe);
        card.append(&inner);
        card.set_tooltip_text(Some("Double-click to open"));
        card.upcast()
    }

    pub(super) fn show_outliner(self: &Rc<Self>, folder: &Item) {
        let b = &self.board;
        clear(&b.outline, |c| b.outline.remove(c));
        let (labels, statuses) = match self.project.borrow().as_ref() {
            Some(p) => (p.labels(), p.statuses()),
            None => (Vec::new(), Vec::new()),
        };
        let name = |tags: &[omaquill::project::Tag], id: Option<i32>| {
            id.and_then(|id| tags.iter().find(|t| t.id == id))
                .filter(|t| t.id >= 0)
                .map(|t| t.name.clone())
                .unwrap_or_default()
        };
        let row = |cells: [String; 5], depth: usize, header: bool| {
            let grid = gtk::Grid::builder()
                .column_spacing(12)
                .margin_top(8)
                .margin_bottom(8)
                .margin_start(12 + depth as i32 * 18)
                .margin_end(12)
                .build();
            let widths = [220, 360, 90, 110, 70];
            for (i, text) in cells.iter().enumerate() {
                let l = gtk::Label::builder()
                    .label(text)
                    .xalign(if i == 4 { 1.0 } else { 0.0 })
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .width_request(widths[i] - if i == 0 { depth as i32 * 18 } else { 0 })
                    .hexpand(i == 1)
                    .build();
                if header {
                    l.add_css_class("omaquill-outline-head");
                    l.add_css_class("dim-label");
                } else if i == 1 {
                    l.add_css_class("dim-label");
                }
                grid.attach(&l, i as i32, 0, 1, 1);
            }
            let r = gtk::ListBoxRow::builder().child(&grid).build();
            r.set_activatable(!header);
            r.set_selectable(!header);
            r
        };
        b.outline.append(&row(
            ["Title", "Synopsis", "Label", "Status", "Words"].map(String::from),
            0,
            true,
        ));
        let mut shown = Vec::new();
        let counts = self.counts.borrow().clone();
        fn total(item: &Item, counts: &std::collections::HashMap<String, usize>) -> usize {
            counts.get(&item.uuid).copied().unwrap_or(0)
                + item
                    .children
                    .iter()
                    .map(|c| total(c, counts))
                    .sum::<usize>()
        }
        // Depth-first, children indented under their folder.
        let mut stack: Vec<(&Item, usize)> = folder.children.iter().rev().map(|c| (c, 0)).collect();
        while let Some((item, depth)) = stack.pop() {
            let (text, is_synopsis) = card_text(self, item);
            let synopsis = if is_synopsis { text } else { String::new() };
            b.outline.append(&row(
                [
                    if item.title.is_empty() {
                        "Untitled".into()
                    } else {
                        item.title.clone()
                    },
                    synopsis
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("")
                        .to_string(),
                    name(&labels, item.label),
                    name(&statuses, item.status),
                    group(total(item, &counts)),
                ],
                depth,
                false,
            ));
            shown.push(item.uuid.clone());
            for c in item.children.iter().rev() {
                stack.push((c, depth + 1));
            }
        }
        *b.shown.borrow_mut() = shown;
        self.stack.set_visible_child_name("outliner");
    }

    /// Redraws the corkboard or outliner if one is showing.
    pub(super) fn refresh_board(self: &Rc<Self>) {
        let page = self.stack.visible_child_name();
        let Some(uuid) = self.current.borrow().clone() else {
            return;
        };
        let Some(item) = self.item(&uuid) else { return };
        match page.as_deref() {
            Some("corkboard") => self.show_corkboard(&item),
            Some("outliner") => self.show_outliner(&item),
            _ => {}
        }
    }
}

//! What a folder shows: the corkboard (index cards) or the outliner, plus
//! the pages for images and other files.

use super::{Win, group};
use adw::prelude::*;
use gtk::gio;
use omaquill::project::{Item, Media, PrivateCopies};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;

pub struct Board {
    pub cork_page: gtk::ScrolledWindow,
    flow: gtk::FlowBox,
    pub outline_page: gtk::ScrolledWindow,
    outline: gtk::ListBox,
    pub media_page: gtk::ScrolledWindow,
    picture: gtk::Picture,
    pub file_page: adw::StatusPage,
    /// The media on the file page, kept open for Open in Default App.
    file_media: Rc<RefCell<Option<Media>>>,
    /// Open in Default App, on the file page.
    open: gtk::Button,
    /// Copies made for Open in Default App, one per media item. `None`
    /// until the first one.
    copies: RefCell<Option<PrivateCopies>>,
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
        let file_media: Rc<RefCell<Option<Media>>> = Rc::default();

        let open = gtk::Button::builder()
            .label("Open in Default App")
            .css_classes(["pill", "suggested-action"])
            .halign(gtk::Align::Center)
            .build();
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
            file_media,
            open,
            copies: RefCell::new(None),
            shown: RefCell::new(Vec::new()),
        }
    }

    /// Shows an image, read once from its open file and decoded from
    /// those bytes. A tiny file can claim to be 100,000 pixels square, so
    /// [`decode_image`] stops at the header when the size is not sane.
    /// False: not shown.
    pub fn show_image(&self, media: Option<&Media>) -> bool {
        let pixbuf = media
            .and_then(|m| m.read_capped(MAX_IMAGE_BYTES).ok())
            .and_then(|bytes| decode_image(&bytes));
        match pixbuf {
            Some(pixbuf) => {
                let texture = gtk::gdk::Texture::for_pixbuf(&pixbuf);
                self.picture.set_paintable(Some(&texture));
                true
            }
            None => {
                self.picture.set_paintable(gtk::gdk::Paintable::NONE);
                false
            }
        }
    }

    pub fn say_image_too_large(&self) {
        self.file_page.set_description(Some(
            "This image is too large (or too damaged) for omaquill to show safely.",
        ));
    }

    pub fn show_file(&self, title: &str, media: Option<Media>) {
        self.file_page.set_title(title);
        let openable = media.as_ref().is_some_and(|m| {
            m.head(SNIFF_BYTES)
                .is_ok_and(|head| safe_to_open(Path::new(&m.name), &head))
        });
        self.file_page.set_description(Some(match media {
            Some(_) if openable => "omaquill doesn't display this kind of file itself.",
            Some(_) => {
                "omaquill doesn't open this kind of file from a project, since it could run code. \
                 If you trust it, it's in the project's Files/Data folder."
            }
            None => "The file for this item is missing from the project.",
        }));
        *self.file_media.borrow_mut() = media;
        if let Some(button) = self.file_page.child() {
            button.set_visible(openable);
        }
    }
}

/// Bytes read to sniff a file's type.
const SNIFF_BYTES: usize = 4096;

/// Largest image file omaquill will read to decode: 256 MiB.
const MAX_IMAGE_BYTES: u64 = 256 * 1024 * 1024;

/// Largest image omaquill will decode: 100 megapixels, 30,000 on a side.
const MAX_PIXELS: u64 = 100_000_000;
const MAX_SIDE: i32 = 30_000;

fn size_ok(w: i32, h: i32) -> bool {
    w > 0 && h > 0 && w <= MAX_SIDE && h <= MAX_SIDE && (w as u64) * (h as u64) <= MAX_PIXELS
}

/// Decodes `bytes` as an image. The loader reports the size from the
/// header before it allocates pixels. A size over the limits sets the
/// loader's size to 0 by 0, which makes gdk-pixbuf stop there, as
/// `gdk_pixbuf_get_file_info` does, and the result is `None`.
fn decode_image(bytes: &[u8]) -> Option<gtk::gdk_pixbuf::Pixbuf> {
    use std::cell::Cell;
    let loader = gtk::gdk_pixbuf::PixbufLoader::new();
    let refused = Rc::new(Cell::new(false));
    let sized = Rc::new(Cell::new(false));
    {
        let (refused, sized) = (refused.clone(), sized.clone());
        loader.connect_size_prepared(move |loader, w, h| {
            sized.set(true);
            if !size_ok(w, h) {
                refused.set(true);
                loader.set_size(0, 0);
            }
        });
    }
    for chunk in bytes.chunks(64 * 1024) {
        if refused.get() || loader.write(chunk).is_err() {
            break;
        }
    }
    let closed = loader.close();
    if refused.get() || !sized.get() || closed.is_err() {
        return None;
    }
    loader.pixbuf()
}

/// An allowed type by name (see `is_openable`) whose first bytes, `head`,
/// also look like a document or media file: the desktop decides how to
/// open a file partly by sniffing it, so the name alone isn't enough.
fn safe_to_open(name: &Path, head: &[u8]) -> bool {
    if !omaquill::project::is_openable(name) {
        return false;
    }
    let (kind, _) = gio::content_type_guess(Some(name), Some(head));
    let mime = gio::content_type_get_mime_type(&kind)
        .map(|m| m.to_string())
        .unwrap_or_default();
    mime == "application/pdf"
        || mime == "text/plain"
        || mime == "text/markdown"
        || ["image/", "audio/", "video/"]
            .iter()
            .any(|t| mime.starts_with(t))
}

/// Largest file Open in Default App copies: 1 GiB. The copy usually goes
/// in `$XDG_RUNTIME_DIR`, which is in memory.
const MAX_OPEN_COPY_BYTES: u64 = 1024 * 1024 * 1024;

/// Copies left by earlier runs in `$XDG_RUNTIME_DIR` go once they are this
/// old.
const STALE_COPY_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

impl Board {
    /// A private copy of the media on the file page for another app to
    /// open (see [`PrivateCopies`]), when the copy's own bytes pass
    /// [`safe_to_open`]. Its type is sniffed from the copy, so what was
    /// checked is what the app gets. The error is a sentence to show.
    ///
    /// Copies go in `$XDG_RUNTIME_DIR`, or the temp folder when that is
    /// unset. Opening an item again replaces its copy. In
    /// `$XDG_RUNTIME_DIR`, which only this user can write in, the first
    /// copy also removes copies a day old left by earlier runs.
    fn copy_to_open(&self) -> Result<std::path::PathBuf, String> {
        use std::io::Read;
        let media = self.file_media.borrow();
        let Some(media) = media.as_ref() else {
            return Err("The file for this item is missing from the project.".to_string());
        };
        let mut copies = self.copies.borrow_mut();
        let copies = copies.get_or_insert_with(|| {
            let runtime = std::env::var_os("XDG_RUNTIME_DIR")
                .map(std::path::PathBuf::from)
                .filter(|p| p.is_absolute());
            let copies = PrivateCopies::new(runtime.clone().unwrap_or_else(std::env::temp_dir));
            if runtime.is_some() {
                let _ = copies.remove_stale(STALE_COPY_AGE);
            }
            copies
        });
        let copy = copies
            .copy(media, MAX_OPEN_COPY_BYTES)
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::FileTooLarge => format!(
                    "{} is over 1 GiB, too large for omaquill to hand to another app. \
                     It's in the project's Files/Data folder.",
                    media.name
                ),
                _ => format!("Couldn't make a copy of {} to open: {e}", media.name),
            })?;
        let mut head = Vec::new();
        std::fs::File::open(&copy)
            .and_then(|f| f.take(SNIFF_BYTES as u64).read_to_end(&mut head))
            .map_err(|e| format!("Couldn't read the copy of {}: {e}", media.name))?;
        if safe_to_open(&copy, &head) {
            Ok(copy)
        } else {
            let _ = std::fs::remove_file(&copy);
            Err(format!(
                "omaquill doesn't open {} from a project, since it could run code.",
                media.name
            ))
        }
    }
}

fn clear<W: IsA<gtk::Widget>>(container: &W, remove: impl Fn(&gtk::Widget)) {
    while let Some(c) = container.as_ref().first_child() {
        remove(&c);
    }
}

/// The plain text of `uuid`, loaded once for a folder of rows that share it.
fn plain_once(
    uuid: &str,
    cache: &mut HashMap<String, String>,
    load: impl FnOnce() -> String,
) -> String {
    let key = uuid.to_ascii_uppercase();
    if let Some(text) = cache.get(&key) {
        return text.clone();
    }
    let text = load();
    cache.insert(key, text.clone());
    text
}

/// Words in `item` and its descendants. A UUID counts once.
fn folder_words(item: &Item, counts: &HashMap<String, usize>) -> usize {
    fn add(item: &Item, counts: &HashMap<String, usize>, seen: &mut HashSet<String>) -> usize {
        let mut total = 0;
        if seen.insert(item.uuid.to_ascii_uppercase()) {
            total += counts.get(&item.uuid).copied().unwrap_or(0);
        }
        for child in &item.children {
            total += add(child, counts, seen);
        }
        total
    }
    add(item, counts, &mut HashSet::new())
}

/// A card's text: the synopsis, or failing that the start of the text.
fn card_text(win: &Win, item: &Item, bodies: &mut HashMap<String, String>) -> (String, bool) {
    let p = win.project.borrow();
    let Some(p) = p.as_ref() else {
        return (String::new(), false);
    };
    let synopsis = p.synopsis(&item.uuid);
    if !synopsis.trim().is_empty() {
        return (synopsis, true);
    }
    if item.kind.has_text() {
        let text = plain_once(&item.uuid, bodies, || {
            p.text(&item.uuid)
                .map(|t| t.plain_text())
                .unwrap_or_default()
        });
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
        self.board.open.connect_clicked(move |_| {
            let Some(win) = weak.upgrade() else { return };
            match win.board.copy_to_open() {
                Ok(copy) => {
                    let _ = gio::AppInfo::launch_default_for_uri(
                        &gio::File::for_path(&copy).uri(),
                        gio::AppLaunchContext::NONE,
                    );
                }
                Err(message) => win.toast(&message),
            }
        });
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
        let mut bodies = HashMap::new();
        for child in &folder.children {
            b.flow.append(&self.card(child, &mut bodies));
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

    fn card(&self, item: &Item, bodies: &mut HashMap<String, String>) -> gtk::Widget {
        let (text, is_synopsis) = card_text(self, item, bodies);
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
        // Depth-first, children indented under their folder.
        let mut stack: Vec<(&Item, usize)> = folder.children.iter().rev().map(|c| (c, 0)).collect();
        while let Some((item, depth)) = stack.pop() {
            let synopsis = {
                let project = self.project.borrow();
                project
                    .as_ref()
                    .map(|p| p.synopsis(&item.uuid))
                    .unwrap_or_default()
            };
            let synopsis = synopsis
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("")
                .to_string();
            b.outline.append(&row(
                [
                    if item.title.is_empty() {
                        "Untitled".into()
                    } else {
                        item.title.clone()
                    },
                    synopsis,
                    name(&labels, item.label),
                    name(&statuses, item.status),
                    group(folder_words(item, &counts)),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut body = kind.to_vec();
            body.extend_from_slice(data);
            out.extend_from_slice(&body);
            out.extend_from_slice(&omaquill::zip::crc32(&body).to_be_bytes());
        }
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &ihdr);
        // One stored (uncompressed) deflate block: a 1x1 RGB row.
        let raw = [0u8, 0, 0, 0];
        let mut z = vec![0x78, 0x01, 1, raw.len() as u8, 0, !(raw.len() as u8), 0xff];
        z.extend_from_slice(&raw);
        let mut a: u32 = 1;
        let mut b: u32 = 0;
        for x in raw {
            a = (a + x as u32) % 65521;
            b = (b + a) % 65521;
        }
        z.extend_from_slice(&((b << 16) | a).to_be_bytes());
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    fn text_item(uuid: &str, children: Vec<Item>) -> Item {
        Item {
            uuid: uuid.to_string(),
            kind: omaquill::project::Kind::Text,
            title: String::new(),
            label: None,
            status: None,
            include_in_compile: true,
            extension: None,
            children,
        }
    }

    #[test]
    fn a_shared_document_is_loaded_once_for_a_folder() {
        let shared = "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE";
        let mut cache = HashMap::new();
        let mut loads = 0usize;
        for _ in 0..200 {
            let text = plain_once(shared, &mut cache, || {
                loads += 1;
                "lamp ".repeat(4_000)
            });
            assert!(text.starts_with("lamp "));
        }
        assert_eq!(loads, 1, "loaded the shared document {loads} times");
        let other = plain_once("11111111-2222-3333-4444-555555555555", &mut cache, || {
            loads += 1;
            "other".to_string()
        });
        assert_eq!(other, "other");
        assert_eq!(loads, 2);
    }

    #[test]
    fn folder_words_count_a_shared_uuid_once() {
        let shared = "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE";
        let mut counts = HashMap::new();
        counts.insert(shared.to_string(), 10);
        let mut children = Vec::new();
        for _ in 0..200 {
            children.push(text_item(shared, Vec::new()));
        }
        children.push(text_item(
            "11111111-2222-3333-4444-555555555555",
            Vec::new(),
        ));
        counts.insert("11111111-2222-3333-4444-555555555555".to_string(), 4);
        let folder = text_item(shared, children);
        assert_eq!(folder_words(&folder, &counts), 14);
    }

    #[test]
    fn images_are_size_checked_and_only_documents_open() {
        assert!(decode_image(&png(1, 1)).is_some());
        assert!(
            decode_image(&png(100_000, 100_000)).is_none(),
            "a 10-gigapixel header was accepted"
        );
        assert!(decode_image(b"").is_none());
        assert!(decode_image(b"not an image").is_none());

        assert!(safe_to_open(Path::new("content.png"), &png(1, 1)));
        let script = b"#!/bin/sh\necho hi\n";
        assert!(!safe_to_open(Path::new("content.sh"), script));
        let desktop = b"[Desktop Entry]\nExec=sh\n";
        assert!(!safe_to_open(Path::new("content.desktop"), desktop));
        // A script wearing a .txt name is still only text to a viewer.
        assert!(safe_to_open(Path::new("content.txt"), b"plain words\n"));
    }
}

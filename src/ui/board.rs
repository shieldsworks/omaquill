//! What a folder shows: the corkboard (index cards) or the outliner, plus
//! the pages for images and other files.

use super::{Win, group};
use adw::prelude::*;
use gtk::{gio, glib};
use omaquill::project::{Item, Media, PrivateCopies};
use std::cell::{Cell, RefCell};
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
    /// Set while Open in Default App copies its file on a worker, so a
    /// second click does not start another copy.
    copying: Cell<bool>,
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
            copying: Cell::new(false),
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

/// Where Open in Default App puts its copies: `$XDG_RUNTIME_DIR`, or the
/// temp folder when that is unset. The first copy of the process also
/// removes stale copies, only in `$XDG_RUNTIME_DIR`, which only this user
/// can write in.
fn copies() -> PrivateCopies {
    static CLEANED: std::sync::Once = std::sync::Once::new();
    #[cfg(test)]
    if let Some(base) = TEST_BASE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    {
        return PrivateCopies::new(base);
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute());
    let copies = PrivateCopies::new(runtime.clone().unwrap_or_else(std::env::temp_dir));
    if runtime.is_some() {
        CLEANED.call_once(|| {
            let _ = copies.remove_stale(STALE_COPY_AGE);
        });
    }
    copies
}

/// Where the copies go in tests, instead of the runtime folder.
#[cfg(test)]
static TEST_BASE: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

/// The last launched copy's path per (canonical project folder, item
/// UUID), on the main thread. A new launch of the same item removes the
/// previous copy's folder, so repeated opens don't fill the runtime
/// folder, which is in memory. An app still reading the old file keeps
/// its open file.
type LaunchedKey = (std::path::PathBuf, String);
thread_local! {
    static LAUNCHED: RefCell<HashMap<LaunchedKey, std::path::PathBuf>> =
        RefCell::new(HashMap::new());
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Hands `copy` to its default app.
fn launch(copy: &Path) -> Result<(), String> {
    #[cfg(test)]
    if let Some(fake) = *TEST_LAUNCH
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
    {
        return fake(copy);
    }
    gio::AppInfo::launch_default_for_uri(
        &gio::File::for_path(copy).uri(),
        gio::AppLaunchContext::NONE,
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
type Launcher = fn(&Path) -> Result<(), String>;

/// Stands in for the default app in tests.
#[cfg(test)]
static TEST_LAUNCH: std::sync::Mutex<Option<Launcher>> = std::sync::Mutex::new(None);

/// Held by a test to keep [`copy_for_app`] from starting.
#[cfg(test)]
static TEST_HOLD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A private copy of `media` for another app to open, in a new folder of
/// its own (see [`PrivateCopies`]), when the copy's own bytes pass
/// [`safe_to_open`]. Its type is sniffed from the copy, so what was
/// checked is what the app gets, and no later open writes to it. The
/// error is a sentence to show. This reads and writes up to 1 GiB, so it
/// runs on a worker thread.
fn copy_for_app(media: &Media) -> Result<std::path::PathBuf, String> {
    use std::io::Read;
    #[cfg(test)]
    drop(
        TEST_HOLD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    let copy = copies()
        .copy(media, MAX_OPEN_COPY_BYTES)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::FileTooLarge => format!(
                "{} is over 1 GiB, too large for omaquill to hand to another app. \
                 It's in the project's Files/Data folder.",
                media.name
            ),
            std::io::ErrorKind::StorageFull => format!(
                "{} is larger than half the free space where omaquill puts copies to open. \
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
        let _ = copy.parent().map(std::fs::remove_dir);
        Err(format!(
            "omaquill doesn't open {} from a project, since it could run code.",
            media.name
        ))
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
    let key = uuid.to_string();
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
        if seen.insert(item.uuid.clone()) {
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
    /// Open in Default App: copies the file on show on a worker thread, then
    /// launches the copy from the main thread. The button is off and a
    /// toast says so while the copy runs; a click meanwhile does nothing.
    /// When the window is gone by then, nothing is launched, and the copy
    /// stays for the next open to replace.
    pub(super) fn open_in_default_app(self: &Rc<Self>) {
        if self.board.copying.get() {
            return;
        }
        let media = match self
            .board
            .file_media
            .borrow()
            .as_ref()
            .map(Media::try_clone)
        {
            Some(Ok(media)) => media,
            Some(Err(e)) => {
                self.toast(&format!("Couldn't read the file to open it: {e}"));
                return;
            }
            None => {
                self.toast("The file for this item is missing from the project.");
                return;
            }
        };
        let key = self.project.borrow().as_ref().map(|p| {
            let root = std::fs::canonicalize(&p.path).unwrap_or_else(|_| p.path.clone());
            (root, media.uuid().to_string())
        });
        self.board.copying.set(true);
        self.board.open.set_sensitive(false);
        self.toast(&format!("Copying {} to open it…", media.name));
        let worker = gio::spawn_blocking(move || copy_for_app(&media));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let done = worker
                .await
                .unwrap_or_else(|_| Err("The copy to open stopped unexpectedly.".to_string()));
            let win = weak.upgrade();
            if let Some(win) = &win {
                win.board.copying.set(false);
                win.board.open.set_sensitive(true);
            }
            let copy = match done {
                Ok(copy) => copy,
                Err(message) => {
                    if let Some(win) = win.filter(|w| w.window.is_visible()) {
                        win.toast(&message);
                    }
                    return;
                }
            };
            let Some(win) = win.filter(|w| w.window.is_visible()) else {
                let _ = copies().remove(&copy);
                return;
            };
            if let Err(e) = launch(&copy) {
                let _ = copies().remove(&copy);
                win.toast(&format!(
                    "Couldn't open the copy of {}: {e}",
                    file_name(&copy)
                ));
                return;
            }
            if let Some(key) = key {
                let previous = LAUNCHED.with(|l| l.borrow_mut().insert(key, copy));
                if let Some(previous) = previous {
                    let _ = copies().remove(&previous);
                }
            }
        });
    }

    pub(super) fn wire_board(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.board.open.connect_clicked(move |_| {
            if let Some(win) = weak.upgrade() {
                win.open_in_default_app();
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
pub(super) mod tests {
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

    /// A copy of the fixture project with its image item turned into a
    /// `.bin` file of `size` bytes, sparse after a short header. `.bin` is
    /// never opened in another app, so the copy ends with a toast and
    /// launches nothing.
    fn bin_project(dir: &Path, ext: &str, size: u64) -> std::path::PathBuf {
        const ITEM: &str = "6FBE5C7A-8D9B-4CAE-CFDA-4B5C6D7E8F06";
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/Lighthouse.scriv");
        std::fs::create_dir_all(dir).unwrap();
        let project = dir.join("Lighthouse.scriv");
        let copied = std::process::Command::new("cp")
            .arg("-R")
            .arg(&fixture)
            .arg(&project)
            .status()
            .unwrap();
        assert!(copied.success());
        let scrivx = project.join("Lighthouse.scrivx");
        let text = std::fs::read_to_string(&scrivx).unwrap().replace(
            "<FileExtension>png</FileExtension>",
            &format!("<FileExtension>{ext}</FileExtension>"),
        );
        std::fs::write(&scrivx, text).unwrap();
        let data = project.join("Files/Data").join(ITEM);
        std::fs::remove_file(data.join("content.png")).unwrap();
        let file = std::fs::File::create(data.join(format!("content.{ext}"))).unwrap();
        std::io::Write::write_all(&mut &file, b"not for another app\n").unwrap();
        file.set_len(size).unwrap();
        project
    }

    /// A window on a [`bin_project`] of `size` bytes in `dir/tag`, with its
    /// `.bin` item on the file page.
    fn bin_window(dir: &Path, tag: &str, ext: &str, size: u64) -> Rc<Win> {
        const ITEM: &str = "6FBE5C7A-8D9B-4CAE-CFDA-4B5C6D7E8F06";
        let project = bin_project(&dir.join(tag), ext, size);
        let app = adw::Application::builder()
            .application_id(format!("io.github.shieldsworks.Omaquill.Open{tag}"))
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let win = Win::new(&app);
        win.open_project(&project);
        win.window.present();
        let media = win.project.borrow().as_ref().unwrap().media(ITEM);
        assert!(media.is_some(), "the .bin item has no file");
        *win.board.file_media.borrow_mut() = media;
        win
    }

    pub fn an_open_copy_leaves_the_main_loop_running() {
        use std::time::{Duration, Instant};
        let dir = std::env::temp_dir().join(format!("omaquill-open-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let base = dir.join("runtime");
        std::fs::create_dir_all(&base).unwrap();
        *TEST_BASE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(base.clone());
        let context = glib::MainContext::default();

        let win = bin_window(&dir, "Held", "bin", 4096);
        // Draw the window and a toast before measuring.
        win.toast("warming up");
        let end = Instant::now() + Duration::from_millis(300);
        while Instant::now() < end {
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(1));
        }
        let ticks = Rc::new(RefCell::new(Vec::new()));
        let tick = {
            let ticks = Rc::clone(&ticks);
            glib::timeout_add_local(Duration::from_millis(10), move || {
                ticks.borrow_mut().push(Instant::now());
                glib::ControlFlow::Continue
            })
        };
        // The worker waits on the hold for a second, as a slow copy would.
        // Work done on the main thread instead would show as one gap of
        // that second.
        let hold = TEST_HOLD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let started = Instant::now();
        win.board.open.emit_clicked();
        assert!(win.board.copying.get() && !win.board.open.is_sensitive());
        assert_eq!(
            win.last_toast.borrow().as_str(),
            "Copying content.bin to open it…"
        );
        // A second click while the copy runs starts nothing.
        win.last_toast.borrow_mut().clear();
        win.open_in_default_app();
        assert_eq!(win.last_toast.borrow().as_str(), "");
        while started.elapsed() < Duration::from_secs(1) {
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(win.board.copying.get(), "the copy didn't wait for the hold");
        drop(hold);
        while win.board.copying.get() {
            assert!(started.elapsed() < Duration::from_secs(30));
            context.iteration(true);
        }
        let took = started.elapsed();
        tick.remove();
        let mut gap = Duration::ZERO;
        let mut last = started;
        for &t in ticks.borrow().iter() {
            gap = gap.max(t - last);
            last = t;
        }
        gap = gap.max(started + took - last);
        eprintln!("open held for 1 s: {took:?}, longest main-loop gap {gap:?}");
        assert!(
            gap < Duration::from_millis(100),
            "the main loop stalled {gap:?}"
        );
        assert!(win.board.open.is_sensitive());
        assert_eq!(
            win.last_toast.borrow().as_str(),
            "omaquill doesn't open content.bin from a project, since it could run code."
        );
        win.window.close();

        // A window closed while its copy runs shows and launches nothing,
        // and its copy goes.
        set_launch(Some(launch_must_not_happen));
        let win = bin_window(&dir, "Closed", "txt", 1024);
        win.open_in_default_app();
        win.window.close();
        let end = Instant::now() + Duration::from_secs(10);
        while win.board.copying.get() {
            assert!(Instant::now() < end, "the copy never reported back");
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(1));
        }
        pump_ms(100);
        assert_eq!(
            win.last_toast.borrow().as_str(),
            "Copying content.txt to open it…"
        );
        // Refused copies and a closed window's copy leave no folder.
        assert_eq!(folders(&base), 0);
        set_launch(None);
        *TEST_BASE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn launch_must_not_happen(copy: &Path) -> Result<(), String> {
        panic!("{} was launched", copy.display())
    }

    fn launch_succeeds(_: &Path) -> Result<(), String> {
        Ok(())
    }

    fn launch_fails(_: &Path) -> Result<(), String> {
        Err("no app for it".to_string())
    }

    fn set_launch(fake: Option<Launcher>) {
        *TEST_LAUNCH
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = fake;
    }

    fn pump_ms(ms: u64) {
        let context = glib::MainContext::default();
        let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        while std::time::Instant::now() < end {
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// The copy folders in `base`.
    fn folders(base: &Path) -> usize {
        std::fs::read_dir(base).unwrap().count()
    }

    /// Clicks Open in Default App in `win` and runs the main loop until the
    /// copy reports back.
    fn open_and_wait(win: &Win) {
        win.board.open.emit_clicked();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while win.board.copying.get() {
            assert!(
                std::time::Instant::now() < end,
                "the copy never reported back"
            );
            glib::MainContext::default().iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        pump_ms(50);
    }

    pub fn repeated_opens_keep_one_copy_per_item() {
        let dir = std::env::temp_dir().join(format!("omaquill-open-again-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let base = dir.join("runtime");
        std::fs::create_dir_all(&base).unwrap();
        *TEST_BASE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(base.clone());
        set_launch(Some(launch_succeeds));

        let a = bin_window(&dir, "A", "txt", 64);
        for _ in 0..5 {
            open_and_wait(&a);
        }
        assert_eq!(folders(&base), 1, "five opens of one item");
        let only = std::fs::read_dir(&base)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();

        // A copy of the project shares its UUIDs: its own folder, and A's
        // launched copy stays.
        let b = bin_window(&dir, "B", "txt", 64);
        open_and_wait(&b);
        assert_eq!(folders(&base), 2);
        assert!(only.join("content.txt").exists());

        // A failed launch removes its new copy, keeps the last launched
        // one, and says why.
        set_launch(Some(launch_fails));
        open_and_wait(&a);
        assert_eq!(folders(&base), 2);
        assert!(only.join("content.txt").exists());
        assert_eq!(
            a.last_toast.borrow().as_str(),
            "Couldn't open the copy of content.txt: no app for it"
        );

        set_launch(None);
        a.window.close();
        b.window.close();
        *TEST_BASE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let _ = std::fs::remove_dir_all(&dir);
    }
}

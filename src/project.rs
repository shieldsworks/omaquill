//! A Scrivener project on disk: the `.scriv` folder.
//!
//! ```text
//! Novel.scriv/
//!   Novel.scrivx              the binder and project settings (XML)
//!   Files/Data/<UUID>/        one folder per binder item:
//!     content.rtf             its text (or content.png, content.pdf ...)
//!     notes.rtf               its notes
//!     synopsis.txt            its index-card synopsis
//!   Files/Data/docs.checksum  SHA-1 of each of those files
//!   Files/search.indexes      plain text of everything, for search
//! ```
//!
//! The `.scrivx` stays a lossless XML tree ([`crate::xml`]); every edit is
//! made on that tree, so settings omaquill doesn't know about survive.

use crate::fsdir::{self, Dir};
use crate::rtf::{self, RichText};
use crate::xml::{self, Element};
use crate::{sha1, zip};
#[cfg(test)]
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

#[cfg(test)]
thread_local! {
    static CONTENT_WRITES: Cell<u64> = const { Cell::new(0) };
    static RETAINED_INDEX: Cell<usize> = const { Cell::new(0) };
    static BINDER_WALKS: Cell<u64> = const { Cell::new(0) };
    static INDEX_VISITS: Cell<u64> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_content_writes() {
    CONTENT_WRITES.with(|n| n.set(0));
}

#[cfg(test)]
pub(crate) fn content_writes() -> u64 {
    CONTENT_WRITES.with(|n| n.get())
}

fn note_retained(docs: &HashMap<String, IndexedDoc>) {
    #[cfg(test)]
    {
        let n = docs
            .values()
            .map(|doc| doc.body.capacity() + doc.notes.as_ref().map(String::capacity).unwrap_or(0))
            .sum();
        RETAINED_INDEX.with(|kept| kept.set(n));
    }
    #[cfg(not(test))]
    let _ = docs;
}

#[cfg(test)]
fn retained_index() -> usize {
    RETAINED_INDEX.with(|kept| kept.get())
}

fn note_binder_walk() {
    #[cfg(test)]
    BINDER_WALKS.with(|n| n.set(n.get().saturating_add(1)));
}

fn note_index_visit(n: u64) {
    #[cfg(test)]
    INDEX_VISITS.with(|visits| visits.set(visits.get().saturating_add(n)));
    #[cfg(not(test))]
    let _ = n;
}

#[cfg(test)]
fn reset_index_scan() {
    BINDER_WALKS.with(|n| n.set(0));
    INDEX_VISITS.with(|visits| visits.set(0));
}

#[cfg(test)]
fn binder_walks() -> u64 {
    BINDER_WALKS.with(|n| n.get())
}

#[cfg(test)]
fn index_visits() -> u64 {
    INDEX_VISITS.with(|visits| visits.get())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Folder,
    Draft,
    Research,
    Trash,
    Image,
    Pdf,
    WebArchive,
    Other,
}

impl Kind {
    fn from_attr(s: &str) -> Kind {
        match s {
            "Text" => Kind::Text,
            "Folder" => Kind::Folder,
            "DraftFolder" => Kind::Draft,
            "ResearchFolder" => Kind::Research,
            "TrashFolder" => Kind::Trash,
            "Image" => Kind::Image,
            "PDF" => Kind::Pdf,
            "WebArchive" => Kind::WebArchive,
            _ => Kind::Other,
        }
    }

    fn attr(self) -> &'static str {
        match self {
            Kind::Text => "Text",
            Kind::Folder => "Folder",
            Kind::Draft => "DraftFolder",
            Kind::Research => "ResearchFolder",
            Kind::Trash => "TrashFolder",
            Kind::Image => "Image",
            Kind::Pdf => "PDF",
            Kind::WebArchive => "WebArchive",
            Kind::Other => "Other",
        }
    }

    /// Draft, Research and Trash: always at the top, never moved or deleted.
    pub fn is_root_folder(self) -> bool {
        matches!(self, Kind::Draft | Kind::Research | Kind::Trash)
    }

    pub fn is_folder(self) -> bool {
        matches!(
            self,
            Kind::Folder | Kind::Draft | Kind::Research | Kind::Trash
        )
    }

    /// Items whose content is RTF text omaquill can edit. Folders can hold
    /// text too in Scrivener.
    pub fn has_text(self) -> bool {
        matches!(
            self,
            Kind::Text | Kind::Folder | Kind::Draft | Kind::Research
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// The binder id in its [`canonical_id`] form, whatever case the
    /// `.scrivx` spells it in.
    pub uuid: String,
    pub kind: Kind,
    pub title: String,
    pub label: Option<i32>,
    pub status: Option<i32>,
    pub include_in_compile: bool,
    /// For media items: the `content.<ext>` extension.
    pub extension: Option<String>,
    pub children: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tag {
    pub id: i32,
    pub name: String,
    pub color: Option<(f32, f32, f32)>,
}

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Xml(xml::Error),
    NotAProject(PathBuf),
    NoSuchItem(String),
    Invalid(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Xml(e) => write!(f, "{e}"),
            Error::NotAProject(p) => write!(f, "{} is not a Scrivener project", p.display()),
            Error::NoSuchItem(u) => write!(f, "no binder item {u}"),
            Error::Invalid(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<xml::Error> for Error {
    fn from(e: xml::Error) -> Self {
        Error::Xml(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

fn missing_item(uuid: &str) -> Error {
    if uuid.is_empty() {
        Error::Invalid("binder item has no id".into())
    } else {
        Error::NoSuchItem(uuid.to_string())
    }
}

/// Words compiled from `draft`, counting each document once.
///
/// The count starts at the Draft folder's children, as [`crate::compile::gather`]
/// does. The folder's own text is not a chapter. A child counts only when
/// it is included in compile, and an excluded folder hides its children.
/// Rows that share a UUID are one document.
pub fn compile_words(draft: &Item, counts: &HashMap<String, usize>) -> usize {
    fn add(
        item: &Item,
        counts: &HashMap<String, usize>,
        seen: &mut HashSet<String>,
        gate: bool,
    ) -> usize {
        // Compile omits an excluded item and everything under it. An
        // included child of an excluded folder is not in the manuscript.
        if gate && !item.include_in_compile {
            return 0;
        }
        let mut total = 0;
        if seen.insert(item.uuid.to_ascii_uppercase()) {
            total += counts.get(&item.uuid).copied().unwrap_or(0);
        }
        for child in &item.children {
            total += add(child, counts, seen, true);
        }
        total
    }
    let mut seen = HashSet::new();
    let mut total = 0;
    for child in &draft.children {
        total += add(child, counts, &mut seen, true);
    }
    total
}

/// Characters in the paragraph after trimming the joined runs. One short
/// run does not decide this. The count is characters, not bytes.
fn paragraph_trimmed_chars(par: &rtf::Paragraph) -> usize {
    par.text().trim().chars().count()
}

/// The character style that covers the most characters. A tie keeps the
/// earlier style. A short lead-in is not the result when a later run is longer.
fn dominant_char_style(par: &rtf::Paragraph) -> rtf::CharStyle {
    let mut totals: Vec<(&rtf::CharStyle, usize)> = Vec::new();
    let mut slots: HashMap<&rtf::CharStyle, usize> = HashMap::new();
    for run in &par.runs {
        let n = run.text.chars().count();
        if n == 0 {
            continue;
        }
        let slot = *slots.entry(&run.style).or_insert_with(|| {
            totals.push((&run.style, 0));
            totals.len() - 1
        });
        totals[slot].1 += n;
    }
    let mut best: Option<(&rtf::CharStyle, usize)> = None;
    for (style, n) in totals {
        if best.is_none_or(|pair| n > pair.1) {
            best = Some((style, n));
        }
    }
    best.map(|(style, _)| style.clone()).unwrap_or_default()
}

/// Word counts and lowercase search text, one entry per binder id.
pub struct BinderIndex {
    pub counts: HashMap<String, usize>,
    pub search: HashMap<String, String>,
}

struct IndexedDoc {
    synopsis: String,
    /// Loaded the first time a row with this id has text.
    body: String,
    body_loaded: bool,
    /// `None` when the notes file could not be read. A missing file is
    /// `Some("")`, matching `notes`.
    notes: Option<String>,
}

struct IndexSlot {
    title: String,
    /// The last row with this id has a text body.
    has_text: bool,
    /// Some row with this id had a text body, so the count stays.
    saw_text: bool,
}

fn index_rows(items: &[Item], rows: &mut HashMap<String, (String, bool)>) {
    for item in items {
        rows.entry(item.uuid.clone())
            .or_insert_with(|| (item.title.clone(), item.kind.has_text()));
        index_rows(&item.children, rows);
    }
}

fn index_items(
    project: &Project,
    items: &[Item],
    docs: &mut HashMap<String, IndexedDoc>,
    slots: &mut HashMap<String, IndexSlot>,
) {
    for item in items {
        if is_uuid(&item.uuid) {
            let doc = docs.entry(item.uuid.clone()).or_insert_with(|| IndexedDoc {
                synopsis: project.synopsis(&item.uuid),
                body: String::new(),
                body_loaded: false,
                notes: match project.notes(&item.uuid) {
                    Ok(text) => Some(text.plain_text()),
                    Err(_) => None,
                },
            });
            if item.kind.has_text() && !doc.body_loaded {
                doc.body = project
                    .text(&item.uuid)
                    .map(|text| text.plain_text())
                    .unwrap_or_default();
                doc.body_loaded = true;
            }
        }
        let slot = slots.entry(item.uuid.clone()).or_insert(IndexSlot {
            title: String::new(),
            has_text: false,
            saw_text: false,
        });
        slot.title.clone_from(&item.title);
        slot.has_text = item.kind.has_text();
        if item.kind.has_text() {
            slot.saw_text = true;
        }
        index_items(project, &item.children, docs, slots);
    }
}

pub struct Project {
    pub path: PathBuf,
    /// The project folder, opened once. Every read, write and delete in the
    /// project goes through it and never follows a symlink (see
    /// [`fsdir`]).
    root: Dir,
    /// The `.scrivx` file's name in `root`.
    scrivx: OsString,
    doc: xml::Document,
    /// `lowercase-uuid/file` → SHA-1, in file order.
    checksums: Vec<(String, String)>,
    /// Search-index entries to rewrite on the next save, by UUID.
    reindex: BTreeMap<String, ()>,
    scrivx_dirty: bool,
    checksums_dirty: bool,
}

/// A media item's file, open for reading. See [`Project::media`].
#[derive(Debug)]
pub struct Media {
    /// `content.<ext>`, the file's name in its item folder. Only its
    /// extension is meant for deciding the type.
    pub name: String,
    uuid: String,
    file: std::fs::File,
}

impl Media {
    /// The whole file, or an `InvalidData` error when it is longer than
    /// `max` bytes. At most `max + 1` bytes are read.
    pub fn read_capped(&self, max: u64) -> io::Result<Vec<u8>> {
        use io::{Read, Seek};
        let mut file = &self.file;
        file.seek(io::SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        file.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is larger than {max} bytes", self.name),
            ));
        }
        Ok(bytes)
    }

    /// Up to `n` bytes from the start of the file.
    pub fn head(&self, n: usize) -> io::Result<Vec<u8>> {
        use std::os::unix::fs::FileExt;
        let mut buf = vec![0u8; n];
        let mut got = 0;
        while got < n {
            match self.file.read_at(&mut buf[got..], got as u64) {
                Ok(0) => break,
                Ok(k) => got += k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        buf.truncate(got);
        Ok(buf)
    }

    /// A second handle on the same open file, to read on another thread.
    pub fn try_clone(&self) -> io::Result<Media> {
        Ok(Media {
            name: self.name.clone(),
            uuid: self.uuid.clone(),
            file: self.file.try_clone()?,
        })
    }

    /// The media item's UUID.
    pub fn uuid(&self) -> &str {
        &self.uuid
    }
}

/// Private copies of media for another app to open, in `base`. Every
/// [`PrivateCopies::copy`] makes a new folder `omaquill-open-<random>`,
/// created exclusively with mode `0o700`, holding one copy
/// `content.<ext>` of mode `0o600`. Its path is the one to hand to another
/// app: no other user can swap it, it is not in the project, and no later
/// copy writes to it. Nothing is shared or reused between copies, so two
/// projects whose items share UUIDs can't collide.
#[derive(Debug, Clone)]
pub struct PrivateCopies {
    base: PathBuf,
}

impl PrivateCopies {
    pub fn new(base: PathBuf) -> PrivateCopies {
        PrivateCopies { base }
    }

    /// Copies `media` into a new folder and returns the copy's path. A
    /// file longer than `max` bytes is a `FileTooLarge` error, and one
    /// larger than half the free space in `base` a `StorageFull` error. On any
    /// error the new folder is removed.
    pub fn copy(&self, media: &Media, max: u64) -> io::Result<PathBuf> {
        let too_large = || {
            io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!("{} is larger than {max} bytes", media.name),
            )
        };
        if media.file.metadata()?.len() > max {
            return Err(too_large());
        }
        let base = Dir::open(&self.base)?;
        let size = media.file.metadata()?.len();
        if size > base.free_bytes()? / 2 {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                format!(
                    "{} is larger than half the free space in {}",
                    media.name,
                    self.base.display()
                ),
            ));
        }
        let folder_name = format!("omaquill-open-{}", new_uuid());
        let folder = base.create_private_dir_new(OsStr::new(&folder_name))?;
        let copied = Self::fill(&folder, media, max).and_then(|copied| {
            if copied > max {
                Err(too_large())
            } else {
                Ok(())
            }
        });
        if let Err(e) = copied {
            drop(folder);
            let _ = base.remove_tree(Path::new(&folder_name));
            return Err(e);
        }
        Ok(self.base.join(folder_name).join(&media.name))
    }

    /// Removes the folder of `copy`, a path [`PrivateCopies::copy`]
    /// returned. Anything else is left alone: only an `omaquill-open-*`
    /// folder right in `base` goes, and a link there goes as a link.
    pub fn remove(&self, copy: &Path) -> io::Result<()> {
        let folder = copy
            .parent()
            .filter(|f| f.parent() == Some(self.base.as_path()));
        let name = folder
            .and_then(|f| f.file_name())
            .and_then(|n| n.to_str())
            .filter(|n| n.starts_with("omaquill-open-"));
        match name {
            Some(name) => Dir::open(&self.base)?.remove_tree(Path::new(name)),
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{} is not a copy in {}",
                    copy.display(),
                    self.base.display()
                ),
            )),
        }
    }

    /// Writes at most `max + 1` bytes of `media` to a new `0o600` file in
    /// `folder` and returns how many were written.
    fn fill(folder: &Dir, media: &Media, max: u64) -> io::Result<u64> {
        use io::{Read, Seek};
        let mut out = folder.create_private_new(OsStr::new(&media.name))?;
        let mut from = &media.file;
        from.seek(io::SeekFrom::Start(0))?;
        let copied = io::copy(&mut from.take(max.saturating_add(1)), &mut out)?;
        out.sync_all()?;
        Ok(copied)
    }

    /// Removes `omaquill-open-*` entries in `base` last changed more than
    /// `age` ago and returns how many went. An entry that can't be
    /// removed, such as another user's folder, is skipped, and the rest
    /// are still tried. Symlinks are removed as links, never followed.
    /// Only for a `base` no other user can write in, such as
    /// `$XDG_RUNTIME_DIR`.
    pub fn remove_stale(&self, age: std::time::Duration) -> io::Result<usize> {
        let base = Dir::open(&self.base)?;
        let mut removed = 0;
        for entry in base.entries()? {
            let Some(name) = entry.to_str() else { continue };
            if !name.starts_with("omaquill-open-") {
                continue;
            }
            let stale = std::fs::symlink_metadata(self.base.join(name))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|d| d > age);
            if stale && base.remove_tree(Path::new(name)).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }
}

/// Where a binder item sits: the path of item indices from the binder.
type Loc = Vec<usize>;

impl Project {
    pub fn open(path: &Path) -> Result<Project> {
        let root = Dir::open(path)?;
        Project::open_in(path.to_path_buf(), root)
    }

    fn open_in(path: PathBuf, root: Dir) -> Result<Project> {
        let scrivx = find_scrivx(&root, &path).ok_or_else(|| Error::NotAProject(path.clone()))?;
        let text = String::from_utf8_lossy(&root.read(Path::new(&scrivx))?).into_owned();
        let doc = xml::parse(&text)?;
        if doc.root.name != "ScrivenerProject" || doc.root.child("Binder").is_none() {
            return Err(Error::NotAProject(path));
        }
        let checksums = parse_checksums(
            &root
                .read(Path::new("Files/Data/docs.checksum"))
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default(),
        );
        Ok(Project {
            path,
            root,
            scrivx,
            doc,
            checksums,
            reindex: BTreeMap::new(),
            scrivx_dirty: false,
            checksums_dirty: false,
        })
    }

    /// A new, empty Scrivener 3 project at `path` (which should end in
    /// `.scriv` and not exist yet). The project folder is made with
    /// `mkdirat` in its parent and opened without following a symlink, so
    /// an entry that appears at `path` first is an error, not a place to
    /// write.
    pub fn create(path: &Path) -> Result<Project> {
        let exists = || Error::Invalid(format!("{} already exists", path.display()));
        if path.symlink_metadata().is_ok() {
            return Err(exists());
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled")
            .to_string();
        let folder = path
            .file_name()
            .ok_or_else(|| Error::Invalid(format!("{} names no folder", path.display())))?;
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        std::fs::create_dir_all(parent)?;
        let root = match Dir::open(parent)?.create_dir_new(folder) {
            Ok(root) => root,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(exists()),
            Err(e) => return Err(e.into()),
        };
        root.walk(Path::new("Files/Data"), true)?;
        root.walk(Path::new("Settings"), true)?;
        root.write_atomic(Path::new("Files/version.txt"), b"23", false)?;
        let now = timestamp();
        let (draft, research, trash, chapter) = (new_uuid(), new_uuid(), new_uuid(), new_uuid());
        let scrivx = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<ScrivenerProject Identifier="{id}" Version="2.0" Creator="omaquill-{ver}" Device="omarchy" Author="" Modified="{now}" ModID="{modid}">
    <Binder>
        <BinderItem UUID="{draft}" Type="DraftFolder" Created="{now}" Modified="{now}">
            <Title>Manuscript</Title>
            <MetaData>
                <IncludeInCompile>Yes</IncludeInCompile>
            </MetaData>
            <Children>
                <BinderItem UUID="{chapter}" Type="Text" Created="{now}" Modified="{now}">
                    <Title>Chapter One</Title>
                    <MetaData>
                        <IncludeInCompile>Yes</IncludeInCompile>
                    </MetaData>
                </BinderItem>
            </Children>
        </BinderItem>
        <BinderItem UUID="{research}" Type="ResearchFolder" Created="{now}" Modified="{now}">
            <Title>Research</Title>
        </BinderItem>
        <BinderItem UUID="{trash}" Type="TrashFolder" Created="{now}" Modified="{now}">
            <Title>Trash</Title>
        </BinderItem>
    </Binder>
    <LabelSettings>
        <Title>Label</Title>
        <DefaultLabelID>-1</DefaultLabelID>
        <Labels>
            <Label ID="-1">No Label</Label>
            <Label ID="1" Color="0.993495 0.701207 0.732587">Red</Label>
            <Label ID="2" Color="0.995418 0.790946 0.652385">Orange</Label>
            <Label ID="3" Color="0.997722 0.89273 0.652569">Yellow</Label>
            <Label ID="4" Color="0.715855 0.948712 0.697692">Green</Label>
            <Label ID="5" Color="0.702319 0.888276 0.974252">Blue</Label>
            <Label ID="6" Color="0.957566 0.766747 0.999616">Purple</Label>
        </Labels>
    </LabelSettings>
    <StatusSettings>
        <Title>Status</Title>
        <DefaultStatusID>-1</DefaultStatusID>
        <StatusItems>
            <Status ID="-1">No Status</Status>
            <Status ID="1">To Do</Status>
            <Status ID="2">First Draft</Status>
            <Status ID="3">Revised Draft</Status>
            <Status ID="4">Final Draft</Status>
            <Status ID="5">Done</Status>
        </StatusItems>
    </StatusSettings>
</ScrivenerProject>
"#,
            id = new_uuid(),
            ver = env!("CARGO_PKG_VERSION"),
            modid = new_uuid(),
        );
        root.write_atomic(
            Path::new(&format!("{name}.scrivx")),
            scrivx.as_bytes(),
            false,
        )?;
        root.write_atomic(Path::new("Files/Data/docs.checksum"), b"", false)?;
        Project::open_in(path.to_path_buf(), root)
    }

    pub fn name(&self) -> String {
        self.path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn has_unsaved_changes(&self) -> bool {
        self.scrivx_dirty || self.checksums_dirty || !self.reindex.is_empty()
    }

    // ------------------------------------------------------------ binder

    fn binder(&self) -> &Element {
        self.doc.root.child("Binder").expect("checked on open")
    }

    fn binder_mut(&mut self) -> &mut Element {
        self.doc.root.child_mut("Binder").expect("checked on open")
    }

    /// The whole binder as a tree.
    pub fn items(&self) -> Vec<Item> {
        note_binder_walk();
        fn read(container: &Element) -> Vec<Item> {
            container
                .elements()
                .filter(|e| e.name == "BinderItem")
                .map(|e| {
                    let meta = e.child("MetaData");
                    let meta_text = |name: &str| meta.and_then(|m| m.child_text(name));
                    Item {
                        uuid: canonical_id(&e.attr("UUID").unwrap_or_default()),
                        kind: Kind::from_attr(&e.attr("Type").unwrap_or_default()),
                        title: e.child_text("Title").unwrap_or_default(),
                        label: meta_text("LabelID").and_then(|s| s.trim().parse().ok()),
                        status: meta_text("StatusID").and_then(|s| s.trim().parse().ok()),
                        include_in_compile: meta_text("IncludeInCompile").as_deref() == Some("Yes"),
                        extension: meta_text("FileExtension"),
                        children: e.child("Children").map(read).unwrap_or_default(),
                    }
                })
                .collect()
        }
        read(self.binder())
    }

    pub fn item(&self, uuid: &str) -> Option<Item> {
        fn find(items: Vec<Item>, uuid: &str) -> Option<Item> {
            for item in items {
                if item.uuid.eq_ignore_ascii_case(uuid) {
                    return Some(item);
                }
                if let Some(found) = find(item.children, uuid) {
                    return Some(found);
                }
            }
            None
        }
        find(self.items(), uuid)
    }

    /// The root folder of `kind` (Draft, Research or Trash).
    pub fn root_folder(&self, kind: Kind) -> Option<Item> {
        self.items().into_iter().find(|i| i.kind == kind)
    }

    /// The chain of UUIDs from the top of the binder down to `uuid`.
    pub fn ancestors(&self, uuid: &str) -> Vec<String> {
        let Some(loc) = self.locate(uuid) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for n in 1..loc.len() {
            if let Some(e) = self.element_at(&loc[..n]) {
                out.push(canonical_id(&e.attr("UUID").unwrap_or_default()));
            }
        }
        out
    }

    pub fn is_in_trash(&self, uuid: &str) -> bool {
        let trash = self.root_folder(Kind::Trash).map(|t| t.uuid);
        self.ancestors(uuid)
            .iter()
            .any(|a| Some(a) == trash.as_ref())
    }

    pub fn is_in_draft(&self, uuid: &str) -> bool {
        let draft = self.root_folder(Kind::Draft).map(|t| t.uuid);
        self.ancestors(uuid)
            .iter()
            .any(|a| Some(a) == draft.as_ref())
    }

    fn locate(&self, uuid: &str) -> Option<Loc> {
        fn walk(container: &Element, uuid: &str, loc: &mut Loc) -> bool {
            for (i, e) in container
                .elements()
                .filter(|e| e.name == "BinderItem")
                .enumerate()
            {
                loc.push(i);
                if e.attr("UUID").is_some_and(|u| u.eq_ignore_ascii_case(uuid)) {
                    return true;
                }
                if let Some(c) = e.child("Children")
                    && walk(c, uuid, loc)
                {
                    return true;
                }
                loc.pop();
            }
            false
        }
        let mut loc = Vec::new();
        walk(self.binder(), uuid, &mut loc).then_some(loc)
    }

    fn element_at(&self, loc: &[usize]) -> Option<&Element> {
        let mut container = self.binder();
        let mut found = None;
        for (depth, &i) in loc.iter().enumerate() {
            let e = container
                .elements()
                .filter(|e| e.name == "BinderItem")
                .nth(i)?;
            found = Some(e);
            if depth + 1 < loc.len() {
                container = e.child("Children")?;
            }
        }
        found
    }

    fn element_at_mut(&mut self, loc: &[usize]) -> Option<&mut Element> {
        let mut container = self.binder_mut();
        let (last, path) = loc.split_last()?;
        for &i in path {
            let e = container
                .elements_mut()
                .filter(|e| e.name == "BinderItem")
                .nth(i)?;
            container = e.child_mut("Children")?;
        }
        container
            .elements_mut()
            .filter(|e| e.name == "BinderItem")
            .nth(*last)
    }

    fn element_mut(&mut self, uuid: &str) -> Result<&mut Element> {
        let loc = self
            .locate(uuid)
            .ok_or_else(|| Error::NoSuchItem(uuid.to_string()))?;
        Ok(self.element_at_mut(&loc).expect("just located"))
    }

    /// The whitespace before an item's tag at binder depth `depth` (0 is a
    /// top-level item).
    fn item_indent(depth: usize) -> String {
        format!("\n{}", "    ".repeat(2 + depth * 2))
    }

    fn touch(&mut self, uuid: &str) {
        if let Ok(e) = self.element_mut(uuid) {
            e.set_attr("Modified", &timestamp());
        }
        self.scrivx_dirty = true;
    }

    pub fn set_title(&mut self, uuid: &str, title: &str) -> Result<()> {
        let depth = self.locate(uuid).map_or(0, |l| l.len() - 1);
        let e = self.element_mut(uuid)?;
        e.set_child_text("Title", title, &Self::item_indent(depth));
        self.touch(uuid);
        self.reindex.insert(canonical_id(uuid), ());
        Ok(())
    }

    fn set_meta(&mut self, uuid: &str, name: &str, value: Option<&str>) -> Result<()> {
        let depth = self.locate(uuid).map_or(0, |l| l.len() - 1);
        let own = Self::item_indent(depth);
        let inner = format!("{own}    ");
        let e = self.element_mut(uuid)?;
        if e.child("MetaData").is_none() {
            // MetaData follows Title in Scrivener's files.
            let at = e
                .elements()
                .position(|c| c.name == "Title")
                .map_or(0, |i| i + 1);
            e.insert_element(at, Element::new("MetaData"), &own);
        }
        let meta = e
            .child_mut("MetaData")
            .expect("MetaData was inserted when it was missing");
        match value {
            Some(v) => meta.set_child_text(name, v, &inner),
            None => {
                meta.remove_child(name);
            }
        }
        self.touch(uuid);
        Ok(())
    }

    pub fn set_label(&mut self, uuid: &str, label: Option<i32>) -> Result<()> {
        let v = label.filter(|&l| l >= 0).map(|l| l.to_string());
        self.set_meta(uuid, "LabelID", v.as_deref())
    }

    pub fn set_status(&mut self, uuid: &str, status: Option<i32>) -> Result<()> {
        let v = status.filter(|&s| s >= 0).map(|s| s.to_string());
        self.set_meta(uuid, "StatusID", v.as_deref())
    }

    pub fn set_include_in_compile(&mut self, uuid: &str, include: bool) -> Result<()> {
        self.set_meta(uuid, "IncludeInCompile", include.then_some("Yes"))
    }

    fn tags(&self, settings: &str, list: &str) -> Vec<Tag> {
        let Some(s) = self.doc.root.child(settings).and_then(|s| s.child(list)) else {
            return Vec::new();
        };
        s.elements()
            .filter_map(|e| {
                let id = e.attr("ID")?.trim().parse().ok()?;
                let color = e.attr("Color").and_then(|c| {
                    let v: Vec<f32> = c
                        .split_whitespace()
                        .filter_map(|x| x.parse().ok())
                        .collect();
                    (v.len() >= 3).then(|| (v[0], v[1], v[2]))
                });
                Some(Tag {
                    id,
                    name: e.text(),
                    color,
                })
            })
            .collect()
    }

    pub fn labels(&self) -> Vec<Tag> {
        self.tags("LabelSettings", "Labels")
    }

    pub fn statuses(&self) -> Vec<Tag> {
        self.tags("StatusSettings", "StatusItems")
    }

    /// Adds a new item. With `after`, it goes right after that item (as its
    /// sibling); otherwise it's the last child of `parent`.
    pub fn add_item(
        &mut self,
        kind: Kind,
        title: &str,
        parent: &str,
        after: Option<&str>,
    ) -> Result<String> {
        if kind.is_root_folder() {
            return Err(Error::Invalid(
                "the binder already has its root folders".into(),
            ));
        }
        let uuid = new_uuid();
        let now = timestamp();
        let mut el = Element::new("BinderItem");
        el.set_attr("UUID", &uuid);
        el.set_attr("Type", kind.attr());
        el.set_attr("Created", &now);
        el.set_attr("Modified", &now);
        let mut t = Element::new("Title");
        t.set_text(title);
        el.children.push(xml::Node::Element(t));
        let in_draft =
            self.item(parent).is_some_and(|p| p.kind == Kind::Draft) || self.is_in_draft(parent);
        if in_draft {
            let mut meta = Element::new("MetaData");
            let mut inc = Element::new("IncludeInCompile");
            inc.set_text("Yes");
            meta.children.push(xml::Node::Element(inc));
            el.children.push(xml::Node::Element(meta));
        }
        let (parent_uuid, index) = match after {
            Some(sibling) => {
                let loc = self
                    .locate(sibling)
                    .ok_or_else(|| Error::NoSuchItem(sibling.to_string()))?;
                let parent = self.ancestors(sibling).last().cloned();
                let at = *loc.last().expect("a located binder item has an index");
                (parent, at + 1)
            }
            None => {
                let n = self.item(parent).map_or(0, |p| p.children.len());
                (Some(parent.to_string()), n)
            }
        };
        self.insert_element(parent_uuid.as_deref(), index, el)?;
        self.scrivx_dirty = true;
        self.reindex.insert(uuid.clone(), ());
        Ok(uuid)
    }

    /// Puts `el` at `index` among the children of `parent` (None: top level).
    fn insert_element(
        &mut self,
        parent: Option<&str>,
        index: usize,
        mut el: Element,
    ) -> Result<()> {
        let depth = match parent {
            Some(p) => self
                .locate(p)
                .ok_or_else(|| Error::NoSuchItem(p.to_string()))?
                .len(),
            None => 0,
        };
        let own = Self::item_indent(depth);
        // Lays the item out for its (possibly new) depth.
        xml::pretty(&mut el, &own);
        match parent {
            None => {
                let at = index.min(self.binder().elements().count());
                self.binder_mut().insert_element(at, el, "\n    ");
            }
            Some(p) => {
                let parent_own = Self::item_indent(depth - 1);
                let pe = self.element_mut(p)?;
                if pe.child("Children").is_none() {
                    let mut c = Element::new("Children");
                    c.empty = false;
                    let n = pe.elements().count();
                    pe.insert_element(n, c, &parent_own);
                }
                let children = pe
                    .child_mut("Children")
                    .expect("Children was inserted when it was missing");
                let at = index.min(children.elements().count());
                let children_own = format!("{parent_own}    ");
                children.insert_element(at, el, &children_own);
            }
        }
        Ok(())
    }

    /// Moves `uuid` to be child number `index` of `parent` (None: top
    /// level). Root folders stay put, and nothing moves inside itself.
    pub fn move_item(&mut self, uuid: &str, parent: Option<&str>, index: usize) -> Result<()> {
        let item = self
            .item(uuid)
            .ok_or_else(|| Error::NoSuchItem(uuid.to_string()))?;
        if item.kind.is_root_folder() {
            return Err(Error::Invalid(format!("{} can't be moved", item.title)));
        }
        if let Some(p) = parent {
            if p.eq_ignore_ascii_case(uuid)
                || self
                    .ancestors(p)
                    .iter()
                    .any(|a| a.eq_ignore_ascii_case(uuid))
            {
                return Err(Error::Invalid("can't move an item inside itself".into()));
            }
            let pk = self
                .item(p)
                .ok_or_else(|| Error::NoSuchItem(p.to_string()))?
                .kind;
            if matches!(pk, Kind::Image | Kind::Pdf | Kind::WebArchive | Kind::Other) {
                return Err(Error::Invalid("media items can't hold other items".into()));
            }
        }
        let loc = self.locate(uuid).ok_or_else(|| missing_item(uuid))?;
        let old_parent = self.ancestors(uuid).last().cloned();
        let mut index = index;
        // Removing first shifts later siblings of the same parent up by one.
        let same_parent = match (&old_parent, parent) {
            (None, None) => true,
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        };
        if same_parent && *loc.last().expect("a located binder item has an index") < index {
            index -= 1;
        }
        let el = self.detach(&loc)?;
        self.insert_element(parent, index, el)?;
        self.touch(uuid);
        Ok(())
    }

    fn detach(&mut self, loc: &[usize]) -> Result<Element> {
        let bad = || Error::Invalid("binder changed underneath".into());
        let (last, path) = loc.split_last().ok_or_else(bad)?;
        let container = if path.is_empty() {
            self.binder_mut()
        } else {
            self.element_at_mut(path)
                .ok_or_else(bad)?
                .child_mut("Children")
                .ok_or_else(bad)?
        };
        // `last` counts BinderItems; the container may hold other elements.
        let pos = container
            .elements()
            .enumerate()
            .filter(|(_, e)| e.name == "BinderItem")
            .nth(*last)
            .map(|(i, _)| i)
            .ok_or_else(bad)?;
        let el = container.remove_element(pos).ok_or_else(bad)?;
        if !path.is_empty() {
            let parent = self.element_at_mut(path).ok_or_else(bad)?;
            if parent
                .child("Children")
                .is_some_and(|c| c.elements().next().is_none())
            {
                parent.remove_child("Children");
            }
        }
        Ok(el)
    }

    /// Moves an item to the Trash folder, as Scrivener's Delete does.
    pub fn trash(&mut self, uuid: &str) -> Result<()> {
        let trash = self
            .root_folder(Kind::Trash)
            .ok_or_else(|| Error::Invalid("this project has no Trash folder".into()))?;
        let n = trash.children.len();
        self.move_item(uuid, Some(&trash.uuid), n)
    }

    /// Deletes everything in the Trash, files included.
    pub fn empty_trash(&mut self) -> Result<usize> {
        let Some(trash) = self.root_folder(Kind::Trash) else {
            return Ok(0);
        };
        fn all(items: &[Item], out: &mut Vec<String>) {
            for i in items {
                out.push(i.uuid.clone());
                all(&i.children, out);
            }
        }
        let mut doomed = Vec::new();
        all(&trash.children, &mut doomed);
        let uuid = trash.uuid.clone();
        let loc = self.locate(&uuid).ok_or_else(|| missing_item(&uuid))?;
        let t = self.element_at_mut(&loc).expect("just located");
        t.remove_child("Children");
        for uuid in &doomed {
            // A malformed item (no UUID, or "..") must never turn into
            // "delete Files/Data".
            if !is_uuid(uuid) {
                continue;
            }
            self.root.remove_tree(&data_rel(uuid))?;
            let prefix = format!("{}/", uuid.to_lowercase());
            self.checksums.retain(|(k, _)| !k.starts_with(&prefix));
            self.reindex.insert(canonical_id(uuid), ());
        }
        self.checksums_dirty = true;
        self.scrivx_dirty = true;
        Ok(doomed.len())
    }

    // ---------------------------------------------------------- content

    pub fn data_dir(&self, uuid: &str) -> PathBuf {
        self.path.join(data_rel(uuid))
    }

    /// The file behind a media item (`content.png`, `content.pdf` ...),
    /// opened through the project folder. The extension comes from the
    /// project's XML, so it must be a plain one: letters and digits only,
    /// no path tricks. `None` when the id or extension is not plain, or
    /// when the file is missing, a symlink, under a folder that is one, or
    /// not a regular file (a FIFO named like an image would block its
    /// reader).
    ///
    /// The result is an open file, not a path. Everything that shows or
    /// hands on the media reads this file, so a name swapped after the
    /// check changes nothing.
    pub fn media(&self, uuid: &str) -> Option<Media> {
        // An empty id is not a folder. Joining it would look at
        // `Files/Data/content.<ext>`, which is not a document.
        if !is_uuid(uuid) {
            return None;
        }
        let ext = self.item(uuid)?.extension?;
        if ext.is_empty() || ext.len() > 10 || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        let name = format!("content.{ext}");
        let file = self.root.open_file(&data_rel(uuid).join(&name)).ok()?;
        Some(Media {
            name,
            uuid: uuid.to_string(),
            file,
        })
    }

    fn read_rtf(&self, uuid: &str, file: &str) -> Result<RichText> {
        // A binder row is a document only when its id is a Scrivener UUID.
        // The files live in `Files/Data/<UUID>/`. An empty or missing id has
        // no folder; joining it would read `Files/Data/` itself.
        if !is_uuid(uuid) {
            return Ok(RichText::from_plain(""));
        }
        match self.root.read(&data_rel(uuid).join(file)) {
            Ok(bytes) => Ok(rtf::parse(&bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(RichText::from_plain("")),
            Err(e) => Err(e.into()),
        }
    }

    pub fn text(&self, uuid: &str) -> Result<RichText> {
        self.read_rtf(uuid, "content.rtf")
    }

    /// One parse of a document, then a clone for every later row with that
    /// id. The key is the id's [`canonical_id`] form, matching the
    /// `Files/Data` folder. [`text`] itself stays uncached, so a save cannot
    /// hand back this copy.
    pub fn text_shared(
        &self,
        uuid: &str,
        cache: &mut HashMap<String, RichText>,
    ) -> Result<RichText> {
        let key = canonical_id(uuid);
        if let Some(text) = cache.get(&key) {
            return Ok(text.clone());
        }
        let text = self.text(uuid)?;
        cache.insert(key, text.clone());
        Ok(text)
    }

    pub fn notes(&self, uuid: &str) -> Result<RichText> {
        self.read_rtf(uuid, "notes.rtf")
    }

    /// The formatting new text starts with: the project's own default
    /// (Project Settings in Scrivener) when it has one, otherwise that of
    /// the manuscript's first paragraph of text, so a new scene looks like
    /// the rest of the book.
    pub fn default_style(&self) -> Option<(rtf::ParaStyle, rtf::CharStyle)> {
        let prefs = self
            .root
            .read(Path::new("Settings/projectpreferences.xml"))
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .and_then(|t| xml::parse(&t).ok());
        if let Some(prefs) = prefs
            && prefs.root.child_text("UseProjectPreferences").as_deref() == Some("Yes")
            && let Some(hex) = prefs.root.child_text("TextFormatRTFData")
        {
            let hex = hex.trim();
            let bytes: Vec<u8> = (0..hex.len() / 2)
                .filter_map(|i| u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok())
                .collect();
            let t = rtf::parse(&bytes);
            if let Some(p) = t
                .paragraphs
                .iter()
                .find(|p| p.runs.iter().any(|r| !r.text.is_empty()))
            {
                return Some((p.style.clone(), dominant_char_style(p)));
            }
        }
        // A shared id is one file. Aliases of one short document are parsed
        // once. The search keeps going through later included documents
        // until it finds a paragraph long enough to copy, or the binder ends.
        fn search(
            p: &Project,
            items: &[Item],
            seen: &mut HashSet<String>,
        ) -> Option<(rtf::ParaStyle, rtf::CharStyle)> {
            for i in items.iter().filter(|i| i.include_in_compile) {
                if i.kind.has_text()
                    && is_uuid(&i.uuid)
                    && seen.insert(i.uuid.clone())
                    && let Ok(t) = p.text(&i.uuid)
                    && let Some(par) = t
                        .paragraphs
                        .iter()
                        .find(|par| paragraph_trimmed_chars(par) > 20)
                {
                    return Some((par.style.clone(), dominant_char_style(par)));
                }
                if let Some(found) = search(p, &i.children, seen) {
                    return Some(found);
                }
            }
            None
        }
        let mut seen = HashSet::new();
        search(self, &self.root_folder(Kind::Draft)?.children, &mut seen)
    }

    pub fn synopsis(&self, uuid: &str) -> String {
        if !is_uuid(uuid) {
            return String::new();
        }
        self.root
            .read(&data_rel(uuid).join("synopsis.txt"))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    }

    /// Word counts and the binder search text.
    ///
    /// Scrivener keeps a document's files in `Files/Data/<UUID>/`. Rows that
    /// share a UUID share that folder, and a row with no UUID has no folder.
    /// Each distinct UUID is read once and has one entry, and ids that
    /// differ only in case are one UUID. The entry follows the last row
    /// that used that id, which is what a per-row walk stored.
    pub fn binder_index(&self) -> BinderIndex {
        let mut docs: HashMap<String, IndexedDoc> = HashMap::new();
        let mut slots: HashMap<String, IndexSlot> = HashMap::new();
        index_items(self, &self.items(), &mut docs, &mut slots);
        let mut counts = HashMap::new();
        let mut search = HashMap::new();
        for (uuid, slot) in slots {
            let doc = docs.remove(&uuid);
            let synopsis = doc.as_ref().map(|d| d.synopsis.as_str()).unwrap_or("");
            let mut text = format!("{}\n{synopsis}\n", slot.title);
            if slot.has_text {
                text.push_str(doc.as_ref().map(|d| d.body.as_str()).unwrap_or(""));
            }
            if slot.saw_text {
                let body = doc.as_ref().map(|d| d.body.as_str()).unwrap_or("");
                counts.insert(uuid.clone(), rtf::word_count(body));
            }
            if doc.as_ref().is_none_or(|d| d.notes.is_some()) {
                text.push('\n');
                text.push_str(doc.as_ref().and_then(|d| d.notes.as_deref()).unwrap_or(""));
            }
            search.insert(uuid, text.to_lowercase());
        }
        note_retained(&docs);
        BinderIndex { counts, search }
    }

    /// Writes (or, for empty content, removes) one of an item's files, and
    /// keeps the checksum list in step.
    fn write_file(&mut self, uuid: &str, file: &str, bytes: Option<&[u8]>) -> Result<()> {
        if !is_uuid(uuid) {
            return Err(missing_item(uuid));
        }
        let rel = data_rel(uuid).join(file);
        let key = format!("{}/{file}", uuid.to_lowercase());
        match bytes {
            Some(bytes) => {
                self.root.write_atomic(&rel, bytes, true)?;
                #[cfg(test)]
                if file == "content.rtf" {
                    CONTENT_WRITES.with(|n| n.set(n.get() + 1));
                }
                let sum = sha1::hex(bytes);
                match self.checksums.iter_mut().find(|(k, _)| *k == key) {
                    Some(slot) => slot.1 = sum,
                    None => self.checksums.push((key, sum)),
                }
            }
            None => {
                self.root.remove_file(&rel)?;
                self.checksums.retain(|(k, _)| *k != key);
            }
        }
        self.checksums_dirty = true;
        self.reindex.insert(canonical_id(uuid), ());
        self.touch(uuid);
        Ok(())
    }

    pub fn set_text(&mut self, uuid: &str, text: &RichText) -> Result<()> {
        let rtf = rtf::write(text);
        self.write_file(uuid, "content.rtf", Some(rtf.as_bytes()))
    }

    pub fn set_notes(&mut self, uuid: &str, notes: &RichText) -> Result<()> {
        if notes.is_empty() {
            return self.write_file(uuid, "notes.rtf", None);
        }
        let rtf = rtf::write(notes);
        self.write_file(uuid, "notes.rtf", Some(rtf.as_bytes()))
    }

    pub fn set_synopsis(&mut self, uuid: &str, synopsis: &str) -> Result<()> {
        if synopsis.is_empty() {
            return self.write_file(uuid, "synopsis.txt", None);
        }
        self.write_file(uuid, "synopsis.txt", Some(synopsis.as_bytes()))
    }

    // ------------------------------------------------------------- save

    /// Writes the `.scrivx`, the checksum list and the search index, if
    /// anything changed. Text was already written by `set_text` and friends.
    pub fn save(&mut self) -> Result<()> {
        if self.scrivx_dirty {
            self.doc.root.set_attr("Modified", &timestamp());
            if self.doc.root.attr("ModID").is_some() {
                self.doc.root.set_attr("ModID", &new_uuid());
            }
            let xml = self.doc.to_xml();
            self.root
                .write_atomic(Path::new(&self.scrivx), xml.as_bytes(), false)?;
            self.scrivx_dirty = false;
        }
        if self.checksums_dirty {
            let mut out = String::new();
            for (k, v) in &self.checksums {
                out.push_str(&format!("{k}={v}\n"));
            }
            self.root
                .write_atomic(Path::new("Files/Data/docs.checksum"), out.as_bytes(), true)?;
            self.checksums_dirty = false;
        }
        if !self.reindex.is_empty() {
            self.save_search_index()?;
            self.reindex.clear();
        }
        Ok(())
    }

    fn save_search_index(&mut self) -> Result<()> {
        let path = Path::new("Files/search.indexes");
        let existing = self
            .root
            .read(path)
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned());
        let mut doc = match existing.as_deref().map(xml::parse) {
            Some(Ok(doc)) if doc.root.child("Documents").is_some() => doc,
            _ => xml::parse(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<SearchIndexes Version=\"1.0\">\n    <Documents></Documents>\n</SearchIndexes>\n",
            )?,
        };
        let uuids: Vec<String> = self.reindex.keys().cloned().collect();
        let dirty: HashSet<String> = uuids.iter().map(|uuid| uuid.to_ascii_uppercase()).collect();
        let docs = doc
            .root
            .child_mut("Documents")
            .expect("the search index has a Documents element");
        let children = std::mem::take(&mut docs.children);
        let mut kept = Vec::with_capacity(children.len());
        for node in children {
            note_index_visit(1);
            let drop_doc = match &node {
                xml::Node::Element(el) if el.name == "Document" => el
                    .attr("ID")
                    .is_some_and(|id| dirty.contains(&id.to_ascii_uppercase())),
                _ => false,
            };
            if drop_doc {
                if let Some(xml::Node::Text(text)) = kept.last()
                    && text.trim().is_empty()
                {
                    kept.pop();
                }
                continue;
            }
            kept.push(node);
        }
        if !kept
            .iter()
            .any(|node| matches!(node, xml::Node::Element(_)))
        {
            kept.retain(|node| !matches!(node, xml::Node::Text(text) if text.trim().is_empty()));
        }
        docs.children = kept;
        let mut rows = HashMap::new();
        index_rows(&self.items(), &mut rows);
        let mut entries = Vec::new();
        for uuid in uuids {
            let Some((title, has_text)) = rows.get(&uuid) else {
                continue; // deleted
            };
            let mut entry = Element::new("Document");
            entry.set_attr("ID", &uuid);
            let mut add = |name: &str, text: &str| {
                if !text.is_empty() {
                    let mut e = Element::new(name);
                    e.set_text(text);
                    entry.children.push(xml::Node::Element(e));
                }
            };
            add("Title", title);
            add("Synopsis", &self.synopsis(&uuid));
            if *has_text {
                add("Text", &self.text(&uuid)?.plain_text());
            }
            add("Notes", &self.notes(&uuid)?.plain_text());
            xml::pretty(&mut entry, "\n        ");
            entries.push(entry);
        }
        docs.append_elements(entries, "\n    ");
        self.root
            .write_atomic(path, doc.to_xml().as_bytes(), false)?;
        Ok(())
    }

    /// Zips the whole project into `dir`; see [`backup`]. The project is
    /// read through its open folder.
    pub fn backup(&self, dir: &Path, keep: usize) -> Result<PathBuf> {
        backup_folder(&self.root, dir, keep, &timestamp())
    }

    /// [`Project::backup`] with `now` in place of the clock.
    pub fn backup_at(&self, dir: &Path, keep: usize, now: &str) -> Result<PathBuf> {
        backup_folder(&self.root, dir, keep, now)
    }

    /// A second descriptor for the open project folder, for a backup on
    /// another thread ([`backup_folder`]).
    pub fn folder(&self) -> io::Result<Dir> {
        self.root.try_clone()
    }
}

/// Zips the project at `path` into its own folder under `dir`
/// (`<name>-<id>/<date>.zip`, the id from the project's full path, so two
/// projects never share or prune each other's backups), and keeps only the
/// newest `keep` there. `dir` and that folder are private to this user
/// ([`Dir::private_dir`]), each archive is made with mode `0o600` before
/// any byte is written, and archives already there are narrowed to
/// `0o600` ([`close_backups`]), so a backup never opens a project to other
/// users. The project folder is opened once; see [`backup_folder`].
pub fn backup(path: &Path, dir: &Path, keep: usize) -> Result<PathBuf> {
    backup_folder(&Dir::open(path)?, dir, keep, &timestamp())
}

/// [`backup`] with `now` in place of the clock. `now` has the shape
/// [`timestamp`] returns.
pub fn backup_at(path: &Path, dir: &Path, keep: usize, now: &str) -> Result<PathBuf> {
    backup_folder(&Dir::open(path)?, dir, keep, now)
}

/// [`backup_at`] for a project folder that is already open, as
/// [`Project::folder`] gives one. Every file is read through `root`. The
/// backup folder's name and id come from the path `root` was opened by;
/// that path is only hashed, never opened. A free function so it can run
/// on another thread while the project is open.
pub fn backup_folder(root: &Dir, dir: &Path, keep: usize, now: &str) -> Result<PathBuf> {
    let path = root.path();
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let full = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let id = &sha1::hex(full.to_string_lossy().as_bytes())[..8];
    let backups = dir
        .file_name()
        .ok_or_else(|| Error::Invalid(format!("{} names no folder", dir.display())))?;
    let parent = match dir.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(parent)?;
    let own = format!("{name}-{id}");
    let folder = Dir::open(parent)?
        .private_dir(backups)?
        .private_dir(OsStr::new(&own))?;
    let dir = dir.join(own);
    close_backups(&folder)?;
    let stamp = backup_stamp(now);
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = OsString::from(format!(
        ".backup.{}-{}.omaquill-tmp",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    if let Err(e) = write_backup(root, &folder, &tmp) {
        let _ = folder.remove_file(Path::new(&tmp));
        return Err(e);
    }
    let out = publish_backup(&folder, &stamp, &tmp)?;
    let mut old: Vec<String> = folder
        .entries()?
        .into_iter()
        .filter_map(|n| n.into_string().ok())
        .filter(|n| *n != out && is_backup_name(n))
        .collect();
    old.sort_by_key(|n| backup_order(n).expect("filtered to backup names"));
    while old.len() + 1 > keep.max(1) && !old.is_empty() {
        let _ = folder.remove_file(Path::new(&old.remove(0)));
    }
    Ok(dir.join(out))
}

/// Narrows each backup archive in `folder`, and each staging file
/// ([`is_staging_name`]) an earlier run left behind, to at most `0o600`,
/// so one an older omaquill left readable is closed too. Only a regular
/// file this user owns with one of those names is changed, through a
/// descriptor opened without following a symlink. A symlink, another kind
/// of entry, or a file this user may not open or change is left as it is.
/// The folder's owner stands for this user: [`Dir::private_dir`] has just
/// made it, narrowed it, or opened a folder with no group or other access,
/// which for a non-root user means this user owns it.
fn close_backups(folder: &Dir) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let skip = |e: &io::Error| {
        matches!(
            e.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidInput
        ) || fsdir::is_link_refusal(e)
    };
    let me = folder.owner()?;
    for name in folder.entries()? {
        if !name
            .to_str()
            .is_some_and(|n| is_backup_name(n) || is_staging_name(n))
        {
            continue;
        }
        let rel = Path::new(&name);
        if folder.kind(rel)? != Some(fsdir::Kind::File) {
            continue;
        }
        let file = match folder.open_file(rel) {
            Ok(file) => file,
            Err(e) if skip(&e) => continue,
            Err(e) => return Err(e.into()),
        };
        let meta = file.metadata()?;
        let mode = meta.permissions().mode() & 0o7777;
        if meta.uid() != me || mode & !0o600 == 0 {
            continue;
        }
        match file.set_permissions(std::fs::Permissions::from_mode(mode & 0o600)) {
            Ok(()) => {}
            Err(e) if skip(&e) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// `.backup.<pid>-<n>.omaquill-tmp`, the name [`backup_folder`] stages an
/// archive under.
fn is_staging_name(n: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    n.strip_prefix(".backup.")
        .and_then(|rest| rest.strip_suffix(".omaquill-tmp"))
        .and_then(|rest| rest.split_once('-'))
        .is_some_and(|(pid, count)| digits(pid) && digits(count))
}

/// `2026-09-28 19-05-02.zip` or `2026-09-28 19-05-02 2.zip`: only files
/// omaquill wrote are ever pruned.
fn is_backup_name(n: &str) -> bool {
    let Some(stem) = n.strip_suffix(".zip") else {
        return false;
    };
    let b = stem.as_bytes();
    if b.len() < 19 {
        return false;
    }
    let shape = b"dddd-dd-dd dd-dd-dd";
    let stamp_ok = b[..19].iter().zip(shape).all(|(c, s)| {
        if *s == b'd' {
            c.is_ascii_digit()
        } else {
            c == s
        }
    });
    stamp_ok
        && (b.len() == 19
            || (b[19] == b' ' && b[20..].iter().all(u8::is_ascii_digit) && b.len() > 20))
}

/// Zips the project open at `root` into the new file `tmp` in `folder`,
/// walking it through [`Dir`]: a file or folder that is a symlink is left
/// out, including one swapped in while the backup runs.
fn write_backup(root: &Dir, folder: &Dir, tmp: &OsStr) -> Result<()> {
    let file = open_backup_temp(folder, tmp)?;
    let mut zip = zip::ZipWriter::new(io::BufWriter::new(file));
    let top = root
        .path()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Gone, a symlink now, or a folder that is a file now: a save or a
    // second user changed it while the backup ran, and it is left out.
    let skip = |e: &io::Error| {
        e.kind() == io::ErrorKind::NotFound
            || e.raw_os_error() == Some(libc::ENOTDIR)
            || fsdir::is_link_refusal(e)
    };
    // The stack holds paths, not open folders: a project with thousands of
    // documents has thousands of folders waiting, more than a process may
    // hold open. Each one is walked again from `root` when its turn comes.
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let d = match root.walk(&rel, false) {
            Ok(d) => d,
            Err(e) if skip(&e) => continue,
            Err(e) => return Err(e.into()),
        };
        let mut names = d.entries()?;
        names.sort();
        for fname in names {
            let one = Path::new(&fname);
            match d.kind(one)? {
                Some(fsdir::Kind::Dir) => stack.push(rel.join(&fname)),
                Some(fsdir::Kind::File) => {
                    if fname.to_string_lossy().contains(".omaquill-tmp") {
                        continue; // a save in flight
                    }
                    let bytes = match d.read(one) {
                        Ok(b) => b,
                        // Renamed away mid-backup by a save, or swapped for a
                        // symlink: skip it.
                        Err(e) if skip(&e) => continue,
                        Err(e) => return Err(e.into()),
                    };
                    let entry = format!("{top}/{}", rel.join(&fname).to_string_lossy());
                    zip.add(&entry, &bytes)?;
                }
                _ => {}
            }
        }
    }
    use io::Write;
    let mut buffered = zip.finish()?;
    buffered.flush()?;
    buffered
        .into_inner()
        .map_err(|e| e.into_error())?
        .sync_all()?;
    Ok(())
}

/// Links `tmp` in `folder` to the next free backup name, removes `tmp`, and
/// returns the name.
fn publish_backup(folder: &Dir, stamp: &str, tmp: &OsStr) -> Result<String> {
    let linked = (|| {
        for _ in 0..10_000 {
            let out = next_backup_name(folder, stamp)?;
            match folder.link(tmp, OsStr::new(&out)) {
                Ok(()) => return Ok(out),
                Err(e) if already_exists(&e) => continue,
                Err(e) => return Err(Error::from(e)),
            }
        }
        Err(Error::Invalid(
            "could not choose a backup name that was not already there".into(),
        ))
    })();
    let _ = folder.remove_file(Path::new(tmp));
    if linked.is_ok() {
        let _ = folder.sync();
    }
    linked
}

fn backup_stamp(now: &str) -> String {
    let dashed = now.replace(':', "-");
    dashed.split(' ').take(2).collect::<Vec<_>>().join(" ")
}

fn backup_order(name: &str) -> Option<(String, u32)> {
    if !is_backup_name(name) {
        return None;
    }
    let stem = name.strip_suffix(".zip")?;
    let stamp = stem.get(..19)?.to_string();
    let count = stem
        .get(20..)
        .and_then(|rest| rest.parse().ok())
        .unwrap_or(1);
    Some((stamp, count))
}

fn next_backup_name(folder: &Dir, stamp: &str) -> Result<String> {
    let same_second = folder
        .entries()?
        .into_iter()
        .filter_map(|n| n.into_string().ok())
        .filter_map(|n| backup_order(&n))
        .filter(|(got, _)| *got == stamp)
        .map(|(_, count)| count)
        .max();
    Ok(match same_second {
        None => format!("{stamp}.zip"),
        Some(n) => format!("{stamp} {}.zip", n + 1),
    })
}

fn already_exists(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::AlreadyExists || e.raw_os_error() == Some(libc::EEXIST)
}

fn open_backup_temp(folder: &Dir, tmp: &OsStr) -> Result<std::fs::File> {
    match folder.create_private_new(tmp) {
        Ok(file) => Ok(file),
        Err(e) if already_exists(&e) => {
            folder.remove_file(Path::new(tmp))?;
            Ok(folder.create_private_new(tmp)?)
        }
        Err(e) => Err(e.into()),
    }
}

/// The `.scrivx` file's name in the project folder: `<stem>.scrivx` when
/// that is a regular file, or else the first `.scrivx` entry, as a renamed
/// project keeps its old one.
fn find_scrivx(root: &Dir, path: &Path) -> Option<OsString> {
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let named = OsString::from(format!("{stem}.scrivx"));
    if matches!(root.kind(Path::new(&named)), Ok(Some(fsdir::Kind::File))) {
        return Some(named);
    }
    root.entries()
        .ok()?
        .into_iter()
        .find(|n| Path::new(n).extension().is_some_and(|e| e == "scrivx"))
}

/// Where an item's files live, relative to the project folder.
fn data_rel(uuid: &str) -> PathBuf {
    Path::new("Files/Data").join(uuid.to_uppercase())
}

/// File types omaquill will hand to another app ("Open in Default App"):
/// documents and media only. A project can name its files anything, and
/// opening `content.desktop` or `content.sh` from someone's project could
/// run it.
const OPENABLE: &[&str] = &[
    "pdf", "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "heic", "avif", "mp3", "m4a",
    "aac", "wav", "aif", "aiff", "flac", "ogg", "opus", "mp4", "m4v", "mov", "webm", "mkv", "txt",
    "md",
];

/// Whether `path` is a type omaquill will open in another app.
pub fn is_openable(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| OPENABLE.contains(&e.to_ascii_lowercase().as_str()))
}

/// Scrivener's UUIDs: 36 characters of hex and dashes. Anything else is
/// never used to build a path that gets deleted.
pub fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// The one form of a binder id that omaquill holds after reading a project.
///
/// Scrivener writes UUIDs in uppercase and names each `Files/Data` folder
/// after the uppercase UUID, so ids that differ only in ASCII case name one
/// document. A UUID comes back uppercase. Any other id has no folder and is
/// returned unchanged.
pub fn canonical_id(id: &str) -> String {
    if is_uuid(id) {
        id.to_ascii_uppercase()
    } else {
        id.to_string()
    }
}

/// `docs.checksum` lines as `lowercase-uuid/file` → SHA-1, in file order.
///
/// Scrivener writes the id part in lowercase. A key whose id differs only in
/// case names the same file, so it joins the first line for that file and
/// the last value read for it is kept.
fn parse_checksums(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for (key, sum) in text.lines().filter_map(|l| l.split_once('=')) {
        let key = match key.split_once('/') {
            Some((id, file)) if is_uuid(id) => format!("{}/{file}", id.to_ascii_lowercase()),
            _ => key.to_string(),
        };
        match at.get(&key) {
            Some(&i) => out[i].1 = sum.to_string(),
            None => {
                at.insert(key.clone(), out.len());
                out.push((key, sum.to_string()));
            }
        }
    }
    out
}

/// A random (version 4) UUID in Scrivener's uppercase form.
pub fn new_uuid() -> String {
    use io::Read;
    let mut b = [0u8; 16];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .is_err()
    {
        // No /dev/urandom: mix the clock and a counter instead.
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        b[..8].copy_from_slice(&t.to_le_bytes());
        b[8..].copy_from_slice(&(n ^ t.rotate_left(17)).to_le_bytes());
    }
    b[6] = (b[6] & 0x0F) | 0x40;
    b[8] = (b[8] & 0x3F) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02X}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// Local time as Scrivener writes it: `2026-09-28 19:05:02 -0700`.
pub fn timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as libc::time_t;
    // SAFETY: `libc::tm` is a C struct of integers and one pointer. All-zero
    // is a valid value, including a null `tm_zone`.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r only writes the tm we hand it.
    unsafe { libc::localtime_r(&now, &mut tm) };
    let off = tm.tm_gmtoff / 60;
    let sign = if off < 0 { '-' } else { '+' };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {sign}{:02}{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        off.abs() / 60,
        off.abs() % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtf::RichText;
    use crate::xml::Node;
    use std::collections::HashSet;
    use std::path::Path;

    fn temp_project(name: &str) -> (std::path::PathBuf, Project) {
        let dir =
            std::env::temp_dir().join(format!("omaquill-index-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.scriv"));
        let project = Project::create(&path).unwrap();
        (path, project)
    }

    fn naive_index(project: &Project) -> BinderIndex {
        let mut counts = HashMap::new();
        let mut search = HashMap::new();
        fn walk(
            project: &Project,
            items: &[Item],
            counts: &mut HashMap<String, usize>,
            search: &mut HashMap<String, String>,
        ) {
            for item in items {
                let mut text = format!("{}\n{}\n", item.title, project.synopsis(&item.uuid));
                if item.kind.has_text() {
                    let body = project
                        .text(&item.uuid)
                        .map(|text| text.plain_text())
                        .unwrap_or_default();
                    counts.insert(item.uuid.clone(), rtf::word_count(&body));
                    text.push_str(&body);
                }
                if let Ok(notes) = project.notes(&item.uuid) {
                    text.push('\n');
                    text.push_str(&notes.plain_text());
                }
                search.insert(item.uuid.clone(), text.to_lowercase());
                walk(project, &item.children, counts, search);
            }
        }
        walk(project, &project.items(), &mut counts, &mut search);
        BinderIndex { counts, search }
    }

    fn rewrite_ids(path: &Path, aliases: &HashSet<String>, bare: &HashSet<String>, shared: &str) {
        let scrivx = std::fs::read_dir(path)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .find(|entry| entry.path().extension().is_some_and(|ext| ext == "scrivx"))
            .unwrap()
            .path();
        let mut doc = xml::parse(&std::fs::read_to_string(&scrivx).unwrap()).unwrap();
        fn walk(
            el: &mut xml::Element,
            aliases: &HashSet<String>,
            bare: &HashSet<String>,
            shared: &str,
        ) {
            if el.name == "BinderItem" {
                let uuid = el.attr("UUID").unwrap_or_default();
                if aliases.contains(&uuid) {
                    el.set_attr("UUID", shared);
                } else if bare.contains(&uuid) {
                    el.attrs.retain(|(key, _)| key != "UUID");
                }
            }
            for child in &mut el.children {
                if let Node::Element(el) = child {
                    walk(el, aliases, bare, shared);
                }
            }
        }
        walk(&mut doc.root, aliases, bare, shared);
        std::fs::write(scrivx, doc.to_xml()).unwrap();
    }

    fn item(uuid: &str, kind: Kind, children: Vec<Item>) -> Item {
        Item {
            uuid: uuid.to_string(),
            kind,
            title: String::new(),
            label: None,
            status: None,
            include_in_compile: true,
            extension: None,
            children,
        }
    }

    #[test]
    fn save_search_index_walks_each_list_once() {
        let (path, mut project) = temp_project("reindex");
        let draft = project.root_folder(Kind::Draft).unwrap().uuid;
        let mut ids = Vec::new();
        for n in 0..80 {
            let id = project
                .add_item(Kind::Text, &format!("Scene {n}"), &draft, None)
                .unwrap();
            project
                .set_text(&id, &RichText::from_plain(&format!("words {n}")))
                .unwrap();
            ids.push(id);
        }
        project.save().unwrap();
        reset_index_scan();
        for id in &ids {
            project
                .set_text(id, &RichText::from_plain("updated lamp"))
                .unwrap();
        }
        let started = std::time::Instant::now();
        project.save().unwrap();
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        let n = u64::try_from(ids.len()).unwrap();
        let walks = binder_walks();
        let visits = index_visits();
        assert!(
            walks <= 4,
            "saving {n} documents walked the binder {walks} times"
        );
        assert!(
            visits <= n.saturating_mul(8),
            "visited {visits} index rows for {n} documents"
        );
        let index = std::fs::read_to_string(path.join("Files/search.indexes")).unwrap();
        assert_eq!(index.matches("updated lamp").count(), ids.len());
        eprintln!("reindex docs={n} walks={walks} visits={visits} ms={ms:.1}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn save_search_index_inserts_each_document_once() {
        let (path, mut project) = temp_project("reindex-linear");
        let draft = project.root_folder(Kind::Draft).unwrap().uuid;
        let mut ids = Vec::new();
        for n in 0..80 {
            let id = project
                .add_item(Kind::Text, &format!("Scene {n}"), &draft, None)
                .unwrap();
            project
                .set_text(&id, &RichText::from_plain(&format!("words {n}")))
                .unwrap();
            ids.push(id);
        }
        project.save().unwrap();
        for id in &ids {
            project
                .set_text(id, &RichText::from_plain("updated lamp"))
                .unwrap();
        }
        xml::reset_insert_walks();
        project.save().unwrap();
        let n = u64::try_from(ids.len()).unwrap();
        let walks = xml::insert_walks();
        assert!(
            walks <= n.saturating_mul(4),
            "inserting {n} documents walked {walks} element-list steps"
        );
        let index = std::fs::read_to_string(path.join("Files/search.indexes")).unwrap();
        assert_eq!(index.matches("updated lamp").count(), ids.len());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn compile_words_counts_a_shared_uuid_once() {
        let shared = "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE";
        let other = "11111111-2222-3333-4444-555555555555";
        let mut counts = HashMap::new();
        counts.insert(shared.to_string(), 10);
        counts.insert(shared.to_ascii_lowercase(), 10);
        counts.insert(other.to_string(), 4);
        counts.insert("SKIP".to_string(), 1_000);
        let mut aliases = Vec::new();
        for _ in 0..1_000 {
            aliases.push(item(shared, Kind::Text, Vec::new()));
        }
        aliases.push(item(&shared.to_ascii_lowercase(), Kind::Text, Vec::new()));
        aliases.push(item(other, Kind::Text, Vec::new()));
        let mut skipped = item("SKIP", Kind::Text, Vec::new());
        skipped.include_in_compile = false;
        aliases.push(skipped);
        let draft = item(shared, Kind::Draft, aliases);
        let words = compile_words(&draft, &counts);
        assert_eq!(words, 14, "shared uuid counted {words}");
    }

    #[test]
    fn compile_words_skips_an_included_child_of_an_excluded_folder() {
        let child_id = "CCCCCCCC-DDDD-EEEE-FFFF-000000000001";
        let nested_id = "CCCCCCCC-DDDD-EEEE-FFFF-000000000002";
        let folder_id = "CCCCCCCC-DDDD-EEEE-FFFF-000000000003";
        let kept_id = "CCCCCCCC-DDDD-EEEE-FFFF-000000000004";
        let mut nested = item(nested_id, Kind::Text, Vec::new());
        nested.include_in_compile = true;
        let mut child = item(child_id, Kind::Text, vec![nested]);
        child.include_in_compile = true;
        let mut folder = item(folder_id, Kind::Folder, vec![child]);
        folder.include_in_compile = false;
        let kept = item(kept_id, Kind::Text, Vec::new());
        let draft = item("DRAFT", Kind::Draft, vec![folder, kept]);
        let mut counts = HashMap::new();
        counts.insert(child_id.to_string(), 40);
        counts.insert(nested_id.to_string(), 9);
        counts.insert(folder_id.to_string(), 7);
        counts.insert(kept_id.to_string(), 3);
        assert_eq!(compile_words(&draft, &counts), 3);
    }

    #[test]
    fn compile_words_starts_at_the_draft_children() {
        let draft_id = "DDDDDDDD-DDDD-DDDD-DDDD-DDDDDDDDDDDD";
        let child_id = "CCCCCCCC-DDDD-EEEE-FFFF-000000000011";
        let child = item(child_id, Kind::Text, Vec::new());
        let draft = item(draft_id, Kind::Draft, vec![child]);
        let mut counts = HashMap::new();
        counts.insert(draft_id.to_string(), 1_000);
        counts.insert(child_id.to_string(), 5);
        assert_eq!(compile_words(&draft, &counts), 5);
    }

    #[test]
    fn binder_index_releases_each_body() {
        let (path, mut project) = temp_project("release");
        let draft = project.root_folder(Kind::Draft).unwrap().uuid;
        let body = "lamp ".repeat(4_000);
        let mut ids = Vec::new();
        for n in 0..64 {
            let id = project
                .add_item(Kind::Text, &format!("Scene {n}"), &draft, None)
                .unwrap();
            project.set_text(&id, &RichText::from_plain(&body)).unwrap();
            ids.push(id);
        }
        let alias = project.add_item(Kind::Text, "Alias", &draft, None).unwrap();
        project.save().unwrap();
        let shared = ids[0].clone();
        let lower = shared.to_ascii_lowercase();
        let mut aliases = HashSet::new();
        aliases.insert(alias);
        rewrite_ids(&path, &aliases, &HashSet::new(), &lower);
        let project = Project::open(&path).unwrap();
        let indexed = project.binder_index();
        let kept = retained_index();
        let one = body.len();
        assert_eq!(
            kept,
            0,
            "kept {kept} body bytes after indexing {} documents of {one}",
            ids.len()
        );
        let upper = indexed.search.get(&shared).unwrap();
        assert!(upper.contains("lamp"), "{upper}");
        assert!(!indexed.search.contains_key(&lower));
        let words = rtf::word_count(&body);
        assert_eq!(indexed.counts.get(&shared).copied(), Some(words));
        assert!(!indexed.counts.contains_key(&lower));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn binder_index_matches_a_per_row_read_of_lighthouse() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/Lighthouse.scriv");
        let project = Project::open(&path).unwrap();
        let indexed = project.binder_index();
        let naive = naive_index(&project);
        assert_eq!(indexed.counts, naive.counts);
        assert_eq!(indexed.search, naive.search);
        assert!(indexed.search.values().any(|text| text.contains("storm")));
    }

    #[test]
    fn binder_index_reads_each_document_once() {
        let (distinct_path, mut project) = temp_project("distinct");
        let draft = project.root_folder(Kind::Draft).unwrap().uuid;
        let mut distinct = Vec::new();
        for n in 0..32 {
            let id = project
                .add_item(Kind::Text, &format!("Scene {n}"), &draft, None)
                .unwrap();
            project
                .set_text(&id, &RichText::from_plain(&format!("words of scene {n}")))
                .unwrap();
            distinct.push(id);
        }
        project.save().unwrap();
        let project = Project::open(&distinct_path).unwrap();
        rtf::reset_parses();
        let started = std::time::Instant::now();
        let indexed = project.binder_index();
        let distinct_ms = started.elapsed().as_secs_f64() * 1000.0;
        let distinct_parses = rtf::parses();
        assert_eq!(
            distinct_parses, 32,
            "32 documents parsed {distinct_parses} times"
        );
        assert_eq!(
            indexed.counts.get(&distinct[3]).copied(),
            Some(rtf::word_count("words of scene 3"))
        );

        let (path, mut project) = temp_project("aliases");
        let draft = project.root_folder(Kind::Draft).unwrap().uuid;
        let shared = project
            .add_item(Kind::Text, "The Lamp", &draft, None)
            .unwrap();
        project
            .set_text(&shared, &RichText::from_plain("The lamp stays lit."))
            .unwrap();
        let mut aliases = HashSet::new();
        for n in 0..400 {
            aliases.insert(
                project
                    .add_item(Kind::Text, &format!("Alias {n}"), &draft, None)
                    .unwrap(),
            );
        }
        let mut bare = HashSet::new();
        for n in 0..40 {
            bare.insert(
                project
                    .add_item(Kind::Text, &format!("Bare {n}"), &draft, None)
                    .unwrap(),
            );
        }
        project.save().unwrap();
        rewrite_ids(&path, &aliases, &bare, &shared);
        std::fs::write(
            path.join("Files/Data/content.rtf"),
            br"{\rtf1\ansi BAIT bait bait}",
        )
        .unwrap();
        let project = Project::open(&path).unwrap();
        rtf::reset_parses();
        let started = std::time::Instant::now();
        let indexed = project.binder_index();
        let alias_ms = started.elapsed().as_secs_f64() * 1000.0;
        let alias_parses = rtf::parses();
        let rows = 401 + 40;
        assert!(
            alias_parses <= 1,
            "{rows} rows parsed {alias_parses} times; one shared document"
        );
        assert!(
            indexed.search.values().all(|text| !text.contains("bait")),
            "an id-less row must not read Files/Data/content.rtf"
        );
        let found = indexed.search.get(&shared).unwrap();
        assert!(found.contains("the lamp stays lit"), "{found}");
        assert!(indexed.counts.get(&shared).copied().unwrap_or(0) > 0);
        eprintln!(
            "distinct docs=32 parses={distinct_parses} ms={distinct_ms:.1}; aliases={rows} parses={alias_parses} ms={alias_ms:.1}"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let _ = std::fs::remove_dir_all(distinct_path.parent().unwrap());
    }

    #[test]
    fn a_row_with_no_id_is_not_a_document() {
        let (path, mut project) = temp_project("noid");
        let err = project
            .set_text("", &RichText::from_plain("nope"))
            .unwrap_err();
        assert_eq!(err.to_string(), "binder item has no id");
        assert!(!path.join("Files/Data/content.rtf").exists());
        let err = project
            .set_notes("", &RichText::from_plain("nope"))
            .unwrap_err();
        assert_eq!(err.to_string(), "binder item has no id");
        let err = project.set_synopsis("", "nope").unwrap_err();
        assert_eq!(err.to_string(), "binder item has no id");
        std::fs::write(path.join("Files/Data/content.rtf"), br"{\rtf1 BAIT}").unwrap();
        std::fs::write(path.join("Files/Data/synopsis.txt"), b"BAIT synopsis").unwrap();
        rtf::reset_parses();
        assert_eq!(project.text("").unwrap().plain_text(), "");
        assert_eq!(project.notes("").unwrap().plain_text(), "");
        assert_eq!(project.synopsis(""), "");
        assert_eq!(rtf::parses(), 0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn default_style_reads_a_bounded_number_of_documents() {
        let (hit_path, mut project) = temp_project("style-hit");
        let draft = project.root_folder(Kind::Draft).unwrap().uuid;
        let scene = project.add_item(Kind::Text, "Scene", &draft, None).unwrap();
        project
            .set_text(
                &scene,
                &RichText::from_plain("This sentence is definitely longer than twenty characters."),
            )
            .unwrap();
        project.save().unwrap();
        let project = Project::open(&hit_path).unwrap();
        rtf::reset_parses();
        assert!(project.default_style().is_some());
        assert!(rtf::parses() <= 2);

        let (alias_path, mut project) = temp_project("style-alias");
        let draft = project.root_folder(Kind::Draft).unwrap().uuid;
        let shared = project.add_item(Kind::Text, "Short", &draft, None).unwrap();
        project
            .set_text(&shared, &RichText::from_plain("short words"))
            .unwrap();
        let mut aliases = HashSet::new();
        for n in 0..400 {
            aliases.insert(
                project
                    .add_item(Kind::Text, &format!("Alias {n}"), &draft, None)
                    .unwrap(),
            );
        }
        project.save().unwrap();
        rewrite_ids(&alias_path, &aliases, &HashSet::new(), &shared);
        let project = Project::open(&alias_path).unwrap();
        rtf::reset_parses();
        let started = std::time::Instant::now();
        assert!(project.default_style().is_none());
        let alias_ms = started.elapsed().as_secs_f64() * 1000.0;
        let alias_parses = rtf::parses();
        assert!(
            alias_parses <= 2,
            "400 aliases parsed the short file {alias_parses} times"
        );

        let (cap_path, mut project) = temp_project("style-cap");
        let draft = project.root_folder(Kind::Draft).unwrap().uuid;
        for n in 0..24 {
            let id = project
                .add_item(Kind::Text, &format!("Short {n}"), &draft, None)
                .unwrap();
            project
                .set_text(&id, &RichText::from_plain("short words"))
                .unwrap();
        }
        project.save().unwrap();
        let project = Project::open(&cap_path).unwrap();
        rtf::reset_parses();
        let started = std::time::Instant::now();
        assert!(project.default_style().is_none());
        let cap_ms = started.elapsed().as_secs_f64() * 1000.0;
        let cap_parses = rtf::parses();
        assert_eq!(
            cap_parses, 24,
            "24 short documents parsed {cap_parses} times"
        );
        eprintln!(
            "aliases parses={alias_parses} ms={alias_ms:.1}; distinct parses={cap_parses} ms={cap_ms:.1}"
        );
        let _ = std::fs::remove_dir_all(hit_path.parent().unwrap());
        let _ = std::fs::remove_dir_all(alias_path.parent().unwrap());
        let _ = std::fs::remove_dir_all(cap_path.parent().unwrap());
    }

    #[test]
    fn default_style_reads_past_short_included_documents() {
        let (path, mut project) = temp_project("style-after-short");
        let draft = project.root_folder(Kind::Draft).unwrap().uuid;
        for n in 0..8 {
            let id = project
                .add_item(Kind::Text, &format!("Short {n}"), &draft, None)
                .unwrap();
            project
                .set_text(&id, &RichText::from_plain("short words"))
                .unwrap();
        }
        let scene = project.add_item(Kind::Text, "Scene", &draft, None).unwrap();
        let mut paragraph = rtf::Paragraph::default();
        paragraph.style.align = rtf::Align::Center;
        paragraph.runs.push(rtf::Run {
            text: "This sentence is definitely longer than twenty characters.".into(),
            style: rtf::CharStyle {
                italic: true,
                ..rtf::CharStyle::default()
            },
        });
        project
            .set_text(
                &scene,
                &RichText {
                    paragraphs: vec![paragraph],
                    lossy: Vec::new(),
                },
            )
            .unwrap();
        project.save().unwrap();
        let project = Project::open(&path).unwrap();
        let style = project
            .default_style()
            .expect("a long paragraph after eight short documents");
        assert_eq!(style.0.align, rtf::Align::Center);
        assert!(style.1.italic);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn default_style_is_linear_in_distinct_runs() {
        let mut paragraph = rtf::Paragraph::default();
        for i in 0..100_000 {
            paragraph.runs.push(rtf::Run {
                text: "x".into(),
                style: rtf::CharStyle {
                    font: Some(format!("Face{i}")),
                    ..rtf::CharStyle::default()
                },
            });
        }
        let text = RichText {
            paragraphs: vec![paragraph],
            lossy: Vec::new(),
        };
        let (body_path, mut project) = temp_project("style-distinct-body");
        let draft = project.root_folder(Kind::Draft).unwrap().uuid;
        let scene = project.add_item(Kind::Text, "Runs", &draft, None).unwrap();
        project.set_text(&scene, &text).unwrap();
        project.save().unwrap();
        let project = Project::open(&body_path).unwrap();
        let started = std::time::Instant::now();
        let style = project
            .default_style()
            .expect("a 100,000-character paragraph");
        let body_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(style.1.font.as_deref(), Some("Face0"));

        let (prefs_path, project) = temp_project("style-distinct-prefs");
        let hex: String = rtf::write(&text)
            .bytes()
            .map(|b| format!("{b:02X}"))
            .collect();
        std::fs::create_dir_all(prefs_path.join("Settings")).unwrap();
        std::fs::write(
            prefs_path.join("Settings/projectpreferences.xml"),
            format!(
                "<Preferences><UseProjectPreferences>Yes</UseProjectPreferences>\
                 <TextFormatRTFData>{hex}</TextFormatRTFData></Preferences>"
            ),
        )
        .unwrap();
        let started = std::time::Instant::now();
        let style = project.default_style().expect("the preferences paragraph");
        let prefs_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(style.1.font.as_deref(), Some("Face0"));
        eprintln!("100,000 distinct runs: body ms={body_ms:.1}; preferences ms={prefs_ms:.1}");
        assert!(body_ms < 5_000.0 && prefs_ms < 5_000.0);
        let _ = std::fs::remove_dir_all(body_path.parent().unwrap());
        let _ = std::fs::remove_dir_all(prefs_path.parent().unwrap());
    }

    #[test]
    fn default_style_uses_the_paragraph_and_its_dominant_run() {
        let (runs_path, mut project) = temp_project("style-runs");
        let draft = project.root_folder(Kind::Draft).unwrap().uuid;
        let first = project.add_item(Kind::Text, "Runs", &draft, None).unwrap();
        let mut paragraph = rtf::Paragraph::default();
        paragraph.style.align = rtf::Align::Center;
        for i in 0..6 {
            paragraph.runs.push(rtf::Run {
                text: "abcdefghij".into(),
                style: rtf::CharStyle {
                    bold: i % 2 == 0,
                    italic: i % 2 == 1,
                    ..rtf::CharStyle::default()
                },
            });
        }
        project
            .set_text(
                &first,
                &RichText {
                    paragraphs: vec![paragraph],
                    lossy: Vec::new(),
                },
            )
            .unwrap();
        let later = project.add_item(Kind::Text, "Later", &draft, None).unwrap();
        let mut later_par = rtf::Paragraph::default();
        later_par.style.align = rtf::Align::Left;
        later_par.runs.push(rtf::Run {
            text: "This later sentence is longer than twenty characters.".into(),
            style: rtf::CharStyle::default(),
        });
        project
            .set_text(
                &later,
                &RichText {
                    paragraphs: vec![later_par],
                    lossy: Vec::new(),
                },
            )
            .unwrap();
        project.save().unwrap();
        let project = Project::open(&runs_path).unwrap();
        let style = project
            .default_style()
            .expect("six 10-character runs are one 60-character paragraph");
        assert_eq!(style.0.align, rtf::Align::Center);
        assert!(style.1.bold);
        assert!(!style.1.italic);

        let (lead_path, mut project) = temp_project("style-lead");
        let draft = project.root_folder(Kind::Draft).unwrap().uuid;
        let scene = project.add_item(Kind::Text, "Scene", &draft, None).unwrap();
        let mut paragraph = rtf::Paragraph::default();
        paragraph.style.align = rtf::Align::Right;
        paragraph.runs.push(rtf::Run {
            text: "Go ".into(),
            style: rtf::CharStyle {
                bold: true,
                ..rtf::CharStyle::default()
            },
        });
        paragraph.runs.push(rtf::Run {
            text: "This sentence is the body of the paragraph.".into(),
            style: rtf::CharStyle {
                italic: true,
                ..rtf::CharStyle::default()
            },
        });
        project
            .set_text(
                &scene,
                &RichText {
                    paragraphs: vec![paragraph],
                    lossy: Vec::new(),
                },
            )
            .unwrap();
        project.save().unwrap();
        let project = Project::open(&lead_path).unwrap();
        let style = project.default_style().expect("the lead-in paragraph");
        assert_eq!(style.0.align, rtf::Align::Right);
        assert!(style.1.italic);
        assert!(!style.1.bold);
        let _ = std::fs::remove_dir_all(runs_path.parent().unwrap());
        let _ = std::fs::remove_dir_all(lead_path.parent().unwrap());
    }

    #[test]
    fn media_refuses_an_empty_id() {
        let (path, mut project) = temp_project("media-empty");
        let research = project.root_folder(Kind::Research).unwrap().uuid;
        let id = project
            .add_item(Kind::Image, "Plate", &research, None)
            .unwrap();
        project.save().unwrap();
        let scrivx = std::fs::read_dir(&path)
            .unwrap()
            .find_map(|entry| {
                let path = entry.unwrap().path();
                path.extension()
                    .is_some_and(|ext| ext == "scrivx")
                    .then_some(path)
            })
            .unwrap();
        let mut doc = xml::parse(&std::fs::read_to_string(&scrivx).unwrap()).unwrap();
        fn strip(el: &mut xml::Element, id: &str) {
            if el.name == "BinderItem" && el.attr("UUID").as_deref() == Some(id) {
                el.attrs.retain(|(key, _)| key != "UUID");
                let mut meta = xml::Element::new("MetaData");
                let mut ext = xml::Element::new("FileExtension");
                ext.set_text("png");
                meta.children.push(Node::Element(ext));
                el.children.push(Node::Element(meta));
            }
            for child in &mut el.children {
                if let Node::Element(el) = child {
                    strip(el, id);
                }
            }
        }
        strip(&mut doc.root, &id);
        std::fs::write(&scrivx, doc.to_xml()).unwrap();
        let bait = path.join("Files/Data/content.png");
        std::fs::write(&bait, b"BAIT").unwrap();
        let project = Project::open(&path).unwrap();
        assert!(project.item("").is_some());
        assert!(project.media("").is_none());
        assert_eq!(std::fs::read(&bait).unwrap(), b"BAIT");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Gives the binder rows in `ids` the spellings in `to`, row for row,
    /// as text in the `.scrivx`, leaving every other byte alone.
    fn respell_ids(path: &Path, ids: &[String], to: &[String]) -> std::path::PathBuf {
        let scrivx = std::fs::read_dir(path)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .find(|entry| entry.path().extension().is_some_and(|ext| ext == "scrivx"))
            .unwrap()
            .path();
        let mut xml = std::fs::read_to_string(&scrivx).unwrap();
        for (from, to) in ids.iter().zip(to) {
            let from = format!("UUID=\"{from}\"");
            assert_eq!(xml.matches(&from).count(), 1, "{from}");
            xml = xml.replacen(&from, &format!("UUID=\"{to}\""), 1);
        }
        std::fs::write(&scrivx, xml).unwrap();
        scrivx
    }

    #[test]
    fn case_variant_ids_are_one_document() {
        let (path, mut project) = temp_project("case-variants");
        let draft = project.root_folder(Kind::Draft).unwrap().uuid;
        let doc = project.add_item(Kind::Text, "Lamp", &draft, None).unwrap();
        let body = "lamp ".repeat(20_000);
        project
            .set_text(&doc, &RichText::from_plain(&body))
            .unwrap();
        let mut rows = Vec::new();
        for n in 0..16 {
            rows.push(
                project
                    .add_item(Kind::Text, &format!("Alias {n}"), &draft, None)
                    .unwrap(),
            );
        }
        project.save().unwrap();
        drop(project);
        let fixed = "ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDEF".to_string();
        let data = path.join("Files/Data");
        std::fs::rename(data.join(&doc), data.join(&fixed)).unwrap();
        let spellings: Vec<String> = (0..16u32)
            .map(|n| {
                let mut letter = 0;
                fixed
                    .chars()
                    .map(|c| {
                        if !c.is_ascii_alphabetic() {
                            return c;
                        }
                        letter += 1;
                        if n >> (letter % 4) & 1 == 1 {
                            c.to_ascii_lowercase()
                        } else {
                            c
                        }
                    })
                    .collect()
            })
            .collect();
        let distinct: HashSet<&String> = spellings.iter().collect();
        assert_eq!(distinct.len(), spellings.len());
        let mut ids = vec![doc.clone()];
        ids.extend(rows.iter().cloned());
        let mut to = vec![fixed.clone()];
        to.extend(spellings.iter().cloned());
        let scrivx = respell_ids(&path, &ids, &to);
        let before = std::fs::read(&scrivx).unwrap();

        let mut project = Project::open(&path).unwrap();
        let draft_item = project.root_folder(Kind::Draft).unwrap();
        let respelled: Vec<&Item> = draft_item
            .children
            .iter()
            .filter(|i| i.uuid.eq_ignore_ascii_case(&fixed))
            .collect();
        assert_eq!(respelled.len(), 17);
        assert!(respelled.iter().all(|i| i.uuid == fixed));
        assert_eq!(project.item(&spellings[5]).unwrap().uuid, fixed);
        assert_eq!(
            project.ancestors(&spellings[3]),
            vec![draft_item.uuid.clone()]
        );

        let indexed = project.binder_index();
        let entries: Vec<&String> = indexed
            .search
            .keys()
            .filter(|id| id.eq_ignore_ascii_case(&fixed))
            .collect();
        assert_eq!(entries, vec![&fixed], "one search entry per document");
        let held: usize = indexed.search.values().map(String::len).sum();
        assert!(
            held < body.len() * 2,
            "search held {held} bytes for one {}-byte document",
            body.len()
        );
        assert_eq!(
            indexed
                .counts
                .keys()
                .filter(|id| id.eq_ignore_ascii_case(&fixed))
                .count(),
            1
        );
        assert_eq!(
            indexed.counts.get(&fixed).copied(),
            Some(rtf::word_count(&body))
        );
        assert_eq!(
            compile_words(&draft_item, &indexed.counts),
            rtf::word_count(&body)
        );

        assert_eq!(std::fs::read(&scrivx).unwrap(), before);
        project.set_title(&spellings[7], "Renamed").unwrap();
        project.save().unwrap();
        let after = std::fs::read_to_string(&scrivx).unwrap();
        for spelling in &spellings {
            let rows = if *spelling == fixed { 2 } else { 1 };
            assert_eq!(after.matches(&format!("UUID=\"{spelling}\"")).count(), rows);
        }
        let index = std::fs::read_to_string(path.join("Files/search.indexes")).unwrap();
        assert_eq!(
            index
                .to_ascii_uppercase()
                .matches(&format!("ID=\"{fixed}\""))
                .count(),
            1,
            "{index}"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_checksum_key_in_another_case_is_the_same_file() {
        let (path, mut project) = temp_project("checksum-case");
        let draft = project.root_folder(Kind::Draft).unwrap().uuid;
        let doc = project.add_item(Kind::Text, "Lamp", &draft, None).unwrap();
        project.save().unwrap();
        drop(project);
        let sums = path.join("Files/Data/docs.checksum");
        std::fs::write(
            &sums,
            format!("{doc}/content.rtf=old\nother/file=kept\n{doc}/content.rtf=older\n"),
        )
        .unwrap();
        let mut project = Project::open(&path).unwrap();
        project
            .set_text(&doc, &RichText::from_plain("lamp"))
            .unwrap();
        project.save().unwrap();
        let text = std::fs::read_to_string(&sums).unwrap();
        let lower = doc.to_ascii_lowercase();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(
            lines[0].starts_with(&format!("{lower}/content.rtf=")),
            "{text}"
        );
        assert!(!lines[0].ends_with("=old") && !lines[0].ends_with("=older"));
        assert_eq!(lines[1], "other/file=kept");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn canonical_id_uppercases_only_uuids() {
        assert_eq!(
            canonical_id("abcdefab-cdef-4abc-8def-abcdefabcdef"),
            "ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDEF"
        );
        assert_eq!(canonical_id("Draft"), "Draft");
        assert_eq!(canonical_id(""), "");
        assert_eq!(canonical_id("../x"), "../x");
    }
}

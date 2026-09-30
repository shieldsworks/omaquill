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

use crate::rtf::{self, RichText};
use crate::xml::{self, Element};
use crate::{sha1, zip};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

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

pub struct Project {
    pub path: PathBuf,
    /// `path` with every symlink resolved: nothing outside it is ever read,
    /// written or deleted (see [`Project::inside`]).
    root: PathBuf,
    scrivx: PathBuf,
    doc: xml::Document,
    /// `lowercase-uuid/file` → SHA-1, in file order.
    checksums: Vec<(String, String)>,
    /// Search-index entries to rewrite on the next save, by UUID.
    reindex: BTreeMap<String, ()>,
    scrivx_dirty: bool,
    checksums_dirty: bool,
}

/// Where a binder item sits: the path of item indices from the binder.
type Loc = Vec<usize>;

impl Project {
    pub fn open(path: &Path) -> Result<Project> {
        let path = path.to_path_buf();
        let root = std::fs::canonicalize(&path)?;
        let scrivx = find_scrivx(&path).ok_or_else(|| Error::NotAProject(path.clone()))?;
        let text = String::from_utf8_lossy(&read_inside(&root, &scrivx)?).into_owned();
        let doc = xml::parse(&text)?;
        if doc.root.name != "ScrivenerProject" || doc.root.child("Binder").is_none() {
            return Err(Error::NotAProject(path));
        }
        let checksums = read_inside(&root, &path.join("Files/Data/docs.checksum"))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
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
    /// `.scriv` and not exist yet).
    pub fn create(path: &Path) -> Result<Project> {
        if path.exists() {
            return Err(Error::Invalid(format!("{} already exists", path.display())));
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled")
            .to_string();
        std::fs::create_dir_all(path.join("Files/Data"))?;
        std::fs::create_dir_all(path.join("Settings"))?;
        std::fs::write(path.join("Files/version.txt"), "23")?;
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
        let file = path.join(format!("{name}.scrivx"));
        std::fs::write(&file, scrivx)?;
        std::fs::write(path.join("Files/Data/docs.checksum"), "")?;
        Project::open(path)
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
        fn read(container: &Element) -> Vec<Item> {
            container
                .elements()
                .filter(|e| e.name == "BinderItem")
                .map(|e| {
                    let meta = e.child("MetaData");
                    let meta_text = |name: &str| meta.and_then(|m| m.child_text(name));
                    Item {
                        uuid: e.attr("UUID").unwrap_or_default(),
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
                out.push(e.attr("UUID").unwrap_or_default());
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
        self.reindex.insert(uuid.to_uppercase(), ());
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
        let meta = e.child_mut("MetaData").unwrap();
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
                (parent, loc.last().unwrap() + 1)
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
                let children = pe.child_mut("Children").unwrap();
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
        let loc = self.locate(uuid).unwrap();
        let old_parent = self.ancestors(uuid).last().cloned();
        let mut index = index;
        // Removing first shifts later siblings of the same parent up by one.
        let same_parent = match (&old_parent, parent) {
            (None, None) => true,
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        };
        if same_parent && *loc.last().unwrap() < index {
            index -= 1;
        }
        let el = self.detach(&loc)?;
        self.insert_element(parent, index, el)?;
        self.touch(uuid);
        Ok(())
    }

    fn detach(&mut self, loc: &[usize]) -> Result<Element> {
        let (last, path) = loc.split_last().unwrap();
        let bad = || Error::Invalid("binder changed underneath".into());
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
        let loc = self.locate(&trash.uuid).unwrap();
        let t = self.element_at_mut(&loc).unwrap();
        t.remove_child("Children");
        for uuid in &doomed {
            // A malformed item (no UUID, or "..") must never turn into
            // "delete Files/Data".
            if !is_uuid(uuid) {
                continue;
            }
            remove_dir_inside(&self.root, &self.data_dir(uuid))?;
            let prefix = format!("{}/", uuid.to_lowercase());
            self.checksums.retain(|(k, _)| !k.starts_with(&prefix));
            self.reindex.insert(uuid.to_uppercase(), ());
        }
        self.checksums_dirty = true;
        self.scrivx_dirty = true;
        Ok(doomed.len())
    }

    // ---------------------------------------------------------- content

    pub fn data_dir(&self, uuid: &str) -> PathBuf {
        self.path.join("Files/Data").join(uuid.to_uppercase())
    }

    /// The file behind a media item (`content.png`, `content.pdf` ...).
    pub fn media_path(&self, uuid: &str) -> Option<PathBuf> {
        let ext = self.item(uuid)?.extension?;
        let p = self.data_dir(uuid).join(format!("content.{ext}"));
        (p.exists() && inside(&self.root, &p)).then_some(p)
    }

    fn read_rtf(&self, uuid: &str, file: &str) -> Result<RichText> {
        match read_inside(&self.root, &self.data_dir(uuid).join(file)) {
            Ok(bytes) => Ok(rtf::parse(&bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(RichText::from_plain("")),
            Err(e) => Err(e.into()),
        }
    }

    pub fn text(&self, uuid: &str) -> Result<RichText> {
        self.read_rtf(uuid, "content.rtf")
    }

    pub fn notes(&self, uuid: &str) -> Result<RichText> {
        self.read_rtf(uuid, "notes.rtf")
    }

    /// The formatting new text starts with: the project's own default
    /// (Project Settings in Scrivener) when it has one, otherwise that of
    /// the manuscript's first paragraph of text, so a new scene looks like
    /// the rest of the book.
    pub fn default_style(&self) -> Option<(rtf::ParaStyle, rtf::CharStyle)> {
        let prefs = read_inside(
            &self.root,
            &self.path.join("Settings/projectpreferences.xml"),
        )
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
            if let Some(p) = t.paragraphs.iter().find(|p| !p.runs.is_empty()) {
                return Some((p.style.clone(), p.runs[0].style.clone()));
            }
        }
        fn first(p: &Project, items: &[Item]) -> Option<(rtf::ParaStyle, rtf::CharStyle)> {
            for i in items.iter().filter(|i| i.include_in_compile) {
                if i.kind.has_text()
                    && let Ok(t) = p.text(&i.uuid)
                    && let Some(par) = t
                        .paragraphs
                        .iter()
                        .find(|par| par.runs.iter().any(|r| r.text.trim().len() > 20))
                {
                    return Some((par.style.clone(), par.runs[0].style.clone()));
                }
                if let Some(found) = first(p, &i.children) {
                    return Some(found);
                }
            }
            None
        }
        first(self, &self.root_folder(Kind::Draft)?.children)
    }

    pub fn synopsis(&self, uuid: &str) -> String {
        read_inside(&self.root, &self.data_dir(uuid).join("synopsis.txt"))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    }

    /// Writes (or, for empty content, removes) one of an item's files, and
    /// keeps the checksum list in step.
    fn write_file(&mut self, uuid: &str, file: &str, bytes: Option<&[u8]>) -> Result<()> {
        let dir = self.data_dir(uuid);
        let path = dir.join(file);
        let key = format!("{}/{file}", uuid.to_lowercase());
        match bytes {
            Some(bytes) => {
                create_dir_inside(&self.root, &dir)?;
                write_inside(&self.root, &path, bytes)?;
                let sum = sha1::hex(bytes);
                match self.checksums.iter_mut().find(|(k, _)| *k == key) {
                    Some(slot) => slot.1 = sum,
                    None => self.checksums.push((key, sum)),
                }
            }
            None => {
                remove_file_inside(&self.root, &path)?;
                self.checksums.retain(|(k, _)| *k != key);
            }
        }
        self.checksums_dirty = true;
        self.reindex.insert(uuid.to_uppercase(), ());
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
            write_inside(&self.root, &self.scrivx, self.doc.to_xml().as_bytes())?;
            self.scrivx_dirty = false;
        }
        if self.checksums_dirty {
            let mut out = String::new();
            for (k, v) in &self.checksums {
                out.push_str(&format!("{k}={v}\n"));
            }
            let dir = self.path.join("Files/Data");
            create_dir_inside(&self.root, &dir)?;
            write_inside(&self.root, &dir.join("docs.checksum"), out.as_bytes())?;
            self.checksums_dirty = false;
        }
        if !self.reindex.is_empty() {
            self.save_search_index()?;
            self.reindex.clear();
        }
        Ok(())
    }

    fn save_search_index(&mut self) -> Result<()> {
        let path = self.path.join("Files/search.indexes");
        let existing = read_inside(&self.root, &path)
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned());
        let mut doc = match existing.as_deref().map(xml::parse) {
            Some(Ok(doc)) if doc.root.child("Documents").is_some() => doc,
            _ => xml::parse(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<SearchIndexes Version=\"1.0\">\n    <Documents></Documents>\n</SearchIndexes>\n",
            )?,
        };
        let uuids: Vec<String> = self.reindex.keys().cloned().collect();
        for uuid in uuids {
            let docs = doc.root.child_mut("Documents").unwrap();
            let pos = docs.elements().position(|e| {
                e.attr("ID")
                    .is_some_and(|id| id.eq_ignore_ascii_case(&uuid))
            });
            if let Some(pos) = pos {
                docs.remove_element(pos);
            }
            let Some(item) = self.item(&uuid) else {
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
            add("Title", &item.title);
            add("Synopsis", &self.synopsis(&uuid));
            if item.kind.has_text() {
                add("Text", &self.text(&uuid)?.plain_text());
            }
            add("Notes", &self.notes(&uuid)?.plain_text());
            xml::pretty(&mut entry, "\n        ");
            let docs = doc.root.child_mut("Documents").unwrap();
            let n = docs.elements().count();
            docs.insert_element(n, entry, "\n    ");
        }
        write_inside(&self.root, &path, doc.to_xml().as_bytes())?;
        Ok(())
    }

    /// Zips the whole project into `dir`; see [`backup`].
    pub fn backup(&self, dir: &Path, keep: usize) -> Result<PathBuf> {
        backup(&self.path, dir, keep)
    }
}

/// Zips the project at `path` into its own folder under `dir`
/// (`<name>-<id>/<date>.zip`, the id from the project's full path, so two
/// projects never share or prune each other's backups), and keeps only the
/// newest `keep` there. A free function so it can run on another thread
/// while the project is open.
pub fn backup(path: &Path, dir: &Path, keep: usize) -> Result<PathBuf> {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let full = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let id = &sha1::hex(full.to_string_lossy().as_bytes())[..8];
    let dir = dir.join(format!("{name}-{id}"));
    std::fs::create_dir_all(&dir)?;
    let stamp = timestamp().replace(':', "-");
    let stamp = stamp.split(' ').take(2).collect::<Vec<_>>().join(" ");
    // Two backups in the same second get " 2", " 3" ... instead of
    // replacing each other.
    // Numbered past any backup already made this second, so names keep
    // sorting oldest first even after pruning frees a lower number.
    let same_second = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| is_backup_name(n) && n.starts_with(&stamp))
        .map(|n| {
            n[stamp.len()..]
                .trim_end_matches(".zip")
                .trim()
                .parse::<u32>()
                .unwrap_or(1)
        })
        .max();
    let out = match same_second {
        None => dir.join(format!("{stamp}.zip")),
        Some(n) => dir.join(format!("{stamp} {}.zip", n + 1)),
    };
    let tmp = out.with_extension("zip.part");
    let result = write_backup(path, &tmp);
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, &out)?;
    let mut old: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            *p != out
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(is_backup_name)
        })
        .collect();
    // Oldest first: by stamp, then " 2", " 3" within the same second.
    old.sort_by_key(|p| {
        let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let stem = n.strip_suffix(".zip").unwrap_or(n);
        let count: u32 = stem.get(20..).and_then(|c| c.parse().ok()).unwrap_or(1);
        (stem.get(..19).unwrap_or("").to_string(), count)
    });
    while old.len() + 1 > keep.max(1) && !old.is_empty() {
        let _ = std::fs::remove_file(old.remove(0));
    }
    Ok(out)
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

fn write_backup(path: &Path, tmp: &Path) -> Result<()> {
    let file = std::fs::File::create(tmp)?;
    let mut zip = zip::ZipWriter::new(io::BufWriter::new(file));
    let top = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut stack = vec![path.to_path_buf()];
    while let Some(d) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&d)?.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            let ft = e.file_type()?;
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() {
                let fname = e.file_name();
                if fname.to_string_lossy().contains(".omaquill-tmp") {
                    continue; // a save in flight
                }
                let bytes = match std::fs::read(&p) {
                    Ok(b) => b,
                    // Renamed away mid-backup by a save: skip it.
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e.into()),
                };
                let rel = p.strip_prefix(path).unwrap();
                let entry = format!("{top}/{}", rel.to_string_lossy());
                zip.add(&entry, &bytes)?;
            }
        }
    }
    use io::Write;
    zip.finish()?.flush()?;
    Ok(())
}

fn find_scrivx(dir: &Path) -> Option<PathBuf> {
    if dir.is_file() && dir.extension().is_some_and(|e| e == "scrivx") {
        return Some(dir.to_path_buf());
    }
    let stem = dir.file_stem()?.to_string_lossy().into_owned();
    let named = dir.join(format!("{stem}.scrivx"));
    if named.is_file() {
        return Some(named);
    }
    // Renamed projects keep their old .scrivx name.
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|e| e == "scrivx"))
}

// ------------------------------------------------------------ containment
//
// A `.scriv` folder can come from anyone. Its files could be symlinks (or
// sit under a symlinked folder) pointing at the user's other files, and
// editing a chapter must never overwrite `~/.bashrc`. So every read, write
// and delete resolves the path first and acts only if it lands inside the
// project's own (resolved) folder. Links that stay inside still work.

/// Resolves `p` through every symlink that exists, keeping the parts that
/// don't exist yet (a file about to be created).
fn resolve(p: &Path) -> Option<PathBuf> {
    let mut existing = p;
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = std::fs::canonicalize(existing) {
            let mut out = real;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return Some(out);
        }
        rest.push(existing.file_name()?.to_os_string());
        existing = existing.parent()?;
    }
}

/// `p` resolves to somewhere inside `root`.
fn inside(root: &Path, p: &Path) -> bool {
    resolve(p).is_some_and(|r| r.starts_with(root))
}

fn outside_error(p: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "{} links outside the project; omaquill won't follow it",
            p.display()
        ),
    )
}

fn read_inside(root: &Path, p: &Path) -> io::Result<Vec<u8>> {
    if !p.exists() {
        return Err(io::ErrorKind::NotFound.into());
    }
    if !inside(root, p) {
        return Err(outside_error(p));
    }
    std::fs::read(p)
}

/// Writes `p` if its folder is inside the project. A file that's a symlink
/// out of the project is replaced by an ordinary file (the link goes, what
/// it pointed at is left alone).
fn write_inside(root: &Path, p: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = p.parent().unwrap_or(p);
    if !inside(root, parent) {
        return Err(outside_error(p));
    }
    let is_link = std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink());
    if is_link && !std::fs::canonicalize(p).is_ok_and(|t| t.starts_with(root)) {
        std::fs::remove_file(p)?; // the link itself
    }
    write_atomic(p, bytes)
}

fn create_dir_inside(root: &Path, dir: &Path) -> io::Result<()> {
    if !inside(root, dir) {
        return Err(outside_error(dir));
    }
    std::fs::create_dir_all(dir)
}

fn remove_file_inside(root: &Path, p: &Path) -> io::Result<()> {
    let is_link = std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink());
    if !p.exists() && !is_link {
        return Ok(());
    }
    // Removing a link removes only the link, but its folder must be ours.
    if !inside(root, p.parent().unwrap_or(p)) {
        return Err(outside_error(p));
    }
    std::fs::remove_file(p)
}

/// Deletes an item's folder. A folder that's a symlink loses just the
/// link; one that resolves outside the project is left alone.
fn remove_dir_inside(root: &Path, dir: &Path) -> io::Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return Ok(());
    };
    if !inside(root, dir.parent().unwrap_or(dir)) {
        return Err(outside_error(dir));
    }
    if meta.file_type().is_symlink() {
        return std::fs::remove_file(dir);
    }
    if !inside(root, dir) {
        return Err(outside_error(dir));
    }
    std::fs::remove_dir_all(dir)
}

/// Writes through a temporary file and a rename, so a crash never leaves a
/// half-written chapter. Keeps the file's permissions, writes through a
/// symlink to its target, and syncs the folder so the rename survives a
/// power cut.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use io::Write;
    use std::os::unix::fs::PermissionsExt;
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let target = match std::fs::canonicalize(path) {
        Ok(real) => real,
        Err(_) => path.to_path_buf(), // new file
    };
    let mode = std::fs::metadata(&target)
        .ok()
        .map(|m| m.permissions().mode());
    let tmp = target.with_file_name(format!(
        ".{}.{}-{}.omaquill-tmp",
        target.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let written = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        if let Some(mode) = mode {
            f.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &target)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return written;
    }
    if let Some(dir) = target.parent()
        && let Ok(d) = std::fs::File::open(dir)
    {
        let _ = d.sync_all();
    }
    Ok(())
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

//! Compile tests on `tests/fixtures/Lighthouse.scriv` (Draft > "Chapter
//! One" folder > "The Storm", "Morning & After"). The fixture is only read;
//! tests that change the binder work on a temporary copy.
//!
//! DOCX and EPUB are checked structurally with python3 (zipfile and
//! xml.etree parse every XML part), and with LibreOffice and epubcheck too
//! when those are installed.

use omaquill::compile::{self, Format, Headings, Options};
use omaquill::project::{Kind, Project};
use omaquill::rtf::{self, RichText};
use std::path::{Path, PathBuf};
use std::process::Command;

const MORNING: &str = "4D9C3A5E-6B7F-4A8C-ADBE-2F3A4B5C6D04";
const CHAPTER: &str = "2B7A1E3C-4F5D-4E6A-8B9C-0D1E2F3A4B02";
const DRAFT: &str = "1D0E6B55-1E50-4C0B-9D47-3A0B6F6C1A01";

fn fixture() -> Project {
    Project::open(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/Lighthouse.scriv"))
        .unwrap()
}

/// A scratch directory, removed on drop.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(tag: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("omaquill-compile-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), to).unwrap();
        }
    }
}

/// A writable copy of the fixture inside `dir`.
fn fixture_copy(dir: &Scratch) -> Project {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/Lighthouse.scriv");
    let dst = dir.0.join("Lighthouse.scriv");
    copy_dir(&src, &dst);
    Project::open(&dst).unwrap()
}

fn opts(p: &Project) -> Options {
    Options {
        author: "Ada Q. Writer".to_string(),
        ..Options::for_project(p)
    }
}

fn have(tool: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {tool}"))
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Opens `file` as a zip in python3, parses every XML part and prints
/// `name<TAB>compress_type` per entry (in order), then the text of the
/// parts named in `dump`. None when python3 isn't installed.
fn inspect_zip(file: &Path, dump: &[&str]) -> Option<String> {
    if !have("python3") {
        eprintln!("python3 not installed; skipping zip/XML validation");
        return None;
    }
    const SCRIPT: &str = r#"
import sys, zipfile, xml.etree.ElementTree as ET
z = zipfile.ZipFile(sys.argv[1])
assert z.testzip() is None
for info in z.infolist():
    print(f"{info.filename}\t{info.compress_type}")
    if info.filename.endswith((".xml", ".rels", ".xhtml", ".opf")):
        ET.fromstring(z.read(info.filename))
for name in sys.argv[2:]:
    root = ET.fromstring(z.read(name))
    print("TEXT " + name + ": " + " ".join(t.strip() for t in root.itertext() if t.strip()))
"#;
    let out = Command::new("python3")
        .arg("-c")
        .arg(SCRIPT)
        .arg(file)
        .args(dump)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "python3 rejected {}:\n{}",
        file.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).unwrap())
}

/// Adds a text document under `parent` with `text` as its content.
fn add_text(p: &mut Project, parent: &str, title: &str, text: &str) -> String {
    let uuid = p.add_item(Kind::Text, title, parent, None).unwrap();
    p.set_text(&uuid, &RichText::from_plain(text)).unwrap();
    uuid
}

const TRICKY: &str = "Tom & Jerry said \"<hi>\" and 'bye' *twice* [ok] #1";

// ---------------------------------------------------------------- gather

#[test]
fn gathers_chapters_and_scenes() {
    let p = fixture();
    let o = opts(&p);
    assert_eq!(o.title, "Lighthouse");
    assert!(o.manuscript);
    assert_eq!(o.scene_separator, "#");
    let sections = compile::gather(&p, &o).unwrap();
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].heading.as_deref(), Some("Chapter One"));
    assert_eq!(sections[0].scenes.len(), 2);
    let storm = sections[0].scenes[0].plain_text();
    assert!(
        storm.starts_with("The storm came in off the point"),
        "{storm}"
    );
    assert!(
        sections[0].scenes[0]
            .paragraphs
            .iter()
            .flat_map(|p| &p.runs)
            .any(|r| r.style.italic && r.text.contains("Twelve"))
    );
    assert!(sections[0].scenes[1].plain_text().starts_with("By morning"));

    let words: usize = sections[0]
        .scenes
        .iter()
        .map(|s| rtf::word_count(&s.plain_text()))
        .sum();
    assert_eq!(compile::word_count(&sections), words);
    assert!(words > 30);
}

#[test]
fn heading_styles() {
    let p = fixture();
    let mut o = opts(&p);
    o.headings = Headings::Titles;
    assert_eq!(
        compile::gather(&p, &o).unwrap()[0].heading.as_deref(),
        Some("Chapter One")
    );
    o.headings = Headings::None;
    assert_eq!(compile::gather(&p, &o).unwrap()[0].heading, None);
}

#[test]
fn numbers_chapters_and_flattens_nested_folders() {
    let dir = scratch("numbers");
    let mut p = fixture_copy(&dir);
    // A text document directly in Draft is a chapter of one scene.
    add_text(&mut p, DRAFT, "Interlude", "The lamp burned all night.");
    // A folder with its own text, a nested folder and blank documents.
    let three = p.add_item(Kind::Folder, "Third", DRAFT, None).unwrap();
    p.set_text(&three, &RichText::from_plain("Folder text first."))
        .unwrap();
    let nested = p.add_item(Kind::Folder, "Nested", &three, None).unwrap();
    add_text(&mut p, &nested, "Deep", "\n\nFrom deep inside.\n\n");
    add_text(&mut p, &three, "Blank", "  \n\t\n");
    add_text(&mut p, &three, "Last", "After the nest.");

    let mut o = opts(&p);
    let sections = compile::gather(&p, &o).unwrap();
    let headings: Vec<_> = sections
        .iter()
        .map(|s| s.heading.clone().unwrap())
        .collect();
    assert_eq!(headings, ["Chapter One", "Chapter Two", "Chapter Three"]);
    assert_eq!(sections[1].scenes.len(), 1);
    let third: Vec<_> = sections[2].scenes.iter().map(|s| s.plain_text()).collect();
    // Blank lines around a scene are trimmed; blank scenes are dropped.
    assert_eq!(
        third,
        ["Folder text first.", "From deep inside.", "After the nest."]
    );

    o.headings = Headings::Titles;
    let titles: Vec<_> = compile::gather(&p, &o)
        .unwrap()
        .into_iter()
        .map(|s| s.heading.unwrap())
        .collect();
    assert_eq!(titles, ["Chapter One", "Interlude", "Third"]);

    for (n, words) in [
        (1, "One"),
        (11, "Eleven"),
        (21, "Twenty-One"),
        (99, "Ninety-Nine"),
        (100, "100"),
    ] {
        assert_eq!(compile::number_words(n), words);
    }
}

#[test]
fn skips_items_left_out_of_compile() {
    let dir = scratch("skip");
    let mut p = fixture_copy(&dir);
    p.set_include_in_compile(MORNING, false).unwrap();
    let sections = compile::gather(&p, &opts(&p)).unwrap();
    assert_eq!(sections[0].scenes.len(), 1);
    assert!(sections[0].scenes[0].plain_text().starts_with("The storm"));

    // Leaving out a folder leaves out everything in it.
    p.set_include_in_compile(MORNING, true).unwrap();
    p.set_include_in_compile(CHAPTER, false).unwrap();
    assert!(compile::gather(&p, &opts(&p)).unwrap().is_empty());
    // Still compiles to something valid.
    for f in Format::ALL {
        assert!(!compile::compile(&p, &opts(&p), f).unwrap().is_empty());
    }
}

#[test]
fn format_names() {
    let exts: Vec<_> = Format::ALL.iter().map(|f| f.extension()).collect();
    assert_eq!(exts, ["docx", "epub", "md", "txt", "rtf"]);
    assert!(Format::ALL.iter().all(|f| !f.label().is_empty()));
}

// ------------------------------------------------------------------ DOCX

#[test]
fn docx_is_valid_wordprocessingml() {
    let dir = scratch("docx");
    let mut p = fixture_copy(&dir);
    add_text(&mut p, CHAPTER, "Tricky", TRICKY);
    let mut o = opts(&p);
    o.title = "Light & Dark".to_string();
    for manuscript in [true, false] {
        o.manuscript = manuscript;
        let bytes = compile::compile(&p, &o, Format::Docx).unwrap();
        let file = dir.0.join(format!("book-{manuscript}.docx"));
        std::fs::write(&file, &bytes).unwrap();

        let mut dump = vec!["word/document.xml", "docProps/core.xml"];
        if manuscript {
            dump.push("word/header1.xml");
        }
        let Some(out) = inspect_zip(&file, &dump) else {
            return;
        };
        for part in [
            "[Content_Types].xml",
            "_rels/.rels",
            "word/document.xml",
            "word/styles.xml",
            "word/settings.xml",
            "word/_rels/document.xml.rels",
            "docProps/core.xml",
        ] {
            assert!(out.contains(&format!("{part}\t")), "missing {part}\n{out}");
        }
        assert_eq!(out.contains("word/header1.xml\t"), manuscript);
        let doc = out
            .lines()
            .find(|l| l.starts_with("TEXT word/document.xml"))
            .unwrap();
        assert!(doc.contains("Chapter One"));
        assert!(doc.contains("The storm came in off the point"));
        assert!(doc.contains("Twelve,"));
        assert!(doc.contains("By morning the sea"));
        assert!(doc.contains(TRICKY), "{doc}");
        let core = out
            .lines()
            .find(|l| l.starts_with("TEXT docProps/core.xml"))
            .unwrap();
        assert!(core.contains("Light & Dark") && core.contains("Ada Q. Writer"));
        if manuscript {
            assert!(doc.contains("Ada Q. Writer about 100 words"), "{doc}");
            assert!(doc.contains("by Ada Q. Writer"));
            let header = out
                .lines()
                .find(|l| l.starts_with("TEXT word/header1.xml"))
                .unwrap();
            assert!(header.contains("Writer / LIGHT & DARK /"), "{header}");
        }
    }

    // The raw XML carries the formatting.
    o.manuscript = true;
    let xml = docx_part(
        &compile::compile(&p, &o, Format::Docx).unwrap(),
        "word/document.xml",
    );
    assert!(xml.contains("<w:i/><w:iCs/>"));
    assert!(xml.contains("w:line=\"480\" w:lineRule=\"auto\""));
    assert!(xml.contains("w:firstLine=\"720\""));
    assert!(xml.contains("<w:pageBreakBefore/>"));
    assert!(xml.contains("Times New Roman"));
    assert!(!xml.contains("Palatino"));
    o.manuscript = false;
    let xml = docx_part(
        &compile::compile(&p, &o, Format::Docx).unwrap(),
        "word/document.xml",
    );
    // The fixture's own 1.5 spacing and Palatino survive.
    assert!(xml.contains("w:line=\"360\" w:lineRule=\"auto\""));
    assert!(xml.contains("Palatino"));
}

/// Pulls one entry out of a stored (uncompressed) zip by its local header.
fn docx_part(zip: &[u8], name: &str) -> String {
    let mut i = 0;
    while i + 30 <= zip.len() && zip[i..i + 4] == [0x50, 0x4b, 0x03, 0x04] {
        let u16_at = |o: usize| u16::from_le_bytes([zip[i + o], zip[i + o + 1]]) as usize;
        let size = u32::from_le_bytes(zip[i + 18..i + 22].try_into().unwrap()) as usize;
        let (name_len, extra) = (u16_at(26), u16_at(28));
        let entry = &zip[i + 30..i + 30 + name_len];
        let data = i + 30 + name_len + extra;
        if entry == name.as_bytes() {
            return String::from_utf8(zip[data..data + size].to_vec()).unwrap();
        }
        i = data + size;
    }
    panic!("no {name} in zip");
}

#[test]
fn docx_opens_in_libreoffice() {
    let office = ["soffice", "libreoffice"].into_iter().find(|t| have(t));
    let Some(office) = office else {
        eprintln!("LibreOffice not installed; skipping");
        return;
    };
    let dir = scratch("soffice");
    let p = fixture();
    let file = dir.0.join("book.docx");
    std::fs::write(
        &file,
        compile::compile(&p, &opts(&p), Format::Docx).unwrap(),
    )
    .unwrap();
    let status = Command::new(office)
        .args(["--headless", "--convert-to", "txt:Text", "--outdir"])
        .arg(&dir.0)
        .arg(&file)
        .arg(format!(
            "-env:UserInstallation=file://{}",
            dir.0.join("profile").display()
        ))
        .status()
        .unwrap();
    assert!(status.success());
    let text = std::fs::read_to_string(dir.0.join("book.txt")).unwrap();
    assert!(
        text.contains("Chapter One") && text.contains("By morning the sea"),
        "{text}"
    );
}

// ------------------------------------------------------------------ EPUB

#[test]
fn epub_is_well_formed() {
    let dir = scratch("epub");
    let mut p = fixture_copy(&dir);
    add_text(&mut p, CHAPTER, "Tricky", TRICKY);
    let mut o = opts(&p);
    o.title = "Light & Dark".to_string();
    let bytes = compile::compile(&p, &o, Format::Epub).unwrap();
    assert_eq!(&bytes[30..38], b"mimetype");
    let file = dir.0.join("book.epub");
    std::fs::write(&file, &bytes).unwrap();

    if let Some(out) = inspect_zip(
        &file,
        &[
            "OEBPS/content.opf",
            "OEBPS/nav.xhtml",
            "OEBPS/title.xhtml",
            "OEBPS/chapter-1.xhtml",
        ],
    ) {
        let mut entries = out.lines().filter(|l| !l.starts_with("TEXT "));
        assert_eq!(
            entries.next(),
            Some("mimetype\t0"),
            "mimetype must come first, stored"
        );
        for part in ["META-INF/container.xml", "OEBPS/style.css"] {
            assert!(out.contains(&format!("{part}\t")), "missing {part}");
        }
        let chapter = out
            .lines()
            .find(|l| l.starts_with("TEXT OEBPS/chapter-1"))
            .unwrap();
        assert!(chapter.contains("Chapter One"));
        assert!(chapter.contains("Twelve,"));
        assert!(chapter.contains(TRICKY), "{chapter}");
        let opf = out
            .lines()
            .find(|l| l.starts_with("TEXT OEBPS/content.opf"))
            .unwrap();
        assert!(
            opf.contains("urn:uuid:")
                && opf.contains("Light & Dark")
                && opf.contains("Ada Q. Writer")
        );
        assert!(opf.contains(" en "));
    }

    let opf = docx_part(&bytes, "OEBPS/content.opf");
    assert!(opf.contains("properties=\"nav\""));
    assert!(opf.contains("<meta property=\"dcterms:modified\">"));
    let chapter = docx_part(&bytes, "OEBPS/chapter-1.xhtml");
    assert!(chapter.contains("<em>Twelve,</em>"), "{chapter}");
    assert!(chapter.contains("&amp; Jerry said &quot;&lt;hi&gt;&quot;"));
    assert!(chapter.contains("<p class=\"sep\">#</p>"));

    // The identifier stays put across compiles.
    let again = docx_part(
        &compile::compile(&p, &o, Format::Epub).unwrap(),
        "OEBPS/content.opf",
    );
    let id = |s: &str| s.split("urn:uuid:").nth(1).unwrap()[..36].to_string();
    assert_eq!(id(&opf), id(&again));

    if have("epubcheck") {
        let out = Command::new("epubcheck").arg(&file).output().unwrap();
        assert!(
            out.status.success(),
            "epubcheck:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    } else {
        eprintln!("epubcheck not installed; skipping");
    }
}

// ------------------------------------------------------- text formats

#[test]
fn markdown_output() {
    let dir = scratch("md");
    let mut p = fixture_copy(&dir);
    add_text(&mut p, CHAPTER, "Tricky", TRICKY);
    let o = opts(&p);
    let md = String::from_utf8(compile::compile(&p, &o, Format::Markdown).unwrap()).unwrap();
    assert!(
        md.starts_with("---\ntitle: \"Lighthouse\"\nauthor: \"Ada Q. Writer\"\n---\n"),
        "{md}"
    );
    assert!(md.contains("\n# Chapter One\n\nThe storm came in off the point at dusk."));
    assert!(md.contains("\n\n*Twelve,* she thought."), "{md}");
    // The separator is a paragraph, not a heading.
    assert!(md.contains("\n\n\\#\n\nBy morning the sea"), "{md}");
    assert!(
        md.contains("Tom & Jerry said \"\\<hi>\" and 'bye' \\*twice\\* \\[ok\\] #1"),
        "{md}"
    );
    assert!(!md.contains("\n\n\n"));
}

#[test]
fn plain_text_output() {
    let p = fixture();
    let mut o = opts(&p);
    o.scene_separator = "* * *".to_string();
    let txt = String::from_utf8(compile::compile(&p, &o, Format::PlainText).unwrap()).unwrap();
    assert!(txt.starts_with("Lighthouse\nby Ada Q. Writer\n"));
    assert!(txt.contains("\nChapter One\n\nThe storm came in"));
    assert!(txt.contains("\n\nTwelve, she thought."));
    assert!(txt.contains("\n\n* * *\n\nBy morning"), "{txt}");
}

#[test]
fn rtf_output_reads_back() {
    let dir = scratch("rtf");
    let mut p = fixture_copy(&dir);
    add_text(&mut p, CHAPTER, "Tricky", TRICKY);
    let o = opts(&p);
    let bytes = compile::compile(&p, &o, Format::Rtf).unwrap();
    assert!(bytes.starts_with(b"{\\rtf1"));
    let back = rtf::parse(&bytes);
    let text = back.plain_text();
    assert!(text.contains("Chapter One") && text.contains("By morning") && text.contains(TRICKY));
    let heading = back
        .paragraphs
        .iter()
        .find(|p| p.text() == "Chapter One")
        .unwrap();
    assert_eq!(heading.style.align, rtf::Align::Center);
    assert!(heading.runs[0].style.bold);
    let sep = back.paragraphs.iter().find(|p| p.text() == "#").unwrap();
    assert_eq!(sep.style.align, rtf::Align::Center);
    // Manuscript format: everything in Times New Roman, double spaced.
    assert!(
        back.paragraphs
            .iter()
            .flat_map(|p| &p.runs)
            .all(|r| r.style.font.as_deref() == Some("Times New Roman"))
    );
    let body = back
        .paragraphs
        .iter()
        .find(|p| p.text().starts_with("By morning"))
        .unwrap();
    assert_eq!(body.style.line_spacing, rtf::LineSpacing::Multiple(2.0));
    assert_eq!(body.style.first_indent, 720);
    assert!(
        back.paragraphs
            .iter()
            .flat_map(|p| &p.runs)
            .any(|r| r.style.italic && r.text.contains("Twelve"))
    );
}

//! Compile tests on `tests/fixtures/Lighthouse.scriv` (Draft > "Chapter
//! One" folder > "The Storm", "Morning & After"). The fixture is only read;
//! tests that change the binder work on a temporary copy.
//!
//! DOCX and EPUB are checked structurally with python3 (zipfile and
//! xml.etree parse every XML part), and with LibreOffice and epubcheck too
//! when those are installed.

use omaquill::compile::{self, Format, Headings, Options};
use omaquill::export::{self, Done, Job, Stop};
use omaquill::fsdir;
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
    assert_eq!(exts, ["docx", "pdf", "epub", "md", "txt", "rtf"]);
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

/// A scene of one 160,000-letter word: Pango's wrap of a stretch with no
/// break takes time that grows with its square: 7.9 s before.
#[test]
fn a_pdf_of_one_huge_word_compiles_quickly() {
    let dir = scratch("huge-word");
    let mut p = Project::create(&dir.0.join("Huge.scriv")).unwrap();
    let draft = p.root_folder(Kind::Draft).unwrap().uuid;
    let chapter = p.add_item(Kind::Folder, "Chapter", &draft, None).unwrap();
    let scene = p.add_item(Kind::Text, "Scene", &chapter, None).unwrap();
    p.set_text(&scene, &RichText::from_plain(&"a".repeat(160_000)))
        .unwrap();
    p.save().unwrap();
    for manuscript in [true, false] {
        let o = Options {
            manuscript,
            ..Options::for_project(&p)
        };
        let started = std::time::Instant::now();
        let bytes = compile::compile(&p, &o, Format::Pdf).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        assert!(
            started.elapsed().as_secs() < 4,
            "manuscript {manuscript}: {:?}",
            started.elapsed()
        );
    }
}

/// A paperback paragraph of 88,000 letters in long words that hyphenate
/// at nearly every line end. Each word that would make a third hyphenated
/// line in a row used to send the whole paragraph back through Pango:
/// 18 s in a release build. The five long words make third lines in a row
/// in P052, which stands in for Palatino where installed; in DejaVu Serif
/// the 45-letter words do.
#[test]
fn a_pdf_of_a_long_hyphenating_paragraph_compiles_quickly() {
    let words = "incomprehensibilities uncharacteristically internationalization \
                 counterrevolutionaries electroencephalography ";
    let text = words.repeat(800);
    for (tag, text, font) in [
        ("hyphenating", text.clone(), None),
        (
            "45-letter-words",
            "pneumonoultramicroscopicsilicovolcanoconiosis ".repeat(1_950),
            Some("DejaVu Serif"),
        ),
        ("hyphenated-word", "concentrated".repeat(10_000), None),
        (
            "word-after-words",
            format!("{}{}", words.repeat(2), "concentrated".repeat(10_000)),
            None,
        ),
    ] {
        let dir = scratch(tag);
        let mut p = Project::create(&dir.0.join("Hyphens.scriv")).unwrap();
        let draft = p.root_folder(Kind::Draft).unwrap().uuid;
        let chapter = p.add_item(Kind::Folder, "Chapter", &draft, None).unwrap();
        let scene = p.add_item(Kind::Text, "Scene", &chapter, None).unwrap();
        let mut text = RichText::from_plain(&text);
        if let Some(font) = font {
            assert_installed(font);
            for run in text.paragraphs.iter_mut().flat_map(|p| p.runs.iter_mut()) {
                run.style.font = Some(font.into());
            }
        }
        p.set_text(&scene, &text).unwrap();
        p.save().unwrap();
        let o = Options {
            manuscript: false,
            ..Options::for_project(&p)
        };
        let started = std::time::Instant::now();
        let bytes = compile::compile(&p, &o, Format::Pdf).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        assert!(
            started.elapsed().as_secs() < 4,
            "{tag}: {:?}",
            started.elapsed()
        );
    }
}

/// Checks that Pango finds `family` itself rather than another font in its
/// place: where a test's lines break depends on the font's widths.
fn assert_installed(family: &str) {
    use pango::prelude::*;
    let found = pangocairo::FontMap::default()
        .create_context()
        .load_font(&pango::FontDescription::from_string(family))
        .and_then(|font| font.describe().family());
    assert_eq!(
        found.as_deref(),
        Some(family),
        "{family} is not installed: the test needs fonts-dejavu-core"
    );
}

/// A paperback paragraph of 88,000 bytes of long hyphenating words with
/// a Hindi word every 400 bytes or so, every other one ending in a virama.
/// With Pango 1.56, a stretch of it laid out alone breaks some lines
/// otherwise than the whole paragraph does, and setting those lines again
/// from the paragraph's start took 6 s in a release build. With Pango 1.52
/// the stretch breaks them as the whole paragraph does;
/// `a_long_paragraph_the_windows_set_wrong_is_capped_quickly` in the pdf
/// module covers every version.
#[test]
fn a_pdf_of_a_long_paragraph_with_hindi_words_compiles_quickly() {
    use omaquill::compile::Section;
    use omaquill::rtf::{CharStyle, Paragraph, Run};
    let words = "incomprehensibilities uncharacteristically internationalization \
                 counterrevolutionaries electroencephalography";
    let mut text = String::new();
    while text.len() < 88_000 {
        for word in words.split(' ') {
            text.push_str(word);
            text.push(' ');
            match text.len() / 20 % 40 {
                39 => text.push_str("\u{905}\u{930}\u{94d}\u{925}\u{93e}\u{924}\u{94d} "),
                19 => text.push_str("\u{935}\u{93f}\u{926}\u{94d}\u{92f}\u{93e}\u{932}\u{92f} "),
                _ => {}
            }
        }
    }
    let p = Paragraph {
        runs: vec![Run {
            text,
            style: CharStyle {
                font: Some("DejaVu Serif".into()),
                size: 22,
                ..Default::default()
            },
        }],
        ..Default::default()
    };
    let o = Options {
        title: "Hindi".into(),
        author: String::new(),
        manuscript: false,
        headings: Headings::None,
        scene_separator: "#".into(),
    };
    let section = Section {
        heading: None,
        scenes: vec![RichText {
            paragraphs: vec![p],
            lossy: Vec::new(),
        }],
    };
    assert_installed("DejaVu Serif");
    let started = std::time::Instant::now();
    let bytes = omaquill::pdf::pdf(&[section], &o).unwrap();
    assert!(bytes.starts_with(b"%PDF-"));
    assert!(started.elapsed().as_secs() < 4, "{:?}", started.elapsed());
}

/// A styled lead-in, then a plain run of long words with a tab after every
/// seventh. Pango sizes a tab from the font of the first byte it lays out,
/// so a stretch of the paragraph set again on its own must size tabs from
/// the lead-in too, or its lines break elsewhere and the cap takes the
/// soft hyphens out of the wrong words. The paperback keeps at most two
/// hyphenated lines in a row. Lines are checked with pdftotext when
/// installed.
#[test]
fn a_pdf_with_tabs_after_a_styled_lead_in_keeps_two_hyphenated_lines_at_most() {
    use omaquill::compile::Section;
    use omaquill::rtf::{CharStyle, Paragraph, Run, Script};
    let words = [
        "incomprehensibilities",
        "uncharacteristically",
        "internationalization",
        "counterrevolutionaries",
        "electroencephalography",
    ];
    let mut body = String::new();
    for i in 0..400 {
        body.push_str(words[i % words.len()]);
        body.push(if i % 7 == 6 { '\t' } else { ' ' });
    }
    let leads = [
        (
            "DejaVu Serif",
            CharStyle {
                bold: true,
                ..Default::default()
            },
        ),
        (
            "DejaVu Sans",
            CharStyle {
                bold: true,
                ..Default::default()
            },
        ),
        (
            "DejaVu Sans Mono",
            CharStyle {
                script: Script::Super,
                ..Default::default()
            },
        ),
    ];
    for (font, lead) in leads {
        assert_installed(font);
        let style = |s: CharStyle| CharStyle {
            font: Some(font.to_string()),
            size: 22,
            ..s
        };
        let p = Paragraph {
            runs: vec![
                Run {
                    text: "Lo ".into(),
                    style: style(lead),
                },
                Run {
                    text: body.clone(),
                    style: style(CharStyle::default()),
                },
            ],
            ..Default::default()
        };
        let text = RichText {
            paragraphs: vec![p],
            lossy: Vec::new(),
        };
        let o = Options {
            title: "Tabs".into(),
            author: String::new(),
            manuscript: false,
            headings: Headings::None,
            scene_separator: "#".into(),
        };
        let section = Section {
            heading: None,
            scenes: vec![text],
        };
        let bytes = omaquill::pdf::pdf(&[section], &o).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        if !have("pdftotext") {
            continue;
        }
        let dir = scratch(&format!("tabs-{}", font.replace(' ', "-")));
        let file = dir.0.join("out.pdf");
        std::fs::write(&file, &bytes).unwrap();
        let out = Command::new("pdftotext")
            .arg(&file)
            .arg("-")
            .output()
            .unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        let (mut run, mut most, mut hyphenated) = (0, 0, 0);
        for line in text.lines() {
            if line.trim_end().ends_with('\u{ad}') {
                run += 1;
                hyphenated += 1;
                most = most.max(run);
            } else {
                run = 0;
            }
        }
        assert!(hyphenated > 20, "{font}: {hyphenated} hyphenated lines");
        assert!(most <= 2, "{font}: {most} hyphenated lines in a row");
    }
}

/// Both PDF layouts: a real PDF with the text, the right page size, the
/// title and author set. Text is checked with pdftotext when installed.
#[test]
fn pdf_manuscript_and_book() {
    let p = fixture();
    for (manuscript, size) in [(true, "612 x 792"), (false, "432 x 648")] {
        let o = Options {
            manuscript,
            ..opts(&p)
        };
        let bytes = compile::compile(&p, &o, Format::Pdf).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        if !(have("pdftotext") && have("pdfinfo")) {
            continue;
        }
        let dir = scratch(&format!("pdf-{manuscript}"));
        let file = dir.0.join("out.pdf");
        std::fs::write(&file, &bytes).unwrap();
        let info =
            String::from_utf8(Command::new("pdfinfo").arg(&file).output().unwrap().stdout).unwrap();
        assert!(info.contains(size), "{info}");
        assert!(info.contains("Ada Q. Writer"), "{info}");
        let text = String::from_utf8(
            Command::new("pdftotext")
                .arg("-layout")
                .arg(&file)
                .arg("-")
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        // The paperback hyphenates: rejoin words split at a soft hyphen.
        let mut joined = String::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{ad}' {
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
            } else {
                joined.push(c);
            }
        }
        let squashed = joined.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(squashed.contains("Chapter One"), "{squashed}");
        assert!(
            squashed.contains("The storm came in off the point at dusk."),
            "{squashed}"
        );
        assert!(
            squashed.contains("a boat lay on the rocks below the cliff"),
            "{squashed}"
        );
        if manuscript {
            assert!(squashed.contains("Writer / LIGHTHOUSE / 2"), "{squashed}");
            assert!(squashed.contains("about 100 words"), "{squashed}");
        }
    }
}

// ------------------------------------------------------- off the main thread

/// File names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// omaquill's program, which renders the PDFs [`export::run`] writes.
fn renderer() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_omaquill"))
}

fn job(p: &Project, opts: &Options, format: Format, path: &Path) -> Job {
    Job {
        sections: compile::gather(p, opts).unwrap(),
        opts: opts.clone(),
        format,
        path: path.to_path_buf(),
    }
}

/// What `n` threads write when each runs the same job at once.
fn on_workers(p: &Project, opts: &Options, format: Format, dir: &Path, n: usize) -> Vec<Vec<u8>> {
    let workers: Vec<_> = (0..n)
        .map(|i| {
            let path = dir.join(format!("worker{i}.{}", format.extension()));
            let job = job(p, opts, format, &path);
            std::thread::spawn(move || {
                assert_eq!(
                    export::run(job, &Stop::default(), renderer()),
                    Ok(Done::Written)
                );
                std::fs::read(path).unwrap()
            })
        })
        .collect();
    workers.into_iter().map(|w| w.join().unwrap()).collect()
}

#[test]
fn every_format_compiles_the_same_bytes_on_worker_threads() {
    let p = fixture();
    let dir = scratch("workers");
    for manuscript in [true, false] {
        let opts = Options {
            manuscript,
            ..opts(&p)
        };
        for format in Format::ALL {
            // DOCX, EPUB and PDF carry the time to the second. Equal
            // compiles on this thread before and after the workers show
            // that the second did not change while they ran.
            let attempt = (0..5).find_map(|_| {
                let before = compile::compile(&p, &opts, format).unwrap();
                let workers = on_workers(&p, &opts, format, &dir.0, 3);
                let after = compile::compile(&p, &opts, format).unwrap();
                (before == after).then_some((before, workers))
            });
            let (here, workers) = attempt.expect("the clock ticked during every attempt");
            for bytes in workers {
                assert!(
                    bytes == here,
                    "{format:?} (manuscript {manuscript}) differs on a worker thread"
                );
            }
        }
    }
}

/// The streams of a PDF that cairo wrote, in order, less the last two: the
/// object stream that holds the document information, with the time of the
/// compile to the second, and the cross-reference stream, whose offsets
/// move when that one's compressed length does.
fn timeless(pdf: &[u8]) -> Vec<&[u8]> {
    let find = |from: usize, what: &[u8]| {
        pdf[from..]
            .windows(what.len())
            .position(|w| w == what)
            .map(|at| from + at)
    };
    let mut streams = Vec::new();
    let mut at = 0;
    while let Some(start) = find(at, b">>\nstream\n") {
        let end = find(start, b"endstream").expect("an unterminated stream");
        streams.push(start..end);
        at = end;
    }
    let last = streams.pop().expect("a PDF without streams");
    let dict = &pdf[last.start.saturating_sub(300)..last.start];
    assert!(
        dict.windows(11).any(|w| w == b"/Type /XRef"),
        "the last stream is not the cross-reference stream"
    );
    streams.pop();
    streams.into_iter().map(|r| &pdf[r]).collect()
}

/// 400 paragraphs in DejaVu Sans, 110 pages of a paperback.
fn long_manuscript() -> Vec<compile::Section> {
    let words = "The keeper wrote in the log, uncharacteristically, that the storm had passed over the headland. ";
    let style = rtf::CharStyle {
        font: Some("DejaVu Sans".to_string()),
        ..rtf::CharStyle::default()
    };
    let paragraphs = (0..400)
        .map(|_| rtf::Paragraph {
            runs: vec![rtf::Run {
                text: words.repeat(6),
                style: style.clone(),
            }],
            ..rtf::Paragraph::default()
        })
        .collect();
    vec![compile::Section {
        heading: Some("One".to_string()),
        scenes: vec![RichText {
            paragraphs,
            lossy: Vec::new(),
        }],
    }]
}

/// Two windows can compile at once. Cairo's FreeType faces are shared by
/// every thread in a process, and PDFs rendered at once on two threads of
/// one process set some lines with other spaces than a PDF rendered alone.
#[test]
fn pdf_exports_at_once_each_write_what_one_alone_writes() {
    assert_installed("DejaVu Sans");
    let dir = scratch("at-once");
    let opts = Options {
        title: "T".to_string(),
        author: String::new(),
        manuscript: false,
        headings: Headings::None,
        scene_separator: "#".to_string(),
    };
    let alone = compile::render(&long_manuscript(), &opts, Format::Pdf).unwrap();
    let alone = timeless(&alone);
    assert!(alone.len() > 200, "{} streams", alone.len());
    for round in 0..3 {
        let workers: Vec<_> = (0..3)
            .map(|i| {
                let path = dir.0.join(format!("{round}-{i}.pdf"));
                let job = Job {
                    sections: long_manuscript(),
                    opts: opts.clone(),
                    format: Format::Pdf,
                    path: path.clone(),
                };
                std::thread::spawn(move || {
                    assert_eq!(
                        export::run(job, &Stop::default(), renderer()),
                        Ok(Done::Written)
                    );
                    std::fs::read(path).unwrap()
                })
            })
            .collect();
        for (i, worker) in workers.into_iter().enumerate() {
            let bytes = worker.join().unwrap();
            let streams = timeless(&bytes);
            let differ = streams.iter().zip(&alone).filter(|(a, b)| a != b).count();
            assert!(
                streams.len() == alone.len() && differ == 0,
                "round {round}, export {i}: {differ} of {} streams differ from a PDF rendered alone",
                alone.len()
            );
        }
    }
}

/// An executable `dir/name` that runs `script` in sh, standing in for the
/// PDF renderer. A child sh writes it, so no thread of this process holds
/// it open for writing when another test's fork copies the descriptors,
/// which would make running it fail with ETXTBSY.
fn fake_renderer(dir: &Path, name: &str, script: &str) -> PathBuf {
    let path = dir.join(name);
    let made = Command::new("sh")
        .arg("-c")
        .arg("printf '#!/bin/sh\\n%s\\n' \"$1\" > \"$2\" && chmod +x \"$2\"")
        .arg("sh")
        .arg(script)
        .arg(&path)
        .status()
        .unwrap();
    assert!(made.success());
    path
}

#[test]
fn a_pdf_export_reports_what_went_wrong_in_its_renderer() {
    let p = fixture();
    let dir = scratch("renderer");
    let opts = opts(&p);
    let path = dir.0.join("Out.pdf");
    std::fs::write(&path, "the last good compile").unwrap();
    let pdf = || job(&p, &opts, Format::Pdf, &path);
    let missing = dir.0.join("no renderer");
    let err = export::run(pdf(), &Stop::default(), &missing).unwrap_err();
    assert!(
        err.starts_with("couldn't start the PDF renderer: "),
        "{err}"
    );
    let failing = fake_renderer(
        &dir.0,
        "failing",
        "cat > /dev/null; echo 'omaquill: out of paper' >&2; exit 1",
    );
    let err = export::run(pdf(), &Stop::default(), &failing).unwrap_err();
    assert_eq!(err, "out of paper");
    // A renderer that crashes takes nothing else down with it.
    let crashing = fake_renderer(&dir.0, "crashing", "kill -SEGV $$");
    let err = export::run(pdf(), &Stop::default(), &crashing).unwrap_err();
    assert!(err.contains("SIGSEGV"), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), b"the last good compile");
    // Only a PDF needs the renderer.
    let md = dir.0.join("Out.md");
    let done = export::run(
        job(&p, &opts, Format::Markdown, &md),
        &Stop::default(),
        &missing,
    );
    assert_eq!(done, Ok(Done::Written));
    assert_eq!(names(&dir.0), ["Out.md", "Out.pdf", "crashing", "failing"]);
}

#[test]
fn a_stop_ends_the_pdf_renderer_and_keeps_the_target() {
    let p = fixture();
    let dir = scratch("renderer-stop");
    let opts = opts(&p);
    let path = dir.0.join("Out.pdf");
    std::fs::write(&path, "the last good compile").unwrap();
    let slow = fake_renderer(&dir.0, "slow", "exec sleep 60");
    let stop = Stop::default();
    let worker = {
        let (job, stop) = (job(&p, &opts, Format::Pdf, &path), stop.clone());
        std::thread::spawn(move || export::run(job, &stop, &slow))
    };
    std::thread::sleep(std::time::Duration::from_millis(200));
    let stopped = std::time::Instant::now();
    assert!(stop.stop());
    assert_eq!(worker.join().unwrap(), Ok(Done::Stopped));
    assert!(
        stopped.elapsed() < std::time::Duration::from_secs(5),
        "the renderer outlived its stop by {:?}",
        stopped.elapsed()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"the last good compile");
    assert_eq!(names(&dir.0), ["Out.pdf", "slow"]);
}

#[test]
fn an_export_replaces_its_target_whole() {
    let p = fixture();
    let dir = scratch("export-replace");
    let opts = opts(&p);
    let path = dir.0.join("Out.md");
    std::fs::write(&path, "an older and much longer compile ".repeat(1_000)).unwrap();
    let stop = Stop::default();
    let done = export::run(job(&p, &opts, Format::Markdown, &path), &stop, renderer());
    assert_eq!(done, Ok(Done::Written));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        compile::compile(&p, &opts, Format::Markdown).unwrap()
    );
    assert!(
        !stop.stop(),
        "a stop after the rename reports it came too late"
    );
    assert_eq!(names(&dir.0), ["Out.md"]);
}

#[test]
fn a_stopped_export_leaves_its_target() {
    let p = fixture();
    let dir = scratch("export-stop");
    let opts = opts(&p);
    let path = dir.0.join("Out.pdf");
    std::fs::write(&path, "the last good compile").unwrap();
    let stop = Stop::default();
    assert!(stop.stop());
    assert!(stop.stop(), "a second stop still reports the target kept");
    let done = export::run(job(&p, &opts, Format::Pdf, &path), &stop, renderer());
    assert_eq!(done, Ok(Done::Stopped));
    assert_eq!(std::fs::read(&path).unwrap(), b"the last good compile");
    assert_eq!(names(&dir.0), ["Out.pdf"]);
    // A stop that lands after the bytes are on disk, just before the
    // rename: the whole new file sits beside the target, and is removed.
    let kept = fsdir::write_atomic_if(&path, b"the new compile", || {
        assert_eq!(names(&dir.0).len(), 2, "{:?}", names(&dir.0));
        assert_eq!(std::fs::read(&path).unwrap(), b"the last good compile");
        false
    })
    .unwrap();
    assert!(!kept);
    assert_eq!(std::fs::read(&path).unwrap(), b"the last good compile");
    assert_eq!(names(&dir.0), ["Out.pdf"]);
}

#[test]
fn an_export_that_cannot_write_reports_why_and_leaves_nothing() {
    let p = fixture();
    let dir = scratch("export-unwritable");
    let opts = opts(&p);
    let missing = dir.0.join("no such folder").join("Out.txt");
    let err = export::run(
        job(&p, &opts, Format::PlainText, &missing),
        &Stop::default(),
        renderer(),
    )
    .unwrap_err();
    assert!(err.contains("No such file or directory"), "{err}");
    // A folder where the file would go: the rename fails, and the folder
    // keeps what it holds.
    let folder = dir.0.join("Out.rtf");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("keep.txt"), "kept").unwrap();
    let err = export::run(
        job(&p, &opts, Format::Rtf, &folder),
        &Stop::default(),
        renderer(),
    )
    .unwrap_err();
    assert!(!err.is_empty());
    assert_eq!(names(&dir.0), ["Out.rtf"]);
    assert_eq!(names(&folder), ["keep.txt"]);
}

//! Project-level tests on a copy of `tests/fixtures/Lighthouse.scriv`, a
//! small made-up project laid out the way Scrivener 3 writes one.

use omaquill::project::{Kind, Project};
use omaquill::rtf::{self, RichText};
use omaquill::{sha1, xml};
use std::path::{Path, PathBuf};

const SCENE: &str = "3C8B2F4D-5A6E-4F7B-9CAD-1E2F3A4B5C03";
const MORNING: &str = "4D9C3A5E-6B7F-4A8C-ADBE-2F3A4B5C6D04";
const CHAPTER: &str = "2B7A1E3C-4F5D-4E6A-8B9C-0D1E2F3A4B02";
const DRAFT: &str = "1D0E6B55-1E50-4C0B-9D47-3A0B6F6C1A01";
const RESEARCH: &str = "5EAD4B6F-7C8A-4B9D-BECF-3A4B5C6D7E05";

struct Copy(PathBuf);

impl Drop for Copy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

fn copy_fixture(tag: &str) -> Copy {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/Lighthouse.scriv");
    let dir = std::env::temp_dir().join(format!("omaquill-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let dst = dir.join("Lighthouse.scriv");
    copy_dir(&src, &dst);
    Copy(dst)
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

fn scrivx(p: &Path) -> String {
    std::fs::read_to_string(p.join("Lighthouse.scrivx")).unwrap()
}

/// Every element outside the Binder, serialized, to prove it survived.
fn outside_binder(xml_text: &str) -> Vec<String> {
    let doc = xml::parse(xml_text).unwrap();
    doc.root
        .children
        .iter()
        .filter_map(|n| match n {
            xml::Node::Element(e) if e.name != "Binder" => Some(format!("{e:?}")),
            _ => None,
        })
        .collect()
}

#[test]
fn reads_the_binder() {
    let c = copy_fixture("read");
    let p = Project::open(&c.0).unwrap();
    let items = p.items();
    let kinds: Vec<Kind> = items.iter().map(|i| i.kind).collect();
    assert_eq!(kinds, [Kind::Draft, Kind::Research, Kind::Trash]);
    let scene = p.item(SCENE).unwrap();
    assert_eq!(scene.title, "The Storm");
    assert_eq!(scene.label, Some(11));
    assert_eq!(scene.status, Some(2));
    assert!(scene.include_in_compile);
    assert_eq!(p.item(MORNING).unwrap().title, "Morning & After");
    assert_eq!(p.ancestors(SCENE), [DRAFT, CHAPTER]);
    assert!(p.is_in_draft(SCENE));
    assert!(!p.is_in_trash(SCENE));
    assert_eq!(p.synopsis(SCENE), "Mara keeps the light through the storm.");
    assert!(
        p.text(SCENE)
            .unwrap()
            .plain_text()
            .starts_with("The storm came in")
    );
    assert!(p.notes(SCENE).unwrap().plain_text().contains("Fresnel"));
    assert_eq!(
        p.labels()
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["No Label", "Red", "Blue"]
    );
    assert_eq!(p.statuses().len(), 3);
    assert!(
        p.media_path("6FBE5C7A-8D9B-4CAE-CFDA-4B5C6D7E8F06")
            .is_some()
    );
}

#[test]
fn saving_without_changes_touches_nothing() {
    let c = copy_fixture("noop");
    let before = scrivx(&c.0);
    let mut p = Project::open(&c.0).unwrap();
    assert!(!p.has_unsaved_changes());
    p.save().unwrap();
    assert_eq!(scrivx(&c.0), before);
}

#[test]
fn edits_survive_a_reopen_and_keep_everything_else() {
    let c = copy_fixture("edit");
    let before = scrivx(&c.0);
    let mut p = Project::open(&c.0).unwrap();

    let mut text = p.text(SCENE).unwrap();
    text.paragraphs[0].runs[0]
        .text
        .push_str(" The glass hummed.");
    p.set_text(SCENE, &text).unwrap();
    p.set_title(MORNING, "Morning <After>").unwrap();
    p.set_synopsis(MORNING, "The wreck.").unwrap();
    p.set_label(MORNING, Some(7)).unwrap();
    p.set_status(SCENE, None).unwrap();
    let new = p.add_item(Kind::Text, "Epilogue", CHAPTER, None).unwrap();
    p.set_text(&new, &RichText::from_plain("The light went dark in 1939."))
        .unwrap();
    p.save().unwrap();

    let after = scrivx(&c.0);
    assert_eq!(outside_binder(&after), outside_binder(&before));
    // Still parses as XML another tool would accept.
    xml::parse(&after).unwrap();

    let p = Project::open(&c.0).unwrap();
    assert!(
        p.text(SCENE)
            .unwrap()
            .plain_text()
            .contains("The glass hummed.")
    );
    let morning = p.item(MORNING).unwrap();
    assert_eq!(morning.title, "Morning <After>");
    assert_eq!(morning.label, Some(7));
    assert_eq!(p.synopsis(MORNING), "The wreck.");
    assert_eq!(p.item(SCENE).unwrap().status, None);
    let chapter = p.item(CHAPTER).unwrap();
    assert_eq!(chapter.children.len(), 3);
    assert_eq!(chapter.children[2].title, "Epilogue");
    assert!(
        chapter.children[2].include_in_compile,
        "new draft items compile"
    );

    // docs.checksum matches what's on disk.
    let sums = std::fs::read_to_string(c.0.join("Files/Data/docs.checksum")).unwrap();
    for line in sums.lines() {
        let (k, v) = line.split_once('=').unwrap();
        let (uuid, file) = k.split_once('/').unwrap();
        let bytes =
            std::fs::read(c.0.join("Files/Data").join(uuid.to_uppercase()).join(file)).unwrap();
        assert_eq!(sha1::hex(&bytes), v, "{k}");
    }
    assert!(sums.contains(&format!("{}/synopsis.txt=", MORNING.to_lowercase())));

    // The search index has the new text.
    let index = std::fs::read_to_string(c.0.join("Files/search.indexes")).unwrap();
    assert!(index.contains("The glass hummed."));
    assert!(index.contains("<Title>Morning &lt;After&gt;</Title>"));
}

#[test]
fn new_items_are_laid_out_like_scrivener_does() {
    let c = copy_fixture("layout");
    let mut p = Project::open(&c.0).unwrap();
    let uuid = p
        .add_item(Kind::Text, "Coda", CHAPTER, Some(MORNING))
        .unwrap();
    p.save().unwrap();
    let x = scrivx(&c.0);
    let at = x.find(&uuid).unwrap();
    let line_start = x[..at].rfind('\n').unwrap() + 1;
    // Siblings of MORNING sit at 24 spaces.
    assert_eq!(
        &x[line_start..at],
        "                        <BinderItem UUID=\""
    );
    let block_end = at + x[at..].find("</BinderItem>").unwrap();
    let block = &x[line_start..block_end];
    assert!(
        block.contains("\n                            <Title>Coda</Title>\n"),
        "{block}"
    );
    assert!(
        block.contains(
            "\n                                <IncludeInCompile>Yes</IncludeInCompile>\n"
        ),
        "{block}"
    );
}

#[test]
fn moving_and_trashing() {
    let c = copy_fixture("move");
    let mut p = Project::open(&c.0).unwrap();
    // Morning to the top of the chapter.
    p.move_item(MORNING, Some(CHAPTER), 0).unwrap();
    let kids: Vec<String> = p
        .item(CHAPTER)
        .unwrap()
        .children
        .iter()
        .map(|i| i.title.clone())
        .collect();
    assert_eq!(kids, ["Morning & After", "The Storm"]);
    // Moving down within the same parent counts from before the move.
    p.move_item(MORNING, Some(CHAPTER), 2).unwrap();
    let kids: Vec<String> = p
        .item(CHAPTER)
        .unwrap()
        .children
        .iter()
        .map(|i| i.title.clone())
        .collect();
    assert_eq!(kids, ["The Storm", "Morning & After"]);
    // Out to Research, deeper than before, keeps its metadata.
    p.move_item(SCENE, Some(RESEARCH), 0).unwrap();
    assert_eq!(p.item(SCENE).unwrap().label, Some(11));
    assert_eq!(p.ancestors(SCENE), [RESEARCH]);
    // No cycles, no moving root folders.
    assert!(p.move_item(CHAPTER, Some(MORNING), 0).is_err());
    assert!(p.move_item(DRAFT, Some(RESEARCH), 0).is_err());
    p.trash(CHAPTER).unwrap();
    assert!(p.is_in_trash(MORNING));
    p.save().unwrap();

    let x = scrivx(&c.0);
    xml::parse(&x).unwrap();
    let p2 = Project::open(&c.0).unwrap();
    assert!(p2.is_in_trash(MORNING));
    assert_eq!(p2.item(DRAFT).unwrap().children.len(), 0);
    // An emptied Children element goes away entirely.
    assert!(!x.contains("<Children>\n            </Children>"));

    let mut p2 = p2;
    let gone = p2.empty_trash().unwrap();
    assert_eq!(gone, 3); // Cut Scene, Chapter One, Morning
    p2.save().unwrap();
    assert!(!c.0.join("Files/Data").join(MORNING).exists());
    assert!(
        c.0.join("Files/Data").join(SCENE).exists(),
        "Scene was in Research"
    );
}

#[test]
fn backups_are_valid_zips() {
    let c = copy_fixture("backup");
    let p = Project::open(&c.0).unwrap();
    let dir = c.0.parent().unwrap().join("backups");
    let zip = p.backup(&dir, 2).unwrap();
    let out = std::process::Command::new("unzip")
        .arg("-l")
        .arg(&zip)
        .output();
    if let Ok(out) = out {
        let list = String::from_utf8_lossy(&out.stdout);
        assert!(
            list.contains("Lighthouse.scriv/Lighthouse.scrivx"),
            "{list}"
        );
        assert!(list.contains(&format!("Lighthouse.scriv/Files/Data/{SCENE}/content.rtf")));
    }
}

#[test]
fn creates_a_project_scrivener_can_read() {
    let dir = std::env::temp_dir().join(format!("omaquill-new-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("My Novel.scriv");
    let mut p = Project::create(&path).unwrap();
    let draft = p.root_folder(Kind::Draft).unwrap();
    assert_eq!(draft.children.len(), 1);
    let ch = draft.children[0].uuid.clone();
    p.set_text(
        &ch,
        &RichText::from_plain("It was a dark and stormy night."),
    )
    .unwrap();
    p.save().unwrap();
    let p = Project::open(&path).unwrap();
    assert_eq!(
        p.text(&ch).unwrap().plain_text(),
        "It was a dark and stormy night."
    );
    assert!(path.join("My Novel.scrivx").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn rich_text_survives_a_save() {
    let c = copy_fixture("rich");
    let mut p = Project::open(&c.0).unwrap();
    let text = p.text(SCENE).unwrap();
    assert!(text.paragraphs[2].runs[0].style.italic);
    p.set_text(SCENE, &text).unwrap();
    p.save().unwrap();
    let again = Project::open(&c.0).unwrap().text(SCENE).unwrap();
    assert_eq!(again.paragraphs, text.paragraphs);
    let raw = std::fs::read(c.0.join("Files/Data").join(SCENE).join("content.rtf")).unwrap();
    assert_eq!(rtf::parse(&raw).paragraphs, text.paragraphs);
}

#[test]
fn backups_never_prune_another_projects() {
    let c = copy_fixture("prune");
    let dir = c.0.parent().unwrap().join("backups");
    // Another project whose name starts the same way, backed up earlier.
    let other = c.0.parent().unwrap().join("Lighthouse Two.scriv");
    copy_dir(&c.0, &other);
    let other_zip = omaquill::project::backup(&other, &dir, 3).unwrap();
    let mut mine = Vec::new();
    for _ in 0..4 {
        mine.push(omaquill::project::backup(&c.0, &dir, 2).unwrap());
    }
    assert!(other_zip.exists(), "another project's backup was pruned");
    assert!(mine.last().unwrap().exists(), "the new backup was pruned");
    let kept = mine.iter().filter(|p| p.exists()).count();
    assert_eq!(kept, 2);
    // Same second, different files.
    let names: std::collections::HashSet<_> = mine.iter().collect();
    assert_eq!(names.len(), 4);
}

#[test]
fn emptying_trash_never_touches_data_for_malformed_items() {
    let c = copy_fixture("badtrash");
    // A Trash child with no UUID, as a damaged file might have.
    let x = scrivx(&c.0).replace(
        "<Title>Cut Scene</Title>",
        "<Title>Cut Scene</Title>\n                    <Children>\n                        <BinderItem Type=\"Text\"><Title>No id</Title></BinderItem>\n                    </Children>",
    );
    std::fs::write(c.0.join("Lighthouse.scrivx"), x).unwrap();
    let mut p = Project::open(&c.0).unwrap();
    p.empty_trash().unwrap();
    p.save().unwrap();
    assert!(
        c.0.join("Files/Data")
            .join(SCENE)
            .join("content.rtf")
            .exists()
    );
}

#[test]
fn saves_keep_permissions_and_symlinks() {
    use std::os::unix::fs::PermissionsExt;
    let c = copy_fixture("perms");
    let file = c.0.join("Files/Data").join(SCENE).join("content.rtf");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    // The notes file is a symlink to a file elsewhere.
    let real = c.0.parent().unwrap().join("notes-elsewhere.rtf");
    let notes = c.0.join("Files/Data").join(SCENE).join("notes.rtf");
    std::fs::rename(&notes, &real).unwrap();
    std::os::unix::fs::symlink(&real, &notes).unwrap();

    let mut p = Project::open(&c.0).unwrap();
    p.set_text(SCENE, &RichText::from_plain("private")).unwrap();
    p.set_notes(SCENE, &RichText::from_plain("new note"))
        .unwrap();
    p.save().unwrap();
    let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert!(
        std::fs::symlink_metadata(&notes)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(String::from_utf8_lossy(&std::fs::read(&real).unwrap()).contains("new note"));
}

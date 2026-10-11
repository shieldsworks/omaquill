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
fn a_pruned_same_second_name_is_not_reused() {
    let c = copy_fixture("prune-name");
    let dir = c.0.parent().unwrap().join("backups");
    let stamp_of = |path: &Path| {
        path.file_name()
            .unwrap()
            .to_string_lossy()
            .chars()
            .take(19)
            .collect::<String>()
    };
    let count_of = |path: &Path| {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let stem = name.strip_suffix(".zip").unwrap_or(&name).to_string();
        stem.get(20..)
            .and_then(|rest| rest.parse().ok())
            .unwrap_or(1)
    };
    for _ in 0..5 {
        let first = omaquill::project::backup(&c.0, &dir, 10).unwrap();
        let second = omaquill::project::backup(&c.0, &dir, 10).unwrap();
        if stamp_of(&first) != stamp_of(&second) {
            continue;
        }
        std::fs::remove_file(&first).unwrap();
        let third = omaquill::project::backup(&c.0, &dir, 10).unwrap();
        assert!(
            count_of(&third) > count_of(&second),
            "pruned {} came back as {}",
            first.display(),
            third.display()
        );
        assert!(second.is_file());
        return;
    }
    panic!("five tries never placed two backups in one second");
}

#[test]
fn leftover_temp_does_not_block_the_next_backup() {
    if std::env::var_os("OMAQUILL_STALE_TEMP").is_some() {
        leftover_temp_child();
        return;
    }
    let c = copy_fixture("stale");
    let parent = c.0.parent().unwrap().join("backups");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "leftover_temp_does_not_block_the_next_backup"])
        .env("OMAQUILL_STALE_TEMP", "1")
        .env("OMAQUILL_STALE_PROJECT", &c.0)
        .env("OMAQUILL_STALE_DIR", &parent)
        .status()
        .unwrap();
    assert!(status.success(), "stale temp still blocked the backup");
}

fn leftover_temp_child() {
    let project = PathBuf::from(std::env::var("OMAQUILL_STALE_PROJECT").unwrap());
    let parent = PathBuf::from(std::env::var("OMAQUILL_STALE_DIR").unwrap());
    let full = std::fs::canonicalize(&project).unwrap();
    let name = project.file_stem().unwrap().to_string_lossy();
    let id = &sha1::hex(full.to_string_lossy().as_bytes())[..8];
    let dir = parent.join(format!("{name}-{id}"));
    std::fs::create_dir_all(&dir).unwrap();
    let stale = dir.join(format!(".backup.{}-0.omaquill-tmp", std::process::id()));
    std::fs::write(&stale, b"not a zip").unwrap();
    let before = file_bytes(&project);
    let zip = omaquill::project::backup(&project, &parent, 5).unwrap();
    assert_eq!(file_bytes(&unzip_stored(&zip)), before);
    assert!(!stale.exists(), "leftover temp was published");
}

#[test]
fn concurrent_backups_in_one_second_each_restore_the_project() {
    let c = copy_fixture("race");
    let pad = vec![0xA5u8; 2 * 1024 * 1024];
    std::fs::write(c.0.join("pad.bin"), &pad).unwrap();
    let before = file_bytes(&c.0);
    let dir = c.0.parent().unwrap().join("backups");
    for round in 0..3 {
        let mut handles = Vec::new();
        for _ in 0..4 {
            let path = c.0.clone();
            let dir = dir.clone();
            handles.push(std::thread::spawn(move || {
                omaquill::project::backup(&path, &dir, 100).unwrap()
            }));
        }
        let zips: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let mut names = std::collections::HashSet::new();
        for zip in &zips {
            assert!(
                names.insert(zip.clone()),
                "round {round} reused {}",
                zip.display()
            );
            let restored = unzip_stored(zip);
            let got = file_bytes(&restored);
            assert_eq!(
                got,
                before,
                "round {round} {} restored {} files, snapshot has {}",
                zip.display(),
                got.len(),
                before.len()
            );
        }
    }
}

fn file_bytes(root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap())
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let ft = entry.file_type().unwrap();
            if ft.is_dir() {
                walk(root, &path, out);
            } else if ft.is_file() {
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn unzip_stored(zip_path: &Path) -> PathBuf {
    let dest = zip_path.with_extension("restored");
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(&dest).unwrap();
    let data = std::fs::read(zip_path).unwrap();
    let mut i = 0usize;
    while i + 30 <= data.len() {
        let sig = u32::from_le_bytes(data[i..i + 4].try_into().unwrap());
        if sig != 0x04034b50 {
            break;
        }
        let method = u16::from_le_bytes(data[i + 8..i + 10].try_into().unwrap());
        assert_eq!(method, 0, "{} is not a stored zip", zip_path.display());
        let size = u32::from_le_bytes(data[i + 18..i + 22].try_into().unwrap()) as usize;
        let name_len = u16::from_le_bytes(data[i + 26..i + 28].try_into().unwrap()) as usize;
        let extra_len = u16::from_le_bytes(data[i + 28..i + 30].try_into().unwrap()) as usize;
        let name_at = i + 30;
        assert!(
            name_at + name_len + extra_len + size <= data.len(),
            "{} ends inside an entry",
            zip_path.display()
        );
        let name = std::str::from_utf8(&data[name_at..name_at + name_len]).unwrap();
        let start = name_at + name_len + extra_len;
        let out = dest.join(name);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&out, &data[start..start + size]).unwrap();
        i = start + size;
    }
    dest.join("Lighthouse.scriv")
}

#[test]
fn trash_without_an_id_does_not_delete_files() {
    let c = copy_fixture("trash-id");
    let x = scrivx(&c.0).replace(
        "UUID=\"7ACF6D8B-9EAC-4DBF-DAEB-5C6D7E8F9A07\" Type=\"TrashFolder\"",
        "Type=\"TrashFolder\"",
    );
    std::fs::write(c.0.join("Lighthouse.scrivx"), x).unwrap();
    let mut p = Project::open(&c.0).unwrap();
    let err = p.empty_trash().unwrap_err();
    assert_eq!(err.to_string(), "binder item has no id");
    assert!(
        c.0.join("Files/Data")
            .join(SCENE)
            .join("content.rtf")
            .exists()
    );
}

#[test]
fn moving_an_item_with_no_id_is_an_error() {
    let c = copy_fixture("move-id");
    let x = scrivx(&c.0).replace(&format!("UUID=\"{SCENE}\" "), "");
    std::fs::write(c.0.join("Lighthouse.scrivx"), x).unwrap();
    let mut p = Project::open(&c.0).unwrap();
    let err = p.move_item("", Some(CHAPTER), 0).unwrap_err();
    assert_eq!(err.to_string(), "binder item has no id");
    let chapter = p.item(CHAPTER).unwrap();
    assert!(
        chapter
            .children
            .iter()
            .any(|c| c.title == "The Storm" && c.uuid.is_empty())
    );
    assert!(
        c.0.join("Files/Data")
            .join(SCENE)
            .join("content.rtf")
            .exists()
    );
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
fn saves_keep_permissions_and_links_inside_the_project() {
    use std::os::unix::fs::PermissionsExt;
    let c = copy_fixture("perms");
    let file = c.0.join("Files/Data").join(SCENE).join("content.rtf");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    // The notes file links to another file inside the project: followed.
    let real = c.0.join("Files/notes-elsewhere.rtf");
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

/// A project someone sends you can't make omaquill touch your other files
/// (marketplace review of c1b4b49): a chapter linked to `~/.bashrc` is
/// never read into the editor, and saving it writes inside the project.
#[test]
fn links_out_of_the_project_are_never_followed() {
    let c = copy_fixture("escape");
    let outside = c.0.parent().unwrap().join("bashrc");
    std::fs::write(&outside, "export SECRET=1\n").unwrap();
    let chapter = c.0.join("Files/Data").join(SCENE).join("content.rtf");
    std::fs::remove_file(&chapter).unwrap();
    std::os::unix::fs::symlink(&outside, &chapter).unwrap();

    let mut p = Project::open(&c.0).unwrap();
    assert!(
        p.text(SCENE).is_err(),
        "read through a link out of the project"
    );
    p.set_text(SCENE, &RichText::from_plain("new chapter"))
        .unwrap();
    p.save().unwrap();
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "export SECRET=1\n"
    );
    assert!(
        !std::fs::symlink_metadata(&chapter)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(p.text(SCENE).unwrap().plain_text(), "new chapter");

    // A whole item folder linked outside: nothing written, nothing deleted.
    let elsewhere = c.0.parent().unwrap().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("keep.txt"), "keep").unwrap();
    let dir = c.0.join("Files/Data").join(MORNING);
    std::fs::remove_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &dir).unwrap();
    let mut p = Project::open(&c.0).unwrap();
    assert!(p.set_text(MORNING, &RichText::from_plain("x")).is_err());
    // Clearing a synopsis deletes synopsis.txt; not one outside.
    std::fs::write(elsewhere.join("synopsis.txt"), "theirs").unwrap();
    assert_eq!(p.synopsis(MORNING), "", "read through a linked folder");
    assert!(p.set_synopsis(MORNING, "").is_err());
    assert!(elsewhere.join("synopsis.txt").exists());
    assert!(!elsewhere.join("content.rtf").exists());
    p.trash(MORNING).unwrap();
    p.empty_trash().unwrap();
    assert!(
        elsewhere.join("keep.txt").exists(),
        "emptying the Trash reached outside"
    );
    assert!(
        std::fs::symlink_metadata(&dir).is_err(),
        "the link itself goes"
    );
}

/// The project names its media files' extensions; omaquill only builds
/// plain ones, and only offers to open documents and media.
#[test]
fn media_extensions_are_plain_and_openable_types_are_few() {
    use omaquill::project::is_openable;
    let c = copy_fixture("media");
    let img = "6FBE5C7A-8D9B-4CAE-CFDA-4B5C6D7E8F06";
    let p = Project::open(&c.0).unwrap();
    assert!(p.media_path(img).is_some());
    for (bad, ext) in [("../../x", "png"), ("desktop", "desktop")] {
        let x = scrivx(&c.0).replace(
            "<FileExtension>png</FileExtension>",
            &format!("<FileExtension>{bad}</FileExtension>"),
        );
        std::fs::write(c.0.join("Lighthouse.scrivx"), x).unwrap();
        std::fs::copy(
            c.0.join("Files/Data").join(img).join("content.png"),
            c.0.join("Files/Data")
                .join(img)
                .join(format!("content.{ext}")),
        )
        .unwrap();
        let p = Project::open(&c.0).unwrap();
        let path = p.media_path(img);
        assert!(
            path.as_deref().is_none_or(|p| !is_openable(p)),
            "{bad}: {path:?}"
        );
        std::fs::write(
            c.0.join("Lighthouse.scrivx"),
            scrivx(&c.0).replace(
                &format!("<FileExtension>{bad}</FileExtension>"),
                "<FileExtension>png</FileExtension>",
            ),
        )
        .unwrap();
    }
    for ok in ["a.pdf", "a.PNG", "a.mp3", "a.txt"] {
        assert!(is_openable(Path::new(ok)), "{ok}");
    }
    for no in [
        "a.desktop",
        "a.sh",
        "a.html",
        "a.exe",
        "a.AppImage",
        "a",
        "a.py",
    ] {
        assert!(!is_openable(Path::new(no)), "{no}");
    }
}

fn tree_bytes(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    fn walk(base: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap())
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let path = e.path();
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .to_string();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                out.push((rel, std::fs::read(&path).unwrap()));
            }
        }
    }
    walk(dir, dir, &mut out);
    out
}

#[test]
fn checking_spelling_does_not_change_the_project() {
    let c = copy_fixture("spell");
    let before = tree_bytes(&c.0);
    let project = Project::open(&c.0).unwrap();
    let scene = project.text(SCENE).unwrap();
    let plain = scene
        .paragraphs
        .iter()
        .map(|p| p.text())
        .collect::<Vec<_>>()
        .join("\n");
    let policy = omaquill::spell::Policy {
        hide_word_at_cursor: false,
        ..omaquill::spell::Policy::default()
    };
    let _ =
        omaquill::spell::bad_ranges(&plain, 0, plain.len(), None, &policy, &mut |w| w.len() > 1);
    let _ = omaquill::spell::keystroke_marks(&plain, plain.len(), &policy, &mut |w| w.len() > 1);
    let ignore = omaquill::state::ignore_path(&c.0);
    let dictionary = omaquill::state::dictionary_path();
    assert!(
        !ignore.starts_with(&c.0),
        "ignore list {} is inside the project",
        ignore.display()
    );
    assert!(
        !dictionary.starts_with(&c.0),
        "dictionary {} is inside the project",
        dictionary.display()
    );
    drop(project);
    assert_eq!(before, tree_bytes(&c.0));
}

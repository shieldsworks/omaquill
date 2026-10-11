//! Find and replace against a temp copy of the Lighthouse fixture.
//! The fixture in the repo is never written.

use omaquill::project::Project;
use omaquill::replace::{self, ApplyOutcome, Case, Mode, Options, Word};
use omaquill::sha1;
use std::path::{Path, PathBuf};

const SCENE: &str = "3C8B2F4D-5A6E-4F7B-9CAD-1E2F3A4B5C03";
const MORNING: &str = "4D9C3A5E-6B7F-4A8C-ADBE-2F3A4B5C6D04";

struct Copy(PathBuf);

impl Drop for Copy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

fn copy_fixture(tag: &str) -> Copy {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/Lighthouse.scriv");
    let dir = std::env::temp_dir().join(format!("omaquill-replace-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let dst = dir.join("Lighthouse.scriv");
    copy_dir(&src, &dst);
    Copy(dst)
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), to).unwrap();
        }
    }
}

fn bytes(project: &Project, uuid: &str, file: &str) -> Vec<u8> {
    std::fs::read(project.data_dir(uuid).join(file)).unwrap()
}

fn checksum_line(project: &Project, uuid: &str, file: &str) -> String {
    let text = std::fs::read_to_string(project.path.join("Files/Data/docs.checksum")).unwrap();
    let key = format!("{}/{file}", uuid.to_lowercase());
    text.lines()
        .find(|line| line.starts_with(&format!("{key}=")))
        .unwrap()
        .to_string()
}

fn literal(find: &str, replace: &str) -> replace::Query {
    replace::query(
        find,
        replace,
        Options {
            case: Case::Sensitive,
            word: Word::Whole,
            mode: Mode::Literal,
        },
    )
    .unwrap()
}

fn zip_count(dir: &Path) -> usize {
    fn walk(dir: &Path, n: &mut usize) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                walk(&path, n);
            } else if path.extension().is_some_and(|ext| ext == "zip") {
                *n += 1;
            }
        }
    }
    let mut n = 0;
    if dir.exists() {
        walk(dir, &mut n);
    }
    n
}

#[test]
fn untouched_sibling_checksum_is_unchanged() {
    let copy = copy_fixture("sibling");
    let backups = copy.0.parent().unwrap().join("backups");
    let mut project = Project::open(&copy.0).unwrap();
    let morning = bytes(&project, MORNING, "content.rtf");
    let morning_sum = checksum_line(&project, MORNING, "content.rtf");
    let storm_before = bytes(&project, SCENE, "content.rtf");
    let notes = bytes(&project, SCENE, "notes.rtf");
    let synopsis = std::fs::read(project.data_dir(SCENE).join("synopsis.txt")).unwrap();
    let png = bytes(
        &project,
        "6FBE5C7A-8D9B-4CAE-CFDA-4B5C6D7E8F06",
        "content.png",
    );
    let query = literal("Mara", "Marian");
    let preview = replace::preview(&project, &query).unwrap();
    let ids = preview.ids();
    assert!(!ids.is_empty());
    let outcome = replace::apply(&mut project, &backups, 1, &preview, &ids).unwrap();
    let replace::ApplyOutcome::Done(report) = outcome else {
        panic!("Mara is in the storm scene");
    };
    assert!(report.backup.is_file());
    assert_eq!(bytes(&project, MORNING, "content.rtf"), morning);
    assert_eq!(checksum_line(&project, MORNING, "content.rtf"), morning_sum);
    assert_eq!(bytes(&project, SCENE, "notes.rtf"), notes);
    assert_eq!(
        std::fs::read(project.data_dir(SCENE).join("synopsis.txt")).unwrap(),
        synopsis
    );
    assert_eq!(
        bytes(
            &project,
            "6FBE5C7A-8D9B-4CAE-CFDA-4B5C6D7E8F06",
            "content.png"
        ),
        png
    );
    let storm = bytes(&project, SCENE, "content.rtf");
    assert_ne!(storm, storm_before);
    let storm_sum = checksum_line(&project, SCENE, "content.rtf");
    assert_eq!(
        storm_sum,
        format!("{}/content.rtf={}", SCENE.to_lowercase(), sha1::hex(&storm))
    );
    let reopened = Project::open(&copy.0).unwrap();
    let plain = reopened.text(SCENE).unwrap().plain_text();
    assert!(plain.contains("Marian"));
    assert!(!plain.contains("Mara lit"));
}

#[test]
fn notes_rewrite_leaves_the_body_bytes() {
    let copy = copy_fixture("notes");
    let backups = copy.0.parent().unwrap().join("backups");
    let mut project = Project::open(&copy.0).unwrap();
    let body = bytes(&project, SCENE, "content.rtf");
    let query = literal("Fresnel", "Lens");
    let preview = replace::preview(&project, &query).unwrap();
    let ids = preview.ids();
    let outcome = replace::apply(&mut project, &backups, 1, &preview, &ids).unwrap();
    assert!(matches!(outcome, ApplyOutcome::Done(_)));
    assert_eq!(bytes(&project, SCENE, "content.rtf"), body);
    let notes = project.notes(SCENE).unwrap().plain_text();
    assert!(notes.contains("Lens"));
    assert!(!notes.contains("Fresnel"));
}

#[test]
fn lighthouse_round_trip_keeps_the_italic_run() {
    let copy = copy_fixture("twelve");
    let backups = copy.0.parent().unwrap().join("backups");
    let mut project = Project::open(&copy.0).unwrap();
    let morning = bytes(&project, MORNING, "content.rtf");
    let query = literal("Twelve", "Eleven");
    let preview = replace::preview(&project, &query).unwrap();
    let ids = preview.ids();
    let outcome = replace::apply(&mut project, &backups, 1, &preview, &ids).unwrap();
    let ApplyOutcome::Done(report) = outcome else {
        panic!("Twelve is in the storm scene");
    };
    assert!(report.backup.is_file());
    assert_eq!(bytes(&project, MORNING, "content.rtf"), morning);
    let reopened = Project::open(&copy.0).unwrap();
    let text = reopened.text(SCENE).unwrap();
    assert!(text.plain_text().contains("Eleven"));
    assert!(!text.plain_text().contains("Twelve"));
    assert!(text.plain_text().contains("twelve"));
    assert!(
        text.paragraphs
            .iter()
            .flat_map(|p| &p.runs)
            .any(|run| { run.text.contains("Eleven") && run.style.italic })
    );
}

#[test]
fn regex_groups_keep_the_italic_run() {
    let copy = copy_fixture("groups");
    let backups = copy.0.parent().unwrap().join("backups");
    let mut project = Project::open(&copy.0).unwrap();
    let morning = bytes(&project, MORNING, "content.rtf");
    let query = replace::query(
        r"(Twelve),",
        "$1!",
        Options {
            case: Case::Sensitive,
            word: Word::Any,
            mode: Mode::Regex,
        },
    )
    .unwrap();
    let preview = replace::preview(&project, &query).unwrap();
    let ids = preview.ids();
    let outcome = replace::apply(&mut project, &backups, 1, &preview, &ids).unwrap();
    assert!(matches!(outcome, ApplyOutcome::Done(_)));
    assert_eq!(bytes(&project, MORNING, "content.rtf"), morning);
    let text = Project::open(&copy.0).unwrap().text(SCENE).unwrap();
    assert!(
        text.paragraphs
            .iter()
            .flat_map(|p| &p.runs)
            .any(|run| { run.text.contains("Twelve!") && run.style.italic })
    );
}

#[test]
fn second_apply_does_not_write_or_backup_again() {
    let copy = copy_fixture("twice");
    let backups = copy.0.parent().unwrap().join("backups");
    let mut project = Project::open(&copy.0).unwrap();
    let morning = bytes(&project, MORNING, "content.rtf");
    let query = literal("Mara", "Marian");
    let preview = replace::preview(&project, &query).unwrap();
    let ids = preview.ids();
    replace::apply(&mut project, &backups, 1, &preview, &ids).unwrap();
    assert_eq!(zip_count(&backups), 1);
    let again = replace::apply(&mut project, &backups, 1, &preview, &ids).unwrap();
    assert!(matches!(again, ApplyOutcome::NothingToDo));
    assert_eq!(zip_count(&backups), 1);
    assert_eq!(bytes(&project, MORNING, "content.rtf"), morning);
}

#[test]
fn identity_replacement_does_not_rewrite_or_backup() {
    let copy = copy_fixture("identity");
    let backups = copy.0.parent().unwrap().join("backups");
    let mut project = Project::open(&copy.0).unwrap();
    let before = bytes(&project, SCENE, "content.rtf");
    let query = literal("Mara", "Mara");
    let preview = replace::preview(&project, &query).unwrap();
    assert!(preview.groups().is_empty());
    let outcome = replace::apply(&mut project, &backups, 1, &preview, &[]).unwrap();
    assert!(matches!(outcome, ApplyOutcome::NothingToDo));
    assert_eq!(zip_count(&backups), 0);
    assert_eq!(bytes(&project, SCENE, "content.rtf"), before);
}

#[test]
fn unchecked_matches_are_not_written() {
    let copy = copy_fixture("unchecked");
    let backups = copy.0.parent().unwrap().join("backups");
    let mut project = Project::open(&copy.0).unwrap();
    let before = bytes(&project, SCENE, "content.rtf");
    let query = literal("Mara", "Marian");
    let preview = replace::preview(&project, &query).unwrap();
    assert!(!preview.ids().is_empty());
    let outcome = replace::apply(&mut project, &backups, 1, &preview, &[]).unwrap();
    assert!(matches!(outcome, ApplyOutcome::NothingToDo));
    assert_eq!(bytes(&project, SCENE, "content.rtf"), before);
    assert_eq!(zip_count(&backups), 0);
}

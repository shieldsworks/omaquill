use omaquill::project::{Kind, Project};
use omaquill::replace::{self, ApplyError, ApplyOutcome, Case, Mode, Options, Word};
use omaquill::rtf::{self, Align, CharStyle, ParaStyle, Paragraph, RichText, Run};
use omaquill::sha1;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

#[test]
fn preview_lists_every_match_and_writes_nothing() {
    let copy = copy_fixture("preview");
    let backups = copy.0.parent().unwrap().join("backups");
    let mut project = Project::open(&copy.0).unwrap();
    let draft = project.root_folder(Kind::Draft).unwrap().uuid.clone();
    let id = project.add_item(Kind::Text, "Extra", &draft, None).unwrap();
    project
        .set_text(&id, &RichText::from_plain("Mara and Mara"))
        .unwrap();
    project.save().unwrap();
    let before = file_bytes(&copy.0);
    let query = literal("Mara", "Marian");
    let preview = replace::preview(&project, &query).unwrap();
    let matched: Vec<_> = preview
        .groups()
        .iter()
        .flat_map(|g| g.rows().iter().map(|r| r.snippet().matched.clone()))
        .collect();
    assert_eq!(matched, ["Mara", "Mara", "Mara"]);
    assert!(preview.groups().iter().all(|g| g.lossy().is_empty()));
    assert_eq!(file_bytes(&copy.0), before);
    assert_eq!(zip_count(&backups), 0);
}

#[test]
fn two_replaces_in_one_second_keep_two_restorable_backups() {
    let copy = copy_fixture("twice-zip");
    let backups = copy.0.parent().unwrap().join("backups");
    let mut project = Project::open(&copy.0).unwrap();
    let original = file_bytes(&copy.0);
    let first = apply_all(&mut project, &backups, "Mara", "Marian");
    let mid = file_bytes(&copy.0);
    let second = apply_all(&mut project, &backups, "Twelve", "Eleven");
    assert_ne!(first, second);
    assert!(first.is_file());
    assert!(second.is_file());
    let n1 = first.file_name().unwrap().to_string_lossy();
    let n2 = second.file_name().unwrap().to_string_lossy();
    assert_eq!(&n1[..19], &n2[..19]);
    assert!(n2.contains(" 2") || n1.contains(" 2"));
    assert_eq!(file_bytes(&unzip_stored(&first)), original);
    assert_eq!(file_bytes(&unzip_stored(&second)), mid);
    let live = Project::open(&copy.0).unwrap().text(SCENE).unwrap();
    assert!(live.plain_text().contains("Marian"));
    assert!(live.plain_text().contains("Eleven"));
    assert!(
        live.paragraphs
            .iter()
            .flat_map(|p| &p.runs)
            .any(|run| run.text.contains("Eleven") && run.style.italic)
    );
    assert!(part_files(&backups).is_empty());
}

#[test]
fn styled_replacement_survives_reopen() {
    let copy = copy_fixture("styled");
    let backups = copy.0.parent().unwrap().join("backups");
    let mut project = Project::open(&copy.0).unwrap();
    let draft = project.root_folder(Kind::Draft).unwrap().uuid.clone();
    let id = project
        .add_item(Kind::Text, "Styled", &draft, None)
        .unwrap();
    let style = CharStyle {
        bold: true,
        italic: true,
        underline: true,
        color: Some((12, 34, 56)),
        size: 28,
        ..CharStyle::default()
    };
    let text = RichText {
        paragraphs: vec![Paragraph {
            style: ParaStyle {
                align: Align::Center,
                ..ParaStyle::default()
            },
            runs: vec![
                Run {
                    text: "xxAA".into(),
                    style: style.clone(),
                },
                Run {
                    text: "BByy".into(),
                    style: CharStyle {
                        italic: true,
                        ..CharStyle::default()
                    },
                },
            ],
        }],
        lossy: Vec::new(),
    };
    project.set_text(&id, &text).unwrap();
    project.save().unwrap();
    let morning = bytes(&project, MORNING, "content.rtf");
    let query = replace::query(
        "AABB",
        "Z",
        Options {
            case: Case::Sensitive,
            word: Word::Any,
            mode: Mode::Literal,
        },
    )
    .unwrap();
    let preview = replace::preview(&project, &query).unwrap();
    let outcome = replace::apply(&mut project, &backups, 5, &preview, &preview.ids()).unwrap();
    assert!(matches!(outcome, ApplyOutcome::Done(_)));
    let again = Project::open(&copy.0).unwrap().text(&id).unwrap();
    assert_eq!(again.plain_text(), "xxZyy");
    assert_eq!(again.paragraphs[0].style.align, Align::Center);
    assert_eq!(again.paragraphs[0].runs.len(), 2);
    assert_eq!(again.paragraphs[0].runs[0].text, "xxZ");
    assert!(again.paragraphs[0].runs[0].style.bold);
    assert!(again.paragraphs[0].runs[0].style.underline);
    assert_eq!(again.paragraphs[0].runs[0].style.color, Some((12, 34, 56)));
    assert_eq!(again.paragraphs[0].runs[1].text, "yy");
    assert!(again.paragraphs[0].runs[1].style.italic);
    assert!(!again.paragraphs[0].runs[1].style.bold);
    assert_eq!(bytes(&project, MORNING, "content.rtf"), morning);
}

#[test]
fn literal_dollar_and_regex_group_round_trip() {
    let copy = copy_fixture("dollar");
    let backups = copy.0.parent().unwrap().join("backups");
    let mut project = Project::open(&copy.0).unwrap();
    let query = replace::query(
        "Mara",
        "$1",
        Options {
            case: Case::Sensitive,
            word: Word::Whole,
            mode: Mode::Literal,
        },
    )
    .unwrap();
    let preview = replace::preview(&project, &query).unwrap();
    let ids = preview.ids();
    replace::apply(&mut project, &backups, 2, &preview, &ids).unwrap();
    let plain = Project::open(&copy.0)
        .unwrap()
        .text(SCENE)
        .unwrap()
        .plain_text();
    assert!(plain.contains("$1 lit"));
    assert!(!plain.contains("Mara"));

    let mut project = Project::open(&copy.0).unwrap();
    let query = replace::query(
        r"(\$1)",
        "[$1]",
        Options {
            case: Case::Sensitive,
            word: Word::Any,
            mode: Mode::Regex,
        },
    )
    .unwrap();
    let preview = replace::preview(&project, &query).unwrap();
    let ids = preview.ids();
    replace::apply(&mut project, &backups, 2, &preview, &ids).unwrap();
    let plain = Project::open(&copy.0)
        .unwrap()
        .text(SCENE)
        .unwrap()
        .plain_text();
    assert!(plain.contains("[$1] lit"), "{plain}");
}

#[test]
fn lossy_image_is_named_before_the_rewrite_drops_it() {
    let copy = copy_fixture("pict");
    let backups = copy.0.parent().unwrap().join("backups");
    let mut project = Project::open(&copy.0).unwrap();
    let draft = project.root_folder(Kind::Draft).unwrap().uuid.clone();
    let id = project.add_item(Kind::Text, "Plate", &draft, None).unwrap();
    project.save().unwrap();
    let raw = b"{\\rtf1\\ansi\\pard alpha {\\pict\\pngblip deadbeef} beta}";
    std::fs::create_dir_all(project.data_dir(&id)).unwrap();
    std::fs::write(project.data_dir(&id).join("content.rtf"), raw).unwrap();
    let project = Project::open(&copy.0).unwrap();
    let seen = project.text(&id).unwrap();
    assert_eq!(seen.lossy, ["images"]);
    assert!(seen.plain_text().contains("alpha"));
    let query = literal("alpha", "omega");
    let preview = replace::preview(&project, &query).unwrap();
    let plate = preview
        .groups()
        .iter()
        .find(|g| g.id().uuid() == id)
        .unwrap();
    assert_eq!(plate.lossy(), ["images"]);
    let before = std::fs::read(project.data_dir(&id).join("content.rtf")).unwrap();
    assert!(before.windows(4).any(|w| w == b"pict"));
    let mut project = project;
    let ids = preview
        .groups()
        .iter()
        .filter(|g| g.id().uuid() == id)
        .flat_map(|g| g.rows().iter().map(|r| r.id()))
        .collect::<Vec<_>>();
    replace::apply(&mut project, &backups, 2, &preview, &ids).unwrap();
    let after = std::fs::read(project.data_dir(&id).join("content.rtf")).unwrap();
    assert!(!after.windows(4).any(|w| w == b"pict"));
    assert!(
        Project::open(&copy.0)
            .unwrap()
            .text(&id)
            .unwrap()
            .plain_text()
            .contains("omega")
    );
}

#[test]
fn empty_find_is_rejected() {
    let err = replace::query("", "x", Options::default()).unwrap_err();
    assert_eq!(err.to_string(), "Enter text to find");
    let err = replace::query(
        "",
        "x",
        Options {
            mode: Mode::Regex,
            ..Options::default()
        },
    )
    .unwrap_err();
    assert_eq!(err.to_string(), "Enter text to find");
}

#[test]
fn unicode_and_escapes_round_trip() {
    let copy = copy_fixture("unicode");
    let backups = copy.0.parent().unwrap().join("backups");
    let mut project = Project::open(&copy.0).unwrap();
    let draft = project.root_folder(Kind::Draft).unwrap().uuid.clone();
    let id = project.add_item(Kind::Text, "Names", &draft, None).unwrap();
    project
        .set_text(
            &id,
            &RichText::from_plain("Café Straße İstanbul a {b} \\c end"),
        )
        .unwrap();
    project.save().unwrap();
    let query = replace::query(
        "café",
        "tea",
        Options {
            case: Case::Insensitive,
            word: Word::Whole,
            mode: Mode::Literal,
        },
    )
    .unwrap();
    let preview = replace::preview(&project, &query).unwrap();
    let ids = preview.ids();
    assert_eq!(ids.len(), 1);
    replace::apply(&mut project, &backups, 2, &preview, &ids).unwrap();
    let plain = Project::open(&copy.0)
        .unwrap()
        .text(&id)
        .unwrap()
        .plain_text();
    assert_eq!(plain, "tea Straße İstanbul a {b} \\c end");

    let mut project = Project::open(&copy.0).unwrap();
    let query = replace::query(
        "straße",
        "road",
        Options {
            case: Case::Insensitive,
            word: Word::Whole,
            mode: Mode::Literal,
        },
    )
    .unwrap();
    let preview = replace::preview(&project, &query).unwrap();
    assert_eq!(preview.ids().len(), 1);
    replace::apply(&mut project, &backups, 2, &preview, &preview.ids()).unwrap();
    let plain = Project::open(&copy.0)
        .unwrap()
        .text(&id)
        .unwrap()
        .plain_text();
    assert_eq!(plain, "tea road İstanbul a {b} \\c end");
}

#[test]
fn read_only_document_stays_byte_for_byte() {
    let copy = copy_fixture("ro");
    let backups = copy.0.parent().unwrap().join("backups");
    let project = Project::open(&copy.0).unwrap();
    let before = file_bytes(&copy.0);
    let _guard = ModeGuard::deny_write(project.data_dir(SCENE));
    let mut project = project;
    let query = literal("Mara", "Marian");
    let preview = replace::preview(&project, &query).unwrap();
    let err = replace::apply(&mut project, &backups, 2, &preview, &preview.ids()).unwrap_err();
    assert!(matches!(err, ApplyError::Write { written, .. } if written.is_empty()));
    assert_eq!(file_bytes(&copy.0), before);
    let zips = zip_paths(&backups);
    assert_eq!(zips.len(), 1);
    assert_eq!(file_bytes(&unzip_stored(&zips[0])), before);
}

#[test]
fn disk_full_backup_leaves_the_project() {
    if std::env::var_os("OMAQUILL_FULL_READY").is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "disk_full_backup_leaves_the_project",
                "--test-threads=1",
            ])
            .env("OMAQUILL_FULL_READY", "1")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let copy = copy_fixture("full");
    std::fs::write(copy.0.join("pad.bin"), vec![0xA5u8; 64 * 1024]).unwrap();
    let before = file_bytes(&copy.0);
    // The limit is process-wide, so this runs in the child above. Past the
    // limit the kernel raises SIGXFSZ, whose default action kills the
    // process. Ignoring it makes the zip write return EFBIG.
    unsafe {
        libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
    }
    let limit = libc::rlimit {
        rlim_cur: 4096,
        rlim_max: 4096,
    };
    let rc = unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &limit) };
    assert_eq!(rc, 0);
    let backups = copy.0.parent().unwrap().join("backups");
    let mut project = Project::open(&copy.0).unwrap();
    let query = literal("Mara", "Marian");
    let preview = replace::preview(&project, &query).unwrap();
    let err = replace::apply(&mut project, &backups, 2, &preview, &preview.ids()).unwrap_err();
    assert!(matches!(err, ApplyError::Backup(_)), "{err}");
    assert!(err.to_string().contains("File too large"), "{err}");
    assert_eq!(file_bytes(&copy.0), before);
    assert!(
        std::fs::read(project.data_dir(SCENE).join("content.rtf"))
            .unwrap()
            .windows(4)
            .any(|w| w == b"Mara")
    );
}

#[test]
fn failed_backup_does_not_change_the_project() {
    let copy = copy_fixture("nobackup");
    let backups = copy.0.parent().unwrap().join("backups");
    std::fs::write(&backups, b"not a directory").unwrap();
    let before = file_bytes(&copy.0);
    let mut project = Project::open(&copy.0).unwrap();
    let query = literal("Mara", "Marian");
    let preview = replace::preview(&project, &query).unwrap();
    let err = replace::apply(&mut project, &backups, 2, &preview, &preview.ids()).unwrap_err();
    assert!(matches!(err, ApplyError::Backup(_)));
    assert_eq!(
        bytes(&project, SCENE, "content.rtf"),
        before[&format!("Files/Data/{SCENE}/content.rtf")]
    );
    assert_eq!(file_bytes(&copy.0), before);
}

#[test]
fn write_failure_after_the_first_document_restores_from_the_backup() {
    let copy = copy_fixture("partial");
    let backups = copy.0.parent().unwrap().join("backups");
    let mut project = Project::open(&copy.0).unwrap();
    let draft = project.root_folder(Kind::Draft).unwrap().uuid.clone();
    let id = project.add_item(Kind::Text, "Later", &draft, None).unwrap();
    project
        .set_text(&id, &RichText::from_plain("Mara waits"))
        .unwrap();
    project.save().unwrap();
    let before = file_bytes(&copy.0);
    let dir = project.data_dir(&id);
    let _guard = ModeGuard::deny_write(dir.clone());
    let query = literal("Mara", "Marian");
    let preview = replace::preview(&project, &query).unwrap();
    assert!(preview.ids().len() >= 2);
    let err = replace::apply(&mut project, &backups, 2, &preview, &preview.ids()).unwrap_err();
    match err {
        ApplyError::Write { written, .. } => assert_eq!(written.len(), 1),
        other => panic!("{other}"),
    }
    let storm = std::fs::read(project.data_dir(SCENE).join("content.rtf")).unwrap();
    assert_ne!(storm, before[&format!("Files/Data/{SCENE}/content.rtf")]);
    let later = std::fs::read(dir.join("content.rtf")).unwrap();
    assert_eq!(later, before[&format!("Files/Data/{id}/content.rtf")]);
    let zips = zip_paths(&backups);
    assert_eq!(zips.len(), 1);
    assert_eq!(file_bytes(&unzip_stored(&zips[0])), before);
}

#[test]
fn a_long_manuscript_replaces_every_match_and_leaves_the_quiet_file() {
    let copy = copy_fixture("huge");
    let backups = copy.0.parent().unwrap().join("backups");
    let mut project = Project::open(&copy.0).unwrap();
    let draft = project.root_folder(Kind::Draft).unwrap().uuid.clone();
    let quiet = project.add_item(Kind::Text, "Quiet", &draft, None).unwrap();
    project
        .set_text(&quiet, &RichText::from_plain("Nothing to see here."))
        .unwrap();
    let body = "alpha ".repeat(8_000);
    let big = project.add_item(Kind::Text, "Big", &draft, None).unwrap();
    project
        .set_text(&big, &RichText::from_plain(&body))
        .unwrap();
    project.save().unwrap();
    let quiet_bytes = bytes(&project, &quiet, "content.rtf");
    let morning = bytes(&project, MORNING, "content.rtf");
    let query = literal("alpha", "beta");
    let preview = replace::preview(&project, &query).unwrap();
    let rows = preview
        .groups()
        .iter()
        .find(|g| g.id().uuid() == big)
        .unwrap()
        .rows()
        .len();
    assert_eq!(rows, 8_000);
    let before = file_bytes(&copy.0);
    replace::apply(&mut project, &backups, 2, &preview, &preview.ids()).unwrap();
    let text = Project::open(&copy.0)
        .unwrap()
        .text(&big)
        .unwrap()
        .plain_text();
    assert_eq!(text, "beta ".repeat(8_000));
    assert!(!text.contains("alpha"));
    assert_eq!(bytes(&project, &quiet, "content.rtf"), quiet_bytes);
    assert_eq!(bytes(&project, MORNING, "content.rtf"), morning);
    let zips = zip_paths(&backups);
    assert_eq!(zips.len(), 1);
    assert_eq!(file_bytes(&unzip_stored(&zips[0])), before);
}

#[test]
fn kill_during_apply_keeps_the_original_in_the_backup() {
    if let Ok(spec) = std::env::var("OMAQUILL_KILL_CHILD") {
        let (project, backups) = spec.split_once('\n').unwrap();
        let mut project = Project::open(Path::new(project)).unwrap();
        let query = literal("alpha", "beta");
        let preview = replace::preview(&project, &query).unwrap();
        let _ = replace::apply(
            &mut project,
            Path::new(backups),
            5,
            &preview,
            &preview.ids(),
        );
        std::process::exit(0);
    }

    let template = copy_fixture("kill-src");
    let mut project = Project::open(&template.0).unwrap();
    let draft = project.root_folder(Kind::Draft).unwrap().uuid.clone();
    let body = RichText::from_plain(&"alpha ".repeat(2_000));
    let mut ids = Vec::new();
    for i in 0..24 {
        let id = project
            .add_item(Kind::Text, &format!("Part {i}"), &draft, None)
            .unwrap();
        project.set_text(&id, &body).unwrap();
        ids.push(id);
    }
    project.save().unwrap();
    drop(project);
    let before = file_bytes(&template.0);
    let mut saw_mixed = false;
    for attempt in 0..4 {
        let root = template.0.parent().unwrap().join(format!("try-{attempt}"));
        let _ = std::fs::remove_dir_all(&root);
        let project_path = root.join("Lighthouse.scriv");
        copy_dir(&template.0, &project_path);
        let backups = root.join("backups");
        let docs: Vec<PathBuf> = ids
            .iter()
            .map(|id| project_path.join("Files/Data").join(id).join("content.rtf"))
            .collect();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("kill_during_apply_keeps_the_original_in_the_backup")
            .arg("--test-threads=1")
            .env(
                "OMAQUILL_KILL_CHILD",
                format!("{}\n{}", project_path.display(), backups.display()),
            )
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut killed = false;
        while Instant::now() < deadline {
            let (same, changed) = split_docs(&docs, &before, &project_path);
            if changed > 0 && same > 0 {
                child.kill().unwrap();
                killed = true;
                saw_mixed = true;
                break;
            }
            if child.try_wait().unwrap().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let status = child.wait().unwrap();
        if !killed {
            assert!(status.success(), "attempt {attempt} child failed: {status}");
        }
        let zips = zip_paths(&backups);
        if zips.is_empty() {
            assert_eq!(file_bytes(&project_path), before);
            continue;
        }
        assert_eq!(zips.len(), 1, "attempt {attempt}");
        assert_eq!(file_bytes(&unzip_stored(&zips[0])), before);
        for doc in &docs {
            let now = std::fs::read(doc).unwrap();
            let rel = doc
                .strip_prefix(&project_path)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if now == before[&rel] {
                continue;
            }
            let text = rtf::parse(&now).plain_text();
            assert_eq!(text, "beta ".repeat(2_000));
        }
        if saw_mixed {
            break;
        }
    }
    assert!(saw_mixed, "never killed between two document writes");
}

fn apply_all(project: &mut Project, backups: &Path, find: &str, replace_with: &str) -> PathBuf {
    let query = literal(find, replace_with);
    let preview = replace::preview(project, &query).unwrap();
    let ids = preview.ids();
    let outcome = replace::apply(project, backups, 5, &preview, &ids).unwrap();
    let ApplyOutcome::Done(report) = outcome else {
        panic!("{find} had no matches");
    };
    report.backup
}

struct ModeGuard {
    path: PathBuf,
    mode: u32,
}

impl ModeGuard {
    fn deny_write(path: PathBuf) -> Self {
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        let guard = ModeGuard { path, mode };
        let mut perms = std::fs::metadata(&guard.path).unwrap().permissions();
        perms.set_mode(mode & !0o222);
        std::fs::set_permissions(&guard.path, perms).unwrap();
        guard
    }
}

impl Drop for ModeGuard {
    fn drop(&mut self) {
        let mut perms = std::fs::metadata(&self.path).unwrap().permissions();
        perms.set_mode(self.mode);
        let _ = std::fs::set_permissions(&self.path, perms);
    }
}

fn file_bytes(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
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
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn unzip_stored(zip_path: &Path) -> PathBuf {
    let dest = zip_path.with_file_name(format!(
        "restored-{}",
        zip_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace(' ', "_")
    ));
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
        assert!(name_at + name_len + extra_len + size <= data.len());
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

fn zip_paths(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        if !dir.is_dir() {
            return;
        }
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "zip") {
                out.push(path);
            }
        }
    }
    walk(dir, &mut out);
    out.sort();
    out
}

fn part_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        if !dir.is_dir() {
            return;
        }
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().unwrap().is_dir() {
                walk(&path, out);
            } else if name.contains("omaquill-tmp") || name.ends_with("zip.part") {
                out.push(path);
            }
        }
    }
    walk(dir, &mut out);
    out
}

fn split_docs(docs: &[PathBuf], before: &BTreeMap<String, Vec<u8>>, root: &Path) -> (usize, usize) {
    let mut same = 0usize;
    let mut changed = 0usize;
    for doc in docs {
        let rel = doc
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let now = std::fs::read(doc).unwrap_or_default();
        if now == before[&rel] {
            same += 1;
        } else {
            changed += 1;
        }
    }
    (same, changed)
}

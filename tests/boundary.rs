//! The project boundary under a second writer. Another user who can write
//! into a shared project folder swaps files and folders for symlinks while
//! omaquill saves. Every test asserts that the files outside the project
//! keep their bytes and that no new file appears beside them.

use omaquill::project::Project;
use omaquill::rtf::RichText;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const SCENE: &str = "3C8B2F4D-5A6E-4F7B-9CAD-1E2F3A4B5C03";
const RACE_FOR: Duration = Duration::from_millis(2_500);

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(tag: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("omaquill-boundary-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/Lighthouse.scriv");
    copy_dir(&src, &dir.join("Lighthouse.scriv"));
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

/// Every file under `dir` with its bytes, symlinks named as links.
fn tree(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let ft = e.file_type().unwrap();
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_symlink() {
                let to = std::fs::read_link(e.path()).unwrap();
                out.push((e.path(), format!("link to {}", to.display())));
            } else {
                let bytes = std::fs::read(e.path()).unwrap();
                out.push((e.path(), String::from_utf8_lossy(&bytes).into_owned()));
            }
        }
    }
    out.sort();
    out
}

/// A private folder outside the project that mirrors `Files/Data/<SCENE>`.
fn private_tree(root: &Path) -> PathBuf {
    let private = root.join("private");
    let scene = private.join("Files/Data").join(SCENE);
    std::fs::create_dir_all(&scene).unwrap();
    for name in ["content.rtf", "notes.rtf", "synopsis.txt", "docs.checksum"] {
        std::fs::write(scene.join(name), format!("private {name}\n")).unwrap();
    }
    std::fs::write(private.join("Files/Data/docs.checksum"), "private sums\n").unwrap();
    std::fs::write(private.join("bashrc"), "export SECRET=1\n").unwrap();
    private
}

struct Swapper {
    stop: Arc<AtomicBool>,
    rounds: Arc<AtomicU64>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Swapper {
    /// Runs `swap` in a loop on its own thread until dropped.
    fn start(swap: impl Fn() + Send + 'static) -> Swapper {
        let stop = Arc::new(AtomicBool::new(false));
        let rounds = Arc::new(AtomicU64::new(0));
        let handle = {
            let (stop, rounds) = (stop.clone(), rounds.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    swap();
                    rounds.fetch_add(1, Ordering::Relaxed);
                }
            })
        };
        Swapper {
            stop,
            rounds,
            handle: Some(handle),
        }
    }

    fn finish(mut self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.take().unwrap().join().unwrap();
        self.rounds.load(Ordering::Relaxed)
    }
}

/// Replaces `at` with a symlink to `to` in one rename, as a second user
/// would: the name is never missing.
fn plant_link(to: &Path, at: &Path) {
    let tmp = at.with_extension("swap-link");
    let _ = std::fs::remove_file(&tmp);
    if std::os::unix::fs::symlink(to, &tmp).is_ok() {
        let _ = std::fs::rename(&tmp, at);
    }
}

/// Edits and saves `p` in a loop for [`RACE_FOR`]. `edit` reports whether
/// its writes went through.
fn autosave_loop(p: &mut Project, mut edit: impl FnMut(&mut Project, u64) -> bool) -> Saves {
    let mut saves = Saves::default();
    let started = Instant::now();
    let mut n = 0u64;
    while started.elapsed() < RACE_FOR {
        n += 1;
        if edit(p, n) {
            saves.edits += 1;
        } else {
            saves.refused += 1;
        }
        if p.save().is_ok() {
            saves.saves += 1;
        }
    }
    saves
}

#[derive(Default)]
struct Saves {
    /// Rounds where every write in the edit went through.
    edits: u64,
    /// Rounds where an edit was refused.
    refused: u64,
    /// Rounds where `Project::save` went through.
    saves: u64,
}

fn report(name: &str, rounds: u64, s: Saves) {
    eprintln!(
        "{name}: {rounds} swaps, {} edits written, {} edits refused, {} saves written",
        s.edits, s.refused, s.saves
    );
    assert!(rounds > 100, "{name}: the swapper barely ran ({rounds})");
    assert!(s.edits > 100, "{name}: autosave barely wrote ({})", s.edits);
}

#[test]
fn a_chapter_swapped_for_a_link_during_autosave_never_writes_outside() {
    let s = scratch("race-file");
    let project = s.0.join("Lighthouse.scriv");
    let private = private_tree(&s.0);
    let before = tree(&private);
    let chapter = project.join("Files/Data").join(SCENE).join("content.rtf");
    let notes = project.join("Files/Data").join(SCENE).join("notes.rtf");
    let sums = project.join("Files/Data/docs.checksum");
    let target = private.join("bashrc");
    let swapper = Swapper::start(move || {
        plant_link(&target, &chapter);
        plant_link(&target, &notes);
        plant_link(&target, &sums);
    });
    let mut p = Project::open(&project).unwrap();
    let saves = autosave_loop(&mut p, |p, n| {
        let text = RichText::from_plain(&format!("draft {n}"));
        p.set_text(SCENE, &text).is_ok() && p.set_notes(SCENE, &text).is_ok()
    });
    let rounds = swapper.finish();
    report("file swap", rounds, saves);
    assert_eq!(tree(&private), before, "a save wrote outside the project");
}

#[test]
fn an_item_folder_swapped_for_a_link_during_autosave_never_writes_outside() {
    let s = scratch("race-dir");
    let project = s.0.join("Lighthouse.scriv");
    let private = private_tree(&s.0);
    let before = tree(&private);
    let data = project.join("Files/Data");
    let item = data.join(SCENE);
    let parked = data.join("parked");
    let outside_item = private.join("Files/Data").join(SCENE);
    let files = project.join("Files");
    let files_parked = project.join("Files-parked");
    let outside_files = private.join("Files");
    let swapper = Swapper::start(move || {
        // The item folder, then the whole of Files, swapped for links and
        // put back.
        if std::fs::rename(&item, &parked).is_ok() {
            let _ = std::os::unix::fs::symlink(&outside_item, &item);
            std::thread::yield_now();
            let _ = std::fs::remove_file(&item);
            let _ = std::fs::rename(&parked, &item);
        }
        if std::fs::rename(&files, &files_parked).is_ok() {
            let _ = std::os::unix::fs::symlink(&outside_files, &files);
            std::thread::yield_now();
            let _ = std::fs::remove_file(&files);
            let _ = std::fs::rename(&files_parked, &files);
        }
    });
    let mut p = Project::open(&project).unwrap();
    let saves = autosave_loop(&mut p, |p, n| {
        let text = RichText::from_plain(&format!("draft {n}"));
        let synopsis = if n % 2 == 0 { "" } else { "a synopsis" };
        p.set_text(SCENE, &text).is_ok()
            && p.set_notes(SCENE, &text).is_ok()
            && p.set_synopsis(SCENE, synopsis).is_ok()
    });
    let rounds = swapper.finish();
    report("folder swap", rounds, saves);
    assert_eq!(tree(&private), before, "a save wrote outside the project");
}

/// Backups read the project while a second user swaps a chapter for a link
/// to a private file. No backup holds the private bytes.
#[test]
fn a_backup_raced_by_link_swaps_never_reads_outside() {
    let s = scratch("race-backup");
    let project = s.0.join("Lighthouse.scriv");
    let private = private_tree(&s.0);
    let chapter = project.join("Files/Data").join(SCENE).join("content.rtf");
    let data = project.join("Files/Data");
    let item = data.join(SCENE);
    let parked = data.join("parked");
    let outside_item = private.join("Files/Data").join(SCENE);
    let secret = private.join("bashrc");
    let swapper = Swapper::start(move || {
        plant_link(&secret, &chapter);
        let _ = std::fs::remove_file(&chapter);
        let _ = std::fs::write(&chapter, "{\\rtf1 mine}");
        if std::fs::rename(&item, &parked).is_ok() {
            let _ = std::os::unix::fs::symlink(&outside_item, &item);
            std::thread::yield_now();
            let _ = std::fs::remove_file(&item);
            let _ = std::fs::rename(&parked, &item);
        }
    });
    let backups = s.0.join("backups");
    let (mut ok, mut failed) = (0, 0);
    let started = Instant::now();
    let mut n = 0;
    while started.elapsed() < RACE_FOR {
        n += 1;
        let now = format!("2026-10-21 07:{:02}:{:02} -0700", (n / 60) % 60, n % 60);
        match omaquill::project::backup_at(&project, &backups, 5, &now) {
            Ok(zip) => {
                ok += 1;
                let bytes = std::fs::read(&zip).unwrap();
                for leaked in [
                    &b"export SECRET"[..],
                    b"private content.rtf",
                    b"private notes.rtf",
                ] {
                    assert!(
                        !bytes.windows(leaked.len()).any(|w| w == leaked),
                        "backup {} holds {}",
                        zip.display(),
                        String::from_utf8_lossy(leaked)
                    );
                }
            }
            Err(_) => failed += 1,
        }
    }
    let rounds = swapper.finish();
    eprintln!("backup race: {rounds} swaps, {ok} backups, {failed} failed");
    assert!(rounds > 100 && ok > 10, "the race barely ran");
}

#[test]
fn a_folder_link_that_stays_inside_the_project_is_refused_too() {
    let s = scratch("inside-folder-link");
    let project = s.0.join("Lighthouse.scriv");
    let item = project.join("Files/Data").join(SCENE);
    let moved = project.join("Files/moved");
    std::fs::rename(&item, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &item).unwrap();
    let before = tree(&moved);
    let mut p = Project::open(&project).unwrap();
    let err = p
        .set_text(SCENE, &RichText::from_plain("through the link"))
        .unwrap_err();
    assert!(err.to_string().contains("won't follow it"), "{err}");
    assert!(p.text(SCENE).is_err());
    assert!(p.set_synopsis(SCENE, "").is_err());
    assert_eq!(tree(&moved), before);
}

#[test]
fn a_new_project_is_not_made_where_a_link_already_is() {
    let s = scratch("create-link");
    let elsewhere = s.0.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let path = s.0.join("New.scriv");
    std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
    let err = Project::create(&path)
        .err()
        .expect("created through a link");
    assert!(err.to_string().contains("already exists"), "{err}");
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    // A dangling link is an entry too.
    let dangling = s.0.join("Dangling.scriv");
    std::os::unix::fs::symlink(s.0.join("nowhere"), &dangling).unwrap();
    assert!(Project::create(&dangling).is_err());
    assert!(!s.0.join("nowhere").exists());
    // And a plain new path still works.
    let p = Project::create(&s.0.join("Fresh.scriv")).unwrap();
    assert!(!p.items().is_empty());
}

#[test]
fn emptying_the_trash_removes_links_inside_an_item_as_links() {
    let s = scratch("trash-links");
    let project = s.0.join("Lighthouse.scriv");
    let private = private_tree(&s.0);
    let before = tree(&private);
    let item = project.join("Files/Data").join(SCENE);
    std::os::unix::fs::symlink(&private, item.join("private-dir")).unwrap();
    std::os::unix::fs::symlink(private.join("bashrc"), item.join("bashrc")).unwrap();
    let mut p = Project::open(&project).unwrap();
    p.trash(SCENE).unwrap();
    p.empty_trash().unwrap();
    assert!(std::fs::symlink_metadata(&item).is_err());
    assert_eq!(tree(&private), before);
}

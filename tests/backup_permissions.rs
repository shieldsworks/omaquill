//! Backup permissions under the common umask `022`. This file is its own
//! test binary with one test, so the umask it sets reaches no other test.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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

fn make_private(path: &Path) {
    let mode = if path.is_dir() { 0o700 } else { 0o600 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    if path.is_dir() {
        for e in std::fs::read_dir(path).unwrap() {
            make_private(&e.unwrap().path());
        }
    }
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn backups_of_a_private_project_stay_private() {
    // SAFETY: umask only sets this process's file creation mask. This
    // binary runs one test, so no other thread is creating files.
    unsafe { libc::umask(0o022) };
    let dir = std::env::temp_dir().join(format!("omaquill-backup-mode-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let scratch = Scratch(dir.clone());
    let project = dir.join("Lighthouse.scriv");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/Lighthouse.scriv"),
        &project,
    );
    make_private(&project);
    let backups = dir.join("share/backups");

    let zip =
        omaquill::project::backup_at(&project, &backups, 3, "2026-10-26 07:00:00 -0700").unwrap();
    let folder = zip.parent().unwrap().to_path_buf();
    assert_eq!(mode(&backups), 0o700, "backups folder");
    assert_eq!(mode(&folder), 0o700, "project backup folder");
    assert_eq!(mode(&zip), 0o600, "backup archive");
    assert_eq!(
        mode(&dir.join("share")),
        0o755,
        "an ancestor keeps its mode"
    );

    set_mode(&backups, 0o755);
    set_mode(&folder, 0o755);
    set_mode(&zip, 0o644);
    let target = dir.join("elsewhere.txt");
    std::fs::write(&target, "elsewhere").unwrap();
    set_mode(&target, 0o644);
    let planted = folder.join("2026-10-25 07-00-00.zip");
    std::os::unix::fs::symlink(&target, &planted).unwrap();
    let unrelated = folder.join("notes.txt");
    std::fs::write(&unrelated, "notes").unwrap();
    set_mode(&unrelated, 0o644);
    let staging = folder.join(".backup.999999999-0.omaquill-tmp");
    std::fs::write(&staging, "partial").unwrap();
    set_mode(&staging, 0o644);
    let sibling = backups.join("Other-00000000");
    std::fs::create_dir(&sibling).unwrap();
    set_mode(&sibling, 0o755);
    let sibling_zip = sibling.join("2026-10-25 07-00-00.zip");
    std::fs::write(&sibling_zip, "zip").unwrap();
    set_mode(&sibling_zip, 0o644);
    let next =
        omaquill::project::backup_at(&project, &backups, 3, "2026-10-26 07:00:01 -0700").unwrap();
    assert_eq!(
        mode(&backups),
        0o700,
        "an existing backups folder is tightened"
    );
    assert_eq!(
        mode(&folder),
        0o700,
        "an existing project folder is tightened"
    );
    assert_eq!(mode(&next), 0o600, "the next archive");
    assert_eq!(mode(&zip), 0o600, "an existing archive is narrowed");
    assert!(
        std::fs::symlink_metadata(&planted)
            .unwrap()
            .file_type()
            .is_symlink(),
        "a planted link stays a link"
    );
    assert_eq!(std::fs::read_link(&planted).unwrap(), target);
    assert_eq!(mode(&target), 0o644, "a link's target keeps its mode");
    assert_eq!(std::fs::read(&target).unwrap(), b"elsewhere");
    assert_eq!(mode(&unrelated), 0o644, "an unrelated file keeps its mode");
    assert_eq!(mode(&staging), 0o600, "a leftover staging file is narrowed");
    assert_eq!(std::fs::read(&staging).unwrap(), b"partial", "and kept");
    assert_eq!(
        mode(&sibling),
        0o755,
        "another project's folder is not changed"
    );
    assert_eq!(
        mode(&sibling_zip),
        0o644,
        "another project's archive is not changed"
    );
    assert_eq!(
        mode(&backups),
        0o700,
        "the closed backups folder covers other projects' folders"
    );

    let linked = dir.join("linked/backups");
    std::fs::create_dir_all(dir.join("linked")).unwrap();
    std::os::unix::fs::symlink(&backups, &linked).unwrap();
    let e = omaquill::project::backup_at(&project, &linked, 3, "2026-10-26 07:00:02 -0700")
        .unwrap_err()
        .to_string();
    assert!(
        e.contains(&linked.display().to_string()) && e.contains("is a symlink"),
        "{e}"
    );
    drop(scratch);
}

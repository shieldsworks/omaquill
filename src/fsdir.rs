//! Files reached through an open folder.
//!
//! A path is resolved by the kernel each time it is used. When a second
//! user can write into a folder on that path, a check on the path and a
//! later write to it can land on two different files. [`Dir`] holds an open
//! descriptor instead, and every file under it is reached one name at a
//! time from that descriptor with `openat`, `O_NOFOLLOW` and the other `*at`
//! calls.
//!
//! - A symlink is never followed. A folder on the way that is a symlink
//!   refuses the call with `PermissionDenied`, and [`is_link_refusal`] tells
//!   that refusal apart. A file that is a symlink is not read, and a write
//!   replaces the link itself with an ordinary file. The link's target keeps
//!   its bytes.
//! - `..`, an absolute path, and a name with a NUL byte are refused with
//!   `InvalidInput`, so a relative path stays under the folder it starts in.
//! - [`Dir::write_atomic_if`] writes a new temporary file in the target's
//!   folder, syncs it, and renames it onto the name with `renameat` in that
//!   same folder. A rename replaces the directory entry. It never writes
//!   into the file the entry named before, so neither a symlink nor a hard
//!   link at that name carries the write anywhere else.
//!
//! [`Dir::open`] is the one call that resolves a path. Its caller names the
//! folder: the project the user opened, or the folder of a file the user
//! chose.

use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// An open folder. Every call below resolves names from this descriptor.
#[derive(Debug)]
pub struct Dir {
    fd: OwnedFd,
    /// The path the folder was reached by. Messages show it. No call opens it.
    shown: PathBuf,
}

/// What a name in a folder is, read without following a symlink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Link,
    /// A FIFO, socket or device.
    Other,
}

/// What [`Dir::stat`] reads about one name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stat {
    kind: Kind,
    mode: u32,
    uid: u32,
}

/// The permission bits a replacement for `existing` takes, written by the
/// user `me`. Only a regular file that `me` owns passes its bits on. A file
/// someone else put at the name, with `0o666` say, does not make the new
/// file readable to them. `None` leaves the mode `create_new` gave.
fn inherited_mode(existing: Option<Stat>, me: u32) -> Option<u32> {
    existing
        .filter(|st| st.kind == Kind::File && st.uid == me)
        .map(|st| st.mode)
}

/// The longest name one folder entry can have, in bytes.
const NAME_MAX: usize = 255;

/// `.<name>.<pid>-<n>.omaquill-tmp`, with `name` cut short so the whole
/// stays within [`NAME_MAX`] bytes.
fn temp_name(name: &OsStr, n: u64) -> OsString {
    let tail = format!(".{}-{n}.omaquill-tmp", std::process::id());
    let room = NAME_MAX - 1 - tail.len();
    let b = name.as_bytes();
    let mut out = Vec::with_capacity(NAME_MAX);
    out.push(b'.');
    out.extend_from_slice(&b[..b.len().min(room)]);
    out.extend_from_slice(tail.as_bytes());
    OsString::from_vec(out)
}

/// The error a symlink on the way gets. [`is_link_refusal`] finds it.
#[derive(Debug)]
struct LinkRefused(PathBuf);

impl std::fmt::Display for LinkRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is a symlink; omaquill won't follow it",
            self.0.display()
        )
    }
}

impl std::error::Error for LinkRefused {}

/// Whether `e` is the refusal of a symlink, as opposed to a folder the user
/// may not write.
pub fn is_link_refusal(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<LinkRefused>())
}

fn link_refused(shown: PathBuf) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, LinkRefused(shown))
}

fn invalid(what: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, what)
}

/// `name` as one entry of a folder: not empty, not `.` or `..`, with no `/`
/// and no NUL.
fn entry_name(name: &OsStr) -> io::Result<CString> {
    let b = name.as_bytes();
    if b.is_empty() || b == b"." || b == b".." || b.contains(&b'/') {
        return Err(invalid(format!("{name:?} is not a file name")));
    }
    CString::new(b).map_err(|_| invalid(format!("{name:?} contains a NUL byte")))
}

/// The names in `rel`, a path under a folder. `.` parts are dropped; `..`,
/// a root, or a prefix are refused.
fn names(rel: &Path) -> io::Result<Vec<&OsStr>> {
    rel.components()
        .filter_map(|c| match c {
            Component::CurDir => None,
            Component::Normal(n) => Some(Ok(n)),
            _ => Some(Err(invalid(format!(
                "{} leaves the folder it starts in",
                rel.display()
            )))),
        })
        .collect()
}

/// `rel` split into its folder and its last name.
fn split(rel: &Path) -> io::Result<(Vec<&OsStr>, &OsStr)> {
    let mut parts = names(rel)?;
    let last = parts
        .pop()
        .ok_or_else(|| invalid(format!("{} names no file", rel.display())))?;
    Ok((parts, last))
}

fn cvt(r: libc::c_int) -> io::Result<()> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

impl Dir {
    /// Opens the folder at `path`, following symlinks in `path` this once.
    pub fn open(path: &Path) -> io::Result<Dir> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY)
            .open(path)?;
        Ok(Dir {
            fd: file.into(),
            shown: path.to_path_buf(),
        })
    }

    /// The path this folder was reached by, for messages.
    pub fn path(&self) -> &Path {
        &self.shown
    }

    /// A new descriptor for this folder with its own read position.
    fn reopen(&self) -> io::Result<Dir> {
        let fd = self.openat(c".", libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
        Ok(Dir {
            fd,
            shown: self.shown.clone(),
        })
    }

    fn openat(&self, name: &CStr, flags: libc::c_int, mode: libc::mode_t) -> io::Result<OwnedFd> {
        // SAFETY: `self.fd` is open for the life of `self` and `name` is a
        // NUL-terminated string that outlives the call. openat returns -1
        // or a new descriptor that nothing else owns, and only the second
        // is handed to `OwnedFd`.
        // A signal during an open on NFS or FUSE returns EINTR; the open
        // is then tried again.
        unsafe {
            loop {
                let fd = libc::openat(
                    self.fd.as_raw_fd(),
                    name.as_ptr(),
                    flags | libc::O_CLOEXEC,
                    libc::c_uint::from(mode),
                );
                if fd >= 0 {
                    return Ok(OwnedFd::from_raw_fd(fd));
                }
                let err = io::Error::last_os_error();
                if err.kind() != io::ErrorKind::Interrupted {
                    return Err(err);
                }
            }
        }
    }

    /// The kind, permission bits and owner of `name`, or `None` when it is
    /// absent. A symlink is reported as [`Kind::Link`], not followed.
    fn stat(&self, name: &CStr) -> io::Result<Option<Stat>> {
        // SAFETY: `libc::stat` is a C struct of integers, so all-zero is a
        // valid value. fstatat writes only into `st`. `self.fd` is open and
        // `name` is NUL-terminated for the whole call.
        let (r, st) = unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            let r = libc::fstatat(
                self.fd.as_raw_fd(),
                name.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            );
            (r, st)
        };
        match cvt(r) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        }
        let kind = match st.st_mode & libc::S_IFMT {
            libc::S_IFREG => Kind::File,
            libc::S_IFDIR => Kind::Dir,
            libc::S_IFLNK => Kind::Link,
            _ => Kind::Other,
        };
        Ok(Some(Stat {
            kind,
            mode: st.st_mode & 0o777,
            uid: st.st_uid,
        }))
    }

    /// The child folder `name`, created first when `create` is set and it
    /// is absent. A symlink in its place is refused.
    fn child(&self, name: &OsStr, create: bool) -> io::Result<Dir> {
        let c = entry_name(name)?;
        let shown = self.shown.join(name);
        if create {
            match self.mkdir(&c, 0o777) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        match self.openat(&c, libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW, 0) {
            Ok(fd) => Ok(Dir { fd, shown }),
            // Linux answers `O_DIRECTORY | O_NOFOLLOW` on a symlink with
            // ENOTDIR, and ELOOP elsewhere. The open has already failed
            // either way; the stat only picks the message.
            Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => {
                match self.stat(&c) {
                    Ok(Some(Stat {
                        kind: Kind::Link, ..
                    })) => Err(link_refused(shown)),
                    _ => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }

    fn mkdir(&self, name: &CStr, mode: libc::mode_t) -> io::Result<()> {
        // SAFETY: `self.fd` is open and `name` is NUL-terminated for the
        // whole call. mkdirat reads nothing else.
        cvt(unsafe { libc::mkdirat(self.fd.as_raw_fd(), name.as_ptr(), mode) })
    }

    /// The folder at `rel` under this one. Each folder on the way is opened
    /// from the one before it, and a symlink anywhere on the way is refused.
    /// With `create`, absent folders are made.
    pub fn walk(&self, rel: &Path, create: bool) -> io::Result<Dir> {
        let mut dir = self.reopen()?;
        for name in names(rel)? {
            dir = dir.child(name, create)?;
        }
        Ok(dir)
    }

    fn walk_parts(&self, parts: &[&OsStr], create: bool) -> io::Result<Dir> {
        let mut dir = self.reopen()?;
        for name in parts {
            dir = dir.child(name, create)?;
        }
        Ok(dir)
    }

    /// Makes the folder `name` here. An existing entry of that name is an
    /// `AlreadyExists` error. The new folder is opened without following a
    /// symlink, so a link raced into its place is refused.
    pub fn create_dir_new(&self, name: &OsStr) -> io::Result<Dir> {
        self.mkdir(&entry_name(name)?, 0o777)?;
        self.child(name, false)
    }

    /// [`Dir::create_dir_new`] with mode `0o700`: only this user can list
    /// or open what is put in it.
    pub fn create_private_dir_new(&self, name: &OsStr) -> io::Result<Dir> {
        self.mkdir(&entry_name(name)?, 0o700)?;
        self.child(name, false)
    }

    /// Bytes free for this user on the filesystem holding this folder.
    pub fn free_bytes(&self) -> io::Result<u64> {
        // SAFETY: `libc::statvfs` is a C struct of integers, so all-zero is
        // a valid value. fstatvfs writes only into `st`, and `self.fd` is
        // open for the whole call.
        let (r, st) = unsafe {
            let mut st: libc::statvfs = std::mem::zeroed();
            let r = libc::fstatvfs(self.fd.as_raw_fd(), &mut st);
            (r, st)
        };
        cvt(r)?;
        Ok(st.f_bavail.saturating_mul(st.f_frsize))
    }

    /// A second descriptor for this folder, with its own read position. It
    /// can move to another thread.
    pub fn try_clone(&self) -> io::Result<Dir> {
        self.reopen()
    }

    /// What `rel` is, without following a symlink at its end. `None` when it
    /// or a folder on the way is absent.
    pub fn kind(&self, rel: &Path) -> io::Result<Option<Kind>> {
        let (parts, name) = split(rel)?;
        let dir = match self.walk_parts(&parts, false) {
            Ok(dir) => dir,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(dir.stat(&entry_name(name)?)?.map(|st| st.kind))
    }

    /// Opens the regular file at `rel` for reading. A symlink, a FIFO, a
    /// device or a folder is refused. A FIFO would block `open` until a
    /// writer appears, so the open is non-blocking and the opened file's
    /// own type decides; a regular file is then switched to blocking mode.
    /// The file stays the one that was checked, whatever happens to its
    /// name later.
    pub fn open_file(&self, rel: &Path) -> io::Result<File> {
        let (parts, name) = split(rel)?;
        let dir = self.walk_parts(&parts, false)?;
        let c = entry_name(name)?;
        let shown = dir.shown.join(name);
        let fd = match dir.openat(&c, libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK, 0) {
            Ok(fd) => fd,
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => return Err(link_refused(shown)),
            Err(e) => return Err(e),
        };
        let file = File::from(fd);
        if !file.metadata()?.is_file() {
            return Err(invalid(format!(
                "{} is not a regular file",
                shown.display()
            )));
        }
        let raw = file.as_raw_fd();
        // SAFETY: fcntl reads and sets the status flags of `raw`, which
        // `file` owns and keeps open for the whole block.
        let set = unsafe {
            let flags = libc::fcntl(raw, libc::F_GETFL);
            if flags < 0 {
                flags
            } else {
                libc::fcntl(raw, libc::F_SETFL, flags & !libc::O_NONBLOCK)
            }
        };
        cvt(set)?;
        Ok(file)
    }

    /// The bytes of the regular file at `rel`; see [`Dir::open_file`].
    pub fn read(&self, rel: &Path) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.open_file(rel)?.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    /// The names in this folder, without `.` and `..`, in no set order.
    pub fn entries(&self) -> io::Result<Vec<OsString>> {
        let raw = self.reopen()?.fd.into_raw_fd();
        // SAFETY: `raw` is a descriptor this function owns. fdopendir takes
        // it over on success, and closedir closes it once. On failure it is
        // still ours and `OwnedFd` closes it. Each `dirent` readdir returns
        // stays valid until the next readdir on the same stream, and its
        // name is copied out before then. readdir returns null both at the
        // end and on an error. Only an error sets errno, so errno is cleared
        // before each call and read after a null. `__errno_location` is
        // this thread's errno.
        unsafe {
            let stream = libc::fdopendir(raw);
            if stream.is_null() {
                let err = io::Error::last_os_error();
                drop(OwnedFd::from_raw_fd(raw));
                return Err(err);
            }
            let mut out = Vec::new();
            let mut failed = None;
            loop {
                *libc::__errno_location() = 0;
                let entry = libc::readdir(stream);
                if entry.is_null() {
                    let errno = *libc::__errno_location();
                    if errno != 0 {
                        failed = Some(io::Error::from_raw_os_error(errno));
                    }
                    break;
                }
                let name = CStr::from_ptr((*entry).d_name.as_ptr()).to_bytes();
                if name != b"." && name != b".." {
                    out.push(OsStr::from_bytes(name).to_os_string());
                }
            }
            libc::closedir(stream);
            match failed {
                Some(err) => Err(err),
                None => Ok(out),
            }
        }
    }

    /// Creates `name` here as a new, empty file with `O_EXCL`, so an entry
    /// already at that name, a symlink included, is an `AlreadyExists`
    /// error.
    pub fn create_new(&self, name: &OsStr) -> io::Result<File> {
        let fd = self.openat(
            &entry_name(name)?,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
            0o666,
        )?;
        Ok(File::from(fd))
    }

    /// Links the file `from` here to the new name `to`, both in this folder.
    /// An entry already at `to` is an `AlreadyExists` error. A symlink at
    /// `from` is linked as the link.
    pub fn link(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        let (from, to) = (entry_name(from)?, entry_name(to)?);
        let fd = self.fd.as_raw_fd();
        // SAFETY: `fd` is open and both names are NUL-terminated for the
        // whole call. Flags 0 do not follow a symlink at `from`.
        cvt(unsafe { libc::linkat(fd, from.as_ptr(), fd, to.as_ptr(), 0) })
    }

    fn unlink(&self, name: &CStr, flags: libc::c_int) -> io::Result<()> {
        // SAFETY: `self.fd` is open and `name` is NUL-terminated for the
        // whole call.
        cvt(unsafe { libc::unlinkat(self.fd.as_raw_fd(), name.as_ptr(), flags) })
    }

    fn rename(&self, from: &CStr, to: &CStr) -> io::Result<()> {
        let fd = self.fd.as_raw_fd();
        // SAFETY: `fd` is open and both names are NUL-terminated for the
        // whole call.
        cvt(unsafe { libc::renameat(fd, from.as_ptr(), fd, to.as_ptr()) })
    }

    /// Removes the entry at `rel`. A symlink goes as a link. An absent entry
    /// is not an error. A folder is refused.
    pub fn remove_file(&self, rel: &Path) -> io::Result<()> {
        let (parts, name) = split(rel)?;
        let dir = match self.walk_parts(&parts, false) {
            Ok(dir) => dir,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        match dir.unlink(&entry_name(name)?, 0) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    /// Removes the folder at `rel` and everything in it. A symlink at `rel`
    /// goes as a link, and a symlink inside goes as a link. An absent
    /// entry is not an error.
    pub fn remove_tree(&self, rel: &Path) -> io::Result<()> {
        let (parts, name) = split(rel)?;
        let dir = match self.walk_parts(&parts, false) {
            Ok(dir) => dir,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        dir.remove_entry(name)
    }

    fn remove_entry(&self, name: &OsStr) -> io::Result<()> {
        let c = entry_name(name)?;
        match self.stat(&c)?.map(|st| st.kind) {
            None => Ok(()),
            Some(Kind::Dir) => {
                let inner = self.child(name, false)?;
                for entry in inner.entries()? {
                    inner.remove_entry(&entry)?;
                }
                self.unlink(&c, libc::AT_REMOVEDIR)
            }
            Some(_) => match self.unlink(&c, 0) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                other => other,
            },
        }
    }

    /// Calls `fsync` on the folder, so a rename in it survives a power cut.
    pub fn sync(&self) -> io::Result<()> {
        File::from(self.fd.try_clone()?).sync_all()
    }

    /// Replaces the file at `rel` with `bytes`; see [`Dir::write_atomic_if`].
    /// With `create_dirs`, absent folders on the way are made.
    pub fn write_atomic(&self, rel: &Path, bytes: &[u8], create_dirs: bool) -> io::Result<()> {
        let (parts, name) = split(rel)?;
        self.walk_parts(&parts, create_dirs)?
            .write_atomic_if(name, bytes, || true)
            .map(|_| ())
    }

    /// Replaces the file `name` in this folder with `bytes`.
    ///
    /// The bytes go to a new file `.<name>.<pid>-<n>.omaquill-tmp` in this
    /// folder, created with `O_EXCL`, with `name` cut short when the whole
    /// would pass 255 bytes. When `name` is a regular file this user owns,
    /// the new file takes its permission bits (`0o777` of them); otherwise
    /// it keeps the mode `0o666` less the umask. The new file is
    /// written and synced. `commit` is asked once the bytes are on disk;
    /// when it answers false the new file is removed, `name` is left as it
    /// was, and the result is `Ok(false)`. Otherwise `renameat` puts the new
    /// file at `name` and the folder is synced. A failure removes the new
    /// file. A folder sync error does not fail the write.
    ///
    /// A symlink at `name` is replaced by the new file and its target is
    /// left alone. A folder, FIFO, socket or device at `name` is refused
    /// with `InvalidInput` before anything is written.
    pub fn write_atomic_if(
        &self,
        name: &OsStr,
        bytes: &[u8],
        commit: impl FnOnce() -> bool,
    ) -> io::Result<bool> {
        static N: AtomicU64 = AtomicU64::new(0);
        let target = entry_name(name)?;
        let existing = self.stat(&target)?;
        match existing.map(|st| st.kind) {
            None | Some(Kind::Link | Kind::File) => {}
            Some(Kind::Dir | Kind::Other) => {
                return Err(invalid(format!(
                    "{} is not a regular file",
                    self.shown.join(name).display()
                )));
            }
        }
        let mut tries = 0;
        let (tmp, mut file) = loop {
            tries += 1;
            let tmp = temp_name(name, N.fetch_add(1, Ordering::Relaxed));
            match self.create_new(&tmp) {
                Ok(file) => break (entry_name(&tmp)?, file),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && tries < 1_000 => continue,
                Err(e) => return Err(e),
            }
        };
        let written = (|| {
            // The new file belongs to this process's user, so its owner is
            // the user to compare with.
            let me = file.metadata()?.uid();
            if let Some(mode) = inherited_mode(existing, me) {
                file.set_permissions(std::fs::Permissions::from_mode(mode))?;
            }
            file.write_all(bytes)?;
            file.sync_all()?;
            if !commit() {
                return Ok(false);
            }
            self.rename(&tmp, &target).map(|()| true)
        })();
        if !matches!(written, Ok(true)) {
            let _ = self.unlink(&tmp, 0);
            return written;
        }
        let _ = self.sync();
        Ok(true)
    }
}

/// Replaces the file at `path`, outside any project, with `bytes`.
/// [`write_atomic_if`] with a `commit` that always answers true.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_if(path, bytes, || true).map(|_| ())
}

/// Replaces the file at `path` with `bytes` through
/// [`Dir::write_atomic_if`] in `path`'s folder. The folder is opened once,
/// following symlinks in the folder's own path; that path is the one the
/// user chose. A symlink at `path` itself is replaced, not followed.
pub fn write_atomic_if(
    path: &Path,
    bytes: &[u8],
    commit: impl FnOnce() -> bool,
) -> io::Result<bool> {
    let name = path
        .file_name()
        .ok_or_else(|| invalid(format!("{} names no file", path.display())))?;
    let folder = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    Dir::open(folder)?.write_atomic_if(name, bytes, commit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A folder with `inside/` (the folder a [`Dir`] opens) and `outside/`
    /// holding `secret`.
    fn scratch(tag: &str) -> (Scratch, Dir, PathBuf) {
        let dir = std::env::temp_dir().join(format!("omaquill-fsdir-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("inside/sub")).unwrap();
        std::fs::create_dir_all(dir.join("outside")).unwrap();
        std::fs::write(dir.join("outside/secret"), "secret").unwrap();
        let root = Dir::open(&dir.join("inside")).unwrap();
        (Scratch(dir.clone()), root, dir)
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    fn mkfifo(p: &Path) {
        let made = std::process::Command::new("mkfifo")
            .arg(p)
            .status()
            .unwrap();
        assert!(made.success());
    }

    #[test]
    fn a_folder_on_the_way_that_is_a_link_is_refused_even_inside() {
        let (_s, root, dir) = scratch("mid-link");
        symlink(dir.join("outside"), dir.join("inside/out")).unwrap();
        symlink(dir.join("inside/sub"), dir.join("inside/alias")).unwrap();
        for via in ["out", "alias"] {
            let rel = Path::new(via).join("secret");
            let e = root.write_atomic(&rel, b"x", true).unwrap_err();
            assert!(is_link_refusal(&e), "{via}: {e}");
            assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
            assert!(is_link_refusal(&root.read(&rel).unwrap_err()));
            assert!(is_link_refusal(&root.remove_file(&rel).unwrap_err()));
            assert!(is_link_refusal(&root.remove_tree(&rel).unwrap_err()));
            assert!(is_link_refusal(
                &root.walk(Path::new(via), true).unwrap_err()
            ));
        }
        assert_eq!(
            std::fs::read(dir.join("outside/secret")).unwrap(),
            b"secret"
        );
        assert_eq!(names_in(&dir.join("outside")), ["secret"]);
        assert!(names_in(&dir.join("inside/sub")).is_empty());
    }

    #[test]
    fn a_file_that_is_a_link_is_not_read_and_a_write_replaces_the_link() {
        let (_s, root, dir) = scratch("end-link");
        let link = dir.join("inside/chapter");
        symlink(dir.join("outside/secret"), &link).unwrap();
        let e = root.read(Path::new("chapter")).unwrap_err();
        assert!(is_link_refusal(&e), "{e}");
        root.write_atomic(Path::new("chapter"), b"mine", false)
            .unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().is_file());
        assert_eq!(std::fs::read(&link).unwrap(), b"mine");
        assert_eq!(
            std::fs::read(dir.join("outside/secret")).unwrap(),
            b"secret"
        );
        assert_eq!(names_in(&dir.join("inside")), ["chapter", "sub"]);
    }

    #[test]
    fn a_hard_link_at_the_target_is_replaced_not_written_through() {
        let (_s, root, dir) = scratch("hard-link");
        std::fs::hard_link(dir.join("outside/secret"), dir.join("inside/chapter")).unwrap();
        root.write_atomic(Path::new("chapter"), b"mine", false)
            .unwrap();
        assert_eq!(std::fs::read(dir.join("inside/chapter")).unwrap(), b"mine");
        assert_eq!(
            std::fs::read(dir.join("outside/secret")).unwrap(),
            b"secret"
        );
    }

    #[test]
    fn a_target_that_is_not_a_regular_file_is_refused_and_left() {
        let (_s, root, dir) = scratch("not-regular");
        mkfifo(&dir.join("inside/pipe"));
        std::fs::create_dir(dir.join("inside/folder")).unwrap();
        for name in ["pipe", "folder"] {
            let e = root.write_atomic(Path::new(name), b"x", false).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{name}: {e}");
            let e = root.read(Path::new(name)).unwrap_err();
            assert!(!is_link_refusal(&e), "{name}: {e}");
        }
        use std::os::unix::fs::FileTypeExt;
        let pipe = std::fs::symlink_metadata(dir.join("inside/pipe")).unwrap();
        assert!(pipe.file_type().is_fifo());
        assert!(names_in(&dir.join("inside/folder")).is_empty());
        assert_eq!(names_in(&dir.join("inside")), ["folder", "pipe", "sub"]);
    }

    #[test]
    fn names_that_leave_the_folder_are_refused() {
        let (_s, root, dir) = scratch("names");
        for rel in [
            "../outside/secret",
            "/etc/passwd",
            "sub/../../outside/secret",
            "",
            ".",
        ] {
            let e = root.write_atomic(Path::new(rel), b"x", true).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{rel:?}: {e}");
            let e = root.read(Path::new(rel)).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{rel:?}: {e}");
        }
        let nul = OsStr::from_bytes(b"a\0b");
        let e = root.write_atomic_if(nul, b"x", || true).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            std::fs::read(dir.join("outside/secret")).unwrap(),
            b"secret"
        );
        assert_eq!(names_in(&dir.join("inside")), ["sub"]);
    }

    #[test]
    fn create_new_refuses_an_existing_link() {
        let (_s, root, dir) = scratch("create-new");
        symlink(dir.join("outside/secret"), dir.join("inside/tmp")).unwrap();
        let e = root.create_new(OsStr::new("tmp")).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        let e = root.create_dir_new(OsStr::new("tmp")).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(dir.join("outside/secret")).unwrap(),
            b"secret"
        );
    }

    #[test]
    fn removes_take_links_as_links() {
        let (_s, root, dir) = scratch("remove");
        symlink(dir.join("outside/secret"), dir.join("inside/sub/file-link")).unwrap();
        symlink(dir.join("outside"), dir.join("inside/sub/dir-link")).unwrap();
        std::fs::create_dir_all(dir.join("inside/sub/deep")).unwrap();
        std::fs::write(dir.join("inside/sub/deep/a"), "a").unwrap();
        root.remove_file(Path::new("sub/file-link")).unwrap();
        root.remove_file(Path::new("sub/absent")).unwrap();
        root.remove_tree(Path::new("absent/deeper")).unwrap();
        assert!(root.remove_file(Path::new("sub/deep")).is_err());
        root.remove_tree(Path::new("sub")).unwrap();
        assert_eq!(names_in(&dir.join("inside")), Vec::<String>::new());
        assert_eq!(names_in(&dir.join("outside")), ["secret"]);
        assert_eq!(
            std::fs::read(dir.join("outside/secret")).unwrap(),
            b"secret"
        );
    }

    #[test]
    fn a_refused_commit_leaves_the_target_and_no_temporary_file() {
        let (_s, root, dir) = scratch("commit");
        std::fs::write(dir.join("inside/out"), "old").unwrap();
        let kept = root
            .write_atomic_if(OsStr::new("out"), b"new", || {
                assert_eq!(names_in(&dir.join("inside")).len(), 3);
                false
            })
            .unwrap();
        assert!(!kept);
        assert_eq!(std::fs::read(dir.join("inside/out")).unwrap(), b"old");
        assert_eq!(names_in(&dir.join("inside")), ["out", "sub"]);
    }

    #[test]
    fn a_write_keeps_permission_bits_but_not_set_id_bits() {
        let (_s, root, dir) = scratch("mode");
        let out = dir.join("inside/out");
        std::fs::write(&out, "old").unwrap();
        std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o4640)).unwrap();
        root.write_atomic(Path::new("out"), b"new", false).unwrap();
        let mode = std::fs::metadata(&out).unwrap().permissions().mode();
        assert_eq!(mode & 0o7777, 0o640);
    }

    #[test]
    fn a_file_outside_any_project_is_replaced_and_its_link_not_followed() {
        let (_s, _root, dir) = scratch("outside-target");
        let target = dir.join("inside/Book.pdf");
        symlink(dir.join("outside/secret"), &target).unwrap();
        write_atomic(&target, b"%PDF-").unwrap();
        assert!(std::fs::symlink_metadata(&target).unwrap().is_file());
        assert_eq!(std::fs::read(&target).unwrap(), b"%PDF-");
        assert_eq!(
            std::fs::read(dir.join("outside/secret")).unwrap(),
            b"secret"
        );
    }

    #[test]
    fn entries_lists_each_name_once_and_twice_in_a_row() {
        let (_s, root, dir) = scratch("entries");
        std::fs::write(dir.join("inside/a"), "a").unwrap();
        let mut first = root.entries().unwrap();
        let mut second = root.entries().unwrap();
        first.sort();
        second.sort();
        assert_eq!(first, [OsString::from("a"), OsString::from("sub")]);
        assert_eq!(first, second);
    }

    #[test]
    fn only_an_owned_regular_file_passes_its_mode_on() {
        let st = |kind, uid| {
            Some(Stat {
                kind,
                mode: 0o640,
                uid,
            })
        };
        assert_eq!(inherited_mode(st(Kind::File, 1000), 1000), Some(0o640));
        assert_eq!(inherited_mode(st(Kind::File, 1001), 1000), None);
        assert_eq!(inherited_mode(st(Kind::Link, 1000), 1000), None);
        assert_eq!(inherited_mode(None, 1000), None);
    }

    #[test]
    fn a_long_name_gets_a_temporary_name_within_name_max() {
        let (_s, root, dir) = scratch("long-name");
        let long = "n".repeat(NAME_MAX);
        let tmp = temp_name(OsStr::new(&long), u64::MAX);
        assert!(tmp.len() <= NAME_MAX, "{}", tmp.len());
        assert!(tmp.to_string_lossy().ends_with(".omaquill-tmp"));
        root.write_atomic(Path::new(&long), b"long", false).unwrap();
        assert_eq!(
            std::fs::read(dir.join("inside").join(&long)).unwrap(),
            b"long"
        );
        assert_eq!(names_in(&dir.join("inside")), [long, "sub".to_string()]);
    }
}

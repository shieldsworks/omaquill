//! Small files omaquill keeps outside the project: settings, recent
//! projects, per-project view state, and today's word count for the Omarchy
//! bar widget.
//!
//! ```text
//! ~/.config/omaquill/settings.conf          key = value
//! ~/.local/state/omaquill/recent            one project path per line
//! ~/.local/state/omaquill/projects/<id>     open item, expanded folders
//! ~/.local/state/omaquill/projects/<id>.ignore
//! ~/.local/state/omaquill/dictionary        words added from the editor
//! ~/.local/state/omaquill/today.json        read by the bar widget
//! ~/.local/share/omaquill/backups/          zipped projects
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(fallback))
        .join("omaquill")
}

pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

pub fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state")
}

pub fn backup_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share").join("backups")
}

/// `key = value` lines, `#` comments.
fn parse_kv(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

fn read_kv(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .map(|text| parse_kv(&text))
        .unwrap_or_default()
}

enum Disk {
    Missing,
    Empty,
    NotUtf8,
    NoKeys,
    Keys(BTreeMap<String, String>),
}

fn read_disk(path: &Path) -> std::io::Result<Disk> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Disk::Missing),
        Err(e) => return Err(e),
    };
    if bytes.is_empty() {
        return Ok(Disk::Empty);
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(Disk::NotUtf8);
    };
    let parsed = parse_kv(text);
    if parsed.is_empty() {
        Ok(Disk::NoKeys)
    } else {
        Ok(Disk::Keys(parsed))
    }
}

/// `rename` replaces an existing name. A link fails instead, so a backup
/// already on disk stays.
fn park_unparsed(path: &Path) -> std::io::Result<()> {
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "settings path includes a file name",
        )
    })?;
    let name = name.to_string_lossy();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for n in 0..10_000u32 {
        let backup = if n == 0 {
            format!("{name}.bad-{stamp}")
        } else {
            format!("{name}.bad-{stamp}-{n}")
        };
        let dest = path.with_file_name(&backup);
        match std::fs::hard_link(path, &dest) {
            Ok(()) => return std::fs::remove_file(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "settings backup name is taken",
    ))
}

fn write_kv(path: &Path, header: &str, kv: &BTreeMap<String, String>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut out = String::from(header);
    for (k, v) in kv {
        out.push_str(&format!("{k} = {v}\n"));
    }
    crate::fsdir::write_atomic(path, out.as_bytes())
}

const SETTINGS_HEADER: &str = "# omaquill settings; the Preferences window writes this file.\n";

#[must_use = "the lock releases when this value is dropped"]
struct SettingsLock {
    _file: std::fs::File,
}

fn lock_unsupported(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::Unsupported
        || err.raw_os_error() == Some(libc::ENOTSUP)
        || err.raw_os_error() == Some(libc::ENOLCK)
}

fn settings_lock(path: &Path) -> std::io::Result<SettingsLock> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "settings path includes a file name",
        )
    })?;
    let lock_path = path.with_file_name(format!(".{}.lock", name.to_string_lossy()));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    // flock drops when the holder exits, including kill -9. This save runs
    // on the window thread. A blocking lock would freeze that window until
    // a stopped or stuck holder released it, so a busy lock is an error.
    match file.try_lock() {
        Ok(()) => Ok(SettingsLock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "settings file is busy",
        )),
        // A filesystem with no locks still gets the save. Two windows can
        // then race; refusing the lock would drop every save.
        Err(std::fs::TryLockError::Error(e)) if lock_unsupported(&e) => {
            Ok(SettingsLock { _file: file })
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

fn overlay_changes(
    disk: &BTreeMap<String, String>,
    loaded: &BTreeMap<String, String>,
    mine: &BTreeMap<String, String>,
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let mut out = disk.clone();
    let mut next = loaded.clone();
    for (key, value) in mine {
        if loaded.get(key) != Some(value) {
            out.insert(key.clone(), value.clone());
            next.insert(key.clone(), value.clone());
        }
    }
    (out, next)
}

#[derive(Debug, Clone, PartialEq)]
enum Origin {
    Fresh,
    Loaded(BTreeMap<String, String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Text size in the editor, 1.0 = the document's own sizes.
    pub zoom: f64,
    /// Show all text in this font instead of the document's fonts.
    pub editor_font: Option<String>,
    /// Words to write per day; 0 = no target.
    pub daily_target: u32,
    /// Backups kept per project.
    pub backups: usize,
    /// The name Compile puts on title pages.
    pub author: String,
    /// Red marks under misspelled words. F7 flips this and it is remembered.
    pub spellcheck: bool,
    /// Composition mode hides marks unless this is on.
    pub spell_in_compose: bool,
    /// Leave the word at the cursor unmarked until the cursor leaves it.
    pub spell_hide_current: bool,
    /// Empty follows the system locale. Otherwise an Enchant tag such as `en_GB`.
    pub spell_language: String,
    origin: Origin,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            zoom: 1.25,
            editor_font: None,
            daily_target: 0,
            backups: 10,
            author: String::new(),
            spellcheck: true,
            spell_in_compose: false,
            spell_hide_current: true,
            spell_language: String::new(),
            origin: Origin::Fresh,
        }
    }
}

fn flag(kv: &BTreeMap<String, String>, key: &str, default: bool) -> bool {
    match kv.get(key).map(String::as_str) {
        Some("true") => true,
        Some("false") => false,
        _ => default,
    }
}

impl Settings {
    pub fn load() -> Settings {
        Self::load_from(&config_dir().join("settings.conf"))
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        self.save_to(&config_dir().join("settings.conf"))
    }

    pub fn load_from(path: &Path) -> Settings {
        Settings::from_kv(&read_kv(path))
    }

    /// Re-reads a loaded file and writes keys that differ from that load.
    /// A missing or empty file, or a value built in memory, writes every key.
    /// Invalid UTF-8, or a non-empty file with no keys, is moved to
    /// `<name>.bad-<timestamp>` and then replaced. A name already there
    /// gets `-<n>` after the timestamp.
    /// A busy lock or a file this process cannot read returns an error and
    /// leaves the file as it was.
    pub fn save_to(&mut self, path: &Path) -> std::io::Result<()> {
        let _lock = settings_lock(path)?;
        let mine = self.to_kv();
        let disk = read_disk(path)?;
        if matches!(disk, Disk::NotUtf8 | Disk::NoKeys) {
            park_unparsed(path)?;
        }
        let (kv, next) = match (&self.origin, &disk) {
            (Origin::Loaded(loaded), Disk::Keys(map)) => overlay_changes(map, loaded, &mine),
            _ => (mine.clone(), mine),
        };
        write_kv(path, SETTINGS_HEADER, &kv)?;
        self.origin = Origin::Loaded(next);
        Ok(())
    }

    fn from_kv(kv: &BTreeMap<String, String>) -> Settings {
        let d = Settings::default();
        let mut parsed = Settings {
            zoom: kv
                .get("zoom")
                .and_then(|v| v.parse().ok())
                .filter(|z: &f64| (0.5..=3.0).contains(z))
                .unwrap_or(d.zoom),
            editor_font: kv.get("editor_font").filter(|v| !v.is_empty()).cloned(),
            daily_target: kv
                .get("daily_target")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            backups: kv
                .get("backups")
                .and_then(|v| v.parse().ok())
                .unwrap_or(d.backups),
            author: kv.get("author").cloned().unwrap_or_default(),
            spellcheck: flag(kv, "spellcheck", d.spellcheck),
            spell_in_compose: flag(kv, "spell_in_compose", d.spell_in_compose),
            spell_hide_current: flag(kv, "spell_hide_current", d.spell_hide_current),
            spell_language: kv.get("spell_language").cloned().unwrap_or_default(),
            origin: Origin::Fresh,
        };
        parsed.origin = Origin::Loaded(parsed.to_kv());
        parsed
    }

    fn to_kv(&self) -> BTreeMap<String, String> {
        let mut kv = BTreeMap::new();
        kv.insert("zoom".into(), format!("{:.2}", self.zoom));
        kv.insert(
            "editor_font".into(),
            self.editor_font.clone().unwrap_or_default(),
        );
        kv.insert("daily_target".into(), self.daily_target.to_string());
        kv.insert("backups".into(), self.backups.to_string());
        kv.insert("author".into(), self.author.clone());
        kv.insert("spellcheck".into(), bool_word(self.spellcheck));
        kv.insert("spell_in_compose".into(), bool_word(self.spell_in_compose));
        kv.insert(
            "spell_hide_current".into(),
            bool_word(self.spell_hide_current),
        );
        kv.insert("spell_language".into(), self.spell_language.clone());
        kv
    }
}

fn bool_word(on: bool) -> String {
    if on {
        "true".to_string()
    } else {
        "false".to_string()
    }
}

pub fn recent() -> Vec<PathBuf> {
    std::fs::read_to_string(state_dir().join("recent"))
        .unwrap_or_default()
        .lines()
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .collect()
}

pub fn add_recent(path: &Path) {
    let mut list = recent();
    list.retain(|p| p != path);
    list.insert(0, path.to_path_buf());
    list.truncate(10);
    let text: String = list.iter().map(|p| format!("{}\n", p.display())).collect();
    let dir = state_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = crate::fsdir::write_atomic(&dir.join("recent"), text.as_bytes());
}

/// Per-project view state, kept out of the project so Scrivener's own
/// settings files stay untouched.
/// Ids are read back in their [`crate::project::canonical_id`] form, the
/// form the binder holds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectView {
    pub open: Option<String>,
    pub expanded: Vec<String>,
}

fn project_id(project: &Path) -> String {
    let id = crate::sha1::hex(project.to_string_lossy().as_bytes());
    id[..16].to_string()
}

fn project_file(project: &Path) -> PathBuf {
    state_dir().join("projects").join(project_id(project))
}

/// Words the author added from the editor. Outside every project.
pub fn dictionary_path() -> PathBuf {
    state_dir().join("dictionary")
}

/// Per-project words to skip. The file sits next to view state, not in the `.scriv`.
pub fn ignore_path(project: &Path) -> PathBuf {
    state_dir()
        .join("projects")
        .join(format!("{}.ignore", project_id(project)))
}

pub fn load_words(path: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_lowercase())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

pub fn save_words(path: &Path, words: &BTreeSet<String>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut out = String::new();
    for w in words {
        out.push_str(w);
        out.push('\n');
    }
    crate::fsdir::write_atomic(path, out.as_bytes())
}

impl ProjectView {
    pub fn load(project: &Path) -> ProjectView {
        ProjectView::load_from(&project_file(project))
    }

    /// [`ProjectView::load`] from the view-state file at `path`.
    fn load_from(path: &Path) -> ProjectView {
        let kv = read_kv(path);
        ProjectView {
            open: kv
                .get("open")
                .filter(|v| !v.is_empty())
                .map(|v| crate::project::canonical_id(v)),
            expanded: kv
                .get("expanded")
                .map(|v| {
                    v.split(',')
                        .filter(|s| !s.is_empty())
                        .map(crate::project::canonical_id)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    pub fn save(&self, project: &Path) {
        let mut kv = BTreeMap::new();
        kv.insert("path".into(), project.display().to_string());
        kv.insert("open".into(), self.open.clone().unwrap_or_default());
        kv.insert("expanded".into(), self.expanded.join(","));
        let _ = write_kv(&project_file(project), "", &kv);
    }
}

/// Today's writing, for the bar widget: `today.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct Today {
    pub date: String,
    pub project: String,
    pub path: String,
    /// Manuscript words when the day's first session began.
    pub baseline: i64,
    pub manuscript: i64,
    pub target: u32,
    /// omaquill has this project open right now.
    pub open: bool,
}

impl Today {
    pub fn words(&self) -> i64 {
        self.manuscript - self.baseline
    }

    fn path() -> PathBuf {
        state_dir().join("today.json")
    }

    pub fn load() -> Option<Today> {
        let text = std::fs::read_to_string(Self::path()).ok()?;
        let s = |k: &str| json_field(&text, k);
        Some(Today {
            date: s("date")?,
            project: s("project").unwrap_or_default(),
            path: s("path").unwrap_or_default(),
            baseline: s("baseline")?.parse().ok()?,
            manuscript: s("manuscript")?.parse().ok()?,
            target: s("target").and_then(|v| v.parse().ok()).unwrap_or(0),
            open: s("open").as_deref() == Some("true"),
        })
    }

    /// Starts (or continues) today's count for a project with `manuscript`
    /// words in it now.
    pub fn begin(project: &str, path: &Path, manuscript: i64, target: u32) -> Today {
        let date = today();
        let path_s = path.display().to_string();
        let baseline = match Today::load() {
            Some(t) if t.date == date && t.path == path_s => t.baseline,
            _ => manuscript,
        };
        Today {
            date,
            project: project.to_string(),
            path: path_s,
            baseline,
            manuscript,
            target,
            open: true,
        }
    }

    pub fn save(&self) {
        let json = format!(
            "{{\n  \"date\": \"{}\",\n  \"project\": \"{}\",\n  \"path\": \"{}\",\n  \"words\": {},\n  \"baseline\": {},\n  \"manuscript\": {},\n  \"target\": {},\n  \"open\": {}\n}}\n",
            self.date,
            json_escape(&self.project),
            json_escape(&self.path),
            self.words(),
            self.baseline,
            self.manuscript,
            self.target,
            self.open
        );
        let dir = state_dir();
        let _ = std::fs::create_dir_all(&dir);
        let _ = crate::fsdir::write_atomic(&Self::path(), json.as_bytes());
    }
}

/// Local date as YYYY-MM-DD.
pub fn today() -> String {
    crate::project::timestamp()[..10].to_string()
}

fn json_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// A top-level scalar from the flat JSON this module writes.
fn json_field(text: &str, key: &str) -> Option<String> {
    let at = text.find(&format!("\"{key}\""))?;
    let rest = text[at + key.len() + 2..]
        .trim_start()
        .strip_prefix(':')?
        .trim_start();
    if let Some(body) = rest.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = body.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(out),
                '\\' => match chars.next()? {
                    'n' => out.push('\n'),
                    'u' => {
                        let hex: String = chars.by_ref().take(4).collect();
                        out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                    }
                    c => out.push(c),
                },
                c => out.push(c),
            }
        }
        None
    } else {
        let end = rest.find([',', '\n', '}']).unwrap_or(rest.len());
        Some(rest[..end].trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip() {
        let t = Today {
            date: "2026-09-28".into(),
            project: "Lighthouse \"Book\" 1".into(),
            path: "/home/x/N.scriv".into(),
            baseline: 100,
            manuscript: 450,
            target: 500,
            open: true,
        };
        let json = format!(
            "{{\n  \"date\": \"{}\",\n  \"project\": \"{}\",\n  \"baseline\": {},\n  \"manuscript\": {},\n  \"open\": true\n}}",
            t.date,
            json_escape(&t.project),
            t.baseline,
            t.manuscript
        );
        assert_eq!(
            json_field(&json, "project").unwrap(),
            "Lighthouse \"Book\" 1"
        );
        assert_eq!(json_field(&json, "manuscript").unwrap(), "450");
        assert_eq!(json_field(&json, "open").unwrap(), "true");
        assert_eq!(t.words(), 350);
    }

    #[test]
    fn spellcheck_choices_survive_a_reload() {
        let path = std::env::temp_dir().join(format!(
            "omaquill-settings-{}-{}.conf",
            std::process::id(),
            "spell"
        ));
        let fresh = Settings::load_from(&path);
        assert!(fresh.spellcheck);
        assert!(!fresh.spell_in_compose);
        assert!(fresh.spell_hide_current);
        assert!(fresh.spell_language.is_empty());
        let mut saved = Settings {
            spellcheck: false,
            spell_in_compose: true,
            spell_hide_current: false,
            spell_language: "en_GB".into(),
            ..Settings::default()
        };
        saved.save_to(&path).unwrap();
        let back = Settings::load_from(&path);
        assert!(!back.spellcheck);
        assert!(back.spell_in_compose);
        assert!(!back.spell_hide_current);
        assert_eq!(back.spell_language, "en_GB");
        assert_eq!(back.zoom, Settings::default().zoom);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn saving_spelling_in_one_copy_keeps_zoom_from_the_other() {
        let path = std::env::temp_dir().join(format!(
            "omaquill-settings-{}-two-windows.conf",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut zoomed = Settings::load_from(&path);
        let mut spelling = Settings::load_from(&path);
        zoomed.zoom = 2.0;
        zoomed.save_to(&path).unwrap();
        spelling.spellcheck = false;
        spelling.save_to(&path).unwrap();
        let disk = Settings::load_from(&path);
        assert_eq!(disk.zoom, 2.0);
        assert!(!disk.spellcheck);
        spelling.spellcheck = true;
        spelling.save_to(&path).unwrap();
        let disk = Settings::load_from(&path);
        assert_eq!(disk.zoom, 2.0);
        assert!(disk.spellcheck);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn saving_spelling_keeps_zoom_written_with_one_decimal() {
        let path = std::env::temp_dir().join(format!(
            "omaquill-settings-{}-zoom-decimal.conf",
            std::process::id()
        ));
        std::fs::write(&path, "zoom = 1.5\n").unwrap();
        let mut spelling = Settings::load_from(&path);
        let mut zoomed = Settings::load_from(&path);
        zoomed.zoom = 2.0;
        zoomed.save_to(&path).unwrap();
        spelling.spellcheck = false;
        spelling.save_to(&path).unwrap();
        let disk = Settings::load_from(&path);
        assert_eq!(disk.zoom, 2.0);
        assert!(!disk.spellcheck);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_ignore_list_is_words_outside_the_project() {
        let dir = std::env::temp_dir().join(format!("omaquill-words-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("project.ignore");
        std::fs::write(&path, "Mara\n# note\n\nXYZ\n").unwrap();
        let loaded = load_words(&path);
        assert!(loaded.contains("mara"));
        assert!(loaded.contains("xyz"));
        assert!(!loaded.contains("note"));
        let mut words = loaded;
        words.insert("helo".into());
        save_words(&path, &words).unwrap();
        let again = load_words(&path);
        assert!(again.contains("helo") && again.contains("mara"));
        let project = dir.join("Book.scriv");
        std::fs::create_dir_all(&project).unwrap();
        let ignore = ignore_path(&project);
        let dictionary = dictionary_path();
        assert!(!ignore.starts_with(&project));
        assert!(!dictionary.starts_with(&project));
        assert_eq!(ignore.extension().and_then(|e| e.to_str()), Some("ignore"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("omaquill-settings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    struct HeldLock {
        child: std::process::Child,
    }

    impl Drop for HeldLock {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    impl HeldLock {
        fn release(mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn hold_settings_lock(dir: &Path) -> HeldLock {
        let lock_path = dir.join(".settings.conf.lock");
        let ready = dir.join("lock-ready");
        // One process holds the lock. `flock -c` runs the sleep in a child
        // that inherits the fd, so killing the parent would leave the lock.
        let child = std::process::Command::new("python3")
            .arg("-c")
            .arg(
                "import fcntl, os, sys, time\n\
                 fd = os.open(sys.argv[1], os.O_RDWR | os.O_CREAT, 0o644)\n\
                 fcntl.flock(fd, fcntl.LOCK_EX)\n\
                 os.close(os.open(sys.argv[2], os.O_CREAT | os.O_WRONLY, 0o644))\n\
                 time.sleep(120)\n",
            )
            .arg(&lock_path)
            .arg(&ready)
            .spawn()
            .unwrap();
        let start = std::time::Instant::now();
        while !ready.exists() {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(2),
                "lock holder did not start"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        HeldLock { child }
    }

    #[test]
    fn the_first_save_with_no_file_keeps_the_other_defaults() {
        let dir = temp_dir("first");
        let path = dir.join("settings.conf");
        assert!(!path.exists());
        let mut settings = Settings::load_from(&path);
        settings.zoom = 2.0;
        settings.author = "Casey".into();
        settings.save_to(&path).unwrap();
        let back = Settings::load_from(&path);
        assert_eq!(back.zoom, 2.0);
        assert_eq!(back.author, "Casey");
        assert!(back.spellcheck);
        assert_eq!(back.backups, Settings::default().backups);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_config_dir_is_created_on_save() {
        let dir = temp_dir("missing-dir");
        let path = dir.join("nested").join("settings.conf");
        assert!(!path.parent().unwrap().exists());
        let started = std::time::Instant::now();
        let mut settings = Settings::load_from(&path);
        settings.zoom = 2.0;
        settings.save_to(&path).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(Settings::load_from(&path).zoom, 2.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_read_only_config_dir_returns_and_leaves_the_file() {
        let dir = temp_dir("readonly-dir");
        let path = dir.join("settings.conf");
        let original = "zoom = 2.00\nauthor = Casey\n";
        std::fs::write(&path, original).unwrap();
        let mut settings = Settings::load_from(&path);
        settings.spellcheck = false;
        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        let mode = std::os::unix::fs::PermissionsExt::mode(&perms);
        perms.set_readonly(true);
        std::fs::set_permissions(&dir, perms).unwrap();
        let started = std::time::Instant::now();
        let result = settings.save_to(&path);
        let elapsed = started.elapsed();
        let mut restore = std::fs::metadata(&dir).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut restore, mode);
        std::fs::set_permissions(&dir, restore).unwrap();
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "{elapsed:?}"
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_save_that_cannot_read_the_file_leaves_the_bytes() {
        let dir = temp_dir("unreadable");
        let path = dir.join("settings.conf");
        let original = "zoom = 2.00\nauthor = Casey\nfuture_layout = columns\n";
        std::fs::write(&path, original).unwrap();
        let mut settings = Settings::load_from(&path);
        settings.spellcheck = false;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0);
        std::fs::set_permissions(&path, perms).unwrap();
        let started = std::time::Instant::now();
        let result = settings.save_to(&path);
        let elapsed = started.elapsed();
        let mut restore = std::fs::metadata(&path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut restore, 0o644);
        std::fs::set_permissions(&path, restore).unwrap();
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "{elapsed:?}"
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_file_is_replaced_by_the_in_memory_settings() {
        let dir = temp_dir("corrupt");
        let path = dir.join("settings.conf");
        std::fs::write(&path, "zoom = 2.00\nauthor = Casey\n").unwrap();
        let mut settings = Settings::load_from(&path);
        std::fs::write(&path, b"\xff\xfe not text").unwrap();
        settings.spellcheck = false;
        settings.save_to(&path).unwrap();
        let back = Settings::load_from(&path);
        assert_eq!(back.zoom, 2.0);
        assert_eq!(back.author, "Casey");
        assert!(!back.spellcheck);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with('\n'), "{text:?}");
        assert!(text.contains("zoom = 2.00"), "{text}");
        assert!(text.contains("author = Casey"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn garbage_text_does_not_drop_settings_loaded_before_it() {
        let dir = temp_dir("garbage");
        let path = dir.join("settings.conf");
        std::fs::write(&path, "zoom = 2.00\nauthor = Casey\n").unwrap();
        let mut settings = Settings::load_from(&path);
        std::fs::write(&path, "this is not settings\n").unwrap();
        settings.spellcheck = false;
        settings.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("author = Casey"), "{text}");
        assert!(text.contains("zoom = 2.00"), "{text}");
        assert!(text.contains("spellcheck = false"), "{text}");
        assert!(!text.contains("this is not settings"), "{text}");
        assert!(text.ends_with('\n'));
        let back = Settings::load_from(&path);
        assert_eq!(back.zoom, 2.0);
        assert_eq!(back.author, "Casey");
        assert!(!back.spellcheck);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_utf8_keeps_future_layout_in_a_backup() {
        let dir = temp_dir("bad-utf8");
        let path = dir.join("settings.conf");
        let mut original = b"future_layout = columns\n".to_vec();
        original.push(0xff);
        std::fs::write(&path, &original).unwrap();
        let mut settings = Settings::load_from(&path);
        settings.spellcheck = false;
        settings.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("spellcheck = false"), "{text}");
        let backup = backup_with(dir.as_path(), b"future_layout = columns");
        assert_eq!(std::fs::read(&backup).unwrap(), original);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_keyless_file_keeps_future_layout_in_a_backup() {
        let dir = temp_dir("keyless");
        let path = dir.join("settings.conf");
        let original = b"future_layout columns\nstill not a key\n";
        std::fs::write(&path, original).unwrap();
        let mut settings = Settings::load_from(&path);
        settings.spellcheck = false;
        settings.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("spellcheck = false"), "{text}");
        assert!(!text.contains("future_layout"), "{text}");
        let backup = backup_with(dir.as_path(), b"future_layout");
        assert_eq!(std::fs::read(&backup).unwrap(), original);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_file_is_replaced_with_no_backup() {
        let dir = temp_dir("empty-file");
        let path = dir.join("settings.conf");
        std::fs::write(&path, b"").unwrap();
        let mut settings = Settings::load_from(&path);
        settings.zoom = 2.0;
        settings.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path).zoom, 2.0);
        let names = dir_names(&dir);
        assert!(names.iter().all(|n| !n.contains(".bad-")), "{names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_unparsed_save_does_not_replace_the_first_backup() {
        let dir = temp_dir("two-bad");
        let path = dir.join("settings.conf");
        let first = b"future_layout columns\n";
        std::fs::write(&path, first).unwrap();
        let mut settings = Settings::load_from(&path);
        settings.spellcheck = false;
        settings.save_to(&path).unwrap();
        let second = b"\xfffuture_layout still here";
        std::fs::write(&path, second).unwrap();
        settings.author = "Casey".into();
        settings.save_to(&path).unwrap();
        let names = dir_names(&dir);
        let backups: Vec<_> = names.iter().filter(|n| n.contains(".bad-")).collect();
        assert_eq!(backups.len(), 2, "{names:?}");
        let bodies: Vec<Vec<u8>> = backups
            .iter()
            .map(|n| std::fs::read(dir.join(n)).unwrap())
            .collect();
        assert!(bodies.contains(&first.to_vec()), "{names:?}");
        assert!(bodies.contains(&second.to_vec()), "{names:?}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("author = Casey"), "{text}");
        assert!(text.contains("spellcheck = false"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn dir_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn backup_with(dir: &Path, needle: &[u8]) -> PathBuf {
        let found: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("settings.conf.bad-"))
                    && std::fs::read(p)
                        .unwrap()
                        .windows(needle.len())
                        .any(|w| w == needle)
            })
            .collect();
        assert_eq!(found.len(), 1, "{found:?}");
        found[0].clone()
    }

    #[test]
    fn an_unknown_key_survives_a_spelling_save() {
        let dir = temp_dir("unknown");
        let path = dir.join("settings.conf");
        std::fs::write(
            &path,
            "future_layout = columns\nzoom = 1.50\nauthor = Casey\n",
        )
        .unwrap();
        let mut settings = Settings::load_from(&path);
        settings.spellcheck = false;
        settings.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("future_layout = columns"), "{text}");
        assert!(text.contains("author = Casey"), "{text}");
        let back = Settings::load_from(&path);
        assert_eq!(back.zoom, 1.5);
        assert_eq!(back.author, "Casey");
        assert!(!back.spellcheck);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_killed_lock_holder_does_not_block_the_next_save() {
        let dir = temp_dir("killed");
        let path = dir.join("settings.conf");
        std::fs::write(&path, "zoom = 1.25\nauthor = Casey\n").unwrap();
        let held = hold_settings_lock(&dir);
        held.release();
        let mut settings = Settings::load_from(&path);
        settings.spellcheck = false;
        let started = std::time::Instant::now();
        settings.save_to(&path).unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "save blocked for {:?}",
            started.elapsed()
        );
        let back = Settings::load_from(&path);
        assert!(!back.spellcheck);
        assert_eq!(back.zoom, 1.25);
        assert_eq!(back.author, "Casey");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_live_lock_holder_does_not_block_and_the_file_stays() {
        let dir = temp_dir("live");
        let path = dir.join("settings.conf");
        let original = "zoom = 2.00\nauthor = Casey\nfuture_layout = columns\n";
        std::fs::write(&path, original).unwrap();
        let held = hold_settings_lock(&dir);
        let mut settings = Settings::load_from(&path);
        settings.spellcheck = false;
        let started = std::time::Instant::now();
        let result = settings.save_to(&path);
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "{elapsed:?}"
        );
        let err = result.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        drop(held);
        settings.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("future_layout = columns"), "{text}");
        assert!(text.contains("spellcheck = false"), "{text}");
        let back = Settings::load_from(&path);
        assert_eq!(back.zoom, 2.0);
        assert_eq!(back.author, "Casey");
        assert!(!back.spellcheck);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_saves_at_once_keep_both_changes_and_a_whole_file() {
        let dir = temp_dir("both");
        let path = dir.join("settings.conf");
        std::fs::write(
            &path,
            "zoom = 1.25\nauthor = Casey\nfuture_layout = columns\n",
        )
        .unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let torn = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let path = path.clone();
            let stop = stop.clone();
            let torn = torn.clone();
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    match std::fs::read(&path) {
                        Ok(bytes) => {
                            if std::str::from_utf8(&bytes).is_err() || !bytes.ends_with(b"\n") {
                                torn.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(_) => torn.store(true, std::sync::atomic::Ordering::Relaxed),
                    }
                }
            })
        };
        for _ in 0..20 {
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let zoom = {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut settings = Settings::load_from(&path);
                    settings.zoom = 2.0;
                    barrier.wait();
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                    loop {
                        match settings.save_to(&path) {
                            Ok(()) => return,
                            Err(e)
                                if e.kind() == std::io::ErrorKind::WouldBlock
                                    && std::time::Instant::now() < deadline =>
                            {
                                std::thread::sleep(std::time::Duration::from_millis(2));
                            }
                            Err(e) => panic!("{e}"),
                        }
                    }
                })
            };
            let spell = {
                let path = path.clone();
                std::thread::spawn(move || {
                    let mut settings = Settings::load_from(&path);
                    settings.spellcheck = false;
                    barrier.wait();
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                    loop {
                        match settings.save_to(&path) {
                            Ok(()) => return,
                            Err(e)
                                if e.kind() == std::io::ErrorKind::WouldBlock
                                    && std::time::Instant::now() < deadline =>
                            {
                                std::thread::sleep(std::time::Duration::from_millis(2));
                            }
                            Err(e) => panic!("{e}"),
                        }
                    }
                })
            };
            zoom.join().unwrap();
            spell.join().unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(text.contains("future_layout = columns"), "{text}");
            assert!(text.contains("author = Casey"), "{text}");
            assert!(text.ends_with('\n'), "{text:?}");
            let back = Settings::load_from(&path);
            assert_eq!(back.zoom, 2.0);
            assert!(!back.spellcheck);
            assert_eq!(back.author, "Casey");
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        reader.join().unwrap();
        assert!(!torn.load(std::sync::atomic::Ordering::Relaxed));
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("omaquill-tmp"))
            .collect();
        assert!(names.is_empty(), "{names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_saves_of_zoom_leave_one_value_and_the_unknown_key() {
        let dir = temp_dir("same-key");
        let path = dir.join("settings.conf");
        std::fs::write(&path, "zoom = 1.25\nfuture_layout = columns\n").unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let one = {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut settings = Settings::load_from(&path);
                settings.zoom = 2.0;
                barrier.wait();
                save_loaded(&path, settings);
            })
        };
        let two = {
            let path = path.clone();
            std::thread::spawn(move || {
                let mut settings = Settings::load_from(&path);
                settings.zoom = 1.75;
                barrier.wait();
                save_loaded(&path, settings);
            })
        };
        one.join().unwrap();
        two.join().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("future_layout = columns"), "{text}");
        assert!(text.ends_with('\n'));
        let zoom = Settings::load_from(&path).zoom;
        assert!(zoom == 2.0 || zoom == 1.75, "{zoom}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn save_loaded(path: &Path, mut settings: Settings) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match settings.save_to(path) {
                Ok(()) => return,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(e) => panic!("{e}"),
            }
        }
    }

    #[test]
    fn view_state_ids_read_back_canonical() {
        let dir = temp_dir("view-case");
        let path = dir.join("view");
        std::fs::write(
            &path,
            "open = abcdefab-cdef-4abc-8def-abcdefabcdef\nexpanded = aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee,Draft\n",
        )
        .unwrap();
        let view = ProjectView::load_from(&path);
        assert_eq!(
            view.open.as_deref(),
            Some("ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDEF")
        );
        assert_eq!(
            view.expanded,
            vec![
                "AAAAAAAA-BBBB-4CCC-8DDD-EEEEEEEEEEEE".to_string(),
                "Draft".to_string()
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

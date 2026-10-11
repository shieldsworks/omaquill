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
fn read_kv(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

fn write_kv(path: &Path, header: &str, kv: &BTreeMap<String, String>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut out = String::from(header);
    for (k, v) in kv {
        out.push_str(&format!("{k} = {v}\n"));
    }
    crate::project::write_atomic(path, out.as_bytes())
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

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&config_dir().join("settings.conf"))
    }

    pub fn load_from(path: &Path) -> Settings {
        Settings::from_kv(&read_kv(path))
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        write_kv(
            path,
            "# omaquill settings; the Preferences window writes this file.\n",
            &self.to_kv(),
        )
    }

    fn from_kv(kv: &BTreeMap<String, String>) -> Settings {
        let d = Settings::default();
        Settings {
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
        }
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
    let _ = crate::project::write_atomic(&dir.join("recent"), text.as_bytes());
}

/// Per-project view state, kept out of the project so Scrivener's own
/// settings files stay untouched.
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
    crate::project::write_atomic(path, out.as_bytes())
}

impl ProjectView {
    pub fn load(project: &Path) -> ProjectView {
        let kv = read_kv(&project_file(project));
        ProjectView {
            open: kv.get("open").filter(|v| !v.is_empty()).cloned(),
            expanded: kv
                .get("expanded")
                .map(|v| {
                    v.split(',')
                        .filter(|s| !s.is_empty())
                        .map(String::from)
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
        let _ = crate::project::write_atomic(&Self::path(), json.as_bytes());
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
        let saved = Settings {
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
}

//! Small files omaquill keeps outside the project: settings, recent
//! projects, per-project view state, and today's word count for the Omarchy
//! bar widget.
//!
//! ```text
//! ~/.config/omaquill/settings.conf          key = value
//! ~/.local/state/omaquill/recent            one project path per line
//! ~/.local/state/omaquill/projects/<id>     open item, expanded folders
//! ~/.local/state/omaquill/today.json        read by the bar widget
//! ~/.local/share/omaquill/backups/          zipped projects
//! ```

use std::collections::BTreeMap;
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
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            zoom: 1.25,
            editor_font: None,
            daily_target: 0,
            backups: 10,
            author: String::new(),
        }
    }
}

impl Settings {
    pub fn load() -> Settings {
        let kv = read_kv(&config_dir().join("settings.conf"));
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
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let mut kv = BTreeMap::new();
        kv.insert("zoom".into(), format!("{:.2}", self.zoom));
        kv.insert(
            "editor_font".into(),
            self.editor_font.clone().unwrap_or_default(),
        );
        kv.insert("daily_target".into(), self.daily_target.to_string());
        kv.insert("backups".into(), self.backups.to_string());
        kv.insert("author".into(), self.author.clone());
        write_kv(
            &config_dir().join("settings.conf"),
            "# omaquill settings; the Preferences window writes this file.\n",
            &kv,
        )
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

fn project_file(project: &Path) -> PathBuf {
    let id = crate::sha1::hex(project.to_string_lossy().as_bytes());
    state_dir().join("projects").join(&id[..16])
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
}

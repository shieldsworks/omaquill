//! Omarchy's theme, applied to libadwaita's named colors.
//!
//! Omarchy keeps the current theme in
//! `~/.local/state/omarchy/current/theme/colors.toml`. omaquill reads it on
//! start and again whenever it changes, so `omarchy-theme-set` restyles the
//! window at once. Without Omarchy, plain libadwaita colors apply.

use gtk::gio;
use gtk::prelude::*;
use std::collections::HashMap;
use std::path::PathBuf;

pub fn theme_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("OMAQUILL_THEME_DIR") {
        return PathBuf::from(dir);
    }
    // XDG_STATE_HOME first; Omarchy's own scripts use ~/.local/state.
    let candidates: Vec<PathBuf> = [
        std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")),
    ]
    .into_iter()
    .flatten()
    .map(|s| s.join("omarchy/current/theme"))
    .collect();
    candidates
        .iter()
        .find(|d| d.join("colors.toml").exists())
        .or(candidates.last())
        .cloned()
        .unwrap_or_default()
}

/// Reads `key = "value"` lines; enough for colors.toml, not general TOML.
pub fn read_colors(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            let v = v.trim();
            // Quoted values may hold '#' (colors do); bare ones end at a comment.
            let v = match v.chars().next() {
                Some(q @ ('"' | '\'')) => v[1..].split(q).next().unwrap_or(""),
                _ => v.split('#').next().unwrap_or("").trim(),
            };
            Some((k.trim().to_string(), v.to_string()))
        })
        .collect()
}

/// libadwaita CSS for an Omarchy palette.
pub fn css(colors: &HashMap<String, String>) -> String {
    let get = |k: &str| colors.get(k).cloned();
    // libadwaita 1.6+ reads CSS variables (`--window-bg-color`); older
    // versions read `@define-color window_bg_color`. Write both.
    let mut out = String::new();
    let mut vars = String::new();
    let mut def = |name: &str, value: Option<String>| {
        if let Some(v) = value {
            out.push_str(&format!("@define-color {name} {v};\n"));
            vars.push_str(&format!("  --{}: {v};\n", name.replace('_', "-")));
        }
    };
    let bg = get("background");
    let fg = get("foreground");
    let accent = get("accent").or_else(|| get("blue"));
    let lighter = get("lighter_background").or_else(|| bg.clone());
    let darker = get("dark_background").or_else(|| bg.clone());
    def("window_bg_color", bg.clone());
    def("window_fg_color", fg.clone());
    def("view_bg_color", bg.clone());
    def(
        "view_fg_color",
        get("bright_foreground").or_else(|| fg.clone()),
    );
    def("headerbar_bg_color", darker.clone());
    def("headerbar_fg_color", fg.clone());
    def("headerbar_backdrop_color", darker.clone());
    def("sidebar_bg_color", darker.clone());
    def("sidebar_fg_color", fg.clone());
    def("sidebar_backdrop_color", darker.clone());
    def("secondary_sidebar_bg_color", darker.clone());
    def("secondary_sidebar_fg_color", fg.clone());
    def("card_bg_color", lighter.clone());
    def("card_fg_color", fg.clone());
    def("popover_bg_color", lighter.clone());
    def("popover_fg_color", fg.clone());
    def("dialog_bg_color", lighter.clone());
    def("dialog_fg_color", fg.clone());
    def("accent_bg_color", accent.clone());
    def("accent_color", accent.clone());
    def("accent_fg_color", bg.clone());
    def("destructive_bg_color", get("red"));
    def("destructive_color", get("red"));
    def("success_color", get("green"));
    def("warning_color", get("yellow"));
    def("error_color", get("red"));
    out.push_str(&format!(":root {{\n{vars}}}\n"));
    if let Some(sel) = get("selection") {
        out.push_str(&format!(
            "textview.omaquill-text text selection {{ background-color: {sel}; }}\n"
        ));
    }
    if let Some(muted) = get("dark_foreground").or_else(|| get("muted")) {
        out.push_str(&format!(".omaquill-dim {{ color: {muted}; }}\n"));
    }
    out
}

pub struct Theme {
    _provider: gtk::CssProvider,
    _monitor: Option<gio::FileMonitor>,
}

impl Theme {
    pub fn install() -> Theme {
        let provider = gtk::CssProvider::new();
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        let dir = theme_dir();
        let reload = {
            let provider = provider.clone();
            let dir = dir.clone();
            move || {
                let text = std::fs::read_to_string(dir.join("colors.toml")).unwrap_or_default();
                let colors = read_colors(&text);
                let style = adw::StyleManager::default();
                style.set_color_scheme(match colors.get("mode").map(String::as_str) {
                    Some("light") => adw::ColorScheme::ForceLight,
                    Some("dark") => adw::ColorScheme::ForceDark,
                    _ => adw::ColorScheme::Default,
                });
                provider.load_from_string(&css(&colors));
            }
        };
        reload();
        // `current/theme` is a directory Omarchy swaps; watch the parent too.
        let monitor = gio::File::for_path(dir.parent().unwrap_or(&dir))
            .monitor(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            .ok();
        if let Some(m) = &monitor {
            m.connect_changed(move |_, _, _, _| reload());
        }
        Theme {
            _provider: provider,
            _monitor: monitor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_omarchy_colors() {
        let c = read_colors(
            "mode = \"dark\"\n\naccent = \"#7aa2f7\" # blue\n# comment\nbackground = \"#1a1b26\"\nsize = 12 # pt\n",
        );
        assert_eq!(c.get("size").map(String::as_str), Some("12"));
        assert_eq!(c.get("accent").map(String::as_str), Some("#7aa2f7"));
        assert_eq!(c.get("mode").map(String::as_str), Some("dark"));
        let css = css(&c);
        assert!(css.contains("@define-color accent_bg_color #7aa2f7;"));
        assert!(css.contains("  --accent-bg-color: #7aa2f7;"));
    }
}

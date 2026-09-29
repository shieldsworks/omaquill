//! Dialogs: rename, Compile, preferences, keyboard shortcuts, about.

use super::{Win, rich};
use adw::prelude::*;
use gtk::{gio, glib};
use omaquill::compile::{self, Format, Headings, Options};
use std::rc::Rc;

pub fn rename(win: &Rc<Win>, uuid: &str) {
    let Some(item) = win.item(uuid) else { return };
    if item.kind.is_root_folder() && item.kind == omaquill::project::Kind::Trash {
        return;
    }
    let entry = gtk::Entry::builder()
        .text(&item.title)
        .activates_default(true)
        .build();
    let dialog = adw::AlertDialog::new(Some("Rename"), None);
    dialog.set_extra_child(Some(&entry));
    dialog.add_responses(&[("cancel", "Cancel"), ("rename", "Rename")]);
    dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("rename"));
    dialog.set_close_response("cancel");
    let weak = Rc::downgrade(win);
    let uuid = uuid.to_string();
    let e = entry.clone();
    dialog.connect_response(None, move |_, r| {
        if r == "rename"
            && let Some(win) = weak.upgrade()
        {
            let title = e.text().trim().to_string();
            if !title.is_empty() {
                win.set_title(&uuid, &title);
            }
        }
    });
    dialog.present(Some(&win.window));
    entry.grab_focus();
    entry.select_region(0, -1);
}

pub fn shortcuts() -> adw::Dialog {
    let sections: &[(&str, &[(&str, &str)])] = &[
        (
            "Project",
            &[
                ("<Ctrl>o", "Open project"),
                ("<Ctrl><Shift>o", "New project"),
                ("<Ctrl>s", "Save now (omaquill also saves as you type)"),
                ("<Ctrl><Shift>e", "Compile"),
                ("<Ctrl><Shift>f", "Search the project"),
                ("<Ctrl>comma", "Preferences"),
                ("<Ctrl>q", "Quit"),
            ],
        ),
        (
            "Binder",
            &[
                ("<Ctrl>n", "New text"),
                ("<Ctrl><Shift>n", "New folder"),
                ("F2", "Rename"),
                ("Delete", "Move to the Trash (binder focused)"),
                ("j k", "Next / previous item (binder focused)"),
                ("h l", "Fold / unfold (binder focused)"),
                ("<Ctrl><Alt>k <Ctrl><Alt>j", "Move up / down"),
                ("<Ctrl><Alt>l <Ctrl><Alt>h", "Indent / outdent"),
                ("<Ctrl><Alt>1", "Focus the binder"),
                ("<Ctrl><Alt>2", "Focus the editor"),
            ],
        ),
        (
            "Writing",
            &[
                ("<Ctrl>b", "Bold"),
                ("<Ctrl>i", "Italic"),
                ("<Ctrl>u", "Underline"),
                ("<Ctrl>f", "Find in document"),
                ("<Ctrl>z", "Undo"),
                ("<Ctrl><Shift>z", "Redo"),
                ("F11", "Composition mode"),
                ("<Ctrl>plus <Ctrl>minus", "Zoom in / out"),
                ("<Ctrl>0", "Reset zoom"),
            ],
        ),
        (
            "View",
            &[
                ("<Ctrl>1", "Text"),
                ("<Ctrl>2", "Corkboard"),
                ("<Ctrl>3", "Outliner"),
                ("<Ctrl><Alt>b", "Show or hide the binder"),
                ("<Ctrl><Alt>i", "Show or hide the inspector"),
            ],
        ),
    ];
    let page = adw::PreferencesPage::new();
    for (title, rows) in sections {
        let group = adw::PreferencesGroup::builder().title(*title).build();
        for (accel, what) in *rows {
            let row = adw::ActionRow::builder().title(*what).build();
            let keys = gtk::Box::builder()
                .spacing(6)
                .valign(gtk::Align::Center)
                .build();
            for a in accel.split(' ') {
                if a.starts_with('<') || a.starts_with('F') || a == "Delete" {
                    // ShortcutLabel is deprecated in GTK 4.18; draw keycaps.
                    let label = gtk::accelerator_parse(a)
                        .map(|(key, mods)| gtk::accelerator_get_label(key, mods).to_string())
                        .unwrap_or_else(|| a.to_string());
                    keys.append(
                        &gtk::Label::builder()
                            .label(label)
                            .css_classes(["keycap"])
                            .build(),
                    );
                } else {
                    keys.append(
                        &gtk::Label::builder()
                            .label(a)
                            .css_classes(["dim-label"])
                            .build(),
                    );
                }
            }
            row.add_suffix(&keys);
            group.add(&row);
        }
        page.add(&group);
    }
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&page));
    adw::Dialog::builder()
        .title("Keyboard Shortcuts")
        .content_width(560)
        .content_height(640)
        .child(&view)
        .build()
}

impl Win {
    pub(super) fn show_about(&self) {
        let about = adw::AboutDialog::builder()
            .application_name("omaquill")
            .application_icon(super::APP_ID)
            .version(env!("CARGO_PKG_VERSION"))
            .developer_name("Casey Shields")
            .license_type(gtk::License::MitX11)
            .website("https://github.com/shieldsworks/omaquill")
            .issue_url("https://github.com/shieldsworks/omaquill/issues")
            .comments("A writing studio for Omarchy that opens Scrivener projects.")
            .build();
        about.present(Some(&self.window));
    }

    pub(super) fn show_preferences(self: &Rc<Self>) {
        let s = self.settings.borrow().clone();
        let page = adw::PreferencesPage::new();

        let text = adw::PreferencesGroup::builder().title("Text").build();
        let zoom = adw::SpinRow::with_range(50.0, 300.0, 5.0);
        zoom.set_title("Zoom");
        zoom.set_subtitle("Percent of the document's own text size");
        zoom.set_value((s.zoom * 100.0).round());
        text.add(&zoom);
        let own_fonts = adw::SwitchRow::builder()
            .title("Use the Document's Fonts")
            .subtitle("Off: show everything in one font. The file keeps its fonts either way.")
            .active(s.editor_font.is_none())
            .build();
        text.add(&own_fonts);
        let font_button = gtk::FontDialogButton::new(Some(gtk::FontDialog::new()));
        font_button.set_level(gtk::FontLevel::Family);
        font_button.set_valign(gtk::Align::Center);
        let desc =
            gtk::pango::FontDescription::from_string(s.editor_font.as_deref().unwrap_or("serif"));
        font_button.set_font_desc(&desc);
        let font_row = adw::ActionRow::builder().title("Editor Font").build();
        font_row.add_suffix(&font_button);
        font_row.set_sensitive(s.editor_font.is_some());
        text.add(&font_row);
        let subs = rich::look().substitutes;
        let mut pairs: Vec<(String, String)> = subs.into_iter().collect();
        pairs.sort();
        pairs.dedup_by(|a, b| a.1 == b.1 && a.0.split(' ').next() == b.0.split(' ').next());
        let note = if pairs.is_empty() {
            "Mac fonts like Palatino aren't installed here, so similar fonts stand in for them. Install tex-gyre-fonts for close matches.".to_string()
        } else {
            let list: Vec<String> = pairs
                .iter()
                .filter(|(mac, _)| {
                    ["Palatino", "Times", "Helvetica", "Courier", "Baskerville"]
                        .contains(&mac.as_str())
                })
                .map(|(mac, sub)| format!("{mac} → {sub}"))
                .collect();
            format!("Stand-ins for Mac fonts: {}", list.join(", "))
        };
        text.set_description(Some(&note));
        page.add(&text);

        let writing = adw::PreferencesGroup::builder().title("Writing").build();
        let target = adw::SpinRow::with_range(0.0, 100_000.0, 50.0);
        target.set_title("Daily Word Target");
        target.set_subtitle("Shown in the status bar and the Omarchy bar widget. 0 turns it off.");
        target.set_value(s.daily_target as f64);
        writing.add(&target);
        let author = adw::EntryRow::builder()
            .title("Author Name")
            .text(&s.author)
            .build();
        writing.add(&author);
        page.add(&writing);

        let backups = adw::PreferencesGroup::builder()
            .title("Backups")
            .description("omaquill zips the whole project each time it opens it.")
            .build();
        let keep = adw::SpinRow::with_range(0.0, 100.0, 1.0);
        keep.set_title("Backups to Keep");
        keep.set_subtitle("Per project. 0 turns backups off.");
        keep.set_value(s.backups as f64);
        backups.add(&keep);
        let folder = adw::ActionRow::builder()
            .title("Backups Folder")
            .subtitle(glib::markup_escape_text(
                &omaquill::state::backup_dir().display().to_string(),
            ))
            .activatable(true)
            .action_name("win.show-backups")
            .build();
        folder.add_suffix(&gtk::Image::from_icon_name("folder-open-symbolic"));
        backups.add(&folder);
        page.add(&backups);

        let dialog = adw::PreferencesDialog::new();
        dialog.add(&page);
        let apply = {
            let weak = Rc::downgrade(self);
            let (zoom, own_fonts, font_button, target, author, keep) = (
                zoom.clone(),
                own_fonts.clone(),
                font_button.clone(),
                target.clone(),
                author.clone(),
                keep.clone(),
            );
            let font_row = font_row.clone();
            Rc::new(move || {
                let Some(win) = weak.upgrade() else { return };
                font_row.set_sensitive(!own_fonts.is_active());
                {
                    let mut s = win.settings.borrow_mut();
                    s.zoom = zoom.value() / 100.0;
                    s.editor_font = if own_fonts.is_active() {
                        None
                    } else {
                        font_button
                            .font_desc()
                            .and_then(|d| d.family().map(|f| f.to_string()))
                    };
                    s.daily_target = target.value() as u32;
                    s.author = author.text().to_string();
                    s.backups = keep.value() as usize;
                    let _ = s.save();
                }
                win.apply_look();
                win.update_totals();
            })
        };
        let a = apply.clone();
        zoom.connect_value_notify(move |_| a());
        let a = apply.clone();
        own_fonts.connect_active_notify(move |_| a());
        let a = apply.clone();
        font_button.connect_font_desc_notify(move |_| a());
        let a = apply.clone();
        target.connect_value_notify(move |_| a());
        let a = apply.clone();
        author.connect_changed(move |_| a());
        let a = apply.clone();
        keep.connect_value_notify(move |_| a());
        dialog.present(Some(&self.window));
    }

    pub(super) fn show_compile(self: &Rc<Self>) {
        let Some(mut opts) = self.project.borrow().as_ref().map(Options::for_project) else {
            self.toast("Open a project first");
            return;
        };
        self.save_now();
        opts.author = self.settings.borrow().author.clone();

        let formats: Vec<&str> = Format::ALL.iter().map(|f| f.label()).collect();
        let format = adw::ComboRow::builder()
            .title("Format")
            .model(&gtk::StringList::new(&formats))
            .build();
        let title = adw::EntryRow::builder()
            .title("Title")
            .text(&opts.title)
            .build();
        let author = adw::EntryRow::builder()
            .title("Author")
            .text(&opts.author)
            .build();
        let manuscript = adw::SwitchRow::builder()
            .title("Standard Manuscript Format")
            .subtitle(
                "Times 12 pt, double spaced, 1\" margins, title page. For agents and editors.",
            )
            .active(opts.manuscript)
            .build();
        let headings = adw::ComboRow::builder()
            .title("Chapter Headings")
            .model(&gtk::StringList::new(&[
                "Chapter One, Chapter Two…",
                "Binder Titles",
                "None",
            ]))
            .build();
        let separator = adw::EntryRow::builder()
            .title("Scene Separator")
            .text(&opts.scene_separator)
            .build();
        let words = {
            let p = self.project.borrow();
            p.as_ref()
                .and_then(|p| compile::gather(p, &opts).ok())
                .map(|s| compile::word_count(&s))
                .unwrap_or(0)
        };
        let group = adw::PreferencesGroup::builder()
            .description(format!(
                "Compiles everything in the manuscript marked Include in Compile: {} words.",
                super::group(words)
            ))
            .build();
        group.add(&format);
        group.add(&title);
        group.add(&author);
        group.add(&manuscript);
        group.add(&headings);
        group.add(&separator);
        let page = adw::PreferencesPage::new();
        page.add(&group);
        let go = gtk::Button::builder()
            .label("Compile…")
            .css_classes(["suggested-action"])
            .build();
        let header = adw::HeaderBar::new();
        header.pack_end(&go);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&page));
        let dialog = adw::Dialog::builder()
            .title("Compile")
            .content_width(520)
            .child(&view)
            .build();

        let weak = Rc::downgrade(self);
        let d = dialog.clone();
        go.connect_clicked(move |_| {
            let Some(win) = weak.upgrade() else { return };
            let fmt = Format::ALL[format.selected() as usize % Format::ALL.len()];
            let opts = Options {
                title: title.text().trim().to_string(),
                author: author.text().trim().to_string(),
                manuscript: manuscript.is_active(),
                headings: match headings.selected() {
                    1 => Headings::Titles,
                    2 => Headings::None,
                    _ => Headings::Numbered,
                },
                scene_separator: separator.text().to_string(),
            };
            if !opts.author.is_empty() {
                let mut s = win.settings.borrow_mut();
                if s.author != opts.author {
                    s.author = opts.author.clone();
                    let _ = s.save();
                }
            }
            let name = format!(
                "{}.{}",
                if opts.title.is_empty() {
                    "Manuscript"
                } else {
                    &opts.title
                },
                fmt.extension()
            );
            let chooser = gtk::FileDialog::builder()
                .title("Compile To")
                .initial_name(name)
                .modal(true)
                .build();
            let weak = Rc::downgrade(&win);
            let d = d.clone();
            chooser.save(Some(&win.window), gio::Cancellable::NONE, move |res| {
                let (Some(win), Ok(file)) = (weak.upgrade(), res) else {
                    return;
                };
                let Some(path) = file.path() else { return };
                let bytes = {
                    let p = win.project.borrow();
                    let Some(p) = p.as_ref() else { return };
                    compile::compile(p, &opts, fmt)
                };
                let result = bytes
                    .map_err(|e| e.to_string())
                    .and_then(|b| std::fs::write(&path, b).map_err(|e| e.to_string()));
                match result {
                    Ok(()) => {
                        d.close();
                        let toast = adw::Toast::builder()
                            .title(format!(
                                "Compiled {}",
                                path.file_name().unwrap_or_default().to_string_lossy()
                            ))
                            .button_label("Open")
                            .build();
                        let uri = gio::File::for_path(&path).uri();
                        toast.connect_button_clicked(move |_| {
                            let _ = gio::AppInfo::launch_default_for_uri(
                                &uri,
                                gio::AppLaunchContext::NONE,
                            );
                        });
                        win.toasts.add_toast(toast);
                    }
                    Err(e) => win.toast(&format!("Compile failed: {e}")),
                }
            });
        });
        dialog.present(Some(&self.window));
    }
}

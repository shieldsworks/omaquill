//! Dialogs: rename, Compile, preferences, keyboard shortcuts, about.

use super::{Win, rich};
use adw::prelude::*;
use gtk::{gio, glib};
use omaquill::compile::{self, Format, Headings, Options};
use omaquill::export::{self, Done, Stop};
use std::path::PathBuf;
use std::rc::Rc;

/// The parts of the Compile dialog that show a compile running.
#[derive(Clone)]
struct CompileUi {
    dialog: adw::Dialog,
    go: gtk::Button,
    cancel: gtk::Button,
}

impl CompileUi {
    fn busy(&self, busy: bool) {
        self.go.set_sensitive(!busy);
        self.go
            .set_label(if busy { "Compiling…" } else { "Compile…" });
        self.cancel.set_visible(busy);
        self.cancel.set_sensitive(busy);
    }
}

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
                ("<Ctrl>h", "Find and replace"),
                ("<Ctrl>z", "Undo"),
                ("<Ctrl><Shift>z", "Redo"),
                ("F11", "Composition mode"),
                ("F7", "Turn spelling marks on or off"),
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
            .comments("A free writing studio for authors and writers on Linux and Omarchy.")
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

        let spelling = adw::PreferencesGroup::builder()
            .title("Spelling")
            .description("Leave Language blank to follow the system. A tag such as en_GB picks that dictionary.")
            .build();
        let spell_on = adw::SwitchRow::builder()
            .title("Check Spelling")
            .subtitle("Red marks under misspelled words. F7 turns this off.")
            .active(s.spellcheck)
            .build();
        spelling.add(&spell_on);
        let in_compose = adw::SwitchRow::builder()
            .title("Show Marks in Composition Mode")
            .subtitle("Off while composing, so the page stays quiet.")
            .active(s.spell_in_compose)
            .build();
        spelling.add(&in_compose);
        let wait = adw::SwitchRow::builder()
            .title("Wait to Mark the Word Being Typed")
            .subtitle("The mark appears when the cursor leaves the word.")
            .active(s.spell_hide_current)
            .build();
        spelling.add(&wait);
        let language = adw::EntryRow::builder()
            .title("Language")
            .text(&s.spell_language)
            .build();
        spelling.add(&language);
        page.add(&spelling);

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
        let look = {
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
            move || {
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
                }
                win.persist_settings();
                win.apply_look();
                win.update_totals();
            }
        };
        let look = Rc::new(look);
        let spell_apply = {
            let weak = Rc::downgrade(self);
            let (spell_on, in_compose, wait, language) = (
                spell_on.clone(),
                in_compose.clone(),
                wait.clone(),
                language.clone(),
            );
            move || {
                let Some(win) = weak.upgrade() else { return };
                {
                    let mut s = win.settings.borrow_mut();
                    s.spellcheck = spell_on.is_active();
                    s.spell_in_compose = in_compose.is_active();
                    s.spell_hide_current = wait.is_active();
                    s.spell_language = language.text().trim().to_string();
                }
                win.persist_settings();
                win.spell
                    .apply_settings(&win.settings.borrow(), win.composing.get());
                if let Some(action) = win
                    .window
                    .lookup_action("spellcheck")
                    .and_then(|a| a.downcast::<gio::SimpleAction>().ok())
                {
                    action.set_state(&spell_on.is_active().to_variant());
                }
            }
        };
        let spell_apply = Rc::new(spell_apply);
        let a = look.clone();
        zoom.connect_value_notify(move |_| a());
        let a = look.clone();
        own_fonts.connect_active_notify(move |_| a());
        let a = look.clone();
        font_button.connect_font_desc_notify(move |_| a());
        let a = look.clone();
        target.connect_value_notify(move |_| a());
        let a = look.clone();
        author.connect_changed(move |_| a());
        let a = look.clone();
        keep.connect_value_notify(move |_| a());
        let a = spell_apply.clone();
        spell_on.connect_active_notify(move |_| a());
        let a = spell_apply.clone();
        in_compose.connect_active_notify(move |_| a());
        let a = spell_apply.clone();
        wait.connect_active_notify(move |_| a());
        let a = spell_apply.clone();
        language.connect_changed(move |_| a());
        dialog.present(Some(&self.window));
    }

    pub(super) fn show_compile(self: &Rc<Self>) {
        if self.compiling.borrow().is_some() {
            self.toast("A compile is already running");
            return;
        }
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
        // For PDF the switch picks between the two page layouts.
        {
            let manuscript = manuscript.clone();
            let describe = move |f: Format| {
                manuscript.set_subtitle(if f == Format::Pdf {
                    "On: US Letter, Times 12 pt, double spaced, title page. Off: a 6×9\" paperback in your book's typeface."
                } else {
                    "Times 12 pt, double spaced, 1\" margins, title page. For agents and editors."
                });
            };
            describe(Format::ALL[0]);
            format.connect_selected_notify(move |row| {
                describe(Format::ALL[row.selected() as usize % Format::ALL.len()]);
            });
        }
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
        let cancel = gtk::Button::builder()
            .label("Cancel")
            .visible(false)
            .build();
        let header = adw::HeaderBar::new();
        header.pack_end(&go);
        header.pack_start(&cancel);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&page));
        let dialog = adw::Dialog::builder()
            .title("Compile")
            .content_width(520)
            .child(&view)
            .build();
        let ui = CompileUi {
            dialog: dialog.clone(),
            go: go.clone(),
            cancel: cancel.clone(),
        };

        let weak = Rc::downgrade(self);
        cancel.connect_clicked(move |cancel| {
            let Some(win) = weak.upgrade() else { return };
            win.stop_compile();
            cancel.set_sensitive(false);
        });
        let weak = Rc::downgrade(self);
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
                    drop(s);
                    win.persist_settings();
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
            let ui = ui.clone();
            chooser.save(Some(&win.window), gio::Cancellable::NONE, move |res| {
                let (Some(win), Ok(file)) = (weak.upgrade(), res) else {
                    return;
                };
                let Some(path) = file.path() else { return };
                win.compile_to(&ui, opts, fmt, path);
            });
        });
        dialog.present(Some(&self.window));
    }

    /// Gathers the manuscript here, on the thread that owns the project, so
    /// edits made while the compile runs stay out of it. A worker renders
    /// and writes that copy.
    fn compile_to(self: &Rc<Self>, ui: &CompileUi, opts: Options, format: Format, path: PathBuf) {
        if self.compiling.borrow().is_some() {
            self.toast("A compile is already running");
            return;
        }
        let sections = {
            let p = self.project.borrow();
            let Some(p) = p.as_ref() else { return };
            compile::gather(p, &opts)
        };
        let sections = match sections {
            Ok(sections) => sections,
            Err(e) => {
                self.toast(&format!("Compile failed: {e}"));
                return;
            }
        };
        let job = export::Job {
            sections,
            opts,
            format,
            path: path.clone(),
        };
        let renderer = renderer();
        self.run_compile(ui, path, move |stop| export::run(job, stop, &renderer));
    }

    /// Runs `work` on a worker thread and reports its result here, on the
    /// main loop, which keeps running meanwhile. The dialog shows the
    /// compile until then, and a second compile can't start.
    fn run_compile(
        self: &Rc<Self>,
        ui: &CompileUi,
        path: PathBuf,
        work: impl FnOnce(&Stop) -> Result<Done, String> + Send + 'static,
    ) {
        let stop = Stop::default();
        *self.compiling.borrow_mut() = Some(stop.clone());
        ui.busy(true);
        // Closing the last window, as logout's SIGTERM does, stops the
        // compile; the app runs on until the worker has put the target back
        // as it was or replaced it whole.
        let hold = self.window.application().map(|app| app.hold());
        let worker = gio::spawn_blocking(move || work(&stop));
        let weak = Rc::downgrade(self);
        let ui = ui.clone();
        glib::spawn_future_local(async move {
            let _hold = hold;
            let done = worker.await.unwrap_or_else(|panic| {
                let why = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned());
                Err(match why {
                    Some(why) => format!("the compiler stopped: {why}"),
                    None => "the compiler stopped".to_string(),
                })
            });
            ui.busy(false);
            let Some(win) = weak.upgrade() else { return };
            *win.compiling.borrow_mut() = None;
            match done {
                Ok(Done::Written) => {
                    // The user may have closed the dialog meanwhile.
                    if ui.dialog.parent().is_some() {
                        ui.dialog.close();
                    }
                    let toast = adw::Toast::builder()
                        .title(format!(
                            "Compiled {}",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        ))
                        .button_label("Open")
                        .build();
                    let uri = gio::File::for_path(&path).uri();
                    toast.connect_button_clicked(move |_| {
                        let _ =
                            gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE);
                    });
                    win.toasts.add_toast(toast);
                }
                Ok(Done::Stopped) => win.toast("Compile canceled"),
                Err(e) => win.toast(&format!("Compile failed: {e}")),
            }
        });
    }

    /// Stops a running compile before it replaces its target, if it is not
    /// too late.
    pub(super) fn stop_compile(&self) {
        if let Some(stop) = self.compiling.borrow().as_ref() {
            stop.stop();
        }
    }
}

/// The program that renders PDFs: this one, as the kernel holds it, so a
/// package upgrade that replaces the file on disk does not change it.
#[cfg(not(test))]
fn renderer() -> PathBuf {
    PathBuf::from("/proc/self/exe")
}

/// The tests run in libtest's program. Cargo builds omaquill's own next to
/// it, a folder up, for the tests in `tests/`.
#[cfg(test)]
fn renderer() -> PathBuf {
    let exe = std::env::current_exe().expect("the test program's path");
    let folder = exe
        .parent()
        .and_then(std::path::Path::parent)
        .expect("a target folder");
    folder.join("omaquill")
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use omaquill::project::{Kind, Project};
    use omaquill::rtf::RichText;
    use std::cell::RefCell;
    use std::path::Path;
    use std::time::{Duration, Instant};

    /// A temporary folder, removed on drop.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// One paragraph of about `bytes` bytes of words.
    fn paragraph(bytes: usize) -> String {
        let words = [
            "the",
            "lighthouse",
            "keeper",
            "remembered",
            "extraordinary",
            "storms",
            "and",
            "unaccountable",
            "silences",
        ];
        let mut text = String::new();
        for word in words.iter().cycle() {
            if text.len() >= bytes {
                break;
            }
            text.push_str(word);
            text.push(' ');
        }
        text
    }

    /// A window on a new project whose one chapter is `text`, with the
    /// Compile dialog open. Returns the chapter's id too.
    fn compile_window(tag: &str, text: &str) -> (Scratch, Rc<Win>, CompileUi, String) {
        let dir =
            std::env::temp_dir().join(format!("omaquill-compile-ui-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = Scratch(dir);
        let path = dir.0.join("Novel.scriv");
        let mut project = Project::create(&path).unwrap();
        let draft = project.root_folder(Kind::Draft).unwrap().uuid;
        let chapter = project
            .add_item(Kind::Text, "Chapter", &draft, None)
            .unwrap();
        project
            .set_text(&chapter, &RichText::from_plain(text))
            .unwrap();
        project.save().unwrap();
        drop(project);
        let app = adw::Application::builder()
            .application_id(format!("io.github.shieldsworks.Omaquill.Compile{tag}"))
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let win = Win::new(&app);
        win.open_project(&path);
        win.window.present();
        win.show_compile();
        let ui = dialog_ui(&win);
        pump(Duration::from_millis(200));
        (dir, win, ui, chapter)
    }

    /// The Compile dialog on show in `win`.
    fn dialog_ui(win: &Win) -> CompileUi {
        let dialog = win.window.visible_dialog().expect("the Compile dialog");
        let button = |label: &str| {
            let mut stack = vec![dialog.clone().upcast::<gtk::Widget>()];
            while let Some(w) = stack.pop() {
                if let Ok(b) = w.clone().downcast::<gtk::Button>()
                    && b.label().as_deref() == Some(label)
                {
                    return b;
                }
                let mut child = w.first_child();
                while let Some(c) = child {
                    child = c.next_sibling();
                    stack.push(c);
                }
            }
            panic!("no {label} button in the Compile dialog");
        };
        CompileUi {
            go: button("Compile…"),
            cancel: button("Cancel"),
            dialog,
        }
    }

    /// Starts the compile of `path` the dialog would start, on a worker that
    /// waits for a send on the returned channel first.
    fn held_compile(
        win: &Rc<Win>,
        ui: &CompileUi,
        format: Format,
        path: &Path,
    ) -> std::sync::mpsc::Sender<()> {
        let opts = book(win);
        let sections = compile::gather(win.project.borrow().as_ref().unwrap(), &opts).unwrap();
        let job = export::Job {
            sections,
            opts,
            format,
            path: path.to_path_buf(),
        };
        let (start, wait) = std::sync::mpsc::channel();
        win.run_compile(ui, path.to_path_buf(), move |stop| {
            let _ = wait.recv();
            export::run(job, stop, &renderer())
        });
        start
    }

    /// Runs the main loop for `time`.
    fn pump(time: Duration) {
        let context = glib::MainContext::default();
        let end = Instant::now() + time;
        while Instant::now() < end {
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Runs the main loop until `done` holds.
    fn pump_until(done: impl Fn() -> bool) {
        let context = glib::MainContext::default();
        let end = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < end, "timed out");
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Runs the main loop until the window's compile reports back.
    fn finish(win: &Win) {
        let context = glib::MainContext::default();
        let end = Instant::now() + Duration::from_secs(120);
        while win.compiling.borrow().is_some() {
            assert!(Instant::now() < end, "the compile never reported back");
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(1));
        }
        pump(Duration::from_millis(100));
    }

    fn book(win: &Win) -> Options {
        Options {
            manuscript: false,
            ..Options::for_project(win.project.borrow().as_ref().unwrap())
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// The longest the main loop may go without running a 10 ms timeout while
    /// a compile runs.
    const STALL: Duration = Duration::from_millis(100);

    /// Calls `start`, which starts a compile in `win`, and runs the main loop
    /// until the compile reports back. Returns how long that took and the
    /// longest the main loop went without running a 10 ms timeout meanwhile.
    fn longest_gap(win: &Win, start: impl FnOnce()) -> (Duration, Duration) {
        let ticks = Rc::new(RefCell::new(Vec::new()));
        let tick = {
            let ticks = Rc::clone(&ticks);
            glib::timeout_add_local(Duration::from_millis(10), move || {
                ticks.borrow_mut().push(Instant::now());
                glib::ControlFlow::Continue
            })
        };
        let started = Instant::now();
        start();
        let context = glib::MainContext::default();
        while win.compiling.borrow().is_some() {
            assert!(started.elapsed() < Duration::from_secs(120));
            context.iteration(true);
        }
        let took = started.elapsed();
        tick.remove();
        let mut gap = Duration::ZERO;
        let mut last = started;
        for &t in ticks.borrow().iter() {
            gap = gap.max(t - last);
            last = t;
        }
        (took, gap)
    }

    pub fn a_compile_leaves_the_main_loop_running() {
        let (dir, win, ui, _) = compile_window("Busy", &paragraph(150_000));
        let target = dir.0.join("Out.pdf");
        // Under xvfb GTK draws through Mesa's llvmpipe, which compiles the
        // shaders for each drawing state it has not met on the main thread,
        // in gsk_renderer_render. With an empty shader cache, as on a new CI
        // runner, that holds the main loop for 250-390 ms the first time the
        // busy dialog is drawn and up to 210 ms the second time, whatever the
        // worker does. Workers that only sleep take the dialog through the
        // same states without touching Pango, until one of them leaves the
        // main loop under the limit. The compile measured after that draws
        // only the states the workers drew, so its gap is the compile's own.
        let mut quiet = None;
        for _ in 0..6 {
            let (_, gap) = longest_gap(&win, || {
                win.run_compile(&ui, target.clone(), |_| {
                    std::thread::sleep(Duration::from_millis(300));
                    Ok(Done::Stopped)
                });
            });
            eprintln!("a worker that sleeps 300 ms: longest gap {gap:?}");
            if gap < STALL {
                quiet = Some(gap);
                break;
            }
        }
        let quiet = quiet.expect("the main loop stalls with a worker that only sleeps");
        let second = dir.0.join("Second.md");
        let (took, gap) = longest_gap(&win, || {
            win.compile_to(&ui, book(&win), Format::Pdf, target.clone());
            assert!(!ui.go.is_sensitive());
            assert_eq!(ui.go.label().as_deref(), Some("Compiling…"));
            assert!(ui.cancel.is_visible() && ui.cancel.is_sensitive());
            // A second compile, from this dialog or a new one, does not start.
            win.compile_to(&ui, book(&win), Format::Markdown, second.clone());
            assert_eq!(
                win.last_toast.borrow().as_str(),
                "A compile is already running"
            );
            win.last_toast.borrow_mut().clear();
            win.show_compile();
            assert_eq!(
                win.last_toast.borrow().as_str(),
                "A compile is already running"
            );
            assert_eq!(win.window.visible_dialog().as_ref(), Some(&ui.dialog));
        });
        eprintln!(
            "compile of a 150 KB paragraph: {took:?}, longest gap {gap:?} \
             against {quiet:?} with a worker that sleeps"
        );
        assert!(
            took >= Duration::from_millis(150),
            "the compile took {took:?}, too quick to show the main loop running"
        );
        assert!(gap < STALL, "the main loop stalled {gap:?}");
        pump(Duration::from_millis(300));
        assert!(std::fs::read(&target).unwrap().starts_with(b"%PDF-"));
        assert!(!second.exists());
        assert_eq!(names(&dir.0), ["Novel.scriv", "Out.pdf"]);
        assert!(ui.go.is_sensitive());
        assert_eq!(ui.go.label().as_deref(), Some("Compile…"));
        assert!(!ui.cancel.is_visible());
        assert!(
            win.window.visible_dialog().is_none(),
            "a finished compile closes the dialog"
        );
        win.window.close();
        win.join_backup_reader();
    }

    pub fn cancel_and_closing_the_window_keep_the_target() {
        let (dir, win, ui, _) = compile_window("Cancel", "a short chapter");
        let target = dir.0.join("Out.pdf");
        std::fs::write(&target, "the last good compile").unwrap();
        let start = held_compile(&win, &ui, Format::Pdf, &target);
        ui.cancel.emit_clicked();
        assert!(!ui.cancel.is_sensitive());
        assert!(
            !ui.go.is_sensitive(),
            "no new compile until the worker stops"
        );
        start.send(()).unwrap();
        finish(&win);
        assert_eq!(win.last_toast.borrow().as_str(), "Compile canceled");
        assert_eq!(std::fs::read(&target).unwrap(), b"the last good compile");
        assert_eq!(names(&dir.0), ["Novel.scriv", "Out.pdf"]);
        assert!(ui.go.is_sensitive());
        assert_eq!(win.window.visible_dialog().as_ref(), Some(&ui.dialog));

        // Closing the dialog leaves the compile running; it still writes.
        let other = dir.0.join("Other.pdf");
        let start = held_compile(&win, &ui, Format::Pdf, &other);
        ui.dialog.close();
        pump_until(|| win.window.visible_dialog().is_none());
        assert!(win.compiling.borrow().is_some());
        start.send(()).unwrap();
        finish(&win);
        assert!(std::fs::read(&other).unwrap().starts_with(b"%PDF-"));

        // Closing the window stops it. A window's close takes an open
        // dialog first, so the dialog goes first here.
        win.show_compile();
        let ui = dialog_ui(&win);
        pump(Duration::from_millis(200));
        let start = held_compile(&win, &ui, Format::Pdf, &target);
        ui.dialog.close();
        pump_until(|| win.window.visible_dialog().is_none());
        win.window.close();
        pump_until(|| !win.window.is_visible());
        assert!(win.compiling.borrow().is_some());
        start.send(()).unwrap();
        finish(&win);
        assert_eq!(std::fs::read(&target).unwrap(), b"the last good compile");
        assert_eq!(names(&dir.0), ["Novel.scriv", "Other.pdf", "Out.pdf"]);
        win.join_backup_reader();
    }

    pub fn closing_every_window_waits_for_the_compile_to_finish() {
        let (dir, win, ui, _) = compile_window("Quit", "a short chapter");
        let target = dir.0.join("Out.pdf");
        std::fs::write(&target, "the last good compile").unwrap();
        ui.dialog.close();
        pump_until(|| win.window.visible_dialog().is_none());
        let start = held_compile(&win, &ui, Format::Pdf, &target);
        let app = win.window.application().unwrap();
        app.connect_activate(|_| {});
        // As at logout: SIGTERM closes every window, then the worker gets
        // to the target.
        glib::idle_add_local_once({
            let app = app.clone();
            move || super::super::close_every_window(&app)
        });
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            start.send(()).unwrap();
        });
        app.run_with_args::<&str>(&[]);
        assert!(
            win.compiling.borrow().is_none(),
            "the app stopped before its compile reported back"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"the last good compile");
        assert_eq!(names(&dir.0), ["Novel.scriv", "Out.pdf"]);
        win.join_backup_reader();
    }

    pub fn an_edit_during_a_compile_stays_out_of_it() {
        let (dir, win, ui, chapter) = compile_window("Edit", &paragraph(150_000));
        let opts = book(&win);
        let target = dir.0.join("Out.txt");
        let expected = {
            let p = win.project.borrow();
            compile::compile(p.as_ref().unwrap(), &opts, Format::PlainText).unwrap()
        };
        win.compile_to(&ui, opts, Format::PlainText, target.clone());
        win.with_project(|p| {
            p.set_text(&chapter, &RichText::from_plain("rewritten"))
                .unwrap();
            p.save().unwrap();
        });
        finish(&win);
        assert_eq!(std::fs::read(&target).unwrap(), expected);
        win.window.close();
        win.join_backup_reader();
    }

    pub fn a_failed_or_panicking_compile_is_reported() {
        let (dir, win, ui, _) = compile_window("Fail", "a short chapter");
        let missing = dir.0.join("no such folder").join("Out.docx");
        win.compile_to(&ui, book(&win), Format::Docx, missing.clone());
        finish(&win);
        let toast = win.last_toast.borrow().clone();
        assert!(
            toast.starts_with("Compile failed: ") && toast.contains("No such file"),
            "{toast}"
        );
        assert_eq!(win.window.visible_dialog().as_ref(), Some(&ui.dialog));
        assert!(ui.go.is_sensitive());

        win.run_compile(&ui, missing, |_| panic!("this test panics on purpose"));
        finish(&win);
        assert_eq!(
            win.last_toast.borrow().as_str(),
            "Compile failed: the compiler stopped: this test panics on purpose"
        );
        assert!(ui.go.is_sensitive());
        assert_eq!(ui.go.label().as_deref(), Some("Compile…"));
        // The window still compiles.
        let target = dir.0.join("Out.md");
        win.compile_to(&ui, book(&win), Format::Markdown, target.clone());
        finish(&win);
        assert!(target.exists());
        win.window.close();
        win.join_backup_reader();
    }
}

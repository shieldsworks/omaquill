//! The GTK4 app: one window per open project.
//!
//! ```text
//! ┌ header: binder toggle, new, title, views, compose, inspector, menu ┐
//! │ binder │ editor / corkboard / outliner / media │ inspector        │
//! └ status: document words · manuscript words · today              ┘
//! ```

mod binder;
mod board;
mod dialogs;
mod editor;
mod inspector;
pub mod rich;
mod theme;

use adw::prelude::*;
use editor::Editor;
use gtk::{gio, glib};
use omaquill::project::{Item, Kind, Project};
use omaquill::rtf;
use omaquill::state::{ProjectView, Settings, Today};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub const APP_ID: &str = "io.github.shieldsworks.Omaquill";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FolderView {
    Text,
    Corkboard,
    Outliner,
}

pub struct Win {
    pub window: adw::ApplicationWindow,
    settings: RefCell<Settings>,
    project: RefCell<Option<Project>>,
    /// The binder item on show.
    current: RefCell<Option<String>>,
    /// The item whose text is in the editor. Editor text is only ever
    /// saved here, never to `current`, so a failed load or save can't send
    /// one document's words into another.
    editor_uuid: RefCell<Option<String>>,
    dirty_text: Cell<bool>,
    dirty_notes: Cell<bool>,
    dirty_synopsis: Cell<bool>,
    save_source: RefCell<Option<glib::SourceId>>,
    folder_view: Cell<FolderView>,
    composing: Cell<bool>,
    /// Words in each text item, for the manuscript total.
    counts: RefCell<HashMap<String, usize>>,
    /// Lowercased text of each item, for project search.
    search_text: RefCell<HashMap<String, String>>,
    today: RefCell<Option<Today>>,
    label_css: gtk::CssProvider,

    toolbar_view: adw::ToolbarView,
    title: adw::WindowTitle,
    outer: adw::OverlaySplitView,
    inner: adw::OverlaySplitView,
    stack: gtk::Stack,
    toasts: adw::ToastOverlay,
    banner: adw::Banner,
    editor: Rc<Editor>,
    notes: Rc<Editor>,
    format_bar: gtk::Box,
    bold: gtk::ToggleButton,
    italic: gtk::ToggleButton,
    underline: gtk::ToggleButton,
    align: gtk::DropDown,
    updating_format: Cell<bool>,
    view_buttons: gtk::Box,
    view_text: gtk::ToggleButton,
    view_cork: gtk::ToggleButton,
    view_outline: gtk::ToggleButton,
    doc_words: gtk::Label,
    total_words: gtk::Label,
    save_state: gtk::Label,
    welcome_recent: gtk::ListBox,

    binder: binder::Binder,
    board: board::Board,
    inspector: inspector::Inspector,
}

pub fn run() -> glib::ExitCode {
    // A test or demo copy can run beside the real one under its own id
    // (OMAQUILL_APP_ID); with the same id GTK hands it to the running app.
    let app_id = std::env::var("OMAQUILL_APP_ID").unwrap_or_else(|_| APP_ID.to_string());
    let app = adw::Application::builder()
        .application_id(app_id)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    let theme: Rc<RefCell<Option<theme::Theme>>> = Rc::default();
    {
        let theme = theme.clone();
        app.connect_startup(move |app| {
            *theme.borrow_mut() = Some(theme::Theme::install());
            install_base_css();
            icon_fallback();
            set_accels(app);
            let quit = gio::ActionEntry::builder("quit")
                .activate(|app: &adw::Application, _, _| {
                    for w in app.windows() {
                        w.close();
                    }
                })
                .build();
            app.add_action_entries([quit]);
        });
    }
    app.connect_activate(|app| {
        // Already running (the bar widget, the launcher): bring it forward.
        if let Some(w) = app
            .active_window()
            .or_else(|| app.windows().into_iter().next())
        {
            w.present();
            return;
        }
        let win = Win::new(app);
        // Reopen the last project, the way Scrivener does.
        if let Some(last) = omaquill::state::recent().into_iter().next() {
            win.open_project(&last);
        }
        win.window.present();
    });
    app.connect_open(|app, files, _| {
        let shots = std::env::var_os("OMAQUILL_SCREENSHOT").map(PathBuf::from);
        for f in files {
            let Some(path) = f.path() else { continue };
            let path = project_dir(&path);
            // Already open? Bring that window up.
            if let Some(w) = find_window(app, &path) {
                w.present();
                continue;
            }
            let win = Win::new(app);
            win.open_project(&path);
            win.window.present();
            if let Some(shot) = &shots {
                win.screenshot_then_quit(shot.clone());
            }
        }
    });
    // Logout and shutdown send SIGTERM: close windows the normal way so
    // pending text is saved.
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        let app = app.clone();
        glib_unix::unix_signal_add_local(signal, move || {
            // Closing saves; a window whose save fails stays open with its
            // "Changes Not Saved" question rather than quitting over it.
            for w in app.windows() {
                w.close();
            }
            glib::ControlFlow::Break
        });
    }
    let _keep = theme;
    app.run()
}

/// A `.scrivx` inside a project opens the project.
fn project_dir(path: &Path) -> PathBuf {
    let dir = if path.extension().is_some_and(|e| e == "scrivx") {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    // One spelling per project, so the same one is never open twice.
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

thread_local! {
    static WINDOWS: RefCell<Vec<Rc<Win>>> = const { RefCell::new(Vec::new()) };
}

fn find_window(_app: &adw::Application, path: &Path) -> Option<adw::ApplicationWindow> {
    WINDOWS.with(|w| {
        w.borrow()
            .iter()
            .find(|win| {
                win.project
                    .borrow()
                    .as_ref()
                    .is_some_and(|p| p.path == path)
            })
            .map(|win| win.window.clone())
    })
}

fn set_accels(app: &adw::Application) {
    let accels: &[(&str, &[&str])] = &[
        ("app.quit", &["<Ctrl>q"]),
        ("win.save", &["<Ctrl>s"]),
        ("win.open", &["<Ctrl>o"]),
        ("win.new-project", &["<Ctrl><Shift>o"]),
        ("win.new-text", &["<Ctrl>n"]),
        ("win.new-folder", &["<Ctrl><Shift>n"]),
        ("win.rename", &["F2"]),
        ("win.trash", &["<Ctrl>Delete"]),
        ("win.move-up", &["<Ctrl><Alt>Up", "<Ctrl><Alt>k"]),
        ("win.move-down", &["<Ctrl><Alt>Down", "<Ctrl><Alt>j"]),
        ("win.indent", &["<Ctrl><Alt>Right", "<Ctrl><Alt>l"]),
        ("win.outdent", &["<Ctrl><Alt>Left", "<Ctrl><Alt>h"]),
        ("win.bold", &["<Ctrl>b"]),
        ("win.italic", &["<Ctrl>i"]),
        ("win.underline", &["<Ctrl>u"]),
        ("win.find", &["<Ctrl>f"]),
        ("win.project-search", &["<Ctrl><Shift>f"]),
        ("win.view-text", &["<Ctrl>1"]),
        ("win.view-corkboard", &["<Ctrl>2"]),
        ("win.view-outliner", &["<Ctrl>3"]),
        ("win.compose", &["F11"]),
        ("win.toggle-binder", &["<Ctrl><Alt>b"]),
        ("win.toggle-inspector", &["<Ctrl><Alt>i"]),
        ("win.zoom-in", &["<Ctrl>plus", "<Ctrl>equal"]),
        ("win.zoom-out", &["<Ctrl>minus"]),
        ("win.zoom-reset", &["<Ctrl>0"]),
        ("win.compile", &["<Ctrl><Shift>e"]),
        ("win.preferences", &["<Ctrl>comma"]),
        ("win.show-help-overlay", &["<Ctrl>question"]),
        ("win.focus-binder", &["<Ctrl><Alt>1"]),
        ("win.focus-editor", &["<Ctrl><Alt>2"]),
    ];
    for (action, keys) in accels {
        app.set_accels_for_action(action, keys);
    }
}

/// Adwaita's symbolic icons as a last resort. GTK only falls back to
/// hicolor, so a missing or partial icon theme (an Omarchy theme can name
/// one that isn't installed) would leave broken-image squares. Icons the
/// real theme has still win: unthemed search-path icons come last.
fn icon_fallback() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let theme = gtk::IconTheme::for_display(&display);
    for dir in [
        "actions",
        "places",
        "mimetypes",
        "status",
        "ui",
        "apps",
        "devices",
    ] {
        for root in [
            "/usr/share/icons/Adwaita/symbolic",
            "/usr/local/share/icons/Adwaita/symbolic",
        ] {
            let path = Path::new(root).join(dir);
            if path.is_dir() {
                theme.add_search_path(&path);
            }
        }
    }
}

fn install_base_css() {
    let css = gtk::CssProvider::new();
    css.load_from_string(
        r#"
textview.omaquill-page { font-family: serif; font-size: 16px; }
textview.omaquill-page, textview.omaquill-page text { background: transparent; }
.omaquill-compose textview.omaquill-page { font-size: 17px; }
textview.omaquill-notes { font-size: 14px; }
.omaquill-binder row { padding: 2px 4px; }
.omaquill-binder .drop-before { box-shadow: inset 0 2px @accent_bg_color; }
.omaquill-binder .drop-after { box-shadow: inset 0 -2px @accent_bg_color; }
.omaquill-binder .drop-into { background: alpha(@accent_bg_color, 0.25); border-radius: 6px; }
.omaquill-dot { min-width: 8px; min-height: 8px; border-radius: 4px; }
.omaquill-card { padding: 0; min-height: 150px; }
.omaquill-card .omaquill-stripe { min-height: 5px; border-radius: 12px 12px 0 0; }
.omaquill-card .title { font-weight: bold; }
.omaquill-card.selected { outline: 2px solid @accent_bg_color; outline-offset: -2px; }
.omaquill-status { padding: 4px 12px; font-size: 0.9em; }
.omaquill-format { padding: 4px 8px; }
.omaquill-outline-head { font-weight: bold; font-size: 0.9em; }
"#,
    );
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION - 1,
        );
    }
}

fn icon_button(icon: &str, tip: &str, action: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tip)
        .action_name(action)
        .build()
}

fn toggle(icon: &str, tip: &str) -> gtk::ToggleButton {
    gtk::ToggleButton::builder()
        .icon_name(icon)
        .tooltip_text(tip)
        .css_classes(["flat"])
        .build()
}

fn primary_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let file = gio::Menu::new();
    file.append(Some("New Project…"), Some("win.new-project"));
    file.append(Some("Open Project…"), Some("win.open"));
    file.append(Some("Compile…"), Some("win.compile"));
    menu.append_section(None, &file);
    let project = gio::Menu::new();
    project.append(Some("Back Up Now"), Some("win.backup"));
    project.append(Some("Show Backups"), Some("win.show-backups"));
    project.append(Some("Empty Trash…"), Some("win.empty-trash"));
    menu.append_section(None, &project);
    let app = gio::Menu::new();
    app.append(Some("Preferences"), Some("win.preferences"));
    app.append(Some("Keyboard Shortcuts"), Some("win.show-help-overlay"));
    app.append(Some("About omaquill"), Some("win.about"));
    menu.append_section(None, &app);
    menu
}

impl Win {
    pub fn new(app: &adw::Application) -> Rc<Win> {
        let settings = Settings::load();
        apply_look(&settings, None);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .default_width(1400)
            .default_height(900)
            .title("omaquill")
            .icon_name(APP_ID)
            .build();

        // Header.
        let title = adw::WindowTitle::new("omaquill", "");
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        let binder_toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text("Binder (Ctrl+Alt+B)")
            .active(true)
            .build();
        header.pack_start(&binder_toggle);
        let new_menu = gio::Menu::new();
        new_menu.append(Some("New Text"), Some("win.new-text"));
        new_menu.append(Some("New Folder"), Some("win.new-folder"));
        let new_button = adw::SplitButton::builder()
            .icon_name("document-new-symbolic")
            .tooltip_text("New Text (Ctrl+N)")
            .action_name("win.new-text")
            .menu_model(&new_menu)
            .build();
        header.pack_start(&new_button);

        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&primary_menu())
            .primary(true)
            .tooltip_text("Main Menu")
            .build();
        header.pack_end(&menu_button);
        let inspector_toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-right-symbolic")
            .tooltip_text("Inspector (Ctrl+Alt+I)")
            .active(true)
            .build();
        header.pack_end(&inspector_toggle);
        header.pack_end(&icon_button(
            "view-fullscreen-symbolic",
            "Composition Mode (F11)",
            "win.compose",
        ));
        let view_text = toggle("text-x-generic-symbolic", "Text (Ctrl+1)");
        let view_cork = toggle("view-grid-symbolic", "Corkboard (Ctrl+2)");
        let view_outline = toggle("view-list-symbolic", "Outliner (Ctrl+3)");
        view_cork.set_group(Some(&view_text));
        view_outline.set_group(Some(&view_text));
        let view_buttons = gtk::Box::builder().css_classes(["linked"]).build();
        view_buttons.append(&view_text);
        view_buttons.append(&view_cork);
        view_buttons.append(&view_outline);
        header.pack_end(&view_buttons);

        // Editor page with its format bar.
        let editor = Editor::new(true);
        let bold = toggle("format-text-bold-symbolic", "Bold (Ctrl+B)");
        let italic = toggle("format-text-italic-symbolic", "Italic (Ctrl+I)");
        let underline = toggle("format-text-underline-symbolic", "Underline (Ctrl+U)");
        let align = gtk::DropDown::from_strings(&["Left", "Center", "Right", "Justified"]);
        align.set_tooltip_text(Some("Alignment"));
        align.add_css_class("flat");
        let format_bar = gtk::Box::builder()
            .spacing(4)
            .css_classes(["toolbar", "omaquill-format"])
            .halign(gtk::Align::Center)
            .build();
        format_bar.append(&bold);
        format_bar.append(&italic);
        format_bar.append(&underline);
        format_bar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        format_bar.append(&align);
        let banner = adw::Banner::new("");
        let editor_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        editor_page.append(&format_bar);
        editor_page.append(&banner);
        editor_page.append(&editor.root);

        let board = board::Board::new();
        let notes = Editor::new(false);
        let inspector = inspector::Inspector::new(&notes);
        let binder = binder::Binder::new();

        // Welcome page.
        let welcome_recent = gtk::ListBox::builder()
            .css_classes(["boxed-list"])
            .selection_mode(gtk::SelectionMode::None)
            .build();
        let welcome = {
            let open = gtk::Button::builder()
                .label("Open Project…")
                .action_name("win.open")
                .css_classes(["pill", "suggested-action"])
                .build();
            let new = gtk::Button::builder()
                .label("New Project…")
                .action_name("win.new-project")
                .css_classes(["pill"])
                .build();
            let buttons = gtk::Box::builder()
                .spacing(12)
                .halign(gtk::Align::Center)
                .build();
            buttons.append(&open);
            buttons.append(&new);
            let col = gtk::Box::new(gtk::Orientation::Vertical, 24);
            col.append(&buttons);
            col.append(&welcome_recent);
            let clamp = adw::Clamp::builder().maximum_size(520).child(&col).build();
            adw::StatusPage::builder()
                .icon_name("document-edit-symbolic")
                .title("omaquill")
                .description("Open a Scrivener project (.scriv) or start a new one.")
                .child(&clamp)
                .build()
        };

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(120)
            .build();
        stack.add_named(&welcome, Some("welcome"));
        stack.add_named(&editor_page, Some("editor"));
        stack.add_named(&board.cork_page, Some("corkboard"));
        stack.add_named(&board.outline_page, Some("outliner"));
        stack.add_named(&board.media_page, Some("media"));
        stack.add_named(&board.file_page, Some("file"));
        stack.add_named(
            &adw::StatusPage::builder()
                .icon_name("go-previous-symbolic")
                .title("Pick something in the binder")
                .build(),
            Some("empty"),
        );

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stack));

        let inner = adw::OverlaySplitView::builder()
            .content(&toasts)
            .sidebar(&inspector.root)
            .sidebar_position(gtk::PackType::End)
            .min_sidebar_width(280.0)
            .max_sidebar_width(360.0)
            .build();
        let outer = adw::OverlaySplitView::builder()
            .content(&inner)
            .sidebar(&binder.root)
            .min_sidebar_width(240.0)
            .max_sidebar_width(320.0)
            .build();
        binder_toggle
            .bind_property("active", &outer, "show-sidebar")
            .bidirectional()
            .build();
        inspector_toggle
            .bind_property("active", &inner, "show-sidebar")
            .bidirectional()
            .build();

        // Status bar.
        let doc_words = gtk::Label::builder().xalign(0.0).hexpand(true).build();
        let save_state = gtk::Label::builder().css_classes(["dim-label"]).build();
        let total_words = gtk::Label::builder().xalign(1.0).hexpand(true).build();
        let status = gtk::Box::builder()
            .css_classes(["omaquill-status"])
            .spacing(12)
            .build();
        status.append(&doc_words);
        status.append(&save_state);
        status.append(&total_words);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header);
        toolbar_view.set_content(Some(&outer));
        toolbar_view.add_bottom_bar(&status);
        window.set_content(Some(&toolbar_view));

        // Narrow windows: sidebars overlay the text instead of squeezing it.
        let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            1000.0,
            adw::LengthUnit::Sp,
        ));
        bp.add_setter(&inner, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(bp);
        let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            700.0,
            adw::LengthUnit::Sp,
        ));
        bp.add_setter(&inner, "collapsed", Some(&true.to_value()));
        bp.add_setter(&outer, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(bp);

        let label_css = gtk::CssProvider::new();
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &label_css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }

        let win = Rc::new(Win {
            window,
            settings: RefCell::new(settings),
            project: RefCell::new(None),
            current: RefCell::new(None),
            editor_uuid: RefCell::new(None),
            dirty_text: Cell::new(false),
            dirty_notes: Cell::new(false),
            dirty_synopsis: Cell::new(false),
            save_source: RefCell::new(None),
            folder_view: Cell::new(FolderView::Corkboard),
            composing: Cell::new(false),
            counts: RefCell::new(HashMap::new()),
            search_text: RefCell::new(HashMap::new()),
            today: RefCell::new(None),
            label_css,
            toolbar_view,
            title,
            outer,
            inner,
            stack,
            toasts,
            banner,
            editor,
            notes,
            format_bar,
            bold,
            italic,
            underline,
            align,
            updating_format: Cell::new(false),
            view_buttons,
            view_text,
            view_cork,
            view_outline,
            doc_words,
            total_words,
            save_state,
            welcome_recent,
            binder,
            board,
            inspector,
        });
        win.wire();
        // First focus: GTK gives it to the first header button when the
        // window first becomes active (and a typed space then clicks "New
        // Text"). Take it back for the text at that moment, once.
        let weak = Rc::downgrade(&win);
        let done = Cell::new(false);
        win.window.connect_is_active_notify(move |window| {
            if window.is_active()
                && !done.replace(true)
                && let Some(w) = weak.upgrade()
            {
                w.focus_content_soon();
            }
        });
        win.show_welcome();
        WINDOWS.with(|w| w.borrow_mut().push(win.clone()));
        win
    }

    fn wire(self: &Rc<Self>) {
        self.add_actions();
        self.wire_binder();
        self.wire_board();
        self.wire_inspector();

        let weak = Rc::downgrade(self);
        self.editor.connect_changed(move || {
            if let Some(w) = weak.upgrade() {
                w.dirty_text.set(true);
                w.update_doc_words();
                w.schedule_save();
            }
        });
        let weak = Rc::downgrade(self);
        self.notes.connect_changed(move || {
            if let Some(w) = weak.upgrade() {
                w.dirty_notes.set(true);
                w.schedule_save();
            }
        });
        let weak = Rc::downgrade(self);
        self.editor.connect_formatting(move |f| {
            if let Some(w) = weak.upgrade() {
                w.updating_format.set(true);
                w.bold.set_active(f.bold);
                w.italic.set_active(f.italic);
                w.underline.set_active(f.underline);
                w.align.set_selected(match f.align {
                    rtf::Align::Center => 1,
                    rtf::Align::Right => 2,
                    rtf::Align::Justify => 3,
                    _ => 0,
                });
                w.updating_format.set(false);
            }
        });
        for (button, tag) in [
            (&self.bold, "c:b"),
            (&self.italic, "c:i"),
            (&self.underline, "c:u"),
        ] {
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(w) = weak.upgrade()
                    && !w.updating_format.get()
                {
                    w.editor.toggle(tag);
                    w.editor.view.grab_focus();
                }
            });
        }
        let weak = Rc::downgrade(self);
        self.align.connect_selected_notify(move |d| {
            if let Some(w) = weak.upgrade()
                && !w.updating_format.get()
            {
                w.editor.set_align(match d.selected() {
                    1 => rtf::Align::Center,
                    2 => rtf::Align::Right,
                    3 => rtf::Align::Justify,
                    _ => rtf::Align::Left,
                });
                w.editor.view.grab_focus();
            }
        });
        for (button, view) in [
            (&self.view_text, FolderView::Text),
            (&self.view_cork, FolderView::Corkboard),
            (&self.view_outline, FolderView::Outliner),
        ] {
            let weak = Rc::downgrade(self);
            button.connect_toggled(move |b| {
                if let Some(w) = weak.upgrade()
                    && b.is_active()
                    && w.folder_view.get() != view
                {
                    w.set_folder_view(view);
                }
            });
        }

        // The text view swallows some Ctrl shortcuts before the app's
        // accelerators see them; catch these on the way down instead.
        let early = gtk::ShortcutController::new();
        early.set_propagation_phase(gtk::PropagationPhase::Capture);
        for (trigger, action) in [
            ("<Ctrl>comma", "win.preferences"),
            ("<Ctrl>question", "win.show-help-overlay"),
            ("<Ctrl><Shift>e", "win.compile"),
        ] {
            early.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(gtk::NamedAction::new(action)),
            ));
        }
        self.window.add_controller(early);

        // Escape leaves composition mode.
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            if let Some(w) = weak.upgrade()
                && key == gtk::gdk::Key::Escape
                && w.composing.get()
            {
                w.set_composing(false);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.window.add_controller(keys);

        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |_| {
            let Some(w) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if let Err(e) = w.close_project() {
                return w.confirm_close_after_error(&e);
            }
            WINDOWS.with(|ws| ws.borrow_mut().retain(|x| !Rc::ptr_eq(x, &w)));
            glib::Propagation::Proceed
        });
    }

    fn add_actions(self: &Rc<Self>) {
        let simple = |name: &str, f: fn(&Rc<Win>)| {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(w) = weak.upgrade() {
                    f(&w);
                }
            });
            self.window.add_action(&action);
        };
        simple("save", |w| {
            w.save_now();
        });
        simple("open", |w| w.choose_project());
        simple("new-project", |w| w.create_project());
        simple("new-text", |w| w.add_item(Kind::Text));
        simple("new-folder", |w| w.add_item(Kind::Folder));
        simple("rename", |w| w.rename_selected());
        simple("trash", |w| w.trash_selected());
        simple("empty-trash", |w| w.empty_trash());
        simple("move-up", |w| w.nudge(binder::Nudge::Up));
        simple("move-down", |w| w.nudge(binder::Nudge::Down));
        simple("indent", |w| w.nudge(binder::Nudge::In));
        simple("outdent", |w| w.nudge(binder::Nudge::Out));
        simple("bold", |w| w.format("c:b"));
        simple("italic", |w| w.format("c:i"));
        simple("underline", |w| w.format("c:u"));
        simple("find", |w| {
            if w.stack.visible_child_name().as_deref() == Some("editor") {
                w.editor.show_find();
            }
        });
        simple("project-search", |w| {
            w.outer.set_show_sidebar(true);
            w.binder.search.grab_focus();
        });
        simple("view-text", |w| w.set_folder_view(FolderView::Text));
        simple("view-corkboard", |w| {
            w.set_folder_view(FolderView::Corkboard)
        });
        simple("view-outliner", |w| w.set_folder_view(FolderView::Outliner));
        simple("compose", |w| w.set_composing(!w.composing.get()));
        simple("toggle-binder", |w| {
            w.outer.set_show_sidebar(!w.outer.shows_sidebar())
        });
        simple("toggle-inspector", |w| {
            w.inner.set_show_sidebar(!w.inner.shows_sidebar())
        });
        simple("zoom-in", |w| w.zoom(1.1));
        simple("zoom-out", |w| w.zoom(1.0 / 1.1));
        simple("zoom-reset", |w| w.zoom(0.0));
        simple("compile", |w| w.show_compile());
        simple("preferences", |w| w.show_preferences());
        simple("about", |w| w.show_about());
        simple("backup", |w| w.backup_now());
        simple("show-backups", |_| {
            let dir = omaquill::state::backup_dir();
            let _ = std::fs::create_dir_all(&dir);
            let _ = gio::AppInfo::launch_default_for_uri(
                &gio::File::for_path(&dir).uri(),
                gio::AppLaunchContext::NONE,
            );
        });
        simple("focus-binder", |w| {
            w.outer.set_show_sidebar(true);
            w.binder.list.grab_focus();
        });
        simple("focus-editor", |w| {
            w.editor.view.grab_focus();
        });
        simple("show-help-overlay", |w| {
            dialogs::shortcuts().present(Some(&w.window))
        });
    }

    fn format(&self, tag: &str) {
        if self.stack.visible_child_name().as_deref() == Some("editor") {
            if self.notes.view.has_focus() {
                self.notes.toggle(tag);
            } else {
                self.editor.toggle(tag);
            }
        } else if self.notes.view.has_focus() {
            self.notes.toggle(tag);
        }
    }

    /// For README and listing images: renders this window to a PNG at 2x
    /// once it has settled, then quits (`OMAQUILL_SCREENSHOT=out.png
    /// omaquill PROJECT`). GTK draws it itself, so the window needn't be on
    /// screen.
    fn screenshot_then_quit(self: &Rc<Self>, path: PathBuf) {
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(2500), move || {
            let Some(w) = weak.upgrade() else { return };
            let (width, height) = (w.window.width() as f64, w.window.height() as f64);
            let paintable = gtk::WidgetPaintable::new(Some(&w.window));
            let snapshot = gtk::Snapshot::new();
            snapshot.scale(2.0, 2.0);
            paintable.snapshot(&snapshot, width, height);
            let saved = snapshot.to_node().and_then(|node| {
                let renderer = w.window.native()?.renderer()?;
                let texture = renderer.render_texture(
                    &node,
                    Some(&gtk::graphene::Rect::new(
                        0.0,
                        0.0,
                        width as f32 * 2.0,
                        height as f32 * 2.0,
                    )),
                );
                texture.save_to_png(&path).ok()
            });
            if saved.is_none() {
                eprintln!("omaquill: couldn't render the screenshot");
            }
            if let Some(app) = w.window.application() {
                app.quit();
            }
        });
    }

    /// Puts the keyboard in the editor (or the binder) once the window is
    /// up; GTK gives first focus to the header buttons otherwise.
    fn focus_content_soon(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(w) = weak.upgrade() {
                match w.stack.visible_child_name().as_deref() {
                    Some("editor") => w.editor.view.grab_focus(),
                    Some("welcome") => false,
                    _ => w.binder.list.grab_focus(),
                };
            }
        });
    }

    pub fn toast(&self, text: &str) {
        // Toast titles are markup; titles can hold '&' and '<'.
        self.toasts
            .add_toast(adw::Toast::new(&glib::markup_escape_text(text)));
    }

    fn with_project<T>(&self, f: impl FnOnce(&mut Project) -> T) -> Option<T> {
        self.project.borrow_mut().as_mut().map(f)
    }

    fn item(&self, uuid: &str) -> Option<Item> {
        self.project.borrow().as_ref()?.item(uuid)
    }

    // ------------------------------------------------------------ open

    fn show_welcome(&self) {
        while let Some(row) = self.welcome_recent.first_child() {
            self.welcome_recent.remove(&row);
        }
        let recent = omaquill::state::recent();
        for path in &recent {
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&name))
                .subtitle(glib::markup_escape_text(&path.display().to_string()))
                .activatable(true)
                .build();
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let path = path.clone();
            let window = self.window.downgrade();
            row.connect_activated(move |_| {
                if let Some(window) = window.upgrade() {
                    let win =
                        WINDOWS.with(|ws| ws.borrow().iter().find(|w| w.window == window).cloned());
                    if let Some(win) = win {
                        // Already open in another window: go there.
                        let app = win.window.application().and_downcast::<adw::Application>();
                        if let Some(other) = app.and_then(|a| find_window(&a, &project_dir(&path)))
                            && other != win.window
                        {
                            other.present();
                            return;
                        }
                        win.open_project(&path);
                    }
                }
            });
            self.welcome_recent.append(&row);
        }
        self.welcome_recent.set_visible(!recent.is_empty());
        self.stack.set_visible_child_name("welcome");
        self.title.set_title("omaquill");
        self.title.set_subtitle("");
        self.outer.set_show_sidebar(false);
        self.inner.set_show_sidebar(false);
        self.view_buttons.set_visible(false);
    }

    pub fn open_project(self: &Rc<Self>, path: &Path) {
        let path = project_dir(path);
        // Open the new one first: if it isn't a project, the one on screen
        // stays open and editable.
        let project = match Project::open(&path) {
            Ok(p) => p,
            Err(e) => {
                self.toast(&format!("Couldn't open {}: {e}", path.display()));
                return;
            }
        };
        let open = self.project.borrow().is_some();
        if open && let Err(e) = self.close_project() {
            self.toast(&format!("Couldn't save: {e}"));
            return;
        }
        // A backup before anything is touched, off the main thread.
        let keep = self.settings.borrow().backups;
        if keep > 0 {
            let path = project.path.clone();
            std::thread::spawn(move || {
                if let Err(e) =
                    omaquill::project::backup(&path, &omaquill::state::backup_dir(), keep)
                {
                    eprintln!("omaquill: backup of {} failed: {e}", path.display());
                }
            });
        }
        omaquill::state::add_recent(&project.path);
        self.title.set_title(&project.name());
        self.window
            .set_title(Some(&format!("{} — omaquill", project.name())));
        self.index_project(&project);
        self.set_label_css(&project);
        self.inspector
            .set_tags(&project.labels(), &project.statuses());
        self.editor.set_default_style(project.default_style());
        let view = ProjectView::load(&project.path);
        let manuscript = self.manuscript_words(&project);
        let today = Today::begin(
            &project.name(),
            &project.path,
            manuscript as i64,
            self.settings.borrow().daily_target,
        );
        today.save();
        *self.today.borrow_mut() = Some(today);
        *self.project.borrow_mut() = Some(project);

        self.outer.set_show_sidebar(true);
        self.inner.set_show_sidebar(true);
        self.refresh_binder(Some(&view.expanded));
        let open = view.open.filter(|u| self.item(u).is_some()).or_else(|| {
            // First time: the first document in the manuscript.
            let p = self.project.borrow();
            let draft = p.as_ref()?.root_folder(Kind::Draft)?;
            fn first_text(items: &[Item]) -> Option<String> {
                for i in items {
                    if i.kind == Kind::Text {
                        return Some(i.uuid.clone());
                    }
                    if let Some(u) = first_text(&i.children) {
                        return Some(u);
                    }
                }
                None
            }
            first_text(&draft.children).or(Some(draft.uuid))
        });
        if let Some(uuid) = open {
            self.select(&uuid);
        } else {
            self.stack.set_visible_child_name("empty");
        }
        self.update_totals();
        if self.window.is_mapped() {
            self.focus_content_soon();
        }
    }

    /// Reads every text once: word counts and search text.
    fn index_project(&self, project: &Project) {
        let mut counts = HashMap::new();
        let mut search = HashMap::new();
        fn walk(
            p: &Project,
            items: &[Item],
            counts: &mut HashMap<String, usize>,
            search: &mut HashMap<String, String>,
        ) {
            for i in items {
                let mut text = format!("{}\n{}\n", i.title, p.synopsis(&i.uuid));
                if i.kind.has_text() {
                    let body = p.text(&i.uuid).map(|t| t.plain_text()).unwrap_or_default();
                    counts.insert(i.uuid.clone(), rtf::word_count(&body));
                    text.push_str(&body);
                }
                if let Ok(n) = p.notes(&i.uuid) {
                    text.push('\n');
                    text.push_str(&n.plain_text());
                }
                search.insert(i.uuid.clone(), text.to_lowercase());
                walk(p, &i.children, counts, search);
            }
        }
        walk(project, &project.items(), &mut counts, &mut search);
        *self.counts.borrow_mut() = counts;
        *self.search_text.borrow_mut() = search;
    }

    /// Words in everything the manuscript compiles.
    fn manuscript_words(&self, project: &Project) -> usize {
        let counts = self.counts.borrow();
        fn sum(items: &[Item], counts: &HashMap<String, usize>) -> usize {
            items
                .iter()
                .filter(|i| i.include_in_compile)
                .map(|i| counts.get(&i.uuid).copied().unwrap_or(0) + sum(&i.children, counts))
                .sum()
        }
        project
            .root_folder(Kind::Draft)
            .map(|d| counts.get(&d.uuid).copied().unwrap_or(0) + sum(&d.children, &counts))
            .unwrap_or(0)
    }

    fn set_label_css(&self, project: &Project) {
        let mut css = String::new();
        for l in project.labels() {
            if let Some((r, g, b)) = l.color {
                css.push_str(&format!(
                    ".omaquill-label-{} {{ background-color: rgb({}, {}, {}); }}\n",
                    l.id.max(0),
                    (r * 255.0) as u8,
                    (g * 255.0) as u8,
                    (b * 255.0) as u8
                ));
            }
        }
        self.label_css.load_from_string(&css);
    }

    fn choose_project(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("Open a Scrivener Project")
            .modal(true)
            .build();
        let weak = Rc::downgrade(self);
        // A .scriv project is a folder.
        dialog.select_folder(Some(&self.window), gio::Cancellable::NONE, move |res| {
            let (Some(w), Ok(folder)) = (weak.upgrade(), res) else {
                return;
            };
            let Some(path) = folder.path() else { return };
            let path = project_dir(&path);
            if let Some(existing) =
                find_window(&w.window.application().and_downcast().unwrap(), &path)
            {
                existing.present();
                return;
            }
            w.open_project(&path);
        });
    }

    fn create_project(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("New Project")
            .initial_name("Untitled.scriv")
            .modal(true)
            .build();
        let weak = Rc::downgrade(self);
        dialog.save(Some(&self.window), gio::Cancellable::NONE, move |res| {
            let (Some(w), Ok(file)) = (weak.upgrade(), res) else {
                return;
            };
            let Some(mut path) = file.path() else { return };
            if path.extension().is_none_or(|e| e != "scriv") {
                path.set_extension("scriv");
            }
            match Project::create(&path) {
                Ok(_) => w.open_project(&path),
                Err(e) => w.toast(&format!("Couldn't create the project: {e}")),
            }
        });
    }

    // ------------------------------------------------------------ items

    /// Shows a binder item in the main area.
    fn show_item(self: &Rc<Self>, uuid: &str) {
        if self.current.borrow().as_deref() == Some(uuid) {
            return;
        }
        let Some(item) = self.item(uuid) else { return };
        self.commit_title();
        if !self.save_now() {
            // Unsaved edits stay with their own document: don't switch.
            self.reselect_current();
            return;
        }
        // Everything is saved; the editor and inspector are free.
        *self.editor_uuid.borrow_mut() = None;
        self.dirty_text.set(false);
        *self.current.borrow_mut() = Some(uuid.to_string());
        self.title.set_subtitle(&item.title);
        self.show_inspector(&item);
        self.banner.set_revealed(false);
        let is_folder = item.kind.is_folder();
        self.view_buttons.set_visible(is_folder);
        match item.kind {
            Kind::Text => self.show_text(&item),
            k if k.is_folder() => {
                let view = self.folder_view.get();
                self.sync_view_buttons(view);
                match view {
                    FolderView::Text if k.has_text() => self.show_text(&item),
                    FolderView::Outliner => self.show_outliner(&item),
                    _ => self.show_corkboard(&item),
                }
            }
            Kind::Image => {
                let path = self
                    .project
                    .borrow()
                    .as_ref()
                    .and_then(|p| p.media_path(uuid));
                if self.board.show_image(path.as_deref()) {
                    self.stack.set_visible_child_name("media");
                } else {
                    // Missing, unreadable, or too large to decode safely.
                    self.board.show_file(&item.title, path.as_deref());
                    if path.is_some() {
                        self.board.say_image_too_large();
                    }
                    self.stack.set_visible_child_name("file");
                }
            }
            _ => {
                let path = self
                    .project
                    .borrow()
                    .as_ref()
                    .and_then(|p| p.media_path(uuid));
                self.board.show_file(&item.title, path.as_deref());
                self.stack.set_visible_child_name("file");
            }
        }
        self.update_doc_words();
        if let Some(p) = self.project.borrow().as_ref() {
            let mut view = ProjectView::load(&p.path);
            view.open = Some(uuid.to_string());
            view.expanded = self.binder.expanded();
            view.save(&p.path);
        }
    }

    fn show_text(self: &Rc<Self>, item: &Item) {
        let text = self
            .project
            .borrow()
            .as_ref()
            .map(|p| p.text(&item.uuid))
            .transpose();
        match text {
            Ok(Some(text)) => {
                if !text.lossy.is_empty() {
                    self.banner.set_title(&format!(
                        "This document has {} that omaquill shows as plain text. Editing it keeps the words but not that layout.",
                        join_words(&text.lossy)
                    ));
                    self.banner.set_revealed(true);
                }
                self.editor.load(&text);
                self.dirty_text.set(false);
                *self.editor_uuid.borrow_mut() = Some(item.uuid.clone());
                self.format_bar.set_visible(true);
                self.stack.set_visible_child_name("editor");
                // Browsing the binder with the keyboard keeps focus there.
                // set_focus works before the window is shown; grab_focus doesn't.
                let in_binder = gtk::prelude::RootExt::focus(&self.window)
                    .is_some_and(|f| f.is_ancestor(&self.binder.root));
                if !in_binder {
                    GtkWindowExt::set_focus(&self.window, Some(&self.editor.view));
                }
            }
            Ok(None) => {}
            Err(e) => {
                // Never leave another document's text editable here.
                *self.editor_uuid.borrow_mut() = None;
                self.stack.set_visible_child_name("empty");
                self.toast(&format!("Couldn't read {}: {e}", item.title));
            }
        }
    }

    fn set_folder_view(self: &Rc<Self>, view: FolderView) {
        self.folder_view.set(view);
        self.sync_view_buttons(view);
        let Some(uuid) = self.current.borrow().clone() else {
            return;
        };
        let Some(item) = self.item(&uuid) else { return };
        if !item.kind.is_folder() {
            return;
        }
        if !self.save_now() {
            return;
        }
        *self.editor_uuid.borrow_mut() = None;
        self.dirty_text.set(false);
        match view {
            FolderView::Text if item.kind.has_text() => self.show_text(&item),
            FolderView::Outliner => self.show_outliner(&item),
            _ => self.show_corkboard(&item),
        }
        self.update_doc_words();
    }

    fn sync_view_buttons(&self, view: FolderView) {
        match view {
            FolderView::Text => self.view_text.set_active(true),
            FolderView::Corkboard => self.view_cork.set_active(true),
            FolderView::Outliner => self.view_outline.set_active(true),
        }
    }

    // ------------------------------------------------------------ saving

    fn schedule_save(self: &Rc<Self>) {
        self.save_state.set_text("Edited");
        if let Some(id) = self.save_source.borrow_mut().take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
            if let Some(w) = weak.upgrade() {
                w.save_source.borrow_mut().take();
                w.save_now();
            }
        });
        *self.save_source.borrow_mut() = Some(id);
    }

    /// Writes whatever's pending. Errors show as a toast and stay pending.
    /// Returns false if something couldn't be saved.
    pub fn save_now(&self) -> bool {
        if let Some(id) = self.save_source.borrow_mut().take() {
            id.remove();
        }
        match self.try_save() {
            Ok(()) => true,
            Err(e) => {
                self.save_state.set_text("Not saved");
                self.toast(&format!("Couldn't save: {e}"));
                false
            }
        }
    }

    /// An edited but not applied title (typed without Enter) is kept
    /// rather than dropped when leaving the item.
    fn commit_title(self: &Rc<Self>) {
        let Some((uuid, text)) = self.inspector.pending_title() else {
            return;
        };
        if self.item(&uuid).is_some_and(|i| i.title != text) {
            self.set_title(&uuid, &text);
        }
    }

    /// Puts the binder selection back on the item on show.
    fn reselect_current(&self) {
        let cur = self.current.borrow().clone();
        self.binder.select_quietly(cur.as_deref());
    }

    fn try_save(&self) -> omaquill::project::Result<()> {
        let editor_uuid = self.editor_uuid.borrow().clone();
        let inspector_uuid = self.inspector.uuid();
        let mut guard = self.project.borrow_mut();
        let Some(p) = guard.as_mut() else {
            return Ok(());
        };
        let mut touched = Vec::new();
        if self.dirty_text.get() {
            if let Some(uuid) = &editor_uuid {
                let text = self.editor.text();
                p.set_text(uuid, &text)?;
                self.counts
                    .borrow_mut()
                    .insert(uuid.clone(), rtf::word_count(&text.plain_text()));
                touched.push(uuid.clone());
            }
            self.dirty_text.set(false);
        }
        if let Some(uuid) = &inspector_uuid {
            if self.dirty_notes.get() {
                p.set_notes(uuid, &self.notes.text())?;
                touched.push(uuid.clone());
            }
            if self.dirty_synopsis.get() {
                p.set_synopsis(uuid, &self.inspector.synopsis_text())?;
                touched.push(uuid.clone());
            }
        }
        self.dirty_notes.set(false);
        self.dirty_synopsis.set(false);
        let was_dirty = p.has_unsaved_changes();
        p.save()?;
        for uuid in &touched {
            if let Some(item) = p.item(uuid) {
                let mut text = format!("{}\n{}\n", item.title, p.synopsis(uuid));
                if item.kind.has_text() {
                    text.push_str(&p.text(uuid).map(|t| t.plain_text()).unwrap_or_default());
                }
                if let Ok(n) = p.notes(uuid) {
                    text.push('\n');
                    text.push_str(&n.plain_text());
                }
                self.search_text
                    .borrow_mut()
                    .insert(uuid.clone(), text.to_lowercase());
            }
        }
        if was_dirty {
            self.save_state.set_text("Saved");
        }
        drop(guard);
        if touched.iter().any(|u| Some(u) == editor_uuid.as_ref()) {
            self.update_totals();
        }
        Ok(())
    }

    /// Saves and forgets the open project. Leaves the window on the
    /// welcome page.
    fn close_project(&self) -> omaquill::project::Result<()> {
        if let Some(id) = self.save_source.borrow_mut().take() {
            id.remove();
        }
        self.try_save()?;
        if let Some(p) = self.project.borrow().as_ref() {
            let view = ProjectView {
                open: self.current.borrow().clone(),
                expanded: self.binder.expanded(),
            };
            view.save(&p.path);
        }
        if let Some(t) = self.today.borrow_mut().as_mut() {
            t.open = false;
            t.save();
        }
        *self.project.borrow_mut() = None;
        *self.current.borrow_mut() = None;
        *self.editor_uuid.borrow_mut() = None;
        self.inspector.forget();
        Ok(())
    }

    fn confirm_close_after_error(
        self: &Rc<Self>,
        e: &omaquill::project::Error,
    ) -> glib::Propagation {
        let dialog = adw::AlertDialog::new(
            Some("Changes Not Saved"),
            Some(&format!(
                "omaquill couldn't save: {e}\n\nClose anyway and lose the latest changes?"
            )),
        );
        dialog.add_responses(&[("cancel", "Keep Open"), ("close", "Close Anyway")]);
        dialog.set_response_appearance("close", adw::ResponseAppearance::Destructive);
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, r| {
            if r == "close"
                && let Some(w) = weak.upgrade()
            {
                *w.project.borrow_mut() = None;
                WINDOWS.with(|ws| ws.borrow_mut().retain(|x| !Rc::ptr_eq(x, &w)));
                w.window.destroy();
            }
        });
        dialog.present(Some(&self.window));
        glib::Propagation::Stop
    }

    fn backup_now(&self) {
        let Some(path) = self.project.borrow().as_ref().map(|p| p.path.clone()) else {
            return;
        };
        self.save_now();
        let keep = self.settings.borrow().backups.max(1);
        match omaquill::project::backup(&path, &omaquill::state::backup_dir(), keep) {
            Ok(zip) => self.toast(&format!(
                "Backed up to {}",
                zip.file_name().unwrap_or_default().to_string_lossy()
            )),
            Err(e) => self.toast(&format!("Backup failed: {e}")),
        }
    }

    // ------------------------------------------------------------ counts

    fn update_doc_words(&self) {
        let page = self.stack.visible_child_name();
        let text = if page.as_deref() == Some("editor") {
            let n = self.editor.word_count();
            let chars = self.editor.plain_text().chars().count();
            format!("{} words · {} characters", group(n), group(chars))
        } else {
            String::new()
        };
        self.doc_words.set_text(&text);
    }

    fn update_totals(&self) {
        let total = {
            let p = self.project.borrow();
            match p.as_ref() {
                Some(p) => self.manuscript_words(p),
                None => return,
            }
        };
        let target = self.settings.borrow().daily_target;
        let mut today = self.today.borrow_mut();
        let mut text = format!("Manuscript {} words", group(total));
        if let Some(t) = today.as_mut() {
            if t.date != omaquill::state::today() {
                // Past midnight: a new day starts from here.
                t.date = omaquill::state::today();
                t.baseline = total as i64;
            }
            t.manuscript = total as i64;
            t.target = target;
            t.save();
            let words = t.words();
            let sign = if words >= 0 { "+" } else { "−" };
            text.push_str(&format!(
                " · Today {sign}{}",
                group(words.unsigned_abs() as usize)
            ));
            if target > 0 {
                text.push_str(&format!(" of {}", group(target as usize)));
            }
        }
        self.total_words.set_text(&text);
    }

    // ------------------------------------------------------------ look

    fn zoom(&self, factor: f64) {
        {
            let mut s = self.settings.borrow_mut();
            s.zoom = if factor == 0.0 {
                Settings::default().zoom
            } else {
                (s.zoom * factor).clamp(0.5, 3.0)
            };
            let _ = s.save();
        }
        self.apply_look();
        self.toast(&format!("Zoom {:.0}%", self.settings.borrow().zoom * 100.0));
    }

    pub fn apply_look(&self) {
        apply_look(&self.settings.borrow(), Some(&self.window));
        rich::restyle(&self.editor.buffer);
        rich::restyle(&self.notes.buffer);
    }

    fn set_composing(&self, on: bool) {
        self.composing.set(on);
        self.toolbar_view.set_reveal_top_bars(!on);
        self.toolbar_view.set_reveal_bottom_bars(!on);
        self.outer.set_show_sidebar(!on);
        self.inner.set_show_sidebar(!on);
        self.format_bar.set_visible(!on);
        if on {
            self.window.add_css_class("omaquill-compose");
            self.window.fullscreen();
            self.editor.clamp.set_maximum_size(700);
            self.editor.view.grab_focus();
        } else {
            self.window.remove_css_class("omaquill-compose");
            self.window.unfullscreen();
            self.editor.clamp.set_maximum_size(760);
        }
    }
}

fn apply_look(settings: &Settings, widget: Option<&adw::ApplicationWindow>) {
    let installed: Vec<String> = widget
        .map(|w| w.pango_context())
        .or_else(|| Some(gtk::Label::new(None).pango_context()))
        .map(|ctx| {
            ctx.list_families()
                .iter()
                .map(|f| f.name().to_string())
                .collect()
        })
        .unwrap_or_default();
    rich::set_look(rich::Look {
        zoom: settings.zoom,
        font_override: settings.editor_font.clone(),
        substitutes: omaquill::fonts::find_substitutes(&installed),
    });
}

/// 12345 → "12,345".
pub fn group(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn join_words(list: &[&str]) -> String {
    match list {
        [] => String::new(),
        [one] => one.to_string(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// Every GTK check, from one test: gtk-rs allows GTK only on the thread
/// that started it, and each #[test] gets its own thread. Needs a display;
/// skipped without one (CI).
#[cfg(test)]
#[test]
fn gtk_checks() {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none() {
        eprintln!("no display; GTK checks skipped");
        return;
    }
    adw::init().unwrap();
    rich::tests::buffer_round_trip();
    editor::tests::typing_carries_formatting();
    editor::tests::empty_paragraphs_keep_their_style_and_lines_align_alone();
}

use super::Win;
use super::editor::Editor;
use adw::prelude::*;
use omaquill::replace::{self, ApplyOutcome, Case, Field, Mode, Options, Place, Word};
use std::cell::RefCell;
use std::rc::Rc;

pub fn present(win: &Rc<Win>) {
    if win.project.borrow().is_none() {
        win.toast("Open a project first");
        return;
    }
    let find = adw::EntryRow::builder().title("Find").build();
    let replace_with = adw::EntryRow::builder().title("Replace with").build();
    if let Some(editor) = focused(win)
        && let Some(selected) = selected_line(&editor)
    {
        find.set_text(&selected);
    }
    let match_case = adw::SwitchRow::builder().title("Match case").build();
    let whole_word = adw::SwitchRow::builder().title("Whole word").build();
    let regex = adw::SwitchRow::builder()
        .title("Regular expression")
        .subtitle("Groups use $1 and ${name}. $$ is a dollar sign.")
        .build();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .child(&list)
        .vexpand(true)
        .min_content_height(220)
        .build();
    let status = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .css_classes(["dim-label"])
        .build();
    let listed: Rc<RefCell<Option<Listed>>> = Rc::new(RefCell::new(None));

    let replace_one = gtk::Button::with_label("Replace");
    let replace_all = gtk::Button::with_label("Replace All");
    let preview = gtk::Button::with_label("Preview in Project");
    let replace_checked = gtk::Button::builder()
        .label("Replace Checked")
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();

    let opts = {
        let match_case = match_case.clone();
        let whole_word = whole_word.clone();
        let regex = regex.clone();
        move || Options {
            case: if match_case.is_active() {
                Case::Sensitive
            } else {
                Case::Insensitive
            },
            word: if whole_word.is_active() {
                Word::Whole
            } else {
                Word::Any
            },
            mode: if regex.is_active() {
                Mode::Regex
            } else {
                Mode::Literal
            },
        }
    };

    {
        let win = win.clone();
        let find = find.clone();
        let replace_with = replace_with.clone();
        let opts = opts.clone();
        replace_one.connect_clicked(move |_| {
            let Some(editor) = focused(&win) else {
                win.toast("Click in the document first");
                return;
            };
            let query = match parsed(&find, &replace_with, opts()) {
                Ok(query) => query,
                Err(message) => {
                    win.toast(&message);
                    return;
                }
            };
            match replace::step(&editor.text(), &query, place_of(&editor.buffer)) {
                replace::Step::Replace(plan) => {
                    if let Some(caret) = editor.apply_edits(&plan) {
                        let again = editor.text();
                        if let Some(hit) = replace::find_next(&again, &query, Place::Caret(caret)) {
                            select_range(&editor, hit.range());
                        }
                    }
                }
                replace::Step::Found(hit) => select_range(&editor, hit.range()),
                replace::Step::None => {
                    find.add_css_class("error");
                    win.toast("No matches");
                }
            }
        });
    }
    {
        let win = win.clone();
        let find = find.clone();
        let replace_with = replace_with.clone();
        let opts = opts.clone();
        replace_all.connect_clicked(move |_| {
            let Some(editor) = focused(&win) else {
                win.toast("Click in the document first");
                return;
            };
            let query = match parsed(&find, &replace_with, opts()) {
                Ok(query) => query,
                Err(message) => {
                    win.toast(&message);
                    return;
                }
            };
            let plan = replace::plan_all(&editor.text(), &query);
            let count = plan.edits().len();
            if editor.apply_edits(&plan).is_none() {
                find.add_css_class("error");
                win.toast("No matches");
                return;
            }
            find.remove_css_class("error");
            win.toast(&format!("Replaced {count}"));
        });
    }
    {
        let win = win.clone();
        let find = find.clone();
        let replace_with = replace_with.clone();
        let opts = opts.clone();
        let list = list.clone();
        let status = status.clone();
        let listed = listed.clone();
        let replace_checked = replace_checked.clone();
        preview.connect_clicked(move |_| {
            let query = match parsed(&find, &replace_with, opts()) {
                Ok(query) => query,
                Err(message) => {
                    win.toast(&message);
                    return;
                }
            };
            if !win.save_now() {
                return;
            }
            let preview = {
                let guard = win.project.borrow();
                let Some(project) = guard.as_ref() else {
                    return;
                };
                match replace::preview(project, &query) {
                    Ok(preview) => preview,
                    Err(e) => {
                        win.toast(&e.to_string());
                        return;
                    }
                }
            };
            fill_list(&list, &preview, &listed);
            let count: usize = preview.groups().iter().map(|g| g.rows().len()).sum();
            status.set_text(&format!("{count} matches"));
            replace_checked.set_sensitive(count > 0);
            if count == 0 {
                find.add_css_class("error");
                win.toast("No matches");
            } else {
                find.remove_css_class("error");
            }
        });
    }
    {
        let win = win.clone();
        let listed = listed.clone();
        replace_checked.connect_clicked(move |_| {
            let prepared = {
                let guard = listed.borrow();
                let Some(listed) = guard.as_ref() else {
                    return;
                };
                let ids: Vec<_> = listed
                    .checks
                    .iter()
                    .filter(|(_, check)| check.is_active())
                    .map(|(id, _)| *id)
                    .collect();
                (listed.preview.clone(), ids)
            };
            if !win.save_now() {
                return;
            }
            win.join_backup_reader();
            let keep = win.settings.borrow().backups;
            let outcome = {
                let mut guard = win.project.borrow_mut();
                let Some(project) = guard.as_mut() else {
                    return;
                };
                replace::apply(
                    project,
                    &omaquill::state::backup_dir(),
                    keep,
                    &prepared.0,
                    &prepared.1,
                )
            };
            match outcome {
                Ok(ApplyOutcome::NothingToDo) => win.toast("Nothing to replace"),
                Ok(ApplyOutcome::SkippedOnly(skipped)) => {
                    win.toast(&replace::skipped_lines(&skipped));
                }
                Ok(ApplyOutcome::Done(report)) => {
                    let n = report.written.len();
                    reload(&win, &report);
                    win.toast(&format!("Updated {n} files"));
                }
                Err(e) => win.toast(&e.to_string()),
            }
        });
    }

    let buttons = gtk::Box::builder()
        .spacing(8)
        .halign(gtk::Align::End)
        .margin_top(12)
        .margin_bottom(12)
        .margin_end(12)
        .build();
    buttons.append(&replace_one);
    buttons.append(&replace_all);
    buttons.append(&preview);
    buttons.append(&replace_checked);

    let fields = adw::PreferencesGroup::new();
    fields.add(&find);
    fields.add(&replace_with);
    fields.add(&match_case);
    fields.add(&whole_word);
    fields.add(&regex);
    let page = adw::PreferencesPage::new();
    page.add(&fields);
    let lower = gtk::Box::new(gtk::Orientation::Vertical, 8);
    lower.append(&status);
    lower.append(&scroll);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&page);
    content.append(&lower);
    content.append(&buttons);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&content));
    let dialog = adw::Dialog::builder()
        .title("Find and Replace")
        .content_width(560)
        .content_height(640)
        .child(&view)
        .build();
    dialog.present(Some(&win.window));
}

struct Listed {
    preview: replace::Preview,
    checks: Vec<(replace::MatchId, gtk::CheckButton)>,
}

fn parsed(
    find: &adw::EntryRow,
    replace_with: &adw::EntryRow,
    opts: Options,
) -> Result<replace::Query, String> {
    match replace::query(&find.text(), &replace_with.text(), opts) {
        Ok(query) => {
            find.remove_css_class("error");
            Ok(query)
        }
        Err(err) => {
            find.add_css_class("error");
            Err(err.to_string())
        }
    }
}

fn focused(win: &Win) -> Option<Rc<Editor>> {
    if win.notes.view.has_focus() && win.inspector.uuid().is_some() {
        return Some(win.notes.clone());
    }
    if win.stack.visible_child_name().as_deref() == Some("editor")
        && win.editor_uuid.borrow().is_some()
    {
        return Some(win.editor.clone());
    }
    None
}

fn selected_line(editor: &Editor) -> Option<String> {
    let (start, end) = editor.buffer.selection_bounds()?;
    let text = editor.buffer.text(&start, &end, false).to_string();
    if text.is_empty() || text.contains('\n') {
        None
    } else {
        Some(text)
    }
}

fn place_of(buffer: &gtk::TextBuffer) -> Place {
    let insert = usize::try_from(buffer.cursor_position()).unwrap_or(0);
    let Some((start, end)) = buffer.selection_bounds() else {
        return Place::Caret(replace::Cursor::new(insert));
    };
    let s = usize::try_from(start.offset()).unwrap_or(0);
    let e = usize::try_from(end.offset()).unwrap_or(0);
    replace::CharRange::try_bounds(s, e)
        .map(Place::Selection)
        .unwrap_or(Place::Caret(replace::Cursor::new(insert)))
}

fn select_range(editor: &Editor, range: replace::CharRange) {
    let start = editor
        .buffer
        .iter_at_offset(i32::try_from(range.start()).unwrap_or(0));
    let end = editor
        .buffer
        .iter_at_offset(i32::try_from(range.end()).unwrap_or(0));
    editor.buffer.select_range(&start, &end);
    let mut end = end;
    editor.view.scroll_to_iter(&mut end, 0.1, false, 0.0, 0.0);
    editor.view.grab_focus();
}

fn fill_list(
    list: &gtk::ListBox,
    preview: &replace::Preview,
    listed: &Rc<RefCell<Option<Listed>>>,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let mut checks = Vec::new();
    for group in preview.groups() {
        let heading = match group.id().field() {
            Field::Body => group.title().to_string(),
            Field::Notes => format!("{} notes", group.title()),
        };
        let mut detail = group.path_label().to_string();
        if group.in_trash() {
            detail = if detail.is_empty() {
                "Trash".to_string()
            } else {
                format!("{detail}. Trash")
            };
        }
        if !group.lossy().is_empty() {
            let names = group.lossy().join(", ");
            detail = if detail.is_empty() {
                format!("Rewriting drops {names}")
            } else {
                format!("{detail}. Rewriting drops {names}")
            };
        }
        let title = gtk::Label::builder()
            .label(heading)
            .halign(gtk::Align::Start)
            .css_classes(["heading"])
            .margin_top(12)
            .build();
        list.append(&title);
        if !detail.is_empty() {
            list.append(
                &gtk::Label::builder()
                    .label(detail)
                    .halign(gtk::Align::Start)
                    .css_classes(["dim-label"])
                    .build(),
            );
        }
        for row in group.rows() {
            let snippet = row.snippet();
            let check = gtk::CheckButton::builder()
                .label(format!("{} to {}", snippet.matched, snippet.replacement))
                .active(true)
                .build();
            let context = format!("{}{}{}", snippet.before, snippet.matched, snippet.after);
            let box_ = gtk::Box::new(gtk::Orientation::Vertical, 2);
            box_.append(&check);
            box_.append(
                &gtk::Label::builder()
                    .label(context)
                    .halign(gtk::Align::Start)
                    .css_classes(["dim-label"])
                    .build(),
            );
            list.append(&box_);
            checks.push((row.id(), check));
        }
    }
    *listed.borrow_mut() = Some(Listed {
        preview: preview.clone(),
        checks,
    });
}

fn reload(win: &Win, report: &replace::Report) {
    {
        let guard = win.project.borrow();
        let Some(project) = guard.as_ref() else {
            return;
        };
        for id in &report.written {
            if id.field() == Field::Body
                && win.editor_uuid.borrow().as_deref() == Some(id.uuid())
                && let Ok(text) = project.text(id.uuid())
            {
                win.editor.load(&text);
                win.dirty_text.set(false);
            }
            if id.field() == Field::Notes
                && win.inspector.uuid().as_deref() == Some(id.uuid())
                && let Ok(notes) = project.notes(id.uuid())
            {
                win.notes.load(&notes);
                win.dirty_notes.set(false);
            }
        }
        win.index_project(project);
    }
    win.update_totals();
    win.update_doc_words();
}

//! The rich-text editor: a `GtkTextView` over a buffer of [`super::rich`]
//! tags, with typing that carries formatting forward the way a word
//! processor does (GTK leaves new text unformatted by default).

use super::rich;
use gtk::glib;
use gtk::prelude::*;
use omaquill::rtf::{Align, RichText};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Default, Clone, Copy, PartialEq)]
pub struct Formatting {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub align: Align,
}

type FormattingCallback = Box<dyn Fn(Formatting)>;

pub struct Editor {
    pub root: gtk::Box,
    pub view: gtk::TextView,
    pub buffer: gtk::TextBuffer,
    pub clamp: adw::Clamp,
    pub search_bar: gtk::SearchBar,
    search_entry: gtk::SearchEntry,
    /// Formatting switched on or off with nothing selected, for the next
    /// characters typed: (tag name, on).
    pending: RefCell<Vec<(String, bool)>>,
    /// Tags for text typed where there's nothing to take formatting from
    /// (a new, empty document).
    default_tags: RefCell<Vec<String>>,
    loading: Cell<bool>,
    inserting: Cell<bool>,
    /// Features the file on disk would lose if this buffer were written back.
    lossy: RefCell<Vec<&'static str>>,
    changed: RefCell<Vec<Box<dyn Fn()>>>,
    formatting: RefCell<Vec<FormattingCallback>>,
    cursor: RefCell<Vec<Box<dyn Fn()>>>,
    /// Set when a paragraph has too many style runs to tag on the UI thread.
    /// The buffer then holds the plain words, and [`Self::text`] returns this
    /// document so a save cannot strip the formatting.
    painted_source: RefCell<Option<RichText>>,
    /// Typing is allowed when the buffer is fully tagged. A held document
    /// stays read-only.
    edit_when_ready: Cell<bool>,
    /// Scrolls a held document that is longer than [`HELD_WINDOW`].
    held_bar: gtk::Scrollbar,
    /// Character index of the first buffer character in that document.
    held_origin: Cell<usize>,
    /// The bar is changing the buffer, so its own signal does not run again.
    held_adjusting: Cell<bool>,
    copy_whole_model: Cell<bool>,
}

/// Characters of a held document placed in the buffer at once.
/// A 1,024-character window took 101.0 ms for the first layout on the
/// x86_64 xvfb runner and 135.9 ms for the next page on aarch64, in
/// `time_styled_paragraph_open` at `692be6b`. Two hundred characters wrap
/// in the column and are the text that layout shapes. A held document
/// longer than this shows this many of its own characters.
const HELD_WINDOW: usize = 200;

/// The first draw of a mapped text view.
///
/// The frame clock is not a ready source until the frame time, so
/// `pending()` is false in the gap after map. That draw then lands on
/// the next turn. On the x86_64 xvfb runner it was the 101.9 ms turn in
/// `time_styled_paragraph_open` at `82d82ea`. A held line on a view that
/// had already been painted took 0.5 ms in the same run. A timeout that
/// waited for the tick again was the 126.7 ms aarch64 turn at `f63b914`.
/// The map idle runs until one tick and leaves no later source.
fn queue_first_paint(view: &gtk::TextView) {
    let armed = Rc::new(Cell::new(false));
    let watched = view.clone();
    view.connect_map(move |_| {
        if armed.replace(true) {
            return;
        }
        let weak = watched.downgrade();
        glib::idle_add_local_once(move || paint_when_mapped(weak));
    });
}

thread_local! {
    static PUMPING: Cell<bool> = const { Cell::new(false) };
    static WAITING: RefCell<Vec<glib::WeakRef<gtk::TextView>>> = const { RefCell::new(Vec::new()) };
}

fn paint_when_mapped(weak: glib::WeakRef<gtk::TextView>) {
    if PUMPING.with(Cell::get) {
        // The page and the notes view map together. This idle is running
        // inside the other view's frame wait. Remember it for that wait
        // to paint, instead of starting a second one.
        WAITING.with(|queue| queue.borrow_mut().push(weak));
        return;
    }
    let Some(view) = weak.upgrade() else {
        return;
    };
    // A closed window's idle runs after unmap. There is no frame to wait for.
    if !view.is_mapped() {
        return;
    }
    pump_first_paint(&view);
    loop {
        let next = WAITING.with(|queue| queue.borrow_mut().pop());
        let Some(next) = next else {
            break;
        };
        let Some(view) = next.upgrade() else {
            continue;
        };
        if view.is_mapped() {
            pump_first_paint(&view);
        }
    }
}

fn pump_first_paint(view: &gtk::TextView) {
    struct End;
    impl Drop for End {
        fn drop(&mut self) {
            PUMPING.with(|flag| flag.set(false));
        }
    }
    PUMPING.with(|flag| flag.set(true));
    let _end = End;
    let done = Rc::new(Cell::new(false));
    let timed_out = Rc::new(Cell::new(false));
    let flag = done.clone();
    view.add_tick_callback(move |_view, _clock| {
        flag.set(true);
        glib::ControlFlow::Break
    });
    if let Some(clock) = view.frame_clock() {
        clock.request_phase(gtk::gdk::FrameClockPhase::PAINT);
    }
    view.queue_draw();
    let cap = glib::timeout_add_local(std::time::Duration::from_millis(200), {
        let flag = done.clone();
        let timed_out = timed_out.clone();
        move || {
            timed_out.set(true);
            flag.set(true);
            glib::ControlFlow::Break
        }
    });
    let context = glib::MainContext::default();
    for _ in 0..30 {
        if done.get() {
            break;
        }
        if !context.iteration(true) {
            break;
        }
    }
    if !timed_out.get() {
        cap.remove();
    }
}

mod doc_view {
    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub struct DocView;

    #[glib::object_subclass]
    impl ObjectSubclass for DocView {
        const NAME: &'static str = "OmaquillDocView";
        type Type = super::DocView;
        type ParentType = gtk::TextView;

        fn class_init(klass: &mut Self::Class) {
            // The context menu runs this action. The key binding emits
            // `select-all` instead, so the menu never reached that handler.
            klass.install_action("selection.select-all", None, |view, _, _| {
                view.emit_select_all(true);
            });
        }
    }

    impl ObjectImpl for DocView {}
    impl WidgetImpl for DocView {}
    impl TextViewImpl for DocView {}
}

glib::wrapper! {
    pub struct DocView(ObjectSubclass<doc_view::DocView>)
        @extends gtk::TextView, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

impl DocView {
    fn with_buffer(buffer: &gtk::TextBuffer) -> Self {
        glib::Object::builder().property("buffer", buffer).build()
    }
}

impl Editor {
    /// `page` is the main editor: centered column, find bar. Otherwise a
    /// compact editor for notes.
    pub fn new(page: bool) -> Rc<Editor> {
        let buffer = gtk::TextBuffer::new(None);
        let view: gtk::TextView = DocView::with_buffer(&buffer).upcast();
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.set_accepts_tab(true);
        view.add_css_class("omaquill-text");
        queue_first_paint(&view);
        let clamp = adw::Clamp::builder()
            .maximum_size(760)
            .tightening_threshold(600)
            .build();
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Find in document")
            .build();
        let search_bar = gtk::SearchBar::builder()
            .child(&search_entry)
            .show_close_button(true)
            .build();
        search_bar.connect_entry(&search_entry);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        if page {
            view.add_css_class("omaquill-page");
            view.set_top_margin(48);
            view.set_bottom_margin(240);
            view.set_left_margin(24);
            view.set_right_margin(24);
            clamp.set_child(Some(&view));
            scrolled.set_child(Some(&clamp));
            root.append(&search_bar);
        } else {
            view.add_css_class("omaquill-notes");
            view.set_top_margin(8);
            view.set_bottom_margin(8);
            view.set_left_margin(8);
            view.set_right_margin(8);
            scrolled.set_child(Some(&view));
        }
        let held_bar = gtk::Scrollbar::new(
            gtk::Orientation::Vertical,
            Some(&gtk::Adjustment::new(0.0, 0.0, 0.0, 1.0, 1.0, 0.0)),
        );
        held_bar.set_visible(false);
        held_bar.set_halign(gtk::Align::End);
        held_bar.set_valign(gtk::Align::Fill);
        // The bar overlays the column. A sibling would shrink the text view
        // and lay the line out a second time when the bar appears.
        let overlay = gtk::Overlay::new();
        overlay.set_vexpand(true);
        scrolled.set_hexpand(true);
        overlay.set_child(Some(&scrolled));
        overlay.add_overlay(&held_bar);
        root.append(&overlay);

        let ed = Rc::new(Editor {
            root,
            view,
            buffer,
            clamp,
            search_bar,
            search_entry,
            pending: RefCell::new(Vec::new()),
            default_tags: RefCell::new(Vec::new()),
            loading: Cell::new(false),
            inserting: Cell::new(false),
            lossy: RefCell::new(Vec::new()),
            changed: RefCell::new(Vec::new()),
            formatting: RefCell::new(Vec::new()),
            cursor: RefCell::new(Vec::new()),
            painted_source: RefCell::new(None),
            edit_when_ready: Cell::new(true),
            held_bar,
            held_origin: Cell::new(0),
            held_adjusting: Cell::new(false),
            copy_whole_model: Cell::new(false),
        });
        ed.wire();
        ed
    }

    fn wire(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.buffer
            .connect_insert_text(move |buffer, _iter, _text| {
                if weak.upgrade().is_some_and(|ed| ed.blocks_signal()) {
                    buffer.stop_signal_emission_by_name("insert-text");
                }
            });
        let weak = Rc::downgrade(self);
        self.buffer
            .connect_delete_range(move |buffer, _start, _end| {
                if weak.upgrade().is_some_and(|ed| ed.blocks_signal()) {
                    buffer.stop_signal_emission_by_name("delete-range");
                }
            });
        let weak = Rc::downgrade(self);
        self.buffer.connect_closure(
            "insert-text",
            true,
            glib::closure_local!(move |buffer: gtk::TextBuffer,
                                       end: gtk::TextIter,
                                       text: &str,
                                       _len: i32| {
                if let Some(ed) = weak.upgrade() {
                    ed.after_insert(&buffer, &end, text);
                }
            }),
        );
        // One value per property: applying `c:size=40` (a paste brings its
        // own tags) takes any other `c:size=` off that stretch first.
        // Loading applies a finished document, so that scan stays off until
        // the buffer is back on screen.
        let weak = Rc::downgrade(self);
        self.buffer.connect_apply_tag(move |buffer, tag, s, e| {
            if weak.upgrade().is_some_and(|ed| ed.loading.get()) {
                return;
            }
            let Some(name) = tag.name() else { return };
            let Some((key, _)) = name.split_once('=') else {
                return;
            };
            let prefix = format!("{key}=");
            let mut others = Vec::new();
            buffer.tag_table().foreach(|t| {
                if let Some(n) = t.name()
                    && n.starts_with(&prefix)
                    && n != name
                {
                    others.push(t.clone());
                }
            });
            for t in others {
                buffer.remove_tag(&t, s, e);
            }
        });
        let weak = Rc::downgrade(self);
        self.buffer.connect_changed(move |_| {
            if let Some(ed) = weak.upgrade()
                && ed.allows_edits()
            {
                for f in ed.changed.borrow().iter() {
                    f();
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.buffer.connect_mark_set(move |_buffer, _, mark| {
            let Some(ed) = weak.upgrade() else {
                return;
            };
            let name = mark.name();
            let caret = name.as_deref() == Some("insert");
            // `select_range` of the bounds already selected still emits mark-set.
            if caret || name.as_deref() == Some("selection_bound") {
                ed.copy_whole_model.set(false);
            }
            if caret && !ed.inserting.get() {
                ed.pending.borrow_mut().clear();
                ed.report();
                if !ed.loading.get() {
                    for f in ed.cursor.borrow().iter() {
                        f();
                    }
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.view.connect_copy_clipboard(move |view| {
            let Some(ed) = weak.upgrade() else {
                return;
            };
            if !ed.copy_whole_model.get() {
                return;
            }
            let Some(text) = ed
                .painted_source
                .borrow()
                .as_ref()
                .map(|source| source.plain_text())
            else {
                return;
            };
            let Ok(shown) = usize::try_from(ed.buffer.char_count()) else {
                return;
            };
            if text.chars().count() <= shown {
                return;
            }
            view.clipboard().set_text(&text);
            view.stop_signal_emission_by_name("copy-clipboard");
        });
        let weak = Rc::downgrade(self);
        self.view.connect_cut_clipboard(move |view| {
            let Some(ed) = weak.upgrade() else {
                return;
            };
            if !ed.model_outruns_buffer() {
                return;
            }
            // The default handler copies the window and then deletes it.
            // A held document is read-only, so cut copies the same text
            // Copy would and leaves the buffer alone.
            let text = if ed.copy_whole_model.get() {
                ed.painted_source
                    .borrow()
                    .as_ref()
                    .map(|source| source.plain_text())
            } else {
                ed.buffer
                    .selection_bounds()
                    .map(|(s, e)| ed.buffer.text(&s, &e, false).to_string())
            };
            let Some(text) = text else {
                return;
            };
            view.clipboard().set_text(&text);
            view.stop_signal_emission_by_name("cut-clipboard");
        });
        // After the default handler, so the flag survives the mark changes
        // that Select All makes while it selects the window.
        let weak = Rc::downgrade(self);
        self.view.connect_closure(
            "select-all",
            true,
            glib::closure_local!(move |_view: gtk::TextView, select: bool| {
                let Some(ed) = weak.upgrade() else {
                    return;
                };
                ed.copy_whole_model.set(select && ed.model_outruns_buffer());
            }),
        );
        let weak = Rc::downgrade(self);
        self.held_bar
            .adjustment()
            .connect_value_changed(move |adj| {
                let Some(ed) = weak.upgrade() else {
                    return;
                };
                if ed.held_adjusting.get() || ed.painted_source.borrow().is_none() {
                    return;
                }
                let origin = bar_origin(adj);
                if origin == ed.held_origin.get() {
                    return;
                }
                ed.held_origin.set(origin);
                ed.fill_held_window();
            });
        let weak = Rc::downgrade(self);
        self.search_entry.connect_activate(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.find(true);
            }
        });
        let weak = Rc::downgrade(self);
        self.search_entry.connect_search_changed(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.find_from_start();
            }
        });
        let weak = Rc::downgrade(self);
        self.search_entry.connect_next_match(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.find(true);
            }
        });
        let weak = Rc::downgrade(self);
        self.search_entry.connect_previous_match(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.find(false);
            }
        });
        let weak = Rc::downgrade(self);
        self.search_entry.connect_stop_search(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.search_bar.set_search_mode(false);
                ed.view.grab_focus();
            }
        });
    }

    pub fn connect_changed(&self, f: impl Fn() + 'static) {
        self.changed.borrow_mut().push(Box::new(f));
    }

    /// Called with the formatting at the cursor whenever it moves.
    pub fn connect_formatting(&self, f: impl Fn(Formatting) + 'static) {
        self.formatting.borrow_mut().push(Box::new(f));
    }

    /// Called when the cursor moves and the buffer is not loading.
    pub fn connect_cursor(&self, f: impl Fn() + 'static) {
        self.cursor.borrow_mut().push(Box::new(f));
    }

    /// `text` moves in. A held document keeps that value for a later save.
    pub fn load(&self, text: RichText) {
        // Drop the previous hold without changing wrap mode or the bar.
        // Restoring word-char wrap, hiding the bar, and writing the buffer
        // again queues a layout of a line this load is about to replace.
        // On aarch64 xvfb that layout was the 132.4 ms turn in
        // `time_styled_paragraph_open` at `1e2cc98`.
        self.painted_source.borrow_mut().take();
        self.held_origin.set(0);
        *self.lossy.borrow_mut() = text.lossy.clone();
        self.loading.set(true);
        let edit = self.edit_when_ready.get();
        // A paragraph under the threshold is tagged before this returns.
        // A heavier one is shown as plain words. Tagging it, then laying
        // that line out, blocked the UI thread for longer than a slice
        // can hide.
        if !rich::needs_plain_view(&text) {
            self.set_wrap(gtk::WrapMode::WordChar);
            self.held_bar.set_visible(false);
            let mut paint = rich::Paint::begin(&self.buffer, &text);
            while !paint.pump(&self.buffer, f64::MAX) {}
            paint.finish(&self.buffer);
            self.loading.set(false);
            self.view.set_editable(edit);
            self.view.set_cursor_visible(edit);
            self.pending.borrow_mut().clear();
            self.report();
            return;
        }
        // Character wrap is GTK's own break. The buffer holds the file's
        // characters and no others. A held document longer than
        // [`HELD_WINDOW`] is shown one window at a time. Wrapping 10,000
        // unspaced characters in the first frame exceeded 100 ms under xvfb.
        self.set_wrap(gtk::WrapMode::Char);
        let plain = text.plain_text();
        let total = plain.chars().count();
        self.painted_source.borrow_mut().replace(text);
        self.held_origin.set(0);
        if total > HELD_WINDOW {
            self.fill_held_window();
            self.place_held_bar(total, 0);
        } else {
            self.held_bar.set_visible(false);
            self.buffer.begin_irreversible_action();
            self.buffer.set_text(&plain);
            self.buffer.end_irreversible_action();
            self.buffer.place_cursor(&self.buffer.start_iter());
        }
        self.buffer.set_modified(false);
        self.view.set_editable(false);
        self.view.set_cursor_visible(true);
        self.loading.set(false);
        self.pending.borrow_mut().clear();
        self.report();
    }

    /// Typing, tags, paste, drop, and undo may change the buffer.
    pub fn allows_edits(&self) -> bool {
        !self.is_too_formatted() && !self.loading.get() && self.view.is_editable()
    }

    /// `insert-text` and `delete-range` while a document is loading are the
    /// load itself. After that, a held document and a read-only view refuse
    /// every insert and delete, including paste, drop, and undo.
    fn blocks_signal(&self) -> bool {
        !self.loading.get() && !self.allows_edits()
    }

    /// Whether typing is allowed. A document held for too much formatting
    /// stays read-only even when `on` is true.
    pub fn set_editing(&self, on: bool) {
        self.edit_when_ready.set(on);
        if !self.is_too_formatted() {
            self.view.set_editable(on);
            self.view.set_cursor_visible(on);
        }
    }

    /// The buffer is the plain words of a document with too much inline
    /// formatting to tag safely. [`Self::text`] still returns that document.
    pub fn is_too_formatted(&self) -> bool {
        self.painted_source.borrow().is_some()
    }

    /// Held because one unbreakable run is long. The formatting banner would
    /// name the wrong reason, including when the tag count is also over the cap.
    pub fn held_for_length(&self) -> bool {
        self.painted_source
            .borrow()
            .as_ref()
            .is_some_and(|text| rich::breakless_chars(text) > rich::SYNC_BREAKLESS_CHARS)
    }

    /// Drops a plain-only hold and clears the buffer.
    ///
    /// The held window can be the same characters as the document that was
    /// open before it. Leaving those characters, or their tags, would keep
    /// that document's bold, links, and paragraph formatting. The next
    /// [`Self::load`] writes the document being opened.
    pub fn release_plain(&self) {
        let held = self.painted_source.borrow().is_some();
        self.painted_source.borrow_mut().take();
        self.held_origin.set(0);
        self.held_bar.set_visible(false);
        if held {
            // `blocks_signal` stops delete and insert while a hold is
            // read-only. The clear is the release, so it runs as a load.
            self.loading.set(true);
            self.buffer.begin_irreversible_action();
            self.buffer.set_text("");
            self.buffer.end_irreversible_action();
            self.buffer.place_cursor(&self.buffer.start_iter());
            self.loading.set(false);
            self.set_wrap(gtk::WrapMode::WordChar);
            let edit = self.edit_when_ready.get();
            self.view.set_editable(edit);
            self.view.set_cursor_visible(edit);
        } else {
            self.loading.set(false);
        }
    }

    /// The held file is longer than the characters in the buffer.
    fn model_outruns_buffer(&self) -> bool {
        let Some(text) = self
            .painted_source
            .borrow()
            .as_ref()
            .map(|source| source.plain_text())
        else {
            return false;
        };
        let Ok(shown) = usize::try_from(self.buffer.char_count()) else {
            return false;
        };
        text.chars().count() > shown
    }

    fn set_wrap(&self, mode: gtk::WrapMode) {
        if self.view.wrap_mode() != mode {
            self.view.set_wrap_mode(mode);
        }
    }

    /// The buffer shows one window of a long held document. The characters
    /// are the file's, starting at [`Self::held_origin`].
    ///
    /// The slice is written even when those characters are already in the
    /// buffer. The previous document may have been the same letters with
    /// bold, a link, paragraph formatting, and a selection. Find starts at
    /// that selection. Replacing the text drops the tags and the selection,
    /// and the cursor is placed at the start.
    fn fill_held_window(&self) {
        let Some(plain) = self
            .painted_source
            .borrow()
            .as_ref()
            .map(|text| text.plain_text())
        else {
            return;
        };
        let origin = self.held_origin.get();
        let slice: String = plain.chars().skip(origin).take(HELD_WINDOW).collect();
        let was_loading = self.loading.get();
        self.loading.set(true);
        self.buffer.begin_irreversible_action();
        self.buffer.set_text(&slice);
        self.buffer.end_irreversible_action();
        self.buffer.place_cursor(&self.buffer.start_iter());
        self.loading.set(was_loading);
    }

    fn place_held_bar(&self, total: usize, origin: usize) {
        let adj = self.held_bar.adjustment();
        self.held_adjusting.set(true);
        let span = total.max(1);
        let page = HELD_WINDOW.min(span);
        adj.set_lower(0.0);
        adj.set_upper(span as f64);
        adj.set_page_size(page as f64);
        adj.set_page_increment(page as f64);
        adj.set_step_increment(1.0);
        let max = (span - page) as f64;
        adj.set_value((origin as f64).min(max));
        self.held_adjusting.set(false);
        let show = total > HELD_WINDOW;
        if self.held_bar.is_visible() != show {
            self.held_bar.set_visible(show);
        }
    }

    /// The formatting for text typed into an empty document.
    pub fn set_default_style(
        &self,
        style: Option<(omaquill::rtf::ParaStyle, omaquill::rtf::CharStyle)>,
    ) {
        *self.default_tags.borrow_mut() = match style {
            Some((p, c)) => {
                let mut c = c;
                c.link = None;
                rich::char_tags(&c)
                    .into_iter()
                    .chain(rich::para_tags(&p))
                    .collect()
            }
            None => Vec::new(),
        };
    }

    pub fn text(&self) -> RichText {
        if let Some(source) = self.painted_source.borrow().as_ref() {
            return source.clone();
        }
        rich::save(&self.buffer)
    }

    /// Content the loaded file had that this buffer does not.
    pub fn lossy(&self) -> Vec<&'static str> {
        self.lossy.borrow().clone()
    }

    pub fn plain_text(&self) -> String {
        if let Some(source) = self.painted_source.borrow().as_ref() {
            return source.plain_text();
        }
        rich::logical_plain(&self.buffer)
    }

    pub fn word_count(&self) -> usize {
        omaquill::rtf::word_count(&self.plain_text())
    }

    /// Replace All for this buffer. A held document is reported the way an
    /// image is: the plan is refused, and nothing is written.
    pub fn plan_replace(
        &self,
        query: &omaquill::replace::Query,
    ) -> Result<omaquill::replace::EditPlan, omaquill::replace::ReplaceBlock> {
        if self.is_too_formatted() {
            return Err(omaquill::replace::ReplaceBlock::Lossy(vec![
                "inline formatting",
            ]));
        }
        omaquill::replace::plan_document(&self.lossy(), &self.text(), query)
    }

    pub fn apply_edits(
        &self,
        plan: &omaquill::replace::EditPlan,
    ) -> Option<omaquill::replace::Cursor> {
        if plan.is_empty() || !self.allows_edits() {
            return None;
        }
        self.pending.borrow_mut().clear();
        self.buffer.begin_user_action();
        self.inserting.set(true);
        let edits = plan.edits();
        let mut caret = 0usize;
        for (i, edit) in edits.iter().enumerate().rev() {
            let start = edit.range().start();
            let end = edit.range().end();
            let mut from = self.buffer.iter_at_offset(text_offset(start));
            let mut to = self.buffer.iter_at_offset(text_offset(end));
            self.buffer.delete(&mut from, &mut to);
            let tags = tags_for(&self.buffer, edit.char_style());
            let mut at = self.buffer.iter_at_offset(text_offset(start));
            let origin = at.offset();
            for (part_ix, part) in edit.replacement().split('\n').enumerate() {
                let refs: Vec<&gtk::TextTag> = tags.iter().collect();
                if part_ix > 0 {
                    self.buffer.insert_with_tags(&mut at, "\n", &refs);
                }
                if !part.is_empty() {
                    let refs: Vec<&gtk::TextTag> = tags.iter().collect();
                    self.buffer.insert_with_tags(&mut at, part, &refs);
                }
            }
            paint_new_lines(&self.buffer, origin, at.offset(), edit.para_style());
            let written = edit.replacement().chars().count();
            if i + 1 == edits.len() {
                caret = start + written;
            } else {
                let delta = text_len(written) - text_len(end - start);
                caret = usize::try_from(text_len(caret) + delta)
                    .expect("the caret stays inside the document");
            }
        }
        self.inserting.set(false);
        self.buffer.end_user_action();
        let mut caret_iter = self.buffer.iter_at_offset(text_offset(caret));
        self.buffer.place_cursor(&caret_iter);
        self.view
            .scroll_to_iter(&mut caret_iter, 0.1, false, 0.0, 0.0);
        Some(omaquill::replace::Cursor::new(caret))
    }

    /// Tags new text should carry, from the text it was typed next to.
    fn source_tags(&self, start: &gtk::TextIter, end: &gtk::TextIter) -> Vec<String> {
        // Character formatting: at the start of a paragraph, from the text
        // that follows (if the paragraph has any); otherwise from the
        // character before.
        let mut before = *start;
        let from_before = if start.starts_line() && !end.ends_line() {
            false
        } else {
            before.backward_char()
        };
        let mut chars = if from_before {
            rich::tag_names(&before)
        } else if !end.is_end() {
            rich::tag_names(end)
        } else {
            Vec::new()
        };
        if chars.is_empty() && !from_before {
            let mut b = *start;
            if b.backward_char() {
                chars = rich::tag_names(&b);
            }
        }
        // Paragraph formatting: always the paragraph's own, which the
        // character after the insert (its text or its newline) carries.
        // Only at the very end of the text is there nothing after.
        let para = if !end.is_end() {
            rich::tag_names(end)
        } else {
            chars.clone()
        };
        chars
            .into_iter()
            .filter(|n| n.starts_with("c:"))
            .chain(para.into_iter().filter(|n| n.starts_with("p:")))
            .collect()
    }

    fn after_insert(&self, buffer: &gtk::TextBuffer, end: &gtk::TextIter, text: &str) {
        if self.loading.get() || self.inserting.get() {
            return;
        }
        self.inserting.set(true);
        let mut start = *end;
        start.backward_chars(text.chars().count() as i32);
        let mut names = self.source_tags(&start, end);
        // Nothing to inherit from (every formatted character carries at
        // least a size): an empty document takes the project's default.
        if names.is_empty() {
            names = self.default_tags.borrow().clone();
        }
        for (name, on) in self.pending.borrow().iter() {
            names.retain(|n| n != name);
            if *on {
                names.push(name.clone());
            }
        }
        // Drop what the insert inherited and apply the chosen set.
        let current = rich::tag_names(&start);
        for n in current
            .iter()
            .filter(|n| n.starts_with("c:") || n.starts_with("p:"))
        {
            buffer.remove_tag(&rich::tag(buffer, n), &start, end);
        }
        for n in &names {
            buffer.apply_tag(&rich::tag(buffer, n), &start, end);
        }
        self.inserting.set(false);
    }

    /// Formatting at the cursor (or the start of the selection).
    pub fn formatting(&self) -> Formatting {
        let buffer = &self.buffer;
        let mut at = buffer.iter_at_mark(&buffer.get_insert());
        let para = rich::tag_names(&{
            let mut p = at;
            p.set_line_offset(0);
            p
        });
        if !buffer.has_selection() {
            at.backward_char();
        } else if let Some((s, _)) = buffer.selection_bounds() {
            at = s;
        }
        let mut names = rich::tag_names(&at);
        for (name, on) in self.pending.borrow().iter() {
            names.retain(|n| n != name);
            if *on {
                names.push(name.clone());
            }
        }
        let has = |n: &str| names.iter().any(|x| x == n);
        let align = if para.iter().any(|n| n == "p:align=center") {
            Align::Center
        } else if para.iter().any(|n| n == "p:align=right") {
            Align::Right
        } else if para.iter().any(|n| n == "p:align=justify") {
            Align::Justify
        } else {
            Align::Left
        };
        Formatting {
            bold: has("c:b"),
            italic: has("c:i"),
            underline: has("c:u"),
            strike: has("c:s"),
            align,
        }
    }

    fn report(&self) {
        let f = self.formatting();
        for cb in self.formatting.borrow().iter() {
            cb(f);
        }
    }

    /// Bold, italic, underline or strikethrough (`c:b`, `c:i`, `c:u`,
    /// `c:s`) on the selection, or for what's typed next.
    pub fn toggle(&self, name: &str) {
        if !self.allows_edits() {
            return;
        }
        let buffer = &self.buffer;
        let tag = rich::tag(buffer, name);
        match buffer.selection_bounds() {
            Some((s, e)) => {
                // All of it has the tag: take it off. Otherwise put it on.
                let mut next = s;
                let all = s.has_tag(&tag) && {
                    next.forward_to_tag_toggle(Some(&tag));
                    next >= e
                };
                if all {
                    buffer.remove_tag(&tag, &s, &e);
                } else {
                    buffer.apply_tag(&tag, &s, &e);
                }
                // Tag changes don't emit `changed`; the document did change.
                for f in self.changed.borrow().iter() {
                    f();
                }
            }
            None => {
                let on = !self.formatting_has(name);
                let mut pending = self.pending.borrow_mut();
                pending.retain(|(n, _)| n != name);
                pending.push((name.to_string(), on));
            }
        }
        self.report();
    }

    fn formatting_has(&self, name: &str) -> bool {
        let f = self.formatting();
        match name {
            "c:b" => f.bold,
            "c:i" => f.italic,
            "c:u" => f.underline,
            "c:s" => f.strike,
            _ => false,
        }
    }

    /// Aligns every paragraph the selection touches.
    pub fn set_align(&self, align: Align) {
        if !self.allows_edits() {
            return;
        }
        let buffer = &self.buffer;
        let (mut s, mut e) = buffer.selection_bounds().unwrap_or_else(|| {
            let i = buffer.iter_at_mark(&buffer.get_insert());
            (i, i)
        });
        s.set_line_offset(0);
        // A selection ending at the start of a line (a triple-click) doesn't
        // take that next paragraph along.
        if e > s && e.starts_line() {
            e.backward_char();
        }
        if !e.ends_line() {
            e.forward_to_line_end();
        }
        e.forward_char(); // the newline
        for n in [
            "p:align=left",
            "p:align=center",
            "p:align=right",
            "p:align=justify",
        ] {
            buffer.remove_tag(&rich::tag(buffer, n), &s, &e);
        }
        let name = match align {
            Align::Natural | Align::Left => None,
            Align::Center => Some("p:align=center"),
            Align::Right => Some("p:align=right"),
            Align::Justify => Some("p:align=justify"),
        };
        if let Some(n) = name {
            buffer.apply_tag(&rich::tag(buffer, n), &s, &e);
        }
        for f in self.changed.borrow().iter() {
            f();
        }
        self.report();
    }

    pub fn show_find(&self) {
        self.search_bar.set_search_mode(true);
        if let Some((s, e)) = self.buffer.selection_bounds() {
            let text = self.buffer.text(&s, &e, false);
            if !text.contains('\n') {
                self.search_entry.set_text(&text);
            }
        }
        self.search_entry.grab_focus();
    }

    fn find_from_start(&self) {
        let mut at = self.buffer.iter_at_mark(&self.buffer.get_insert());
        if let Some((s, _)) = self.buffer.selection_bounds() {
            at = s;
        }
        self.buffer.place_cursor(&at);
        self.find(true);
    }

    pub fn find(&self, forward: bool) {
        let needle = self.search_entry.text();
        if needle.is_empty() {
            return;
        }
        if self.held_bar.is_visible() {
            self.find_in_held(forward);
            return;
        }
        let buffer = &self.buffer;
        let (sel_start, sel_end) = buffer.selection_bounds().unwrap_or_else(|| {
            let i = buffer.iter_at_mark(&buffer.get_insert());
            (i, i)
        });
        let flags = gtk::TextSearchFlags::CASE_INSENSITIVE | gtk::TextSearchFlags::TEXT_ONLY;
        let found = if forward {
            sel_end
                .forward_search(&needle, flags, None)
                .or_else(|| buffer.start_iter().forward_search(&needle, flags, None))
        } else {
            sel_start
                .backward_search(&needle, flags, None)
                .or_else(|| buffer.end_iter().backward_search(&needle, flags, None))
        };
        match found {
            Some((s, e)) => {
                self.search_entry.remove_css_class("error");
                buffer.select_range(&s, &e);
                let mut s = s;
                self.view.scroll_to_iter(&mut s, 0.1, true, 0.0, 0.35);
            }
            None => self.search_entry.add_css_class("error"),
        }
    }

    /// Find in a held document whose buffer is one window of the file text.
    fn find_in_held(&self, forward: bool) {
        let needle = self.search_entry.text();
        let Some(plain) = self
            .painted_source
            .borrow()
            .as_ref()
            .map(|text| text.plain_text())
        else {
            return;
        };
        let total = plain.chars().count();
        let origin = self.held_origin.get();
        let (sel_start, sel_end) = self.buffer.selection_bounds().unwrap_or_else(|| {
            let i = self.buffer.iter_at_mark(&self.buffer.get_insert());
            (i, i)
        });
        let start_off = text_index(sel_start.offset());
        let end_off = text_index(sel_end.offset());
        let shown = text_index(self.buffer.char_count());
        let from = scan_from(
            &plain,
            needle.as_str(),
            forward,
            origin,
            start_off,
            end_off,
            shown,
        );
        let wrap_at = if forward { 0 } else { total };
        let Some((start, end)) = find_ci(&plain, needle.as_str(), forward, from)
            .or_else(|| find_ci(&plain, needle.as_str(), forward, wrap_at))
        else {
            self.search_entry.add_css_class("error");
            return;
        };
        let window_end = origin + HELD_WINDOW;
        let mut next = origin;
        if start < origin || end > window_end {
            let page = start / HELD_WINDOW * HELD_WINDOW;
            next = if end > page + HELD_WINDOW {
                start
            } else {
                page
            };
        }
        let max_origin = total.saturating_sub(HELD_WINDOW);
        if next > max_origin {
            next = max_origin;
        }
        if next != origin {
            self.held_origin.set(next);
            self.fill_held_window();
            self.place_held_bar(total, next);
        }
        let origin = self.held_origin.get();
        let s = self
            .buffer
            .iter_at_offset(text_offset(start.saturating_sub(origin)));
        let e = self
            .buffer
            .iter_at_offset(text_offset(end.saturating_sub(origin)));
        self.search_entry.remove_css_class("error");
        self.buffer.select_range(&s, &e);
        let mut s = s;
        self.view.scroll_to_iter(&mut s, 0.1, true, 0.0, 0.35);
    }
}

fn bar_origin(adj: &gtk::Adjustment) -> usize {
    let mut origin = bar_chars(adj.value());
    let page = bar_chars(adj.page_size()).max(1);
    let total = bar_chars(adj.upper());
    if total > page {
        origin = origin.min(total - page);
    }
    origin
}

fn bar_chars(value: f64) -> usize {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    value as usize
}

fn eq_ci(a: char, b: char) -> bool {
    if a == b || a.eq_ignore_ascii_case(&b) {
        return true;
    }
    let mut ac = a.to_lowercase();
    let mut bc = b.to_lowercase();
    loop {
        match (ac.next(), bc.next()) {
            (None, None) => return true,
            (Some(x), Some(y)) if x == y => {}
            _ => return false,
        }
    }
}

/// Character offsets of `needle` in `hay`. `from_char` is where the scan
/// starts. A forward scan begins there. A backward scan begins on the
/// character before it.
fn find_ci(hay: &str, needle: &str, forward: bool, from_char: usize) -> Option<(usize, usize)> {
    let hay: Vec<char> = hay.chars().collect();
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    let last = hay.len() - needle.len();
    let matches = |i: usize| {
        hay[i..i + needle.len()]
            .iter()
            .zip(needle.iter())
            .all(|(a, b)| eq_ci(*a, *b))
    };
    if forward {
        let start = from_char.min(hay.len());
        if start > last {
            return None;
        }
        return (start..=last)
            .find(|&i| matches(i))
            .map(|i| (i, i + needle.len()));
    }
    if from_char == 0 {
        return None;
    }
    let start = from_char.saturating_sub(1).min(last);
    (0..=start)
        .rev()
        .find(|&i| matches(i))
        .map(|i| (i, i + needle.len()))
}

fn text_index(offset: i32) -> usize {
    usize::try_from(offset.max(0)).expect("a text offset is non-negative")
}

/// Character where a held scan starts.
///
/// A caret includes a match that begins there. A selection that is the
/// current match, including one clipped by the window, starts after that
/// match's end in the file. Any other selection starts at its own end.
/// Backward search starts where a match can still end at the caret, so
/// the step is the whole needle. `gtk_text_iter_backward_search` refuses
/// a match whose end lies after the iter.
fn scan_from(
    plain: &str,
    needle: &str,
    forward: bool,
    origin: usize,
    start_off: usize,
    end_off: usize,
    shown: usize,
) -> usize {
    let model_start = origin + start_off;
    let model_end = origin + end_off;
    if !forward {
        let needle_len = needle.chars().count();
        return model_start.saturating_sub(needle_len.saturating_sub(1));
    }
    if start_off == end_off {
        return model_start;
    }
    let Some(end) = match_end(plain, model_start, needle) else {
        return model_end;
    };
    let visible_end = end.min(origin + shown);
    if model_end >= visible_end && model_end <= end {
        end
    } else {
        model_end
    }
}

fn match_end(hay: &str, at: usize, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let mut rest = hay.chars().skip(at);
    let mut matched = 0usize;
    for n in needle.chars() {
        let c = rest.next()?;
        if !eq_ci(c, n) {
            return None;
        }
        matched += 1;
    }
    Some(at + matched)
}

fn text_offset(n: usize) -> i32 {
    i32::try_from(n).expect("a text offset fits in i32")
}

fn text_len(n: usize) -> isize {
    isize::try_from(n).expect("a text length fits in isize")
}

fn tags_for(buffer: &gtk::TextBuffer, style: &omaquill::rtf::CharStyle) -> Vec<gtk::TextTag> {
    rich::char_tags(style)
        .iter()
        .map(|name| rich::tag(buffer, name))
        .collect()
}

fn paint_new_lines(
    buffer: &gtk::TextBuffer,
    origin: i32,
    end: i32,
    style: &omaquill::rtf::ParaStyle,
) {
    if origin >= end {
        return;
    }
    let mut line = buffer.iter_at_offset(origin);
    if !line.starts_line() && !line.forward_line() {
        return;
    }
    while line.offset() < end {
        let start = line;
        let mut next = line;
        if !next.forward_line() {
            next = buffer.end_iter();
        }
        let mut drop = Vec::new();
        buffer.tag_table().foreach(|tag| {
            if tag.name().is_some_and(|name| name.starts_with("p:")) {
                drop.push(tag.clone());
            }
        });
        for tag in drop {
            buffer.remove_tag(&tag, &start, &next);
        }
        for name in rich::para_tags(style) {
            buffer.apply_tag(&rich::tag(buffer, &name), &start, &next);
        }
        if next.is_end() {
            break;
        }
        line = next;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use omaquill::rtf::{self, Align, CharStyle, ParaStyle, Paragraph, RichText, Run};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    fn type_at(ed: &Editor, offset: i32, text: &str) {
        let mut at = ed.buffer.iter_at_offset(offset);
        ed.buffer.insert(&mut at, text);
    }

    /// Typed text takes the formatting around it, and Ctrl+B with nothing
    /// selected applies to what's typed next. Needs a display.
    pub(crate) fn empty_paragraphs_keep_their_style_and_lines_align_alone() {
        let ed = Editor::new(true);
        let src = r"{\rtf1\pard one\
\pard\qc \
\pard three}";
        ed.load(rtf::parse(src.as_bytes()));
        let empty = ed.plain_text().find('\n').unwrap() as i32 + 1;
        type_at(&ed, empty, "two");
        let t = ed.text();
        assert_eq!(t.paragraphs[1].text(), "two");
        assert_eq!(t.paragraphs[1].style.align, Align::Center);
        assert_eq!(t.paragraphs[0].style.align, Align::Natural);
        // Select line one as a triple-click does (up to the next line).
        let s = ed.buffer.iter_at_offset(0);
        let e = ed.buffer.iter_at_offset(4);
        ed.buffer.select_range(&s, &e);
        ed.set_align(Align::Right);
        let t = ed.text();
        assert_eq!(t.paragraphs[0].style.align, Align::Right);
        assert_eq!(t.paragraphs[1].style.align, Align::Center);
    }

    pub(crate) fn typing_carries_formatting() {
        let ed = Editor::new(true);
        let src = r"{\rtf1\ansi{\fonttbl\f0 Palatino-Roman;}\pard\qc\fi720\f0\fs26 plain {\i slanted} end\
next}";
        ed.load(rtf::parse(src.as_bytes()));
        // Inside the italic run: italic, same font and size.
        type_at(&ed, 9, "XX");
        // At the start of the second paragraph: its own style.
        let second = ed.plain_text().find("next").unwrap() as i32;
        type_at(&ed, second, "Y");
        let t = ed.text();
        let slanted: Vec<_> = t.paragraphs[0]
            .runs
            .iter()
            .filter(|r| r.style.italic)
            .collect();
        assert_eq!(slanted.len(), 1);
        assert_eq!(slanted[0].text, "slaXXnted");
        assert_eq!(slanted[0].style.size, 26);
        assert_eq!(slanted[0].style.font.as_deref(), Some("Palatino"));
        assert_eq!(t.paragraphs[1].text(), "Ynext");
        assert!(
            t.paragraphs[1]
                .runs
                .iter()
                .all(|r| !r.style.italic && r.style.size == 26)
        );

        // Enter at the end of a paragraph keeps the paragraph's style.
        let end = ed.plain_text().find(" end").unwrap() as i32 + 4;
        ed.buffer.place_cursor(&ed.buffer.iter_at_offset(end));
        type_at(&ed, end, "\nnew");
        let t = ed.text();
        assert_eq!(t.paragraphs[1].text(), "new");
        assert_eq!(t.paragraphs[1].style.align, Align::Center);
        assert_eq!(t.paragraphs[1].style.first_indent, 720);

        // Bold toggled with no selection: the next characters are bold.
        let at = ed.plain_text().find("new").unwrap() as i32 + 3;
        ed.buffer.place_cursor(&ed.buffer.iter_at_offset(at));
        ed.toggle("c:b");
        type_at(&ed, at, "B");
        let t = ed.text();
        let bold: Vec<_> = t.paragraphs[1]
            .runs
            .iter()
            .filter(|r| r.style.bold)
            .collect();
        assert_eq!(bold.len(), 1);
        assert_eq!(bold[0].text, "B");

        // Bold on a selection, then off again.
        let s = ed.buffer.iter_at_offset(0);
        let e = ed.buffer.iter_at_offset(5);
        ed.buffer.select_range(&s, &e);
        ed.toggle("c:b");
        assert!(ed.text().paragraphs[0].runs[0].style.bold);
        assert_eq!(ed.text().paragraphs[0].runs[0].text, "plain");
        ed.toggle("c:b");
        assert!(!ed.text().paragraphs[0].runs[0].style.bold);

        // A paste brings its own size; the text keeps one size, not two.
        let s0 = ed.buffer.iter_at_offset(0);
        let s1 = ed.buffer.iter_at_offset(2);
        ed.buffer
            .apply_tag(&rich::tag(&ed.buffer, "c:size=40"), &s0, &s1);
        let names = rich::tag_names(&ed.buffer.iter_at_offset(0));
        assert_eq!(names.iter().filter(|n| n.starts_with("c:size=")).count(), 1);
    }

    /// A spelling underline is a tag the file does not store.
    pub(crate) fn a_spelling_mark_is_not_saved() {
        let ed = Editor::new(true);
        ed.load(rtf::parse(br"{\rtf1 hello helo}"));
        let before = ed.text();
        let tag = gtk::TextTag::new(Some("spell"));
        tag.set_underline(gtk::pango::Underline::Error);
        ed.buffer.tag_table().add(&tag);
        let start = ed.buffer.iter_at_offset(6);
        let end = ed.buffer.iter_at_offset(10);
        ed.buffer.apply_tag(&tag, &start, &end);
        assert_eq!(ed.text(), before);
        assert_eq!(ed.plain_text(), "hello helo");
    }

    pub(crate) fn replace_all_is_one_undo_and_keeps_the_run_style() {
        let ed = Editor::new(true);
        let src =
            r"{\rtf1\ansi{\fonttbl\f0 Palatino-Roman;}\pard\f0\fs24 Say hello and {\i hello}.}";
        ed.load(rtf::parse(src.as_bytes()));
        let query = omaquill::replace::query(
            "hello",
            "hi",
            omaquill::replace::Options {
                case: omaquill::replace::Case::Sensitive,
                word: omaquill::replace::Word::Any,
                mode: omaquill::replace::Mode::Literal,
            },
        )
        .unwrap();
        let plan = omaquill::replace::plan_all(&ed.text(), &query);
        ed.apply_edits(&plan);
        let text = ed.text();
        let plain = text.plain_text();
        assert_eq!(plain, "Say hi and hi.");
        let styled: Vec<bool> = text.paragraphs[0]
            .runs
            .iter()
            .flat_map(|run| run.text.chars().map(|_| run.style.italic))
            .collect();
        let first = plain.find("hi").unwrap();
        let second = plain.rfind("hi").unwrap();
        assert!(styled[first..first + 2].iter().all(|italic| !italic));
        assert!(styled[second..second + 2].iter().all(|italic| *italic));
        assert!(ed.buffer.can_undo());
        ed.buffer.undo();
        assert_eq!(ed.plain_text(), "Say hello and hello.");
        assert!(!ed.buffer.can_undo());
    }

    pub(crate) fn replace_all_refuses_a_loaded_image() {
        let ed = Editor::new(true);
        let image = rtf::parse(br"{\rtf1\ansi\pard alpha {\pict\pngblip deadbeef} beta}");
        ed.load(image);
        assert_eq!(ed.lossy(), ["images"]);
        let query = omaquill::replace::query(
            "alpha",
            "omega",
            omaquill::replace::Options {
                case: omaquill::replace::Case::Sensitive,
                word: omaquill::replace::Word::Any,
                mode: omaquill::replace::Mode::Literal,
            },
        )
        .unwrap();
        let blocked = omaquill::replace::plan_document(&ed.lossy(), &ed.text(), &query);
        assert!(
            matches!(
                blocked,
                Err(omaquill::replace::ReplaceBlock::Lossy(ref reason))
                    if reason.as_slice() == ["images"]
            ),
            "{blocked:?}"
        );
        assert!(ed.plain_text().contains("alpha"));
        ed.load(rtf::parse(br"{\rtf1\ansi\pard alpha beta}"));
        assert!(ed.lossy().is_empty());
        let plan = omaquill::replace::plan_document(&ed.lossy(), &ed.text(), &query).unwrap();
        ed.apply_edits(&plan).unwrap();
        assert_eq!(ed.plain_text(), "omega beta");
    }

    fn toggled(runs: usize) -> RichText {
        let link: Rc<str> = Rc::from("https://example.test/chapter");
        let mut paragraph = Paragraph::default();
        for i in 0..runs {
            paragraph.runs.push(Run {
                text: "a".into(),
                style: CharStyle {
                    bold: i % 2 == 0,
                    link: Some(Rc::clone(&link)),
                    ..CharStyle::default()
                },
            });
        }
        RichText {
            paragraphs: vec![paragraph],
            lossy: Vec::new(),
        }
    }

    /// A style change is not a line break. Find uses the buffer's own
    /// search, so a needle that sits on two runs has to be one span of text.
    pub(crate) fn find_matches_across_a_style_boundary() {
        let mut paragraph = Paragraph::default();
        for i in 0..32 {
            paragraph.runs.push(Run {
                text: "q".into(),
                style: CharStyle {
                    bold: i % 2 == 0,
                    ..CharStyle::default()
                },
            });
        }
        paragraph.runs.push(Run {
            text: "z".into(),
            style: CharStyle {
                italic: true,
                ..CharStyle::default()
            },
        });
        paragraph.runs.push(Run {
            text: "z".into(),
            style: CharStyle::default(),
        });
        let text = RichText {
            paragraphs: vec![paragraph],
            lossy: Vec::new(),
        };
        let ed = Editor::new(true);
        ed.load(text);
        assert_eq!(
            ed.buffer.line_count(),
            1,
            "one paragraph became {} lines",
            ed.buffer.line_count()
        );
        ed.search_entry.set_text("qz");
        ed.find(true);
        let (start, end) = ed
            .buffer
            .selection_bounds()
            .expect("find missed qz where a style run changes");
        assert_eq!(ed.buffer.text(&start, &end, false).as_str(), "qz");
        assert!(!ed.search_entry.has_css_class("error"));
    }

    /// The editor window opens a run-heavy document. The view is on screen,
    /// which is where a per-run insert relayouts.
    pub(crate) fn large_documents_open_in_the_window() {
        let ed = Editor::new(true);
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&ed.root));
        window.present();
        let context = gtk::glib::MainContext::default();
        for _ in 0..20 {
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
        let runs = 64usize;
        let text = toggled(runs);
        let started = std::time::Instant::now();
        ed.load(text);
        let mut shown_ms = started.elapsed().as_secs_f64() * 1000.0;
        for _ in 0..30 {
            if ed.view.iter_location(&ed.buffer.start_iter()).height() > 0 {
                shown_ms = started.elapsed().as_secs_f64() * 1000.0;
                break;
            }
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
        eprintln!("gtk open runs={runs} ms={shown_ms:.1}");
        assert_eq!(ed.buffer.line_count(), 1);
        let names = rich::tag_names(&ed.buffer.start_iter());
        assert!(names.iter().any(|name| name == "c:b"));
        assert!(names.iter().any(|name| name.starts_with("c:link=")));
        let back = ed.text();
        assert_eq!(back.paragraphs.len(), 1);
        assert_eq!(back.paragraphs[0].runs.len(), runs);
        window.close();
    }

    /// A paragraph GTK cannot tag without a long block shows its words at
    /// once, on one line, without the style tags. Find works on those words.
    /// The saved form stays the source document, so a save cannot strip the
    /// formatting. The view stays read-only.
    pub(crate) fn a_hostile_paragraph_opens_plain_and_read_only() {
        let mut text = toggled(2_000);
        text.paragraphs[0].runs[1_000].text = "q".into();
        text.paragraphs[0].runs[1_001].text = "z".into();
        let ed = Editor::new(true);
        let owned = text.clone();
        let started = std::time::Instant::now();
        ed.load(owned);
        let open_ms = started.elapsed().as_secs_f64() * 1000.0;
        eprintln!("hostile open ms={open_ms:.1}");
        assert!(
            open_ms < 100.0,
            "open blocked the thread for {open_ms:.1} ms"
        );
        assert!(ed.is_too_formatted());
        assert!(!ed.held_for_length());
        assert!(!ed.view.is_editable());
        assert_eq!(ed.view.wrap_mode(), gtk::WrapMode::Char);
        assert_eq!(ed.buffer.line_count(), 1);
        assert_eq!(ed.text(), text);
        assert!(ed.plain_text().contains("qz"));
        let names = rich::tag_names(&ed.buffer.start_iter());
        assert!(
            !names.iter().any(|name| name == "c:b"),
            "plain open applied a bold tag: {names:?}"
        );
        ed.search_entry.set_text("qz");
        ed.find(true);
        let (start, end) = ed
            .buffer
            .selection_bounds()
            .expect("find missed qz in the plain paragraph");
        assert_eq!(ed.buffer.text(&start, &end, false).as_str(), "qz");
        let context = gtk::glib::MainContext::default();
        for _ in 0..10 {
            if !context.iteration(false) {
                break;
            }
        }
        assert!(ed.is_too_formatted());
        assert!(!ed.view.is_editable());
        assert_eq!(ed.text(), text);
        assert_eq!(ed.buffer.line_count(), 1);
    }

    /// Loading another document drops the plain hold. Later idle turns must
    /// not write the old paragraph back.
    pub(crate) fn switching_mid_paint_leaves_the_new_document() {
        let big = toggled(2_000);
        let ed = Editor::new(true);
        ed.load(big.clone());
        assert!(ed.is_too_formatted());
        assert_eq!(ed.text(), big);
        let hello = rtf::RichText::from_plain("Hello");
        ed.load(hello);
        assert!(!ed.is_too_formatted());
        assert!(ed.view.is_editable());
        assert_eq!(ed.plain_text(), "Hello");
        assert_eq!(ed.text().plain_text(), "Hello");
        let context = gtk::glib::MainContext::default();
        for _ in 0..40 {
            if !context.iteration(false) {
                break;
            }
        }
        assert!(!ed.is_too_formatted());
        assert_eq!(ed.plain_text(), "Hello");
        assert_eq!(ed.text().plain_text(), "Hello");
    }

    /// A held document accepts no edit. Alignment, paste, undo, and replace
    /// leave the buffer and the source alone, and replace is reported the
    /// way an image document is.
    pub(crate) fn a_held_document_refuses_every_edit() {
        let mut text = toggled(2_000);
        text.paragraphs[0].style.align = Align::Center;
        text.paragraphs[0].runs[10].text = " helo ".into();
        let ed = Editor::new(true);
        ed.load(text.clone());
        assert!(ed.is_too_formatted());
        assert!(!ed.held_for_length());
        assert!(!ed.loading.get());
        assert!(!ed.allows_edits());
        assert!(!ed.view.is_editable());
        let plain = ed.plain_text();
        let dirtied = Rc::new(Cell::new(false));
        let flag = dirtied.clone();
        ed.connect_changed(move || flag.set(true));
        ed.set_align(Align::Left);
        ed.toggle("c:b");
        assert!(!dirtied.get());
        assert_eq!(ed.text(), text);
        assert_eq!(ed.text().paragraphs[0].style.align, Align::Center);
        let mut at = ed.buffer.start_iter();
        ed.buffer.insert(&mut at, "ZZ");
        assert_eq!(ed.plain_text(), plain, "insert changed a held buffer");
        assert!(!dirtied.get());
        ed.buffer.undo();
        assert_eq!(ed.plain_text(), plain);
        let query = omaquill::replace::query(
            "helo",
            "hello",
            omaquill::replace::Options {
                case: omaquill::replace::Case::Sensitive,
                word: omaquill::replace::Word::Any,
                mode: omaquill::replace::Mode::Literal,
            },
        )
        .unwrap();
        let blocked = ed.plan_replace(&query);
        assert!(
            matches!(
                blocked,
                Err(omaquill::replace::ReplaceBlock::Lossy(ref reason))
                    if reason.as_slice() == ["inline formatting"]
            ),
            "{blocked:?}"
        );
        let plan = omaquill::replace::plan_document(&[], &text, &query).unwrap();
        assert!(ed.apply_edits(&plan).is_none());
        assert_eq!(ed.plain_text(), plain);
        assert_eq!(ed.text(), text);
        assert!(!dirtied.get());
    }

    /// Adjacent runs with the same style are one GTK range, so a uniform
    /// paragraph under the line-length cap is styled before load returns.
    pub(crate) fn identical_runs_open_without_an_idle_pass() {
        let runs = 5_000usize;
        let mut paragraph = Paragraph::default();
        for _ in 0..runs {
            paragraph.runs.push(Run {
                text: "a".into(),
                style: CharStyle {
                    bold: true,
                    ..CharStyle::default()
                },
            });
        }
        let text = RichText {
            paragraphs: vec![paragraph],
            lossy: Vec::new(),
        };
        let ed = Editor::new(true);
        ed.load(text);
        assert!(!ed.is_too_formatted());
        assert!(ed.view.is_editable());
        assert_eq!(ed.buffer.line_count(), 1);
        let names = rich::tag_names(&ed.buffer.start_iter());
        assert!(names.iter().any(|name| name == "c:b"));
        let back = ed.text();
        assert_eq!(back.paragraphs.len(), 1);
        assert_eq!(back.paragraphs[0].runs.len(), 1);
        assert_eq!(back.paragraphs[0].runs[0].text.len(), runs);
    }

    /// One uniform style merges to a couple of tag applies, so the tag cap
    /// does not hold it. The line is one unspaced run, and word-char layout
    /// of that run is what blocks the UI thread.
    pub(crate) fn a_long_uniform_line_opens_plain() {
        let runs = 10_000usize;
        let mut paragraph = Paragraph::default();
        for _ in 0..runs {
            paragraph.runs.push(Run {
                text: "a".into(),
                style: CharStyle {
                    bold: true,
                    ..CharStyle::default()
                },
            });
        }
        let text = RichText {
            paragraphs: vec![paragraph],
            lossy: Vec::new(),
        };
        assert!(rich::heavy_applies(&text) <= rich::SYNC_TAG_APPLIES);
        assert!(rich::needs_plain_view(&text));
        let ed = Editor::new(true);
        let owned = text.clone();
        let started = std::time::Instant::now();
        ed.load(owned);
        let open_ms = started.elapsed().as_secs_f64() * 1000.0;
        eprintln!("uniform plain runs={runs} open_ms={open_ms:.1}");
        assert!(
            open_ms < 100.0,
            "open blocked the thread for {open_ms:.1} ms"
        );
        assert!(ed.is_too_formatted());
        assert!(ed.held_for_length());
        assert!(!ed.loading.get());
        assert!(!ed.view.is_editable());
        assert_eq!(ed.view.wrap_mode(), gtk::WrapMode::Char);
        assert_eq!(ed.buffer.line_count(), 1);
        assert_eq!(ed.text(), text);
        assert_eq!(ed.plain_text().chars().count(), runs);
        let names = rich::tag_names(&ed.buffer.start_iter());
        assert!(
            !names.iter().any(|name| name == "c:b"),
            "plain open applied a bold tag: {names:?}"
        );
        let plain = ed.plain_text();
        let mut at = ed.buffer.start_iter();
        ed.buffer.insert(&mut at, "ZZ");
        assert_eq!(ed.plain_text(), plain);
    }

    /// Many ordinary paragraphs stay fully styled. The plain hold is for one
    /// paragraph with a large tag count or one long unspaced run, not for a
    /// long manuscript.
    pub(crate) fn many_plain_paragraphs_stay_editable() {
        let mut paragraphs = Vec::new();
        for _ in 0..2_000 {
            paragraphs.push(Paragraph {
                runs: vec![Run {
                    text: "Hello".into(),
                    style: CharStyle::default(),
                }],
                ..Paragraph::default()
            });
        }
        let ed = Editor::new(true);
        ed.load(RichText {
            paragraphs,
            lossy: Vec::new(),
        });
        assert!(!ed.is_too_formatted());
        assert!(ed.view.is_editable());
        assert_eq!(ed.buffer.line_count(), 2_000);
    }

    fn one_line(chars: usize) -> RichText {
        RichText {
            paragraphs: vec![Paragraph {
                runs: vec![Run {
                    text: "a".repeat(chars),
                    style: CharStyle::default(),
                }],
                ..Paragraph::default()
            }],
            lossy: Vec::new(),
        }
    }

    fn buffer_string(ed: &Editor) -> String {
        let (start, end) = ed.buffer.bounds();
        ed.buffer.text(&start, &end, false).to_string()
    }

    fn applied_tag_names(buffer: &gtk::TextBuffer) -> Vec<String> {
        let mut names = Vec::new();
        let mut iter = buffer.start_iter();
        loop {
            for name in rich::tag_names(&iter) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
            if buffer.char_count() == 0 || !iter.forward_char() {
                break;
            }
        }
        names
    }

    /// A styled document whose letters match the held document's first
    /// window used to stay bold, linked, and selected. The held view is
    /// plain, Find starts at the cursor, and releasing the hold clears
    /// the buffer.
    pub(crate) fn a_held_load_clears_the_previous_tags_and_selection() {
        let ed = Editor::new(true);
        let link: Rc<str> = Rc::from("https://example.test/chapter");
        let styled = RichText {
            paragraphs: vec![Paragraph {
                style: ParaStyle {
                    align: Align::Center,
                    left_indent: 720,
                    ..ParaStyle::default()
                },
                runs: vec![Run {
                    text: "a".repeat(HELD_WINDOW),
                    style: CharStyle {
                        bold: true,
                        link: Some(Rc::clone(&link)),
                        ..CharStyle::default()
                    },
                }],
            }],
            lossy: Vec::new(),
        };
        ed.load(styled);
        assert!(!ed.is_too_formatted());
        let styled_tags = applied_tag_names(&ed.buffer);
        assert!(
            styled_tags.iter().any(|name| name == "c:b"),
            "the styled open had no bold tag: {styled_tags:?}"
        );
        assert!(
            styled_tags.iter().any(|name| name.starts_with("c:link=")),
            "the styled open had no link tag: {styled_tags:?}"
        );
        assert!(
            styled_tags.iter().any(|name| name == "p:align=center"),
            "the styled open had no center tag: {styled_tags:?}"
        );
        assert!(
            styled_tags.iter().any(|name| name == "p:li=720"),
            "the styled open had no indent tag: {styled_tags:?}"
        );
        let from = ed.buffer.iter_at_offset(4);
        let to = ed.buffer.iter_at_offset(12);
        ed.buffer.select_range(&from, &to);
        assert!(ed.buffer.selection_bounds().is_some());
        assert_ne!(ed.buffer.cursor_position(), 0);

        let mut held_body = "a".repeat(HELD_WINDOW);
        held_body.push_str(&"b".repeat(10_000));
        ed.load(RichText::from_plain(&held_body));
        assert!(ed.is_too_formatted());
        let (start, end) = ed.buffer.bounds();
        assert_eq!(
            ed.buffer.text(&start, &end, false).as_str(),
            "a".repeat(HELD_WINDOW)
        );
        let held_tags = applied_tag_names(&ed.buffer);
        assert!(
            held_tags.is_empty(),
            "a held load kept the previous document's tags: {held_tags:?}"
        );
        assert!(
            ed.buffer.selection_bounds().is_none(),
            "a held load kept the previous selection"
        );
        assert_eq!(
            ed.buffer.cursor_position(),
            0,
            "a held load left the cursor at {}",
            ed.buffer.cursor_position()
        );

        ed.release_plain();
        assert!(!ed.is_too_formatted());
        assert_eq!(buffer_string(&ed), "");
        let released = applied_tag_names(&ed.buffer);
        assert!(released.is_empty(), "release_plain kept tags: {released:?}");
        assert!(ed.buffer.selection_bounds().is_none());
        assert_eq!(ed.buffer.cursor_position(), 0);
        assert_eq!(ed.view.wrap_mode(), gtk::WrapMode::WordChar);
    }

    /// Letters for one held page. `id` is unique per page, so the first
    /// letters of the page are unique and the rest do not repeat a
    /// 26-character cycle.
    fn append_page(out: &mut String, id: usize, len: usize) {
        let mut n = id;
        for i in 0..len {
            let digit = if i < 6 {
                let d = (n % 26) as u8;
                n /= 26;
                d
            } else {
                ((i + id) % 26) as u8
            };
            out.push(char::from(b'a' + digit));
        }
    }

    fn page_style(kind: u8, page: usize) -> CharStyle {
        match kind {
            0 => CharStyle {
                bold: true,
                highlight: Some(((page % 200) as u8, (page % 199) as u8, 40)),
                ..CharStyle::default()
            },
            1 => CharStyle {
                bold: page.is_multiple_of(2),
                italic: !page.is_multiple_of(2),
                link: Some(Rc::from(format!("https://example.test/{page}"))),
                ..CharStyle::default()
            },
            _ => CharStyle {
                underline: page.is_multiple_of(2),
                strike: !page.is_multiple_of(2),
                color: Some((20, (page % 200) as u8, 80)),
                ..CharStyle::default()
            },
        }
    }

    /// One held paragraph. Each page is its own run, with its own letters
    /// and its own character style. `kind` and `size_tag` keep two
    /// documents from sharing a page.
    fn timed_document(chars: usize, kind: u8, size_tag: usize) -> RichText {
        let mut runs = Vec::new();
        let mut page = 0usize;
        let mut done = 0usize;
        while done < chars {
            let n = (chars - done).min(HELD_WINDOW);
            let id = kind as usize * 100_000 + size_tag * 1_000 + page;
            let mut text = String::with_capacity(n);
            append_page(&mut text, id, n);
            runs.push(Run {
                text,
                style: page_style(kind, page),
            });
            done += n;
            page += 1;
        }
        let style = match kind {
            0 => ParaStyle {
                align: Align::Center,
                left_indent: 360,
                ..ParaStyle::default()
            },
            1 => ParaStyle {
                align: Align::Justify,
                space_before: 120,
                ..ParaStyle::default()
            },
            _ => ParaStyle {
                align: Align::Right,
                right_indent: 240,
                ..ParaStyle::default()
            },
        };
        RichText {
            paragraphs: vec![Paragraph { style, runs }],
            lossy: Vec::new(),
        }
    }

    /// Blocks until the frame clock's after-paint signal. The update tick
    /// fires before the frame is drawn.
    fn stop_after_paint(view: &gtk::TextView, context: &gtk::glib::MainContext, label: &str) {
        let done = Rc::new(Cell::new(false));
        let flag = Rc::clone(&done);
        let clock = view
            .frame_clock()
            .unwrap_or_else(|| panic!("{label} has no frame clock"));
        let id = clock.connect_after_paint(move |_| flag.set(true));
        clock.request_phase(gtk::gdk::FrameClockPhase::AFTER_PAINT);
        view.queue_draw();
        let mut max_iter_ms = 0.0f64;
        for _ in 0..60 {
            if done.get() {
                break;
            }
            let step = std::time::Instant::now();
            if !context.iteration(true) {
                break;
            }
            max_iter_ms = max_iter_ms.max(step.elapsed().as_secs_f64() * 1000.0);
        }
        clock.disconnect(id);
        assert!(done.get(), "{label} did not paint");
        assert!(
            max_iter_ms < 100.0,
            "{label} main-loop turn blocked {max_iter_ms:.1} ms"
        );
    }

    fn time_open(ed: &Editor, context: &gtk::glib::MainContext, label: &str, text: &RichText) {
        let plain = text.plain_text();
        let chars = plain.chars().count();
        let head: String = plain.chars().take(HELD_WINDOW).collect();
        let before = buffer_string(ed);
        assert_ne!(
            before, head,
            "{label} loaded a window the buffer already showed"
        );
        // The copy stays here so the timed load matches the window, which
        // moves the document it already owns.
        let owned = text.clone();
        let started = std::time::Instant::now();
        ed.load(owned);
        // The window is already on screen. This wait is the paint of the
        // new text, and the open bound includes it.
        stop_after_paint(&ed.view, context, label);
        let open_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(
            buffer_string(ed),
            head,
            "{label} did not write its first window"
        );
        assert_eq!(ed.buffer.line_count(), 1);
        assert!(ed.is_too_formatted());
        assert!(!ed.view.is_editable());
        assert!(!ed.loading.get());
        let start_y = ed.view.iter_location(&ed.buffer.start_iter()).y();
        let mut last = ed.buffer.end_iter();
        let wrapped = last.backward_char() && ed.view.iter_location(&last).y() > start_y;
        eprintln!(
            "gtk plain shape={label} chars={chars} buf={} lines={} open_ms={open_ms:.1} wrapped={wrapped} bar={}",
            ed.buffer.char_count(),
            ed.buffer.line_count(),
            ed.held_bar.is_visible()
        );
        assert!(
            open_ms < 100.0,
            "{label} open blocked {open_ms:.1} ms at {chars}"
        );
        assert!(wrapped, "{label} did not wrap the new line");
        assert_eq!(ed.view.wrap_mode(), gtk::WrapMode::Char);
        let start_y = ed.view.iter_location(&ed.buffer.start_iter()).y();
        let mut last = ed.buffer.end_iter();
        assert!(last.backward_char(), "{label} was empty");
        let end_y = ed.view.iter_location(&last).y();
        assert!(
            end_y > start_y,
            "{label} did not wrap the line, start {start_y} end {end_y}"
        );
        assert_eq!(ed.plain_text(), text.plain_text());
        assert_eq!(ed.plain_text().chars().count(), chars);
        assert_eq!(ed.text(), *text);
        let (buf_start, buf_end) = ed.buffer.bounds();
        let shown = ed.buffer.text(&buf_start, &buf_end, false);
        assert!(
            text.plain_text().starts_with(shown.as_str()),
            "{label} buffer is not a prefix of the file"
        );
        assert_eq!(shown.chars().count(), chars.min(HELD_WINDOW));
        if chars > HELD_WINDOW {
            assert!(ed.held_bar.is_visible());
            // The last page starts on a window boundary and holds the
            // remainder, which is short when the file is not a whole
            // number of windows.
            let last_page = chars.saturating_sub(1) / HELD_WINDOW;
            let last_len = chars - last_page * HELD_WINDOW;
            let expected: String = plain
                .chars()
                .skip(last_page * HELD_WINDOW)
                .take(last_len)
                .collect();
            assert_ne!(
                buffer_string(ed),
                expected,
                "{label} scrolled onto a window the buffer already showed"
            );
            let adj = ed.held_bar.adjustment();
            let started = std::time::Instant::now();
            adj.set_value(adj.upper());
            let scroll_label = format!("{label} scroll");
            stop_after_paint(&ed.view, context, &scroll_label);
            let scroll_ms = started.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(
                buffer_string(ed),
                expected,
                "{label} scroll did not write the last window"
            );
            let start_y = ed.view.iter_location(&ed.buffer.start_iter()).y();
            let mut last = ed.buffer.end_iter();
            let tail_wrapped = last.backward_char() && ed.view.iter_location(&last).y() > start_y;
            assert!(tail_wrapped, "{label} did not wrap the scrolled page");
            eprintln!("gtk plain scroll={label} chars={chars} scroll_ms={scroll_ms:.1}");
            assert!(
                scroll_ms < 100.0,
                "{label} scroll blocked {scroll_ms:.1} ms at {chars}"
            );
            assert_eq!(expected.chars().count(), last_len);
        } else {
            assert!(!ed.held_bar.is_visible());
        }
        let names = rich::tag_names(&ed.buffer.start_iter());
        assert!(
            !names.iter().any(|name| name == "c:b"),
            "{label} applied a bold tag"
        );
    }

    /// Word-char layout cost of one unspaced line, before the plain-view
    /// cap. Set OMAQUILL_GTK_OPEN=probe. This is the cost the cap avoids.
    fn probe_word_char(ed: &Editor, context: &gtk::glib::MainContext) {
        ed.view.set_wrap_mode(gtk::WrapMode::WordChar);
        for chars in [2_048usize, 4_096, 8_192, 10_000] {
            let body = "a".repeat(chars);
            let started = std::time::Instant::now();
            ed.buffer.set_text(&body);
            let open_ms = started.elapsed().as_secs_f64() * 1000.0;
            let mut max_iter_ms = 0.0f64;
            for _ in 0..20 {
                let step = std::time::Instant::now();
                if !context.iteration(false) && !context.pending() {
                    break;
                }
                max_iter_ms = max_iter_ms.max(step.elapsed().as_secs_f64() * 1000.0);
            }
            eprintln!(
                "gtk wordchar chars={chars} open_ms={open_ms:.1} max_iter_ms={max_iter_ms:.1} lines={}",
                ed.buffer.line_count()
            );
        }
    }

    /// `present` queues a frame. `pending()` can be false until that frame
    /// is ready, so a non-blocking drain returns without mapping the window.
    fn show_window(window: &gtk::Window) {
        window.present();
        let context = gtk::glib::MainContext::default();
        for _ in 0..30 {
            if window.is_mapped() {
                return;
            }
            if !context.iteration(true) {
                return;
            }
        }
    }

    /// The first painted frame of a newly mapped text view is the window
    /// coming on screen. On `9b7b2d6` the held line, loaded into a view
    /// that had already been painted, took 0.2 ms. The next window's first
    /// timed turn was 104.6 ms on x86_64 because this paint had not finished.
    /// `pending()` is false until the frame is ready, so the wait blocks.
    fn paint_editor(ed: &Editor) {
        let context = gtk::glib::MainContext::default();
        ed.view.set_wrap_mode(gtk::WrapMode::WordChar);
        ed.buffer.set_text("Hello");
        ed.buffer.place_cursor(&ed.buffer.start_iter());
        for _ in 0..30 {
            if ed.view.iter_location(&ed.buffer.start_iter()).height() > 0 && !context.pending() {
                return;
            }
            if !context.iteration(true) {
                return;
            }
        }
    }

    /// A held unspaced line wraps on characters inside the column. The buffer
    /// is a window of the file text, and find matches that text.
    ///
    /// The timed turns start after the view has been painted. A document
    /// opens into a window that is already on screen.
    pub(crate) fn a_held_long_line_wraps_on_characters() {
        let ed = Editor::new(true);
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&ed.root));
        show_window(&window);
        assert!(window.is_mapped(), "the window did not map");
        paint_editor(&ed);
        assert!(
            ed.view.iter_location(&ed.buffer.start_iter()).height() > 0,
            "the view was not painted"
        );
        let context = gtk::glib::MainContext::default();
        let chars = 10_000usize;
        let body = "a".repeat(chars);
        let started = std::time::Instant::now();
        ed.load(one_line(chars));
        let open_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert!(open_ms < 100.0, "open blocked {open_ms:.1} ms");
        assert!(ed.is_too_formatted());
        assert!(ed.held_for_length());
        assert!(!ed.view.is_editable());
        assert_eq!(ed.view.wrap_mode(), gtk::WrapMode::Char);
        assert_eq!(ed.plain_text(), body);
        assert!(ed.held_bar.is_visible());
        let (buf_start, buf_end) = ed.buffer.bounds();
        assert_eq!(
            ed.buffer.text(&buf_start, &buf_end, false).chars().count(),
            HELD_WINDOW
        );
        let mut max_iter = 0.0f64;
        let mut wrapped = false;
        for _ in 0..40 {
            let step = std::time::Instant::now();
            let _ran = context.iteration(false);
            let start_y = ed.view.iter_location(&ed.buffer.start_iter()).y();
            let mut last = ed.buffer.end_iter();
            if last.backward_char() {
                let end_y = ed.view.iter_location(&last).y();
                if end_y > start_y {
                    wrapped = true;
                }
            }
            max_iter = max_iter.max(step.elapsed().as_secs_f64() * 1000.0);
            if wrapped && !context.pending() {
                break;
            }
        }
        assert!(wrapped, "the line did not wrap");
        eprintln!("gtk held wrap open_ms={open_ms:.1} max_iter_ms={max_iter:.1}");
        assert!(max_iter < 100.0, "main-loop turn blocked {max_iter:.1} ms");
        ed.search_entry.set_text(&"a".repeat(100));
        ed.find(true);
        let (start, end) = ed.buffer.selection_bounds().expect("find missed the run");
        assert_eq!(
            ed.buffer.text(&start, &end, false).as_str(),
            "a".repeat(100)
        );
        window.close();
    }

    /// A zero-width space in the file is text. Open, an edit beside it, and
    /// replace keep that character. The saved RTF matches a write of that
    /// document, and the replace starts on the character after the space.
    pub(crate) fn a_zero_width_space_survives_edit_and_replace() {
        let source = "pre\u{200B}post";
        let file = rtf::write(&RichText::from_plain(source));
        assert!(
            file.contains("\\uc0\\u8203 "),
            "the file dropped U+200B: {file}"
        );
        let parsed = rtf::parse(file.as_bytes());
        assert_eq!(parsed.plain_text(), source);
        assert_eq!(parsed.paragraphs[0].runs.len(), 1);
        let ed = Editor::new(true);
        ed.load(parsed.clone());
        assert!(!ed.is_too_formatted());
        assert!(ed.allows_edits());
        assert_eq!(ed.view.wrap_mode(), gtk::WrapMode::WordChar);
        assert_eq!(ed.plain_text(), source);
        let (start, end) = ed.buffer.bounds();
        assert_eq!(ed.buffer.text(&start, &end, false).as_str(), source);
        assert_eq!(
            rtf::write(&ed.text()).as_bytes(),
            rtf::write(&parsed).as_bytes()
        );

        let mut at = ed.buffer.iter_at_offset(3);
        ed.buffer.insert(&mut at, "X");
        assert_eq!(ed.plain_text(), "preX\u{200B}post");
        let mut expected = parsed.clone();
        expected.paragraphs[0].runs[0].text = "preX\u{200B}post".into();
        assert_eq!(
            rtf::write(&ed.text()).as_bytes(),
            rtf::write(&expected).as_bytes()
        );

        ed.load(parsed.clone());
        assert_eq!(ed.plain_text(), source);
        let query = omaquill::replace::query(
            "post",
            "end",
            omaquill::replace::Options {
                case: omaquill::replace::Case::Sensitive,
                word: omaquill::replace::Word::Any,
                mode: omaquill::replace::Mode::Literal,
            },
        )
        .unwrap();
        let plan = ed.plan_replace(&query).unwrap();
        let edit = plan.edits().first().expect("post was not found");
        assert_eq!(edit.range().start(), 4);
        ed.apply_edits(&plan).unwrap();
        assert_eq!(ed.plain_text(), "pre\u{200B}end");
        expected.paragraphs[0].runs[0].text = "pre\u{200B}end".into();
        let saved = rtf::write(&ed.text());
        assert!(saved.contains("\\uc0\\u8203 "));
        assert_eq!(saved.as_bytes(), rtf::write(&expected).as_bytes());
    }

    /// A match that starts on the current page and ends past it. The window
    /// moves so the whole match is on screen, and copy takes that match.
    pub(crate) fn find_across_a_held_page_edge_selects_the_whole_match() {
        let ed = Editor::new(true);
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&ed.root));
        show_window(&window);
        // Two characters of the match sit on the first page. The rest
        // starts the next page, so a page-aligned window clips them.
        let mut body = "a".repeat(HELD_WINDOW - 2);
        body.push_str("EDXY");
        body.push_str(&"b".repeat(10_000));
        ed.load(RichText::from_plain(&body));
        assert!(ed.is_too_formatted());
        assert!(ed.held_bar.is_visible());
        let (start, end) = ed.buffer.bounds();
        let shown = ed.buffer.text(&start, &end, false);
        assert_eq!(shown.chars().count(), HELD_WINDOW);
        assert!(
            shown.ends_with("ED"),
            "the first page should end on the start of the match, got {shown:?}"
        );
        ed.search_entry.set_text("EDXY");
        ed.find(true);
        let (start, end) = ed
            .buffer
            .selection_bounds()
            .expect("find missed the match that crosses the page");
        let selected = ed.buffer.text(&start, &end, false);
        assert_eq!(
            selected.as_str(),
            "EDXY",
            "a match across the page edge was clipped to {selected:?}"
        );
        let (buf_start, buf_end) = ed.buffer.bounds();
        let shown = ed.buffer.text(&buf_start, &buf_end, false);
        assert!(
            shown.contains("EDXY"),
            "the window did not move to show the whole match: {shown:?}"
        );
        ed.view.emit_copy_clipboard();
        assert_eq!(
            clipboard_text(&ed.view),
            "EDXY",
            "copy took the clipped selection"
        );
        let origin = (HELD_WINDOW - 2) as f64;
        let bar = ed.held_bar.adjustment().value();
        assert!(
            (bar - origin).abs() < f64::EPSILON,
            "find left the bar at {bar}, not the window origin {origin}"
        );
        ed.held_bar.adjustment().set_value(bar + 0.4);
        assert_eq!(
            ed.held_origin.get(),
            HELD_WINDOW - 2,
            "a drag inside the same character refilled the window"
        );
        let (start, end) = ed
            .buffer
            .selection_bounds()
            .expect("the drag cleared the match");
        assert_eq!(ed.buffer.text(&start, &end, false).as_str(), "EDXY");
        let (buf_start, buf_end) = ed.buffer.bounds();
        let shown = ed.buffer.text(&buf_start, &buf_end, false);
        assert!(
            shown.starts_with("EDXY"),
            "the drag refilled from the start of the file: {shown}"
        );
        window.close();
    }

    pub(crate) fn find_of_a_long_needle_starts_at_the_match() {
        let ed = Editor::new(true);
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&ed.root));
        show_window(&window);
        let needle = "N".repeat(HELD_WINDOW + 50);
        let mut body = "a".repeat(HELD_WINDOW - 2);
        body.push_str(&needle);
        body.push_str(&"b".repeat(10_000));
        ed.load(RichText::from_plain(&body));
        assert!(ed.held_bar.is_visible());
        ed.search_entry.set_text(&needle);
        ed.find(true);
        let (buf_start, buf_end) = ed.buffer.bounds();
        let shown = ed.buffer.text(&buf_start, &buf_end, false);
        assert_eq!(shown.chars().count(), HELD_WINDOW);
        assert!(
            shown.chars().all(|c| c == 'N'),
            "the long needle was clipped at the page edge: {shown}"
        );
        let (start, end) = ed
            .buffer
            .selection_bounds()
            .expect("find missed the long needle");
        let selected = ed.buffer.text(&start, &end, false);
        assert_eq!(
            selected.chars().count(),
            HELD_WINDOW,
            "the selection kept the page-edge clip {selected:?}"
        );
        assert!(
            selected.chars().all(|c| c == 'N'),
            "the selection was not the needle: {selected}"
        );
        window.close();
    }

    pub(crate) fn find_next_on_a_long_needle_reaches_the_second_match() {
        let ed = Editor::new(true);
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&ed.root));
        show_window(&window);
        let needle = "N".repeat(HELD_WINDOW + 50);
        let first = HELD_WINDOW - 2;
        let second = first + needle.chars().count();
        let mut body = "a".repeat(first);
        body.push_str(&needle);
        body.push_str(&needle);
        body.push_str(&"b".repeat(10_000));
        ed.load(RichText::from_plain(&body));
        assert!(ed.held_bar.is_visible());
        ed.search_entry.set_text(&needle);
        ed.find(true);
        assert_eq!(ed.held_origin.get(), first);
        assert!(!ed.search_entry.has_css_class("error"));
        ed.find(true);
        assert_eq!(
            ed.held_origin.get(),
            second,
            "Find Next stayed inside the first match"
        );
        let bar = ed.held_bar.adjustment().value();
        assert!(
            (bar - second as f64).abs() < f64::EPSILON,
            "the scrollbar stayed at {bar}, not the second match at {second}"
        );
        assert!(!ed.search_entry.has_css_class("error"));
        let (buf_start, buf_end) = ed.buffer.bounds();
        let shown = ed.buffer.text(&buf_start, &buf_end, false);
        assert!(
            shown.chars().all(|c| c == 'N'),
            "the second match was not on screen: {shown}"
        );
        window.close();
    }

    pub(crate) fn find_next_on_the_only_long_needle_wraps() {
        let ed = Editor::new(true);
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&ed.root));
        show_window(&window);
        let needle = "N".repeat(HELD_WINDOW + 50);
        let at = HELD_WINDOW - 2;
        let mut body = "a".repeat(at);
        body.push_str(&needle);
        body.push_str(&"b".repeat(10_000));
        ed.load(RichText::from_plain(&body));
        assert!(ed.held_bar.is_visible());
        ed.search_entry.set_text(&needle);
        ed.find(true);
        assert_eq!(ed.held_origin.get(), at);
        assert!(!ed.search_entry.has_css_class("error"));
        ed.find(true);
        assert_eq!(
            ed.held_origin.get(),
            at,
            "the only match moved instead of wrapping"
        );
        let (start, end) = ed
            .buffer
            .selection_bounds()
            .expect("the wrap cleared the only match");
        let selected = ed.buffer.text(&start, &end, false);
        assert!(
            selected.chars().all(|c| c == 'N') && selected.chars().count() == HELD_WINDOW,
            "the wrap dropped the match: {selected}"
        );
        assert!(
            !ed.search_entry.has_css_class("error"),
            "a wrap that still finds the only long match painted the search entry red"
        );
        ed.load(RichText::from_plain("one unique word"));
        assert!(!ed.held_bar.is_visible());
        ed.search_entry.set_text("unique");
        ed.find(true);
        ed.find(true);
        let (start, end) = ed
            .buffer
            .selection_bounds()
            .expect("Find Next cleared the only word");
        assert_eq!(ed.buffer.text(&start, &end, false).as_str(), "unique");
        assert!(
            !ed.search_entry.has_css_class("error"),
            "Find Next on the only word painted the search entry red"
        );
        ed.search_entry.set_text("missing");
        ed.find(true);
        assert!(
            ed.search_entry.has_css_class("error"),
            "a query with no match left the search entry unmarked"
        );
        window.close();
    }

    /// Select All copies the held document from the model. The buffer is
    /// one window. A selection made by hand copies that selection.
    pub(crate) fn select_all_copies_the_held_document() {
        let ed = Editor::new(true);
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&ed.root));
        show_window(&window);
        let body = format!("Hello {}", "a".repeat(10_000));
        ed.load(RichText::from_plain(&body));
        assert!(ed.is_too_formatted());
        assert_eq!(buffer_string(&ed).chars().count(), HELD_WINDOW);
        ed.view.grab_focus();

        let (start, end) = ed.buffer.bounds();
        ed.buffer.select_range(&start, &end);
        ed.view.emit_copy_clipboard();
        let visible = clipboard_text(&ed.view);
        assert_eq!(
            visible.chars().count(),
            HELD_WINDOW,
            "a hand selection of the window copied {} characters",
            visible.chars().count()
        );
        assert_ne!(visible, body);

        ed.view.emit_select_all(true);
        ed.view.emit_copy_clipboard();
        assert_eq!(
            clipboard_text(&ed.view),
            body,
            "Select All copied the visible window, not the document"
        );

        let (start, end) = ed.buffer.bounds();
        ed.buffer.select_range(&start, &end);
        ed.view.emit_copy_clipboard();
        let again = clipboard_text(&ed.view);
        assert_eq!(
            again.chars().count(),
            HELD_WINDOW,
            "selecting the window again after Select All copied {} characters",
            again.chars().count()
        );
        assert_ne!(again, body);

        let from = ed.buffer.iter_at_offset(0);
        let to = ed.buffer.iter_at_offset(5);
        ed.buffer.select_range(&from, &to);
        ed.view.emit_copy_clipboard();
        assert_eq!(clipboard_text(&ed.view), "Hello");
        window.close();
    }

    fn clipboard_text(view: &gtk::TextView) -> String {
        let got = Rc::new(RefCell::new(None));
        let slot = Rc::clone(&got);
        view.clipboard()
            .read_text_async(None::<&gtk::gio::Cancellable>, move |result| {
                let text = result
                    .expect("clipboard read")
                    .unwrap_or_default()
                    .to_string();
                *slot.borrow_mut() = Some(text);
            });
        let context = gtk::glib::MainContext::default();
        for _ in 0..50 {
            if got.borrow().is_some() {
                break;
            }
            if !context.iteration(true) {
                break;
            }
        }
        got.borrow().clone().expect("the clipboard did not answer")
    }

    /// A match past the first window moves that window. The selected text is
    /// the file's own characters.
    pub(crate) fn find_in_a_long_held_line_reaches_the_tail() {
        let ed = Editor::new(true);
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&ed.root));
        window.present();
        // Past the breakless cap, and past the first window, so find has to
        // move the bar. A window-sized prefix alone is under the cap.
        let mut body = "a".repeat(rich::SYNC_BREAKLESS_CHARS + HELD_WINDOW);
        body.push_str("TAIL");
        ed.load(RichText::from_plain(&body));
        assert!(ed.is_too_formatted());
        assert!(ed.held_bar.is_visible());
        let (start, end) = ed.buffer.bounds();
        assert!(!ed.buffer.text(&start, &end, false).contains("TAIL"));
        ed.search_entry.set_text("TAIL");
        ed.find(true);
        let (start, end) = ed.buffer.selection_bounds().expect("find missed the tail");
        assert_eq!(ed.buffer.text(&start, &end, false).as_str(), "TAIL");
        assert!(ed.plain_text().ends_with("TAIL"));
        window.close();
    }

    fn presented_editor() -> (Rc<Editor>, gtk::Window) {
        let ed = Editor::new(true);
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&ed.root));
        show_window(&window);
        (ed, window)
    }

    fn file_span(ed: &Editor) -> Option<(usize, usize)> {
        let (s, e) = ed.buffer.selection_bounds()?;
        let origin = if ed.held_bar.is_visible() {
            ed.held_origin.get()
        } else {
            0
        };
        Some((
            origin + text_index(s.offset()),
            origin + text_index(e.offset()),
        ))
    }

    /// `search-changed` calls [`Editor::find_from_start`]. GTK delays that
    /// signal, so the test holds the delay off and calls the handler itself.
    fn type_query(ed: &Editor, query: &str) {
        ed.search_entry.set_search_delay(60_000);
        ed.search_entry.set_text(query);
        ed.find_from_start();
    }

    fn expect_hit(failures: &mut Vec<String>, label: &str, ed: &Editor, start: usize, end: usize) {
        let span = file_span(ed);
        let red = ed.search_entry.has_css_class("error");
        if span != Some((start, end)) || red {
            let insert = text_index(ed.buffer.iter_at_mark(&ed.buffer.get_insert()).offset());
            failures.push(format!(
                "{label}: span {span:?} red={red} origin={} insert={insert} entry={:?}, expected ({start}, {end}) and no red mark",
                ed.held_origin.get(),
                ed.search_entry.text()
            ));
        }
    }

    /// Held Find follows plain Find. Typing and the first Find include the
    /// match at the caret. Find Next steps past a selected match. A wrap
    /// that still finds a match is not red.
    pub(crate) fn held_find_matches_plain_find() {
        let mut failures = Vec::new();
        let prefix = "xx WORD yy WORD zz ";
        let (held, held_window) = presented_editor();
        let (plain, plain_window) = presented_editor();
        held.load(RichText::from_plain(&format!(
            "{prefix}{}",
            "q".repeat(10_000)
        )));
        plain.load(RichText::from_plain(prefix));
        assert!(held.held_bar.is_visible());
        assert!(!plain.held_bar.is_visible());
        for (query, end) in [("W", 4usize), ("WO", 5), ("WOR", 6), ("WORD", 7)] {
            type_query(&held, query);
            expect_hit(&mut failures, &format!("held type {query}"), &held, 3, end);
            type_query(&plain, query);
            expect_hit(
                &mut failures,
                &format!("plain type {query}"),
                &plain,
                3,
                end,
            );
        }

        let (wordword, wordword_window) = presented_editor();
        let (plain_word, plain_word_window) = presented_editor();
        wordword.load(RichText::from_plain(&format!(
            "WORDWORD{}",
            "q".repeat(10_000)
        )));
        plain_word.load(RichText::from_plain("WORDWORD"));
        type_query(&wordword, "WORD");
        expect_hit(&mut failures, "held WORDWORD", &wordword, 0, 4);
        type_query(&plain_word, "WORD");
        expect_hit(&mut failures, "plain WORDWORD", &plain_word, 0, 4);

        let (only, only_window) = presented_editor();
        let (plain_only, plain_only_window) = presented_editor();
        only.load(RichText::from_plain(&format!("WORD{}", "q".repeat(10_000))));
        plain_only.load(RichText::from_plain("WORD"));
        type_query(&only, "WORD");
        expect_hit(&mut failures, "held only match at 0", &only, 0, 4);
        type_query(&plain_only, "WORD");
        expect_hit(&mut failures, "plain only match at 0", &plain_only, 0, 4);
        only.find(true);
        expect_hit(&mut failures, "held wrap onto the only match", &only, 0, 4);
        plain_only.find(true);
        expect_hit(
            &mut failures,
            "plain wrap onto the only match",
            &plain_only,
            0,
            4,
        );

        let mut body = "a".repeat(HELD_WINDOW - 2);
        body.push_str("EDXY");
        body.push_str(&"b".repeat(400));
        body.push_str("EDXY");
        let second = (HELD_WINDOW - 2) + 4 + 400;
        assert_eq!(second, 602);
        let (edge, edge_window) = presented_editor();
        let (plain_edge, plain_edge_window) = presented_editor();
        edge.load(RichText::from_plain(&format!(
            "{body}{}",
            "c".repeat(10_000)
        )));
        plain_edge.load(RichText::from_plain(&body));
        edge.held_bar
            .adjustment()
            .set_value((HELD_WINDOW - 2) as f64);
        assert_eq!(edge.held_origin.get(), HELD_WINDOW - 2);
        let at = plain_edge
            .buffer
            .iter_at_offset(text_offset(HELD_WINDOW - 2));
        plain_edge.buffer.place_cursor(&at);
        type_query(&edge, "EDXY");
        expect_hit(
            &mut failures,
            "held EDXY already at the window start",
            &edge,
            HELD_WINDOW - 2,
            HELD_WINDOW - 2 + 4,
        );
        if edge.held_origin.get() != HELD_WINDOW - 2 {
            failures.push(format!(
                "held Find left the EDXY window at {}, the later match is at {second}",
                edge.held_origin.get()
            ));
        }
        type_query(&plain_edge, "EDXY");
        expect_hit(
            &mut failures,
            "plain EDXY at the caret",
            &plain_edge,
            HELD_WINDOW - 2,
            HELD_WINDOW - 2 + 4,
        );

        let (enter, enter_window) = presented_editor();
        let (plain_enter, plain_enter_window) = presented_editor();
        enter.load(RichText::from_plain(&format!(
            "{prefix}{}",
            "q".repeat(10_000)
        )));
        plain_enter.load(RichText::from_plain(prefix));
        enter.search_entry.set_search_delay(60_000);
        plain_enter.search_entry.set_search_delay(60_000);
        enter.search_entry.set_text("WORD");
        plain_enter.search_entry.set_text("WORD");
        let at = enter.buffer.iter_at_offset(3);
        enter.buffer.place_cursor(&at);
        let at = plain_enter.buffer.iter_at_offset(3);
        plain_enter.buffer.place_cursor(&at);
        enter.search_entry.emit_activate();
        expect_hit(&mut failures, "held Enter on the caret", &enter, 3, 7);
        plain_enter.search_entry.emit_activate();
        expect_hit(
            &mut failures,
            "plain Enter on the caret",
            &plain_enter,
            3,
            7,
        );

        let (ctrl_f, ctrl_f_window) = presented_editor();
        let (plain_ctrl, plain_ctrl_window) = presented_editor();
        ctrl_f.load(RichText::from_plain(&format!(
            "{prefix}{}",
            "q".repeat(10_000)
        )));
        plain_ctrl.load(RichText::from_plain(prefix));
        ctrl_f.search_entry.set_search_delay(60_000);
        plain_ctrl.search_entry.set_search_delay(60_000);
        ctrl_f.search_entry.set_text("");
        plain_ctrl.search_entry.set_text("");
        let s = ctrl_f.buffer.iter_at_offset(3);
        let e = ctrl_f.buffer.iter_at_offset(7);
        ctrl_f.buffer.select_range(&s, &e);
        ctrl_f.show_find();
        ctrl_f.find_from_start();
        expect_hit(&mut failures, "held Ctrl+F on a selection", &ctrl_f, 3, 7);
        let s = plain_ctrl.buffer.iter_at_offset(3);
        let e = plain_ctrl.buffer.iter_at_offset(7);
        plain_ctrl.buffer.select_range(&s, &e);
        plain_ctrl.show_find();
        plain_ctrl.find_from_start();
        expect_hit(
            &mut failures,
            "plain Ctrl+F on a selection",
            &plain_ctrl,
            3,
            7,
        );

        let (back, back_window) = presented_editor();
        let (plain_back, plain_back_window) = presented_editor();
        back.load(RichText::from_plain(&format!(
            "aaaaaa{}",
            "q".repeat(10_000)
        )));
        plain_back.load(RichText::from_plain("aaaaaa"));
        back.search_entry.set_search_delay(60_000);
        plain_back.search_entry.set_search_delay(60_000);
        back.search_entry.set_text("aa");
        plain_back.search_entry.set_text("aa");
        let at = back.buffer.iter_at_offset(2);
        back.buffer.place_cursor(&at);
        let at = plain_back.buffer.iter_at_offset(2);
        plain_back.buffer.place_cursor(&at);
        for (n, (start, end)) in [(0usize, 2usize), (4, 6), (2, 4), (0, 2)]
            .into_iter()
            .enumerate()
        {
            back.find(false);
            expect_hit(
                &mut failures,
                &format!("held aa previous {n}"),
                &back,
                start,
                end,
            );
            plain_back.find(false);
            expect_hit(
                &mut failures,
                &format!("plain aa previous {n}"),
                &plain_back,
                start,
                end,
            );
        }

        let needle = "N".repeat(250);
        let (run, run_window) = presented_editor();
        let (plain_run, plain_run_window) = presented_editor();
        run.load(RichText::from_plain(&"N".repeat(10_000)));
        plain_run.load(RichText::from_plain(&"N".repeat(1_000)));
        run.search_entry.set_search_delay(60_000);
        plain_run.search_entry.set_search_delay(60_000);
        run.search_entry.set_text(&needle);
        plain_run.search_entry.set_text(&needle);
        plain_run.find(true);
        let plain_first = file_span(&plain_run);
        if plain_first != Some((0, 250)) {
            failures.push(format!("plain 250-N first hit: span {plain_first:?}"));
        }
        run.find(true);
        if run.held_origin.get() != 0 || run.search_entry.has_css_class("error") {
            failures.push(format!(
                "held 250-N first hit: origin {} red={}",
                run.held_origin.get(),
                run.search_entry.has_css_class("error")
            ));
        }
        run.find(true);
        if run.held_origin.get() != 250 || run.search_entry.has_css_class("error") {
            failures.push(format!(
                "held 250-N next: origin {} red={}",
                run.held_origin.get(),
                run.search_entry.has_css_class("error")
            ));
        }
        run.find(false);
        if run.held_origin.get() != 0 || run.search_entry.has_css_class("error") {
            failures.push(format!(
                "held 250-N previous: origin {} red={} (a one-character step stays inside the run)",
                run.held_origin.get(),
                run.search_entry.has_css_class("error")
            ));
        }
        plain_run.find(false);
        if file_span(&plain_run).is_none() {
            failures.push("plain 250-N previous cleared the selection".into());
        }

        for window in [
            held_window,
            plain_window,
            wordword_window,
            plain_word_window,
            only_window,
            plain_only_window,
            edge_window,
            plain_edge_window,
            enter_window,
            plain_enter_window,
            ctrl_f_window,
            plain_ctrl_window,
            back_window,
            plain_back_window,
            run_window,
            plain_run_window,
        ] {
            window.close();
        }
        if !failures.is_empty() {
            panic!("{}", failures.join("\n"));
        }
    }

    /// The context menu's Select All is the `selection.select-all` action.
    /// Copy after it takes the held file, as the keyboard shortcut does.
    pub(crate) fn held_menu_select_all_copies_the_document() {
        let (ed, window) = presented_editor();
        let body = format!("Hello {}", "a".repeat(10_000));
        ed.load(RichText::from_plain(&body));
        ed.view.grab_focus();
        ed.view
            .activate_action("selection.select-all", None)
            .expect("the text view menu Select All action");
        ed.view
            .activate_action("clipboard.copy", None)
            .expect("clipboard.copy");
        ed.view.emit_copy_clipboard();
        let copied = clipboard_text(&ed.view);
        assert_eq!(
            copied.chars().count(),
            body.chars().count(),
            "menu Select All copied {} characters of the window",
            copied.chars().count()
        );
        assert_eq!(copied, body);
        let (start, end) = ed.buffer.bounds();
        ed.buffer.select_range(&start, &end);
        ed.view.emit_copy_clipboard();
        assert_eq!(
            clipboard_text(&ed.view).chars().count(),
            HELD_WINDOW,
            "a hand selection after menu Select All copied the file"
        );
        window.close();
    }

    /// Cut on a held document copies what Copy would copy and leaves the
    /// window and the file text in place.
    pub(crate) fn held_cut_leaves_the_document() {
        let (ed, window) = presented_editor();
        let body = format!("Hello {}", "a".repeat(10_000));
        ed.load(RichText::from_plain(&body));
        let before = buffer_string(&ed);
        let model = ed.plain_text();
        ed.view.grab_focus();
        ed.view.emit_select_all(true);
        ed.view.emit_cut_clipboard();
        assert_eq!(buffer_string(&ed), before, "Cut deleted the held window");
        assert_eq!(ed.plain_text(), model, "Cut changed the held file text");
        let copied = clipboard_text(&ed.view);
        assert_eq!(
            copied.chars().count(),
            body.chars().count(),
            "Cut after Select All copied {} characters of the window",
            copied.chars().count()
        );
        assert_eq!(copied, body);
        let from = ed.buffer.iter_at_offset(0);
        let to = ed.buffer.iter_at_offset(5);
        ed.buffer.select_range(&from, &to);
        ed.view.emit_cut_clipboard();
        assert_eq!(buffer_string(&ed), before, "Cut deleted a held selection");
        assert_eq!(
            ed.plain_text(),
            model,
            "Cut of a selection changed the file"
        );
        assert_eq!(clipboard_text(&ed.view), "Hello");
        let _ = ed.view.activate_action("clipboard.cut", None);
        assert_eq!(
            buffer_string(&ed),
            before,
            "menu Cut deleted the held window"
        );
        assert_eq!(
            ed.plain_text(),
            model,
            "menu Cut changed the held file text"
        );
        window.close();
    }

    /// A short match in the last page stays on the scrollbar's last legal
    /// window. The bar's value is that window's character origin.
    pub(crate) fn held_find_in_the_last_page_stays_on_the_bar() {
        let (ed, window) = presented_editor();
        let total = 10_050usize;
        let mut body = "a".repeat(total - 4);
        body.push_str("TAIL");
        assert_eq!(body.chars().count(), total);
        let at = total - 4;
        ed.load(RichText::from_plain(&body));
        ed.search_entry.set_text("TAIL");
        ed.find(true);
        let max = total - HELD_WINDOW;
        let origin = ed.held_origin.get();
        let bar = ed.held_bar.adjustment().value();
        assert!(
            origin <= max,
            "find parked the window at {origin}, past the bar maximum {max}"
        );
        assert!(
            (bar - origin as f64).abs() < f64::EPSILON,
            "the bar sat at {bar} while the window origin was {origin}"
        );
        let (start, end) = ed.buffer.selection_bounds().expect("find missed TAIL");
        assert_eq!(ed.buffer.text(&start, &end, false).as_str(), "TAIL");
        assert_eq!(file_span(&ed), Some((at, at + 4)));
        let (buf_start, buf_end) = ed.buffer.bounds();
        assert!(
            ed.buffer.text(&buf_start, &buf_end, false).contains("TAIL"),
            "TAIL was selected from a window that does not show it"
        );
        window.close();
    }

    /// One open, timed from just before `load` and `present` until the
    /// first after-paint. `role` is `plain` or `held`.
    fn cold_open_ms(role: &str) -> f64 {
        let sentence = "The lamp at the point stays lit.";
        assert_eq!(sentence.chars().count(), 32);
        let text = match role {
            "plain" => RichText::from_plain(sentence),
            "held" => one_line(10_000),
            other => panic!("unknown cold role {other}"),
        };
        let ed = Editor::new(true);
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&ed.root));
        assert!(!window.is_mapped(), "a fresh window was already on screen");
        let elapsed = Rc::new(Cell::new(f64::NAN));
        let started = Rc::new(RefCell::new(None::<std::time::Instant>));
        let elapsed_flag = Rc::clone(&elapsed);
        let started_flag = Rc::clone(&started);
        ed.view.connect_realize(move |view| {
            let Some(clock) = view.frame_clock() else {
                return;
            };
            let elapsed_flag = Rc::clone(&elapsed_flag);
            let started_flag = Rc::clone(&started_flag);
            clock.connect_after_paint(move |_| {
                let borrowed = started_flag.borrow();
                let Some(at) = borrowed.as_ref() else {
                    return;
                };
                if elapsed_flag.get().is_nan() {
                    elapsed_flag.set(at.elapsed().as_secs_f64() * 1000.0);
                }
            });
            clock.request_phase(gtk::gdk::FrameClockPhase::AFTER_PAINT);
        });
        *started.borrow_mut() = Some(std::time::Instant::now());
        ed.load(text);
        window.present();
        let context = gtk::glib::MainContext::default();
        for _ in 0..90 {
            if elapsed.get().is_finite() {
                break;
            }
            if !context.iteration(true) {
                break;
            }
        }
        let ms = elapsed.get();
        assert!(ms.is_finite(), "{role} cold open did not finish a paint");
        if role == "held" {
            assert!(ed.is_too_formatted());
            assert_eq!(buffer_string(&ed).chars().count(), HELD_WINDOW);
        } else {
            assert!(!ed.is_too_formatted());
            assert_eq!(buffer_string(&ed), sentence);
        }
        window.close();
        for _ in 0..5 {
            if !context.pending() {
                break;
            }
            if !context.iteration(false) {
                break;
            }
        }
        ms
    }

    /// The first frame pays for GTK setup whichever document opens it.
    /// A discarded plain window takes that cost. Each pair then opens
    /// both documents here, alternating which one is first. One pair may
    /// sit on a noisy frame. A second pair at or over 20 ms is the held
    /// open. 20 ms stays the bound.
    fn held_cold_open_matches_a_short_sentence() {
        const PAIRS: usize = 7;
        let _warmup = cold_open_ms("plain");
        let load = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
        let load = load.split_whitespace().next().unwrap_or("?");
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let mut deltas = [0.0; PAIRS];
        let mut lines = Vec::with_capacity(PAIRS);
        for (i, delta_slot) in deltas.iter_mut().enumerate() {
            let held_first = i % 2 == 1;
            let (plain_ms, held_ms) = if held_first {
                let held_ms = cold_open_ms("held");
                let plain_ms = cold_open_ms("plain");
                (plain_ms, held_ms)
            } else {
                let plain_ms = cold_open_ms("plain");
                let held_ms = cold_open_ms("held");
                (plain_ms, held_ms)
            };
            let delta = held_ms - plain_ms;
            *delta_slot = delta;
            let first = if held_first { "held" } else { "plain" };
            lines.push(format!(
                "gtk cold pair={i} first={first} plain_ms={plain_ms:.1} held_ms={held_ms:.1} delta_ms={delta:.1}"
            ));
        }
        let over = deltas.iter().filter(|delta| **delta >= 20.0).count();
        let min_delta = deltas.iter().copied().fold(f64::INFINITY, f64::min);
        let max_delta = deltas.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        lines.push(format!(
            "gtk cold pairs={PAIRS} over_20={over} min_delta_ms={min_delta:.1} max_delta_ms={max_delta:.1} load={load} cores={cores}"
        ));
        for line in &lines {
            report_line(line);
        }
        assert!(
            over < 2,
            "held cold open was at least 20 ms slower than a 32-character sentence on {over} pairs\n{}",
            lines.join("\n")
        );
    }

    /// `cargo test` captures a passing test's stdout. The parent of this
    /// process is that harness, and its stdout is the log that keeps the
    /// cold numbers.
    fn report_line(line: &str) {
        eprintln!("{line}");
        let Some(ppid) = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status.lines().find_map(|row| {
                    row.strip_prefix("PPid:")
                        .map(|rest| rest.trim().to_string())
                })
            })
        else {
            return;
        };
        let Ok(mut out) = std::fs::OpenOptions::new()
            .write(true)
            .open(format!("/proc/{ppid}/fd/1"))
        else {
            return;
        };
        let _ = std::io::Write::write_all(&mut out, line.as_bytes());
        let _ = std::io::Write::write_all(&mut out, b"\n");
        let _ = std::io::Write::flush(&mut out);
    }

    /// Open times for one paragraph. Uniform runs, alternating runs, and
    /// one unspaced line, at 10k, 50k, and 100k. Each page has its own
    /// letters and its own tags, so every timed load and every scroll
    /// writes the buffer. Those opens are warm: the window is already
    /// painted, and each timer stops on after-paint.
    /// Set OMAQUILL_GTK_OPEN to one of those counts to time only that size.
    /// Set it to `probe` to time word-char layout under the cap. A held
    /// paragraph stays one buffer line and wraps on characters.
    pub(crate) fn time_styled_paragraph_open() {
        let which = std::env::var("OMAQUILL_GTK_OPEN").unwrap_or_default();
        let ed = Editor::new(true);
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&ed.root));
        let context = gtk::glib::MainContext::default();
        if which == "probe" {
            show_window(&window);
            assert!(window.is_mapped(), "the window did not map");
            paint_editor(&ed);
            probe_word_char(&ed, &context);
            window.close();
            return;
        }
        held_cold_open_matches_a_short_sentence();
        show_window(&window);
        assert!(window.is_mapped(), "the warm window did not map");
        paint_editor(&ed);
        let sizes: Vec<usize> = if which.is_empty() || which == "1" {
            vec![10_000, 50_000, 100_000]
        } else if let Ok(n) = which.parse() {
            vec![n]
        } else {
            vec![10_000, 50_000, 100_000]
        };
        let mut seen = std::collections::HashSet::new();
        for (size_tag, chars) in sizes.iter().copied().enumerate() {
            for (kind, label) in [(0u8, "uniform"), (1, "alternating"), (2, "line")] {
                let text = timed_document(chars, kind, size_tag);
                let runs = &text.paragraphs[0].runs;
                if chars > HELD_WINDOW {
                    assert!(
                        runs.len() >= 2,
                        "{label} at {chars} has one page, so the scroll does not write"
                    );
                    assert!(
                        runs.windows(2).all(|pair| pair[0].style != pair[1].style),
                        "{label} at {chars} reuses a tag run on the next page"
                    );
                }
                let letters: Vec<char> = text.plain_text().chars().collect();
                let mut offset = 0usize;
                let mut page = 0usize;
                while offset < letters.len() {
                    let end = (offset + HELD_WINDOW).min(letters.len());
                    let slice: String = letters[offset..end].iter().collect();
                    assert!(
                        seen.insert(slice),
                        "{label} at {chars} page {page} repeats an earlier window"
                    );
                    offset = end;
                    page += 1;
                }
                let name = format!("{label} {chars}");
                time_open(&ed, &context, &name, &text);
            }
        }
        // Three shapes and three lengths. Every page is its own window.
        if sizes == [10_000, 50_000, 100_000] {
            assert_eq!(seen.len(), 2_400, "timed {} distinct pages", seen.len());
        }
        window.close();
    }
}

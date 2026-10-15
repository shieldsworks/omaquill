//! Text that another program pastes or drops, read with a size cap and a
//! deadline.
//!
//! GTK reads a foreign clipboard or drop into one growing buffer until the
//! other program closes its end, and only then hands the text to the
//! widget. [`install`] guards every `GtkText` and `GtkTextView` in every
//! window: a paste, a middle-click paste, or a drop from another program is
//! read by [`read`] instead. A clipboard this process owns and a drag that
//! started here stay on GTK's own path, which copies the value in memory
//! without a stream.

use super::rich;
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::time::Duration;

/// Bytes read from another program for one paste or drop. A mebibyte of
/// prose laid out in a visible text view in 0.9 s on xvfb, GTK 4.18.6.
pub(crate) const LIMIT: usize = 1 << 20;

/// Time one paste or drop may take, from the request to the last byte.
pub(crate) const DEADLINE: Duration = Duration::from_secs(5);

/// Bytes asked of the stream per read.
const CHUNK: usize = 64 * 1024;

/// The formats GTK's own string reader accepts first.
const MIME_TYPES: [&str; 2] = ["text/plain;charset=utf-8", "text/plain"];

thread_local! {
    /// One foreign read at a time, so held-down Ctrl+V cannot stack reads.
    static READING: Cell<bool> = const { Cell::new(false) };
    static GUARDED: RefCell<Vec<glib::WeakRef<gtk::Widget>>> = const { RefCell::new(Vec::new()) };
}

/// Guards every editable in each window when it maps, when it presents a
/// dialog, and whenever its focus moves. That covers the windows GTK opens
/// itself, such as the font chooser, and widgets added after the window maps.
pub fn install() {
    let toplevels = gtk::Window::toplevels();
    let watch = |item: Option<glib::Object>| {
        let Some(window) = item.and_downcast::<gtk::Window>() else {
            return;
        };
        window.connect_map(|w| guard(w.upcast_ref()));
        window.connect_focus_widget_notify(|w| guard(w.upcast_ref()));
        if let Some(w) = window.downcast_ref::<adw::ApplicationWindow>() {
            w.connect_visible_dialog_notify(|w| guard(w.upcast_ref()));
        } else if let Some(w) = window.downcast_ref::<adw::Window>() {
            w.connect_visible_dialog_notify(|w| guard(w.upcast_ref()));
        }
        if window.is_mapped() {
            guard(window.upcast_ref());
        }
    };
    for i in 0..toplevels.n_items() {
        watch(toplevels.item(i));
    }
    toplevels.connect_items_changed(move |list, position, _removed, added| {
        for i in position..position + added {
            watch(list.item(i));
        }
    });
}

/// Routes every `GtkText` and `GtkTextView` under `root` through [`read`].
/// A widget already guarded is left as it is.
pub(crate) fn guard(root: &gtk::Widget) {
    let mut stack = vec![root.clone()];
    while let Some(widget) = stack.pop() {
        if (widget.is::<gtk::Text>() || widget.is::<gtk::TextView>()) && !is_guarded(&widget) {
            guard_one(&widget);
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            stack.push(c);
        }
    }
}

pub(crate) fn is_guarded(widget: &gtk::Widget) -> bool {
    GUARDED.with_borrow_mut(|list| {
        list.retain(|w| w.upgrade().is_some());
        list.iter().any(|w| w.upgrade().as_ref() == Some(widget))
    })
}

/// Where the text goes when it arrives.
enum At {
    /// The caret, replacing the selection.
    Cursor,
    /// A text view point that a mark holds while the read runs.
    Mark(gtk::TextMark),
}

fn guard_one(widget: &gtk::Widget) {
    GUARDED.with_borrow_mut(|list| list.push(widget.downgrade()));

    // Ctrl+V, Shift+Insert, the Paste key, and the context menu's Paste
    // all emit this signal.
    widget.connect_closure(
        "paste-clipboard",
        false,
        glib::closure_local!(move |widget: gtk::Widget| {
            let clipboard = widget.clipboard();
            if clipboard.is_local() && !local_too_long(&widget, &clipboard) {
                return;
            }
            widget.stop_signal_emission_by_name("paste-clipboard");
            if clipboard.is_local() {
                widget.error_bell();
                return;
            }
            start(
                &widget,
                At::Cursor,
                clipboard.read_future(&MIME_TYPES, glib::Priority::DEFAULT),
                None,
            );
        }),
    );

    // GTK pastes the primary selection from its own click gesture, without
    // a signal, so this one claims the press first. It claims a double
    // click's second press too: GTK's gesture never saw the first, so it
    // would take the second for a first and paste.
    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_MIDDLE);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    click.connect_pressed(|gesture, n_press, x, y| {
        let Some(widget) = gesture.widget() else {
            return;
        };
        let clipboard = widget.primary_clipboard();
        if !widget.settings().is_gtk_enable_primary_paste() {
            return;
        }
        if clipboard.is_local() && !local_too_long(&widget, &clipboard) {
            return;
        }
        gesture.set_state(gtk::EventSequenceState::Claimed);
        if n_press != 1 {
            return;
        }
        if clipboard.is_local() || !editable(&widget) {
            widget.error_bell();
            return;
        }
        let at = at_point(&widget, x, y);
        start(
            &widget,
            at,
            clipboard.read_future(&MIME_TYPES, glib::Priority::DEFAULT),
            None,
        );
    });
    widget.add_controller(click);

    // GTK's own drop target reads the whole offer before its drop handler
    // runs. This one answers a drop from another program first.
    let target = gtk::DropTargetAsync::new(None, gdk::DragAction::COPY);
    target.set_propagation_phase(gtk::PropagationPhase::Capture);
    target.connect_accept(|target, drop| {
        drop.drag().is_none()
            && drop.formats().contains_type(glib::Type::STRING)
            && target.widget().is_some_and(|w| editable(&w))
    });
    target.connect_drag_enter(|_, _, _, _| gdk::DragAction::COPY);
    target.connect_drag_motion(|_, _, _, _| gdk::DragAction::COPY);
    target.connect_drop(|target, drop, x, y| {
        let Some(widget) = target.widget() else {
            drop.finish(gdk::DragAction::empty());
            return true;
        };
        let at = at_point(&widget, x, y);
        let read = drop.read_future(&MIME_TYPES, glib::Priority::DEFAULT);
        start(&widget, at, read, Some(drop.clone()));
        true
    });
    widget.add_controller(target);
}

fn editable(widget: &gtk::Widget) -> bool {
    if let Some(text) = widget.downcast_ref::<gtk::Text>() {
        text.is_editable()
    } else if let Some(view) = widget.downcast_ref::<gtk::TextView>() {
        view.is_editable()
    } else {
        false
    }
}

/// A text view lays out an unbreakable run in time that grows with the
/// square of its length. Text this process put on the clipboard as a string
/// meets the same limit as text read from another program.
fn local_too_long(widget: &gtk::Widget, clipboard: &gdk::Clipboard) -> bool {
    widget.is::<gtk::TextView>()
        && clipboard
            .content()
            .and_then(|c| c.value(glib::Type::STRING).ok())
            .and_then(|v| v.get::<String>().ok())
            .is_some_and(|text| rich::breakless_len(text.chars()) > rich::SYNC_BREAKLESS_CHARS)
}

fn at_point(widget: &gtk::Widget, x: f64, y: f64) -> At {
    let Some(view) = widget.downcast_ref::<gtk::TextView>() else {
        return At::Cursor;
    };
    // Coordinates are inside the widget, so they fit in i32.
    let (bx, by) = view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
    let buffer = view.buffer();
    let iter = view
        .iter_at_location(bx, by)
        .unwrap_or_else(|| buffer.end_iter());
    At::Mark(buffer.create_mark(None, &iter, true))
}

fn start(
    widget: &gtk::Widget,
    at: At,
    open: impl Future<Output = Result<(gio::InputStream, glib::GString), glib::Error>> + 'static,
    drop: Option<gdk::Drop>,
) {
    if READING.get() {
        finish(widget, &at, None, drop.as_ref());
        return;
    }
    READING.set(true);
    let weak = widget.downgrade();
    glib::spawn_future_local(async move {
        let text = read(open, LIMIT, DEADLINE).await;
        READING.set(false);
        if let Some(widget) = weak.upgrade() {
            finish(&widget, &at, text.as_deref(), drop.as_ref());
        } else if let Some(drop) = drop {
            drop.finish(gdk::DragAction::empty());
        }
    });
}

/// Opens the stream with `open` and reads it to the end. `None` when the
/// stream fails, sends more than `limit` bytes, or is not done within
/// `deadline`. A NUL ends the text, as it does in GTK's string reader.
pub(crate) async fn read(
    open: impl Future<Output = Result<(gio::InputStream, glib::GString), glib::Error>>,
    limit: usize,
    deadline: Duration,
) -> Option<String> {
    let all = async {
        let (stream, _) = open.await.ok()?;
        let mut bytes = Vec::new();
        loop {
            let want = (limit - bytes.len()).saturating_add(1).min(CHUNK);
            let chunk = stream
                .read_bytes_future(want, glib::Priority::DEFAULT)
                .await
                .ok()?;
            if chunk.is_empty() {
                break;
            }
            if chunk.len() > limit - bytes.len() {
                return None;
            }
            bytes.extend_from_slice(&chunk);
        }
        if let Some(nul) = bytes.iter().position(|&b| b == 0) {
            bytes.truncate(nul);
        }
        Some(String::from_utf8_lossy(&bytes).into_owned())
    };
    glib::future_with_timeout(deadline, all)
        .await
        .ok()
        .flatten()
}

fn finish(widget: &gtk::Widget, at: &At, text: Option<&str>, drop: Option<&gdk::Drop>) {
    let inserted = text.is_some_and(|text| insert(widget, at, text));
    if let At::Mark(mark) = at
        && let Some(buffer) = mark.buffer()
    {
        buffer.delete_mark(mark);
    }
    if let Some(drop) = drop {
        drop.finish(if inserted {
            gdk::DragAction::COPY
        } else {
            gdk::DragAction::empty()
        });
    }
    if !inserted {
        widget.error_bell();
    }
}

fn insert(widget: &gtk::Widget, at: &At, text: &str) -> bool {
    if let Some(entry) = widget.downcast_ref::<gtk::Text>() {
        if !entry.is_editable() {
            return false;
        }
        let text = if entry.must_truncate_multiline() {
            text.split(['\n', '\r']).next().unwrap_or_default()
        } else {
            text
        };
        entry.delete_selection();
        let mut pos = entry.position();
        entry.insert_text(text, &mut pos);
        entry.set_position(pos);
        return true;
    }
    let Some(view) = widget.downcast_ref::<gtk::TextView>() else {
        return false;
    };
    if !view.is_editable() || rich::breakless_len(text.chars()) > rich::SYNC_BREAKLESS_CHARS {
        return false;
    }
    let buffer = view.buffer();
    buffer.begin_user_action();
    let inserted = match at {
        At::Cursor => {
            buffer.delete_selection(true, true);
            buffer.insert_interactive_at_cursor(text, true)
        }
        At::Mark(mark) => {
            let mut iter = buffer.iter_at_mark(mark);
            let inserted = buffer.insert_interactive(&mut iter, text, true);
            buffer.place_cursor(&iter);
            inserted
        }
    };
    buffer.end_user_action();
    view.scroll_mark_onscreen(&buffer.get_insert());
    inserted
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use gtk::subclass::prelude::*;
    use std::pin::Pin;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    /// A stream that sends `byte` forever, one `delay` per read, and counts
    /// what it sent.
    mod flood {
        use super::*;

        #[derive(Default)]
        pub struct Flood {
            pub sent: Arc<AtomicUsize>,
            pub delay: Cell<Duration>,
        }

        #[glib::object_subclass]
        impl ObjectSubclass for Flood {
            const NAME: &'static str = "OmaquillTestFlood";
            type Type = super::FloodStream;
            type ParentType = gio::InputStream;
        }

        impl ObjectImpl for Flood {}

        impl InputStreamImpl for Flood {
            fn read(
                &self,
                buffer: &mut [u8],
                _cancellable: Option<&gio::Cancellable>,
            ) -> Result<usize, glib::Error> {
                std::thread::sleep(self.delay.get());
                buffer.fill(b'a');
                self.sent.fetch_add(buffer.len(), Ordering::SeqCst);
                Ok(buffer.len())
            }
        }
    }

    glib::wrapper! {
        pub struct FloodStream(ObjectSubclass<flood::Flood>) @extends gio::InputStream;
    }

    fn flood(delay: Duration) -> (FloodStream, Arc<AtomicUsize>) {
        let stream: FloodStream = glib::Object::new();
        stream.imp().delay.set(delay);
        let sent = Arc::clone(&stream.imp().sent);
        (stream, sent)
    }

    fn opened(
        stream: impl IsA<gio::InputStream>,
    ) -> impl Future<Output = Result<(gio::InputStream, glib::GString), glib::Error>> {
        let stream = stream.upcast();
        async move { Ok((stream, glib::GString::from("text/plain;charset=utf-8"))) }
    }

    fn memory(bytes: &[u8]) -> gio::MemoryInputStream {
        gio::MemoryInputStream::from_bytes(&glib::Bytes::from(bytes))
    }

    fn block_on<T>(f: impl Future<Output = T>) -> T {
        glib::MainContext::new().block_on(f)
    }

    #[test]
    fn text_up_to_the_limit_is_read() {
        let got = block_on(read(opened(memory(b"find me")), 7, DEADLINE));
        assert_eq!(got.as_deref(), Some("find me"));
    }

    #[test]
    fn one_byte_past_the_limit_is_refused() {
        let got = block_on(read(opened(memory(b"find me!")), 7, DEADLINE));
        assert_eq!(got, None);
    }

    #[test]
    fn a_nul_ends_the_text() {
        let got = block_on(read(opened(memory(b"find\0me")), 64, DEADLINE));
        assert_eq!(got.as_deref(), Some("find"));
    }

    #[test]
    fn an_endless_stream_stops_at_the_limit() {
        let (stream, sent) = flood(Duration::ZERO);
        let got = block_on(read(opened(stream), LIMIT, DEADLINE));
        assert_eq!(got, None);
        let sent = sent.load(Ordering::SeqCst);
        assert!(
            sent <= LIMIT + 1,
            "read {sent} bytes for a {LIMIT}-byte limit"
        );
    }

    #[test]
    fn a_slow_stream_stops_at_the_deadline() {
        let (stream, sent) = flood(Duration::from_millis(20));
        let started = Instant::now();
        let got = block_on(read(opened(stream), LIMIT, Duration::from_millis(200)));
        let took = started.elapsed();
        assert_eq!(got, None);
        assert!(took < Duration::from_secs(2), "read for {took:?}");
        assert!(sent.load(Ordering::SeqCst) < LIMIT);
    }

    #[test]
    fn a_provider_that_never_answers_stops_at_the_deadline() {
        let open = std::future::pending::<Result<(gio::InputStream, glib::GString), glib::Error>>();
        let started = Instant::now();
        let got = block_on(read(open, LIMIT, Duration::from_millis(200)));
        assert_eq!(got, None);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// Clipboard content served by another display connection, which GTK
    /// treats as another program: `total` bytes of `a`, or no end.
    mod provider {
        use super::*;

        #[derive(Default)]
        pub struct Provider {
            pub total: Cell<Option<usize>>,
            pub sent: Rc<Cell<usize>>,
        }

        #[glib::object_subclass]
        impl ObjectSubclass for Provider {
            const NAME: &'static str = "OmaquillTestProvider";
            type Type = super::Provider;
            type ParentType = gdk::ContentProvider;
        }

        impl ObjectImpl for Provider {}

        impl ContentProviderImpl for Provider {
            fn formats(&self) -> gdk::ContentFormats {
                gdk::ContentFormats::new(&["text/plain;charset=utf-8"])
            }

            fn write_mime_type_future(
                &self,
                _mime_type: &str,
                stream: &gio::OutputStream,
                io_priority: glib::Priority,
            ) -> Pin<Box<dyn Future<Output = Result<(), glib::Error>> + 'static>> {
                let stream = stream.clone();
                let total = self.total.get();
                let sent = Rc::clone(&self.sent);
                Box::pin(async move {
                    loop {
                        let left = total.map_or(CHUNK, |t| t - sent.get()).min(CHUNK);
                        if left == 0 {
                            break;
                        }
                        stream
                            .write_all_future(vec![b'a'; left], io_priority)
                            .await
                            .map_err(|(_, e)| e)?;
                        sent.set(sent.get() + left);
                    }
                    stream.close_future(io_priority).await
                })
            }
        }
    }

    glib::wrapper! {
        pub struct Provider(ObjectSubclass<provider::Provider>) @extends gdk::ContentProvider;
    }

    fn pump_until(mut done: impl FnMut() -> bool, limit: Duration) {
        let context = glib::MainContext::default();
        let end = Instant::now() + limit;
        while !done() && Instant::now() < end {
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Puts `total` bytes, or an endless stream, on the clipboard from a
    /// second connection to the display. Returns that display, which must
    /// outlive the paste, and the count of bytes it sent.
    fn foreign_clipboard(total: Option<usize>) -> Option<(gdk::Display, Rc<Cell<usize>>)> {
        let ours = gdk::Display::default()?;
        let other = gdk::Display::open(Some(&ours.name()))?;
        let provider: Provider = glib::Object::new();
        provider.imp().total.set(total);
        let sent = Rc::clone(&provider.imp().sent);
        other
            .clipboard()
            .set_content(Some(provider.upcast_ref::<gdk::ContentProvider>()))
            .ok()?;
        let clipboard = ours.clipboard();
        pump_until(
            || !clipboard.is_local() && clipboard.formats().contains_type(glib::Type::STRING),
            Duration::from_secs(2),
        );
        (!clipboard.is_local()).then_some((other, sent))
    }

    fn find_field(ed: &super::super::Editor) -> gtk::Text {
        let mut stack = vec![ed.root.clone().upcast::<gtk::Widget>()];
        while let Some(w) = stack.pop() {
            if let Ok(text) = w.clone().downcast::<gtk::Text>() {
                return text;
            }
            let mut child = w.first_child();
            while let Some(c) = child {
                child = c.next_sibling();
                stack.push(c);
            }
        }
        panic!("the editor has no Find field");
    }

    fn paste_into(text: &gtk::Text, by_menu: bool) {
        if by_menu {
            text.activate_action("clipboard.paste", None)
                .expect("clipboard.paste");
        } else {
            text.emit_by_name::<()>("paste-clipboard", &[]);
        }
        pump_until(|| !READING.get(), DEADLINE + Duration::from_secs(1));
    }

    pub(crate) fn an_oversized_foreign_paste_into_find_is_refused() {
        let ed = super::super::Editor::new(true);
        guard(ed.root.upcast_ref());
        let find = find_field(&ed);
        let Some((other, _)) = foreign_clipboard(Some(2 * LIMIT)) else {
            eprintln!("no second display connection; foreign paste check skipped");
            return;
        };
        let started = Instant::now();
        paste_into(&find, false);
        assert!(
            started.elapsed() < DEADLINE,
            "the paste ran to the deadline"
        );
        // GTK's own read of the same offer finishes within this time.
        pump_until(|| false, Duration::from_secs(1));
        assert_eq!(find.text(), "", "the oversized paste reached the field");
        other.close();
    }

    pub(crate) fn a_foreign_paste_under_the_limit_reaches_find_and_the_editor() {
        let ed = super::super::Editor::new(true);
        guard(ed.root.upcast_ref());
        let find = find_field(&ed);
        let Some((other, _)) = foreign_clipboard(Some(60_000)) else {
            eprintln!("no second display connection; foreign paste check skipped");
            return;
        };
        paste_into(&find, true);
        assert_eq!(find.text().len(), 60_000);
        other.close();
        let (other, _) = foreign_clipboard(Some(8_000)).expect("the clipboard answered once");
        ed.view.emit_paste_clipboard();
        pump_until(|| !READING.get(), DEADLINE + Duration::from_secs(1));
        assert_eq!(ed.buffer.char_count(), 8_000);
        other.close();
        // 300,000 characters with no break would freeze text view layout.
        let (other, _) = foreign_clipboard(Some(300_000)).expect("the clipboard answered twice");
        ed.view.emit_paste_clipboard();
        pump_until(|| !READING.get(), DEADLINE + Duration::from_secs(1));
        assert_eq!(
            ed.buffer.char_count(),
            8_000,
            "the unbroken paste reached the editor"
        );
        other.close();
    }
}

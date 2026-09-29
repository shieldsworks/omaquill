# omaquill

A writing studio for [Omarchy](https://omarchy.org) that opens your
Scrivener projects.

omaquill reads and writes Scrivener 3's `.scriv` format directly. Point it at
the project you've been writing in on the Mac and keep going: the binder,
your chapters and scenes, synopses, notes, labels and status are all there,
and what you write is saved back into the same project. There's no import
step and no second copy.

It's written from scratch in Rust with GTK 4 and libadwaita. The Scrivener
reader and writer, the RTF engine, and the DOCX and EPUB writers are omaquill's
own, so the only dependencies are GTK and libadwaita. The Omarchy theme
colors the window and follows theme changes live.

**Status: v0.1.** It's for writing. Most of Scrivener's Compile, and its
snapshots, comments and footnotes, aren't here yet. See
[What carries over](#what-carries-over).

## Install

On Omarchy (or any Arch system with GTK 4.12+ and libadwaita 1.5+):

```sh
git clone https://github.com/shieldsworks/omaquill ~/.local/share/omaquill/src
cd ~/.local/share/omaquill/src
bash scripts/install-local.sh --plugin
```

That builds a release (you need Rust: `mise install` in the checkout gets
the right one), puts `omaquill` in `~/.local/bin`, and adds it to the app
launcher with its icon. `.scrivx` files open in it, and `--plugin` adds the
bar widget. There's also an AUR-style `packaging/PKGBUILD`.

For text that looks like it did on the Mac, install the TeX Gyre fonts. They
closely match Palatino, Times, Helvetica and Courier:

```sh
sudo pacman -S tex-gyre-fonts
```

Your files keep their original font names either way. The stand-ins are
only used on screen.

## Bringing a project over from the Mac

Copy the whole `.scriv` folder across: AirDrop to a phone and back, a USB
stick, `scp`, a synced folder, whatever works. With iCloud or Dropbox, make
sure every file has finished downloading first. Then open the `.scriv`
folder from omaquill (Ctrl+O), or run `omaquill path/to/Novel.scriv`.

Before editing, you can have omaquill read the whole project without
changing anything:

```sh
omaquill check "Novel.scriv"
```

It reports what it found and names any document with things omaquill shows
as plain text (see below).

## Use

The window works like Scrivener's:

- **Binder** (left): the project's folders and documents. Drag a row onto
  another's top or bottom edge to put it before or after, or onto its
  middle to put it inside. Right-click for New, Rename, Move to Trash.
- **Editor** (middle): the selected document. Selecting a folder shows its
  **corkboard** of index cards or its **outliner** (Ctrl+2, Ctrl+3). Ctrl+1
  shows a folder's own text.
- **Inspector** (right): title, label, status, Include in Compile, the
  synopsis (the card's text) and notes.

omaquill saves as you type: a second and a half after you stop, and whenever
you switch documents or close.

| Keys | |
|---|---|
| Ctrl+N / Ctrl+Shift+N | New text / new folder |
| F2 | Rename |
| Ctrl+B, Ctrl+I, Ctrl+U | Bold, italic, underline |
| Ctrl+F | Find in the document |
| Ctrl+Shift+F | Search the whole project |
| F11 | Composition mode: full screen, just the text (Esc leaves) |
| Ctrl+1 / 2 / 3 | Text / corkboard / outliner |
| Ctrl+Alt+K / J | Move the item up / down |
| Ctrl+Alt+L / H | Indent / outdent (into the item above / out of its folder) |
| j k h l | Move and fold in the binder, when it has focus |
| Ctrl+Alt+1 / 2 | Focus the binder / the editor |
| Ctrl+Alt+B / I | Show or hide the binder / inspector |
| Ctrl+plus / minus / 0 | Zoom |
| Ctrl+Shift+E | Compile |
| Ctrl+? | All the shortcuts |

## Compile

Ctrl+Shift+E compiles everything in the Manuscript folder that's marked
Include in Compile, in binder order. Each top-level item is a chapter; a
folder's documents are its scenes, separated by `#`. The formats:

- **Word (DOCX)**. By default in standard manuscript format: Times 12 pt,
  double spaced, 1" margins, 0.5" indents, a title page with your name and
  word count, and a running header. Turn that off to keep the text's own
  formatting.
- **EPUB**, **Markdown**, **plain text** and **RTF**.

From a terminal, too:

```sh
omaquill compile "Novel.scriv" novel.docx --author "Your Name"
omaquill compile "Novel.scriv" novel.epub
```

## What carries over

omaquill changes only what you change. A document you don't edit is never
rewritten, and neither is anything in the project it doesn't understand:
compile settings, section types, targets, collections and the rest stay
exactly as Scrivener left them.

**Read and written:** the binder (folders, documents, order, nesting,
Trash); document text with fonts, sizes, bold, italic, underline,
strikethrough, color, highlight, super/subscript, alignment, indents,
paragraph spacing, line height and tab stops; links, including links
between documents and the anchors of Scrivener's comments and footnotes
(shown highlighted; their text stays in Scrivener's comments file,
untouched); synopses; notes; labels; status; Include
in Compile; titles; new documents and folders; the project search index and
checksums, updated the way Scrivener does.

**Shown, not editable:** images and PDFs in the binder. PDFs open in your
PDF viewer.

**Shown as plain text:** bullet and numbered lists (you see the markers),
tables (cells separated by tabs), inline images, and older-style inline
footnotes and annotations.
When a document has any of these, a banner says so. omaquill keeps them
until you edit that document; if you do, the words stay but that layout
goes.

**Not yet:** reading or writing the text of comments and footnotes,
Scrivenings (editing several documents as one), snapshots, collections,
keywords, custom metadata, project targets, and most of Scrivener's Compile
options. Undo brings back deleted words but not their formatting (italics
come back plain), a limit of GTK's undo; retype or reformat them.

omaquill is built for moving to Linux, not for going back and forth. A
project it has edited still opens in Scrivener, but a document edited here
loses any Scrivener-only formatting listed above.

## Safety

- Every time omaquill opens a project, it zips the whole thing into
  `~/.local/share/omaquill/backups/` before changing anything. It keeps the
  last 10 per project; Preferences changes that.
- Files are written to a temporary name and renamed into place, so a crash
  or power cut never leaves half a chapter.
- Logging out or shutting down closes the window cleanly and saves.

## The Omarchy bar widget

`org.omaquill.words` shows the words you've written today, like `󰏫 312/500`
with a daily target set in Preferences. It's highlighted once you reach the
target. Click it to open omaquill. To add it without the install script:

```sh
omarchy plugin add https://github.com/shieldsworks/omaquill --enable
```

The widget reads `~/.local/state/omaquill/today.json`, which the app keeps
current as you type.

## Development

```sh
mise install       # the Rust toolchain
mise test          # unit and project tests
mise lint          # rustfmt + clippy, warnings denied
mise start         # build and run
```

The tests use `tests/fixtures/Lighthouse.scriv`, a small made-up project
laid out the way Scrivener 3 writes one. To also run a real project through
the reader, the GTK round trip, and a comparison with Scrivener's own
search index and checksums (all read-only):

```sh
OMAQUILL_REAL_PROJECT=~/Novel.scriv cargo test
```

Code map: `src/xml.rs` (a lossless XML tree for the `.scrivx`), `src/rtf.rs`
(RTF in and out), `src/project.rs` (the project on disk), `src/compile.rs`,
and `src/ui/` (the app; `rich.rs` moves text between RTF and GTK's text
buffer).

## License

MIT

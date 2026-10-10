# Working on omaquill

omaquill is a writing studio for authors on Linux and Omarchy. It reads and
writes Scrivener 3 `.scriv` projects. The window is GTK 4 and libadwaita.
`omaquill check` reads a project and writes nothing. `omaquill compile`
writes `.docx`, PDF, EPUB, Markdown, plain text, or RTF from the manuscript.
The bar widget in `ui/WordsBar.qml` reads today's word count and does not
write the project.

## Toolchain

Run `mise install` first. `mise.toml` pins Rust 1.98 with rustfmt and clippy.
`Cargo.toml` sets `edition` to 2024 and `rust-version` to 1.89, the oldest
Rust the code promises to build on. A system cargo older than that fails.
`mise tasks` lists every job, and each runs as `mise <job>`.

The build needs GTK 4.12 and libadwaita 1.5. CI installs `libgtk-4-dev` and
`libadwaita-1-dev`. The bar widget runs under Quickshell. CI does not launch
it, and CI does not run qmllint.

## Done means verified

This repo has no `scripts/verify.sh`. You are not done until both of these
pass from the repo root.

```sh
mise lint
mise test
```

`mise lint` runs `cargo fmt --check`, then
`cargo clippy --locked --all-targets -- -D warnings`. `mise test` runs
`cargo test --locked`. CI runs both, then `cargo build --release --locked`,
on x86_64 and on aarch64. It runs on pull requests and on pushes to `main`.
Fix what a check reports. Do not weaken the check that reported it.

There is no `.cursor/skills/verify` skill and no feature map. Do not claim
you drove one.

What the tests cover:

- `tests/project.rs` copies `tests/fixtures/Lighthouse.scriv` into a temp
  directory and edits the copy.
- `tests/compile.rs` checks compiled `.docx`, PDF, EPUB, Markdown, plain
  text, and RTF. `epubcheck`, `pdfinfo`, and `pdftotext` run only when they
  are already installed. CI does not install them. Without them the PDF test
  still requires a `%PDF-` header, and it skips the page-size check.
- `tests/real_project.rs` runs only when `OMAQUILL_REAL_PROJECT` points at a
  `.scriv` folder. It only reads that folder. With the variable unset, those
  tests return and pass.
- Modules under `src/` contain their own unit tests.
- There is no `tests/golden/` directory.

## The gates are not yours to move

These files set the rules. Change them only in a change whose whole purpose
is changing them, and have a human review that change.

- `.github/workflows/`
- the `lint`, `test`, and `build` tasks in `mise.toml`
- `#![cfg_attr(not(test), deny(clippy::unwrap_used))]` in `src/lib.rs` and
  in `src/main.rs`

This repo has no `[lints]` table, no `clippy.toml`, no `scripts/verify.sh`,
and no `scripts/check-comments.sh`. Do not add any of them in a change about
behavior. A new gate is its own change, and a human reviews it.

Clippy's pedantic lints, the cast lints, `undocumented_unsafe_blocks`, and
`allow_attributes_without_reason` are not turned on. Do not turn one on in
a behavior change. CI denies warnings, so a new deny fails the build until
every hit is fixed, and that fix belongs in the gate change.

To silence one lint at one site, put
`#[expect(clippy::<lint>, reason = "<the fact that makes this correct>")]`
on the smallest item. `#[expect]` fails once the code stops needing it. A
crate-level allow is not the fix. Tests may call `unwrap`.
`cargo clippy --all-targets` still lints the non-test build, so `unwrap` in
`src/` outside `#[cfg(test)]` fails. Each file under `tests/` is its own
crate and does not carry the deny.

## Every behavior change has a test

- A bug fix starts with a failing test that reproduces the bug.
- New behavior lands with the test that pins it. This repo has no golden
  files. Assert the bytes, the text, or the error in a Rust test. Do not
  add a `tests/golden/` directory in a behavior change.
- A refactor changes no test expectation. If one has to change, it was not
  a refactor.
- Tests write only to a temp directory. `copy_fixture` in `tests/project.rs`
  is the pattern. Do not edit `tests/fixtures/` by hand. `Lighthouse.scriv`
  is a made-up Scrivener 3 project.
- `OMAQUILL_REAL_PROJECT` is read-only. A test must not write into that
  folder.

## No apologetic comments

Comments state facts about the code and the world it handles. They do not
apologize, defer, or hedge. Do not write TODO, FIXME, XXX, HACK, workaround,
temporary fix, quick fix, for the time being, not ideal, should be fixed,
sorry, kludge, or band-aid. If something is wrong, fix it in this change or
open an issue and leave the code honest. A constraint from outside the repo
is written as the fact. Say what the outside thing does, and what this code
does about it.

No script in this repo checks those words. Do not add one in a behavior
change.

## Copy the right pattern

You will copy what you see. Before copying, check that the code you copy
passes today's lints and has a test. Old code may predate the rules.

- No `unwrap()` outside tests. Return a `Result`, or call the `expect`
  method with the invariant that makes the call safe. `src/xml.rs` and
  `src/project.rs` do the latter, as in `expect("checked on open")`.
- Cast lints are not on. Do not rewrite an existing `as` cast in an
  unrelated change. A new `as` that can truncate needs the range stated in
  the change.
- Take the shortcut only when it is also the right path. If the right path
  is hard, say so in the change and do not ship the shortcut.

## Allowed unsafe

Three blocks, in two functions. Do not add another unless std and the crates
already in `Cargo.toml` have no safe function for the job. A new block needs
a `// SAFETY:` comment that says why it is sound, and the change names the
site.

- `timestamp` in `src/project.rs` zero-fills a `libc::tm`, then calls
  `libc::localtime_r` on that struct. Scrivener stores a local modified time
  with a numeric offset. `SystemTime` does not provide the offset.
- `main` in `src/main.rs` calls `libc::signal` with `SIGPIPE` and `SIG_DFL`
  before any other thread starts. `omaquill check` piped to `head` then
  stops without aborting on `SIGPIPE`.

## Review

The agent that wrote a change does not approve, merge, or mark it verified.
A different agent, with fresh context and a clean checkout, or Casey,
reviews it. They run `mise lint` and `mise test`.

The change lists what changed, the tests that prove it, and what was not
checked. The reviewer reports what they ran and what they saw.

## Conventions

Rust, edition 2024. Ask before adding a crate. Always pass `--locked`.
Change `Cargo.lock` only in a change about dependencies.

The direct dependencies are these, and no others.

- `libc`, for `localtime_r` and for signal numbers.
- `gtk`, package `gtk4`, feature `v4_12`.
- `adw`, package `libadwaita`, feature `v1_5`.
- `cairo-rs`, features `pdf` and `v1_16`. The crate name in code is `cairo`.
- `pango`, feature `v1_44`.
- `pangocairo`.
- `glib-unix`.

`src/lib.rs` does not import the `gtk` crate. `src/pdf.rs` uses `cairo`,
`pango`, and `pangocairo`. The window under `src/ui/` uses `gtk` and `adw`.
`glib-unix` is called from `src/ui/mod.rs`.

Do not add a second XML, RTF, zip, SHA-1, or PDF crate. Those jobs are
`src/xml.rs`, `src/rtf.rs`, `src/zip.rs`, `src/sha1.rs`, and Cairo. Do not
add a Scrivener SDK.

Do not edit `data/hyphenation/hyph-en-us.pat.txt` or
`data/hyphenation/hyph-en-us.hyp.txt` by hand. They are the hyph-utf8
American English patterns. Keep the copyright notice in
`data/hyphenation/README.md`.

## Rules specific to omaquill

A `.scriv` folder is the user's book. These rules keep a save from leaving
a half-written chapter. They also keep a write inside the project.

Project text, the `.scrivx`, `Files/Data/docs.checksum`,
`Files/search.indexes`, and the files in `src/state.rs` are replaced through
`write_atomic` in `src/project.rs`.

- `write_atomic` creates a new temp file in the same directory. The name is
  `.<filename>.<pid>-<n>.omaquill-tmp`, opened with `create_new`.
- When the target already exists, the temp file takes the target's
  permission bits.
- It writes the bytes, calls `sync_all` on the temp file, and renames the
  temp file onto the target. A failure deletes the temp file. The file
  `sync_all` is part of that result, so a failed file sync does not rename.
- It then opens the parent directory and calls `sync_all` on it, so the
  rename survives a power cut. That directory call ignores its error. Do
  not delete the call. Do not make a directory sync error fail the save in
  an unrelated change. That is its own change, with a test.
- If `path` is an existing symlink, `canonicalize` sends the write to the
  target file.

`write_inside` is the project boundary around `write_atomic`.

- The parent directory must resolve inside the project. Otherwise the write
  returns `PermissionDenied` and the outside file stays as it was.
- A symlink whose target is outside the project is removed, and the new
  file is a normal file inside the project. The outside target is left
  unchanged.
- A symlink whose target stays inside the project is kept, and the write
  follows it.

`read_inside` refuses a path that resolves outside the project.
`remove_file_inside` and `remove_dir_inside` do the same for deletes.
`empty_trash` skips a name that fails `is_uuid`, so a missing id cannot
become a delete of `Files/Data`.

`set_text`, `set_notes`, and `set_synopsis` write that one file immediately
through `write_inside`. `Project::save` rewrites the `.scrivx`,
`docs.checksum`, and the search index only when those are dirty. Do not
make a save rewrite every document.

Settings, the recent-project list, per-project view state, and `today.json`
call `write_atomic` directly. They live outside the project, under the paths
documented in `src/state.rs`. Do not send them through `write_inside`.

`project::backup` zips one project into `state::backup_dir`. The folder name
includes a hash of the project's full path, so two projects do not share or
prune each other's zips. The zip is written to `*.zip.part` and then
renamed. The window starts that backup on open, on another thread, when the
backup count in settings is above zero. `omaquill check` does not back up
and does not write the project.

The editor saves text for `editor_uuid` only, and not for the binder
selection in `current`. A failed load must not write one document's words
into another.

`is_openable` allows documents and media only. The test
`media_extensions_are_plain_and_openable_types_are_few` refuses `.desktop`,
`.sh`, `.html`, `.exe`, `.AppImage`, and `.py`. `media_path` accepts a
short alphanumeric extension when the file exists and resolves inside the
project.
`safe_to_open` in `src/ui/board.rs` also reads up to 4096 bytes and checks
the sniffed type. `image_size_ok` reads the header and refuses an image of
more than 100,000,000 pixels, or a side longer than 30,000, before decode.

Logout uses `glib_unix::unix_signal_add_local` for `SIGTERM`, `SIGINT`, and
`SIGHUP`. The handler closes windows so a pending edit can save. Do not
replace that handler with an exit that skips the close.

A change to `write_atomic`, the inside checks, `is_uuid` in `empty_trash`,
`is_openable`, `safe_to_open`, `image_size_ok`, backup pruning, or the
`editor_uuid` save path needs a test that fails when the guarantee is gone.

## Layout

- `src/main.rs` is the `omaquill` command. It handles `check`, `compile`, and the window.
- `src/lib.rs` declares the library modules and does not import the `gtk` crate.
- `src/project.rs` is the `.scriv` project, containment, `write_atomic`, backup, and timestamps.
- `src/rtf.rs` reads and writes RTF.
- `src/xml.rs` reads and writes the project XML.
- `src/compile.rs` gathers the manuscript and dispatches each format. PDF layout is `src/pdf.rs`.
- `src/pdf.rs` lays out PDF with Pango and Cairo.
- `src/zip.rs` writes the zip data for `.docx`, EPUB, and backups.
- `src/sha1.rs` is the document checksum and the backup folder id.
- `src/fonts.rs` picks stand-in fonts for names the machine does not have.
- `src/hyphen.rs` hyphenates the paperback from `data/hyphenation`.
- `src/state.rs` is settings, recent projects, view state, `today.json`, and the backup directory.
- `src/ui/mod.rs` is the window, save timing, and the logout signals.
- `src/ui/binder.rs` is the binder.
- `src/ui/editor.rs` is the text editor.
- `src/ui/inspector.rs` is the inspector.
- `src/ui/board.rs` is the corkboard, the outliner, image limits, and opening a file in another app.
- `src/ui/dialogs.rs` is rename and the shortcut dialog.
- `src/ui/theme.rs` reads the Omarchy theme colors.
- `src/ui/rich.rs` maps RTF styles onto a GTK text buffer.
- `ui/WordsBar.qml` is the bar widget. It reads `today.json` and does not write the project.
- `tests/project.rs` edits a temp copy of the fixture.
- `tests/compile.rs` checks each compile format.
- `tests/real_project.rs` reads an external project when asked.
- `tests/fixtures/Lighthouse.scriv` is the made-up project.
- `scripts/install-local.sh` and `scripts/uninstall-local.sh` install and remove the local build.
- `packaging/PKGBUILD` is the Arch package recipe.
- `data/` holds the desktop entry, the icon, the mime type, and the hyphenation patterns.
- `.github/workflows/ci.yml` is the CI workflow.
- `mise.toml` pins Rust and the tasks `lint`, `test`, `build`, `format`, and `start`.

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

You are not done until these pass from the repo root.

```sh
mise lint
mise test
scripts/verify.sh
```

`mise lint` runs `cargo fmt --check`, then
`cargo clippy --locked --all-targets -- -D warnings`. `mise test` runs
`cargo test --locked`. `scripts/verify.sh` checks the sha256 pin on
`scripts/check-comments.sh` with `sha256sum -c`, then runs that script.
CI runs `mise lint` and `mise test`, then `cargo build --release --locked`,
on x86_64 and on aarch64. It runs on pull requests and on pushes to `main`.
Fix what a check reports. Do not weaken the check that reported it.

There is no `.cursor/skills/verify` skill and no feature map. Do not claim
you drove one.

What the tests cover:

- `tests/project.rs` copies `tests/fixtures/Lighthouse.scriv` into a temp
  directory and edits the copy.
- `tests/compile.rs` checks compiled `.docx`, PDF, EPUB, Markdown, plain
  text, and RTF, and that a compile on a worker thread writes the same
  bytes. `epubcheck`, `pdfinfo`, and `pdftotext` run only when they
  are already installed. CI does not install them. Without them the PDF test
  still requires a `%PDF-` header, and it skips the page-size check.
- `tests/boundary.rs` copies the fixture and races a second thread that
  swaps files and folders for symlinks while omaquill saves and backs up.
  It asserts that the files outside the project keep their bytes. Each race
  runs for 2.5 seconds.
- `tests/real_project.rs` runs only when `OMAQUILL_REAL_PROJECT` is set.
  Point it at a `.scriv` folder. The tests only read that folder. With the
  variable unset, they return and pass.
- Some modules under `src/` contain their own unit tests. `src/project.rs`,
  `src/export.rs`, `src/fonts.rs`, `src/main.rs`, `src/ui/binder.rs`, and
  `src/ui/inspector.rs` do not.
- There is no `tests/golden/` directory.

## The gates are not yours to move

These files set the rules. Change them only in a change whose whole purpose
is changing them. Such a PR merges only after review by someone other than
its author, either Casey or Casey's delegated reviewer Dev. Dev's review
means all three. An independent agent verifies the PR head on a clean
checkout. That agent runs `mise lint`, `mise test`, and `scripts/verify.sh`,
and drives the changed behavior. An adversarial review challenges the
change. CI is green on the exact head SHA merged. The author agent never
approves or merges its own PR. A `Cargo.lock` change or a dependency change
merges only after that same review.

- `.github/workflows/`
- the `lint`, `test`, and `build` tasks in `mise.toml`
- `#![cfg_attr(not(test), deny(clippy::unwrap_used))]` in `src/lib.rs` and
  in `src/main.rs`
- `scripts/verify.sh` and `scripts/check-comments.sh`

This repo has no `[lints]` table and no `clippy.toml`. Do not add either in
a change about behavior. Do not change `scripts/verify.sh` or
`scripts/check-comments.sh` in a change about behavior. A new gate is its
own change. Such a PR merges only after review
by someone other than its author, either Casey or Casey's delegated
reviewer Dev. Dev's review means all three. An independent agent verifies
the PR head on a clean checkout. That agent runs `mise lint`, `mise test`,
and `scripts/verify.sh`, and drives the changed behavior. An adversarial
review challenges the change. CI is green on the exact head SHA merged.
The author agent never approves or merges its own PR.

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

`scripts/check-comments.sh` is a byte-for-byte copy of omahoy's canonical
script. The pin is this sha256. A copy with any other hash is not this
rule.

```
f3ec8a2d07e8f1e7c50770d0bb93579fc7bb0d60fc8e359a22607ba55cebc6c6  scripts/check-comments.sh
```

From the repo root, pipe that line to `sha256sum -c`. `scripts/verify.sh`
does that, then runs the script with no arguments to scan the default set.

```sh
scripts/check-comments.sh
```

The script reads tracked `.rs`, `.qml`, `.js`, `.sh`, `.py`, `.toml`, and
`.yml` files. It skips `tests/fixtures/` and `**/vendor/**`. It does not
read Markdown. The rule holds there too. Pass git pathspecs to scan a
different set. Those arguments replace the default set. The script still
excludes its own path.

## Copy the right pattern

You will copy what you see. Before copying, check that the code you copy
passes today's lints and has a test. Old code may predate the rules.

- No `unwrap()` outside tests. Return a `Result`, or call the `expect`
  method with the invariant that makes the call safe. `src/project.rs` does
  the latter, as in `expect("checked on open")`. `src/xml.rs` does it with
  the parser's own invariant, as in
  `expect("parser stack is non-empty until the root element returns")`.
- Cast lints are not on. Do not rewrite an existing `as` cast in an
  unrelated change. A new `as` that can truncate needs the range stated in
  the change.
- Take the shortcut only when it is also the right path. If the right path
  is hard, say so in the change and do not ship the shortcut.

## Allowed unsafe

Eighteen blocks, in seventeen functions. Do not add another unless std and the crates
already in `Cargo.toml` have no safe function for the job. A new block needs
a `// SAFETY:` comment that says why it is sound, and the change names the
site.

- `src/fsdir.rs` calls the `*at` functions that std does not offer. std
  opens files by path only. Each block is one call on a descriptor the
  `Dir` owns and a NUL-terminated name that outlives the call.
  - `Dir::openat` calls `libc::openat` and hands a descriptor it returns to
    `OwnedFd`, which is then its only owner.
  - `Dir::stat` zero-fills a `libc::stat` and calls `libc::fstatat` with
    `AT_SYMLINK_NOFOLLOW` on it.
  - `Dir::mkdir`, `Dir::link`, `Dir::unlink`, and `Dir::rename` call
    `mkdirat`, `linkat` with no flags, `unlinkat`, and `renameat`.
  - `Dir::free_bytes` zero-fills a `libc::statvfs` and calls
    `libc::fstatvfs` on the folder's descriptor.
  - `Dir::open_file` clears `O_NONBLOCK` with `fcntl` on the file it
    opened, and a failed `fcntl` fails the open.
  - `Dir::entries` clears `errno` before each `readdir` and reads it after
    a null, so a read error is returned and not taken for the end. It
    hands a fresh descriptor to `fdopendir`, copies each
    name `readdir` returns before the next call, and calls `closedir` once.
    When `fdopendir` fails, `OwnedFd` closes the descriptor.
- `timestamp` in `src/project.rs` zero-fills a `libc::tm`, then calls
  `libc::localtime_r` on that struct. Scrivener stores a local modified time
  with a numeric offset. `SystemTime` does not provide the offset.
- `main` in `src/main.rs` calls `libc::signal` with `SIGPIPE` and `SIG_DFL`
  before any other thread starts. `omaquill check` piped to `head` then
  stops without aborting on `SIGPIPE`.
- `Library::open` in `src/enchant.rs` loads `libenchant-2.so.2` with
  `dlopen` and `RTLD_NOW`, then resolves seven Enchant 2 entry points. The
  soname and the symbol names are NUL-terminated literals. A null `dlopen`
  or `dlsym` returns `None`. Each function pointer is transmuted to the
  prototype quoted on its alias. The handle stays in the `Library` that
  owns those pointers. Enchant is not a crate in `Cargo.toml`, and omaquill
  does not link it, so a build without the library still compiles.
- `Drop` for `Handle` in `src/enchant.rs` is the one `dlclose` for that
  `dlopen`. `Handle` is not `Clone` or `Copy`. Enchant calls through the
  pointers return before the handle drops.
- `Broker::open` in `src/enchant.rs` calls `enchant_broker_init` and
  `enchant_broker_request_dict`. Each tag is a NUL-terminated string that
  outlives the request. A null broker, or no dictionary, returns `None`.
  A broker with no dictionary is freed once on that path.
- `Broker::check` in `src/enchant.rs` calls `enchant_dict_check` with the
  dictionary `open` stored and one NUL-terminated UTF-8 word. `Broker` is
  neither `Send` nor `Sync`, so the call stays on the thread that opened it.
- `Broker::suggest` in `src/enchant.rs` calls `enchant_dict_suggest`, copies
  at most 32 UTF-8 strings, and frees that list once with the same
  dictionary.
- `Drop` for `Broker` in `src/enchant.rs` frees the dictionary, then the
  broker. The library closes after those calls return.

## Review

A PR merges only after review by someone other than its author, either
Casey or Casey's delegated reviewer Dev. Dev's review means all three. An
independent agent verifies the PR head on a clean checkout. That agent runs
`mise lint`, `mise test`, and `scripts/verify.sh`, and drives the changed
behavior. An adversarial review challenges the change. CI is green on the
exact head SHA merged. The author agent never approves or merges its own
PR. The author agent does not mark the change verified.

The change lists what changed, the tests that prove it, and what was not
checked. The reviewer reports what they ran and what they saw.

## Conventions

Rust, edition 2024. Ask before adding a crate. Always pass `--locked`.
Change `Cargo.lock` only in a change about dependencies.

The direct dependencies are these, and no others.

- `libc`, for `localtime_r`, for signal numbers, and for `dlopen` of Enchant.
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

Every file in a project is reached through `fsdir::Dir` in
`src/fsdir.rs`. `Project::open` opens the project folder once. Each read,
write and delete after that starts from that descriptor and opens one name
at a time with `openat` and `O_NOFOLLOW`. No path in the project is checked
first and opened later.

- A folder on the way that is a symlink refuses the call with
  `PermissionDenied`. This holds when the link points inside the project
  too. `fsdir::is_link_refusal` tells that error apart.
- A file that is a symlink is not read. A write replaces the link with an
  ordinary file. The link's target keeps its bytes.
- `..`, an absolute path, an empty name, and a NUL byte are refused with
  `InvalidInput`.
- A folder, FIFO, socket or device where a file is written is refused with
  `InvalidInput`, and nothing is written.

`Dir::write_atomic_if` replaces one file.

- It creates `.<filename>.<pid>-<n>.omaquill-tmp` in the target's folder
  with `O_CREAT | O_EXCL | O_NOFOLLOW`. A name already taken moves on to
  the next `n`, up to 1,000 tries.
- When the target is a regular file owned by this user, the temp file
  takes its `0o777` permission bits. A file someone else owns passes on
  nothing, and the temp file keeps `0o666` less the umask. Set-id and
  sticky bits are never copied.
- A long target name is cut short in the temp name, so the temp name stays
  within 255 bytes.
- It writes the bytes, calls `sync_all` on the temp file, and renames the
  temp file onto the target with `renameat` in that same folder. A failure
  deletes the temp file. The file `sync_all` is part of that result, so a
  failed file sync does not rename.
- It then calls `sync_all` on the folder, so the rename survives a power
  cut. That folder call ignores its error. Do not delete the call. Do not
  make a folder sync error fail the save in an unrelated change. That is
  its own change, with a test.
- A rename replaces the directory entry. It does not write into the file
  the entry named before, so a hard link at the target does not carry the
  write anywhere else.

`set_text` writes `content.rtf` immediately through `Dir::write_atomic`.
`set_notes` and `set_synopsis` write that way when the text is not empty,
and remove the file through `Dir::remove_file` when it is. `Project::save`
rewrites the `.scrivx`, `docs.checksum`, and the search index only when
those are dirty. Do not make a save rewrite every document.

`Dir::remove_file` and `Dir::remove_tree` remove a symlink as a link and
leave its target unchanged. `remove_tree` walks the folder through
descriptors, so a symlink inside it is removed as a link too.
`empty_trash` skips a child name that fails `is_uuid`, so a missing id
cannot become a delete of `Files/Data`. A Trash folder with no id returns
an error and deletes nothing.

`Project::create` makes the project folder with `mkdirat` in its parent
and opens it without following a symlink. An entry already at that path,
a dangling symlink included, is an error.

Files outside a project go through `fsdir::write_atomic` or
`fsdir::write_atomic_if` with a path. These are the window's Compile, the
`omaquill compile` command's file targets, settings, the recent-project list,
per-project view state, the word lists, and `today.json`. That call opens
the file's folder once, following symlinks in the folder's path, which the
user chose. Then it is `Dir::write_atomic_if` in that folder. A symlink at
the file itself is replaced, not followed. That covers a link planted in a
shared folder where the user exports. It also means a settings file kept
as a symlink into a dotfiles repository becomes a plain file on the next
save. Settings and state files stay strict on purpose. A user who keeps
them in a dotfiles repository has to link the folder, not each file.

`omaquill compile PROJECT OUT` picks its write by what OUT is
(`write_output` in `src/main.rs`).

- `-`, `/dev/stdout`, `/dev/fd/1`, and `/proc/self/fd/1` write to the
  process's own standard output. The path is not printed after the bytes.
- A FIFO, device, or socket gets a plain write, opened with `O_NOFOLLOW`
  and with no file created. `/dev/null` works this way.
- A symlink at a system name for a stream (`/dev/stdin`, `/dev/stdout`,
  `/dev/stderr`, `/dev/fd/*`, `/dev/tty*`, `/proc/self/fd/*`; see
  `names_a_stream`) is followed for a plain write and never replaced, as
  root too. When it leads to a regular file, the command writes nothing
  and says so. Links elsewhere in `/dev`, such as world-writable
  `/dev/shm` and `/dev/mqueue`, are not trusted.
- Anything else is `fsdir::write_atomic`. A symlink there is replaced, and
  a folder is refused with "is not a regular file".

`project::backup` zips one project into `state::backup_dir`. The folder name
includes a hash of the project's full path, so two projects do not share or
prune each other's zips. `Project::backup`, the backup before a project
replace, and the backup the window starts on open read the open project
folder (`Project::folder`, `backup_folder`). They do not open the project
path again. A file or folder that is a symlink is left out of the zip, and
so is one that vanishes or changes kind while the backup runs. The zip is written to
`.backup.<pid>-<n>.omaquill-tmp` in that folder, synced, then hard-linked
to `<date>.zip` (or `<date> N.zip` when that second already has a backup).
The link fails if the name is already there, so two backups never share a
partial file or replace each other. The window starts that backup on open,
on another thread, when the backup count in settings is above zero.
`omaquill check` does not back up and does not write the project.

The editor saves text for `editor_uuid` only, and not for the binder
selection in `current`. A failed load must not write one document's words
into another.

`is_openable` allows documents and media only. The test
`media_extensions_are_plain_and_openable_types_are_few` refuses `.desktop`,
`.sh`, `.html`, `.exe`, `.AppImage`, and `.py`. `Project::media` accepts a
short alphanumeric extension and opens the file through the project's
`Dir`. A symlink, a folder link on the way, a FIFO, or anything else that
is not a regular file is refused. It returns `Media`, an open file. No
project path is handed to GTK, gio, or another app.

- An image is read once from that file, up to 256 MiB, and decoded from
  the bytes. `decode_image` in `src/ui/board.rs` stops at the header for an
  image of more than 100,000,000 pixels, or a side longer than 30,000.
- `safe_to_open` checks the name and the sniffed type of the first 4096
  bytes.
- Open in Default App copies the open file with `PrivateCopies` in
  `src/project.rs`. Every open makes a new `omaquill-open-<random>`
  folder, created exclusively with mode `0o700`, in `$XDG_RUNTIME_DIR`
  (the temp folder when that is unset), holding one `content.<ext>` of
  mode `0o600`. No folder is shared or reused, so a copy an app was given
  is never written again, and projects that share UUIDs can't collide.
  Files over 1 GiB, or over half the free space in that folder
  (`Dir::free_bytes`), are refused with a toast. After a launch succeeds,
  the previous launched copy of the same item (keyed by canonical project
  folder and UUID, per process) is removed, so repeated opens keep one
  copy. A failed launch, or a window closed mid-copy, removes the new
  copy. In `$XDG_RUNTIME_DIR`, the
  process's first copy removes `omaquill-open-*` entries over a day old,
  as links when they are links, skipping any it can't remove. The copy is
  sniffed again and launched; a refused copy's folder is removed.
- The copy runs on a `gio::spawn_blocking` worker. The button is off and
  a toast says "Copying…" meanwhile, and the launch happens on the main
  thread when the copy reports back, unless the window was closed.
  `board::tests::an_open_copy_leaves_the_main_loop_running` holds the
  worker for a second and checks the main loop's gaps from the click.

Logout uses `glib_unix::unix_signal_add_local` for `SIGTERM`, `SIGINT`, and
`SIGHUP`. The handler closes windows so a pending edit can save. Do not
replace that handler with an exit that skips the close.

A change to `src/fsdir.rs`, `is_uuid` in `empty_trash`, `is_openable`,
`safe_to_open`, `decode_image`, `Project::media`, backup pruning, or the
`editor_uuid` save path needs a test that fails when the guarantee is gone.

## Layout

- `src/main.rs` is the `omaquill` command. It handles `check`, `compile`, and the window.
- `src/lib.rs` declares the library modules and does not import the `gtk` crate.
- `src/project.rs` is the `.scriv` project, backup, and timestamps.
- `src/fsdir.rs` is `Dir`, the open folder every project file is reached through, and the atomic write.
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
- `src/ui/dialogs.rs` is rename, Compile, preferences, keyboard shortcuts, and about.
- `src/ui/theme.rs` reads the Omarchy theme colors.
- `src/ui/rich.rs` maps RTF styles onto a GTK text buffer.
- `ui/WordsBar.qml` is the bar widget. It reads `today.json` and does not write the project.
- `tests/project.rs` edits a temp copy of the fixture.
- `tests/compile.rs` checks each compile format.
- `tests/boundary.rs` races symlink swaps against saves and backups.
- `tests/real_project.rs` reads an external project when asked.
- `tests/fixtures/Lighthouse.scriv` is the made-up project.
- `scripts/check-comments.sh` is the byte-for-byte copy of omahoy's comment rule.
- `scripts/verify.sh` checks that copy's sha256, then runs the script.
- `scripts/install-local.sh` and `scripts/uninstall-local.sh` install and remove the local build.
- `packaging/PKGBUILD` is the Arch package recipe.
- `data/` holds the desktop entry, the icon, the mime type, and the hyphenation patterns.
- `.github/workflows/ci.yml` is the CI workflow.
- `mise.toml` pins Rust and the tasks `lint`, `test`, `build`, `format`, and `start`.

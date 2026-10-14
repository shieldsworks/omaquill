# Open a project

Open a project shows the binder, the first manuscript scene, and the window
title. `omaquill check` reads the same folder and writes nothing.

## Sub-features

- `open-window` opens a `.scriv` folder in the window.
- `open-first-scene` shows the first text document in Manuscript.
- `open-check` prints the document and word counts from the terminal.

## How to get to it (user POV)

- Run `omaquill path/to/Project.scriv`.
- In a running window, press `Ctrl+O` and choose the `.scriv` folder.
- Press `Ctrl+Shift+O` to create a new project. That path is a different feature.

## Driving it with verify-omaquill

Preconditions:

- Launch from `../SKILL.md` has copied the fixture to `$run/Lighthouse.scriv`.
- `backups` in this run's settings is 0.
- Doctor reports a live pid and the window title `Lighthouse — omaquill`.

- **Window title.** Read the title after launch. `xdotool search --name "Lighthouse — omaquill"` prints a window id. The accessibility frame has that same name.
- **First scene.** Read the editor text. The empty-named text widget starts with `The storm came in off the point at dusk.` The binder has a button named `The Storm`.
- **Read-only check.** In another command, after the window is up or after it has quit, run `./target/debug/omaquill check "$run/Lighthouse.scriv"`. Exit code 0. Stdout on an unmodified copy is the five lines below.

```
Lighthouse
  7 documents, 62 words in all
  manuscript: 1 chapters, 51 words compile
  binder: 3 items in Manuscript
  every document reads fully
```

- **Fixture untouched.** `git status -- tests/fixtures/Lighthouse.scriv` is empty. The copy under `$run` is the one the window opened.

## Gotchas

- The same `OMAQUILL_APP_ID` as a running omaquill hands the folder to that process. A verification run sets its own id.
- `Ctrl+O` opens a folder chooser. Driving that chooser needs a click in the dialog. The argv path does not.
- With the default backup count, open writes a zip under the backup directory. This run sets `backups` to 0 so that zip is not created.
- `omaquill check` writes nothing, including no backup and no `today.json`. The window writes `recent`, view state, and `today.json` under this run's `XDG_STATE_HOME`.

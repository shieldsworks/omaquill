# omaquill verification map

This directory is the source for verifying omaquill's user-facing behavior.
Read this index, then use the matching feature file as the recipe. A change
that adds or alters a writer path updates its feature file in the same PR.

## Baseline preconditions

- A debug build of this checkout, launched per `../SKILL.md`, with its own
  `$run` directory, Xvfb display, and session bus.
- The project is a copy of `tests/fixtures/Lighthouse.scriv`. The run reads
  the fixture and does not write it.
- This run's settings set `backups` to 0.
- Doctor passes and names this run's process, window, and accessibility app.
- Never drive an `omaquill` this run did not start.

## Driving conventions

- Start every recipe from the baseline unless its preconditions say otherwise.
- Commands and key names are literal. Keep quoted names unchanged.
- Prefer accessible names over pointer coordinates. The names are in
  `../SKILL.md`.
- `xdotool key --window` does not reach this GTK window. Focus the window,
  then send keys to the display.
- Do not delete proof during cleanup.

## Proof and skip reporting

- Window proof is the accessibility text after the action, a PNG of that
  window, and the command that produced them.
- A manuscript edit also proves itself in `content.rtf`, in `omaquill check`,
  and in a compiled `.txt`.
- CLI proof is the command, stdout, stderr, and the exit code.
- Record the feature id with every artifact.
- An unreachable path is reported with the command tried and the unmet
  precondition. It is never reported as verified through a different path.

## Feature entry contract

Each feature file starts with an H1 and one paragraph, then these four H2s,
in order.

1. `Sub-features`
2. `How to get to it (user POV)`
3. `Driving it with verify-omaquill`
4. `Gotchas`

## Features

- [Open a project](./open-a-project.md) opens the Lighthouse copy and reads it with `omaquill check`.
- [Edit a document](./edit-a-document.md) types into The Storm and saves that document only.
- [Find and replace](./find-and-replace.md) replaces one word in The Storm and leaves the other documents alone.
- [Spellcheck](./spellcheck.md) turns spelling marks on or off. The marks are not written into the project.
- [Compile](./compile.md) writes the manuscript to a file the writer names.

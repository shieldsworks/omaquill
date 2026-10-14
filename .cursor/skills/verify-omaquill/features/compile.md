# Compile

Compile writes the manuscript to a file the writer names. Research and
Trash stay out of that file. The command prints the output path.

## Sub-features

- `compile-txt` writes plain text for the manuscript.
- `compile-skips-research` leaves Keeper's Log out of the file.
- `compile-cli` is the same manuscript from the terminal.

## How to get to it (user POV)

- Press `Ctrl+Shift+E` and choose a format in the window.
- Run `omaquill compile Project.scriv out.txt`.
- Run `omaquill compile Project.scriv book.pdf --book` for the paperback PDF.

## Driving it with verify-omaquill

Preconditions:

- Use the copy under `$run`, never the fixture in the repo.
- The window can be closed. Compile does not need it.
- Doctor is the version check on `./target/debug/omaquill`.

- **Plain text.** Run `./target/debug/omaquill compile "$run/Lighthouse.scriv" "$run/artifacts/verify/compile/lighthouse.txt" --format txt`. Exit code 0. Stdout is the output path. The file contains `Chapter One`, `The storm came in off the point at dusk.`, and `By morning the sea had gone flat`.
- **Research stays out.** The file does not contain `lamp oil` or `Keeper`.
- **Window path.** With the project open, press `Ctrl+Shift+E`. The compile dialog is a separate entry from the terminal command. Drive it when the change is in that dialog. The terminal command is the path recorded with the find-and-replace proof, and that proof's `lighthouse.txt` follows a replace, so its storm sentence says `lantern`.

A fresh copy's plain text still has `the lamp`. The helper writes the compiled manuscript next to the replace proof, at `$run/artifacts/verify/find-replace/lighthouse.txt`, and that copy says `lantern`.

## Gotchas

- The output path is wherever the command says. Point it under `$run/artifacts`. Do not write it into the project or the repo.
- `omaquill check` gathers the manuscript and does not write a compiled file.
- `--book` changes the PDF layout. A `.txt` compile ignores it.
- Include in Compile is on for the Lighthouse manuscript scenes. A scene with it off is absent from the file.

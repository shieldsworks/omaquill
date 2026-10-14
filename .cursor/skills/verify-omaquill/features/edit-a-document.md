# Edit a document

Edit a document types into the open scene and saves that scene's
`content.rtf`. A different document's file stays as it was.

## Sub-features

- `edit-type` inserts characters in the focused editor.
- `edit-save` writes `content.rtf` for the open document.
- `edit-other` leaves every other `content.rtf` unchanged.

## How to get to it (user POV)

- Click in the editor and type.
- Press `Ctrl+Alt+2` to move the keyboard to the editor, then type.
- Press `Ctrl+S` to save before the pause ends. Closing the window also saves.

## Driving it with verify-omaquill

Preconditions:

- The window is healthy and The Storm is the open document.
- Hash every `content.rtf` in the copy before typing.
- Doctor still names this run's pid.

- **Focus.** Press `Ctrl+Alt+2`. Run `xdotool windowfocus "$wid"` and `xdotool key ctrl+alt+2`.
- **Type.** Go to the end and add a sentence. Run `xdotool key ctrl+End` and `xdotool type --delay 20 " The lamp held."`. The editor text ends with `The lamp held.`
- **Save.** Press `Ctrl+S`. Run `xdotool key ctrl+s`. Poll The Storm `content.rtf` until it contains `The lamp held.` The status text in the window becomes `Saved` when the write finishes.
- **Other files.** Hash the other `content.rtf` files again. Morning & After, Keeper's Log, and Cut Scene match the hashes from before the typing.
- **Check.** Run `./target/debug/omaquill check "$run/Lighthouse.scriv"`. The word count is higher than 62. Exit code 0.

This recipe was not the run that proved the skill. Find and replace was.

## Gotchas

- The save waits 1.5 seconds after the last key, and it also runs on `Ctrl+S` and on close. Poll the file. Do not treat a sleep as the proof.
- Text is saved for the document loaded in the editor. A failed load must not write this sentence into another file. The hash check is that proof.
- Typing into `tests/fixtures/Lighthouse.scriv` changes the repo fixture. Type into the copy.

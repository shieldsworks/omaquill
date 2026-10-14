# Find and replace

Find and replace changes a word in the open scene, or finds it without
changing it. Project search filters the binder. It does not edit.

## Sub-features

- `find-document` highlights a match in the open scene.
- `replace-one-document` replaces every match in that scene from the dialog.
- `replace-leaves-others` leaves the other scenes' files unchanged.
- `find-project` filters the binder from the project search box.

## How to get to it (user POV)

- Press `Ctrl+F` to find in the open document.
- Press `Ctrl+H` to open Find and Replace.
- Press `Ctrl+Shift+F` to search the whole project from the binder box.

## Driving it with verify-omaquill

Preconditions:

- The window is healthy and The Storm is open.
- The Storm still contains `the lamp`. Keeper's Log still contains `lamp oil`.
- Hash The Storm, Morning & After, Keeper's Log, and Cut Scene `content.rtf` files.
- Doctor reports the pid, the window title, and the accessibility app `omaquill`.

The helper runs this drive.

```sh
bash .cursor/skills/verify-omaquill/drive-find-replace.sh
```

- **Open the dialog.** Focus the editor, then press `Ctrl+H`. Run `xdotool key ctrl+alt+2` and `xdotool key ctrl+h`. A dialog is present. Its text widgets are named `Find` and `Replace with`. Its buttons include `Replace All`.
- **Fill the fields.** Set `Find` to `lamp` and `Replace with` to `lantern` with `setTextContents`. Read both values back. They match what was set.
- **Replace.** Click `Replace All` with `doAction(0)`. The editor text contains `Mara lit the lantern` and does not contain `lit the lamp`. A label is named `Replaced 1`.
- **Save.** Press `Escape`, then `Ctrl+S`. Poll The Storm `content.rtf` until it contains `lantern` and does not contain `lamp`.
- **Other documents.** Morning & After, Keeper's Log, and Cut Scene keep the hashes from before the click. Keeper's Log still contains `lamp oil`.
- **Second view.** Quit with `Ctrl+Q`. Run `omaquill check` on the copy. Exit code 0, and stdout still contains `62 words` and `every document reads fully`. Replacing `lamp` with `lantern` does not change the word count. Run `omaquill compile "$run/Lighthouse.scriv" "$run/artifacts/verify/find-replace/lighthouse.txt" --format txt`. The file contains `Mara lit the lantern` and `By morning the sea had gone flat`. It does not contain `lamp oil`.
- **Proof.** The helper writes `editor.txt`, `toast.txt`, `after-click.png`, `saved.png`, `check.txt`, `lighthouse.txt`, and `storm-after.rtf` under `$run/artifacts/verify/find-replace/`.

A run of the helper on 2026-10-14 exited 0. Doctor printed `omaquill 0.2.0`, `pid 8483 alive`, and `window Lighthouse — omaquill`. The pid changes every launch. `editor.txt` was:

```
The storm came in off the point at dusk. Mara lit the lantern and counted the seconds between the flashes, the way her father had taught her.

Twelve, she thought. “Always twelve.”
```

`toast.txt` was `Replaced 1`. `check.txt` was:

```
Lighthouse
  7 documents, 62 words in all
  manuscript: 1 chapters, 51 words compile
  binder: 3 items in Manuscript
  every document reads fully
```

`lighthouse.txt` contained `Mara lit the lantern` and `By morning the sea had gone flat and gray`. `storm-after.rtf` contained `lantern` and did not contain `lamp`. The other three `content.rtf` hashes matched the pre-drive file. After the script exited, `after-click.png` and `editor.txt` were still in that directory.

`find-document` and `find-project` are not part of that helper. Drive them from the same launch if the change touches `Ctrl+F` or `Ctrl+Shift+F`. For project search, `Ctrl+Shift+F` focuses the box whose placeholder is `Search project`. The query `lamp` on an unmodified copy lists The Storm and Keeper's Log. The query `lantern` lists nothing until the replace above.

## Gotchas

- The first `Replace` click selects the match. A second click replaces that selection. `Replace All` replaces every match in the focused document in one click.
- `Replace All` does not search the rest of the project. `Preview in Project` does. Do not use it as proof that one scene changed.
- Match case is off unless that switch is on. `lamp` matches `Lamp`.
- A lossy document refuses the replace and toasts the reason. The Storm is not lossy. The toast for this drive is `Replaced 1`, not a refusal.
- The word count stays 62. Assert the letters in the file and in the compiled text.

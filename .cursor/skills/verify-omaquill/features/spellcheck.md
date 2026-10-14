# Spellcheck

Spellcheck draws a mark under a word the dictionary rejects. The mark is
not stored in the `.scriv` project. Turning the marks off is remembered
in this run's settings.

## Sub-features

- `spell-toggle` turns the marks on or off.
- `spell-mark` marks a misspelling in the editor when Enchant and a dictionary are present.
- `spell-not-in-project` leaves every project file unchanged.
- `spell-dictionary` stores a word the writer adds, outside the project.

## How to get to it (user POV)

- Press `F7`.
- Open `Main Menu` and choose `Check Spelling`.
- Open Preferences with `Ctrl+,` and use the spelling switch. That dialog is a separate path from `F7`.

## Driving it with verify-omaquill

Preconditions:

- The window is healthy and a scene is open.
- `ldconfig -p` lists `libenchant-2.so.2`, and a dictionary package such as `hunspell-en-us` is installed. If either is missing, stop and record that. Do not report the marks as verified.
- Hash The Storm `content.rtf` before toggling.
- This run's settings started from the default, so spelling marks begin on.

- **Toggle.** Press `F7`. Run `xdotool key F7`. This run's `settings.conf` contains `spellcheck = false`. Press `F7` again. The file contains `spellcheck = true`.
- **Mark.** With marks on, focus the editor and type `helo` at the end of the scene. Move the caret off that word. A screenshot shows the mark. Right-click the word. The menu includes `hello`, `Add to Dictionary`, and `Ignore`.
- **Project file.** Before saving the typed word, compare The Storm `content.rtf` to the hash from before the toggle. It matches. The mark is not in the RTF.
- **Added word.** Choose `Add to Dictionary` for `helo`. `$XDG_STATE_HOME/omaquill/dictionary` contains `helo`. The `.scriv` copy does not.

This recipe was not the run that proved the skill.

## Gotchas

- Enchant is loaded with `dlopen`. A build without the library still runs. Missing library or missing dictionary means no marks, not a failed save.
- `F7` writes `settings.conf` under this run's `XDG_CONFIG_HOME`. It does not write the project.
- The word at the caret is hidden while `spell_hide_current` is on, which is the default. Click away from `helo` before expecting its mark.
- Words added and words ignored are files under the state directory. They are not inside the `.scriv` folder.

---
name: verify-omaquill
description: "Drive omaquill the way a writer does (open a Scrivener project, edit, find and replace, spellcheck, compile) and keep proof. Use before calling a behavior change done, when reviewing a change, or when asked to verify omaquill."
---

# Verify omaquill

`mise lint` and `mise test` prove the code. This skill proves the window.
It builds the binary from this checkout, opens a copy of the Lighthouse
fixture, drives a feature the way a writer would, and keeps the evidence.
This repo has no `scripts/verify.sh`. Do not add one in a behavior change.
The agent that built a change does not sign off on this run.

## Launch

Run from the repo root. Every run gets its own display, session bus, and
directories. Do not point `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, or
`XDG_DATA_HOME` at the user account. Do not open
`tests/fixtures/Lighthouse.scriv` itself. Copy it.

```sh
mise install
cargo build --locked
run=$(mktemp -d /tmp/omaquill-verify.XXXXXX)
export XDG_CONFIG_HOME="$run/config"
export XDG_STATE_HOME="$run/state"
export XDG_DATA_HOME="$run/data"
export XDG_CACHE_HOME="$run/cache"
mkdir -p "$XDG_CONFIG_HOME/omaquill" "$XDG_STATE_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
printf 'backups = 0\n' > "$XDG_CONFIG_HOME/omaquill/settings.conf"
export OMAQUILL_APP_ID="io.github.shieldsworks.OmaquillVerify$$"
export GDK_BACKEND=x11
export GTK_A11Y=atspi
cp -a tests/fixtures/Lighthouse.scriv "$run/Lighthouse.scriv"
```

The default backup count is 10. The settings line above sets it to 0 so
open does not zip the copy. Start one accessibility bus with
`/usr/libexec/at-spi-bus-launcher --launch-immediately`, and wait until
`org.a11y.Bus.GetAddress` returns. Do not start `at-spi2-registryd`
yourself. The bus launcher starts the registry. A second registry leaves
the accessibility tree empty. Then start `./target/debug/omaquill` on the
copy. All of that runs inside `xvfb-run -a -s "-screen 0 1400x900x24"` and
`dbus-run-session`.

Ready when the pid is alive and `xdotool search --name "Lighthouse — omaquill"`
prints a window id. The title character between the project name and
`omaquill` is an em dash. Xvfb has no window manager. Use `xdotool
windowfocus`, then `xdotool key` with no `--window`. Key events sent with
`--window` do not reach this GTK build.

`drive-find-replace.sh` is this launch, the replace drive, and the cleanup.
Run it to repeat that proof.

```sh
bash .cursor/skills/verify-omaquill/drive-find-replace.sh
```

## Doctor

```sh
./target/debug/omaquill --version
kill -0 "$(cat "$run/app.pid")" && echo "pid $(cat "$run/app.pid") alive"
xdotool search --name "Lighthouse — omaquill"
```

The version line is this checkout's binary. The pid is the process this
run started. The window title is `Lighthouse — omaquill`. In Python,
`pyatspi.Registry.getDesktop(0)` has one app named `omaquill`. Drive only
that app. `libEGL` and Adwaita warnings under Xvfb still leave a passed
doctor when the pid is alive and the window title matches.

Spellcheck also needs `libenchant-2.so.2` and a dictionary such as
`hunspell-en-us`. The binary loads Enchant with `dlopen`, so a missing
library still lets the app start and leaves the red marks off.

## Drive

Follow `features/`. Keys and accessible names from this window:

| Writer action | Handle |
|---|---|
| Focus the editor | `ctrl+alt+2` |
| Find in the document | `ctrl+f`, placeholder `Find in document` |
| Find and replace | `ctrl+h`. Text `Find`, text `Replace with`, button `Replace All` |
| Search the project | `ctrl+shift+f`, placeholder `Search project` |
| Save | `ctrl+s` |
| Quit | `ctrl+q` |
| Spelling marks | `F7`, or the menu button `Main Menu` then `Check Spelling` |

Set text with `queryEditableText().setTextContents`. Click with
`queryAction().doAction(0)`. Read the chapter back from the text widget
whose name is empty and whose text starts with `The storm came in`.

The first open shows The Storm, the first text document in Manuscript.

## Evidence

Write under `$run/artifacts/verify/<feature-id>/`. Copy that directory out
of `/tmp` if it has to outlive the machine.

- Save the command, stdout, stderr, and the exit code.
- Save the editor text and the toast name from the accessibility tree.
- Save a PNG of the window after the click and after the save.
- The second look at a manuscript edit is `content.rtf` for that document,
  then `omaquill check` and `omaquill compile` on the copy.
- Hash the other `content.rtf` files before the drive. They stay equal.
- `omaquill check` writes nothing. Confirm the fixture in the repo is
  unchanged.
- If a path cannot be reached, write the command and the missing
  precondition. Do not mark that path passed.

## Cleanup

The shell `EXIT` trap kills the app pid and the accessibility bus pid this
run started. It does not delete `$run`.

```sh
kill "$(cat "$run/app.pid")" 2>/dev/null || true
for _ in $(seq 40); do
  kill -0 "$(cat "$run/app.pid")" 2>/dev/null || break
  sleep 0.1
done
```

Kill only those pids. Do not kill by process name. `xvfb-run` removes its
own display when the inner command exits. Evidence stays under
`$run/artifacts`. After cleanup, `test -s` the PNG and the text files there.

When the window's keys, accessible names, or the lines `check` prints
change, run `/maintain-verification-skill` over this directory.

#!/bin/bash
set -euo pipefail
if [[ -z "${OMAQUILL_VERIFY_INNER:-}" ]]; then
  export OMAQUILL_VERIFY_INNER=1
  exec xvfb-run -a -s "-screen 0 1400x900x24" dbus-run-session -- bash "$0" "$@"
fi

root=$(cd "$(dirname "$0")/../../.." && pwd)
bin="$root/target/debug/omaquill"
if [[ ! -x "$bin" ]]; then
  echo "missing $bin. From the repo root, run: cargo build --locked" >&2
  exit 1
fi

RUN=$(mktemp -d /tmp/omaquill-verify.XXXXXX)
export RUN
echo "$RUN" >"${OMAQUILL_VERIFY_RUN_FILE:-/tmp/omaquill-verify-last-run}"
art="$RUN/artifacts/verify/find-replace"
mkdir -p "$art"
export XDG_CONFIG_HOME="$RUN/config"
export XDG_STATE_HOME="$RUN/state"
export XDG_DATA_HOME="$RUN/data"
export XDG_CACHE_HOME="$RUN/cache"
mkdir -p "$XDG_CONFIG_HOME/omaquill" "$XDG_STATE_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
printf 'backups = 0\n' >"$XDG_CONFIG_HOME/omaquill/settings.conf"
export OMAQUILL_APP_ID="io.github.shieldsworks.OmaquillVerify$$"
export GDK_BACKEND=x11
export GTK_A11Y=atspi

proj="$RUN/Lighthouse.scriv"
cp -a "$root/tests/fixtures/Lighthouse.scriv" "$proj"
storm="$proj/Files/Data/3C8B2F4D-5A6E-4F7B-9CAD-1E2F3A4B5C03/content.rtf"
keeper="$proj/Files/Data/9CEB8FAD-BACE-4FDB-FCAD-7E8F9AABBC09/content.rtf"
morning="$proj/Files/Data/4D9C3A5E-6B7F-4A8C-ADBE-2F3A4B5C6D04/content.rtf"
cutscene="$proj/Files/Data/8BDA7E9C-AFBD-4ECA-EBFC-6D7E8F9AAB08/content.rtf"
sha256sum "$storm" "$keeper" "$morning" "$cutscene" >"$art/rtf-before.sha256"

pids=()
cleanup() {
  local pid
  for pid in "${pids[@]+"${pids[@]}"}"; do
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
    fi
  done
}
trap cleanup EXIT

/usr/libexec/at-spi-bus-launcher --launch-immediately >"$RUN/atspi-bus.log" 2>&1 &
pids+=($!)
ready=0
for _ in $(seq 50); do
  if gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus --method org.a11y.Bus.GetAddress >"$art/a11y-address.txt" 2>/dev/null; then
    ready=1
    break
  fi
  sleep 0.1
done
if [[ "$ready" != 1 ]]; then
  echo "accessibility bus did not answer" >&2
  exit 1
fi

"$bin" --version | tee "$art/version.txt"
"$bin" "$proj" >"$RUN/app.log" 2>&1 &
app_pid=$!
pids+=($app_pid)
echo "$app_pid" >"$RUN/app.pid"

wid=""
for _ in $(seq 50); do
  wid=$(xdotool search --name "Lighthouse — omaquill" | head -1 || true)
  if [[ -n "$wid" ]]; then
    break
  fi
  sleep 0.1
done
if [[ -z "$wid" ]]; then
  echo "window did not appear" >&2
  cat "$RUN/app.log" >&2 || true
  exit 1
fi
echo "$wid" >"$art/window-id.txt"
sleep 0.5
if ! kill -0 "$app_pid" 2>/dev/null; then
  echo "app exited before the drive" >&2
  exit 1
fi
echo "pid $app_pid alive" | tee "$art/doctor.txt"
echo "window Lighthouse — omaquill" | tee -a "$art/doctor.txt"
xdotool windowfocus "$wid" || true
xdotool key --delay 30 ctrl+alt+2
sleep 0.15
xdotool key --delay 30 ctrl+h
sleep 0.5

export ART="$art"
set +e
python3 - <<'PY'
import os, sys, time, pyatspi
art = os.environ["ART"]
desktop = pyatspi.Registry.getDesktop(0)
app = None
for i in range(desktop.childCount):
    candidate = desktop.getChildAtIndex(i)
    if candidate.name == "omaquill":
        app = candidate
        break
if app is None:
    sys.exit("omaquill is not in the accessibility tree")

def collect(obj, acc):
    acc.append(obj)
    try:
        count = obj.childCount
    except Exception:
        return
    for i in range(count):
        try:
            collect(obj.getChildAtIndex(i), acc)
        except Exception:
            pass

def find(nodes, role, name):
    for obj in nodes:
        try:
            if obj.getRoleName() == role and (obj.name or "") == name:
                return obj
        except Exception:
            pass
    return None

nodes = []
collect(app, nodes)
find_row = find(nodes, "text", "Find")
replace_row = find(nodes, "text", "Replace with")
button = find(nodes, "push button", "Replace All")
if find_row is None or replace_row is None or button is None:
    sys.exit("Find, Replace with, or Replace All is missing")

def set_text(obj, value):
    ok = obj.queryEditableText().setTextContents(value)
    got = obj.queryText().getText(0, 200)
    if not ok or got != value:
        sys.exit(f"{obj.name} stayed {got!r}")

set_text(find_row, "lamp")
set_text(replace_row, "lantern")
action = button.queryAction()
if not action.doAction(0):
    sys.exit("Replace All did not run")
time.sleep(0.4)
nodes = []
collect(app, nodes)
document = ""
for obj in nodes:
    try:
        if obj.getRoleName() != "text" or (obj.name or "") != "":
            continue
        text = obj.queryText().getText(0, 500)
    except Exception:
        continue
    if "The storm came in" in text:
        document = text
        break
toast = find(nodes, "label", "Replaced 1")
if "lantern" not in document or "lit the lamp" in document:
    sys.exit(f"editor text is {document!r}")
if toast is None:
    sys.exit("toast Replaced 1 is missing")
open(os.path.join(art, "editor.txt"), "w").write(document + "\n")
open(os.path.join(art, "toast.txt"), "w").write("Replaced 1\n")
print("editor updated")
print("toast Replaced 1")
PY
drive_code=$?
set -e
echo "$drive_code" >"$art/drive.exit"
test "$drive_code" = 0

xwd -id "$wid" -silent -out "$art/after-click.xwd"
xwdtopnm "$art/after-click.xwd" 2>/dev/null | pnmtopng >"$art/after-click.png"
xdotool key Escape
sleep 0.2
xdotool key ctrl+s
saved=0
for _ in $(seq 30); do
  if grep -q lantern "$storm" && ! grep -q lamp "$storm"; then
    saved=1
    break
  fi
  sleep 0.1
done
if [[ "$saved" != 1 ]]; then
  echo "content.rtf did not save lantern" >&2
  exit 1
fi
cp "$storm" "$art/storm-after.rtf"
xwd -id "$wid" -silent -out "$art/saved.xwd"
xwdtopnm "$art/saved.xwd" 2>/dev/null | pnmtopng >"$art/saved.png"
xdotool key ctrl+q
for _ in $(seq 40); do
  if ! kill -0 "$app_pid" 2>/dev/null; then
    break
  fi
  sleep 0.1
done
if kill -0 "$app_pid" 2>/dev/null; then
  echo "app still running after ctrl+q" >&2
  exit 1
fi

for unchanged in "$keeper" "$morning" "$cutscene"; do
  before=$(grep -F "$unchanged" "$art/rtf-before.sha256")
  now=$(sha256sum "$unchanged")
  if [[ "$before" != "$now" ]]; then
    echo "unexpected write: $unchanged" >&2
    exit 1
  fi
done
grep -q "lamp oil" "$keeper"

set +e
"$bin" check "$proj" >"$art/check.txt" 2>"$art/check.err"
echo $? >"$art/check.exit"
"$bin" compile "$proj" "$art/lighthouse.txt" --format txt >"$art/compile-stdout.txt" 2>"$art/compile.err"
echo $? >"$art/compile.exit"
set -e
test "$(cat "$art/check.exit")" = 0
test "$(cat "$art/compile.exit")" = 0
grep -q "every document reads fully" "$art/check.txt"
grep -q "Mara lit the lantern" "$art/lighthouse.txt"
grep -q "By morning the sea had gone flat" "$art/lighthouse.txt"
if grep -q "lamp oil" "$art/lighthouse.txt"; then
  echo "compile included the research folder" >&2
  exit 1
fi
git -C "$root" status --short -- tests/fixtures/Lighthouse.scriv >"$art/fixture-status.txt"
test ! -s "$art/fixture-status.txt"
cp "$RUN/app.log" "$art/app.log"
test -s "$art/after-click.png"
test -s "$art/saved.png"
test -s "$art/editor.txt"
test -s "$art/toast.txt"
echo 0 >"$art/helper.exit"
echo "evidence $art"
echo "RUN=$RUN"

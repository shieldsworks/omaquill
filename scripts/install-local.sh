#!/usr/bin/env bash
# Installs omaquill for this user from this checkout: builds a release,
# links the binary into ~/.local/bin, and adds the launcher entry, icon and
# .scrivx file type. With --plugin, also adds the Omarchy bar widget.
# Run it again after pulling to update.
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
repo=$PWD
data=${XDG_DATA_HOME:-${HOME:?}/.local/share}
bin=${HOME}/.local/bin

cargo build --release --locked
mkdir -p -- "$bin" "$data/applications" "$data/icons/hicolor/scalable/apps" "$data/mime/packages"
ln -sfn -- "$repo/target/release/omaquill" "$bin/omaquill"
install -m644 data/io.github.shieldsworks.Omaquill.desktop "$data/applications/"
install -m644 data/io.github.shieldsworks.Omaquill.svg "$data/icons/hicolor/scalable/apps/"
install -m644 data/io.github.shieldsworks.Omaquill.mime.xml "$data/mime/packages/"
command -v update-mime-database >/dev/null && update-mime-database "$data/mime" >/dev/null 2>&1 || true
command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" >/dev/null 2>&1 || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -qtf "$data/icons/hicolor" 2>/dev/null || true
echo "omaquill installed: $bin/omaquill"

if [[ ${1:-} == --plugin ]]; then
  plugins=${XDG_CONFIG_HOME:-$HOME/.config}/omarchy/plugins
  mkdir -p -- "$plugins"
  ln -sfn -- "$repo" "$plugins/org.omaquill.words"
  if command -v omarchy >/dev/null; then
    omarchy plugin enable org.omaquill.words >/dev/null 2>&1 || true
  fi
  echo "bar widget linked: $plugins/org.omaquill.words"
fi

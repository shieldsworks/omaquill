#!/usr/bin/env bash
# Removes what install-local.sh added: the omaquill command, launcher entry,
# icon and .scrivx file type. Leaves your projects, omaquill's settings and
# state, and your backups alone. The bar widget is removed with
# `omarchy plugin remove io.github.shieldsworks.omaquill`.
set -euo pipefail

data=${XDG_DATA_HOME:-${HOME:?}/.local/share}
rm -f -- "$HOME/.local/bin/omaquill" \
  "$data/applications/io.github.shieldsworks.Omaquill.desktop" \
  "$data/icons/hicolor/scalable/apps/io.github.shieldsworks.Omaquill.svg" \
  "$data/mime/packages/io.github.shieldsworks.Omaquill.mime.xml"
command -v update-mime-database >/dev/null && update-mime-database "$data/mime" >/dev/null 2>&1 || true
command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" >/dev/null 2>&1 || true
echo "omaquill removed. Kept: ~/.config/omaquill, ~/.local/state/omaquill, ~/.local/share/omaquill/backups"

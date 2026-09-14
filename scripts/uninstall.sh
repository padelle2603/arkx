#!/bin/bash
# Remove every file that scripts/install.sh put into system locations:
# the binary, the legacy symlink, the desktop entry, all icon sizes and the
# Dolphin service menus. Home-local leftovers (e.g. the upkeep AppImage or
# ~/.local/share/kio/servicemenus installed by install-menus.sh) are out of
# scope: this script only undoes the system-wide `sudo install` steps.
set -euo pipefail

echo "==> Removing binaries..."
sudo rm -f /usr/local/bin/arkx
sudo rm -f /usr/local/bin/extractor

echo "==> Removing desktop entries..."
sudo rm -f /usr/share/applications/arkx.desktop
sudo rm -f /usr/share/applications/extractor.desktop

echo "==> Removing icons..."
sudo rm -f /usr/share/icons/hicolor/scalable/apps/arkx.svg
for s in 16 32 48 64 128 256; do
  sudo rm -f "/usr/share/icons/hicolor/${s}x${s}/apps/arkx.png"
done

echo "==> Removing Dolphin service menus..."
sudo rm -f /usr/share/kio/servicemenus/arkx-compress.desktop
sudo rm -f /usr/share/kio/servicemenus/arkx-extract.desktop

echo "==> Refreshing caches..."
sudo update-desktop-database /usr/share/applications 2>/dev/null || true
sudo gtk-update-icon-cache /usr/share/icons/hicolor 2>/dev/null || true
kbuildsycoca6 --noincremental 2>/dev/null || true

echo "==> Verifying..."
if command -v arkx >/dev/null 2>&1; then
  echo "Note: 'arkx' still resolves to $(command -v arkx)" >&2
  echo "      (another copy in PATH, e.g. ~/.local/bin or an AppImage)" >&2
else
  echo "arkx removed from PATH."
fi
echo "Done."
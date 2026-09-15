#!/bin/bash
# Remove every file that scripts/install.sh put into system locations:
# the binary, the legacy symlink, the desktop entry, all icon sizes and the
# Dolphin service menus. Home-local leftovers (e.g. the upkeep AppImage or
# ~/.local/share/kio/servicemenus installed by install-menus.sh) are out of
# scope: this script only undoes the system-wide `sudo install` steps.
set -euo pipefail

VERBOSE=0
for arg in "$@"; do
    case "$arg" in
        --verbose) VERBOSE=1 ;;
    esac
done
_vlog()  { if [ "$VERBOSE" -eq 1 ]; then echo "  $1"; fi; }
_warn()  { echo "  ! $1" >&2; }

echo "arkx - Uninstallation"

_vlog "Removing binaries..."
sudo rm -f /usr/local/bin/arkx
sudo rm -f /usr/local/bin/extractor

_vlog "Removing desktop entries..."
sudo rm -f /usr/share/applications/arkx.desktop
sudo rm -f /usr/share/applications/extractor.desktop

_vlog "Removing icons..."
sudo rm -f /usr/share/icons/hicolor/scalable/apps/arkx.svg
for s in 16 32 48 64 128 256; do
  sudo rm -f "/usr/share/icons/hicolor/${s}x${s}/apps/arkx.png"
done

_vlog "Removing Dolphin service menus..."
sudo rm -f /usr/share/kio/servicemenus/arkx-compress.desktop
sudo rm -f /usr/share/kio/servicemenus/arkx-extract.desktop

_vlog "Refreshing caches..."
sudo update-desktop-database /usr/share/applications 2>/dev/null || true
sudo gtk-update-icon-cache /usr/share/icons/hicolor 2>/dev/null || true
kbuildsycoca6 --noincremental 2>/dev/null || true

if command -v arkx >/dev/null 2>&1; then
  _warn "'arkx' still resolves to $(command -v arkx) (another copy in PATH, e.g. ~/.local/bin or an AppImage)"
fi

echo ""
echo "Removed: binary, desktop entries, icons and Dolphin service menus."
echo "Done."
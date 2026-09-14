#!/bin/bash
set -e
# Run from anywhere: resolve the repo root (cwd is not guaranteed).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.."
echo "==> Building arkx (release, optimized for $(nproc) threads)..."
cargo build --release
echo "==> Installing to /usr/local/bin/arkx..."
sudo install -Dm755 target/release/arkx /usr/local/bin/arkx
sudo install -Dm644 data/arkx.desktop /usr/share/applications/arkx.desktop
# App icons (SVG + rendered PNGs)
sudo install -Dm644 data/icons/hicolor/scalable/apps/arkx.svg /usr/share/icons/hicolor/scalable/apps/arkx.svg
for s in 16 32 48 64 128 256; do
  sudo install -Dm644 "data/icons/hicolor/${s}x${s}/apps/arkx.png" "/usr/share/icons/hicolor/${s}x${s}/apps/arkx.png"
done
# Backwards compatibility with the old name (scripts, file-manager entries)
sudo ln -sf /usr/local/bin/arkx /usr/local/bin/extractor || true
# Remove the legacy desktop entry if present
sudo rm -f /usr/share/applications/extractor.desktop || true
# Dolphin integration (Ark parity): Compress / Extract service menus.
# Replaces the "Compress" entry lost when uninstalling Ark.
sudo install -Dm644 data/servicemenus/arkx-compress.desktop /usr/share/kio/servicemenus/arkx-compress.desktop
sudo install -Dm644 data/servicemenus/arkx-extract.desktop /usr/share/kio/servicemenus/arkx-extract.desktop
sudo update-desktop-database /usr/share/applications || true
# Reload Dolphin service menus (harmless if kbuildsycoca6 is missing)
kbuildsycoca6 --noincremental 2>/dev/null || true
sudo gtk-update-icon-cache /usr/share/icons/hicolor || true
echo "==> Verifying..."
arkx --version
arkx --help | head -n 20
echo "==> CLI smoke test..."
SMOKE_DIR="$(mktemp -d)"
echo "test" > "$SMOKE_DIR/a.txt"
arkx a "$SMOKE_DIR/t.zip" "$SMOKE_DIR/a.txt" >/dev/null 2>&1
if arkx l "$SMOKE_DIR/t.zip" >/dev/null 2>&1 && [ "$(arkx l "$SMOKE_DIR/t.zip" 2>/dev/null | grep -c 'a.txt')" -eq 1 ]; then
    echo "Smoke test passed ✓"
else
    echo "WARNING: smoke test failed" >&2
fi
rm -rf "$SMOKE_DIR"
echo "==> Done! Launch with: arkx  or  arkx archive.zip"
echo "    Dolphin: right click a folder -> Compress -> Compress to zip... / tar.gz... / 7zip..."
echo "             right click an archive -> Extract -> Extract here"

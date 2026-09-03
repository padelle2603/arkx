#!/bin/bash
set -e
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
sudo update-desktop-database /usr/share/applications || true
sudo gtk-update-icon-cache /usr/share/icons/hicolor || true
echo "==> Verifying..."
arkx --version
arkx --help | head -n 20
echo "==> CLI smoke test..."
arkx l /tmp/test_arkx.zip 2>&1 | head -n 10 || echo "create a test zip: 7z a /tmp/test.zip file..."
echo "==> Done! Launch with: arkx  or  arkx archive.zip"
echo "    Dolphin integration: right click -> Open with -> Arkx"

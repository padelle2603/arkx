#!/bin/bash
# Build a portable Arkx AppImage with linuxdeploy.
#
# Usage: ./appimage/build-appimage.sh [VERSION]
# Expects: cargo project root as CWD, linuxdeploy (+gtk plugin) on PATH or
#   LINUXDEPLOY= /LINUXDEPLOY_GTK= pointing at them (CI downloads them).
set -euo pipefail

VERSION="${1:-$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)}"
ARCH="${ARCH:-x86_64}"
APPDIR="${APPDIR:-AppDir}"
OUT="Arkx-${VERSION}-${ARCH}.AppImage"

LINUXDEPLOY="${LINUXDEPLOY:-linuxdeploy}"
LINUXDEPLOY_GTK="${LINUXDEPLOY_GTK:-linuxdeploy-plugin-gtk.sh}"

echo "==> Building arkx ${VERSION} (release)..."
cargo build --release --locked

echo "==> Deploying to ${APPDIR}..."
rm -rf "${APPDIR}"
# First pass populates AppDir (no --output yet: 7z + icons go in next).
BSDTAR="$(command -v bsdtar || true)"
"${LINUXDEPLOY}" --appdir "${APPDIR}" \
  -e target/release/arkx \
  ${BSDTAR:+-e "${BSDTAR}"} \
  -d data/arkx.desktop \
  -i data/icons/hicolor/256x256/apps/arkx.png \
  --plugin gtk

echo "==> Bundling 7z backend..."
SEVEN="$(command -v 7z)"
cp "${SEVEN}" "${APPDIR}/usr/bin/"
# p7zip keeps its codecs next to the lib: ship them so RAR/7Z keep working.
if [ -d /usr/lib/p7zip ]; then
  mkdir -p "${APPDIR}/usr/lib"
  cp -r /usr/lib/p7zip "${APPDIR}/usr/lib/"
fi

echo "==> Bundling icons + legal docs..."
mkdir -p "${APPDIR}/usr/share/icons"
cp -r data/icons/hicolor "${APPDIR}/usr/share/icons/"
cp data/arkx.desktop "${APPDIR}/"
cp "data/icons/hicolor/256x256/apps/arkx.png" "${APPDIR}/arkx.png"
mkdir -p "${APPDIR}/usr/share/doc/arkx"
cp LEGAL.md PRIVACY.md README.md "${APPDIR}/usr/share/doc/arkx/"

echo "==> Smoke test inside AppDir..."
"${APPDIR}/usr/bin/arkx" --version
test -x "${APPDIR}/usr/bin/7z" || { echo "7z missing from AppDir!"; exit 1; }

echo "==> Baking ${OUT}..."
OUTPUT="${OUT}" "${LINUXDEPLOY}" --appdir "${APPDIR}" --output appimage

echo "==> Done: ${OUT}"
ls -la "${OUT}"

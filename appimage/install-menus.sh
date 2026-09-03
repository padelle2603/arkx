#!/bin/bash
# Install Dolphin Compress/Extract menus for the Arkx AppImage
# managed by upkeep — all user-local, no sudo.
#
# Reads the AppImage path from the upkeep-generated .desktop
# (source of truth: stays valid after `upkeep update`, which replaces
# the file in place), extracts the ServiceMenu templates from the
# AppImage itself and installs them into ~/.local/share/kio/servicemenus/ with
# Exec/TryExec/Icon rewritten to the real path.
#
# Usage:
#   ./appimage/install-menus.sh [--appimage PATH] [--force] [--no-icon]
#   ./appimage/install-menus.sh --uninstall
set -euo pipefail

DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
UPKEEP_DESKTOP="$DATA_HOME/applications/arkx.desktop"
DEST_DIR="$DATA_HOME/kio/servicemenus"
FILES="arkx-compress.desktop arkx-extract.desktop"

APPIMAGE=""
FORCE=0
WITH_ICON=1
UNINSTALL=0

usage() {
    echo "Usage: $(basename "$0") [--appimage PATH] [--force] [--no-icon]"
    echo "       $(basename "$0") --uninstall"
    echo ""
    echo "Installs Dolphin Compress/Extract menus for the upkeep-managed Arkx AppImage."
}

while [ $# -gt 0 ]; do
    case "$1" in
        --appimage) APPIMAGE="${2:?missing path}"; shift 2 ;;
        --force) FORCE=1; shift ;;
        --no-icon) WITH_ICON=0; shift ;;
        --uninstall) UNINSTALL=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown option: $1" >&2; usage >&2; exit 1 ;;
    esac
done

refresh_dolphin() {
    if command -v kbuildsycoca6 >/dev/null 2>&1; then
        kbuildsycoca6 --noincremental >/dev/null 2>&1 || true
    fi
}

if [ "$UNINSTALL" -eq 1 ]; then
    for f in $FILES; do
        rm -f "$DEST_DIR/$f"
    done
    refresh_dolphin
    echo "Arkx service menus removed from $DEST_DIR."
    exit 0
fi

# 1. Find the AppImage: upkeep .desktop > --appimage > search.
# Robust parsing with shlex (handles paths with spaces and quotes).
if [ -z "$APPIMAGE" ] && [ -f "$UPKEEP_DESKTOP" ]; then
    APPIMAGE="$(python3 -c '
import shlex, sys
try:
    with open(sys.argv[1], encoding="utf-8") as fh:
        for line in fh:
            if line.startswith("Exec="):
                parts = shlex.split(line[len("Exec="):].strip())
                if parts:
                    print(parts[0])
                    break
except Exception:
    pass
' "$UPKEEP_DESKTOP" | head -n1)"
fi
if [ -z "$APPIMAGE" ]; then
    for cand in "$HOME/Applicazioni"/Arkx*.AppImage "$DATA_HOME/applications/Appimages/Arkx.AppImage"; do
        if [ -x "$cand" ]; then APPIMAGE="$cand"; break; fi
    done
fi
if [ -z "$APPIMAGE" ]; then
    echo "Error: Arkx AppImage not found (tried upkeep desktop + --appimage)." >&2
    echo "Run: $(basename "$0") --appimage /path/to/Arkx.AppImage" >&2
    exit 1
fi
# The AppImage must be executable: after `upkeep update` the file is
# replaced and may lose +x (on NTFS/exFAT/noexec it will never be
# executable). Without +x Dolphin shows the menu but then fails with
# "not authorized to run the application".
if [ ! -e "$APPIMAGE" ]; then
    echo "Error: AppImage not found: $APPIMAGE" >&2
    exit 1
fi
if [ ! -x "$APPIMAGE" ]; then
    echo "AppImage not executable, trying chmod +x: $APPIMAGE"
    chmod +x "$APPIMAGE" 2>/dev/null || true
fi
if [ ! -x "$APPIMAGE" ]; then
    echo "Error: AppImage is not executable: $APPIMAGE" >&2
    echo "If it is on NTFS/exFAT or a noexec mount, move it to an ext4" >&2
    echo "directory (e.g. ~/.local/share/applications/Appimages/) and re-run." >&2
    exit 1
fi
APPIMAGE="$(realpath "$APPIMAGE")"
echo "AppImage: $APPIMAGE"

# FUSE is required for type-2 AppImages: without it, launching from Dolphin
# fails while the real FUSE error would only be visible from a terminal.
if [ ! -e /dev/fuse ] && ! command -v fusermount3 >/dev/null 2>&1 && ! command -v fusermount >/dev/null 2>&1; then
    echo "Warning: FUSE not found (/dev/fuse or fusermount missing)." >&2
    echo "The AppImage may fail to start: install fuse/libfuse2 for your distro." >&2
fi

# 2. Check --progress support (introduced after v1.0.1).
# Note: old AppImages ignore 'compress' and open the GUI (which stays
# hung): timeout so we never block. Output must be captured before
# grep because the script runs with 'pipefail' and the AppImage exits 1 on --help.
HELP_OUT="$(timeout 15 "$APPIMAGE" compress --help 2>&1 || true)"
if ! printf '%s\n' "$HELP_OUT" | grep -q -- "--progress"; then
    echo "Error: this AppImage is too old (no 'compress --progress')." >&2
    echo "Run 'upkeep update arkx' first to fetch a release with file-manager support." >&2
    [ "$FORCE" -eq 1 ] || exit 1
    echo "(--force: installing anyway)"
fi

# 3. Templates: from the AppImage itself, fallback to the repo checkout.
TMPD="$(mktemp -d)"
trap 'rm -rf "$TMPD"' EXIT
TEMPLATE_DIR=""
if (cd "$TMPD" && "$APPIMAGE" --appimage-extract "usr/share/kio/servicemenus/*.desktop" >/dev/null 2>&1) \
    && [ -n "$(ls "$TMPD"/squashfs-root/usr/share/kio/servicemenus/*.desktop 2>/dev/null)" ]; then
    TEMPLATE_DIR="$TMPD/squashfs-root/usr/share/kio/servicemenus"
    echo "Templates: extracted from AppImage."
else
    SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
    if ls "$SCRIPT_DIR/../data/servicemenus/"*.desktop >/dev/null 2>&1; then
        TEMPLATE_DIR="$SCRIPT_DIR/../data/servicemenus"
        echo "Templates: from repo checkout."
    else
        echo "Error: no service-menu templates (AppImage too old and no repo checkout)." >&2
        exit 1
    fi
fi

# 4. Icon: reuse the one already extracted by upkeep, if requested.
ICON=""
if [ "$WITH_ICON" -eq 1 ] && [ -f "$UPKEEP_DESKTOP" ]; then
    ICON="$(sed -n 's/^Icon=//p' "$UPKEEP_DESKTOP" | head -n1)"
    case "$ICON" in
        ""|"application-x-executable") ICON="" ;;
        *) [ -f "$ICON" ] || ICON="" ;;
    esac
fi

# 5. Generate the files (python3 transformation: robust to spaces and special chars).
# KDE/Plasma 6 note: user-local ServiceMenus NOT owned by root must have
# the executable flag, otherwise Dolphin logs
# `Access ... denied, not owned by root and executable flag not set`
# and shows "not authorized to run the application".
mkdir -p "$DEST_DIR"
export APPIMAGE ICON TEMPLATE_DIR DEST_DIR FILES
python3 - <<'EOF'
import glob, os, stat

appimage = os.environ["APPIMAGE"]
icon = os.environ.get("ICON") or ""
dest_dir = os.environ["DEST_DIR"]
files = os.environ["FILES"].split()
template_dir = os.environ["TEMPLATE_DIR"]

def quote(p: str) -> str:
    # Quote only if needed (spaces, quotes, backslash): TryExec stays bare per spec.
    if any(c in p for c in ' \t"\'' + "\\"):
        return '"' + p.replace("\\", "\\\\").replace('"', '\\"') + '"'
    return p

exec_prefix = "Exec=" + quote(appimage)

names = sorted(glob.glob(os.path.join(template_dir, "*.desktop")))
wanted = {os.path.join(template_dir, f) for f in files}
missing = sorted(wanted - set(names))
if missing:
    raise SystemExit(f"missing templates: {', '.join(missing)}")

for src in sorted(wanted):
    out_lines = []
    with open(src, encoding="utf-8") as fh:
        for line in fh.read().splitlines():
            if line.startswith("TryExec="):
                continue  # re-added below (bare, without quotes; omitted if path contains spaces)
            if line == "Exec=arkx" or line.startswith("Exec=arkx "):
                line = exec_prefix + line[len("Exec=arkx"):]
            elif line.startswith("Icon=arkx") and icon:
                line = "Icon=" + icon
            out_lines.append(line)
    # TryExec with the real bare path (per spec: single path, no quoting).
    # If the path contains spaces it cannot be represented in TryExec -> omit it
    # (the quoted Exec above stays valid and the menu stays visible).
    if any(c in appimage for c in ' \t"'):
        pass
    else:
        # If the AppImage disappears, the menu hides itself.
        out_lines.insert(3, f'TryExec={appimage}')
    dst = os.path.join(dest_dir, os.path.basename(src))
    with open(dst, "w", encoding="utf-8") as fh:
        fh.write("\n".join(out_lines) + "\n")
    # KDE trust: +x required for user-local .desktops (see note above).
    st = os.stat(dst)
    os.chmod(dst, st.st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    print(f"installed {dst}")
EOF

refresh_dolphin
echo "Done. Right-click a folder in Dolphin -> Compress."
echo "(If Dolphin was open, restart it. Re-run after moving the AppImage or after 'upkeep update arkx'.)"
echo "Trust check (they must be -rwxr-xr-x, otherwise Dolphin denies launch):"
ls -l "$DEST_DIR"/arkx-*.desktop

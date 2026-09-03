#!/bin/bash
# Installa i menu Comprimi/Estrai di Dolphin per l'AppImage di Arkx
# gestita da upkeep — tutto user-local, niente sudo.
#
# Legge il percorso della AppImage dal .desktop generato da upkeep
# (source of truth: resta valido dopo `upkeep update`, che rimpiazza
# il file in place), estrae i template ServiceMenu dalla AppImage
# stessa e li installa in ~/.local/share/kio/servicemenus/ con
# Exec/TryExec/Icon riscritti sul percorso reale.
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
    echo "Installs Dolphin Comprimi/Estrai menus for the upkeep-managed Arkx AppImage."
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

# 1. Trova la AppImage: .desktop di upkeep > --appimage > ricerca.
if [ -z "$APPIMAGE" ] && [ -f "$UPKEEP_DESKTOP" ]; then
    # Exec="/path/Arkx.AppImage" (upkeep lo virgoletta sempre)
    APPIMAGE="$(sed -n 's/^Exec=//p' "$UPKEEP_DESKTOP" | head -n1 | sed 's/^"//; s/"$//; s/ .*//')"
fi
if [ -z "$APPIMAGE" ]; then
    for cand in "$HOME/Applicazioni"/Arkx*.AppImage "$DATA_HOME/applications/Appimages/Arkx.AppImage"; do
        if [ -x "$cand" ]; then APPIMAGE="$cand"; break; fi
    done
fi
if [ -z "$APPIMAGE" ] || [ ! -x "$APPIMAGE" ]; then
    echo "Error: Arkx AppImage not found (tried upkeep desktop + --appimage)." >&2
    echo "Run: $(basename "$0") --appimage /path/to/Arkx.AppImage" >&2
    exit 1
fi
APPIMAGE="$(realpath "$APPIMAGE")"
echo "AppImage: $APPIMAGE"

# 2. Verifica supporto --progress (introdotto dopo la v1.0.1).
# Nota: le AppImage vecchie ignorano 'compress' e aprono la GUI (che resta
# appesa): timeout per non bloccarsi mai. L'output va catturato prima del
# grep perché lo script gira con 'pipefail' e la AppImage esce 1 sul --help.
HELP_OUT="$(timeout 15 "$APPIMAGE" compress --help 2>&1 || true)"
if ! printf '%s\n' "$HELP_OUT" | grep -q -- "--progress"; then
    echo "Error: this AppImage is too old (no 'compress --progress')." >&2
    echo "Run 'upkeep update arkx' first to fetch a release with file-manager support." >&2
    [ "$FORCE" -eq 1 ] || exit 1
    echo "(--force: installing anyway)"
fi

# 3. Template: dalla AppImage stessa, fallback al checkout del repo.
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

# 4. Icona: riusa quella già estratta da upkeep, se richiesta.
ICON=""
if [ "$WITH_ICON" -eq 1 ] && [ -f "$UPKEEP_DESKTOP" ]; then
    ICON="$(sed -n 's/^Icon=//p' "$UPKEEP_DESKTOP" | head -n1)"
    case "$ICON" in
        ""|"application-x-executable") ICON="" ;;
        *) [ -f "$ICON" ] || ICON="" ;;
    esac
fi

# 5. Genera i file (trasformazione in python3: robusta a spazi e caratteri speciali).
mkdir -p "$DEST_DIR"
export APPIMAGE ICON TEMPLATE_DIR DEST_DIR FILES
python3 - <<'EOF'
import glob, os

appimage = os.environ["APPIMAGE"]
icon = os.environ.get("ICON") or ""
dest_dir = os.environ["DEST_DIR"]
files = os.environ["FILES"].split()
template_dir = os.environ["TEMPLATE_DIR"]

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
                continue  # re-added below with the real path
            if line == "Exec=arkx" or line.startswith("Exec=arkx "):
                line = 'Exec="' + appimage + '"' + line[len("Exec=arkx"):]
            elif line.startswith("Icon=arkx") and icon:
                line = "Icon=" + icon
            out_lines.append(line)
    # TryExec col path reale: se la AppImage sparisce, il menu si nasconde da solo.
    out_lines.insert(3, f'TryExec="{appimage}"')
    dst = os.path.join(dest_dir, os.path.basename(src))
    with open(dst, "w", encoding="utf-8") as fh:
        fh.write("\n".join(out_lines) + "\n")
    print(f"installed {dst}")
EOF

refresh_dolphin
echo "Done. Right-click a folder in Dolphin -> Comprimi."
echo "(If Dolphin was open, restart it. Re-run after moving the AppImage.)"

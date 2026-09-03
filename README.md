# Arkx

Fast, multi-threaded archive manager for Linux — open, browse and extract ZIP, 7Z, RAR, TAR and 20+ more formats, from a clean GTK4 interface or the terminal.

![License: GPL-3.0](https://img.shields.io/badge/License-GPL--3.0-blue) ![Rust](https://img.shields.io/badge/Rust-1.90-orange) ![GTK4](https://img.shields.io/badge/GTK4-blue)

## Download

Grab the latest portable build from the [Releases page](https://github.com/padelle2603/arkx/releases):

- **Arkx-x86_64.AppImage** — download, `chmod +x`, double-click. No install, no dependencies.

## Use

```bash
# GUI — drag an archive in, or open one from your file manager
arkx
arkx archive.zip

# CLI
arkx l archive.7z                 # list contents
arkx x archive.7z ~/Downloads    # extract (all CPU threads)
arkx x archive.rar . -p secret   # RAR with password
arkx a archive.7z docs/ -l 9     # create an archive
arkx a archive.tar.zst docs/ --threads 4  # cap worker threads (or ARKX_THREADS=4)
arkx c --here --format=zip docs/ # compress (Dolphin-style, no overwrite)
arkx --help                      # everything else
```

## Dolphin integration (replaces Ark's Compress menu)

Uninstalling Ark removes its right-click **Compress** entry (it was an Ark
plugin, not a Dolphin feature). `./install.sh` restores it with Arkx:

* Right-click folder/file → **Compress** → Compress to zip... / tar.gz... / 7zip...
* Right-click archive → **Extract** → Extract here / Extract to...

Headless by default (no window, no overwrite: `docs.zip`, `docs-2.zip`, …),
with desktop notification + highlight in Dolphin when done.
From the context menu Arkx shows the same progress window as the app
(bar with speed, ETA and Details); on success it closes itself, on error
it stays open with the message. `arkx compress --no-progress` forces
text mode (scripts, ssh).

### AppImage-only setup (upkeep workflow)

If you run Arkx as an AppImage managed by upkeep (no system install),
Dolphin can't see inside the AppImage — register the menus user-locally:

```bash
upkeep update arkx                       # needs a release with --progress support
./appimage/install-menus.sh              # reads the path from upkeep's .desktop, no sudo
./appimage/install-menus.sh --uninstall  # remove them again
```

The script points the menus at the upkeep AppImage path (stable across
updates) with its real icon. Re-run it after every `upkeep update arkx`
(the update can drop the executable bit) and if you move the AppImage —
otherwise Dolphin denies the launch ("not authorized to run the application").
Right-click files inside an archive to extract just what you selected. Wrong password? Arkx asks again instead of failing.

## Features

- **Never hangs**: every job runs on a background worker; the UI stays fluid and anything can be cancelled
- **Honest progress**: a real byte-based 0%→100% bar with speed, ETA and a details pane (Windows copy-dialog style) — no fake percentages
- **Hybrid backends**: native Rust streaming for ZIP/TAR plus multithreaded `7z` for RAR/7Z/ISO/CAB and friends, with automatic fallback
- **File-manager feel**: breadcrumb navigation, live filter, dark responsive layout, drag & drop
- **Resilient**: corrupt archives warn instead of crashing; long/emoji/CJK names display fine

## Build from source

```bash
sudo pacman -S gtk4 libadwaita 7zip libarchive   # or apt/dnf equivalents
cargo build --release
./target/release/arkx
```

Tests: `cargo test` · Lint: `cargo clippy` · Install: `./install.sh`
Bench (any machine): `./bench.sh` — adaptive threads/levels compared on a synthetic corpus.

## Privacy & Legal

Arkx works **100% offline** — no accounts, no telemetry, no network calls. See [PRIVACY.md](PRIVACY.md). License and third-party notices: [LEGAL.md](LEGAL.md) (GPL-3.0).

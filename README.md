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
arkx --help                      # everything else
```

Right-click files inside an archive to extract just what you selected. Wrong password? Arkx asks again instead of failing.

## Features

- **Never hangs**: every job runs on a background worker; the UI stays fluid and anything can be cancelled
- **Honest progress**: a real byte-based 0%→100% bar with speed, ETA and a details pane (Windows copy-dialog style) — no fake percentages
- **Hybrid backends**: native Rust streaming for ZIP/TAR plus multithreaded `7z` for RAR/7Z/ISO/CAB and friends, with automatic fallback
- **File-manager feel**: breadcrumb navigation, live filter, dark responsive layout, drag & drop
- **Resilient**: corrupt archives warn instead of crashing; long/emoji/CJK names display fine

## Build from source

```bash
sudo pacman -S gtk4 libadwaita 7zip   # or apt/dnf equivalents
cargo build --release
./target/release/arkx
```

Tests: `cargo test` · Lint: `cargo clippy` · Install: `./install.sh`

## Privacy & Legal

Arkx works **100% offline** — no accounts, no telemetry, no network calls. See [PRIVACY.md](PRIVACY.md). License and third-party notices: [LEGAL.md](LEGAL.md) (GPL-3.0).

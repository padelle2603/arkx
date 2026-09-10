# Changelog

## v1.2.0 — adaptive performance + huge archives

- **Adaptive threads**: `--threads N` (or `ARKX_THREADS=N`) on `a`/`c`/`x`;
  auto default from CPU/RAM (reserve for the system on small machines,
  RAM cap for hungry codecs). No more fixed per-machine values.
- **Honored levels**: `-l 0-9` applies to all tars (gz/bz2/xz/zstd,
  previously ignored with fixed levels).
- **Multithreaded zstd** on `tar.zst` (e.g. 69MB mixed: 5.6s → 0.09s at same ratio).
- **Large zips via 7z** (`-mmt`) above adaptive threshold with fallback to native.
- **Honest creation progress on huge archives**: byte-based floor
  for completed files (7z % with `-mmt` stalls), polling bytes
  read from `/proc` (moves even inside a single 10GB file),
  size map with double key (absolute + relative: 7z prints relative
  names), keepalive, 7z status lines filtered by filename;
  percentages with adaptive decimals (`0.42%` below 1%, locale decimal
  separator, never 100% early); disk-space preflight with clear error.
- Removed unused dependencies (`tokio`, `futures`, `memmap2`, `bytes`).

## v1.1.1 — Dolphin integration: Compress/Extract menus + progress window

Restores the right-click **Compress** entry lost when uninstalling Ark,
with Ark parity and the in-app progress window.

### File-manager integration

- **Compress submenu** (Dolphin ServiceMenu): Compress to zip... / tar.gz... /
  7zip... with Italian translations, no-overwrite naming (`docs.zip`, `docs-2.zip`…)
- **Extract submenu**: Extract here / Extract to... / Open with Arkx
- New CLI: `arkx compress [--here] [--format zip|tar.gz|7z] [--to DEST] [--dialog]`
  and `arkx extract --here/--dialog` (multi-archive aware, `file://` URI decoding)
- **Progress window** (`--progress`): same bar as the app (speed, ETA, Details),
  auto-closes on success with notification + highlight in Dolphin, stays open on
  error, Cancel kills the backend and deletes partials
- Real 7z creation progress (previously silent 0→100%); cancellable creates
  across native and 7z backends
- AppImage-only setup: `./scripts/install-menus.sh` registers the menus
  user-locally for upkeep-managed AppImages (no sudo); AppImage bundles the
  ServiceMenus as reference

## v1.0.1 — Full MIME coverage: 33 archive types

This release extends Arkx to every archive MIME type used by Linux file
managers (Nautilus, Dolphin, Thunar…): open, browse and extract them from
the GTK4 UI or the CLI, with honest byte-based progress throughout.

### New formats

- **Compressed tars**: `.tar.Z` / `.taz` (TAR.Z), `.tar.lzma` / `.tlz`
  (TAR.LZMA), `.tar.lz` (TAR.LZIP), `.tzo` / `.tar.lzo` (TAR.LZO),
  `.tar.lrz` (TAR.LRZIP)
- **Single-file codecs**: `.Z` (COMPRESS), `.lzma` (LZMA)
- **System archives**: CPIO family (`.cpio`, `.bcpio`, odc/new-ascii/binary),
  XAR (`.xar`, `.xip`), AR static libraries (`.a`, `.ar`),
  AppImage (`.AppImage`)
- **Aliases now recognized**: `application/x-rar`, `application/x-bzip`,
  `application/x-cd-image`, `application/x-source-rpm`,
  `application/x-lha`, plus all `*-compressed-tar` / `x-tzo` / `x-tarz`
  MIME variants

### New libarchive backend

A third backend based on `bsdtar` (with GNU `tar` fallback) handles the
tar flavors that neither the native streamer nor 7z can open
(lzip, lzo, lrzip, compress, lzma-alone). The backend manager now falls
back across native → 7z → libarchive, and exotic formats degrade to a
clear "install libarchive-tools" hint instead of an "unknown file" error.

### Fixed

- `.tar.lz4` archives failed to open (the stream was parsed as plain tar);
  they are now decoded natively via `lz4_flex`, including `.lz4`
  single files and `.tar.lz4` creation (`arkx a … .tar.lz4`)
- Files recognized only by extension (e.g. `.cpio`, `.a`, `.xar`) were
  reported as `unknown`: the extension fallback in format detection never
  ran — it does now
- Wrong `ar` magic-byte check (`!<arch>`) that never matched

### Integration

- `arkx.desktop`: all 33 MIME types registered, so Arkx appears as
  "Open with" handler for every supported archive
- GTK open dialog: file filter extended with all new MIME types and
  filename patterns
- AppImage now bundles `bsdtar` next to `7z` for portable exotic-format
  support; `--help` lists the full format matrix

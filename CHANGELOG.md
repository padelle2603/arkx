# Changelog

## v1.1.1 — Dolphin integration: Comprimi/Estrai menus + progress window

Restores the right-click **Compress** entry lost when uninstalling Ark,
with Ark parity and the in-app progress window.

### File-manager integration

- **Comprimi submenu** (Dolphin ServiceMenu): Comprimi in zip... / tar.gz... /
  7zip... with Italian translations, no-overwrite naming (`docs.zip`, `docs-2.zip`…)
- **Estrai submenu**: Estrai qui / Estrai in... / Apri con Arkx
- New CLI: `arkx compress [--here] [--format zip|tar.gz|7z] [--to DEST] [--dialog]`
  and `arkx extract --here/--dialog` (multi-archive aware, `file://` URI decoding)
- **Progress window** (`--progress`): same bar as the app (speed, ETA, Details),
  auto-closes on success with notification + highlight in Dolphin, stays open on
  error, Cancel kills the backend and deletes partials
- Real 7z creation progress (previously silent 0→100%); cancellable creates
  across native and 7z backends
- AppImage-only setup: `./appimage/install-menus.sh` registers the menus
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

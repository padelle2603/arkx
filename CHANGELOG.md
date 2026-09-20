# Changelog

## v1.6.3 — parallel zip creation, per-entry extract progress, MT xz

- **Perf**: zip creation now compresses files in parallel (each worker emits an
  in-memory chunk that is merged verbatim into the final archive, no
  re-compression) when no password is set and RAM allows; deflate is handled
  by zlib-ng for all gzip/zip writers.
- **Perf**: 7z and bsdtar extraction progress is driven by the tool's own
  per-entry completion lines (O(1) per tick) instead of walking the
  destination directory every 250ms; `.tar.xz`/`.xz` creation uses liblzma's
  multithreaded encoder when available.
- **Perf**: `read_lines_until` and the zip read/write `io::copy` paths use a
  large fixed buffer and avoid per-line/stream allocations, slicing large
  I/O into 1 MiB chunks.

## v1.6.2 — cancel-aware extraction, password-aware edits and faster matching

- **Fixed**: a cancel request now genuinely stops extraction — the cancel flag is
  shared with the backend manager and the running extract children (7z, bsdtar)
  are killed instead of only discarding progress events.
- **Fixed**: the size walk and AppleDouble-free staging copy no longer recurse
  forever when a symlink points back at the source.
- **Fixed**: edit operations (add, remove, rename, paste, open-with, new folder)
  now correctly reuse the session password, or prompt for it up front when the
  open archive is encrypted.
- **Fixed**: the shared 16GiB zip-bomb quota is enforced on the 7z and bsdtar
  backends; bsdtar extraction no longer restores archive ownership
  (`--no-same-owner`), matching the native backend.
- **Perf**: the listing total is reused for extraction, so 7z/bsdtar skip their
  pre-extract listing pass; selection matching is normalized once and the hot
  loops are allocation-free; single-file extraction progress is throttled.
- **Refactor**: the 7z thread cap is unified in a single helper and
  `BackendKind::Libarchive` is renamed to `Bsdtar` to match the actual component.

## v1.6.1 — checksums, inline rename and multi-volume creation

- **Feature**: SHA-256 and MD5 checksums (with copy-to-clipboard) in the archive
  properties window; the hash runs off the UI thread and does not block browsing.
- **Feature**: inline entry rename with F2/Alt+F2 (instead of the dialog) and
  explicit confirmation when the new name collides with an existing entry.
- **Feature**: *New Folder* accepts nested paths and does not require a folder
  to be selected first.
- **Feature**: multi-volume creation for `.zip` (Info-ZIP `zip -s`) and `.rar`
  (`rar -v`), alongside the existing native 7z split; the reported size matches
  the split naming of each format.
- **Fixed**: secure delete's zeroing pass is now streamed in chunks instead of
  buffering the whole archive in RAM (OOM on large archives).
- **Perf**: shared extract progress poller, hoisted per-archive path
  normalization in secure extract, reused read/write buffers, and cached
  backend probe (`7z` sidecar / bsdtar availability).
- **Refactor**: duplicated extract/sum/drain-pipe/entry-argument helpers
  consolidated across native, 7z and bsdtar backends; tar.* format detection
  now has a single source of truth.

## v1.6.0 — editing suite: comments, AES-256, convert, multi-volume split

- **Context menu at full height**: the right-click menu is no longer capped at
  340px, so every option is visible without scrolling (it sizes to its
  content; GTK only scrolls it if it would overflow the screen).
- **Archive properties as a full window**: the properties view is now a
  resizable secondary window sized to its content instead of a scrollable
  alert dialog, so every row is visible at once (no scrolling).
- **Visually hidden cut entries**: after a *Cut* the selected entries disappear
  from the view until the pending paste is submitted; a successful paste keeps
  them out (they moved away), a failed paste restores them in place — the
  working copy/paste logic is untouched.
- **Editable archive comment (zip)**: comments are now read (`zip.comment()`,
  `Comment =` in 7z/RAR) and written on `.zip` via a native rewrite shared with
  remove/rename. The *Archive properties* dialog shows an editable field and a
  *Save* button for zip archives; 7z/RAR report "read-only; use zip", formats
  without comment support report a clear error.
- **Native AES-256 zip (create/extract)**: a password on `arkx a` and on
  extraction now runs through the native backend for zip (AES-256, `aes-crypto`
  feature). Encrypted entries decrypt per entry and a wrong password surfaces
  the typed `WrongPassword`; the 7z re-run is skipped once the native backend
  already rejected the credentials (it left partial garbage behind).
- **Sortable columns**: clicking a table header sorts the current view by that
  column (asc/desc toggle), directories always first, numeric order for
  size/date. The properties dialog is also wider and taller.
- **Format conversion**: `arkx convert <src> <dest>` re-packs any readable
  archive into a writer-backed destination (zip/7z/tar.*): extract to a
  temporary dir, then re-create with the existing backends and progressive
  output. Single-file stream destinations are refused up front, as is
  overwriting the source archive itself.
- **Multi-volume split**: `arkx a dest.7z ... -v <size>` splits the output into
  volumes (e.g. `50m`, `1g`) via 7z's `-v` switch — only `.7z` destinations
  accept it (clear refusal elsewhere). The CLI also auto-opens a multi-volume
  archive when pointed at the base name (`out.7z` → `out.7z.001`), and the
  created size reports the sum of all volume files.
- **Hardened archive editing**: "open with" and secure-delete resolve zip
  entries through a secure join (path-traversal paths are skipped and
  reported); 7z/tar entry names starting with `-` are prefixed so they cannot
  be parsed as command-line options; cancelling one operation no longer aborts
  the other pending jobs (per-job cancel flags).
- **Robustness fixes**: archive paths that are not valid UTF-8 are passed to 7z
  verbatim instead of being mangled; cancelling the password prompt resets the
  pending state so the next attempt starts clean; the CLI `test`/`wipe`
  commands honour the `--threads` override; non-panicking GUI guards replace
  `unwrap`s on polled results.
- **No leaked temp dirs**: convert, paste and new-folder staging dirs are now
  cleaned by a RAII guard on every path (success or error) instead of only on
  the happy path.

## v1.5.0 — archive editing: rename, copy/paste, open-with, integrity check

- **Rename entries**: entries can be renamed in place on `.zip` (native rewrite)
  and `.7z`; in the GUI press `F2` or use the context menu → *Rename* (an
  explicit path in the dialog moves the entry); from the CLI use
  `arkx rename <archive> <old_entry> <new_entry>`.
- **Integrity check**: new `arkx test / t <archive> [entries...]` verifies
  entries against the stored CRC32 on zip/tar/7z (native recompute through
  `crc32fast`, `7z t` for 7z) and prints a per-entry pass/fail report in the
  CLI; the worker/gui already carry the published `TestReport`.
- **Copy / cut / paste inside the archive**: the context menu gained *Copy*,
  *Cut*, *Paste* and *Copy path* working on an internal clipboard; paste
  duplicates or moves the selection to the browsed folder (temp + atomic
  update, originals removed on cut). Same-archive only, cross-archive paste
  reports a clear error.
- **Open with external app**: extract an entry to a temp dir and launch the
  default application (`xdg-open`) — context menu → *Open with*, or
  `arkx open / o <archive> <entry>`.
- **Secure delete (CLI)**: `arkx wipe / w <archive> <entry...> [--passes N]`
  overwrites then removes the selected entries (destructive, no GUI by design).
- **AES-encrypted zip foundation**: the `zip` crate now builds with the
  `aes-crypto` feature (and `sha2` is added), so native AES-encrypted ZIP
  support has its dependency groundwork in place; encrypted reads/creates
  still route through `7z` until the native path consumes the password.

### Fixed

- The browser no longer jumps back to the archive root after adding,
  removing, renaming or pasting: the current folder is re-listed in place.
- The context menu no longer crashes/freezes when the file list is rebuilt
  while the popover is open (the popover is now parented to the window, so a
  `remove_all` on the list can never hit a "Tried to remove non-child"
  state).

### Chore

- Removed dead code: unused `JobKind::Copy`/`Cut` variants and the always
  `None` `ArchiveInfo`/`ArchiveProperties.comment` field (plus all backend
  stubs). Same-release refactor of the repeated busy-guard/dismiss pattern
  into a single helper.

## v1.4.0 — password-protected extraction + hardening

- **Password-protected extraction**: encrypted ZIPs and header-encrypted 7z/RAR
  archives now prompt for a password (native, CLI `-p`, and the file-manager
  progress app); a wrong password re-opens the same dialog with the destination
  preserved, and password errors are typed (`ArkxError::WrongPassword`) instead
  of matched on stderr strings.
- **Typed worker events**: the worker pool carries `ArkxError` in `Finished`/
  `Error` events, so the UI stops string-matching backend output; the shared
  7z error classifier spots missing volumes before the misleading
  "Wrong password?" hint and strips the 7z banner from user-facing messages.

### Security

- A hostile tar could write outside the extract directory through symlinks in
  the archive and through a symlinked `dest`; both escapes are now rejected.
- `setuid`/`setgid`/sticky bits from zip/tar metadata are stripped on extract.
- Refuses to decompress past a 16 GiB quota, stopping zip-bomb fill-ups up
  front (declared size) and during the read loop.

### Performance

- Reused 64 KB buffers across entries (no per-file allocation) and single-pass
  zip extraction.

### Detector & CLI

- TAR and ARJ recognized by magic bytes without a matching extension; single
  lzip (`.lz`) handling for composite tar.lz.
- `--threads`/`-l` reject invalid values (no silent downgrade to auto), unknown
  flags exit with an error, and `--progress` is limited to a single archive.

### Fixed

- Multi-format fallback chains stop on password/missing-volume errors instead
  of masking them with an irrelevant libarchive retry.

## v1.3.0 — add/remove entries + hardening & speedups

- **Add to archive**: `arkx a/u <archive> <file...>` appends files to an
  existing `.zip`/`.7z`/`.tar` (`--to <dir>` offsets entry paths, `-p` for
  encrypted archives); in the GUI, drag & drop files onto the open archive to
  add them to the browsed folder (never into the archive itself).
- **Remove from archive**: `arkx r/d <archive> <entry...>` deletes entries
  (folders recursively); in the GUI, right-click a selection → *Remove*.
  Zip updates are rewritten natively, 7z/tar use 7z update mode, and
  stream-compressed tars fail clearly instead of corrupting.

### Fixed

- **Security**: a hostile archive could write outside the extract directory
  via an intermediate symlink (the old check only canonicalized full paths
  whose target already existed); each existing component is now validated.
- **Cancel**: the first job submitted right after a cancel was silently
  dropped as "Cancelled" (the flag was cleared only on the next submit).
- **File-manager open**: double-clicking an archive in Dolphin did not load
  it — the `gio open` path was never handed to the window; a file queued via
  `GApplication::open` is now opened on startup.

### Performance

- Tree building in the archive browser went from O(n²) to O(n) for large
  archives (explicit-dir lookup via hash map).
- `7z`/`bsdtar` binary discovery is cached once per process instead of
  spawning probe subprocesses on every job.

### Chore

- Removed unused dependencies (`mime_guess`, `open`, `serde_json`).

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

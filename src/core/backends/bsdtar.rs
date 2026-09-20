//! libarchive backend (`bsdtar`, fallback GNU `tar`) for tar.* compressions
//! that neither the native streaming backend nor 7z can open:
//! `.tar.lz` (lzip), `.tzo`/`.tar.lzo` (lzop), `.tar.lrz` (lrzip).
//! It also serves as last-resort fallback for the other tar flavors,
//! CPIO, XAR, AR and ISO/AppImage when their primary backend fails.

use crate::core::archive::{
    ArchiveBackend, ArchiveEntry, ArchiveInfo, ProgressInfo, SharedCallback,
};
use crate::core::detector::ArchiveFormat;
use crate::core::error::{ArkxError, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

pub struct BsdtarBackend {
    bin: PathBuf,
    is_bsdtar: bool,
    cancel: Arc<AtomicBool>,
}

impl BsdtarBackend {
    pub fn new() -> Self {
        let (bin, is_bsdtar) = locate_tar();
        Self {
            bin,
            is_bsdtar,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn with_cancel(cancel: Arc<AtomicBool>) -> Self {
        let (bin, is_bsdtar) = locate_tar();
        Self {
            bin,
            is_bsdtar,
            cancel,
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn is_available(&self) -> bool {
        // `bsdtar`/`tar` resolved from PATH (or absolute fallback): probe once
        // per process, reusing the discovery already done by `locate_tar`.
        use std::sync::OnceLock;
        static AVAILABLE: OnceLock<bool> = OnceLock::new();
        *AVAILABLE.get_or_init(|| {
            Command::new(&self.bin)
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        })
    }
}

impl ArchiveBackend for BsdtarBackend {
    fn supports(&self, fmt: &ArchiveFormat) -> bool {
        matches!(
            fmt,
            ArchiveFormat::Tar
                | ArchiveFormat::TarGz
                | ArchiveFormat::TarBz2
                | ArchiveFormat::TarXz
                | ArchiveFormat::TarZst
                | ArchiveFormat::TarLz4
                | ArchiveFormat::TarZ
                | ArchiveFormat::TarLzma
                | ArchiveFormat::TarLzip
                | ArchiveFormat::TarLzo
                | ArchiveFormat::TarLrzip
                | ArchiveFormat::Cpio
                | ArchiveFormat::Xar
                | ArchiveFormat::Ar
                | ArchiveFormat::Iso
                | ArchiveFormat::AppImage
        )
    }

    fn list(&self, path: &Path) -> Result<ArchiveInfo> {
        self.list_inner(path)
    }

    fn extract(
        &self,
        archive: &Path,
        dest: &Path,
        entries: Option<&[String]>,
        _password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        self.extract_inner(archive, dest, entries, None, progress)
    }

    fn extract_with_total(
        &self,
        archive: &Path,
        dest: &Path,
        entries: Option<&[String]>,
        _password: Option<&str>,
        known: Option<(u64, u64)>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        self.extract_inner(archive, dest, entries, known, progress)
    }

    fn create(
        &self,
        _dest: &Path,
        _sources: &[PathBuf],
        _level: u8,
        _password: Option<&str>,
        _volume_size: Option<&str>,
        _progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        Err(ArkxError::UnsupportedFormat(
            "creating this format is not supported (extract-only)".into(),
        ))
    }
}

impl Default for BsdtarBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn locate_tar() -> (PathBuf, bool) {
    use std::sync::OnceLock;
    static CACHE: OnceLock<(PathBuf, bool)> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            // Inside an AppImage bsdtar ships next to us (like 7z): prefer a binary
            // beside the current executable, then PATH.
            if let Some(p) = crate::core::backends::tools::sidecar_bin(&["bsdtar", "tar"]) {
                let is_bsdtar = p.file_name().is_some_and(|n| n == "bsdtar");
                return (p, is_bsdtar);
            }
            for name in ["bsdtar", "tar"] {
                if let Ok(out) = Command::new(name).arg("--version").output() {
                    if out.status.success() {
                        let stdout = String::from_utf8_lossy(&out.stdout).to_lowercase();
                        // GNU tar reports "tar (GNU tar)"; bsdtar reports "bsdtar".
                        let is_bsdtar = !stdout.contains("gnu tar");
                        return (PathBuf::from(name), is_bsdtar);
                    }
                }
            }
            (PathBuf::from("bsdtar"), true)
        })
        .clone()
}

/// Extra decompressor flag for GNU `tar` (reads ignore -z/-j/-J, but exotic
/// codecs need an explicit program). `bsdtar`/libarchive auto-detects.
fn gnu_decompress_flag(fmt: &ArchiveFormat) -> Option<String> {
    match fmt {
        ArchiveFormat::TarLz4 => Some("-I".into()),
        ArchiveFormat::TarLrzip => Some("-I".into()),
        ArchiveFormat::TarLzip => Some("--lzip".into()),
        ArchiveFormat::TarLzo => Some("--lzop".into()),
        ArchiveFormat::TarZ => Some("-Z".into()),
        _ => None,
    }
}

fn gnu_decompress_arg(fmt: &ArchiveFormat) -> Option<String> {
    match fmt {
        ArchiveFormat::TarLz4 => Some("lz4 -d".into()),
        ArchiveFormat::TarLrzip => Some("lrzip -d -c".into()),
        _ => None,
    }
}

impl BsdtarBackend {
    fn base_cmd(&self, fmt: &ArchiveFormat) -> Command {
        let mut cmd = Command::new(&self.bin);
        // Stable English dates for `parse_tvf_line` regardless of locale.
        cmd.env("LC_ALL", "C");
        if !self.is_bsdtar {
            if let Some(flag) = gnu_decompress_flag(fmt) {
                if flag == "-I" {
                    if let Some(prog) = gnu_decompress_arg(fmt) {
                        cmd.arg("-I").arg(prog);
                    }
                } else {
                    cmd.arg(flag);
                }
            }
        }
        cmd
    }

    fn list_inner(&self, path: &Path) -> Result<ArchiveInfo> {
        if !self.is_available() {
            return Err(ArkxError::Backend(
                "neither bsdtar nor tar found: install libarchive-tools to open this format".into(),
            ));
        }
        let fmt = crate::core::detector::detect_format(path);
        let mut cmd = self.base_cmd(&fmt);
        cmd.arg("-tvf").arg(path);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let output = cmd
            .output()
            .map_err(|e| ArkxError::Backend(format!("cannot run tar backend: {}", e)))?;
        if !output.status.success() {
            let msg = String::from_utf8_lossy(&output.stderr);
            return Err(map_tar_error(&msg));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_tvf(&stdout, path)
    }

    fn extract_inner(
        &self,
        archive: &Path,
        dest: &Path,
        entries: Option<&[String]>,
        known: Option<(u64, u64)>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        if !self.is_available() {
            return Err(ArkxError::Backend(
                "neither bsdtar nor tar found: install libarchive-tools to extract this format"
                    .into(),
            ));
        }
        std::fs::create_dir_all(dest).map_err(ArkxError::Io)?;
        if self.cancelled() {
            return Err(ArkxError::Cancelled);
        }
        let fmt = crate::core::detector::detect_format(archive);
        let mut cmd = self.base_cmd(&fmt);
        cmd.arg("-xf").arg(archive).arg("-C").arg(dest);
        // Never restore archive ownership: extracting as root would otherwise
        // let an archive claim any uid/gid (privilege escalation).
        cmd.arg("--no-same-owner");
        // `-v` emits one "x <name>" line per extracted entry: the O(1)-per-tick
        // feed for the per-entry progress bar (no dir_size(dest) walk).
        cmd.arg("-v");
        if let Some(sel) = entries {
            for e in sel {
                cmd.arg(crate::core::paths::entry_arg(e));
            }
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        // Without progress: run to completion, draining pipes (no -v needed).
        if progress.is_none() {
            let output = cmd
                .output()
                .map_err(|e| ArkxError::Backend(format!("spawn tar: {}", e)))?;
            if !output.status.success() {
                return Err(map_tar_error(&String::from_utf8_lossy(&output.stderr)));
            }
            return Ok(());
        }
        let cb = progress.unwrap();

        // Entry-based progress: total = non-directory entry count from the
        // listing (or the caller's known value); the byte total is only used
        // for the zip-bomb quota. Computed BEFORE spawn so a declared zip bomb
        // is refused before the decompressor child starts pumping bytes.
        let (pre_total, pre_entries) = match known {
            Some((t, n)) => (t, n),
            None => match self.list(archive) {
                Ok(info) => (
                    crate::core::util::sum_selected(&info, entries),
                    crate::core::util::count_selected(&info, entries),
                ),
                // Listing unavailable (e.g. headers encrypted): byte total
                // vanishes (pulse path with the quota unknown), entries too.
                Err(_) => (0, 0),
            },
        };
        if pre_total > super::native::MAX_EXTRACTED_BYTES {
            return Err(super::native::quota_error());
        }
        cb(ProgressInfo::preparing(pre_total));

        let mut child = cmd
            .spawn()
            .map_err(|e| ArkxError::Backend(format!("spawn tar: {}", e)))?;
        let cb_arc: SharedCallback = Arc::new(Mutex::new(cb));
        let done = Arc::new(AtomicU64::new(0));
        let entries_total = pre_entries.max(1);

        // Drain stderr on a separate thread to avoid 64KB pipe deadlock.
        // stdout carries one "x <name>" line per extracted entry: count them.
        let err_handle = crate::core::util::spawn_drain_pipe(child.stderr.take());
        let out_cb = cb_arc.clone();
        let out_done = done.clone();
        let out_handle = if let Some(stdout) = child.stdout.take() {
            std::thread::spawn(move || {
                let mut last_emit = Instant::now();
                let mut last_label = String::new();
                crate::core::util::read_lines_until(stdout, |line| {
                    let name = line.trim();
                    if name.is_empty() || last_label == name {
                        return false;
                    }
                    last_label = name.to_string();
                    out_done.fetch_add(1, Ordering::Relaxed);
                    if last_emit.elapsed().as_millis() > 500 {
                        last_emit = Instant::now();
                        let d = out_done.load(Ordering::Relaxed);
                        if let Ok(guard) = out_cb.lock() {
                            guard(ProgressInfo::new(name.to_string(), d, entries_total));
                        }
                    }
                    false
                });
            })
        } else {
            std::thread::spawn(|| {})
        };

        // Wait, polling every 100ms so a cancel kills the child promptly.
        let status = loop {
            if self.cancelled() {
                let _ = child.kill();
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(e) => return Err(ArkxError::Io(e)),
            }
        };
        if self.cancelled() {
            return Err(ArkxError::Cancelled);
        }
        out_handle.join().ok();
        err_handle.join().ok();
        if !status.success() {
            return Err(ArkxError::Backend(format!(
                "extraction failed (code {:?})",
                status.code()
            )));
        }
        if let Ok(g) = cb_arc.lock() {
            g(crate::core::util::completed(entries_total));
        }
        Ok(())
    }
}

fn map_tar_error(stderr: &str) -> ArkxError {
    let m = stderr.trim();
    // libarchive cannot decrypt encrypted entries (rar/zip): surface a
    // password prompt instead of a raw backend error.
    if m.contains("Encryption is not supported")
        || m.contains("Password required")
        || m.contains("Wrong password")
    {
        return ArkxError::WrongPassword;
    }
    // Known libarchive bug (&lt; 3.9.0): on truncated archives or with non
    // UTF-8 names `archive_error_string()` returns NULL and bsdtar prints "(null)".
    // Translate to a human message instead of showing "(null)" to the user.
    if m.contains("(null)") {
        return ArkxError::Corrupted(
            "cannot open archive (damaged file or unsupported names; try `7z l` for details)"
                .into(),
        );
    }
    if m.contains("Cannot allocate memory") {
        return ArkxError::Backend("not enough memory to open archive".into());
    }
    if m.contains("Unrecognized archive format")
        || m.contains("not seem to be a tar archive")
        || m.contains("does not look like a tar archive")
        || m.contains("non sembra un archivio tar")
        || m.contains("Error opening archive")
        || m.contains("Is not archive")
    {
        return ArkxError::Corrupted(m.to_string());
    }
    if m.contains("lz4")
        && (m.contains("not found") || m.contains("Cannot exec") || m.contains("non riuscita"))
    {
        return ArkxError::Backend("lz4 decoder missing: install lz4 or libarchive-tools".into());
    }
    if m.contains("lzip") && (m.contains("not found") || m.contains("Cannot exec")) {
        return ArkxError::Backend("lzip decoder missing: install lzip or libarchive-tools".into());
    }
    if m.contains("lzop") && (m.contains("not found") || m.contains("Cannot exec")) {
        return ArkxError::Backend("lzop decoder missing: install lzop or libarchive-tools".into());
    }
    if m.contains("lrzip") && (m.contains("not found") || m.contains("Cannot exec")) {
        return ArkxError::Backend(
            "lrzip decoder missing: install lrzip or libarchive-tools".into(),
        );
    }
    if m.is_empty() {
        return ArkxError::Corrupted("cannot open archive (empty error)".into());
    }
    ArkxError::Backend(m.to_string())
}

fn parse_tvf(output: &str, archive_path: &Path) -> Result<ArchiveInfo> {
    let mut entries: Vec<ArchiveEntry> = Vec::new();
    let mut total_size = 0u64;
    for line in output.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            continue;
        }
        if let Some(e) = parse_tvf_line(line) {
            total_size = total_size.saturating_add(if e.is_dir { 0 } else { e.size });
            entries.push(e);
        }
    }
    if entries.is_empty() {
        return Err(ArkxError::Corrupted(
            "cannot list archive (empty output: file may be empty or damaged)".into(),
        ));
    }
    let (num_files, num_dirs) = crate::core::archive::count_files_dirs(&entries);
    Ok(ArchiveInfo {
        path: archive_path.to_string_lossy().to_string(),
        format: crate::core::detector::detect_format(archive_path)
            .display_name()
            .to_string(),
        entries,
        total_size,
        total_packed: total_size,
        num_files,
        num_dirs,
        has_encrypted: false,
        comment: None,
    })
}

/// Parse one `tar -tvf` line in either bsdtar (`-rw-r--r-- 1 u g SIZE Mon D T path`)
/// or GNU tar (`-rw-r--r-- u/g SIZE YYYY-MM-DD HH:MM path`) shape.
/// Paths with spaces survive (split from the left, path takes the tail);
/// symlinks keep the link path (`a -> b` → `a`).
fn parse_tvf_line(line: &str) -> Option<ArchiveEntry> {
    let mut it = line.split_whitespace();
    let perms = it.next()?;
    let fc = perms.chars().next()?;
    if !"bcdlpsw-".contains(fc) {
        return None;
    }
    let is_dir = fc == 'd';
    let rest: Vec<&str> = it.collect();
    // bsdtar: [nlink, user, group, size, mon, day, time/year, path...]
    // GNU tar: [user/group, size, date, time, path...]
    let (size, path_tokens): (u64, &[&str]) = if rest.len() >= 8
        && rest[0].chars().all(|c| c.is_ascii_digit())
        && rest[4]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
    {
        (rest[3].parse().unwrap_or(0), &rest[7..])
    } else if rest.len() >= 5 && rest[1].chars().all(|c| c.is_ascii_digit()) {
        (rest[1].parse().unwrap_or(0), &rest[4..])
    } else {
        return None;
    };
    if path_tokens.is_empty() {
        return None;
    }
    let raw_path = path_tokens.join(" ");
    // Strip symlink target.
    let link_path = raw_path
        .split(" -> ")
        .next()
        .unwrap_or(&raw_path)
        .to_string();
    let norm = crate::core::paths::normalize(&link_path);
    if norm.is_empty() {
        return None;
    }
    let (path, is_dir) = if is_dir || norm.ends_with('/') {
        (crate::core::paths::with_trailing_slash(&norm), true)
    } else {
        (norm, false)
    };
    Some(ArchiveEntry {
        path,
        is_dir,
        size: if is_dir { 0 } else { size },
        packed_size: if is_dir { 0 } else { size },
        modified: parse_tvf_date(rest.as_slice()),
        mode: parse_mode(perms),
        crc32: None,
        method: None,
        encrypted: false,
    })
}

fn parse_tvf_date(rest: &[&str]) -> Option<chrono::DateTime<chrono::Local>> {
    // bsdtar: [nlink, user, group, size, Mon, Day, Time|Year]
    if rest.len() >= 7
        && rest[4]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
    {
        let mon = rest[4];
        let day = rest[5];
        let third = rest[6];
        if third.contains(':') {
            let year = chrono::Local::now().format("%Y").to_string();
            let s = format!("{} {} {} {}", mon, day, year, third);
            if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(&s, "%b %d %Y %H:%M") {
                return ndt.and_local_timezone(chrono::Local).single();
            }
        } else {
            let s = format!("{} {} {}", mon, day, third);
            if let Ok(nd) = chrono::NaiveDate::parse_from_str(&s, "%b %d %Y") {
                let ndt = nd.and_hms_opt(0, 0, 0)?;
                return ndt.and_local_timezone(chrono::Local).single();
            }
        }
        return None;
    }
    // GNU tar: [user/group, size, YYYY-MM-DD, HH:MM]
    if rest.len() >= 4 {
        let s = format!("{} {}", rest[2], rest[3]);
        for fmt in ["%Y-%m-%d %H:%M", "%Y-%m-%d %H:%M:%S"] {
            if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(&s, fmt) {
                return ndt.and_local_timezone(chrono::Local).single();
            }
        }
    }
    None
}

/// `-rwxr-xr-x` → `0o755` (best effort, `None` when unreadable).
fn parse_mode(perms: &str) -> Option<u32> {
    let b: Vec<char> = perms.chars().collect();
    if b.len() < 10 {
        return None;
    }
    let bit = |c: char, v: u32| match c {
        'r' => v,
        'w' => v,
        'x' | 's' | 'S' | 't' | 'T' => v,
        _ => 0,
    };
    let mode = bit(b[1], 0o400)
        + bit(b[2], 0o200)
        + bit(b[3], 0o100)
        + bit(b[4], 0o40)
        + bit(b[5], 0o20)
        + bit(b[6], 0o10)
        + bit(b[7], 0o4)
        + bit(b[8], 0o2)
        + bit(b[9], 0o1);
    Some(mode)
}

#[cfg(test)]
mod tests {
    use super::{parse_mode, parse_tvf_line};

    #[test]
    fn test_parse_bsdtar_line() {
        let e =
            parse_tvf_line("-rw-r--r--  0 padelle padelle    11 Sep  3 16:34 hello.txt").unwrap();
        assert_eq!(e.path, "hello.txt");
        assert!(!e.is_dir);
        assert_eq!(e.size, 11);
        assert_eq!(e.mode, Some(0o644));
        let d = parse_tvf_line("drwxr-xr-x  0 padelle padelle     0 Sep  3 16:34 sub/").unwrap();
        assert!(d.is_dir);
        assert_eq!(d.path, "sub/");
    }

    #[test]
    fn test_parse_gnu_line() {
        let e =
            parse_tvf_line("-rw-r--r-- padelle/padelle  11 2025-09-03 16:34 hello.txt").unwrap();
        assert_eq!(e.path, "hello.txt");
        assert_eq!(e.size, 11);
        let spaced =
            parse_tvf_line("-rw-r--r-- padelle/padelle  11 2025-09-03 16:34 my file.txt").unwrap();
        assert_eq!(spaced.path, "my file.txt");
    }

    #[test]
    fn test_parse_symlink_and_mode() {
        let e = parse_tvf_line("lrwxrwxrwx  0 root    root       7 Sep  3 16:34 link -> target")
            .unwrap();
        assert_eq!(e.path, "link");
        assert_eq!(parse_mode("drwxr-xr-x"), Some(0o755));
        assert!(parse_tvf_line("total 123").is_none());
    }
}

#[cfg(test)]
mod error_tests {
    use super::map_tar_error;
    use crate::core::error::ArkxError;

    #[test]
    fn null_error_becomes_human_message() {
        // Bug libarchive < 3.9: error string NULL → "bsdtar: (null)".
        let err = map_tar_error("bsdtar: (null)\nbsdtar: Error exit delayed from previous errors.");
        match err {
            ArkxError::Corrupted(msg) => {
                assert!(!msg.contains("(null)"), "cryptic message: {}", msg);
            }
            other => panic!("expected Corrupted, got: {}", other),
        }
    }

    #[test]
    fn encrypted_archive_is_wrong_password() {
        let err = map_tar_error(
            "bsdtar: Encryption is not supported\nbsdtar: Error exit delayed from previous errors",
        );
        assert!(matches!(err, ArkxError::WrongPassword), "got: {}", err);
    }

    #[test]
    fn gnu_english_not_a_tar_is_corrupted() {
        let err = map_tar_error(
            "tar: This does not look like a tar archive\ntar: Exiting with failure status",
        );
        assert!(matches!(err, ArkxError::Corrupted(_)), "got: {}", err);
    }
}

use crate::core::archive::{
    ArchiveBackend, ArchiveEntry, ArchiveInfo, ProgressInfo, SharedCallback,
};
use crate::core::detector::ArchiveFormat;
use crate::core::error::{ArkxError, Result};
use crate::core::util::{merge_progress_target, Smoother};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::Instant;

/// Zip-bomb guard: refuse to decompress more than this many bytes for a
/// single archive in the native backend (honest quota, not a ratio heuristic).
/// Extraction aborts with a clear error instead of filling the disk.
const IO_BUF_SIZE: usize = 1024 * 1024;
const COPY_CHUNK: usize = 65536;
pub(super) const MAX_EXTRACTED_BYTES: u64 = 16 * 1024 * 1024 * 1024; // 16 GiB

pub(super) fn quota_error() -> ArkxError {
    ArkxError::Corrupted(format!(
        "decompressed data exceeds the {:.0} GiB safety quota; refusing to continue (possible zip bomb)",
        MAX_EXTRACTED_BYTES as f64 / (1024.0 * 1024.0 * 1024.0)
    ))
}

pub struct NativeBackend {
    cancel: Arc<AtomicBool>,
}

impl NativeBackend {
    pub fn with_cancel(cancel: Arc<AtomicBool>) -> Self {
        Self { cancel }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Drop the partial destination after a cancel (best effort).
    fn discard_partial(dest: &Path) {
        let _ = std::fs::remove_file(dest);
    }
}

impl ArchiveBackend for NativeBackend {
    fn supports(&self, fmt: &ArchiveFormat) -> bool {
        Self::supports_format(fmt)
    }

    fn list(&self, path: &Path) -> Result<ArchiveInfo> {
        self.list_inner(path)
    }

    fn extract(
        &self,
        archive: &Path,
        dest: &Path,
        entries: Option<&[String]>,
        password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        self.extract_inner(archive, dest, entries, password, None, progress)
    }

    fn extract_with_total(
        &self,
        archive: &Path,
        dest: &Path,
        entries: Option<&[String]>,
        password: Option<&str>,
        known: Option<(u64, u64)>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        self.extract_inner(
            archive,
            dest,
            entries,
            password,
            known.map(|k| k.0),
            progress,
        )
    }

    fn create(
        &self,
        dest: &Path,
        sources: &[PathBuf],
        level: u8,
        password: Option<&str>,
        volume_size: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        if let Some(v) = volume_size {
            return Err(ArkxError::UnsupportedFormat(format!(
                "split volumes are not supported by the native backend (volume: {v})"
            )));
        }
        self.create_inner(dest, sources, level, password, progress)
    }

    fn add(
        &self,
        archive: &Path,
        sources: &[(PathBuf, String)],
        _password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let fmt = crate::core::detector::detect_format(archive);
        match fmt {
            ArchiveFormat::Zip => self.add_zip(archive, sources, progress),
            _ => Err(ArkxError::UnsupportedFormat(format!("add {:?}", fmt))),
        }
    }

    fn remove(
        &self,
        archive: &Path,
        entries: &[String],
        _password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let fmt = crate::core::detector::detect_format(archive);
        match fmt {
            ArchiveFormat::Zip => self.remove_zip(archive, entries, progress),
            _ => Err(ArkxError::UnsupportedFormat(format!("remove {:?}", fmt))),
        }
    }

    fn rename(
        &self,
        archive: &Path,
        old_name: &str,
        new_name: &str,
        _password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let fmt = crate::core::detector::detect_format(archive);
        match fmt {
            ArchiveFormat::Zip => self.rename_zip(archive, old_name, new_name, progress),
            _ => Err(ArkxError::UnsupportedFormat(format!("rename {:?}", fmt))),
        }
    }

    fn test(
        &self,
        archive: &Path,
        entries: Option<&[String]>,
        _password: Option<&str>,
    ) -> Result<crate::core::archive::TestReport> {
        let fmt = crate::core::detector::detect_format(archive);
        match fmt {
            ArchiveFormat::Zip => self.test_zip(archive, entries),
            _ => Err(ArkxError::UnsupportedFormat(format!("test {:?}", fmt))),
        }
    }

    fn open_with(
        &self,
        archive: &Path,
        entry: &str,
        _password: Option<&str>,
        temp_dir: &Path,
    ) -> Result<PathBuf> {
        let fmt = crate::core::detector::detect_format(archive);
        match fmt {
            ArchiveFormat::Zip => self.open_with_zip(archive, entry, temp_dir),
            _ => Err(ArkxError::UnsupportedFormat(format!("open_with {:?}", fmt))),
        }
    }

    fn secure_delete(
        &self,
        archive: &Path,
        entries: &[String],
        passes: usize,
        _password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let fmt = crate::core::detector::detect_format(archive);
        match fmt {
            ArchiveFormat::Zip => self.secure_delete_zip(archive, entries, passes, progress),
            _ => Err(ArkxError::UnsupportedFormat(format!(
                "secure_delete {:?}",
                fmt
            ))),
        }
    }
}

impl NativeBackend {
    fn supports_format(fmt: &ArchiveFormat) -> bool {
        matches!(
            fmt,
            ArchiveFormat::Zip
                | ArchiveFormat::Tar
                | ArchiveFormat::TarGz
                | ArchiveFormat::TarBz2
                | ArchiveFormat::TarXz
                | ArchiveFormat::TarZst
                | ArchiveFormat::TarLz4
                | ArchiveFormat::Gz
                | ArchiveFormat::Bz2
                | ArchiveFormat::Xz
                | ArchiveFormat::Zst
                | ArchiveFormat::Lz4
        )
    }

    fn list_inner(&self, path: &Path) -> Result<ArchiveInfo> {
        let fmt = crate::core::detector::detect_format(path);
        match fmt {
            ArchiveFormat::Zip => self.list_zip(path),
            ArchiveFormat::Tar
            | ArchiveFormat::TarGz
            | ArchiveFormat::TarBz2
            | ArchiveFormat::TarXz
            | ArchiveFormat::TarZst
            | ArchiveFormat::TarLz4 => self.list_tar(path),
            // Synthetic listing only: Lzma/Compress decoding happens via 7z
            // (extract_inner rejects them and the fallback kicks in).
            ArchiveFormat::Gz
            | ArchiveFormat::Bz2
            | ArchiveFormat::Xz
            | ArchiveFormat::Zst
            | ArchiveFormat::Lz4
            | ArchiveFormat::Lzma
            | ArchiveFormat::Compress => self.list_single(path, &fmt),
            _ => Err(ArkxError::UnsupportedFormat(format!("{:?}", fmt))),
        }
    }

    fn list_zip(&self, path: &Path) -> Result<ArchiveInfo> {
        let file = File::open(path).map_err(ArkxError::Io)?;
        let reader = BufReader::with_capacity(IO_BUF_SIZE, file);
        let mut zip =
            zip::ZipArchive::new(reader).map_err(|e| ArkxError::Corrupted(e.to_string()))?;
        let mut entries = Vec::with_capacity(zip.len());
        let mut total_size = 0u64;
        let mut total_packed = 0u64;
        let mut has_encrypted = false;

        for i in 0..zip.len() {
            let f = zip
                .by_index(i)
                .map_err(|e| ArkxError::Corrupted(e.to_string()))?;
            let is_dir = f.is_dir();
            let size = f.size();
            let comp_size = f.compressed_size();
            let name = f.name().to_string();
            let encrypted = f.encrypted();
            if encrypted {
                has_encrypted = true;
            }
            total_size += size;
            total_packed += comp_size;
            entries.push(ArchiveEntry {
                path: name,
                is_dir,
                size,
                packed_size: comp_size,
                modified: None,
                mode: Some(f.unix_mode().unwrap_or(0o644)),
                crc32: Some(format!("{:08X}", f.crc32())),
                method: Some(format!("{:?}", f.compression())),
                encrypted,
            });
        }

        let (num_files, num_dirs) = crate::core::archive::count_files_dirs(&entries);

        Ok(ArchiveInfo {
            path: path.to_string_lossy().to_string(),
            format: "ZIP".into(),
            entries,
            total_size,
            total_packed,
            num_files,
            num_dirs,
            has_encrypted,
            comment: {
                let c = String::from_utf8_lossy(zip.comment()).trim().to_string();
                if c.is_empty() {
                    None
                } else {
                    Some(c)
                }
            },
        })
    }

    fn list_tar(&self, path: &Path) -> Result<ArchiveInfo> {
        // Streaming tar listing with parallel decompression if needed
        let file = File::open(path).map_err(ArkxError::Io)?;
        let reader: Box<dyn std::io::Read> = create_tar_reader(file, path)?;

        let mut ar = tar::Archive::new(reader);
        let mut entries = Vec::new();
        let mut total_size = 0u64;

        for entry in ar
            .entries()
            .map_err(|e| ArkxError::Corrupted(e.to_string()))?
        {
            let entry = entry.map_err(|e| ArkxError::Corrupted(e.to_string()))?;
            let header = entry.header();
            let path_str = entry
                .path()
                .map_err(|e| ArkxError::Corrupted(e.to_string()))?
                .to_string_lossy()
                .to_string();
            let size = header.size().unwrap_or(0);
            let is_dir = header.entry_type().is_dir();
            total_size += size;
            entries.push(ArchiveEntry {
                path: path_str,
                is_dir,
                size,
                packed_size: size,
                modified: header
                    .mtime()
                    .ok()
                    .and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0))
                    .map(|dt| dt.with_timezone(&chrono::Local)),
                mode: header.mode().ok(),
                crc32: None,
                method: None,
                encrypted: false,
            });
        }

        let (num_files, num_dirs) = crate::core::archive::count_files_dirs(&entries);
        let fmt_str = crate::core::detector::detect_format(path)
            .display_name()
            .to_string();

        Ok(ArchiveInfo {
            path: path.to_string_lossy().to_string(),
            format: fmt_str,
            entries,
            total_size,
            total_packed: total_size,
            num_files,
            num_dirs,
            has_encrypted: false,
            comment: None,
        })
    }

    fn list_single(&self, path: &Path, fmt: &ArchiveFormat) -> Result<ArchiveInfo> {
        let meta = std::fs::metadata(path).map_err(ArkxError::Io)?;
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("file")
            .to_string();
        let entry = ArchiveEntry {
            path: name.clone(),
            is_dir: false,
            size: 0,
            packed_size: meta.len(),
            modified: meta.modified().ok().map(|t| {
                let dt: chrono::DateTime<chrono::Local> = t.into();
                dt
            }),
            mode: None,
            crc32: None,
            method: Some(fmt.display_name().to_string()),
            encrypted: false,
        };
        Ok(ArchiveInfo {
            path: path.to_string_lossy().to_string(),
            format: fmt.display_name().to_string(),
            entries: vec![entry],
            total_size: 0,
            total_packed: meta.len(),
            num_files: 1,
            num_dirs: 0,
            has_encrypted: false,
            comment: None,
        })
    }

    /// Map a zip open/header error; a missing/incorrect password becomes the
    /// typed `WrongPassword` the UI turns into an unlock prompt.
    fn zip_err(e: zip::result::ZipError) -> ArkxError {
        match e {
            zip::result::ZipError::InvalidPassword
            | zip::result::ZipError::UnsupportedArchive(zip::result::ZipError::PASSWORD_REQUIRED) => {
                ArkxError::WrongPassword
            }
            other => ArkxError::Corrupted(other.to_string()),
        }
    }

    /// Map a read error (the crate re-wraps `ZipError` in `io::Error`): a
    /// wrong AES/traditional password only fails mid-stream, so recover it
    /// from the chain instead of reporting a generic decode failure.
    fn zip_read_err(e: std::io::Error) -> ArkxError {
        let wrong_pw = e
            .get_ref()
            .and_then(|s| s.downcast_ref::<zip::result::ZipError>())
            .is_some_and(|z| matches!(z, zip::result::ZipError::InvalidPassword));
        if wrong_pw {
            ArkxError::WrongPassword
        } else {
            ArkxError::Io(e)
        }
    }

    fn extract_inner(
        &self,
        archive: &Path,
        dest: &Path,
        entries: Option<&[String]>,
        password: Option<&str>,
        total: Option<u64>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let fmt = crate::core::detector::detect_format(archive);
        std::fs::create_dir_all(dest).map_err(ArkxError::Io)?;

        match fmt {
            ArchiveFormat::Zip => {
                self.extract_zip(archive, dest, entries, password, total, progress)
            }
            ArchiveFormat::Tar
            | ArchiveFormat::TarGz
            | ArchiveFormat::TarBz2
            | ArchiveFormat::TarXz
            | ArchiveFormat::TarZst
            | ArchiveFormat::TarLz4 => self.extract_tar(archive, dest, entries, total, progress),
            ArchiveFormat::Gz
            | ArchiveFormat::Bz2
            | ArchiveFormat::Xz
            | ArchiveFormat::Zst
            | ArchiveFormat::Lz4 => self.extract_single(archive, dest, total, progress),
            _ => Err(ArkxError::UnsupportedFormat(format!("{:?}", fmt))),
        }
    }

    fn extract_zip(
        &self,
        archive: &Path,
        dest: &Path,
        filter: Option<&[String]>,
        password: Option<&str>,
        known_total: Option<u64>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let file = File::open(archive).map_err(ArkxError::Io)?;
        let reader = BufReader::with_capacity(IO_BUF_SIZE, file);
        let mut zip =
            zip::ZipArchive::new(reader).map_err(|e| ArkxError::Corrupted(e.to_string()))?;

        let filter = filter.map(|f| f.to_vec());
        // Pre-normalize the selection once: the matching loops below only
        // compare (no per-comparison allocations).
        let sels = filter
            .as_ref()
            .map(|f| crate::core::paths::normalize_sel(f));

        // Real total bytes (may be 0 for empty archives: no fake max(1)).
        // The central-directory pass is cheap (never decompresses) and doubles
        // as a first-pass entropy check, so it runs unless the caller already
        // provided the byte total from the listing.
        let total_bytes: u64 = match known_total {
            Some(t) => t,
            None => match &sels {
                Some(sels) => {
                    let mut sum = 0u64;
                    for i in 0..zip.len() {
                        if let Ok(f) = zip.by_index(i) {
                            let norm = crate::core::paths::normalize(f.name());
                            if sels
                                .iter()
                                .any(|s| crate::core::paths::entry_matches_norm(&norm, s))
                            {
                                sum = sum.saturating_add(f.size());
                            }
                        }
                    }
                    sum
                }
                None => {
                    let mut sum = 0u64;
                    for i in 0..zip.len() {
                        if let Ok(f) = zip.by_index(i) {
                            sum = sum.saturating_add(f.size());
                        }
                    }
                    sum
                }
            },
        };

        if let Some(cb) = &progress {
            cb(ProgressInfo::preparing(total_bytes));
        }

        let mut processed_bytes = 0u64;
        // Reused across entries to avoid one 64KB allocation per file.
        let mut buf = vec![0u8; COPY_CHUNK];
        let base = resolved_base(dest);

        for i in 0..zip.len() {
            if self.cancelled() {
                return Err(ArkxError::Cancelled);
            }
            // Encrypted entries are decrypted with the given password; a
            // missing password surfaces as WrongPassword so the caller can
            // prompt, never as an opaque decode failure.
            let mut f = match password {
                Some(pw) => zip
                    .by_index_decrypt(i, pw.as_bytes())
                    .map_err(Self::zip_err)?,
                None => zip.by_index(i).map_err(Self::zip_err)?,
            };
            let name = f.name().to_string();
            let norm = crate::core::paths::normalize(&name);
            if let Some(ref sel) = sels {
                if !sel
                    .iter()
                    .any(|s| crate::core::paths::entry_matches_norm(&norm, s))
                {
                    continue;
                }
            }
            // Declared size over the quota alone is enough to refuse up front:
            // no point streaming gigabytes into a sink just to abort later.
            let declared = f.size();
            if processed_bytes.saturating_add(declared) > MAX_EXTRACTED_BYTES {
                return Err(quota_error());
            }
            let out_path = match secure_join(dest, &base, &name) {
                Some(p) => p,
                None => {
                    eprintln!("[native] skipped unsafe entry: {}", name);
                    continue;
                }
            };
            if f.is_dir() {
                std::fs::create_dir_all(&out_path).map_err(ArkxError::Io)?;
                if let Some(cb) = &progress {
                    cb(ProgressInfo::new(
                        name.clone(),
                        processed_bytes.min(total_bytes),
                        total_bytes,
                    ));
                }
            } else {
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent).map_err(ArkxError::Io)?;
                }
                let mut out = BufWriter::with_capacity(
                    IO_BUF_SIZE,
                    File::create(&out_path).map_err(ArkxError::Io)?,
                );
                // 64KB chunks throttled to 100ms / 512KB so the UI is not spammed
                let mut last_emit = Instant::now();
                let mut last_bytes = processed_bytes;
                loop {
                    let n = match f.read(&mut buf) {
                        Ok(n) => n,
                        Err(e) => {
                            let err = Self::zip_read_err(e);
                            // A wrong password only fails mid-stream: don't
                            // leave half-decrypted garbage on disk.
                            if matches!(err, ArkxError::WrongPassword) {
                                let _ = std::fs::remove_file(&out_path);
                            }
                            return Err(err);
                        }
                    };
                    if n == 0 {
                        break;
                    }
                    out.write_all(&buf[..n]).map_err(ArkxError::Io)?;
                    processed_bytes = processed_bytes.saturating_add(n as u64);
                    // Runtime guard: declared sizes in the central directory can
                    // lie, the read loop catches what the header check missed.
                    if processed_bytes > MAX_EXTRACTED_BYTES {
                        return Err(quota_error());
                    }
                    // Throttle: emit every 100ms, every 512KB, or on file completion
                    let elapsed = last_emit.elapsed().as_millis() > 100
                        || processed_bytes - last_bytes >= 524288;
                    if elapsed {
                        if let Some(cb) = &progress {
                            cb(ProgressInfo::new(
                                name.clone(),
                                processed_bytes.min(total_bytes),
                                total_bytes,
                            ));
                        }
                        last_emit = Instant::now();
                        last_bytes = processed_bytes;
                    }
                }
                out.flush().map_err(ArkxError::Io)?;
                // preserve permissions
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Some(mode) = f.unix_mode() {
                        let _ = std::fs::set_permissions(
                            &out_path,
                            std::fs::Permissions::from_mode(mode & 0o777),
                        );
                    }
                }
                // Final update per file (even if throttled away)
                if let Some(cb) = &progress {
                    cb(ProgressInfo::new(
                        name.clone(),
                        processed_bytes.min(total_bytes),
                        total_bytes,
                    ));
                }
            }
        }
        if let Some(cb) = &progress {
            cb(crate::core::util::completed(total_bytes));
        }
        Ok(())
    }

    fn extract_tar(
        &self,
        archive: &Path,
        dest: &Path,
        filter: Option<&[String]>,
        known_total: Option<u64>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let file = File::open(archive).map_err(ArkxError::Io)?;
        let reader = create_tar_reader(file, archive)?;
        let mut ar = tar::Archive::new(reader);
        ar.set_preserve_permissions(true);
        // Strip setuid/setgid/sticky bits from archive metadata: preserving
        // them verbatim is a privilege-escalation vector when extracting as root.
        ar.set_mask(0o7000);
        ar.set_preserve_mtime(true);

        let filter = filter.map(|f| f.to_vec());
        // Pre-normalize the selection once: per-entry matching below only
        // compares (no per-comparison allocations).
        let sels = filter
            .as_ref()
            .map(|f| crate::core::paths::normalize_sel(f));
        let base = resolved_base(dest);

        // Real total bytes (may be 0: no fake max(1)). When the caller already
        // knows the total (GUI passes the listing-derived sum) skip the second
        // decompression pass that `list_tar` would cost.
        let mut total_bytes = 0u64;
        if progress.is_some() {
            if let Some(t) = known_total {
                total_bytes = t;
            } else if let Ok(info) = self.list_tar(archive) {
                match &sels {
                    Some(sel) => {
                        for e in &info.entries {
                            if sel
                                .iter()
                                .any(|f| crate::core::paths::entry_matches_norm(&e.path, f))
                            {
                                total_bytes = total_bytes.saturating_add(e.size);
                            }
                        }
                    }
                    None => total_bytes = info.total_size,
                }
            }
            // Immediate 0%.
            if let Some(cb) = &progress {
                cb(ProgressInfo::preparing(total_bytes));
            }
        }

        let mut processed_bytes = 0u64;
        // Reused across entries to avoid one 64KB allocation per file.
        let mut buf = vec![0u8; COPY_CHUNK];
        for entry in ar
            .entries()
            .map_err(|e| ArkxError::Corrupted(e.to_string()))?
        {
            if self.cancelled() {
                return Err(ArkxError::Cancelled);
            }
            let mut entry = entry.map_err(|e| ArkxError::Corrupted(e.to_string()))?;
            let path_raw = entry
                .path()
                .map_err(|e| ArkxError::Corrupted(e.to_string()))?
                .to_string_lossy()
                .to_string();
            let path_norm = crate::core::paths::normalize(&path_raw);
            // Skip non-matching entries when filtering
            if let Some(ref sel) = sels {
                if !sel
                    .iter()
                    .any(|f| crate::core::paths::entry_matches_norm(&path_norm, f))
                {
                    continue;
                }
            }
            // Regular files: chunked copy with throttled progress (not per-file)
            let is_file = entry.header().entry_type().is_file();
            if is_file {
                let out_path = match secure_join(dest, &base, &path_norm) {
                    Some(p) => p,
                    None => {
                        eprintln!("[native] skipped unsafe entry: {}", path_raw);
                        continue;
                    }
                };
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent).map_err(ArkxError::Io)?;
                }
                let mut out = BufWriter::with_capacity(
                    IO_BUF_SIZE,
                    File::create(&out_path).map_err(ArkxError::Io)?,
                );
                let mut last_emit = Instant::now();
                let mut last_bytes = processed_bytes;
                loop {
                    let n = entry.read(&mut buf).map_err(ArkxError::Io)?;
                    if n == 0 {
                        break;
                    }
                    out.write_all(&buf[..n]).map_err(ArkxError::Io)?;
                    processed_bytes = processed_bytes.saturating_add(n as u64);
                    let elapsed = last_emit.elapsed().as_millis() > 100
                        || processed_bytes - last_bytes >= 524288;
                    if elapsed {
                        if let Some(cb) = &progress {
                            cb(ProgressInfo::new(
                                path_raw.clone(),
                                processed_bytes.min(total_bytes),
                                total_bytes,
                            ));
                        }
                        last_emit = Instant::now();
                        last_bytes = processed_bytes;
                    }
                }
                out.flush().map_err(ArkxError::Io)?;
                // preserve permissions/mtime
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(mode) = entry.header().mode() {
                        let _ = std::fs::set_permissions(
                            &out_path,
                            std::fs::Permissions::from_mode(mode & 0o777),
                        );
                    }
                }
                // Final update per file
                if let Some(cb) = &progress {
                    cb(ProgressInfo::new(
                        path_raw.clone(),
                        processed_bytes.min(total_bytes),
                        total_bytes,
                    ));
                }
            } else {
                // Directories, symlinks, others. The tar crate validates the
                // destination path, but creates symlinks with the declared
                // target verbatim: reject links that resolve outside `dest`.
                let out_path = match secure_join(dest, &base, &path_norm) {
                    Some(p) => p,
                    None => {
                        eprintln!("[native] skipped unsafe entry: {}", path_raw);
                        continue;
                    }
                };
                if let Some(target) = entry
                    .link_name()
                    .map_err(|e| ArkxError::Corrupted(e.to_string()))?
                {
                    let base = std::fs::canonicalize(dest).unwrap_or_else(|_| absolutize(dest));
                    let link_abs = absolutize(&out_path);
                    let parent = link_abs.parent().unwrap_or(&base);
                    if !resolve_lexically(parent, &target).starts_with(&base) {
                        eprintln!("[native] skipped link escaping destination: {}", path_raw);
                        continue;
                    }
                }
                entry
                    .unpack_in(dest)
                    .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
                if let Some(cb) = &progress {
                    cb(ProgressInfo::new(
                        path_raw.clone(),
                        processed_bytes.min(total_bytes),
                        total_bytes,
                    ));
                }
            }
        }
        // Final 100% (even when total is 0).
        if let Some(cb) = &progress {
            cb(crate::core::util::completed(total_bytes));
        }
        Ok(())
    }

    fn extract_single(
        &self,
        archive: &Path,
        dest: &Path,
        known_total: Option<u64>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let file = File::open(archive).map_err(ArkxError::Io)?;
        let meta = std::fs::metadata(archive).ok();
        let total = known_total.unwrap_or_else(|| meta.map(|m| m.len()).unwrap_or(0));
        let reader: Box<dyn std::io::Read> = create_single_reader(file, archive)?;
        let out_name = archive
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output");
        let out_path = dest.join(out_name);
        let mut out =
            BufWriter::with_capacity(IO_BUF_SIZE, File::create(&out_path).map_err(ArkxError::Io)?);
        if let Some(cb) = progress {
            cb(ProgressInfo::new(out_name.to_string(), 0, total));
            let mut reader = BufReader::with_capacity(IO_BUF_SIZE, reader);
            let mut buf = vec![0u8; COPY_CHUNK];
            let mut extracted: u64 = 0;
            // Throttle 8KB-read emissions to 100ms / 512KB so the UI is not
            // spammed with a progress event per buffer.
            let mut last_emit = Instant::now();
            let mut last_bytes = 0u64;
            loop {
                if self.cancelled() {
                    return Err(ArkxError::Cancelled);
                }
                let n = reader.read(&mut buf).map_err(ArkxError::Io)?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n]).map_err(ArkxError::Io)?;
                extracted = extracted.saturating_add(n as u64);
                if total > 0 {
                    let elapsed =
                        last_emit.elapsed().as_millis() > 100 || extracted - last_bytes >= 524288;
                    if elapsed {
                        cb(ProgressInfo::new(
                            out_name.to_string(),
                            extracted.min(total),
                            total,
                        ));
                        last_emit = Instant::now();
                        last_bytes = extracted;
                    }
                }
            }
            // Final update even if throttled away.
            if total > 0 {
                cb(ProgressInfo::new(
                    out_name.to_string(),
                    extracted.min(total),
                    total,
                ));
            }
            cb(crate::core::util::completed(total));
            out.flush().map_err(ArkxError::Io)?;
        } else {
            let mut reader = BufReader::with_capacity(IO_BUF_SIZE, reader);
            std::io::copy(&mut reader, &mut out).map_err(ArkxError::Io)?;
            out.flush().map_err(ArkxError::Io)?;
        }
        Ok(())
    }

    fn create_inner(
        &self,
        dest: &Path,
        sources: &[PathBuf],
        level: u8,
        password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let fmt = crate::core::detector::detect_format(dest);
        match fmt {
            ArchiveFormat::Zip => self.create_zip(dest, sources, level, password, progress),
            ArchiveFormat::Tar
            | ArchiveFormat::TarGz
            | ArchiveFormat::TarBz2
            | ArchiveFormat::TarXz
            | ArchiveFormat::TarZst
            | ArchiveFormat::TarLz4 => self.create_tar(dest, sources, &fmt, level, progress),
            ArchiveFormat::Gz
            | ArchiveFormat::Bz2
            | ArchiveFormat::Xz
            | ArchiveFormat::Zst
            | ArchiveFormat::Lz4 => self.create_single(dest, sources, &fmt, level, progress),
            _ => Err(ArkxError::UnsupportedFormat(format!("create {:?}", fmt))),
        }
    }

    fn create_zip(
        &self,
        dest: &Path,
        sources: &[PathBuf],
        level: u8,
        password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        // Collect BEFORE creating dest: if dest falls inside the sources
        // (e.g. --to <subfolder>) it must not include itself mid-write.
        let dest_abs = absolutize(dest);
        let mut files: Vec<PathBuf> = Vec::new();
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut skipped = 0u32;
        for src in sources {
            // is_file()/is_dir() follow symlinks (unlike
            // DirEntry::file_type()): a link to a file is archived with
            // the target's content instead of silently disappearing.
            if src.is_dir() {
                for entry in walkdir::WalkDir::new(src).min_depth(0).into_iter() {
                    let entry = match entry {
                        Ok(e) => e,
                        Err(_) => {
                            skipped += 1;
                            continue;
                        }
                    };
                    let p = entry.path().to_path_buf();
                    if is_same_path(&p, &dest_abs) {
                        continue;
                    }
                    if p.is_file() {
                        files.push(p);
                    } else if p.is_dir() {
                        dirs.push(p);
                    } else {
                        skipped += 1; // broken symlink, socket, fifo, ...
                    }
                }
            } else if src.is_file() {
                if is_same_path(src, &dest_abs) {
                    continue;
                }
                files.push(src.clone());
            } else {
                skipped += 1;
            }
        }
        if files.is_empty() && dirs.is_empty() {
            return Err(ArkxError::Backend(format!(
                "nothing to archive ({} skipped: empty, unreadable or special files only)",
                skipped
            )));
        }

        let file = File::create(dest).map_err(ArkxError::Io)?;
        let writer = BufWriter::with_capacity(IO_BUF_SIZE, file);
        let mut zip = zip::ZipWriter::new(writer);
        let options: zip::write::FileOptions<()> = zip::write::FileOptions::default()
            .compression_method(match level {
                0 => zip::CompressionMethod::Stored,
                1..=3 => zip::CompressionMethod::Deflated,
                _ => zip::CompressionMethod::Deflated,
            })
            .compression_level(Some(level as i64));
        // AES-256 per entry when a password was requested (aes-crypto feature).
        let options = match password {
            Some(pw) => options.with_aes_encryption(zip::AesMode::Aes256, pw),
            None => options,
        };

        let base = sources
            .first()
            .and_then(|p| p.parent())
            .unwrap_or(Path::new("."));
        // Stable structure: dirs first (sorted), then files: empty folders are
        // thus preserved as in tar.
        dirs.sort();
        let shared = progress.map(|cb| Arc::new(Mutex::new(cb)));
        // Byte-based progress: the bar reads source bytes actually being
        // compressed (dirs contribute 0), so speed, ETA and "Written" match
        // reality instead of counting entries.
        let total_src: u64 = files
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len()))
            .sum::<u64>();

        // Parallel compression gate (2c): no password (AES needs the serial
        // writer path), real compression (Stored adds no CPU work to spread),
        // more than one file to share, and enough headroom so the per-thread
        // in-memory mini-zips cannot exhaust RAM. Each worker produces a
        // compressed chunk into memory; the main thread then muxes the chunks
        // into `dest` verbatim (`merge_archive` copies raw deflate data, no
        // re-compression: the CPU win is real, the output is bit-identical).
        // The crate's `merge_archive` drops the ZIP64 extra field from the
        // central-directory entries it merges for plain (non-AES) large files,
        // producing a zip that strict readers (7z) reject ("Sub items Errors").
        // Entries that need ZIP64 go down the serial writer, which emits the
        // extra field correctly.
        let any_zip64 = files.iter().any(|p| {
            std::fs::metadata(p)
                .map(|m| zip_entry_large(m.len()))
                .unwrap_or(false)
        });
        let parallel = password.is_none()
            && level > 0
            && !files.is_empty()
            && !any_zip64
            && crate::core::util::available_memory_mb()
                .map(|avail_mb| {
                    // Peak in-flight: ~each worker holds its compressed chunk
                    // (≤ total source) plus the final archive → cap at 3/4 RAM.
                    total_src.saturating_mul(2) <= avail_mb * 1024 * 1024 * 3 / 4
                })
                .unwrap_or(false);

        if parallel {
            use rayon::prelude::*;
            use std::io::Cursor;
            // Directories first, serial (cheap headers, order preserved).
            for path in dirs.iter() {
                if self.cancelled() {
                    drop(zip);
                    Self::discard_partial(dest);
                    return Err(ArkxError::Cancelled);
                }
                let rel = prefixed_name(path, base);
                let name = if rel.ends_with('/') || rel.is_empty() {
                    rel
                } else {
                    format!("{}/", rel)
                };
                if name.is_empty() || name == "/" {
                    continue;
                }
                emit_progress(&shared, ProgressInfo::new(name.clone(), 0, total_src));
                zip.add_directory(name, options)
                    .map_err(|e| ArkxError::Backend(e.to_string()))?;
            }
            // Files : chunked across cores; each worker emits a self-contained
            // mini-zip whose entries get merged raw into the final archive.
            let entries: Vec<(String, PathBuf)> = files
                .iter()
                .filter_map(|p| {
                    let rel = prefixed_name(p, base);
                    if rel.is_empty() || rel == "/" {
                        None
                    } else {
                        Some((rel, p.clone()))
                    }
                })
                .collect();
            let n_chunks = crate::core::util::effective_threads().min(entries.len());
            // Byte progress during the CPU-bound phase: workers count the
            // source bytes each reads into a shared atomic; a lightweight
            // poller turns that into progress events (the cb is never called
            // from inside rayon, where locking would stall the pool).
            let read_bytes = Arc::new(AtomicU64::new(0));
            let current_name: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
            let io_stop = Arc::new(AtomicBool::new(false));
            // Last value the poller put on the bar: the merge anchor must
            // continue from there, otherwise the target jumps to 100%.
            let shown = Arc::new(AtomicU64::new(0));
            // Merge-phase staging (consumed by the poller below): once every
            // chunk is read+deflated, the destination file size carries the
            // bar from the read anchor up to `total_src`. The dest grows
            // continuously as entries are copied, so the merge signal is
            // byte-granular — unlike counting completed buffers, which bursts
            // (one step per mini-zip) and freezes in between.
            let merge_started = Arc::new(AtomicBool::new(false));
            let merge_anchor = Arc::new(AtomicU64::new(0));
            let merge_total_bytes = Arc::new(AtomicU64::new(0));
            // Dest file size at merge start: `current size − anchor` is the
            // merged+written volume (monotonic, continuous ≈ 1 MiB flushes).
            let merge_dest_anchor = Arc::new(AtomicU64::new(0));
            // Set once the merge completes normally: the poller's final tick
            // then lands the bar exactly on the completed state instead of
            // leaving a last-visible-% → 100 jump (cancelled/error merges
            // keep the gap so the bar never fakes completion).
            let merge_normal = Arc::new(AtomicBool::new(false));
            let poller = if shared.is_some() {
                let read_bytes = read_bytes.clone();
                let current_name = current_name.clone();
                let stop = io_stop.clone();
                let merge_started = merge_started.clone();
                let merge_anchor = merge_anchor.clone();
                let merge_total_bytes = merge_total_bytes.clone();
                let merge_dest_anchor = merge_dest_anchor.clone();
                let shared_p = shared.clone();
                let shown = shown.clone();
                let merge_normal = merge_normal.clone();
                let dest_path = dest.to_path_buf();
                Some(std::thread::spawn(move || {
                    // Fixed-cadence + lively smoother: the read counter is
                    // near-continuous (byte-granular), so it tracks real
                    // progress within a ~10% step and the merge maps the
                    // remaining bar off the destination size, so it never
                    // freezes at the phase boundary or between buffers.
                    let mut smoother = Smoother::lively(total_src);
                    loop {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        let raw = read_bytes.load(Ordering::Relaxed).min(total_src);
                        let target = if merge_started.load(Ordering::Relaxed) {
                            let growth = std::fs::metadata(&dest_path)
                                .map(|m| m.len())
                                .unwrap_or(0)
                                .saturating_sub(merge_dest_anchor.load(Ordering::Relaxed));
                            merge_progress_target(
                                merge_anchor.load(Ordering::Relaxed),
                                total_src,
                                growth,
                                merge_total_bytes.load(Ordering::Relaxed),
                            )
                        } else {
                            raw
                        };
                        smoother.nudge(target);
                        if stop.load(Ordering::Relaxed) && merge_normal.load(Ordering::Relaxed) {
                            smoother.complete();
                        }
                        let current = smoother.value();
                        shown.store(current, Ordering::Relaxed);
                        let label = current_name.lock().map(|g| g.clone()).unwrap_or_default();
                        emit_progress(&shared_p, ProgressInfo::new(label, current, total_src));
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                }))
            } else {
                None
            };
            let results: Vec<(Vec<u8>, u64)> = entries
                .par_chunks(entries.len().div_ceil(n_chunks))
                .map(|chunk| {
                    let mut buf: Vec<u8> = Vec::new();
                    let mut cw = zip::ZipWriter::new(Cursor::new(&mut buf));
                    for (name, p) in chunk {
                        let file = File::open(p).map_err(ArkxError::Io)?;
                        let large = zip_entry_large(file.metadata().map(|m| m.len()).unwrap_or(0));
                        cw.start_file(name, options.large_file(large))
                            .map_err(|e| ArkxError::Backend(e.to_string()))?;
                        if let Ok(mut g) = current_name.lock() {
                            *g = name.clone();
                        }
                        let f = BufReader::with_capacity(IO_BUF_SIZE, file);
                        let mut cf = CountingReader {
                            inner: f,
                            counter: read_bytes.clone(),
                        };
                        std::io::copy(&mut cf, &mut cw).map_err(ArkxError::Io)?;
                    }
                    cw.finish().map_err(|e| ArkxError::Backend(e.to_string()))?;
                    Ok::<(Vec<u8>, u64), ArkxError>((buf, chunk.len() as u64))
                })
                .collect::<Result<Vec<_>>>()?;
            // Switch the poller to the merge stage: the read anchor and the
            // compressed bytes (known only after `.collect()`) drive the
            // remaining progress, so the bar keeps climbing through the merge
            // instead of freezing until the final `completed` jump.
            let anchor = shown.load(Ordering::Relaxed).min(total_src);
            let merged_total: u64 = results.iter().map(|(buf, _)| buf.len() as u64).sum();
            if let Ok(mut g) = current_name.lock() {
                *g = "Merging…".to_string();
            }
            let dest_anchor = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
            merge_anchor.store(anchor, Ordering::Relaxed);
            merge_total_bytes.store(merged_total, Ordering::Relaxed);
            merge_dest_anchor.store(dest_anchor, Ordering::Relaxed);
            merge_started.store(true, Ordering::Relaxed);
            let mut cancelled_merge = false;
            let mut merge_err: Option<ArkxError> = None;
            for (buf, _n) in results {
                if self.cancelled() {
                    cancelled_merge = true;
                    break;
                }
                let source = match zip::ZipArchive::new(Cursor::new(buf)) {
                    Ok(z) => z,
                    Err(e) => {
                        merge_err = Some(ArkxError::Backend(e.to_string()));
                        break;
                    }
                };
                if let Err(e) = zip.merge_archive(source) {
                    merge_err = Some(ArkxError::Backend(e.to_string()));
                    break;
                }
            }
            if !cancelled_merge && merge_err.is_none() {
                merge_normal.store(true, Ordering::Relaxed);
            }
            io_stop.store(true, Ordering::Relaxed);
            if let Some(h) = poller {
                let _ = h.join();
            }
            if let Some(e) = merge_err {
                drop(zip);
                Self::discard_partial(dest);
                return Err(e);
            }
            if cancelled_merge {
                drop(zip);
                Self::discard_partial(dest);
                return Err(ArkxError::Cancelled);
            }
            emit_progress(&shared, crate::core::util::completed(total_src));
            // finish() writes the central directory but does NOT flush the
            // BufWriter: without explicit flush small zips stay truncated.
            let writer = zip
                .finish()
                .map_err(|e| ArkxError::Backend(e.to_string()))?;
            let mut file = writer
                .into_inner()
                .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
            file.flush().map_err(ArkxError::Io)?;
            if skipped > 0 {
                eprintln!(
                    "[native] zip: skipped {} unreadable/special entries",
                    skipped
                );
            }
            return Ok(());
        }

        let mut done_bytes = 0u64;
        for path in dirs.iter().chain(files.iter()) {
            if self.cancelled() {
                drop(zip);
                Self::discard_partial(dest);
                return Err(ArkxError::Cancelled);
            }
            let rel = prefixed_name(path, base);
            let is_dir = path.is_dir();
            let name = if is_dir {
                if rel.ends_with('/') {
                    rel
                } else {
                    format!("{}/", rel)
                }
            } else {
                rel
            };
            if name.is_empty() || name == "/" {
                continue;
            }
            if is_dir {
                // Dirs carry 0 output bytes: keep the "current item" label
                // moving, but never fake progress for empty folders.
                emit_progress(
                    &shared,
                    ProgressInfo::new(name.clone(), done_bytes, total_src),
                );
                zip.add_directory(name, options)
                    .map_err(|e| ArkxError::Backend(e.to_string()))?;
                continue;
            }
            let file = File::open(path).map_err(ArkxError::Io)?;
            let size = file.metadata().map(|m| m.len()).unwrap_or(0);
            let large = zip_entry_large(size);
            zip.start_file(name.clone(), options.large_file(large))
                .map_err(|e| ArkxError::Backend(e.to_string()))?;
            let mut f = BufReader::with_capacity(IO_BUF_SIZE, file);
            std::io::copy(&mut f, &mut zip).map_err(ArkxError::Io)?;
            done_bytes = done_bytes.saturating_add(size);
            emit_progress(
                &shared,
                ProgressInfo::new(name.clone(), done_bytes.min(total_src), total_src),
            );
        }
        emit_progress(&shared, crate::core::util::completed(total_src));
        // finish() writes the central directory but does NOT flush the BufWriter:
        // without explicit flush small zips (<1MB) stay truncated/empty.
        let writer = zip
            .finish()
            .map_err(|e| ArkxError::Backend(e.to_string()))?;
        let mut file = writer
            .into_inner()
            .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
        use std::io::Write as _WriteFlush;
        file.flush().map_err(ArkxError::Io)?;
        if skipped > 0 {
            eprintln!(
                "[native] zip: skipped {} unreadable/special entries",
                skipped
            );
        }
        Ok(())
    }

    fn add_zip(
        &self,
        archive: &Path,
        sources: &[(PathBuf, String)],
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        // Encrypted entries cannot be recompressed without the password:
        // refuse early instead of failing halfway through the rewrite.
        let refuse_encrypted = || -> Result<()> {
            Err(ArkxError::Backend(
                "adding to an encrypted archive is not supported".into(),
            ))
        };
        let mut old = {
            let file = File::open(archive).map_err(ArkxError::Io)?;
            let mut z = zip::ZipArchive::new(BufReader::with_capacity(IO_BUF_SIZE, file))
                .map_err(|e| ArkxError::Corrupted(e.to_string()))?;
            for i in 0..z.len() {
                match z.by_index(i) {
                    // `by_index` refuses to open an encrypted entry without a
                    // password; any match with this message means encryption.
                    Err(zip::result::ZipError::UnsupportedArchive(
                        zip::result::ZipError::PASSWORD_REQUIRED,
                    )) => return refuse_encrypted(),
                    Ok(f) if f.encrypted() => return refuse_encrypted(),
                    Err(e) => return Err(ArkxError::Corrupted(format!("entry {}: {}", i, e))),
                    Ok(_) => {}
                }
            }
            z
        };

        // New source names: colliding existing entries are replaced (like `7z a`).
        let replace: std::collections::HashSet<String> =
            sources.iter().map(|(_, n)| n.clone()).collect();

        // Expand the new sources to flat (path, entry-name) pairs, dirs included.
        let mut new_items: Vec<(PathBuf, String)> = Vec::new();
        for (src, name) in sources {
            if src.is_dir() {
                for entry in walkdir::WalkDir::new(src).min_depth(0).into_iter() {
                    let entry = match entry {
                        Ok(e) => e,
                        Err(_) => continue,
                    };
                    let p = entry.path();
                    let is_dir = p.is_dir();
                    let rel = p.strip_prefix(src).unwrap_or(p);
                    let entry_name = if rel.as_os_str().is_empty() {
                        name.clone()
                    } else {
                        format!("{}/{}", name.trim_end_matches('/'), rel.to_string_lossy())
                    };
                    let entry_name = if is_dir {
                        crate::core::paths::with_trailing_slash(&entry_name)
                    } else {
                        entry_name
                    };
                    if !entry_name.is_empty() && entry_name != "/" {
                        new_items.push((p.to_path_buf(), entry_name));
                    }
                }
            } else if src.is_file() {
                new_items.push((src.clone(), name.clone()));
            }
        }
        if new_items.is_empty() {
            return Err(ArkxError::Backend(
                "nothing to add (empty, unreadable or special files only)".into(),
            ));
        }

        let mut tmp = archive.as_os_str().to_os_string();
        tmp.push(format!(".arkx-{}.part", std::process::id()));
        let tmp = PathBuf::from(tmp);

        let shared = progress.map(|cb| Arc::new(Mutex::new(cb)));
        let mut total_bytes = 0u64;
        for i in 0..old.len() {
            if let Ok(f) = old.by_index(i) {
                let name = f.name().to_string();
                if !replace.contains(&name) && !f.is_dir() {
                    total_bytes = total_bytes.saturating_add(f.size());
                }
            }
        }
        total_bytes = new_items.iter().fold(total_bytes, |acc, (p, _)| {
            if p.is_dir() {
                acc
            } else {
                acc.saturating_add(std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
            }
        });
        let result: Result<()> = (|| {
            let file = File::create(&tmp).map_err(ArkxError::Io)?;
            let mut zip = zip::ZipWriter::new(BufWriter::with_capacity(IO_BUF_SIZE, file));
            let new_opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(6));
            let mut done_bytes = 0u64;

            for i in 0..old.len() {
                let mut f = old
                    .by_index(i)
                    .map_err(|e| ArkxError::Corrupted(e.to_string()))?;
                let name = f.name().to_string();
                if replace.contains(&name) {
                    continue;
                }
                let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                    .compression_method(f.compression())
                    .large_file(zip_entry_large(f.size()));
                if f.is_dir() {
                    zip.add_directory(name.clone(), opts)
                        .map_err(|e| ArkxError::Backend(e.to_string()))?;
                } else {
                    zip.start_file(name.clone(), opts)
                        .map_err(|e| ArkxError::Backend(e.to_string()))?;
                    std::io::copy(&mut f, &mut zip).map_err(ArkxError::Io)?;
                    done_bytes = done_bytes.saturating_add(f.size());
                }
                emit_progress(
                    &shared,
                    ProgressInfo::new(
                        name.clone(),
                        done_bytes.min(total_bytes),
                        total_bytes.max(1),
                    ),
                );
            }
            for (path, name) in &new_items {
                if path.is_dir() {
                    zip.add_directory(name, new_opts)
                        .map_err(|e| ArkxError::Backend(e.to_string()))?;
                } else {
                    let file = File::open(path).map_err(ArkxError::Io)?;
                    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
                    let large = zip_entry_large(size);
                    zip.start_file(name, new_opts.large_file(large))
                        .map_err(|e| ArkxError::Backend(e.to_string()))?;
                    let mut f = BufReader::with_capacity(IO_BUF_SIZE, file);
                    std::io::copy(&mut f, &mut zip).map_err(ArkxError::Io)?;
                    done_bytes = done_bytes.saturating_add(size);
                }
                emit_progress(
                    &shared,
                    ProgressInfo::new(
                        name.clone(),
                        done_bytes.min(total_bytes),
                        total_bytes.max(1),
                    ),
                );
            }
            emit_progress(&shared, crate::core::util::completed(total_bytes));
            let writer = zip
                .finish()
                .map_err(|e| ArkxError::Backend(e.to_string()))?;
            let mut file = writer
                .into_inner()
                .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
            use std::io::Write as _WriteFlush;
            file.flush().map_err(ArkxError::Io)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return result;
        }
        // Atomic swap on the same filesystem (temp is a sibling).
        std::fs::rename(&tmp, archive).map_err(ArkxError::Io)?;
        Ok(())
    }

    fn remove_zip(
        &self,
        archive: &Path,
        entries: &[String],
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let sels = crate::core::paths::normalize_sel(entries);
        let skip = move |name: &str| {
            let norm = crate::core::paths::normalize(name);
            sels.iter()
                .any(|s| crate::core::paths::entry_matches_norm(&norm, s))
        };
        self.rewrite_zip(archive, None, &skip, progress)
    }

    /// Set the archive comment (zip). Re-writes the whole archive, same
    /// copy loop as remove/rename: the crate offers no in-place comment edit.
    pub(crate) fn set_comment_zip(&self, archive: &Path, comment: &str) -> Result<()> {
        let keep = |_: &str| false;
        self.rewrite_zip(archive, Some(comment), &keep, None)
    }

    /// Shared zip re-write: copies every non-skipped entry into a temp file
    /// and atomically replaces the archive. `comment` is applied when given.
    fn rewrite_zip(
        &self,
        archive: &Path,
        comment: Option<&str>,
        skip: &dyn Fn(&str) -> bool,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        // Encrypted entries cannot be recompressed without the password.
        let refuse_encrypted = || -> Result<()> {
            Err(ArkxError::Backend(
                "rewriting an encrypted archive is not supported".into(),
            ))
        };
        let mut old = {
            let file = File::open(archive).map_err(ArkxError::Io)?;
            let mut z = zip::ZipArchive::new(BufReader::with_capacity(IO_BUF_SIZE, file))
                .map_err(|e| ArkxError::Corrupted(e.to_string()))?;
            for i in 0..z.len() {
                match z.by_index(i) {
                    Err(zip::result::ZipError::UnsupportedArchive(
                        zip::result::ZipError::PASSWORD_REQUIRED,
                    )) => return refuse_encrypted(),
                    Ok(f) if f.encrypted() => return refuse_encrypted(),
                    Err(e) => return Err(ArkxError::Corrupted(format!("entry {}: {}", i, e))),
                    Ok(_) => {}
                }
            }
            z
        };

        let mut tmp = archive.as_os_str().to_os_string();
        tmp.push(format!(".arkx-{}.part", std::process::id()));
        let tmp = PathBuf::from(tmp);

        let shared = progress.map(|cb| Arc::new(Mutex::new(cb)));
        let mut total_bytes = 0u64;
        for i in 0..old.len() {
            if let Ok(f) = old.by_index(i) {
                let name = f.name().to_string();
                if !skip(&name) && !f.is_dir() {
                    total_bytes = total_bytes.saturating_add(f.size());
                }
            }
        }
        let result: Result<()> = (|| {
            let file = File::create(&tmp).map_err(ArkxError::Io)?;
            let mut zip = zip::ZipWriter::new(BufWriter::with_capacity(IO_BUF_SIZE, file));
            if let Some(c) = comment {
                zip.set_comment(c);
            }
            let mut done_bytes = 0u64;
            for i in 0..old.len() {
                let mut f = old
                    .by_index(i)
                    .map_err(|e| ArkxError::Corrupted(e.to_string()))?;
                let name = f.name().to_string();
                if skip(&name) {
                    continue;
                }
                let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                    .compression_method(f.compression())
                    .large_file(zip_entry_large(f.size()));
                if f.is_dir() {
                    zip.add_directory(name.clone(), opts)
                        .map_err(|e| ArkxError::Backend(e.to_string()))?;
                } else {
                    zip.start_file(name.clone(), opts)
                        .map_err(|e| ArkxError::Backend(e.to_string()))?;
                    std::io::copy(&mut f, &mut zip).map_err(ArkxError::Io)?;
                    done_bytes = done_bytes.saturating_add(f.size());
                }
                emit_progress(
                    &shared,
                    ProgressInfo::new(
                        name.clone(),
                        done_bytes.min(total_bytes),
                        total_bytes.max(1),
                    ),
                );
            }
            emit_progress(&shared, crate::core::util::completed(total_bytes));
            let writer = zip
                .finish()
                .map_err(|e| ArkxError::Backend(e.to_string()))?;
            let mut file = writer
                .into_inner()
                .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
            use std::io::Write as _WriteFlush;
            file.flush().map_err(ArkxError::Io)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return result;
        }
        std::fs::rename(&tmp, archive).map_err(ArkxError::Io)?;
        Ok(())
    }

    fn create_tar(
        &self,
        dest: &Path,
        sources: &[PathBuf],
        fmt: &ArchiveFormat,
        level: u8,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        // As for zip: collect first, create dest after (anti self-inclusion).
        let dest_abs = absolutize(dest);
        let mut files: Vec<PathBuf> = Vec::new();
        let mut skipped = 0u32;
        for src in sources {
            if src.is_dir() {
                for entry in walkdir::WalkDir::new(src).min_depth(0).into_iter() {
                    match entry {
                        Ok(e) => {
                            if !is_same_path(e.path(), &dest_abs) {
                                files.push(e.path().to_path_buf());
                            }
                        }
                        Err(_) => {
                            skipped += 1;
                        }
                    }
                }
            } else {
                if !src.exists() {
                    return Err(ArkxError::Io(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("not found: {}", src.display()),
                    )));
                }
                if !is_same_path(src, &dest_abs) {
                    files.push(src.clone());
                }
            }
        }
        if files.is_empty() {
            return Err(ArkxError::Backend(
                "nothing to archive (empty or unreadable sources)".into(),
            ));
        }
        if skipped > 0 {
            eprintln!("[native] tar: skipped {} unreadable entries", skipped);
        }

        let file = File::create(dest).map_err(ArkxError::Io)?;
        let writer = create_tar_writer(file, fmt, level)?;
        let mut tar = tar::Builder::new(writer);

        let total = files.len() as u64;
        // Single source directory: keep its parent as base so the folder
        // name itself lands inside the tar
        let base = if sources.len() == 1 && sources[0].is_dir() {
            sources[0].parent().unwrap_or(Path::new(".")).to_path_buf()
        } else {
            sources
                .first()
                .and_then(|p| p.parent())
                .map(|p| p.to_path_buf())
                .unwrap_or(PathBuf::from("."))
        };

        for (i, path) in files.iter().enumerate() {
            if self.cancelled() {
                drop(tar);
                Self::discard_partial(dest);
                return Err(ArkxError::Cancelled);
            }
            let rel = path.strip_prefix(&base).unwrap_or(path);
            if let Some(cb) = &progress {
                cb(ProgressInfo::new(
                    rel.to_string_lossy().to_string(),
                    i as u64 + 1,
                    total.max(1),
                ));
            }
            if path.is_dir() {
                tar.append_dir(rel, path)
                    .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
            } else {
                tar.append_path_with_name(path, rel)
                    .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
            }
        }
        if let Some(cb) = &progress {
            cb(crate::core::util::completed(total));
        }
        // Tar trailer (1024 zeros), then MANDATORY codec finish():
        // zstd (and in theory the others) leaves incomplete frames without finish.
        tar.finish()
            .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
        let writer = tar
            .into_inner()
            .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
        writer.finish()?;
        Ok(())
    }

    /// Create a single-file compressed stream (gz/bz2/xz/zst/lz4): exactly one
    /// source file, streamed codec-style (no tar wrapper). The output name
    /// carries the extension (`report.txt.gz`), so extraction (and `gzip -d`)
    /// recover `report.txt` from the file name itself.
    fn create_single(
        &self,
        dest: &Path,
        sources: &[PathBuf],
        fmt: &ArchiveFormat,
        level: u8,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        if sources.len() != 1 || !sources[0].is_file() {
            return Err(ArkxError::Backend(
                "single-file formats (gz/bz2/xz/zst/lz4) compress exactly one file; use .tar.gz/.tar.xz/.tar.zstd/.tar.bz2/.tar.lz4 for folders".into(),
            ));
        }
        let src = &sources[0];
        if is_same_path(src, &absolutize(dest)) {
            return Err(ArkxError::Backend(
                "cannot compress a file into itself".into(),
            ));
        }
        let name = src
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        let total = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);

        let file = File::create(dest).map_err(ArkxError::Io)?;
        let mut writer = create_tar_writer(file, fmt, level)?;
        let mut input =
            BufReader::with_capacity(IO_BUF_SIZE, File::open(src).map_err(ArkxError::Io)?);
        let mut buf = vec![0u8; COPY_CHUNK];
        let mut current = 0u64;
        if let Some(cb) = &progress {
            cb(ProgressInfo::new(name.clone(), 0, total));
        }
        loop {
            if self.cancelled() {
                drop(writer);
                Self::discard_partial(dest);
                return Err(ArkxError::Cancelled);
            }
            let n = input.read(&mut buf).map_err(ArkxError::Io)?;
            if n == 0 {
                break;
            }
            writer.write_all(&buf[..n]).map_err(ArkxError::Io)?;
            current = current.saturating_add(n as u64);
            if total > 0 {
                if let Some(cb) = &progress {
                    cb(ProgressInfo::new(name.clone(), current.min(total), total));
                }
            }
        }
        writer.finish()?;
        if let Some(cb) = &progress {
            cb(crate::core::util::completed(total));
        }
        Ok(())
    }
}

fn create_tar_reader(file: File, path: &Path) -> Result<Box<dyn std::io::Read>> {
    let fmt = crate::core::detector::detect_format(path);
    let reader: Box<dyn std::io::Read> =
        match fmt {
            ArchiveFormat::TarGz => Box::new(flate2::read::GzDecoder::new(
                BufReader::with_capacity(IO_BUF_SIZE, file),
            )),
            ArchiveFormat::TarBz2 => Box::new(bzip2::read::BzDecoder::new(
                BufReader::with_capacity(IO_BUF_SIZE, file),
            )),
            ArchiveFormat::TarXz => Box::new(xz2::read::XzDecoder::new(BufReader::with_capacity(
                IO_BUF_SIZE,
                file,
            ))),
            ArchiveFormat::TarZst => Box::new(
                zstd::stream::read::Decoder::new(BufReader::with_capacity(IO_BUF_SIZE, file))
                    .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?,
            ),
            ArchiveFormat::TarLz4 => Box::new(lz4_flex::frame::FrameDecoder::new(
                BufReader::with_capacity(IO_BUF_SIZE, file),
            )),
            ArchiveFormat::Tar => Box::new(BufReader::with_capacity(IO_BUF_SIZE, file)),
            _ => {
                return Err(ArkxError::UnsupportedFormat(format!(
                    "tar reader {:?}",
                    fmt
                )))
            }
        };
    Ok(reader)
}

fn create_single_reader(file: File, path: &Path) -> Result<Box<dyn std::io::Read>> {
    let fmt = crate::core::detector::detect_format(path);
    let reader: Box<dyn std::io::Read> = match fmt {
        ArchiveFormat::Gz => Box::new(flate2::read::GzDecoder::new(BufReader::with_capacity(
            IO_BUF_SIZE,
            file,
        ))),
        ArchiveFormat::Bz2 => Box::new(bzip2::read::BzDecoder::new(BufReader::with_capacity(
            IO_BUF_SIZE,
            file,
        ))),
        ArchiveFormat::Xz => Box::new(xz2::read::XzDecoder::new(BufReader::with_capacity(
            IO_BUF_SIZE,
            file,
        ))),
        ArchiveFormat::Zst => Box::new(
            zstd::stream::read::Decoder::new(BufReader::with_capacity(IO_BUF_SIZE, file))
                .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?,
        ),
        ArchiveFormat::Lz4 => Box::new(lz4_flex::frame::FrameDecoder::new(
            BufReader::with_capacity(IO_BUF_SIZE, file),
        )),
        _ => {
            return Err(ArkxError::UnsupportedFormat(format!(
                "single reader {:?}",
                fmt
            )))
        }
    };
    Ok(reader)
}

/// Intermediate writer for `create_tar`: enum (not `Box<dyn Write>`) so at the end
/// of the archive the REAL `finish()` of each codec can be called. Without finish,
/// zstd leaves incomplete frames and the archive ends up corrupted (verified:
/// `tar tzf` failed and bsdtar said "Truncated input file").
enum TarWriter {
    Plain(BufWriter<File>),
    Gz(flate2::write::GzEncoder<BufWriter<File>>),
    Bz(bzip2::write::BzEncoder<BufWriter<File>>),
    Xz(xz2::write::XzEncoder<BufWriter<File>>),
    Zst(zstd::stream::write::Encoder<'static, BufWriter<File>>),
    Lz4(lz4_flex::frame::FrameEncoder<BufWriter<File>>),
}

impl std::io::Write for TarWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            TarWriter::Plain(w) => w.write(buf),
            TarWriter::Gz(w) => w.write(buf),
            TarWriter::Bz(w) => w.write(buf),
            TarWriter::Xz(w) => w.write(buf),
            TarWriter::Zst(w) => w.write(buf),
            TarWriter::Lz4(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            TarWriter::Plain(w) => w.flush(),
            TarWriter::Gz(w) => w.flush(),
            TarWriter::Bz(w) => w.flush(),
            TarWriter::Xz(w) => w.flush(),
            TarWriter::Zst(w) => w.flush(),
            TarWriter::Lz4(w) => w.flush(),
        }
    }
}

impl TarWriter {
    fn finish(self) -> Result<()> {
        match self {
            TarWriter::Plain(mut w) => w.flush().map_err(ArkxError::Io),
            TarWriter::Gz(e) => {
                let mut w = e
                    .finish()
                    .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
                w.flush().map_err(ArkxError::Io)
            }
            TarWriter::Bz(e) => {
                let mut w = e
                    .finish()
                    .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
                w.flush().map_err(ArkxError::Io)
            }
            TarWriter::Xz(e) => {
                let mut w = e
                    .finish()
                    .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
                w.flush().map_err(ArkxError::Io)
            }
            TarWriter::Zst(e) => {
                let mut w = e
                    .finish()
                    .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
                w.flush().map_err(ArkxError::Io)
            }
            TarWriter::Lz4(e) => {
                let mut w = e
                    .finish()
                    .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
                w.flush().map_err(ArkxError::Io)
            }
        }
    }
}

fn create_tar_writer(file: File, fmt: &ArchiveFormat, level: u8) -> Result<TarWriter> {
    let buf = BufWriter::with_capacity(IO_BUF_SIZE, file);
    // The user -l flag applies to all codecs (previously ignored:
    // fixed levels gz6/best/xz6/zst3). 0 = fast, 9 = max ratio.
    let writer = match fmt {
        ArchiveFormat::TarGz | ArchiveFormat::Gz => TarWriter::Gz(flate2::write::GzEncoder::new(
            buf,
            flate2::Compression::new(level.clamp(0, 9) as u32),
        )),
        ArchiveFormat::TarBz2 | ArchiveFormat::Bz2 => TarWriter::Bz(bzip2::write::BzEncoder::new(
            buf,
            bzip2::Compression::new(level.clamp(1, 9) as u32),
        )),
        ArchiveFormat::TarXz | ArchiveFormat::Xz => {
            // Adaptive multithreaded xz (workers scaled on CPU/RAM): the
            // stream stays standard .xz, any decoder can read it. Falls back
            // to the single-threaded encoder when the system liblzma was
            // built without MT support (lzma_stream_encoder_mt unavailable).
            let mut mt = xz2::stream::MtStreamBuilder::new();
            let state = mt
                .threads(crate::core::util::zstd_workers().max(1))
                .preset(level.clamp(0, 9) as u32)
                .encoder();
            match state {
                Ok(state) => TarWriter::Xz(xz2::write::XzEncoder::new_stream(buf, state)),
                Err(_) => TarWriter::Xz(xz2::write::XzEncoder::new(buf, level.clamp(0, 9) as u32)),
            }
        }
        ArchiveFormat::TarZst | ArchiveFormat::Zst => {
            let mut enc = zstd::stream::write::Encoder::new(buf, zstd_level(level))
                .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
            // Adaptive multithreaded zstd compression (workers scaled on
            // CPU/RAM): the frame stays standard, any decoder can read it.
            let workers = crate::core::util::zstd_workers();
            if workers >= 1 {
                // on 1 thread it still separates IO and compression
                enc.multithread(workers)
                    .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
            }
            TarWriter::Zst(enc)
        }
        ArchiveFormat::TarLz4 | ArchiveFormat::Lz4 => {
            TarWriter::Lz4(lz4_flex::frame::FrameEncoder::new(buf))
        }
        _ => TarWriter::Plain(buf),
    };
    Ok(writer)
}

/// Maps user level 0-9 onto the zstd 1-22 scale.
fn zstd_level(user: u8) -> i32 {
    const TABLE: [i32; 10] = [1, 3, 5, 7, 9, 12, 15, 17, 19, 22];
    TABLE[user.clamp(0, 9) as usize]
}

/// Safe join under `dest`: normalizes and rejects `..` (zip-slip from hostile
/// archives) and empty names. Returns `None` for entries to discard.
/// Resolve `dest` once per extraction: `dest` may be reached through a
/// symlink (e.g. a symlinked `~/Downloads`); all entry checks compare against
/// this resolved form, otherwise every entry would be discarded.
fn resolved_base(dest: &Path) -> PathBuf {
    std::fs::canonicalize(dest).unwrap_or_else(|_| absolutize(dest))
}

/// Secure join of archive `name` into `dest`: `base` is the precomputed
/// resolved form of `dest` (see [`resolved_base`]); `name` must be
/// normalized to a relative path without `..`.
fn secure_join(dest: &Path, base: &Path, name: &str) -> Option<PathBuf> {
    let norm = crate::core::paths::normalize(name);
    if norm.is_empty() {
        return None;
    }
    if Path::new(&norm)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    let full = dest.join(&norm);
    // Reject if any component along the path is a symlink that resolves
    // outside dest (zip-slip via symlinks).
    match std::fs::canonicalize(&full) {
        Ok(canonical) => {
            if canonical.starts_with(base) {
                Some(full)
            } else {
                None
            }
        }
        // File doesn't exist yet — check intermediate symlinks.
        Err(_) => {
            let mut current = base.to_path_buf();
            for component in Path::new(&norm).components() {
                current = current.join(component);
                // If this component exists, it must be under dest.
                if let Ok(meta) = std::fs::symlink_metadata(&current) {
                    if meta.file_type().is_symlink() {
                        // Resolve symlink and verify target is under dest.
                        match std::fs::canonicalize(&current) {
                            Ok(canonical) => {
                                if !canonical.starts_with(base) {
                                    return None;
                                }
                            }
                            Err(_) => return None,
                        }
                    }
                }
            }
            Some(full)
        }
    }
}

/// Lexically resolve `path` (which may contain `.`/`..`) against `base`,
/// without touching the filesystem.
fn resolve_lexically(base: &Path, path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Absolutizes without touching the fs (dest may not exist yet).
pub(crate) fn absolutize(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(p)
    }
}

/// True if `p` is the destination (textual comparison + canonical when possible).
fn is_same_path(p: &Path, dest_abs: &Path) -> bool {
    if absolutize(p) == *dest_abs {
        return true;
    }
    match (std::fs::canonicalize(p), std::fs::canonicalize(dest_abs)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Name inside the zip: strip of `base`, with fallback to file_name only
/// (never absolute paths: `dest.join("/abs")` during extraction would point
/// outside dest — zip-slip — and odd names confuse the browser).
fn prefixed_name(path: &Path, base: &Path) -> String {
    if let Ok(rel) = path.strip_prefix(base) {
        let s = rel.to_string_lossy().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "file".to_string())
}

/// ZIP64 must be enabled once a single entry exceeds 4 GiB−1 uncompressed:
/// the `zip` crate aborts the write otherwise with "Large file option has
/// not been set" (checked per entry, not on the whole archive).
fn zip_entry_large(size: u64) -> bool {
    size > zip::ZIP64_BYTES_THR
}

/// Read wrapper that counts source bytes read (≈ CPU work done) into a shared
/// atomic, so the parallel compression phase can report real byte progress.
struct CountingReader<R> {
    inner: R,
    counter: Arc<AtomicU64>,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.counter.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// Send a progress event if a sink was registered.
fn emit_progress(shared: &Option<SharedCallback>, info: ProgressInfo) {
    if let Some(cb) = shared {
        if let Ok(guard) = cb.lock() {
            guard(info);
        }
    }
}

impl NativeBackend {
    fn rename_zip(
        &self,
        archive: &Path,
        old_name: &str,
        new_name: &str,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let file = File::open(archive).map_err(ArkxError::Io)?;
        let mut z = zip::ZipArchive::new(BufReader::with_capacity(IO_BUF_SIZE, file))
            .map_err(|e| ArkxError::Corrupted(e.to_string()))?;
        let old_name_norm = crate::core::paths::normalize(old_name);
        let new_name_norm = crate::core::paths::normalize(new_name);

        // Renaming a folder `dir/` → `dir2/` also rebases its contents
        // (`dir/a.txt` → `dir2/a.txt`); otherwise the children keep living in
        // the old folder and the archive ends up with both, which looks like a
        // copy instead of a rename.
        let old_prefix = crate::core::paths::with_trailing_slash(&old_name_norm);
        let new_prefix = crate::core::paths::with_trailing_slash(&new_name_norm);
        // Target path of an entry affected by the rename.
        let rebased = |norm: &str, is_dir: bool| -> String {
            let target = if norm.starts_with(&old_prefix) && norm != old_name_norm {
                format!("{}{}", new_prefix, &norm[old_prefix.len()..])
            } else {
                new_name_norm.clone()
            };
            if is_dir {
                crate::core::paths::with_trailing_slash(&target)
            } else {
                target
            }
        };

        // Which entries move, and which target names they claim: an unrelated
        // entry that already has a claimed name is dropped (replaced).
        let mut affected: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut claimed: std::collections::HashSet<String> = std::collections::HashSet::new();
        for i in 0..z.len() {
            let f = z
                .by_index(i)
                .map_err(|e| ArkxError::Corrupted(e.to_string()))?;
            let norm = crate::core::paths::normalize(f.name());
            if norm == old_name_norm || norm.starts_with(&old_prefix) {
                affected.insert(norm.clone());
                claimed.insert(rebased(&norm, f.is_dir()));
            }
        }

        let mut tmp = archive.as_os_str().to_os_string();
        tmp.push(format!(".arkx-{}.part", std::process::id()));
        let tmp = PathBuf::from(tmp);

        let shared = progress.map(|cb| Arc::new(Mutex::new(cb)));
        let mut total_bytes = 0u64;
        for i in 0..z.len() {
            if let Ok(f) = z.by_index(i) {
                let norm = crate::core::paths::normalize(f.name());
                if (affected.contains(&norm) || !claimed.contains(&norm)) && !f.is_dir() {
                    total_bytes = total_bytes.saturating_add(f.size());
                }
            }
        }
        let result: Result<()> = (|| {
            let file = File::create(&tmp).map_err(ArkxError::Io)?;
            let mut zip = zip::ZipWriter::new(BufWriter::with_capacity(IO_BUF_SIZE, file));
            let mut done_bytes = 0u64;
            for i in 0..z.len() {
                let mut f = z
                    .by_index(i)
                    .map_err(|e| ArkxError::Corrupted(e.to_string()))?;
                let name = f.name().to_string();
                let norm = crate::core::paths::normalize(&name);
                // Affected entry → rebased name; otherwise keep the original,
                // unless a renamed entry claims that target (then drop it).
                let target = if affected.contains(&norm) {
                    rebased(&norm, f.is_dir())
                } else if claimed.contains(&norm) {
                    continue;
                } else {
                    name.clone()
                };
                let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                    .compression_method(f.compression())
                    .large_file(zip_entry_large(f.size()));
                if f.is_dir() {
                    zip.add_directory(&target, opts)
                        .map_err(|e| ArkxError::Backend(e.to_string()))?;
                } else {
                    zip.start_file(&target, opts)
                        .map_err(|e| ArkxError::Backend(e.to_string()))?;
                    std::io::copy(&mut f, &mut zip).map_err(ArkxError::Io)?;
                    done_bytes = done_bytes.saturating_add(f.size());
                }
                emit_progress(
                    &shared,
                    ProgressInfo::new(target, done_bytes.min(total_bytes), total_bytes.max(1)),
                );
            }
            emit_progress(&shared, crate::core::util::completed(total_bytes));
            let writer = zip
                .finish()
                .map_err(|e| ArkxError::Backend(e.to_string()))?;
            let mut file = writer
                .into_inner()
                .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
            use std::io::Write as _WriteFlush;
            file.flush().map_err(ArkxError::Io)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return result;
        }
        std::fs::rename(&tmp, archive).map_err(ArkxError::Io)?;
        Ok(())
    }

    fn test_zip(
        &self,
        archive: &Path,
        entries: Option<&[String]>,
    ) -> Result<crate::core::archive::TestReport> {
        let file = File::open(archive).map_err(ArkxError::Io)?;
        let mut z = zip::ZipArchive::new(BufReader::with_capacity(IO_BUF_SIZE, file))
            .map_err(|e| ArkxError::Corrupted(e.to_string()))?;
        let mut results = Vec::new();
        let mut passed = 0usize;
        let mut failed = 0usize;
        let sels = entries.map(crate::core::paths::normalize_sel);
        // Reused across entries to avoid one 64KB allocation per file.
        let mut buf = vec![0u8; COPY_CHUNK];
        for i in 0..z.len() {
            let mut f = z
                .by_index(i)
                .map_err(|e| ArkxError::Corrupted(e.to_string()))?;
            let name = f.name().to_string();
            if let Some(ref sel) = sels {
                let norm = crate::core::paths::normalize(&name);
                if !sel
                    .iter()
                    .any(|s| crate::core::paths::entry_matches_norm(&norm, s))
                {
                    continue;
                }
            }
            let is_dir = f.is_dir();
            let crc_expected = f.crc32();
            // Recompute CRC32 by reading the entry data
            let crc_actual = if is_dir {
                crc_expected
            } else {
                let mut hasher = crc32fast::Hasher::new();
                loop {
                    let n = f.read(&mut buf).map_err(ArkxError::Io)?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buf[..n]);
                }
                hasher.finalize()
            };
            let passed_entry = crc_expected == crc_actual;
            if passed_entry {
                passed += 1;
            } else {
                failed += 1;
            }
            results.push(crate::core::archive::TestResult {
                entry: name,
                is_dir,
                passed: passed_entry,
            });
        }
        Ok(crate::core::archive::TestReport {
            archive: archive.to_string_lossy().to_string(),
            results,
            passed,
            failed,
        })
    }

    fn open_with_zip(&self, archive: &Path, entry: &str, temp_dir: &Path) -> Result<PathBuf> {
        let file = File::open(archive).map_err(ArkxError::Io)?;
        let mut z = zip::ZipArchive::new(BufReader::with_capacity(IO_BUF_SIZE, file))
            .map_err(|e| ArkxError::Corrupted(e.to_string()))?;
        let idx = (0..z.len())
            .find(|&i| z.by_index(i).ok().map(|f| f.name().to_string()) == Some(entry.to_string()))
            .ok_or_else(|| ArkxError::Corrupted(format!("entry not found: {}", entry)))?;
        let mut f = z
            .by_index(idx)
            .map_err(|e| ArkxError::Corrupted(e.to_string()))?;
        let out_path = secure_join(temp_dir, &resolved_base(temp_dir), entry).ok_or_else(|| {
            crate::core::error::ArkxError::Backend(format!("unsafe entry path in archive: {entry}"))
        })?;
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(ArkxError::Io)?;
        }
        if f.is_dir() {
            std::fs::create_dir_all(&out_path).map_err(ArkxError::Io)?;
            return Ok(out_path);
        }
        // Avoid clobbering a same-named file extracted earlier by another
        // entry: keep the first occurrence.
        if out_path.exists() {
            return Ok(out_path);
        }
        let mut out = File::create(&out_path).map_err(ArkxError::Io)?;
        let mut buf = vec![0u8; COPY_CHUNK];
        loop {
            let n = f.read(&mut buf).map_err(Self::zip_read_err)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(ArkxError::Io)?;
        }
        out.flush().map_err(ArkxError::Io)?;
        Ok(out_path)
    }

    fn secure_delete_zip(
        &self,
        archive: &Path,
        entries: &[String],
        passes: usize,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let file = File::open(archive).map_err(ArkxError::Io)?;
        let mut z = zip::ZipArchive::new(BufReader::with_capacity(IO_BUF_SIZE, file))
            .map_err(|e| ArkxError::Corrupted(e.to_string()))?;
        let archive_len = archive.metadata().map(|m| m.len()).unwrap_or(0);
        let mut tmp = archive.as_os_str().to_os_string();
        tmp.push(format!(".arkx-secure-{}.part", std::process::id()));
        let tmp = PathBuf::from(tmp);

        let sels = crate::core::paths::normalize_sel(entries);
        let shared = progress.map(|cb| Arc::new(Mutex::new(cb)));
        let mut total_bytes = 0u64;
        for i in 0..z.len() {
            if let Ok(f) = z.by_index(i) {
                let norm = crate::core::paths::normalize(f.name());
                let selected = sels
                    .iter()
                    .any(|e| crate::core::paths::entry_matches_norm(&norm, e));
                if !selected && !f.is_dir() {
                    total_bytes = total_bytes.saturating_add(f.size());
                }
            }
        }
        let result: Result<()> = (|| {
            let file = File::create(&tmp).map_err(ArkxError::Io)?;
            let mut zip = zip::ZipWriter::new(BufWriter::with_capacity(IO_BUF_SIZE, file));
            let mut done_bytes = 0u64;
            for i in 0..z.len() {
                let mut f = z
                    .by_index(i)
                    .map_err(|e| ArkxError::Corrupted(e.to_string()))?;
                let name = f.name().to_string();
                let norm = crate::core::paths::normalize(&name);
                if sels
                    .iter()
                    .any(|e| crate::core::paths::entry_matches_norm(&norm, e))
                {
                    continue;
                }
                let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                    .compression_method(f.compression())
                    .large_file(zip_entry_large(f.size()));
                if f.is_dir() {
                    zip.add_directory(&name, opts)
                        .map_err(|e| ArkxError::Backend(e.to_string()))?;
                } else {
                    zip.start_file(&name, opts)
                        .map_err(|e| ArkxError::Backend(e.to_string()))?;
                    std::io::copy(&mut f, &mut zip).map_err(ArkxError::Io)?;
                    done_bytes = done_bytes.saturating_add(f.size());
                }
                emit_progress(
                    &shared,
                    ProgressInfo::new(name, done_bytes.min(total_bytes), total_bytes.max(1)),
                );
            }
            emit_progress(&shared, crate::core::util::completed(total_bytes));
            let writer = zip
                .finish()
                .map_err(|e| ArkxError::Backend(e.to_string()))?;
            let mut file = writer
                .into_inner()
                .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
            use std::io::Write as _WriteFlush;
            file.flush().map_err(ArkxError::Io)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return result;
        }
        // Overwrite the original archive's inode data before replacing
        // it with the clean archive. The original inode contained the
        // entries-to-delete data; after overwrite it's unrecoverable.
        for _ in 0..passes {
            let mut f = File::create(archive).map_err(ArkxError::Io)?;
            let mut urandom = File::open("/dev/urandom").map_err(ArkxError::Io)?;
            let mut buf = vec![0u8; COPY_CHUNK];
            let mut written = 0u64;
            while written < archive_len {
                let n = std::cmp::min(buf.len() as u64, archive_len - written) as usize;
                urandom.read_exact(&mut buf[..n]).map_err(ArkxError::Io)?;
                f.write_all(&buf[..n]).map_err(ArkxError::Io)?;
                written += n as u64;
            }
            f.flush().map_err(ArkxError::Io)?;
        }
        {
            let mut f = File::create(archive).map_err(ArkxError::Io)?;
            let zeros = vec![0u8; COPY_CHUNK];
            let mut written = 0u64;
            while written < archive_len {
                let n = std::cmp::min(zeros.len() as u64, archive_len - written) as usize;
                f.write_all(&zeros[..n]).map_err(ArkxError::Io)?;
                written += n as u64;
            }
            f.flush().map_err(ArkxError::Io)?;
        }
        std::fs::rename(&tmp, archive).map_err(ArkxError::Io)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::archive::ArchiveBackend;
    use std::sync::{atomic::AtomicBool, Arc};

    fn backend() -> NativeBackend {
        NativeBackend::with_cancel(Arc::new(AtomicBool::new(false)))
    }

    #[test]
    fn zip_entry_large_threshold() {
        assert!(!zip_entry_large(0));
        assert!(!zip_entry_large(zip::ZIP64_BYTES_THR));
        assert!(zip_entry_large(zip::ZIP64_BYTES_THR + 1));
    }

    #[test]
    #[ignore = "writes/reads ~4 GiB: run manually"]
    fn zip_create_entry_above_zip64_threshold() {
        // A single entry > 4 GiB−1 must survive the round trip with its real
        // size. `merge_archive` (parallel) drops the ZIP64 extra from plain
        // large entries, so the writer routes ZIP64 archives through the serial
        // path, which emits the extra field; `entry.size()` then equals the
        // real value instead of the 0xFFFFFFFF sentinel. Sparse source keeps
        // disk usage low; deflate turns the zeros into near-nothing.
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.dat");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(zip::ZIP64_BYTES_THR + 1).unwrap();
        drop(f);

        let dest = dir.path().join("big.zip");
        backend()
            .create(&dest, std::slice::from_ref(&big), 1, None, None, None)
            .unwrap();

        let file = std::fs::File::open(&dest).unwrap();
        let mut z = zip::ZipArchive::new(file).unwrap();
        let entry = z.by_index(0).unwrap();
        assert_eq!(entry.size(), zip::ZIP64_BYTES_THR + 1);
    }

    #[test]
    fn zip_keeps_symlink_target_and_empty_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("file.txt"), b"hi").unwrap();
        std::fs::create_dir(folder.join("empty_sub")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("file.txt", folder.join("link.txt")).unwrap();

        let dest = dir.path().join("folder.zip");
        backend()
            .create(&dest, std::slice::from_ref(&folder), 6, None, None, None)
            .unwrap();

        let info = backend().list(&dest).unwrap();
        let names: Vec<&str> = info.entries.iter().map(|e| e.path.as_str()).collect();
        assert!(
            names.contains(&"folder/file.txt"),
            "missing file: {:?}",
            names
        );
        assert!(
            names.contains(&"folder/empty_sub/"),
            "missing empty dir: {:?}",
            names
        );
        #[cfg(unix)]
        assert!(
            names.contains(&"folder/link.txt"),
            "symlink discarded: {:?}",
            names
        );

        // Round-trip: the extracted content must match.
        let out = dir.path().join("out");
        backend().extract(&dest, &out, None, None, None).unwrap();
        assert_eq!(std::fs::read(out.join("folder/file.txt")).unwrap(), b"hi");
        assert!(out.join("folder/empty_sub").is_dir());
    }

    #[test]
    fn zip_aes_wrong_password_surfaces_typed() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("secret.txt");
        std::fs::write(&src, b"classified").unwrap();
        let archive = dir.path().join("sec.zip");
        backend()
            .create(
                &archive,
                std::slice::from_ref(&src),
                6,
                Some("pw"),
                None,
                None,
            )
            .unwrap();

        let out = dir.path().join("out");
        let err = backend()
            .extract(&archive, &out, None, Some("wrong"), None)
            .unwrap_err();
        assert!(matches!(err, ArkxError::WrongPassword), "got: {err}");
        assert!(!out.join("secret.txt").exists());
    }

    #[test]
    fn zip_folder_with_only_empty_subdirs_is_not_empty() {
        // Regression DL.zip: folder with only empty subfolders.
        // The old code collected only files → valid 22-byte zip
        // with 0 entries without errors. Now the dir entries must be there.
        let dir = tempfile::tempdir().unwrap();
        let dl = dir.path().join("DL");
        std::fs::create_dir(&dl).unwrap();
        std::fs::create_dir(dl.join("Games")).unwrap();
        std::fs::create_dir(dl.join("Movies")).unwrap();

        let dest = dir.path().join("DL.zip");
        backend()
            .create(&dest, std::slice::from_ref(&dl), 6, None, None, None)
            .unwrap();

        assert!(
            std::fs::metadata(&dest).unwrap().len() > 22,
            "suspiciously empty zip"
        );
        let info = backend().list(&dest).unwrap();
        assert!(!info.entries.is_empty(), "no entries in {:?}", dest);
        let names: Vec<&str> = info.entries.iter().map(|e| e.path.as_str()).collect();
        assert!(names.contains(&"DL/"), "missing DL/: {:?}", names);
        assert!(names.contains(&"DL/Games/"), "missing Games/: {:?}", names);
        assert!(
            names.contains(&"DL/Movies/"),
            "missing Movies/: {:?}",
            names
        );
    }

    #[test]
    fn tar_zst_roundtrip_is_valid() {
        // Regression: the ZstEncoder was never finalized and the frame
        // ended up incomplete ("Truncated input file" from bsdtar).
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("dati");
        std::fs::create_dir(&src).unwrap();
        let big: Vec<u8> = (0..200_000u32)
            .map(|i| i.wrapping_mul(2654435761).wrapping_rem(251) as u8)
            .collect();
        std::fs::write(src.join("grosso.bin"), &big).unwrap();

        for fmt in [
            ArchiveFormat::TarZst,
            ArchiveFormat::TarGz,
            ArchiveFormat::Tar,
        ] {
            let ext = match fmt {
                ArchiveFormat::TarZst => "tar.zst",
                ArchiveFormat::TarGz => "tar.gz",
                _ => "tar",
            };
            let dest = dir.path().join(format!("a.{}", ext));
            backend()
                .create(&dest, std::slice::from_ref(&src), 6, None, None, None)
                .unwrap();
            let info = backend().list(&dest).unwrap();
            assert!(info.entries.iter().any(|e| e.path == "dati/grosso.bin"));
            let out = dir.path().join(format!("out-{}", ext));
            backend().extract(&dest, &out, None, None, None).unwrap();
            assert_eq!(std::fs::read(out.join("dati/grosso.bin")).unwrap(), big);
        }
    }

    #[test]
    fn secure_join_rejects_traversal() {
        let dest = Path::new("/tmp/dest");
        let base = resolved_base(dest);
        assert!(secure_join(dest, &base, "../../etc/passwd").is_none());
        assert!(secure_join(dest, &base, "a/../../x").is_none());
        assert!(secure_join(dest, &base, "").is_none());
        assert_eq!(
            secure_join(dest, &base, "a/b.txt").unwrap(),
            dest.join("a/b.txt")
        );
        // Normalized absolute paths stay inside dest (no zip-slip).
        assert_eq!(
            secure_join(dest, &base, "/abs.txt").unwrap(),
            dest.join("abs.txt")
        );
    }

    #[test]
    fn secure_join_rejects_symlink_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("dest");
        std::fs::create_dir(&dest).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        // Symlinked intermediate that points outside dest; the final target
        // does not exist yet (the case canonicalize() cannot guard).
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, dest.join("link")).unwrap();
        assert!(
            secure_join(&dest, &resolved_base(&dest), "link/newfile.txt").is_none(),
            "symlinked intermediate escaping dest must be rejected"
        );
        // Symlink inside dest is allowed.
        #[cfg(unix)]
        std::os::unix::fs::symlink("sub", dest.join("inner_link")).unwrap();
        std::fs::create_dir(dest.join("sub")).unwrap();
        assert_eq!(
            secure_join(&dest, &resolved_base(&dest), "inner_link/file.txt").unwrap(),
            dest.join("inner_link/file.txt")
        );
    }

    #[test]
    fn prefixed_name_never_absolute() {
        let base = Path::new("/tmp/base");
        assert_eq!(prefixed_name(Path::new("/tmp/base/a.txt"), base), "a.txt");
        assert_eq!(prefixed_name(Path::new("/altrove/b.txt"), base), "b.txt");
    }

    #[test]
    fn zstd_level_maps_full_range() {
        assert_eq!(zstd_level(0), 1);
        assert_eq!(zstd_level(5), 12);
        assert_eq!(zstd_level(9), 22);
        assert_eq!(zstd_level(99), 22);
        let mut prev = 0;
        for l in 0..=9u8 {
            let v = zstd_level(l);
            assert!(v >= prev, "non-monotonic levels");
            prev = v;
        }
    }

    #[test]
    fn zip_add_preserves_and_appends() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("a.zip");
        let src = dir.path().join("folder");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("old.txt"), b"old").unwrap();
        backend()
            .create(&zip_path, std::slice::from_ref(&src), 6, None, None, None)
            .unwrap();

        // Add a file into a subfolder of the archive.
        let new = dir.path().join("new.txt");
        std::fs::write(&new, b"new").unwrap();
        backend()
            .add(
                &zip_path,
                &[(new.clone(), "folder/new.txt".to_string())],
                None,
                None,
            )
            .unwrap();

        let info = backend().list(&zip_path).unwrap();
        let names: Vec<&str> = info.entries.iter().map(|e| e.path.as_str()).collect();
        assert!(names.contains(&"folder/old.txt"), "lost old: {:?}", names);
        assert!(
            names.contains(&"folder/new.txt"),
            "missing new: {:?}",
            names
        );

        // Extracted content must match on both entries.
        let out = dir.path().join("out");
        backend()
            .extract(&zip_path, &out, None, None, None)
            .unwrap();
        assert_eq!(std::fs::read(out.join("folder/old.txt")).unwrap(), b"old");
        assert_eq!(std::fs::read(out.join("folder/new.txt")).unwrap(), b"new");

        // No stale temp file left behind after the atomic swap.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {:?}", leftovers);
    }

    #[test]
    fn zip_add_root_entry_lands_in_archive_root() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("b.zip");
        let seed = dir.path().join("seed.txt");
        std::fs::write(&seed, b"seed").unwrap();
        backend()
            .create(&zip_path, std::slice::from_ref(&seed), 6, None, None, None)
            .unwrap();
        let a = dir.path().join("a.txt");
        std::fs::write(&a, b"root").unwrap();
        backend()
            .add(&zip_path, &[(a, "a.txt".to_string())], None, None)
            .unwrap();
        let info = backend().list(&zip_path).unwrap();
        let names: Vec<&str> = info.entries.iter().map(|e| e.path.as_str()).collect();
        assert!(names.contains(&"a.txt"), "missing root entry: {:?}", names);
    }

    #[test]
    fn zip_add_replaces_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("c.zip");
        let src = dir.path().join("doc.txt");
        std::fs::write(&src, b"old").unwrap();
        backend()
            .create(&zip_path, std::slice::from_ref(&src), 6, None, None, None)
            .unwrap();

        let new = dir.path().join("new.txt");
        std::fs::write(&new, b"new").unwrap();
        backend()
            .add(&zip_path, &[(new, "doc.txt".to_string())], None, None)
            .unwrap();

        let info = backend().list(&zip_path).unwrap();
        let names: Vec<&str> = info.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            names,
            vec!["doc.txt"],
            "duplicate not replaced: {:?}",
            names
        );
        let out = dir.path().join("out");
        backend()
            .extract(&zip_path, &out, None, None, None)
            .unwrap();
        assert_eq!(std::fs::read(out.join("doc.txt")).unwrap(), b"new");
    }

    #[test]
    fn zip_add_refuses_encrypted() {
        // Needs 7z to build an encrypted zip; skip if unavailable.
        let seven = crate::core::backends::seven_zip::SevenZipBackend::with_cancel(Arc::new(
            AtomicBool::new(false),
        ));
        if !seven.is_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("d.zip");
        let src = dir.path().join("x.txt");
        std::fs::write(&src, b"secret").unwrap();
        // Encrypted ZIP built directly with 7z (zip encrypts data, not headers).
        let st = std::process::Command::new("7z")
            .arg("a")
            .arg("-tzip")
            .arg("-ppw")
            .arg(&zip_path)
            .arg(&src)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "7z failed to build encrypted zip");

        let new = dir.path().join("y.txt");
        std::fs::write(&new, b"plain").unwrap();
        let err = backend()
            .add(&zip_path, &[(new, "y.txt".to_string())], None, None)
            .unwrap_err();
        assert!(
            err.to_string().contains("not supported"),
            "unexpected error: {}",
            err
        );
        assert!(zip_path.exists(), "failed add must not delete the archive");
    }

    #[test]
    fn zip_remove_deletes_entry_preserves_others() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("c.zip");
        let a = dir.path().join("keep.txt");
        let b = dir.path().join("drop.txt");
        std::fs::write(&a, b"keep").unwrap();
        std::fs::write(&b, b"drop").unwrap();
        backend()
            .create(&zip_path, &[a, b.clone()], 6, None, None, None)
            .unwrap();

        backend()
            .remove(&zip_path, &["drop.txt".to_string()], None, None)
            .unwrap();

        let info = backend().list(&zip_path).unwrap();
        let names: Vec<&str> = info.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(names, vec!["keep.txt"], "wrong entries: {:?}", names);
        let out = dir.path().join("out");
        backend()
            .extract(&zip_path, &out, None, None, None)
            .unwrap();
        assert_eq!(std::fs::read(out.join("keep.txt")).unwrap(), b"keep");
        assert!(!out.join("drop.txt").exists());
    }

    #[test]
    fn zip_remove_refuses_encrypted() {
        // Needs 7z to build an encrypted zip; skip if unavailable.
        let seven = crate::core::backends::seven_zip::SevenZipBackend::with_cancel(Arc::new(
            AtomicBool::new(false),
        ));
        if !seven.is_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("d.zip");
        let src = dir.path().join("x.txt");
        std::fs::write(&src, b"secret").unwrap();
        let st = std::process::Command::new("7z")
            .arg("a")
            .arg("-tzip")
            .arg("-ppw")
            .arg(&zip_path)
            .arg(&src)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "7z failed to build encrypted zip");

        let err = backend()
            .remove(&zip_path, &["x.txt".to_string()], None, None)
            .unwrap_err();
        assert!(
            err.to_string().contains("not supported"),
            "unexpected error: {}",
            err
        );
        assert!(
            zip_path.exists(),
            "failed remove must not delete the archive"
        );
    }

    #[test]
    fn secure_join_allows_symlinked_dest_and_rejects_escape() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        // zip-slip and empty names are always rejected.
        let real_base = resolved_base(&real);
        assert!(secure_join(&real, &real_base, "../escape").is_none());
        assert!(secure_join(&real, &real_base, "a/../../escape").is_none());
        assert!(secure_join(&real, &real_base, "").is_none());
        #[cfg(unix)]
        {
            // A symlinked destination must still accept regular entries (bug:
            // every entry was silently discarded).
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert!(secure_join(&link, &resolved_base(&link), "a/b.txt").is_some());
        }
    }

    #[test]
    fn resolve_lexically_collapses_dot_segments() {
        let base = Path::new("/d");
        assert_eq!(
            resolve_lexically(base, Path::new("a/../b")),
            PathBuf::from("/d/b")
        );
        assert_eq!(
            resolve_lexically(base, Path::new("../../x")),
            PathBuf::from("/x")
        );
    }

    #[cfg(unix)]
    #[test]
    fn tar_extract_strips_setuid_bits() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let tar_path = dir.path().join("a.tar");
        {
            let f = std::fs::File::create(&tar_path).unwrap();
            let mut b = tar::Builder::new(f);
            let mut h = tar::Header::new_gnu();
            h.set_size(1);
            h.set_mode(0o4755);
            h.set_mtime(1);
            h.set_entry_type(tar::EntryType::Regular);
            h.set_cksum();
            b.append_data(&mut h, "s.sh", &b"x"[..]).unwrap();
            b.finish().unwrap();
        }
        let out = dir.path().join("out");
        backend()
            .extract(&tar_path, &out, None, None, None)
            .unwrap();
        let mode = std::fs::metadata(out.join("s.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o7000, 0, "setuid/setgid leaked: {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn tar_extract_rejects_escaping_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let tar_path = dir.path().join("a.tar");
        {
            let f = std::fs::File::create(&tar_path).unwrap();
            let mut b = tar::Builder::new(f);
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_size(0);
            h.set_mode(0o777);
            h.set_mtime(1);
            h.set_link_name("/etc").unwrap();
            h.set_cksum();
            b.append_data(&mut h, "evil", &[][..]).unwrap();
            b.finish().unwrap();
        }
        let out = dir.path().join("out");
        backend()
            .extract(&tar_path, &out, None, None, None)
            .unwrap();
        assert!(!out.join("evil").exists(), "escaping symlink was created");
    }

    #[test]
    fn zip_rename_file_renames_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("r.zip");
        let seed = dir.path().join("seed.txt");
        std::fs::write(&seed, b"seed").unwrap();
        backend()
            .create(&zip_path, std::slice::from_ref(&seed), 6, None, None, None)
            .unwrap();
        let new_file = dir.path().join("new.txt");
        std::fs::write(&new_file, b"new").unwrap();
        backend()
            .add(
                &zip_path,
                &[(new_file.clone(), "folder/new.txt".to_string())],
                None,
                None,
            )
            .unwrap();

        backend()
            .rename(
                &zip_path,
                "folder/new.txt",
                "folder/renamed.txt",
                None,
                None,
            )
            .unwrap();

        let info = backend().list(&zip_path).unwrap();
        let names: Vec<&str> = info.entries.iter().map(|e| e.path.as_str()).collect();
        assert!(
            names.contains(&"folder/renamed.txt"),
            "renamed entry missing: {:?}",
            names
        );
        assert!(
            !names.contains(&"folder/new.txt"),
            "original entry still present: {:?}",
            names
        );
        let out = dir.path().join("out");
        backend()
            .extract(&zip_path, &out, None, None, None)
            .unwrap();
        assert_eq!(
            std::fs::read(out.join("folder/renamed.txt")).unwrap(),
            b"new"
        );
    }

    #[test]
    fn zip_rename_folder_rebases_children() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("rf.zip");
        let folder = dir.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("a.txt"), b"a").unwrap();
        std::fs::write(folder.join("b.txt"), b"b").unwrap();
        backend()
            .create(
                &zip_path,
                std::slice::from_ref(&folder),
                6,
                None,
                None,
                None,
            )
            .unwrap();

        backend()
            .rename(&zip_path, "folder/", "photos/", None, None)
            .unwrap();

        let info = backend().list(&zip_path).unwrap();
        let names: Vec<&str> = info.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            names.iter().filter(|n| n.starts_with("folder")).count(),
            0,
            "old folder still present: {:?}",
            names
        );
        assert!(
            names.contains(&"photos/a.txt") && names.contains(&"photos/b.txt"),
            "children not rebased: {:?}",
            names
        );
        let out = dir.path().join("out");
        backend()
            .extract(&zip_path, &out, None, None, None)
            .unwrap();
        assert_eq!(std::fs::read(out.join("photos/a.txt")).unwrap(), b"a");
        assert_eq!(std::fs::read(out.join("photos/b.txt")).unwrap(), b"b");
    }
}

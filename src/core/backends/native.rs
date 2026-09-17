use crate::core::archive::{ArchiveBackend, ArchiveEntry, ArchiveInfo, ProgressInfo};
use crate::core::detector::ArchiveFormat;
use crate::core::error::{ArkxError, Result};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Instant;

/// Zip-bomb guard: refuse to decompress more than this many bytes for a
/// single archive in the native backend (honest quota, not a ratio heuristic).
/// Extraction aborts with a clear error instead of filling the disk.
const MAX_EXTRACTED_BYTES: u64 = 16 * 1024 * 1024 * 1024; // 16 GiB

fn quota_error() -> ArkxError {
    ArkxError::Corrupted(format!(
        "decompressed data exceeds the {:.0} GiB safety quota; refusing to continue (possible zip bomb)",
        MAX_EXTRACTED_BYTES as f64 / (1024.0 * 1024.0 * 1024.0)
    ))
}

/// Final "Completed" progress: unknown total (0 bytes) reports a clean 100/100.
fn completed(total: u64) -> ProgressInfo {
    if total > 0 {
        ProgressInfo::new("Completed".to_string(), total, total)
    } else {
        ProgressInfo::new("Completed".to_string(), 100, 100)
    }
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
        self.extract_inner(archive, dest, entries, password, progress)
    }

    fn create(
        &self,
        dest: &Path,
        sources: &[PathBuf],
        level: u8,
        password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
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
        let reader = BufReader::with_capacity(1024 * 1024, file);
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

        let num_files = entries.iter().filter(|e| !e.is_dir).count();
        let num_dirs = entries.len() - num_files;

        Ok(ArchiveInfo {
            path: path.to_string_lossy().to_string(),
            format: "ZIP".into(),
            entries,
            total_size,
            total_packed,
            num_files,
            num_dirs,
            has_encrypted,
            comment: None,
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

        let num_files = entries.iter().filter(|e| !e.is_dir).count();
        let num_dirs = entries.len() - num_files;
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

    fn extract_inner(
        &self,
        archive: &Path,
        dest: &Path,
        entries: Option<&[String]>,
        _password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let fmt = crate::core::detector::detect_format(archive);
        std::fs::create_dir_all(dest).map_err(ArkxError::Io)?;

        match fmt {
            ArchiveFormat::Zip => self.extract_zip(archive, dest, entries, progress),
            ArchiveFormat::Tar
            | ArchiveFormat::TarGz
            | ArchiveFormat::TarBz2
            | ArchiveFormat::TarXz
            | ArchiveFormat::TarZst
            | ArchiveFormat::TarLz4 => self.extract_tar(archive, dest, entries, progress),
            ArchiveFormat::Gz
            | ArchiveFormat::Bz2
            | ArchiveFormat::Xz
            | ArchiveFormat::Zst
            | ArchiveFormat::Lz4 => self.extract_single(archive, dest, progress),
            _ => Err(ArkxError::UnsupportedFormat(format!("{:?}", fmt))),
        }
    }

    fn extract_zip(
        &self,
        archive: &Path,
        dest: &Path,
        filter: Option<&[String]>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let file = File::open(archive).map_err(ArkxError::Io)?;
        let reader = BufReader::with_capacity(1024 * 1024, file);
        let mut zip =
            zip::ZipArchive::new(reader).map_err(|e| ArkxError::Corrupted(e.to_string()))?;

        let filter = filter.map(|f| f.to_vec());

        // Real total bytes (may be 0 for empty archives: no fake max(1)).
        let total_bytes: u64 = match &filter {
            Some(sel) => {
                let mut sum = 0u64;
                for i in 0..zip.len() {
                    if let Ok(f) = zip.by_index(i) {
                        if sel
                            .iter()
                            .any(|s| crate::core::paths::entry_matches(f.name(), s))
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
        };

        if let Some(cb) = &progress {
            cb(ProgressInfo::new("Preparing…".to_string(), 0, total_bytes));
        }

        let mut processed_bytes = 0u64;
        // Reused across entries to avoid one 64KB allocation per file.
        let mut buf = vec![0u8; 65536];

        for i in 0..zip.len() {
            let mut f = zip
                .by_index(i)
                .map_err(|e| ArkxError::Corrupted(e.to_string()))?;
            let name = f.name().to_string();
            if let Some(ref sel) = filter {
                if !sel
                    .iter()
                    .any(|s| crate::core::paths::entry_matches(&name, s))
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
            let out_path = match secure_join(dest, &name) {
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
                    1024 * 1024,
                    File::create(&out_path).map_err(ArkxError::Io)?,
                );
                // 64KB chunks throttled to 100ms / 512KB so the UI is not spammed
                let mut last_emit = Instant::now();
                let mut last_bytes = processed_bytes;
                loop {
                    let n = f.read(&mut buf).map_err(ArkxError::Io)?;
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
            cb(completed(total_bytes));
        }
        Ok(())
    }

    fn extract_tar(
        &self,
        archive: &Path,
        dest: &Path,
        filter: Option<&[String]>,
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

        // Real total bytes (may be 0: no fake max(1)).
        let mut total_bytes = 0u64;
        if progress.is_some() {
            if let Ok(info) = self.list_tar(archive) {
                match &filter {
                    Some(sel) => {
                        for e in &info.entries {
                            if sel
                                .iter()
                                .any(|f| crate::core::paths::entry_matches(&e.path, f))
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
                cb(ProgressInfo::new("Preparing…".to_string(), 0, total_bytes));
            }
        }

        let mut processed_bytes = 0u64;
        // Reused across entries to avoid one 64KB allocation per file.
        let mut buf = vec![0u8; 65536];
        for entry in ar
            .entries()
            .map_err(|e| ArkxError::Corrupted(e.to_string()))?
        {
            let mut entry = entry.map_err(|e| ArkxError::Corrupted(e.to_string()))?;
            let path_raw = entry
                .path()
                .map_err(|e| ArkxError::Corrupted(e.to_string()))?
                .to_string_lossy()
                .to_string();
            let path_norm = crate::core::paths::normalize(&path_raw);
            // Skip non-matching entries when filtering
            if let Some(ref sel) = filter {
                if !sel
                    .iter()
                    .any(|f| crate::core::paths::entry_matches(&path_norm, f))
                {
                    continue;
                }
            }
            // Regular files: chunked copy with throttled progress (not per-file)
            let is_file = entry.header().entry_type().is_file();
            if is_file {
                let out_path = match secure_join(dest, &path_norm) {
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
                    1024 * 1024,
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
                let out_path = match secure_join(dest, &path_norm) {
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
            cb(completed(total_bytes));
        }
        Ok(())
    }

    fn extract_single(
        &self,
        archive: &Path,
        dest: &Path,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let file = File::open(archive).map_err(ArkxError::Io)?;
        let meta = std::fs::metadata(archive).ok();
        let total = meta.map(|m| m.len()).unwrap_or(0);
        let reader: Box<dyn std::io::Read> = create_single_reader(file, archive)?;
        let out_name = archive
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output");
        let out_path = dest.join(out_name);
        let mut out =
            BufWriter::with_capacity(1024 * 1024, File::create(&out_path).map_err(ArkxError::Io)?);
        if let Some(cb) = progress {
            cb(ProgressInfo::new(out_name.to_string(), 0, total));
            let mut reader = BufReader::with_capacity(1024 * 1024, reader);
            let mut buf = vec![0u8; 8192];
            let mut extracted: u64 = 0;
            loop {
                let n = reader.read(&mut buf).map_err(ArkxError::Io)?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n]).map_err(ArkxError::Io)?;
                extracted = extracted.saturating_add(n as u64);
                if total > 0 {
                    cb(ProgressInfo::new(
                        out_name.to_string(),
                        extracted.min(total),
                        total,
                    ));
                }
            }
            cb(completed(total));
            out.flush().map_err(ArkxError::Io)?;
        } else {
            let mut reader = BufReader::with_capacity(1024 * 1024, reader);
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
        _password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let fmt = crate::core::detector::detect_format(dest);
        match fmt {
            ArchiveFormat::Zip => self.create_zip(dest, sources, level, progress),
            ArchiveFormat::Tar
            | ArchiveFormat::TarGz
            | ArchiveFormat::TarBz2
            | ArchiveFormat::TarXz
            | ArchiveFormat::TarZst
            | ArchiveFormat::TarLz4 => self.create_tar(dest, sources, &fmt, level, progress),
            _ => Err(ArkxError::UnsupportedFormat(format!("create {:?}", fmt))),
        }
    }

    fn create_zip(
        &self,
        dest: &Path,
        sources: &[PathBuf],
        level: u8,
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
        let writer = BufWriter::with_capacity(1024 * 1024, file);
        let mut zip = zip::ZipWriter::new(writer);
        let options: zip::write::FileOptions<()> = zip::write::FileOptions::default()
            .compression_method(match level {
                0 => zip::CompressionMethod::Stored,
                1..=3 => zip::CompressionMethod::Deflated,
                _ => zip::CompressionMethod::Deflated,
            })
            .compression_level(Some(level as i64));

        let base = sources
            .first()
            .and_then(|p| p.parent())
            .unwrap_or(Path::new("."));
        // Dirs first (sorted, stable structure), then files: empty folders
        // are thus preserved as in tar.
        dirs.sort();
        let total = (dirs.len() + files.len()) as u64;
        let mut done = 0u64;

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
            done += 1;
            if let Some(cb) = &progress {
                cb(ProgressInfo::new(name.clone(), done, total.max(1)));
            }
            if is_dir {
                zip.add_directory(name, options)
                    .map_err(|e| ArkxError::Backend(e.to_string()))?;
                continue;
            }
            zip.start_file(name, options)
                .map_err(|e| ArkxError::Backend(e.to_string()))?;
            let mut f = File::open(path).map_err(ArkxError::Io)?;
            std::io::copy(&mut f, &mut zip).map_err(ArkxError::Io)?;
        }
        if let Some(cb) = &progress {
            cb(ProgressInfo::new(
                "Completed".to_string(),
                total.max(1),
                total.max(1),
            ));
        }
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
            let mut z = zip::ZipArchive::new(BufReader::with_capacity(1024 * 1024, file))
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

        let total = old.len() as u64 + new_items.len() as u64;
        let result: Result<()> = (|| {
            let file = File::create(&tmp).map_err(ArkxError::Io)?;
            let mut zip = zip::ZipWriter::new(BufWriter::with_capacity(1024 * 1024, file));
            let new_opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(6));
            let mut done = 0u64;

            for i in 0..old.len() {
                let mut f = old
                    .by_index(i)
                    .map_err(|e| ArkxError::Corrupted(e.to_string()))?;
                let name = f.name().to_string();
                if replace.contains(&name) {
                    continue;
                }
                done += 1;
                if let Some(cb) = &progress {
                    cb(ProgressInfo::new(name.clone(), done, total.max(1)));
                }
                let opts: zip::write::FileOptions<()> =
                    zip::write::FileOptions::default().compression_method(f.compression());
                if f.is_dir() {
                    zip.add_directory(name, opts)
                        .map_err(|e| ArkxError::Backend(e.to_string()))?;
                } else {
                    zip.start_file(name, opts)
                        .map_err(|e| ArkxError::Backend(e.to_string()))?;
                    std::io::copy(&mut f, &mut zip).map_err(ArkxError::Io)?;
                }
            }
            for (path, name) in &new_items {
                done += 1;
                if let Some(cb) = &progress {
                    cb(ProgressInfo::new(name.clone(), done, total.max(1)));
                }
                if path.is_dir() {
                    zip.add_directory(name, new_opts)
                        .map_err(|e| ArkxError::Backend(e.to_string()))?;
                } else {
                    zip.start_file(name, new_opts)
                        .map_err(|e| ArkxError::Backend(e.to_string()))?;
                    let mut f = File::open(path).map_err(ArkxError::Io)?;
                    std::io::copy(&mut f, &mut zip).map_err(ArkxError::Io)?;
                }
            }
            if let Some(cb) = &progress {
                cb(ProgressInfo::new(
                    "Completed".to_string(),
                    total.max(1),
                    total.max(1),
                ));
            }
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
        // Encrypted entries cannot be recompressed without the password.
        let refuse_encrypted = || -> Result<()> {
            Err(ArkxError::Backend(
                "removing from an encrypted archive is not supported".into(),
            ))
        };
        let mut old = {
            let file = File::open(archive).map_err(ArkxError::Io)?;
            let mut z = zip::ZipArchive::new(BufReader::with_capacity(1024 * 1024, file))
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

        let total = old.len() as u64;
        let result: Result<()> = (|| {
            let file = File::create(&tmp).map_err(ArkxError::Io)?;
            let mut zip = zip::ZipWriter::new(BufWriter::with_capacity(1024 * 1024, file));
            let mut done = 0u64;
            for i in 0..old.len() {
                let mut f = old
                    .by_index(i)
                    .map_err(|e| ArkxError::Corrupted(e.to_string()))?;
                let name = f.name().to_string();
                if entries
                    .iter()
                    .any(|s| crate::core::paths::entry_matches(&name, s))
                {
                    continue;
                }
                done += 1;
                if let Some(cb) = &progress {
                    cb(ProgressInfo::new(name.clone(), done, total.max(1)));
                }
                let opts: zip::write::FileOptions<()> =
                    zip::write::FileOptions::default().compression_method(f.compression());
                if f.is_dir() {
                    zip.add_directory(name, opts)
                        .map_err(|e| ArkxError::Backend(e.to_string()))?;
                } else {
                    zip.start_file(name, opts)
                        .map_err(|e| ArkxError::Backend(e.to_string()))?;
                    std::io::copy(&mut f, &mut zip).map_err(ArkxError::Io)?;
                }
            }
            if let Some(cb) = &progress {
                cb(ProgressInfo::new(
                    "Completed".to_string(),
                    total.max(1),
                    total.max(1),
                ));
            }
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
            cb(ProgressInfo::new(
                "Completed".to_string(),
                total.max(1),
                total.max(1),
            ));
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
}

fn create_tar_reader(file: File, path: &Path) -> Result<Box<dyn std::io::Read>> {
    let fmt = crate::core::detector::detect_format(path);
    let reader: Box<dyn std::io::Read> =
        match fmt {
            ArchiveFormat::TarGz => Box::new(flate2::read::GzDecoder::new(
                BufReader::with_capacity(1024 * 1024, file),
            )),
            ArchiveFormat::TarBz2 => Box::new(bzip2::read::BzDecoder::new(
                BufReader::with_capacity(1024 * 1024, file),
            )),
            ArchiveFormat::TarXz => Box::new(xz2::read::XzDecoder::new(BufReader::with_capacity(
                1024 * 1024,
                file,
            ))),
            ArchiveFormat::TarZst => Box::new(
                zstd::stream::read::Decoder::new(BufReader::with_capacity(1024 * 1024, file))
                    .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?,
            ),
            ArchiveFormat::TarLz4 => Box::new(lz4_flex::frame::FrameDecoder::new(
                BufReader::with_capacity(1024 * 1024, file),
            )),
            ArchiveFormat::Tar => Box::new(BufReader::with_capacity(1024 * 1024, file)),
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
            1024 * 1024,
            file,
        ))),
        ArchiveFormat::Bz2 => Box::new(bzip2::read::BzDecoder::new(BufReader::with_capacity(
            1024 * 1024,
            file,
        ))),
        ArchiveFormat::Xz => Box::new(xz2::read::XzDecoder::new(BufReader::with_capacity(
            1024 * 1024,
            file,
        ))),
        ArchiveFormat::Zst => Box::new(
            zstd::stream::read::Decoder::new(BufReader::with_capacity(1024 * 1024, file))
                .map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?,
        ),
        ArchiveFormat::Lz4 => Box::new(lz4_flex::frame::FrameDecoder::new(
            BufReader::with_capacity(1024 * 1024, file),
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
    let buf = BufWriter::with_capacity(1024 * 1024, file);
    // The user -l flag applies to all codecs (previously ignored:
    // fixed levels gz6/best/xz6/zst3). 0 = fast, 9 = max ratio.
    let writer = match fmt {
        ArchiveFormat::TarGz => TarWriter::Gz(flate2::write::GzEncoder::new(
            buf,
            flate2::Compression::new(level.clamp(0, 9) as u32),
        )),
        ArchiveFormat::TarBz2 => TarWriter::Bz(bzip2::write::BzEncoder::new(
            buf,
            bzip2::Compression::new(level.clamp(1, 9) as u32),
        )),
        ArchiveFormat::TarXz => {
            TarWriter::Xz(xz2::write::XzEncoder::new(buf, level.clamp(0, 9) as u32))
        }
        ArchiveFormat::TarZst => {
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
        ArchiveFormat::TarLz4 => TarWriter::Lz4(lz4_flex::frame::FrameEncoder::new(buf)),
        _ => TarWriter::Plain(buf),
    };
    Ok(writer)
}

/// Maps user level 0-9 onto the zstd 1-22 scale.
pub fn zstd_level(user: u8) -> i32 {
    const TABLE: [i32; 10] = [1, 3, 5, 7, 9, 12, 15, 17, 19, 22];
    TABLE[user.clamp(0, 9) as usize]
}

/// Safe join under `dest`: normalizes and rejects `..` (zip-slip from hostile
/// archives) and empty names. Returns `None` for entries to discard.
fn secure_join(dest: &Path, name: &str) -> Option<PathBuf> {
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
    // `dest` itself may be reached through a symlink (e.g. a symlinked
    // `~/Downloads`): compare against its resolved form, otherwise every entry
    // would be discarded and extraction would silently produce nothing.
    let base = std::fs::canonicalize(dest).unwrap_or_else(|_| absolutize(dest));
    let full = dest.join(&norm);
    // Reject if any component along the path is a symlink that resolves
    // outside dest (zip-slip via symlinks).
    match std::fs::canonicalize(&full) {
        Ok(canonical) => {
            if canonical.starts_with(&base) {
                Some(full)
            } else {
                None
            }
        }
        // File doesn't exist yet — check intermediate symlinks.
        Err(_) => {
            let mut current = base.clone();
            for component in Path::new(&norm).components() {
                current = current.join(component);
                // If this component exists, it must be under dest.
                if let Ok(meta) = std::fs::symlink_metadata(&current) {
                    if meta.file_type().is_symlink() {
                        // Resolve symlink and verify target is under dest.
                        match std::fs::canonicalize(&current) {
                            Ok(canonical) => {
                                if !canonical.starts_with(&base) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::archive::ArchiveBackend;
    use std::sync::{atomic::AtomicBool, Arc};

    fn backend() -> NativeBackend {
        NativeBackend::with_cancel(Arc::new(AtomicBool::new(false)))
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
            .create(&dest, std::slice::from_ref(&folder), 6, None, None)
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
            .create(&dest, std::slice::from_ref(&dl), 6, None, None)
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
                .create(&dest, std::slice::from_ref(&src), 6, None, None)
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
        assert!(secure_join(dest, "../../etc/passwd").is_none());
        assert!(secure_join(dest, "a/../../x").is_none());
        assert!(secure_join(dest, "").is_none());
        assert_eq!(secure_join(dest, "a/b.txt").unwrap(), dest.join("a/b.txt"));
        // Normalized absolute paths stay inside dest (no zip-slip).
        assert_eq!(secure_join(dest, "/abs.txt").unwrap(), dest.join("abs.txt"));
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
            secure_join(&dest, "link/newfile.txt").is_none(),
            "symlinked intermediate escaping dest must be rejected"
        );
        // Symlink inside dest is allowed.
        #[cfg(unix)]
        std::os::unix::fs::symlink("sub", dest.join("inner_link")).unwrap();
        std::fs::create_dir(dest.join("sub")).unwrap();
        assert_eq!(
            secure_join(&dest, "inner_link/file.txt").unwrap(),
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
            .create(&zip_path, std::slice::from_ref(&src), 6, None, None)
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
            .create(&zip_path, std::slice::from_ref(&seed), 6, None, None)
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
            .create(&zip_path, std::slice::from_ref(&src), 6, None, None)
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
            .create(&zip_path, &[a, b.clone()], 6, None, None)
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
        assert!(secure_join(&real, "../escape").is_none());
        assert!(secure_join(&real, "a/../../escape").is_none());
        assert!(secure_join(&real, "").is_none());
        #[cfg(unix)]
        {
            // A symlinked destination must still accept regular entries (bug:
            // every entry was silently discarded).
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert!(secure_join(&link, "a/b.txt").is_some());
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
}

use crate::core::archive::{ArchiveBackend, ArchiveEntry, ArchiveInfo, ProgressInfo};
use crate::core::detector::ArchiveFormat;
use crate::core::error::{ArkxError, Result};
use std::path::{Path, PathBuf};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::time::Instant;

pub struct NativeBackend;

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
}

impl NativeBackend {
    fn supports_format(fmt: &ArchiveFormat) -> bool {
        matches!(fmt, ArchiveFormat::Zip | ArchiveFormat::Tar | ArchiveFormat::TarGz | ArchiveFormat::TarBz2 | ArchiveFormat::TarXz | ArchiveFormat::TarZst | ArchiveFormat::Gz | ArchiveFormat::Bz2 | ArchiveFormat::Xz | ArchiveFormat::Zst)
    }

    fn list_inner(&self, path: &Path) -> Result<ArchiveInfo> {
        let fmt = crate::core::detector::detect_format(path);
        match fmt {
            ArchiveFormat::Zip => self.list_zip(path),
            ArchiveFormat::Tar | ArchiveFormat::TarGz | ArchiveFormat::TarBz2 | ArchiveFormat::TarXz | ArchiveFormat::TarZst => self.list_tar(path),
            ArchiveFormat::Gz | ArchiveFormat::Bz2 | ArchiveFormat::Xz | ArchiveFormat::Zst => self.list_single(path, &fmt),
            _ => Err(ArkxError::UnsupportedFormat(format!("{:?}", fmt))),
        }
    }

    fn list_zip(&self, path: &Path) -> Result<ArchiveInfo> {
        let file = File::open(path).map_err(ArkxError::Io)?;
        let reader = BufReader::with_capacity(1024 * 1024, file);
        let mut zip = zip::ZipArchive::new(reader).map_err(|e| ArkxError::Corrupted(e.to_string()))?;
        let mut entries = Vec::with_capacity(zip.len());
        let mut total_size = 0u64;
        let mut total_packed = 0u64;
        let mut has_encrypted = false;

        for i in 0..zip.len() {
            let f = zip.by_index(i).map_err(|e| ArkxError::Corrupted(e.to_string()))?;
            let is_dir = f.is_dir();
            let size = f.size();
            let comp_size = f.compressed_size();
            let name = f.name().to_string();
            let encrypted = f.encrypted();
            if encrypted { has_encrypted = true; }
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
        // Streaming tar list con decompressione parallela se necessario
        let file = File::open(path).map_err(ArkxError::Io)?;
        let reader: Box<dyn std::io::Read> = create_tar_reader(file, path)?;

        let mut ar = tar::Archive::new(reader);
        let mut entries = Vec::new();
        let mut total_size = 0u64;

        for entry in ar.entries().map_err(|e| ArkxError::Corrupted(e.to_string()))? {
            let entry = entry.map_err(|e| ArkxError::Corrupted(e.to_string()))?;
            let header = entry.header();
            let path_str = entry.path().map_err(|e| ArkxError::Corrupted(e.to_string()))?.to_string_lossy().to_string();
            let size = header.size().unwrap_or(0);
            let is_dir = header.entry_type().is_dir();
            total_size += size;
            entries.push(ArchiveEntry {
                path: path_str,
                is_dir,
                size,
                packed_size: size,
                modified: header.mtime().ok().and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0)).map(|dt| dt.with_timezone(&chrono::Local)),
                mode: header.mode().ok(),
                crc32: None,
                method: None,
                encrypted: false,
            });
        }

        let num_files = entries.iter().filter(|e| !e.is_dir).count();
        let num_dirs = entries.len() - num_files;
        let fmt_str = crate::core::detector::detect_format(path).display_name().to_string();

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
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file").to_string();
        let entry = ArchiveEntry {
            path: name.clone(),
            is_dir: false,
            size: 0,
            packed_size: meta.len(),
            modified: meta.modified().ok().map(|t| { let dt: chrono::DateTime<chrono::Local> = t.into(); dt }),
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
            ArchiveFormat::Tar | ArchiveFormat::TarGz | ArchiveFormat::TarBz2 | ArchiveFormat::TarXz | ArchiveFormat::TarZst => self.extract_tar(archive, dest, entries, progress),
            ArchiveFormat::Gz | ArchiveFormat::Bz2 | ArchiveFormat::Xz | ArchiveFormat::Zst => self.extract_single(archive, dest, progress),
            _ => Err(ArkxError::UnsupportedFormat(format!("{:?}", fmt))),
        }
    }

    fn extract_zip(&self, archive: &Path, dest: &Path, filter: Option<&[String]>, progress: Option<Box<dyn Fn(ProgressInfo) + Send>>) -> Result<()> {
        let file = File::open(archive).map_err(ArkxError::Io)?;
        let reader = BufReader::with_capacity(1024 * 1024, file);
        let mut zip = zip::ZipArchive::new(reader).map_err(|e| ArkxError::Corrupted(e.to_string()))?;

        let filter = filter.map(|f| f.to_vec());

        // Real total bytes (may be 0 for empty archives: no fake max(1)).
        let total_bytes: u64 = match &filter {
            Some(sel) => {
                let mut sum = 0u64;
                for i in 0..zip.len() {
                    if let Ok(f) = zip.by_index(i) {
                        if sel.iter().any(|s| crate::core::paths::entry_matches(f.name(), s)) {
                            sum = sum.saturating_add(f.size());
                        }
                    }
                }
                sum
            }
            None => {
                let mut sum = 0u64;
                for i in 0..zip.len() {
                    if let Ok(f) = zip.by_index(i) { sum = sum.saturating_add(f.size()); }
                }
                sum
            }
        };

        if let Some(cb) = &progress {
            cb(ProgressInfo::new("Preparing…".to_string(), 0, total_bytes));
        }

        let mut processed_bytes = 0u64;

        for i in 0..zip.len() {
            let mut f = zip.by_index(i).map_err(|e| ArkxError::Corrupted(e.to_string()))?;
            let name = f.name().to_string();
            if let Some(ref sel) = filter {
                if !sel.iter().any(|s| crate::core::paths::entry_matches(&name, s)) {
                    continue;
                }
            }
            let out_path = dest.join(&name);
            if f.is_dir() {
                std::fs::create_dir_all(&out_path).map_err(ArkxError::Io)?;
                if let Some(cb) = &progress {
                    cb(ProgressInfo::new(name.clone(), processed_bytes.min(total_bytes), total_bytes));
                }
            } else {
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent).map_err(ArkxError::Io)?;
                }
                let mut out = BufWriter::with_capacity(1024 * 1024, File::create(&out_path).map_err(ArkxError::Io)?);
                // 64KB chunks throttled to 100ms / 512KB so the UI is not spammed
                let mut buf = vec![0u8; 65536];
                let mut last_emit = Instant::now();
                let mut last_bytes = processed_bytes;
                loop {
                    let n = f.read(&mut buf).map_err(ArkxError::Io)?;
                    if n == 0 { break; }
                    out.write_all(&buf[..n]).map_err(ArkxError::Io)?;
                    processed_bytes = processed_bytes.saturating_add(n as u64);
                    // Throttle: emit every 100ms, every 512KB, or on file completion
                    let elapsed = last_emit.elapsed().as_millis() > 100 || processed_bytes - last_bytes >= 524288;
                    if elapsed {
                        if let Some(cb) = &progress {
                            cb(ProgressInfo::new(name.clone(), processed_bytes.min(total_bytes), total_bytes));
                        }
                        last_emit = Instant::now();
                        last_bytes = processed_bytes;
                    }
                }
                // preserve permissions
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Some(mode) = f.unix_mode() {
                        let _ = std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(mode));
                    }
                }
                // Final update per file (even if throttled away)
                if let Some(cb) = &progress {
                    cb(ProgressInfo::new(name.clone(), processed_bytes.min(total_bytes), total_bytes));
                }
            }
        }
        if total_bytes > 0 {
            if let Some(cb) = &progress {
                cb(ProgressInfo::new("Completed".to_string(), total_bytes, total_bytes));
            }
        } else if let Some(cb) = &progress {
            // Empty / zero-size archive: 0% during, 100% at the end.
            cb(ProgressInfo::new("Completed".to_string(), 100, 100));
        }
        Ok(())
    }

    fn extract_tar(&self, archive: &Path, dest: &Path, filter: Option<&[String]>, progress: Option<Box<dyn Fn(ProgressInfo) + Send>>) -> Result<()> {
        let file = File::open(archive).map_err(ArkxError::Io)?;
        let reader = create_tar_reader(file, archive)?;
        let mut ar = tar::Archive::new(reader);
        ar.set_preserve_permissions(true);
        ar.set_preserve_mtime(true);

        let filter = filter.map(|f| f.to_vec());

        // Real total bytes (may be 0: no fake max(1)).
        let mut total_bytes = 0u64;
        if progress.is_some() {
            if let Ok(info) = self.list_tar(archive) {
                match &filter {
                    Some(sel) => {
                        for e in &info.entries {
                            if sel.iter().any(|f| crate::core::paths::entry_matches(&e.path, f)) {
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
        for entry in ar.entries().map_err(|e| ArkxError::Corrupted(e.to_string()))? {
            let mut entry = entry.map_err(|e| ArkxError::Corrupted(e.to_string()))?;
            let path_raw = entry.path().map_err(|e| ArkxError::Corrupted(e.to_string()))?.to_string_lossy().to_string();
            let path_norm = crate::core::paths::normalize(&path_raw);
            // Skip non-matching entries when filtering
            if let Some(ref sel) = filter {
                if !sel.iter().any(|f| crate::core::paths::entry_matches(&path_norm, f)) {
                    continue;
                }
            }
            // Regular files: chunked copy with throttled progress (not per-file)
            let is_file = entry.header().entry_type().is_file();
            if is_file {
                let out_path = dest.join(&path_norm);
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent).map_err(ArkxError::Io)?;
                }
                let mut out = BufWriter::with_capacity(1024 * 1024, File::create(&out_path).map_err(ArkxError::Io)?);
                let mut buf = vec![0u8; 65536];
                let mut last_emit = Instant::now();
                let mut last_bytes = processed_bytes;
                loop {
                    let n = entry.read(&mut buf).map_err(ArkxError::Io)?;
                    if n == 0 { break; }
                    out.write_all(&buf[..n]).map_err(ArkxError::Io)?;
                    processed_bytes = processed_bytes.saturating_add(n as u64);
                    let elapsed = last_emit.elapsed().as_millis() > 100 || processed_bytes - last_bytes >= 524288;
                    if elapsed {
                        if let Some(cb) = &progress {
                            cb(ProgressInfo::new(path_raw.clone(), processed_bytes.min(total_bytes), total_bytes));
                        }
                        last_emit = Instant::now();
                        last_bytes = processed_bytes;
                    }
                }
                // preserve permissions/mtime
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(mode) = entry.header().mode() {
                        let _ = std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(mode));
                    }
                }
                // Final update per file
                if let Some(cb) = &progress {
                    cb(ProgressInfo::new(path_raw.clone(), processed_bytes.min(total_bytes), total_bytes));
                }
            } else {
                // Directories, symlinks, others: standard unpack_in
                entry.unpack_in(dest).map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
                if let Some(cb) = &progress {
                    cb(ProgressInfo::new(path_raw.clone(), processed_bytes.min(total_bytes), total_bytes));
                }
            }
        }
        // Final 100% (even when total is 0).
        if let Some(cb) = &progress {
            if total_bytes > 0 {
                cb(ProgressInfo::new("Completed".to_string(), total_bytes, total_bytes));
            } else {
                cb(ProgressInfo::new("Completed".to_string(), 100, 100));
            }
        }
        Ok(())
    }

    fn extract_single(&self, archive: &Path, dest: &Path, progress: Option<Box<dyn Fn(ProgressInfo) + Send>>) -> Result<()> {
        let file = File::open(archive).map_err(ArkxError::Io)?;
        let meta = std::fs::metadata(archive).ok();
        let total = meta.map(|m| m.len()).unwrap_or(0);
        let reader: Box<dyn std::io::Read> = create_single_reader(file, archive)?;
        let out_name = archive.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
        let out_path = dest.join(out_name);
        let mut out = BufWriter::with_capacity(1024 * 1024, File::create(&out_path).map_err(ArkxError::Io)?);
        if let Some(cb) = progress {
            cb(ProgressInfo::new(out_name.to_string(), 0, total));
            let mut reader = BufReader::with_capacity(1024 * 1024, reader);
            let mut buf = vec![0u8; 8192];
            let mut extracted: u64 = 0;
            loop {
                let n = reader.read(&mut buf).map_err(ArkxError::Io)?;
                if n == 0 { break; }
                out.write_all(&buf[..n]).map_err(ArkxError::Io)?;
                extracted = extracted.saturating_add(n as u64);
                if total > 0 {
                    cb(ProgressInfo::new(out_name.to_string(), extracted.min(total), total));
                }
            }
            if total > 0 {
                cb(ProgressInfo::new("Completed".to_string(), total, total));
            } else {
                cb(ProgressInfo::new("Completed".to_string(), 100, 100));
            }
        } else {
            let mut reader = BufReader::with_capacity(1024 * 1024, reader);
            std::io::copy(&mut reader, &mut out).map_err(ArkxError::Io)?;
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
            ArchiveFormat::Tar | ArchiveFormat::TarGz | ArchiveFormat::TarBz2 | ArchiveFormat::TarXz | ArchiveFormat::TarZst => self.create_tar(dest, sources, &fmt, level, progress),
            _ => Err(ArkxError::UnsupportedFormat(format!("create {:?}", fmt))),
        }
    }

    fn create_zip(&self, dest: &Path, sources: &[PathBuf], level: u8, progress: Option<Box<dyn Fn(ProgressInfo) + Send>>) -> Result<()> {
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

        // Collect files recursively with walkdir
        let mut files: Vec<PathBuf> = Vec::new();
        for src in sources {
            if src.is_dir() {
                for entry in walkdir::WalkDir::new(src).into_iter().filter_map(|e| e.ok()) {
                    if entry.file_type().is_file() {
                        files.push(entry.path().to_path_buf());
                    }
                }
            } else {
                files.push(src.clone());
            }
        }

        let base = sources.first().and_then(|p| p.parent()).unwrap_or(Path::new("."));
        let total = files.len() as u64;

        for (i, path) in files.iter().enumerate() {
            let rel = path.strip_prefix(base).unwrap_or(path);
            let name = rel.to_string_lossy().to_string();
            if let Some(cb) = &progress {
                cb(ProgressInfo::new(name.clone(), i as u64 + 1, total.max(1)));
            }
            zip.start_file(name, options).map_err(|e| ArkxError::Backend(e.to_string()))?;
            let mut f = File::open(path).map_err(ArkxError::Io)?;
            std::io::copy(&mut f, &mut zip).map_err(ArkxError::Io)?;
        }
        if let Some(cb) = &progress {
            cb(ProgressInfo::new("Completed".to_string(), total.max(1), total.max(1)));
        }
        zip.finish().map_err(|e| ArkxError::Backend(e.to_string()))?;
        Ok(())
    }

    fn create_tar(&self, dest: &Path, sources: &[PathBuf], fmt: &ArchiveFormat, _level: u8, progress: Option<Box<dyn Fn(ProgressInfo) + Send>>) -> Result<()> {
        let file = File::create(dest).map_err(ArkxError::Io)?;
        let writer: Box<dyn std::io::Write> = create_tar_writer(file, fmt)?;
        let mut tar = tar::Builder::new(writer);

        let mut files: Vec<PathBuf> = Vec::new();
        for src in sources {
            if src.is_dir() {
                for entry in walkdir::WalkDir::new(src).into_iter().filter_map(|e| e.ok()) {
                    files.push(entry.path().to_path_buf());
                }
            } else {
                files.push(src.clone());
            }
        }

        let total = files.len() as u64;
        // Single source directory: keep its parent as base so the folder
        // name itself lands inside the tar
        let base = if sources.len() == 1 && sources[0].is_dir() {
            sources[0].parent().unwrap_or(Path::new(".")).to_path_buf()
        } else {
            sources.first().and_then(|p| p.parent()).map(|p| p.to_path_buf()).unwrap_or(PathBuf::from("."))
        };

        for (i, path) in files.iter().enumerate() {
            let rel = path.strip_prefix(&base).unwrap_or(path);
            if let Some(cb) = &progress {
                cb(ProgressInfo::new(rel.to_string_lossy().to_string(), i as u64 + 1, total.max(1)));
            }
            if path.is_dir() {
                tar.append_dir(rel, path).map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
            } else {
                tar.append_path_with_name(path, rel).map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
            }
        }
        if let Some(cb) = &progress {
            cb(ProgressInfo::new("Completed".to_string(), total.max(1), total.max(1)));
        }
        tar.finish().map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?;
        Ok(())
    }
}

fn create_tar_reader(file: File, path: &Path) -> Result<Box<dyn std::io::Read>> {
    let fmt = crate::core::detector::detect_format(path);
    let reader: Box<dyn std::io::Read> = match fmt {
        ArchiveFormat::TarGz => Box::new(flate2::read::GzDecoder::new(BufReader::with_capacity(1024*1024, file))),
        ArchiveFormat::TarBz2 => Box::new(bzip2::read::BzDecoder::new(BufReader::with_capacity(1024*1024, file))),
        ArchiveFormat::TarXz => Box::new(xz2::read::XzDecoder::new(BufReader::with_capacity(1024*1024, file))),
        ArchiveFormat::TarZst => Box::new(zstd::stream::read::Decoder::new(BufReader::with_capacity(1024*1024, file)).map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?),
        ArchiveFormat::Tar | ArchiveFormat::TarLz4 => Box::new(BufReader::with_capacity(1024*1024, file)),
        _ => Box::new(BufReader::with_capacity(1024*1024, file)),
    };
    Ok(reader)
}

fn create_single_reader(file: File, path: &Path) -> Result<Box<dyn std::io::Read>> {
    let fmt = crate::core::detector::detect_format(path);
    let reader: Box<dyn std::io::Read> = match fmt {
        ArchiveFormat::Gz => Box::new(flate2::read::GzDecoder::new(BufReader::with_capacity(1024*1024, file))),
        ArchiveFormat::Bz2 => Box::new(bzip2::read::BzDecoder::new(BufReader::with_capacity(1024*1024, file))),
        ArchiveFormat::Xz => Box::new(xz2::read::XzDecoder::new(BufReader::with_capacity(1024*1024, file))),
        ArchiveFormat::Zst => Box::new(zstd::stream::read::Decoder::new(BufReader::with_capacity(1024*1024, file)).map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?),
        _ => Box::new(BufReader::with_capacity(1024*1024, file)),
    };
    Ok(reader)
}

fn create_tar_writer(file: File, fmt: &ArchiveFormat) -> Result<Box<dyn std::io::Write>> {
    let writer: Box<dyn std::io::Write> = match fmt {
        ArchiveFormat::TarGz => Box::new(flate2::write::GzEncoder::new(BufWriter::with_capacity(1024*1024, file), flate2::Compression::new(6))),
        ArchiveFormat::TarBz2 => Box::new(bzip2::write::BzEncoder::new(BufWriter::with_capacity(1024*1024, file), bzip2::Compression::best())),
        ArchiveFormat::TarXz => Box::new(xz2::write::XzEncoder::new(BufWriter::with_capacity(1024*1024, file), 6)),
        ArchiveFormat::TarZst => Box::new(zstd::stream::write::Encoder::new(BufWriter::with_capacity(1024*1024, file), 3).map_err(|e| ArkxError::Io(std::io::Error::other(e.to_string())))?),
        _ => Box::new(BufWriter::with_capacity(1024*1024, file)),
    };
    Ok(writer)
}

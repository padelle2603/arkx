pub mod bsdtar;
pub mod native;
pub mod seven_zip;

use super::archive::{ArchiveBackend, ArchiveInfo, ProgressInfo};
use super::detector::{ArchiveFormat, BackendKind};
use super::error::{ArkxError, Result};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

pub struct BackendManager {
    native: native::NativeBackend,
    seven: seven_zip::SevenZipBackend,
    bsdtar: bsdtar::BsdtarBackend,
    cancel: Arc<AtomicBool>,
}

impl BackendManager {
    pub fn new() -> Self {
        Self::with_cancel(Arc::new(AtomicBool::new(false)))
    }

    /// Share an external cancel flag (file-manager progress window):
    /// `cancel_all()` then aborts running `create` jobs and deletes partials.
    pub fn with_cancel(cancel: Arc<AtomicBool>) -> Self {
        Self {
            native: native::NativeBackend::with_cancel(cancel.clone()),
            seven: seven_zip::SevenZipBackend::with_cancel(cancel.clone()),
            bsdtar: bsdtar::BsdtarBackend::new(),
            cancel,
        }
    }

    /// Best-effort abort of a running `create` (the progress window's Cancel).
    pub fn cancel_all(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Primary backend from the format table (`detector::BackendKind`).
    fn primary(&self, fmt: &ArchiveFormat) -> &dyn ArchiveBackend {
        match fmt.backend() {
            BackendKind::Native => &self.native,
            BackendKind::SevenZip => &self.seven,
            BackendKind::Libarchive => &self.bsdtar,
        }
    }

    pub fn detect_and_list(&self, path: &Path) -> Result<ArchiveInfo> {
        let fmt = super::detector::detect_format(path);
        match self.primary(&fmt).list(path) {
            Ok(info) => Ok(info),
            Err(first) => {
                // Cross-fallback chain: native -> 7z -> libarchive.
                // (Exotic tar.* fail on native/7z but open with bsdtar;
                // encrypted ZIPs fail on native but open with 7z.)
                if matches!(fmt.backend(), BackendKind::Native) {
                    eprintln!(
                        "[core] native list failed for {:?}: {}, falling back to 7z",
                        fmt, first
                    );
                    match self.seven.list(path) {
                        Ok(info) => Ok(info),
                        Err(second) => {
                            eprintln!(
                                "[core] 7z list failed for {:?}: {}, falling back to bsdtar",
                                fmt, second
                            );
                            self.bsdtar.list(path)
                        }
                    }
                } else if matches!(fmt.backend(), BackendKind::SevenZip) {
                    if matches!(&first, ArkxError::WrongPassword) {
                        eprintln!(
                            "[core] 7z list failed (encrypted headers), no bsdtar fallback: {}",
                            first
                        );
                        return Err(first);
                    }
                    eprintln!(
                        "[core] 7z list failed for {:?}: {}, falling back to bsdtar",
                        fmt, first
                    );
                    self.bsdtar.list(path)
                } else {
                    eprintln!(
                        "[core] bsdtar list failed for {:?}: {}, falling back to 7z",
                        fmt, first
                    );
                    match self.seven.list(path) {
                        Ok(info) => Ok(info),
                        Err(_) => Err(first),
                    }
                }
            }
        }
    }

    /// List a header-encrypted archive (7z/RAR) with its password. Listing is
    /// 7z-only: no fallback chain, a wrong password must surface directly so
    /// the caller re-prompts.
    pub fn list_with_password(&self, path: &Path, password: &str) -> Result<ArchiveInfo> {
        if !self.seven.is_available() {
            return Err(ArkxError::Backend(
                "cannot unlock header-encrypted archive: 7z is not installed".into(),
            ));
        }
        self.seven.list_with_password(path, password)
    }

    pub fn extract(
        &self,
        archive: &Path,
        dest: &Path,
        entries: Option<&[String]>,
        password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let fmt = super::detector::detect_format(archive);
        // Large zips → multithreaded 7z even if the table says native:
        // zero-fork only pays off below threshold (adaptive on RAM).
        let big_zip = fmt == ArchiveFormat::Zip
            && std::fs::metadata(archive)
                .map(|m| m.len() >= crate::core::util::big_archive_threshold_bytes())
                .unwrap_or(false);
        if big_zip {
            match self
                .seven
                .extract(archive, dest, entries, password, progress)
            {
                Ok(()) => return Ok(()),
                Err(e) => {
                    eprintln!(
                        "[core] 7z extract failed for big zip, falling back to native: {}",
                        e
                    );
                    return self.native.extract(archive, dest, entries, password, None);
                }
            }
        }
        // NOTE: progress callbacks are single-shot (Fn, not clonable):
        // only the backend that actually runs receives it; fallbacks
        // re-run without progress.
        match self
            .primary(&fmt)
            .extract(archive, dest, entries, password, progress)
        {
            Ok(()) => Ok(()),
            Err(e) => {
                if matches!(fmt.backend(), BackendKind::Native) {
                    eprintln!("[core] native extract failed, falling back to 7z: {}", e);
                    match self.seven.extract(archive, dest, entries, password, None) {
                        Ok(()) => Ok(()),
                        Err(second) => {
                            if matches!(&second, ArkxError::WrongPassword)
                                || seven_zip::is_missing_volume_error(&second)
                            {
                                eprintln!(
                                    "[core] 7z extract failed (encrypted/multi-volume), no bsdtar fallback: {}",
                                    second
                                );
                                return Err(second);
                            }
                            eprintln!(
                                "[core] 7z extract failed, falling back to bsdtar: {}",
                                second
                            );
                            self.bsdtar.extract(archive, dest, entries, None, None)
                        }
                    }
                } else if matches!(fmt.backend(), BackendKind::SevenZip) {
                    if matches!(&e, ArkxError::WrongPassword)
                        || seven_zip::is_missing_volume_error(&e)
                    {
                        eprintln!(
                            "[core] 7z extract failed (encrypted/multi-volume), no bsdtar fallback: {}",
                            e
                        );
                        return Err(e);
                    }
                    eprintln!("[core] 7z extract failed, falling back to bsdtar: {}", e);
                    self.bsdtar.extract(archive, dest, entries, None, None)
                } else {
                    eprintln!("[core] bsdtar extract failed, falling back to 7z: {}", e);
                    match self.seven.extract(archive, dest, entries, password, None) {
                        Ok(()) => Ok(()),
                        // Prefer a password/multi-volume diagnosis over the
                        // generic libarchive error so the UI can prompt.
                        Err(second)
                            if matches!(&second, ArkxError::WrongPassword)
                                || seven_zip::is_missing_volume_error(&second) =>
                        {
                            Err(second)
                        }
                        Err(_) => Err(e),
                    }
                }
            }
        }
    }

    pub fn create(
        &self,
        dest: &Path,
        sources: &[PathBuf],
        level: u8,
        password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let fmt = super::detector::detect_format(dest);
        if matches!(fmt.backend(), BackendKind::Libarchive) {
            // lzip/lzo/lrzip tar flavors: creation needs rare external
            // encoders; keep them extract-only with a clear message.
            return Err(ArkxError::UnsupportedFormat(format!(
                "cannot create {:?} (extract-only; use .tar.gz/.tar.xz/.tar.zst instead)",
                fmt
            )));
        }
        // Application bundles (AppImage) and these rare formats cannot be
        // written at all: refuse up-front instead of a cryptic 7z error.
        if matches!(
            fmt,
            ArchiveFormat::AppImage
                | ArchiveFormat::Arj
                | ArchiveFormat::Lzh
                | ArchiveFormat::Iso
                | ArchiveFormat::Cab
                | ArchiveFormat::Deb
                | ArchiveFormat::Rpm
                | ArchiveFormat::Cpio
                | ArchiveFormat::Xar
                | ArchiveFormat::Ar
        ) {
            return Err(ArkxError::UnsupportedFormat(format!(
                "cannot create {fmt:?}: extract-only format (no writer backend)"
            )));
        }
        // Disk-space preflight: a 68GB failing halfway with ENOSPC after
        // hours is worse than an immediate error. Worst-case estimate (total input,
        // incompressible data); if `df` does not respond the check is skipped.
        let total = crate::core::util::total_input_size(sources);
        if total > 0 {
            if let Some(free) = crate::core::util::filesystem_free_bytes(dest) {
                if total > free {
                    return Err(ArkxError::Backend(format!(
                        "not enough disk space for {}: need {} free, have {} on {}",
                        dest.display(),
                        humansize::format_size(total, humansize::BINARY),
                        humansize::format_size(free, humansize::BINARY),
                        dest.parent().unwrap_or(std::path::Path::new(".")).display()
                    )));
                }
            }
        }
        if self.native.supports(&fmt) && password.is_none() {
            // Large zips → multithreaded 7z (-mmt) with fallback to native:
            // above threshold (adaptive on RAM) parallel beats zero-fork.
            if matches!(fmt, ArchiveFormat::Zip)
                && self.seven.is_available()
                && total >= crate::core::util::zip_seven_threshold_bytes()
            {
                eprintln!(
                    "[core] big zip ({} threads): using 7z",
                    crate::core::util::effective_threads()
                );
                match self.seven.create(dest, sources, level, password, progress) {
                    Ok(()) => return Ok(()),
                    Err(e) => {
                        eprintln!("[core] 7z create failed, falling back to native: {}", e);
                        return self.native.create(dest, sources, level, None, None);
                    }
                }
            }
            // Native has no password support yet.
            self.native.create(dest, sources, level, password, progress)
        } else {
            self.seven.create(dest, sources, level, password, progress)
        }
    }

    /// Add files to an existing archive. Routing:
    /// Zip→native rewrite; 7z and plain tar→`7z a` (update mode); everything else
    /// is extract-only. Stream-compressed tar flavors (tar.gz/.xz/…) cannot be
    /// updated in place and fail clearly instead of corrupting the archive. No
    /// fallback chain: a failed update must reach the user as an error.
    pub fn add(
        &self,
        archive: &Path,
        sources: &[(PathBuf, String)],
        password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let fmt = super::detector::detect_format(archive);
        match fmt {
            ArchiveFormat::Zip => self.native.add(archive, sources, password, progress),
            ArchiveFormat::SevenZip | ArchiveFormat::Tar => {
                self.seven.add(archive, sources, password, progress)
            }
            _ => Err(ArkxError::UnsupportedFormat(format!(
                "cannot add to {fmt:?}: stream-compressed formats (tar.gz/tar.xz/…) are add-only if you re-create them; use .tar/.zip/.7z for updates"
            ))),
        }
    }

    /// Create an empty folder entry inside an existing archive. Routes through
    /// `add` (zip native rewrite / 7z update) from a temporary empty directory,
    /// which the backends record as a bare directory entry; the temp dir is
    /// cleaned up on every path.
    pub fn new_folder(&self, archive: &Path, name: &str, password: Option<&str>) -> Result<()> {
        // Entry is a full archive path (trailing slash): each component must be a
        // clean, non-empty name. Rejects "..", "." and double slashes (zip-slip
        // and traversal guard, same rules the extractor enforces).
        let components: Vec<&str> = name.trim_end_matches('/').split('/').collect();
        if components.is_empty()
            || components
                .iter()
                .any(|c| c.is_empty() || *c == "." || *c == ".." || c.contains('\\'))
        {
            return Err(ArkxError::InvalidInput(name.to_string()));
        }
        let nano = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let tmp =
            std::env::temp_dir().join(format!("arkx-newdir-{}-{:09}", std::process::id(), nano));
        std::fs::create_dir_all(&tmp).map_err(ArkxError::Io)?;
        let result = self.add(archive, &[(tmp.clone(), name.to_string())], password, None);
        let _ = std::fs::remove_dir_all(&tmp);
        result
    }

    /// Remove entries from an existing archive. Same routing as `add`.
    pub fn remove(
        &self,
        archive: &Path,
        entries: &[String],
        password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let fmt = super::detector::detect_format(archive);
        match fmt {
            ArchiveFormat::Zip => self.native.remove(archive, entries, password, progress),
            ArchiveFormat::SevenZip | ArchiveFormat::Tar => {
                self.seven.remove(archive, entries, password, progress)
            }
            _ => Err(ArkxError::UnsupportedFormat(format!(
                "cannot remove from {fmt:?}: stream-compressed formats (tar.gz/tar.xz/…) are extract-only; use .tar/.zip/.7z for updates"
            ))),
        }
    }

    /// Rename an entry inside an archive. Routing:
    /// Zip→native rewrite; 7z and plain tar→`7z rn`.
    pub fn rename(
        &self,
        archive: &Path,
        old_name: &str,
        new_name: &str,
        password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let fmt = super::detector::detect_format(archive);
        match fmt {
            ArchiveFormat::Zip => self
                .native
                .rename(archive, old_name, new_name, password, progress),
            ArchiveFormat::SevenZip | ArchiveFormat::Tar => self
                .seven
                .rename(archive, old_name, new_name, password, progress),
            _ => Err(ArkxError::UnsupportedFormat(format!(
                "cannot rename in {fmt:?}: stream-compressed formats are not updatable"
            ))),
        }
    }

    /// Test integrity of archive entries.
    pub fn test(
        &self,
        archive: &Path,
        entries: Option<&[String]>,
        password: Option<&str>,
    ) -> Result<crate::core::archive::TestReport> {
        let fmt = super::detector::detect_format(archive);
        match fmt {
            ArchiveFormat::Zip => self.native.test(archive, entries, password),
            ArchiveFormat::SevenZip | ArchiveFormat::Tar => {
                self.seven.test(archive, entries, password)
            }
            _ => Err(ArkxError::UnsupportedFormat(format!(
                "cannot test {fmt:?}: integrity check not supported"
            ))),
        }
    }

    /// Extract a single entry to a temp dir for external app launch.
    pub fn open_with(
        &self,
        archive: &Path,
        entry: &str,
        password: Option<&str>,
        temp_dir: &Path,
    ) -> Result<PathBuf> {
        let fmt = super::detector::detect_format(archive);
        match fmt {
            ArchiveFormat::Zip => self.native.open_with(archive, entry, password, temp_dir),
            ArchiveFormat::SevenZip | ArchiveFormat::Tar => {
                self.seven.open_with(archive, entry, password, temp_dir)
            }
            _ => Err(ArkxError::UnsupportedFormat(format!(
                "cannot open entries of {fmt:?} with an external app"
            ))),
        }
    }

    /// Get archive/entry properties.
    pub fn properties(
        &self,
        archive: &Path,
        entry: Option<&str>,
        password: Option<&str>,
    ) -> Result<crate::core::archive::ArchiveProperties> {
        let fmt = super::detector::detect_format(archive);
        match fmt {
            ArchiveFormat::Zip => self.native.properties(archive, entry, password),
            _ => Err(ArkxError::UnsupportedFormat(format!(
                "cannot get properties of {fmt:?}"
            ))),
        }
    }

    /// Securely delete entries by overwriting them before removal.
    pub fn secure_delete(
        &self,
        archive: &Path,
        entries: &[String],
        passes: usize,
        password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let fmt = super::detector::detect_format(archive);
        match fmt {
            ArchiveFormat::Zip => self
                .native
                .secure_delete(archive, entries, passes, password, progress),
            ArchiveFormat::SevenZip | ArchiveFormat::Tar => self
                .seven
                .secure_delete(archive, entries, passes, password, progress),
            _ => Err(ArkxError::UnsupportedFormat(format!(
                "cannot secure-delete from {fmt:?}"
            ))),
        }
    }
}

impl Default for BackendManager {
    fn default() -> Self {
        Self::new()
    }
}

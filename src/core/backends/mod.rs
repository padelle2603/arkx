pub mod bsdtar;
pub mod native;
pub mod seven_zip;

use super::archive::{ArchiveBackend, ArchiveInfo, ProgressInfo};
use super::detector::{ArchiveFormat, BackendKind};
use super::error::{ArkxError, Result};
use std::path::{Path, PathBuf};

pub struct BackendManager {
    native: native::NativeBackend,
    seven: seven_zip::SevenZipBackend,
    bsdtar: bsdtar::BsdtarBackend,
}

impl BackendManager {
    pub fn new() -> Self {
        Self {
            native: native::NativeBackend,
            seven: seven_zip::SevenZipBackend::new(),
            bsdtar: bsdtar::BsdtarBackend::new(),
        }
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
                    eprintln!("[core] native list failed for {:?}: {}, falling back to 7z", fmt, first);
                    match self.seven.list(path) {
                        Ok(info) => Ok(info),
                        Err(second) => {
                            eprintln!("[core] 7z list failed for {:?}: {}, falling back to bsdtar", fmt, second);
                            self.bsdtar.list(path)
                        }
                    }
                } else if matches!(fmt.backend(), BackendKind::SevenZip) {
                    eprintln!("[core] 7z list failed for {:?}: {}, falling back to bsdtar", fmt, first);
                    self.bsdtar.list(path)
                } else {
                    eprintln!("[core] bsdtar list failed for {:?}: {}, falling back to 7z", fmt, first);
                    match self.seven.list(path) {
                        Ok(info) => Ok(info),
                        Err(_) => Err(first),
                    }
                }
            }
        }
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
        // Large ZIPs go multithreaded 7z even though the table says native:
        // zero-fork only pays off below ~100MB.
        let big_zip = fmt == ArchiveFormat::Zip
            && std::fs::metadata(archive).map(|m| m.len() >= 100 * 1024 * 1024).unwrap_or(false);
        if big_zip {
            return self.seven.extract(archive, dest, entries, password, progress);
        }
        // NOTE: progress callbacks are single-shot (Fn, not clonable):
        // only the backend that actually runs receives it; fallbacks
        // re-run without progress.
        match self.primary(&fmt).extract(archive, dest, entries, password, progress) {
            Ok(()) => Ok(()),
            Err(e) => {
                if matches!(fmt.backend(), BackendKind::Native) {
                    eprintln!("[core] native extract failed, falling back to 7z: {}", e);
                    match self.seven.extract(archive, dest, entries, password, None) {
                        Ok(()) => Ok(()),
                        Err(second) => {
                            eprintln!("[core] 7z extract failed, falling back to bsdtar: {}", second);
                            self.bsdtar.extract(archive, dest, entries, None, None)
                        }
                    }
                } else if matches!(fmt.backend(), BackendKind::SevenZip) {
                    eprintln!("[core] 7z extract failed, falling back to bsdtar: {}", e);
                    self.bsdtar.extract(archive, dest, entries, None, None)
                } else {
                    eprintln!("[core] bsdtar extract failed, falling back to 7z: {}", e);
                    match self.seven.extract(archive, dest, entries, password, None) {
                        Ok(()) => Ok(()),
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
        if self.native.supports(&fmt) && password.is_none() {
            // Native has no password support yet.
            self.native.create(dest, sources, level, password, progress)
        } else {
            self.seven.create(dest, sources, level, password, progress)
        }
    }
}

impl Default for BackendManager {
    fn default() -> Self { Self::new() }
}

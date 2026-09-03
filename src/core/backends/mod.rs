pub mod native;
pub mod seven_zip;

use super::archive::{ArchiveBackend, ArchiveInfo, ProgressInfo};
use super::detector::{ArchiveFormat, BackendKind};
use super::error::Result;
use std::path::{Path, PathBuf};

pub struct BackendManager {
    native: native::NativeBackend,
    seven: seven_zip::SevenZipBackend,
}

impl BackendManager {
    pub fn new() -> Self {
        Self {
            native: native::NativeBackend,
            seven: seven_zip::SevenZipBackend::new(),
        }
    }

    /// Primary backend from the format table (`detector::BackendKind`).
    fn primary(&self, fmt: &ArchiveFormat) -> &dyn ArchiveBackend {
        match fmt.backend() {
            BackendKind::Native => &self.native,
            BackendKind::SevenZip => &self.seven,
        }
    }

    pub fn detect_and_list(&self, path: &Path) -> Result<ArchiveInfo> {
        let fmt = super::detector::detect_format(path);
        match self.primary(&fmt).list(path) {
            Ok(info) => Ok(info),
            Err(e) => {
                // Cross-fallback (e.g. encrypted ZIPs native cannot open).
                if matches!(fmt.backend(), BackendKind::Native) {
                    eprintln!("[core] native list failed for {:?}: {}, falling back to 7z", fmt, e);
                    self.seven.list(path)
                } else {
                    Err(e)
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
        match self.primary(&fmt).extract(archive, dest, entries, password, progress) {
            Ok(()) => Ok(()),
            Err(e) => {
                if matches!(fmt.backend(), BackendKind::Native) {
                    eprintln!("[core] native extract failed, falling back to 7z: {}", e);
                    self.seven.extract(archive, dest, entries, password, None)
                } else {
                    Err(e)
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

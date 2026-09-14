use chrono::{DateTime, Local};
use serde::Serialize;

use super::error::Result;
use std::path::{Path, PathBuf};

/// One entry inside an archive.
#[derive(Debug, Clone, Serialize)]
pub struct ArchiveEntry {
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub packed_size: u64,
    pub modified: Option<DateTime<Local>>,
    pub mode: Option<u32>,
    pub crc32: Option<String>,
    pub method: Option<String>,
    pub encrypted: bool,
}

impl ArchiveEntry {
    pub fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// Archive contents summary.
#[derive(Debug, Clone, Serialize)]
pub struct ArchiveInfo {
    pub path: String,
    pub format: String,
    pub entries: Vec<ArchiveEntry>,
    pub total_size: u64,
    pub total_packed: u64,
    pub num_files: usize,
    pub num_dirs: usize,
    pub has_encrypted: bool,
    pub comment: Option<String>,
}

/// Byte-based progress event: `percent` is always `current/total*100`.
#[derive(Debug, Clone)]
pub struct ProgressInfo {
    pub file: String,
    pub current: u64,
    pub total: u64,
    pub percent: f32,
}

impl ProgressInfo {
    pub fn new(file: String, current: u64, total: u64) -> Self {
        let percent = if total == 0 {
            0.0
        } else {
            current as f32 / total as f32 * 100.0
        };
        Self {
            file,
            current,
            total,
            percent,
        }
    }
}

/// Backends share this shape: list / extract / create with an optional
/// byte-based progress callback. (Kept minimal on purpose: only the two
/// real backends implement it via `BackendManager`.)
pub trait ArchiveBackend: Send + Sync {
    fn list(&self, path: &Path) -> Result<ArchiveInfo>;
    fn extract(
        &self,
        archive: &Path,
        dest: &Path,
        entries: Option<&[String]>,
        password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()>;
    fn create(
        &self,
        dest: &Path,
        sources: &[PathBuf],
        level: u8,
        password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()>;
    /// Add `sources` (filesystem path → entry path inside the archive) to an
    /// existing archive. Implemented only by backends that can rewrite/update
    /// in place (see `BackendManager::add`); the default is a clear error.
    fn add(
        &self,
        _archive: &Path,
        _sources: &[(PathBuf, String)],
        _password: Option<&str>,
        _progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        Err(super::error::ArkxError::UnsupportedFormat(
            "adding files to this archive format is not supported".into(),
        ))
    }
    /// Remove archive entries by name from an existing archive. Implemented only
    /// by backends that can rewrite/update in place (see `BackendManager::remove`);
    /// the default is a clear error.
    fn remove(
        &self,
        _archive: &Path,
        _entries: &[String],
        _password: Option<&str>,
        _progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        Err(super::error::ArkxError::UnsupportedFormat(
            "removing files from this archive format is not supported".into(),
        ))
    }
    fn supports(&self, format: &super::detector::ArchiveFormat) -> bool;
}

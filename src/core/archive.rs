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
    /// Archive-level comment, when the format stores one (zip/7z/rar).
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

/// Result of an integrity test.
#[derive(Debug, Clone)]
pub struct TestReport {
    pub archive: String,
    pub results: Vec<TestResult>,
    pub passed: usize,
    pub failed: usize,
}

/// Single-entry integrity test result.
#[derive(Debug, Clone)]
pub struct TestResult {
    pub entry: String,
    pub is_dir: bool,
    pub crc32_expected: Option<String>,
    pub crc32_actual: Option<String>,
    pub passed: bool,
}

/// Backends share this shape: list / extract / create / add / remove /
/// rename / test / open_with / secure_delete.
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
        _volume_size: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()>;
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
    fn rename(
        &self,
        _archive: &Path,
        _old_name: &str,
        _new_name: &str,
        _password: Option<&str>,
        _progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        Err(super::error::ArkxError::UnsupportedFormat(
            "renaming entries in this archive format is not supported".into(),
        ))
    }
    fn test(
        &self,
        _archive: &Path,
        _entries: Option<&[String]>,
        _password: Option<&str>,
    ) -> Result<TestReport> {
        Err(super::error::ArkxError::UnsupportedFormat(
            "testing archives of this format is not supported".into(),
        ))
    }
    fn open_with(
        &self,
        _archive: &Path,
        _entry: &str,
        _password: Option<&str>,
        _temp_dir: &Path,
    ) -> Result<PathBuf> {
        Err(super::error::ArkxError::UnsupportedFormat(
            "opening entries of this format with an external app is not supported".into(),
        ))
    }
    fn secure_delete(
        &self,
        _archive: &Path,
        _entries: &[String],
        _passes: usize,
        _password: Option<&str>,
        _progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        Err(super::error::ArkxError::UnsupportedFormat(
            "secure delete is not supported for this format".into(),
        ))
    }
    fn supports(&self, format: &super::detector::ArchiveFormat) -> bool;
}

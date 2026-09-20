use chrono::{DateTime, Local};
use serde::Serialize;

use super::error::Result;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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

/// Progress callback shared between the stdout reader and the completion emit.
pub type SharedCallback = Arc<Mutex<Box<dyn Fn(ProgressInfo) + Send>>>;

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

    /// Initial 0% event before the real work starts.
    pub fn preparing(total: u64) -> Self {
        Self::new("Preparing…".to_string(), 0, total)
    }
}

/// Split an entry list into (files, dirs) counts — the packaged-listing
/// idiom repeated by each backend's `list`.
pub fn count_files_dirs(entries: &[ArchiveEntry]) -> (usize, usize) {
    let files = entries.iter().filter(|e| !e.is_dir).count();
    (files, entries.len() - files)
}

/// SHA-256 and MD5 digests of a single archive entry's decompressed bytes.
#[derive(Debug, Clone, Serialize)]
pub struct EntryHashes {
    pub sha256: String,
    pub md5: String,
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

    /// `extract` with a caller-provided total for the progress bar, as
    /// `(bytes, non-directory entries)`. The GUI computes both from the
    /// already-loaded `ArchiveInfo`; passing them lets tar/7z/bsdtar skip
    /// their own pre-listing pass (no double scan). When given, the 7z/bsdtar
    /// backends drive the bar from per-entry completion lines (O(1) per tick)
    /// instead of walking `dest` every poll; native already counts bytes while
    /// streaming and ignores the entry half.
    /// Default falls back to `extract` (recomputes the total internally).
    #[allow(unused_variables)]
    fn extract_with_total(
        &self,
        archive: &Path,
        dest: &Path,
        entries: Option<&[String]>,
        password: Option<&str>,
        known: Option<(u64, u64)>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        self.extract(archive, dest, entries, password, progress)
    }

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

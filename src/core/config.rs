//! Persistent user speed profiles.
//!
//! Two independent axes, each with three clearly separated choices:
//!
//! - **Extraction** (trade-off: speed vs resources): `Fast` (all cores +
//!   aggressive zip→7z routing from 25 MiB), `Balanced` (system-adaptive,
//!   RAM-scaled thresholds), `Conservative` (single-threaded, native
//!   backends only, minimal RAM).
//! - **Compression** (trade-off: speed vs size): `Fast` (level 1, zip stays
//!   native/parallel), `Balanced` (level 6, RAM-scaled zip→7z threshold),
//!   `Small` (level 9, zip→7z from 64 MiB: smallest archives, slowest).
//!
//! Persisted as JSON in `$XDG_CONFIG_HOME/arkx/config.json`
//! (`~/.config/arkx/config.json` by default). Missing/corrupt files never
//! fail: they fall back to Balanced/Balanced, which is exactly the previous
//! adaptive behavior, so a fresh install is behavior-identical to before.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{LazyLock, RwLock};

pub const CONFIG_DIR_NAME: &str = "arkx";
pub const CONFIG_FILE_NAME: &str = "config.json";

/// Extraction profile: controls parallelism and the zip→7z routing threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtractTier {
    /// All cores + zip→7z routing from 25 MiB (heavy on CPU/RAM).
    Fast,
    /// System-adaptive (default): RAM-scaled threshold, all cores.
    #[default]
    Balanced,
    /// 1 thread, native backends only, no zip→7z routing (minimal RAM).
    Conservative,
}

/// Compression profile: controls the level and the zip→7z routing threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompressTier {
    /// Level 1; zip stays native (parallel rayon writer): fastest, larger files.
    Fast,
    /// Level 6, RAM-scaled zip→7z threshold (default).
    #[default]
    Balanced,
    /// Level 9; zip→7z routing from 64 MiB: smallest archives, slowest.
    Small,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub extraction: ExtractTier,
    #[serde(default)]
    pub compression: CompressTier,
}

/// Runtime mirror of the persisted config. Written on `init()`/`set_…()`;
/// read by the profile-aware helpers in `util`.
static CURRENT: LazyLock<RwLock<Config>> = LazyLock::new(|| RwLock::new(Config::default()));

fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME)
}

/// Load the persisted config (best-effort; never panics) into the runtime
/// mirror. Should be called once at startup (CLI and GUI alike).
pub fn init() {
    let cfg = load_from(&config_path());
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = cfg;
}

/// Read a config from a file, tolerating missing/corrupt content.
pub fn load_from(path: &std::path::Path) -> Config {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("[config] ignoring invalid {}: {}", path.display(), e);
                Config::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => {
            eprintln!("[config] cannot read {}: {}", path.display(), e);
            Config::default()
        }
    }
}

/// Persist `cfg` atomically (tmp + rename) to `path`. Never panics: a failed
/// write only logs (the in-memory profile still applies for this session).
pub fn save_to(cfg: &Config, path: &std::path::Path) {
    let tmp = path.with_extension("json.tmp");
    let write = (|| -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&tmp, serde_json::to_vec_pretty(cfg).unwrap_or_default())?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = write {
        eprintln!("[config] cannot persist {}: {}", path.display(), e);
    }
}

/// Current extraction profile.
pub fn extraction() -> ExtractTier {
    CURRENT.read().unwrap_or_else(|e| e.into_inner()).extraction
}

/// Current compression profile.
pub fn compression() -> CompressTier {
    CURRENT
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .compression
}

/// Update the runtime mirror only (used by tests and profile setters).
pub fn apply(extraction: ExtractTier, compression: CompressTier) {
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = Config {
        extraction,
        compression,
    };
}

/// Set the extraction profile in memory and persist immediately.
pub fn set_extraction(tier: ExtractTier) {
    let compression = compression();
    apply(tier, compression);
    save_current();
}

/// Set the compression profile in memory and persist immediately.
pub fn set_compression(tier: CompressTier) {
    let extraction = extraction();
    apply(extraction, tier);
    save_current();
}

fn save_current() {
    save_to(
        &CURRENT.read().unwrap_or_else(|e| e.into_inner()),
        &config_path(),
    );
}

/// Compression level applied by the CLI when `-l` is not given.
pub fn compression_level() -> u8 {
    match compression() {
        CompressTier::Fast => 1,
        CompressTier::Balanced => 6,
        CompressTier::Small => 9,
    }
}

/// Extraction thread cap from the profile (`None` = auto).
/// `Conservative` limits extraction to a single thread.
pub fn extraction_thread_cap() -> Option<usize> {
    match extraction() {
        ExtractTier::Fast | ExtractTier::Balanced => None,
        ExtractTier::Conservative => Some(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_balanced() {
        assert_eq!(Config::default().extraction, ExtractTier::Balanced);
        assert_eq!(Config::default().compression, CompressTier::Balanced);
    }

    #[test]
    fn roundtrip_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg1.json");
        let cfg = Config {
            extraction: ExtractTier::Conservative,
            compression: CompressTier::Small,
        };
        save_to(&cfg, &path);
        assert_eq!(load_from(&path), cfg);
    }

    #[test]
    fn missing_file_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.json");
        assert_eq!(load_from(&missing), Config::default());
    }

    #[test]
    fn corrupt_file_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.json");
        std::fs::write(&path, "{ not json !!").unwrap();
        assert_eq!(load_from(&path), Config::default());
    }

    #[test]
    fn unknown_fields_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.json");
        std::fs::write(
            &path,
            r#"{"extraction":"fast","compression":"balanced","theme":"x"}"#,
        )
        .unwrap();
        let cfg = load_from(&path);
        assert_eq!(cfg.extraction, ExtractTier::Fast);
        assert_eq!(cfg.compression, CompressTier::Balanced);
    }

    #[test]
    fn tier_mappings_are_distinct() {
        // Compression levels: fast/low, balanced/default, small/high.
        apply(ExtractTier::Balanced, CompressTier::Fast);
        assert_eq!(compression_level(), 1);
        apply(ExtractTier::Balanced, CompressTier::Balanced);
        assert_eq!(compression_level(), 6);
        apply(ExtractTier::Balanced, CompressTier::Small);
        assert_eq!(compression_level(), 9);
        // Extraction thread cap: only Conservative restricts.
        apply(ExtractTier::Fast, CompressTier::Small);
        assert_eq!(extraction_thread_cap(), None);
        apply(ExtractTier::Balanced, CompressTier::Small);
        assert_eq!(extraction_thread_cap(), None);
        apply(ExtractTier::Conservative, CompressTier::Small);
        assert_eq!(extraction_thread_cap(), Some(1));
        // Restore defaults so other tests run pinned to balanced behavior.
        apply(ExtractTier::Balanced, CompressTier::Balanced);
    }
}

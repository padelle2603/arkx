//! File-manager integration helpers (Dolphin ServiceMenus, Ark parity).
//!
//! Ark (KDE) offered the `Compress` entry as a `KFileItemAction` plugin:
//! submenu with `Here (as TAR.GZ)`, `Here (as ZIP)` and `Compress to...`.
//! With static `.desktop` ServiceMenus we cannot hide
//! the entry dynamically for single archives (Ark bug #268163), but
//! we replicate the rest: automatic naming, no-overwrite, native
//! KDE dialogs (kdialog) and reveal in the file manager.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Decode an argument coming from Dolphin's `%F`/`%U`:
/// accepts local paths and `file://` URIs (with percent-encoding).
pub fn decode_input_arg(s: &str) -> PathBuf {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("file://") {
        // file://[host]/path — for local files host is empty or localhost
        let path_part = if let Some(idx) = rest.find('/') {
            let (host, path) = rest.split_at(idx);
            if host.is_empty() || host == "localhost" {
                path
            } else {
                // remote host (smb:/...): unsupported, still return the path
                path
            }
        } else {
            rest
        };
        PathBuf::from(percent_decode(path_part))
    } else {
        PathBuf::from(percent_decode(s))
    }
}

/// Minimal percent-decoding (`%20` → space, `%C3%A0` → `à`).
/// Decodes to bytes then UTF-8 (Dolphin passes `file://` URIs with UTF-8
/// percent-encoded): the old version converted each byte to `char`
/// and corrupted accents/emoji in names ("caffè" → mojibake → "Not found").
fn percent_decode(s: &str) -> String {
    let mut bytes: Vec<u8> = Vec::with_capacity(s.len());
    let raw = s.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' && i + 2 < raw.len() {
            if let (Some(h), Some(l)) = (hex_val(raw[i + 1]), hex_val(raw[i + 2])) {
                bytes.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        bytes.push(raw[i]);
        i += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Canonical extension for `--format`. Accepts common aliases.
pub fn format_extension(format: &str) -> String {
    match format.to_lowercase().as_str() {
        "zip" => "zip".to_string(),
        "tar.gz" | "tgz" => "tar.gz".to_string(),
        "7z" | "7zip" => "7z".to_string(),
        "tar.xz" | "txz" => "tar.xz".to_string(),
        "tar.zst" | "tzst" | "tar.zstd" => "tar.zst".to_string(),
        "tar.bz2" | "tbz2" | "tbz" => "tar.bz2".to_string(),
        "tar" => "tar".to_string(),
        other => other.to_string(),
    }
}

/// Common parent of sources (or cwd if undeterminable).
pub fn common_parent(sources: &[PathBuf]) -> PathBuf {
    if sources.is_empty() {
        return std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    }
    let first_parent = sources[0]
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    // Normalize "" (relative files without dir) → "."
    let first_parent = if first_parent.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        first_parent
    };
    for s in &sources[1..] {
        let p = s
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let p = if p.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            p
        };
        if p != first_parent {
            // Sources from different folders: use cwd like Ark (dest = cwd)
            return std::env::current_dir().unwrap_or(first_parent);
        }
    }
    first_parent
}

/// Base name for the archive: single → file/folder stem;
/// multi-selection → parent folder name (like Ark).
fn base_name_for(sources: &[PathBuf], parent: &Path) -> String {
    if sources.len() == 1 {
        let p = &sources[0];
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "archive".to_string());
        // For files strip the extension (a.txt → a); for dirs keep everything.
        // `file_stem` on "docs" returns "docs", ok for both.
        // On hidden files (".bashrc") file_stem is empty → fallback to the name.
        let stem = Path::new(&name)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let stem = stem.trim();
        if stem.is_empty() {
            sanitize_name(&name)
        } else {
            sanitize_name(stem)
        }
    } else {
        // Multi: name of the folder containing them
        parent
            .file_name()
            .map(|s| sanitize_name(&s.to_string_lossy()))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "archive".to_string())
    }
}

fn sanitize_name(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        return "archive".to_string();
    }
    s.to_string()
}

/// Default destination path, with anti-overwrite
/// (`docs.zip`, `docs-2.zip`, …) like Ark does.
pub fn default_archive_path(sources: &[PathBuf], format: &str) -> PathBuf {
    let parent = common_parent(sources);
    let ext = format_extension(format);
    let base = base_name_for(sources, &parent);
    let candidate = parent.join(format!("{}.{}", base, ext));
    uniquify(&candidate)
}

/// If `path` exists, try `name-2.ext`, `name-3.ext`, …
pub fn uniquify(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let filename = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "archive.zip".to_string());
    // Split only the last known compound extension (tar.gz → stem + tar.gz)
    let (stem, ext) = split_archive_extension(&filename);
    for n in 2..10000 {
        let cand = parent.join(format!("{}-{}.{}", stem, n, ext));
        if !cand.exists() {
            return cand;
        }
    }
    parent.join(format!("{}-{}.{}", stem, std::process::id(), ext))
}

fn split_archive_extension(filename: &str) -> (String, String) {
    let lower = filename.to_lowercase();
    for comp in ["tar.gz", "tar.bz2", "tar.xz", "tar.zst", "tar.zstd", "tar.lz4"] {
        if lower.ends_with(&format!(".{}", comp)) {
            let stem = filename[..filename.len() - comp.len() - 1].to_string();
            return (stem, comp.to_string());
        }
    }
    match filename.rfind('.') {
        Some(i) if i > 0 => (filename[..i].to_string(), filename[i + 1..].to_string()),
        _ => (filename.to_string(), "zip".to_string()),
    }
}

/// Native "Compress to..." dialog (Ark style): try kdialog (Plasma),
/// then zenity (GNOME), otherwise `None` → the caller uses `--here`.
pub fn ask_save_destination(suggested: &Path) -> Option<PathBuf> {
    let suggested_str = suggested.to_string_lossy().to_string();
    // kdialog --getsavefilename <start> [filter]
    if let Ok(out) = Command::new("kdialog")
        .args(["--getsavefilename", &suggested_str])
        .output()
    {
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
            return None; // cancelled
        }
    }
    // zenity --file-selection --save --filename=<suggested> --confirm-overwrite
    if let Ok(out) = Command::new("zenity")
        .args([
            "--file-selection",
            "--save",
            "--confirm-overwrite",
            &format!("--filename={}", suggested_str),
        ])
        .output()
    {
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
    }
    None
}

/// "Extract to..." dialog: asks for a destination folder.
pub fn ask_directory(initial: &Path) -> Option<PathBuf> {
    let initial_str = initial.to_string_lossy().to_string();
    if let Ok(out) = Command::new("kdialog")
        .args(["--getexistingdirectory", &initial_str])
        .output()
    {
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
            return None;
        }
    }
    if let Ok(out) = Command::new("zenity")
        .args([
            "--file-selection",
            "--directory",
            &format!("--filename={}", initial_str),
        ])
        .output()
    {
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
    }
    None
}

/// Best-effort desktop notification (ignore errors: CLI stays usable over ssh).
pub fn notify_created(path: &Path) {
    let msg = format!("Created {}", path.display());
    let _ = Command::new("notify-send")
        .args(["Arkx", &msg, "--icon=arkx"])
        .output();
}

/// Highlight the file in the file manager (like Ark's `highlightInFileManager`).
/// Uses only the freedesktop portal (no new window): if it fails,
/// do nothing — the CLI stays silent and script-friendly.
pub fn reveal_in_file_manager(path: &Path) {
    let uri = format!(
        "file://{}",
        std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
    );
    // org.freedesktop.FileManager1.ShowItems (supported by Dolphin/Nautilus)
    let _ = Command::new("dbus-send")
        .args([
            "--session",
            "--print-reply",
            "--dest=org.freedesktop.FileManager1",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1.ShowItems",
            &format!("array:string:{}", uri),
            "string:",
        ])
        .output();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_file_uris() {
        assert_eq!(
            decode_input_arg("file:///home/user/Miei%20Documenti/docs"),
            PathBuf::from("/home/user/Miei Documenti/docs")
        );
        assert_eq!(
            decode_input_arg("/home/user/docs"),
            PathBuf::from("/home/user/docs")
        );
        assert_eq!(
            decode_input_arg("file://localhost/tmp/a.txt"),
            PathBuf::from("/tmp/a.txt")
        );
    }

    #[test]
    fn decodes_utf8_percent_encoding() {
        // Dolphin passes file:// URIs with UTF-8 percent-encoded: each %XX is a
        // BYTE, not a char (the old version corrupted accents/emoji).
        assert_eq!(
            decode_input_arg("file:///home/user/caff%C3%A8/relazione.txt"),
            PathBuf::from("/home/user/caffè/relazione.txt")
        );
        assert_eq!(
            decode_input_arg("/tmp/cartella%20con%20spazi"),
            PathBuf::from("/tmp/cartella con spazi")
        );
    }

    #[test]
    fn default_name_single_file_and_dir() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("relazione.txt");
        std::fs::write(&f, "x").unwrap();
        let dest = default_archive_path(&[f], "zip");
        assert_eq!(dest.extension().and_then(|s| s.to_str()), Some("zip"));
        assert!(dest.file_name().unwrap().to_string_lossy().starts_with("relazione"));

        let sub = dir.path().join("foto");
        std::fs::create_dir(&sub).unwrap();
        let dest = default_archive_path(&[sub.to_path_buf()], "tar.gz");
        let name = dest.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("foto"));
        assert!(name.ends_with(".tar.gz"));
    }

    #[test]
    fn default_name_multi_uses_parent() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("progetto");
        std::fs::create_dir(&proj).unwrap();
        let a = proj.join("a.txt");
        let b = proj.join("b.txt");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let dest = default_archive_path(&[a, b], "zip");
        assert!(dest
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("progetto"));
    }

    #[test]
    fn uniquify_avoids_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("docs.zip");
        std::fs::write(&target, "x").unwrap();
        let unique = uniquify(&target);
        assert_eq!(
            unique.file_name().unwrap().to_string_lossy(),
            "docs-2.zip"
        );
        // compound tar.gz keeps the whole extension
        let tgz = dir.path().join("foto.tar.gz");
        std::fs::write(&tgz, "x").unwrap();
        let unique = uniquify(&tgz);
        assert!(unique
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(".tar.gz"));
        assert!(unique
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("foto-2"));
    }

    #[test]
    fn format_aliases() {
        assert_eq!(format_extension("TGZ"), "tar.gz");
        assert_eq!(format_extension("7zip"), "7z");
        assert_eq!(format_extension("zip"), "zip");
    }
}

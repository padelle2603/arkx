//! File-manager integration helpers (Dolphin ServiceMenus, Ark parity).
//!
//! Ark (KDE) offriva la voce `Comprimi` come plugin `KFileItemAction`:
//! sottomenu con `Qui (come TAR.GZ)`, `Qui (come ZIP)` e `Comprimi in...`.
//! Con ServiceMenu `.desktop` statici non possiamo nascondere
//! dinamicamente la voce per i singoli archivi (bug Ark #268163), ma
//! replichiamo il resto: naming automatico, no-sovrascrittura, dialoghi
//! nativi KDE (kdialog) e reveal nel file manager.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Decode un argomento proveniente da `%F`/`%U` di Dolphin:
/// accetta path locali e URI `file://` (con percent-encoding).
pub fn decode_input_arg(s: &str) -> PathBuf {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("file://") {
        // file://[host]/path — per i file locali host è vuoto o localhost
        let path_part = if let Some(idx) = rest.find('/') {
            let (host, path) = rest.split_at(idx);
            if host.is_empty() || host == "localhost" {
                path
            } else {
                // host remoto (smb:/...): non supportato, ritorna il path comunque
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

/// Minimal percent-decoding (`%20` → spazio). Errori lasciati intatti.
fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((h * 16 + l) as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Estensione canonica per `--format`. Accetta alias comuni.
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

/// Parent comune dei sorgenti (o cwd se non determinabile).
pub fn common_parent(sources: &[PathBuf]) -> PathBuf {
    if sources.is_empty() {
        return std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    }
    let first_parent = sources[0]
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    // Normalizza "" (file relativi senza dir) → "."
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
            // Sorgenti da cartelle diverse: usa la cwd come Ark (dest = cwd)
            return std::env::current_dir().unwrap_or(first_parent);
        }
    }
    first_parent
}

/// Nome base per l'archivio: singolo → stem del file/cartella;
/// multi-selezione → nome della cartella genitore (come Ark).
fn base_name_for(sources: &[PathBuf], parent: &Path) -> String {
    if sources.len() == 1 {
        let p = &sources[0];
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "archivio".to_string());
        // Per i file togli l'estensione (a.txt → a); per le dir tieni tutto.
        // `file_stem` su "docs" ritorna "docs", ok per entrambi.
        // Su file nascosti (".bashrc") file_stem è vuoto → fallback al nome.
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
        // Multi: nome della cartella che li contiene
        parent
            .file_name()
            .map(|s| sanitize_name(&s.to_string_lossy()))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "archivio".to_string())
    }
}

fn sanitize_name(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        return "archivio".to_string();
    }
    s.to_string()
}

/// Percorso di destinazione di default, con anti-sovrascrittura
/// (`docs.zip`, `docs-2.zip`, …) come fa Ark.
pub fn default_archive_path(sources: &[PathBuf], format: &str) -> PathBuf {
    let parent = common_parent(sources);
    let ext = format_extension(format);
    let base = base_name_for(sources, &parent);
    let candidate = parent.join(format!("{}.{}", base, ext));
    uniquify(&candidate)
}

/// Se `path` esiste, prova `nome-2.ext`, `nome-3.ext`, …
pub fn uniquify(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let filename = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "archivio.zip".to_string());
    // Splitta solo l'ultima estensione composta nota (tar.gz → stem + tar.gz)
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

/// Dialogo nativo "Comprimi in..." (stile Ark): prova kdialog (Plasma),
/// poi zenity (GNOME), altrimenti `None` → il chiamante usa `--here`.
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
            return None; // annullato
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

/// Dialogo "Estrai in...": chiede una cartella di destinazione.
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

/// Notifica desktop best-effort (ignora errori: CLI resta usabile via ssh).
pub fn notify_created(path: &Path) {
    let msg = format!("Creato {}", path.display());
    let _ = Command::new("notify-send")
        .args(["Arkx", &msg, "--icon=arkx"])
        .output();
}

/// Evidenzia il file nel file manager (come `highlightInFileManager` di Ark).
/// Usa solo il portale freedesktop (nessuna nuova finestra): se fallisce,
/// non fa nulla — la CLI resta silenziosa e adatta agli script.
pub fn reveal_in_file_manager(path: &Path) {
    let uri = format!(
        "file://{}",
        std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
    );
    // org.freedesktop.FileManager1.ShowItems (supportato da Dolphin/Nautilus)
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
        // tar.gz composto mantiene l'estensione intera
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

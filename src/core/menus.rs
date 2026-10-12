//! Dolphin "Compress / Extract" service menus, registered by Arkx itself.
//!
//! An AppImage cannot write into `~/.local/share/kio/servicemenus/` on its
//! own — that is Dolphin/KIO's directory, not the bundle's — so the templates
//! used to ship with a shell script the user had to run from a git checkout.
//! They are embedded here instead, so the running binary can install them.
//!
//! Two details matter for Dolphin actually accepting the menu, both learned
//! the hard way from the shell version this replaces:
//! - a user-local `.desktop` that is not executable is refused outright
//!   ("not authorized to run the application"), hence the `chmod +x`;
//! - an unquoted path containing spaces silently breaks the command, hence
//!   the `Exec=` quoting.
//!
//! `TryExec` is deliberately absent. It is only valid for `Type=Application`
//! and `desktop-file-validate` rejects it in a Service file, yet the templates
//! carried one, so every generated menu did too.
//!
//! Everything here is best effort: the GUI must start whether or not this
//! succeeds, so callers ignore the error.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const COMPRESS: &str = include_str!("../../data/servicemenus/arkx-compress.desktop");
const EXTRACT: &str = include_str!("../../data/servicemenus/arkx-extract.desktop");

/// `(file name, template)` pairs, in the order they are installed.
const MENUS: [(&str, &str); 2] = [
    ("arkx-compress.desktop", COMPRESS),
    ("arkx-extract.desktop", EXTRACT),
];

/// Quote a path for `Exec=` the desktop-entry way: only when needed, with
/// backslashes and quotes escaped.
fn quote_exec(path: &str) -> String {
    if !path.contains([' ', '\t', '"', '\'', '\\']) {
        return path.to_string();
    }
    let mut out = String::with_capacity(path.len() + 2);
    out.push('"');
    for c in path.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Point a template at the real executable and icon.
///
/// `exe` replaces the bare `arkx` in every `Exec=` line; `icon`, when given,
/// replaces `Icon=arkx`. Any `TryExec` is dropped: see the module docs.
fn rewrite(template: &str, exe: &str, icon: Option<&str>) -> String {
    let quoted = quote_exec(exe);
    let mut out: Vec<String> = Vec::new();

    for line in template.lines() {
        if line.starts_with("TryExec=") {
            continue;
        }
        if line == "Exec=arkx" || line.starts_with("Exec=arkx ") {
            out.push(format!("Exec={quoted}{}", &line["Exec=arkx".len()..]));
            continue;
        }
        if let Some(icon) = icon {
            if let Some(rest) = line.strip_prefix("Icon=arkx") {
                out.push(format!("Icon={icon}{rest}"));
                continue;
            }
        }
        out.push(line.to_string());
    }

    out.join("\n") + "\n"
}

/// Path of the running AppImage, or `None` outside one.
///
/// The AppImage runtime exports `$APPIMAGE`; a system install has the menus
/// already, from the package.
pub fn appimage_path() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE").map(PathBuf::from)
}

/// Copy the bundle's icon somewhere that outlives the mount.
///
/// `$APPDIR` is a `/tmp/.mount_*` directory that a different path on every
/// launch, so pointing the menus straight at it would leave them with a
/// broken `Icon=` after the next run — and the menus are only written once.
/// Copying it into the user's icon theme gives a stable absolute path *and*
/// makes `arkx` resolvable by name for anything else that asks for it.
///
/// Returns `None` when there is no bundle icon or it cannot be copied, which
/// leaves `Icon=arkx` to the system theme.
pub fn stable_icon_path() -> Option<PathBuf> {
    let src = PathBuf::from(std::env::var_os("APPDIR")?).join("arkx.png");
    if !src.is_file() {
        return None;
    }
    let dest = data_home().join("icons/hicolor/256x256/apps/arkx.png");
    std::fs::create_dir_all(dest.parent()?).ok()?;
    if std::fs::copy(&src, &dest).is_ok() {
        Some(dest)
    } else {
        None
    }
}

/// `$XDG_DATA_HOME`, with the usual fallback.
fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Where KIO looks for user-local service menus.
pub fn dest_dir() -> PathBuf {
    data_home().join("kio").join("servicemenus")
}

/// Install both menus for `exe`. Returns the files written.
pub fn install(exe: &Path, icon: Option<&Path>) -> io::Result<Vec<PathBuf>> {
    let dir = dest_dir();
    std::fs::create_dir_all(&dir)?;

    let exe = exe.to_string_lossy();
    let icon = icon.map(|p| p.to_string_lossy().to_string());

    let mut written = Vec::with_capacity(MENUS.len());
    for (name, template) in MENUS {
        let path = dir.join(name);
        std::fs::write(&path, rewrite(template, &exe, icon.as_deref()))?;
        // Without the executable bit Dolphin refuses user-local service menus.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        written.push(path);
    }
    refresh_kio();
    Ok(written)
}

/// Remove both menus. Missing files are not an error.
pub fn uninstall() -> io::Result<Vec<PathBuf>> {
    let dir = dest_dir();
    let mut removed = Vec::with_capacity(MENUS.len());
    for (name, _) in MENUS {
        let path = dir.join(name);
        match std::fs::remove_file(&path) {
            Ok(()) => removed.push(path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    refresh_kio();
    Ok(removed)
}

/// Ask KIO to re-scan. Best effort: Dolphin may not even be installed.
fn refresh_kio() {
    for cmd in ["kbuildsycoca6", "kbuildsycoca5"] {
        if which(cmd).is_some() {
            let _ = std::process::Command::new(cmd)
                .arg("--noincremental")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            return;
        }
    }
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Register the menus once, at first launch, when running as an AppImage.
///
/// Returns `true` when it actually wrote something. Any failure is silent:
/// a user without Dolphin, or without write access, must still get a window.
pub fn auto_install() -> bool {
    use crate::core::config;

    if config::appmenus_installed() {
        return false;
    }
    let Some(exe) = appimage_path() else {
        return false;
    };
    match install(&exe, stable_icon_path().as_deref()) {
        Ok(written) => {
            config::set_appmenus_installed(true);
            for path in &written {
                println!("registered {} for Dolphin", path.display());
            }
            true
        }
        Err(e) => {
            eprintln!("[menus] not registered: {e}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_path_needs_no_quoting() {
        assert_eq!(quote_exec("/home/u/Arkx.AppImage"), "/home/u/Arkx.AppImage");
    }

    #[test]
    fn paths_with_spaces_or_quotes_are_escaped() {
        assert_eq!(quote_exec("/home/My Files/A.kt"), "\"/home/My Files/A.kt\"");
        assert_eq!(quote_exec("/a/b\"c"), "\"/a/b\\\"c\"");
        assert_eq!(quote_exec("/a/b\\c"), "\"/a/b\\\\c\"");
    }

    #[test]
    fn every_exec_line_is_retargeted() {
        let exe = "/apps/Arkx.AppImage";
        let out = rewrite(COMPRESS, exe, None);
        let execs: Vec<&str> = out.lines().filter(|l| l.starts_with("Exec=")).collect();
        assert_eq!(execs.len(), 3);
        for line in execs {
            assert!(line.starts_with(&format!("Exec={exe} ")), "{line}");
            assert!(!line.contains("Exec=arkx"));
        }
    }

    #[test]
    fn bare_exec_is_retargeted_too() {
        let out = rewrite(EXTRACT, "/apps/Arkx.AppImage", None);
        assert!(out.lines().any(|l| l == "Exec=/apps/Arkx.AppImage %F"));
    }

    #[test]
    fn tryexec_is_never_emitted() {
        // Invalid for Type=Service; desktop-file-validate rejects it.
        for exe in ["/apps/Arkx.AppImage", "/home/My Files/Arkx.AppImage"] {
            for template in [COMPRESS, EXTRACT] {
                assert!(!rewrite(template, exe, None).contains("TryExec="));
            }
        }
    }

    #[test]
    fn a_path_with_spaces_still_lands_quoted_in_exec() {
        let exe = "/home/My Files/Arkx.AppImage";
        let out = rewrite(COMPRESS, exe, None);
        assert!(out.contains(&format!("Exec=\"{exe}\" compress")));
    }

    #[test]
    fn icon_is_rewritten_on_every_action() {
        let out = rewrite(COMPRESS, "/apps/Arkx.AppImage", Some("/apps/arkx.png"));
        let icons: Vec<&str> = out.lines().filter(|l| l.starts_with("Icon=")).collect();
        assert_eq!(icons.len(), 4);
        for line in icons {
            assert_eq!(line, "Icon=/apps/arkx.png");
        }
    }

    #[test]
    fn icon_is_left_alone_when_absent() {
        let out = rewrite(COMPRESS, "/apps/Arkx.AppImage", None);
        assert!(out.contains("Icon=arkx"));
        assert!(!out.contains("Icon=/"));
    }

    #[test]
    fn localised_names_survive() {
        let out = rewrite(COMPRESS, "/apps/Arkx.AppImage", None);
        assert!(out.contains("Name[it]=Comprimi in zip..."));
    }
}

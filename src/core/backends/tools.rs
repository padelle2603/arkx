//! Multi-volume creation delegated to external tools that the pure-Rust
//! writers cannot split (Info-ZIP `zip` for .zip, `rar` for .rar).
//!
//! Both tools are spawned directly (no shell), so sizes and passwords are
//! passed as argv entries — safe from injection. Errors name the missing
//! binary so the CLI/UI can tell the user what to install.

use crate::core::error::{ArkxError, Result};
use std::path::{Path, PathBuf};

/// First existing binary among `names` sitting next to the current executable:
/// AppImages ship their external tools (7z/bsdtar) as sidecars. `None` when
/// not running from a bundle.
pub fn sidecar_bin(names: &[&str]) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    names.iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

fn run(cmd: &mut std::process::Command, missing: &str) -> Result<()> {
    let status = cmd.status().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ArkxError::Backend(missing.to_string())
        } else {
            ArkxError::Backend(format!("cannot run external tool: {e}"))
        }
    })?;
    if !status.success() {
        return Err(ArkxError::Backend(format!(
            "external tool failed (exit {}); see the message above",
            status.code().unwrap_or(-1)
        )));
    }
    Ok(())
}

/// `.zip` split via Info-ZIP: `zip -r -s <size> <dest> <sources…>`.
/// Part naming is `name.z01, name.z02…` with the last part kept as `name.zip`.
pub fn create_zip_split(
    dest: &Path,
    sources: &[PathBuf],
    volume_size: &str,
    password: Option<&str>,
) -> Result<()> {
    let mut cmd = std::process::Command::new("zip");
    cmd.arg("-r").arg("-s").arg(volume_size).arg(dest);
    if let Some(pw) = password {
        cmd.arg(format!("-P{pw}"));
    }
    cmd.args(sources);
    run(
        &mut cmd,
        "cannot split .zip into volumes: Info-ZIP `zip` is not installed\n\
         hint: install the `zip` package (e.g. `sudo apt install zip`)",
    )
}

/// `.rar` split via the `rar` archiver: `rar a -v<size> <dest> <sources…>`.
/// Part naming is `name.part1.rar, name.part2.rar…` (no base file).
pub fn create_rar_split(
    dest: &Path,
    sources: &[PathBuf],
    volume_size: &str,
    password: Option<&str>,
) -> Result<()> {
    let mut cmd = std::process::Command::new("rar");
    cmd.arg("a").arg(format!("-v{volume_size}")).arg(dest);
    if let Some(pw) = password {
        cmd.arg(format!("-p{pw}"));
    }
    cmd.args(sources);
    run(
        &mut cmd,
        "cannot split .rar into volumes: the `rar` archiver is not installed\n\
         (RAR creation is proprietary — 7-Zip can only unpack it)\n\
         hint: install `rar` from rarlab or your distro",
    )
}

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum ArchiveFormat {
    Zip,
    SevenZip,
    Rar,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
    TarZst,
    TarLz4,
    Gz,
    Bz2,
    Xz,
    Zst,
    Lz4,
    Iso,
    Cab,
    Arj,
    Lzh,
    Deb,
    Rpm,
    Unknown(String),
}

impl ArchiveFormat {
    pub fn display_name(&self) -> &str {
        match self {
            Self::Zip => "ZIP",
            Self::SevenZip => "7Z",
            Self::Rar => "RAR",
            Self::Tar => "TAR",
            Self::TarGz => "TAR.GZ",
            Self::TarBz2 => "TAR.BZ2",
            Self::TarXz => "TAR.XZ",
            Self::TarZst => "TAR.ZST",
            Self::TarLz4 => "TAR.LZ4",
            Self::Gz => "GZIP",
            Self::Bz2 => "BZIP2",
            Self::Xz => "XZ",
            Self::Zst => "ZSTD",
            Self::Lz4 => "LZ4",
            Self::Iso => "ISO",
            Self::Cab => "CAB",
            Self::Arj => "ARJ",
            Self::Lzh => "LZH",
            Self::Deb => "DEB",
            Self::Rpm => "RPM",
            Self::Unknown(s) => s,
        }
    }

    pub fn backend(&self) -> BackendKind {
        match self {
            Self::Zip | Self::Tar | Self::TarGz | Self::TarBz2 | Self::TarXz | Self::TarZst | Self::Gz | Self::Bz2 | Self::Xz | Self::Zst => BackendKind::Native,
            _ => BackendKind::SevenZip,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BackendKind {
    Native,
    SevenZip,
}

/// Rileva formato da magic bytes + estensione (estensione come fallback per tar.*)
pub fn detect_format(path: &Path) -> ArchiveFormat {
    // 1. Magic bytes via infer + manuale per tar
    if let Ok(Some(kind)) = infer::get_from_path(path) {
        let mime = kind.mime_type();
        if let Some(fmt) = from_mime(mime) {
            // Composite tar.* check (e.g. tar.xz is inferred as xz)
            if matches!(fmt, ArchiveFormat::Tar | ArchiveFormat::Gz | ArchiveFormat::Bz2 | ArchiveFormat::Xz | ArchiveFormat::Zst | ArchiveFormat::Lz4) {
                if let Some(tar_fmt) = detect_tar_composite(path) {
                    return tar_fmt;
                }
            }
            return fmt;
        }
    }

    // 2. Fallback manuale header
    if let Ok(fmt) = detect_by_header(path) {
        if fmt != ArchiveFormat::Unknown(String::new()) {
            if let Some(tar_fmt) = detect_tar_composite(path) {
                // A gzip header with a tar.gz extension means TarGz
                if matches!(tar_fmt, ArchiveFormat::TarGz | ArchiveFormat::TarBz2 | ArchiveFormat::TarXz | ArchiveFormat::TarZst) {
                    return tar_fmt;
                }
            }
            return fmt;
        }
    }

    // 3. Estensione
    detect_by_extension(path)
}

fn from_mime(mime: &str) -> Option<ArchiveFormat> {
    match mime {
        "application/zip" => Some(ArchiveFormat::Zip),
        "application/x-7z-compressed" => Some(ArchiveFormat::SevenZip),
        "application/vnd.rar" | "application/x-rar-compressed" => Some(ArchiveFormat::Rar),
        "application/x-tar" => Some(ArchiveFormat::Tar),
        "application/gzip" => Some(ArchiveFormat::Gz),
        "application/x-bzip2" => Some(ArchiveFormat::Bz2),
        "application/x-xz" => Some(ArchiveFormat::Xz),
        "application/zstd" => Some(ArchiveFormat::Zst),
        "application/x-iso9660-image" => Some(ArchiveFormat::Iso),
        "application/vnd.ms-cab-compressed" => Some(ArchiveFormat::Cab),
        "application/x-deb" | "application/vnd.debian.binary-package" => Some(ArchiveFormat::Deb),
        "application/x-rpm" => Some(ArchiveFormat::Rpm),
        _ => None,
    }
}

fn detect_tar_composite(path: &Path) -> Option<ArchiveFormat> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Some(ArchiveFormat::TarGz)
    } else if name.ends_with(".tar.bz2") || name.ends_with(".tbz2") {
        Some(ArchiveFormat::TarBz2)
    } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
        Some(ArchiveFormat::TarXz)
    } else if name.ends_with(".tar.zst") || name.ends_with(".tzst") {
        Some(ArchiveFormat::TarZst)
    } else if name.ends_with(".tar.lz4") {
        Some(ArchiveFormat::TarLz4)
    } else {
        None
    }
}

fn detect_by_header(path: &Path) -> Result<ArchiveFormat, std::io::Error> {
    use std::fs::File;
    use std::io::Read;
    let mut f = File::open(path)?;
    let mut buf = [0u8; 12];
    let n = f.read(&mut buf)?;
    if n < 4 {
        return Ok(ArchiveFormat::Unknown("too small".into()));
    }
    // ZIP: PK\x03\x04, PK\x05\x06, PK\x07\x08
    if buf[0] == 0x50 && buf[1] == 0x4B && buf[2] >= 0x03 && buf[2] <= 0x07 {
        return Ok(ArchiveFormat::Zip);
    }
    // 7Z: 37 7A BC AF 27 1C
    if buf.starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]) {
        return Ok(ArchiveFormat::SevenZip);
    }
    // RAR: 52 61 72 21 1A 07
    if buf.starts_with(&[0x52, 0x61, 0x72, 0x21, 0x1A, 0x07]) {
        return Ok(ArchiveFormat::Rar);
    }
    // GZ: 1F 8B
    if buf[0] == 0x1F && buf[1] == 0x8B {
        return Ok(ArchiveFormat::Gz);
    }
    // BZ2: 42 5A 68
    if buf.starts_with(&[0x42, 0x5A, 0x68]) {
        return Ok(ArchiveFormat::Bz2);
    }
    // XZ: FD 37 7A 58 5A 00
    if buf.starts_with(&[0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00]) {
        return Ok(ArchiveFormat::Xz);
    }
    // ZSTD: 28 B5 2F FD
    if buf.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return Ok(ArchiveFormat::Zst);
    }
    // TAR: ustar at 257
    if n >= 262 && buf[0..4] != [0, 0, 0, 0] {
        // Check ustar magic at offset 257 (need more bytes)
        let mut full = [0u8; 512];
        let mut f2 = File::open(path)?;
        let _ = f2.read(&mut full)?;
        if &full[257..262] == b"ustar" {
            return Ok(ArchiveFormat::Tar);
        }
    }
    // ISO: 43 44 30 30 31 at 32769
    // CAB: 4D 53 43 46
    if buf.starts_with(&[0x4D, 0x53, 0x43, 0x46]) {
        return Ok(ArchiveFormat::Cab);
    }
    Ok(ArchiveFormat::Unknown("unknown".into()))
}

fn detect_by_extension(path: &Path) -> ArchiveFormat {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    // Ordine importante: tar.* prima
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        return ArchiveFormat::TarGz;
    }
    if name.ends_with(".tar.bz2") || name.ends_with(".tbz2") {
        return ArchiveFormat::TarBz2;
    }
    if name.ends_with(".tar.xz") || name.ends_with(".txz") {
        return ArchiveFormat::TarXz;
    }
    if name.ends_with(".tar.zst") || name.ends_with(".tzst") {
        return ArchiveFormat::TarZst;
    }
    if name.ends_with(".tar.lz4") {
        return ArchiveFormat::TarLz4;
    }
    if name.ends_with(".7z") {
        return ArchiveFormat::SevenZip;
    }
    if name.ends_with(".zip") || name.ends_with(".jar") || name.ends_with(".apk") {
        return ArchiveFormat::Zip;
    }
    if name.ends_with(".rar") {
        return ArchiveFormat::Rar;
    }
    if name.ends_with(".tar") {
        return ArchiveFormat::Tar;
    }
    if name.ends_with(".gz") {
        return ArchiveFormat::Gz;
    }
    if name.ends_with(".bz2") {
        return ArchiveFormat::Bz2;
    }
    if name.ends_with(".xz") {
        return ArchiveFormat::Xz;
    }
    if name.ends_with(".zst") || name.ends_with(".zstd") {
        return ArchiveFormat::Zst;
    }
    if name.ends_with(".lz4") {
        return ArchiveFormat::Lz4;
    }
    if name.ends_with(".iso") || name.ends_with(".img") {
        return ArchiveFormat::Iso;
    }
    if name.ends_with(".cab") {
        return ArchiveFormat::Cab;
    }
    if name.ends_with(".arj") {
        return ArchiveFormat::Arj;
    }
    if name.ends_with(".lha") || name.ends_with(".lzh") {
        return ArchiveFormat::Lzh;
    }
    if name.ends_with(".deb") {
        return ArchiveFormat::Deb;
    }
    if name.ends_with(".rpm") {
        return ArchiveFormat::Rpm;
    }
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        ArchiveFormat::Unknown(ext.to_lowercase())
    } else {
        ArchiveFormat::Unknown("unknown".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    #[test]
    fn test_ext() {
        assert_eq!(detect_by_extension(&PathBuf::from("a.tar.gz")), ArchiveFormat::TarGz);
        assert_eq!(detect_by_extension(&PathBuf::from("a.zip")), ArchiveFormat::Zip);
        assert_eq!(detect_by_extension(&PathBuf::from("a.7z")), ArchiveFormat::SevenZip);
        assert_eq!(detect_by_extension(&PathBuf::from("a.rar")), ArchiveFormat::Rar);
    }
}

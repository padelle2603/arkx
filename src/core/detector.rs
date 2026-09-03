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
    TarZ,
    TarLzma,
    TarLzip,
    TarLzo,
    TarLrzip,
    Gz,
    Bz2,
    Xz,
    Zst,
    Lz4,
    Lzma,
    Compress,
    Iso,
    AppImage,
    Cab,
    Cpio,
    Xar,
    Ar,
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
            Self::TarZ => "TAR.Z",
            Self::TarLzma => "TAR.LZMA",
            Self::TarLzip => "TAR.LZIP",
            Self::TarLzo => "TAR.LZO",
            Self::TarLrzip => "TAR.LRZIP",
            Self::Gz => "GZIP",
            Self::Bz2 => "BZIP2",
            Self::Xz => "XZ",
            Self::Zst => "ZSTD",
            Self::Lz4 => "LZ4",
            Self::Lzma => "LZMA",
            Self::Compress => "COMPRESS",
            Self::Iso => "ISO",
            Self::AppImage => "APPIMAGE",
            Self::Cab => "CAB",
            Self::Cpio => "CPIO",
            Self::Xar => "XAR",
            Self::Ar => "AR",
            Self::Arj => "ARJ",
            Self::Lzh => "LZH",
            Self::Deb => "DEB",
            Self::Rpm => "RPM",
            Self::Unknown(s) => s,
        }
    }

    pub fn backend(&self) -> BackendKind {
        match self {
            // Letture sequenziali veloci senza fork. Lzma/Compress singoli:
            // la lista è sintetica (native) mentre la decodifica cade su 7z.
            Self::Zip | Self::Tar | Self::TarGz | Self::TarBz2 | Self::TarXz | Self::TarZst | Self::TarLz4 | Self::Gz | Self::Bz2 | Self::Xz | Self::Zst | Self::Lz4 | Self::Lzma | Self::Compress => BackendKind::Native,
            // 7z non apre questi tar.* (né elenca tar.Z/tar.lzma): libarchive.
            Self::TarZ | Self::TarLzma | Self::TarLzip | Self::TarLzo | Self::TarLrzip => BackendKind::Libarchive,
            _ => BackendKind::SevenZip,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BackendKind {
    Native,
    SevenZip,
    Libarchive,
}

/// Rileva formato da magic bytes + estensione (estensione come fallback per tar.*)
pub fn detect_format(path: &Path) -> ArchiveFormat {
    // 1. Magic bytes via infer + manuale per tar
    if let Ok(Some(kind)) = infer::get_from_path(path) {
        let mime = kind.mime_type();
        if let Some(fmt) = from_mime(mime) {
            // Composite tar.* check (e.g. tar.xz is inferred as xz)
            if matches!(fmt, ArchiveFormat::Tar | ArchiveFormat::Gz | ArchiveFormat::Bz2 | ArchiveFormat::Xz | ArchiveFormat::Zst | ArchiveFormat::Lz4 | ArchiveFormat::Lzma | ArchiveFormat::Compress) {
                if let Some(tar_fmt) = detect_tar_composite(path) {
                    return tar_fmt;
                }
            }
            return fmt;
        }
    }

    // 2. Fallback manuale header
    if let Ok(fmt) = detect_by_header(path) {
        if !matches!(fmt, ArchiveFormat::Unknown(_)) {
            if let Some(tar_fmt) = detect_tar_composite(path) {
                // A gzip header with a tar.gz extension means TarGz
                if matches!(tar_fmt, ArchiveFormat::TarGz | ArchiveFormat::TarBz2 | ArchiveFormat::TarXz | ArchiveFormat::TarZst | ArchiveFormat::TarLz4 | ArchiveFormat::TarZ | ArchiveFormat::TarLzma | ArchiveFormat::TarLzip | ArchiveFormat::TarLzo | ArchiveFormat::TarLrzip) {
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
        "application/vnd.rar" | "application/x-rar" | "application/x-rar-compressed" => Some(ArchiveFormat::Rar),
        "application/x-tar" => Some(ArchiveFormat::Tar),
        // Alias dei file manager per i tar compressi (es. Nautilus/Dolphin)
        "application/x-compressed-tar" => Some(ArchiveFormat::TarGz),
        "application/x-bzip-compressed-tar" => Some(ArchiveFormat::TarBz2),
        "application/x-tarz" => Some(ArchiveFormat::TarZ),
        "application/x-xz-compressed-tar" => Some(ArchiveFormat::TarXz),
        "application/x-lzma-compressed-tar" => Some(ArchiveFormat::TarLzma),
        "application/x-lzip-compressed-tar" => Some(ArchiveFormat::TarLzip),
        "application/x-tzo" => Some(ArchiveFormat::TarLzo),
        "application/x-lrzip-compressed-tar" => Some(ArchiveFormat::TarLrzip),
        "application/x-lz4-compressed-tar" => Some(ArchiveFormat::TarLz4),
        "application/x-zstd-compressed-tar" => Some(ArchiveFormat::TarZst),
        "application/gzip" => Some(ArchiveFormat::Gz),
        "application/x-bzip" | "application/x-bzip2" => Some(ArchiveFormat::Bz2),
        "application/x-xz" => Some(ArchiveFormat::Xz),
        "application/zstd" => Some(ArchiveFormat::Zst),
        "application/x-lzma" => Some(ArchiveFormat::Lzma),
        "application/x-compress" => Some(ArchiveFormat::Compress),
        "application/x-iso9660-image" | "application/x-cd-image" => Some(ArchiveFormat::Iso),
        "application/x-iso9660-appimage" => Some(ArchiveFormat::AppImage),
        "application/vnd.ms-cab-compressed" => Some(ArchiveFormat::Cab),
        "application/x-bcpio" | "application/x-cpio" | "application/x-cpio-compressed" | "application/x-sv4cpio" | "application/x-sv4crc" => Some(ArchiveFormat::Cpio),
        "application/x-xar" => Some(ArchiveFormat::Xar),
        "application/x-archive" => Some(ArchiveFormat::Ar),
        "application/x-lha" | "application/x-lzh" => Some(ArchiveFormat::Lzh),
        "application/x-deb" | "application/vnd.debian.binary-package" => Some(ArchiveFormat::Deb),
        "application/x-rpm" | "application/x-source-rpm" => Some(ArchiveFormat::Rpm),
        _ => None,
    }
}

fn detect_tar_composite(path: &Path) -> Option<ArchiveFormat> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Some(ArchiveFormat::TarGz)
    } else if name.ends_with(".tar.bz2") || name.ends_with(".tbz2") || name.ends_with(".tbz") {
        Some(ArchiveFormat::TarBz2)
    } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
        Some(ArchiveFormat::TarXz)
    } else if name.ends_with(".tar.zst") || name.ends_with(".tzst") {
        Some(ArchiveFormat::TarZst)
    } else if name.ends_with(".tar.lz4") || name.ends_with(".tlz4") {
        Some(ArchiveFormat::TarLz4)
    } else if name.ends_with(".tar.z") || name.ends_with(".taz") {
        Some(ArchiveFormat::TarZ)
    } else if name.ends_with(".tar.lzma") || name.ends_with(".tlz") {
        Some(ArchiveFormat::TarLzma)
    } else if name.ends_with(".tar.lz") {
        Some(ArchiveFormat::TarLzip)
    } else if name.ends_with(".tzo") || name.ends_with(".tar.lzo") || name.ends_with(".tar.lzop") {
        Some(ArchiveFormat::TarLzo)
    } else if name.ends_with(".tar.lrz") || name.ends_with(".tlrz") {
        Some(ArchiveFormat::TarLrzip)
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
    // LZ4 frame: 04 22 4D 18
    if buf.starts_with(&[0x04, 0x22, 0x4D, 0x18]) {
        if let Some(tar_fmt) = detect_tar_composite(path) {
            if matches!(tar_fmt, ArchiveFormat::TarLz4) {
                return Ok(tar_fmt);
            }
        }
        return Ok(ArchiveFormat::Lz4);
    }
    // compress (.Z): 1F 9D
    if buf[0] == 0x1F && buf[1] == 0x9D {
        if let Some(tar_fmt) = detect_tar_composite(path) {
            if matches!(tar_fmt, ArchiveFormat::TarZ) {
                return Ok(tar_fmt);
            }
        }
        return Ok(ArchiveFormat::Compress);
    }
    // lzip (.lz): 4C 5A 49 50 ("LZIP" v1)
    if buf.starts_with(b"LZIP") {
        if let Some(tar_fmt) = detect_tar_composite(path) {
            if matches!(tar_fmt, ArchiveFormat::TarLzip) {
                return Ok(tar_fmt);
            }
        }
        // Nessun formato singolo dedicato: lascia all'estensione/mime.
    }
    // ar (.a): "!<arch>\n"
    if buf.starts_with(b"!<arch>") {
        return Ok(ArchiveFormat::Ar);
    }
    // xar (.xar): "xar!" 78 61 72 21
    if buf.starts_with(b"xar!") {
        return Ok(ArchiveFormat::Xar);
    }
    // cpio: new ascii "070701"/"070702", old odc "070707", binario C7 71 / 71 C7
    if buf.starts_with(b"070701") || buf.starts_with(b"070702") || buf.starts_with(b"070707") {
        return Ok(ArchiveFormat::Cpio);
    }
    if (buf[0] == 0xC7 && buf[1] == 0x71) || (buf[0] == 0x71 && buf[1] == 0xC7) {
        return Ok(ArchiveFormat::Cpio);
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
    if name.ends_with(".tar.bz2") || name.ends_with(".tbz2") || name.ends_with(".tbz") {
        return ArchiveFormat::TarBz2;
    }
    if name.ends_with(".tar.xz") || name.ends_with(".txz") {
        return ArchiveFormat::TarXz;
    }
    if name.ends_with(".tar.zst") || name.ends_with(".tzst") {
        return ArchiveFormat::TarZst;
    }
    if name.ends_with(".tar.lz4") || name.ends_with(".tlz4") {
        return ArchiveFormat::TarLz4;
    }
    if name.ends_with(".tar.z") || name.ends_with(".taz") {
        return ArchiveFormat::TarZ;
    }
    if name.ends_with(".tar.lzma") || name.ends_with(".tlz") {
        return ArchiveFormat::TarLzma;
    }
    if name.ends_with(".tar.lz") {
        return ArchiveFormat::TarLzip;
    }
    if name.ends_with(".tzo") || name.ends_with(".tar.lzo") || name.ends_with(".tar.lzop") {
        return ArchiveFormat::TarLzo;
    }
    if name.ends_with(".tar.lrz") || name.ends_with(".tlrz") {
        return ArchiveFormat::TarLrzip;
    }
    if name.ends_with(".7z") {
        return ArchiveFormat::SevenZip;
    }
    if name.ends_with(".zip") || name.ends_with(".jar") || name.ends_with(".apk") {
        return ArchiveFormat::Zip;
    }
    if name.ends_with(".rar") || name.ends_with(".r00") {
        return ArchiveFormat::Rar;
    }
    if name.ends_with(".tar") {
        return ArchiveFormat::Tar;
    }
    if name.ends_with(".gz") || name.ends_with(".tgz") {
        return ArchiveFormat::Gz;
    }
    if name.ends_with(".bz2") || name.ends_with(".tbz") {
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
    if name.ends_with(".lzma") {
        return ArchiveFormat::Lzma;
    }
    if name.ends_with(".z") {
        return ArchiveFormat::Compress;
    }
    if name.ends_with(".cpio") || name.ends_with(".bcpio") {
        return ArchiveFormat::Cpio;
    }
    if name.ends_with(".xar") || name.ends_with(".xip") {
        return ArchiveFormat::Xar;
    }
    if name.ends_with(".ar") || name.ends_with(".lib") {
        return ArchiveFormat::Ar;
    }
    if name.ends_with(".appimage") {
        return ArchiveFormat::AppImage;
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
        let ext_lc = ext.to_lowercase();
        // Single-letter static-library extension (avoid greedy suffix match).
        if ext_lc == "a" {
            return ArchiveFormat::Ar;
        }
        ArchiveFormat::Unknown(ext_lc)
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

    #[test]
    fn test_ext_new_formats() {
        // Tar compositi esotici
        assert_eq!(detect_by_extension(&PathBuf::from("a.tar.Z")), ArchiveFormat::TarZ);
        assert_eq!(detect_by_extension(&PathBuf::from("a.taz")), ArchiveFormat::TarZ);
        assert_eq!(detect_by_extension(&PathBuf::from("a.tar.lzma")), ArchiveFormat::TarLzma);
        assert_eq!(detect_by_extension(&PathBuf::from("a.tlz")), ArchiveFormat::TarLzma);
        assert_eq!(detect_by_extension(&PathBuf::from("a.tar.lz")), ArchiveFormat::TarLzip);
        assert_eq!(detect_by_extension(&PathBuf::from("a.tzo")), ArchiveFormat::TarLzo);
        assert_eq!(detect_by_extension(&PathBuf::from("a.tar.lzo")), ArchiveFormat::TarLzo);
        assert_eq!(detect_by_extension(&PathBuf::from("a.tar.lrz")), ArchiveFormat::TarLrzip);
        assert_eq!(detect_by_extension(&PathBuf::from("a.tar.lz4")), ArchiveFormat::TarLz4);
        assert_eq!(detect_by_extension(&PathBuf::from("a.tar.zst")), ArchiveFormat::TarZst);
        // Singoli
        assert_eq!(detect_by_extension(&PathBuf::from("a.Z")), ArchiveFormat::Compress);
        assert_eq!(detect_by_extension(&PathBuf::from("a.lzma")), ArchiveFormat::Lzma);
        assert_eq!(detect_by_extension(&PathBuf::from("a.lz4")), ArchiveFormat::Lz4);
        // Archivi via 7z
        assert_eq!(detect_by_extension(&PathBuf::from("a.cpio")), ArchiveFormat::Cpio);
        assert_eq!(detect_by_extension(&PathBuf::from("a.bcpio")), ArchiveFormat::Cpio);
        assert_eq!(detect_by_extension(&PathBuf::from("a.xar")), ArchiveFormat::Xar);
        assert_eq!(detect_by_extension(&PathBuf::from("libfoo.a")), ArchiveFormat::Ar);
        assert_eq!(detect_by_extension(&PathBuf::from("a.ar")), ArchiveFormat::Ar);
        assert_eq!(detect_by_extension(&PathBuf::from("a.AppImage")), ArchiveFormat::AppImage);
        assert_eq!(detect_by_extension(&PathBuf::from("a.lha")), ArchiveFormat::Lzh);
        assert_eq!(detect_by_extension(&PathBuf::from("a.src.rpm")), ArchiveFormat::Rpm);
        // Nessun falso positivo per .a a lettera singola
        assert_eq!(detect_by_extension(&PathBuf::from("opera")), ArchiveFormat::Unknown("unknown".into()));
    }

    #[test]
    fn test_mime_aliases() {
        assert_eq!(from_mime("application/x-compressed-tar"), Some(ArchiveFormat::TarGz));
        assert_eq!(from_mime("application/x-bzip-compressed-tar"), Some(ArchiveFormat::TarBz2));
        assert_eq!(from_mime("application/x-tarz"), Some(ArchiveFormat::TarZ));
        assert_eq!(from_mime("application/x-xz-compressed-tar"), Some(ArchiveFormat::TarXz));
        assert_eq!(from_mime("application/x-lzma-compressed-tar"), Some(ArchiveFormat::TarLzma));
        assert_eq!(from_mime("application/x-lzip-compressed-tar"), Some(ArchiveFormat::TarLzip));
        assert_eq!(from_mime("application/x-tzo"), Some(ArchiveFormat::TarLzo));
        assert_eq!(from_mime("application/x-lrzip-compressed-tar"), Some(ArchiveFormat::TarLrzip));
        assert_eq!(from_mime("application/x-lz4-compressed-tar"), Some(ArchiveFormat::TarLz4));
        assert_eq!(from_mime("application/x-zstd-compressed-tar"), Some(ArchiveFormat::TarZst));
        assert_eq!(from_mime("application/x-cd-image"), Some(ArchiveFormat::Iso));
        assert_eq!(from_mime("application/x-bcpio"), Some(ArchiveFormat::Cpio));
        assert_eq!(from_mime("application/x-cpio"), Some(ArchiveFormat::Cpio));
        assert_eq!(from_mime("application/x-cpio-compressed"), Some(ArchiveFormat::Cpio));
        assert_eq!(from_mime("application/x-sv4cpio"), Some(ArchiveFormat::Cpio));
        assert_eq!(from_mime("application/x-sv4crc"), Some(ArchiveFormat::Cpio));
        assert_eq!(from_mime("application/x-source-rpm"), Some(ArchiveFormat::Rpm));
        assert_eq!(from_mime("application/x-xar"), Some(ArchiveFormat::Xar));
        assert_eq!(from_mime("application/x-iso9660-appimage"), Some(ArchiveFormat::AppImage));
        assert_eq!(from_mime("application/x-archive"), Some(ArchiveFormat::Ar));
        assert_eq!(from_mime("application/x-rar"), Some(ArchiveFormat::Rar));
        assert_eq!(from_mime("application/x-compress"), Some(ArchiveFormat::Compress));
        assert_eq!(from_mime("application/x-bzip"), Some(ArchiveFormat::Bz2));
        assert_eq!(from_mime("application/x-lzma"), Some(ArchiveFormat::Lzma));
        assert_eq!(from_mime("application/x-lha"), Some(ArchiveFormat::Lzh));
        // I dialetti tar.* non devono restare Unknown
        for fmt in [from_mime("application/x-tar"), from_mime("application/zip"),
                    from_mime("application/x-7z-compressed"), from_mime("application/vnd.rar"),
                    from_mime("application/gzip"), from_mime("application/x-xz"),
                    from_mime("application/zstd"), from_mime("application/vnd.ms-cab-compressed")] {
            assert!(fmt.is_some());
        }
    }

    #[test]
    fn test_backend_routing() {
        use super::BackendKind;
        // Native veloci senza fork
        for fmt in [ArchiveFormat::Zip, ArchiveFormat::Tar, ArchiveFormat::TarGz,
                    ArchiveFormat::TarXz, ArchiveFormat::TarLz4,
                    ArchiveFormat::Gz, ArchiveFormat::Lz4,
                    ArchiveFormat::Lzma, ArchiveFormat::Compress] {
            assert_eq!(fmt.backend(), BackendKind::Native);
        }
        // Esotici via libarchive (7z non li apre / non ne elenca il contenuto)
        for fmt in [ArchiveFormat::TarZ, ArchiveFormat::TarLzma, ArchiveFormat::TarLzip,
                    ArchiveFormat::TarLzo, ArchiveFormat::TarLrzip] {
            assert_eq!(fmt.backend(), BackendKind::Libarchive);
        }
        // Resto via 7z
        for fmt in [ArchiveFormat::SevenZip, ArchiveFormat::Rar, ArchiveFormat::Iso,
                    ArchiveFormat::AppImage, ArchiveFormat::Cpio, ArchiveFormat::Xar,
                    ArchiveFormat::Ar, ArchiveFormat::Cab] {
            assert_eq!(fmt.backend(), BackendKind::SevenZip);
        }
    }
}

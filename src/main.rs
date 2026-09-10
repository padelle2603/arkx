use arkx::core;
use arkx::ui;

use adw::prelude::*;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    // CLI mode: skip the GUI for fast batch operations
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 {
        match args[1].as_str() {
            "x" | "extract" | "l" | "list" | "a" | "create" | "c" | "compress" | "--help" | "-h" | "--version" | "-V" => return run_cli(args),
            _ => {
                // An existing file argument goes to the GUI (handled by ui)
                let p = std::path::Path::new(&args[1]);
                if p.exists() {
                    // GUI mode with a file
                } else if args[1].starts_with('-') {
                    return run_cli(args);
                }
            }
        }
    }

    // GUI mode
    let app = adw::Application::builder()
        .application_id("io.github.padelle.arkx")
        .build();

    app.connect_activate(|app| {
        ui::build_ui(app);
    });

    // Files opened from the file manager (gio open)
    app.connect_open(|app, files, _hint| {
        ui::build_ui(app);
        // A file passed via open is picked up from argv by the window
        if let Some(file) = files.first() {
            if let Some(path) = file.path() {
                eprintln!("[arkx] open {}", path.display());
            }
        }
    });

    app.run_with_args::<&str>(&[]);
    Ok(())
}

fn print_help() {
    println!(r#"Arkx {} — fast multi-threaded archive manager

Usage:
  arkx [COMMAND] [OPTIONS]

Commands:
  l, list <archive>                List archive contents
  x, extract <archive> [dest]      Extract archive into dest (default: .)
               [--here]            Extract next to each archive (file-manager mode)
               [--dialog]          Ask destination with a native dialog
               [-p <password>]     Password if needed

  a, create <dest> <file...>       Create archive (format from extension)
               [-l <0-9>]          Compression level (default 6)
               [-p <password>]     Password (AES256 for 7z/zip)

  c, compress [--here] [--format <zip|tar.gz|7z>]
               [--to <dest>] [--dialog] [--progress] <file...>
                                   Compress files/folders Dolphin-style:
                                   default creates <name>.<fmt> next to
                                   the sources without overwriting.
                                   --progress shows the in-app progress
                                   window (auto-closes, then notifies).
                                    (Ark parity: Compress to zip.../tar.gz.../7zip...)

  (no arguments)                   Launch the GUI
  arkx <archive>                   Launch the GUI and open the archive

Options:
  -h, --help                       Show this help
  -V, --version                    Show version
  -p <password>                    Password
  --threads <N>                    Cap worker threads (default: auto from CPU/RAM;
                                   also ARKX_THREADS=N)

Examples:
  arkx l archive.zip
  arkx x archive.7z ~/Downloads
  arkx x archive.rar . -p secret
  arkx a archive.7z file1.txt folder/ -l 9 -p pwd
  arkx compress --here --format=zip docs/        # docs.zip next to docs/
  arkx compress --dialog photos/                  # Compress to... (kdialog)
  arkx extract --here download.zip               # Extract here
  arkx archive.tar.gz              # open GUI

Formats: ZIP, 7Z, RAR (extract only), TAR, TAR.GZ, TAR.BZ2, TAR.XZ, TAR.ZST, TAR.LZ4, TAR.LZMA, TAR.Z, TAR.LZIP, TAR.LZO, TAR.LRZIP, GZ, BZ2, XZ, ZST, LZ4, LZMA, COMPRESS, ISO, APPIMAGE, CAB, CPIO, XAR, AR, LHA...
CPU: uses every available thread ({} on this machine) with -mmt=on, streaming, zero-copy.
"#,
        env!("CARGO_PKG_VERSION"), crate::core::util::effective_threads());
}

fn run_cli(args: Vec<String>) -> anyhow::Result<()> {
    // Usage: arkx x archive.7z [dest]  | arkx l archive.zip
    // Uses all threads and the optimized backend
    let backend = core::backends::BackendManager::new();

    // Help/version handling
    if args.len() >= 2 && matches!(args[1].as_str(), "--help" | "-h" | "help") {
        print_help();
        return Ok(());
    }
    if args.len() >= 2 && matches!(args[1].as_str(), "--version" | "-V" | "version") {
        println!("arkx {} (7zip 26.02, libarchive, rayon, gtk4 {})", env!("CARGO_PKG_VERSION"), crate::core::util::effective_threads());
        return Ok(());
    }

    if args.len() < 2 {
        print_help();
        return Ok(());
    }

    match args[1].as_str() {
        "l" | "list" => {
            if args.len() < 3 {
                eprintln!("Usage: arkx l <archive>");
                std::process::exit(1);
            }
            let path = PathBuf::from(&args[2]);
            let info = backend.detect_and_list(&path)?;
            println!("Archive: {} ({})", info.path, info.format);
            println!("Files: {}  Folders: {}  Total: {} -> {}",
                info.num_files, info.num_dirs,
                humansize::format_size(info.total_size, humansize::BINARY),
                humansize::format_size(info.total_packed, humansize::BINARY)
            );
            if info.has_encrypted {
                println!("⚠  Contains encrypted files");
            }
            println!("{:<60} {:>12} {:>12} Method", "Name", "Size", "Packed");
            println!("{}", "-".repeat(100));
            for e in &info.entries {
                println!("{:<60} {:>12} {:>12} {} {}",
                    truncate(&e.path, 60),
                    if e.is_dir { "-".into() } else { humansize::format_size(e.size, humansize::BINARY) },
                    if e.is_dir { "-".into() } else { humansize::format_size(e.packed_size, humansize::BINARY) },
                    e.method.as_deref().unwrap_or("-"),
                    if e.encrypted { "🔒" } else { "" }
                );
            }
        }
        "x" | "extract" => {
            let (archives, dest_arg, here, dialog, progress, password, threads) = parse_extract_args(&args)?;
            crate::core::util::set_thread_override(threads);
            if archives.is_empty() {
                eprintln!("Usage: arkx x <archive> [dest] [--here] [--dialog] [-p password]");
                std::process::exit(1);
            }
            // --dialog: ask only once (like Ark "Extract to...")
            let dialog_dest: Option<PathBuf> = if dialog {
                let initial = archives.first()
                    .and_then(|a| a.parent().map(|p| p.to_path_buf()))
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
                match crate::core::fm::ask_directory(&initial) {
                    Some(d) => Some(d),
                    None => {
                        eprintln!("Cancelled.");
                        return Ok(());
                    }
                }
            } else {
                None
            };
            for archive in &archives {
                if !archive.exists() {
                    eprintln!("Not found: {}", archive.display());
                    std::process::exit(1);
                }
                let dest = if let Some(d) = &dialog_dest {
                    d.clone()
                } else if here || (dest_arg.is_none() && archives.len() > 1) {
                    // --here (file-manager mode): extract next to the archive,
                    // not in the cwd of the Dolphin-launched process.
                    archive.parent()
                        .map(|p| if p.as_os_str().is_empty() { PathBuf::from(".") } else { p.to_path_buf() })
                        .unwrap_or_else(|| PathBuf::from("."))
                } else if let Some(d) = &dest_arg {
                    // Only with a single archive does a positional dest make sense
                    if archives.len() > 1 {
                        eprintln!("With multiple archives use --here or --dialog instead of a single dest.");
                        std::process::exit(1);
                    }
                    d.clone()
                } else {
                    // Compat: `arkx x archive` → cwd (historic terminal usage)
                    // From Dolphin always use --here (see ServiceMenu).
                    std::env::current_dir()?
                };
                // App-style progress window (Dolphin entry): auto-close when done
                // + notification; without display fall back to text mode.
                if progress {
                    if crate::ui::fm_progress::has_display() {
                        return crate::ui::fm_progress::run_extract_with_progress(archive.clone(), dest, password.clone());
                    }
                    eprintln!("(no display: using text progress)");
                }
                println!("Extracting {} -> {} ({} threads)...", archive.display(), dest.display(), crate::core::util::effective_threads());
                let start = std::time::Instant::now();
                backend.extract(archive, &dest, None, password.as_deref(), Some(Box::new(|p| {
                    if p.total == 0 {
                        print!("\r... {}", p.file);
                    } else {
                        print!("\r{:>7} {} ({}/{})",
                            crate::core::util::format_percent(p.percent, p.current, p.total), p.file,
                            humansize::format_size(p.current, humansize::BINARY),
                            humansize::format_size(p.total, humansize::BINARY));
                    }
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                })))?;
                println!("\nCompleted in {:.2}s", start.elapsed().as_secs_f32());
            }
        }
        "c" | "compress" => {
            run_compress(&backend, &args)?;
        }
        "a" | "create" => {
            if args.len() < 4 {
                eprintln!("Usage: arkx a <dest.zip|dest.7z|dest.tar.gz> <file...> [-l 0-9] [-p password]");
                std::process::exit(1);
            }
            let dest = PathBuf::from(&args[2]);
            // Collect sources (up to -l / -p flags)
            let mut sources = Vec::new();
            let mut level = 6u8;
            let mut password: Option<String> = None;
            let mut threads: Option<usize> = None;
            let mut i = 3;
            while i < args.len() {
                let consumed = parse_common_flags(&args[i], args.get(i + 1), &mut password, &mut threads);
                if consumed > 0 {
                    i += consumed;
                    continue;
                }
                match args[i].as_str() {
                    "-l" | "--level" => {
                        if let Some(v) = args.get(i+1) {
                            level = v.parse().unwrap_or(6);
                            i += 2;
                            continue;
                        } else {
                            eprintln!("-l/--level requires a value (0-9)");
                            std::process::exit(1);
                        }
                    }
                    s if s.starts_with('-') => {
                        eprintln!("Unknown flag: {}", s);
                        i += 1;
                        continue;
                    }
                    _ => {
                        sources.push(PathBuf::from(&args[i]));
                    }
                }
                i += 1;
            }
            if sources.is_empty() {
                eprintln!("No source files specified");
                std::process::exit(1);
            }
            crate::core::util::set_thread_override(threads);
            println!("Creating {} ({} files, level {}, {} threads)...", dest.display(), sources.len(), level, crate::core::util::effective_threads());
            let start = std::time::Instant::now();
            backend.create(&dest, &sources, level, password.as_deref(), Some(Box::new(|p| {
                print!("\r{:>7} {}", crate::core::util::format_percent(p.percent, p.current, p.total), p.file);
                use std::io::Write;
                let _ = std::io::stdout().flush();
            })))?;
            println!("\nCreated in {:.2}s ({})", start.elapsed().as_secs_f32(), humansize::format_size(std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0), humansize::BINARY));
        }
        _ => {
            eprintln!("Unknown command: {}", args[1]);
            print_help();
            std::process::exit(1);
        }
    }
    Ok(())
}

type ExtractArgs = (Vec<PathBuf>, Option<PathBuf>, bool, bool, bool, Option<String>, Option<usize>);

/// Handle the two flags shared by every command (`-p/--password`, `--threads`),
/// accepting both `--flag value` and `--flag=value` forms. Returns the number of
/// tokens consumed (0 = not a common flag, 1 = `--flag=value`, 2 = `--flag value`).
fn parse_common_flags(
    tok: &str,
    next: Option<&String>,
    password: &mut Option<String>,
    threads: &mut Option<usize>,
) -> usize {
    if tok == "-p" || tok == "--password" {
        if let Some(v) = next {
            *password = Some(v.clone());
            return 2;
        }
        eprintln!("{} requires a value", tok);
        std::process::exit(1);
    }
    if let Some(v) = tok.strip_prefix("--threads=") {
        *threads = crate::core::util::parse_threads_value(v);
        return 1;
    }
    if tok == "--threads" {
        if let Some(v) = next {
            *threads = crate::core::util::parse_threads_value(v);
            return 2;
        }
        eprintln!("--threads requires a value");
        std::process::exit(1);
    }
    0
}

fn parse_extract_args(args: &[String]) -> anyhow::Result<ExtractArgs> {
    let mut archives = Vec::new();
    let mut dest_arg: Option<PathBuf> = None;
    let mut here = false;
    let mut dialog = false;
    let mut progress = false;
    let mut password: Option<String> = None;
    let mut threads: Option<usize> = None;
    let mut i = 2;
    while i < args.len() {
        let consumed = parse_common_flags(&args[i], args.get(i + 1), &mut password, &mut threads);
        if consumed > 0 {
            i += consumed;
            continue;
        }
        match args[i].as_str() {
            "--here" => here = true,
            "--dialog" | "--ask-dest" => dialog = true,
            "--progress" | "--gui" => progress = true,
            s if s.starts_with('-') && archives.is_empty() && s != "-" => {
                // Unknown flag before archives: clear error
                // (after archives, a file starting with - is almost never intended)
                eprintln!("Unknown flag: {}", s);
            }
            _ => {
                let p = crate::core::fm::decode_input_arg(&args[i]);
                if dest_arg.is_none() && !archives.is_empty() && !here && !dialog
                    && !args[i].starts_with("file://") && {
                        // Compat heuristic: `arkx x archive dest` (non-existent dest
                        // or existing folder, but not a second existing archive)
                        !p.exists() || p.is_dir()
                    } && archives.len() == 1
                {
                    // Could be the historic positional dest; but if the path
                    // exists as a regular file it is more likely a 2nd archive.
                    if !(p.exists() && p.is_file()) {
                        dest_arg = Some(p);
                        i += 1;
                        continue;
                    }
                    archives.push(p);
                } else {
                    archives.push(p);
                }
            }
        }
        i += 1;
    }
    Ok((archives, dest_arg, here, dialog, progress, password, threads))
}

/// `arkx compress` — Ark parity for Dolphin ServiceMenus.
///
///   arkx compress --here --format=zip <file...>   # Here (as ZIP)
///   arkx compress --dialog <file...>              # Compress to... (kdialog)
///   arkx compress --to out.7z <file...> [-l 9] [-p pwd]
fn run_compress(backend: &core::backends::BackendManager, args: &[String]) -> anyhow::Result<()> {
    let mut format = "zip".to_string();
    let mut here = false;
    let mut dialog = false;
    let mut to: Option<PathBuf> = None;
    let mut level = 6u8;
    let mut password: Option<String> = None;
    let mut progress = false;
    let mut no_progress = false;
    let mut threads: Option<usize> = None;
    let mut sources = Vec::new();
    let mut i = 2;
    while i < args.len() {
        let consumed = parse_common_flags(&args[i], args.get(i + 1), &mut password, &mut threads);
        if consumed > 0 {
            i += consumed;
            continue;
        }
        match args[i].as_str() {
            "--here" => here = true,
            "--dialog" => dialog = true,
            "--progress" | "--gui" => progress = true,
            "--no-progress" | "--text" => no_progress = true,
            "-f" | "--format" => {
                if let Some(v) = args.get(i + 1) {
                    format = v.clone();
                    i += 1;
                } else {
                    eprintln!("-f/--format requires a value (zip|tar.gz|7z)");
                    std::process::exit(1);
                }
            }
            "--to" => {
                if let Some(v) = args.get(i + 1) {
                    to = Some(crate::core::fm::decode_input_arg(v));
                    i += 1;
                } else {
                    eprintln!("--to requires a destination path");
                    std::process::exit(1);
                }
            }
            "-l" | "--level" => {
                if let Some(v) = args.get(i+1) {
                    level = v.parse().unwrap_or(6).min(9);
                    i += 1;
                } else {
                    eprintln!("-l/--level requires a value (0-9)");
                    std::process::exit(1);
                }
            }
            s if s.starts_with("--format=") => {
                format = s.trim_start_matches("--format=").to_string();
            }
            s if s.starts_with("--to=") => {
                to = Some(crate::core::fm::decode_input_arg(s.trim_start_matches("--to=")));
            }
            s if s.starts_with('-') => {
                eprintln!("Unknown flag: {}", s);
            }
            _ => sources.push(crate::core::fm::decode_input_arg(&args[i])),
        }
        i += 1;
    }
    if sources.is_empty() {
        eprintln!("Usage: arkx compress [--here] [--format zip|tar.gz|7z] [--to <dest>] [--dialog] [--progress] [--threads N] <file...>");
        std::process::exit(1);
    }
    crate::core::util::set_thread_override(threads);
    // Only local files (like Ark: disabled on remote URLs)
    for s in &sources {
        let str = s.to_string_lossy();
        if str.starts_with("http://") || str.starts_with("https://") || str.starts_with("smb://") {
            eprintln!("Only local files supported: {}", s.display());
            std::process::exit(1);
        }
        if !s.exists() {
            eprintln!("Not found: {}", s.display());
            std::process::exit(1);
        }
    }

    let dest = if dialog {
        // Compress to... (Ark style): native dialog with suggested name
        let suggested = crate::core::fm::default_archive_path(&sources, &format);
        match crate::core::fm::ask_save_destination(&suggested) {
            Some(d) => ensure_archive_extension(d, &format),
            None => {
                eprintln!("Cancelled.");
                return Ok(());
            }
        }
    } else if let Some(t) = to {
        // --to folder/ → create inside; --to without extension → add format
        if t.is_dir() {
            let file = crate::core::fm::default_archive_path(&sources, &format)
                .file_name().map(|s| s.to_owned()).unwrap();
            crate::core::fm::uniquify(&t.join(file))
        } else {
            crate::core::fm::uniquify(&ensure_archive_extension(t, &format))
        }
    } else {
        // --here or default: next to sources, without overwriting (like Ark)
        let _ = here; // default is equivalent to --here
        crate::core::fm::default_archive_path(&sources, &format)
    };

    // App-style progress window (Dolphin entry): auto-close when done
    // + notification; without display fall back to text mode.
    if progress && !no_progress {
        if crate::ui::fm_progress::has_display() {
            return crate::ui::fm_progress::run_compress_with_progress(dest, sources, level, password);
        }
        eprintln!("(no display: using text progress)");
    }

    println!("Creating {} ({} files, level {}, {} threads)...", dest.display(), sources.len(), level, crate::core::util::effective_threads());
    let start = std::time::Instant::now();
    backend.create(&dest, &sources, level, password.as_deref(), Some(Box::new(|p| {
        print!("\r{:>7} {}", crate::core::util::format_percent(p.percent, p.current, p.total), p.file);
        use std::io::Write;
        let _ = std::io::stdout().flush();
    })))?;
    let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    println!("\nCreated in {:.2}s ({})", start.elapsed().as_secs_f32(), humansize::format_size(size, humansize::BINARY));
    crate::core::fm::notify_created(&dest);
    crate::core::fm::reveal_in_file_manager(&dest);
    Ok(())
}

/// If the user chose a name without archive extension, add the format.
fn ensure_archive_extension(dest: PathBuf, format: &str) -> PathBuf {
    let ext = crate::core::fm::format_extension(format);
    let name = dest.file_name().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    let has_known = ["zip", "7z", "tar", "tar.gz", "tar.xz", "tar.zst", "tar.bz2", "tgz"]
        .iter().any(|e| name.ends_with(&format!(".{}", e)));
    if has_known {
        dest
    } else {
        let mut s = dest.into_os_string();
        s.push(format!(".{}", ext));
        PathBuf::from(s)
    }
}

fn truncate(s: &str, max: usize) -> String {
    crate::core::util::truncate_middle(s, max)
}

#[cfg(test)]
mod tests {
    use super::parse_common_flags;

    fn default_pass() -> (Option<String>, Option<usize>) {
        (None, None)
    }

    #[test]
    fn common_flags_take_two_tokens() {
        let (mut pw, mut th) = default_pass();
        let n = parse_common_flags("--password", Some(&"s3cr3t".to_string()), &mut pw, &mut th);
        assert_eq!(n, 2);
        assert_eq!(pw.as_deref(), Some("s3cr3t"));

        let (mut pw2, mut th2) = default_pass();
        let n2 = parse_common_flags("--threads", Some(&"8".to_string()), &mut pw2, &mut th2);
        assert_eq!(n2, 2);
        assert_eq!(th2, Some(8));
    }

    #[test]
    fn common_flags_eq_form() {
        let (mut pw, mut th) = default_pass();
        let n = parse_common_flags("-p", Some(&"x".to_string()), &mut pw, &mut th);
        assert_eq!(n, 2);
        let n2 = parse_common_flags("--threads=4", None, &mut pw, &mut th);
        assert_eq!(n2, 1);
        assert_eq!(th, Some(4));
    }

    #[test]
    fn non_common_flag_unhandled() {
        let (mut pw, mut th) = default_pass();
        let n = parse_common_flags("--here", None, &mut pw, &mut th);
        assert_eq!(n, 0);
        assert!(pw.is_none() && th.is_none());
    }
}

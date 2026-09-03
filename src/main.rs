mod core;
mod worker;
mod ui;

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
                                   (Ark parity: Comprimi in zip.../tar.gz.../7zip...)

  (no arguments)                   Launch the GUI
  arkx <archive>                   Launch the GUI and open the archive

Options:
  -h, --help                       Show this help
  -V, --version                    Show version
  -p <password>                    Password

Examples:
  arkx l archive.zip
  arkx x archive.7z ~/Downloads
  arkx x archive.rar . -p secret
  arkx a archive.7z file1.txt folder/ -l 9 -p pwd
  arkx compress --here --format=zip docs/        # docs.zip next to docs/
  arkx compress --dialog foto/                   # Comprimi in... (kdialog)
  arkx extract --here download.zip               # Estrai qui
  arkx archive.tar.gz              # open GUI

Formats: ZIP, 7Z, RAR (extract only), TAR, TAR.GZ, TAR.BZ2, TAR.XZ, TAR.ZST, TAR.LZ4, TAR.LZMA, TAR.Z, TAR.LZIP, TAR.LZO, TAR.LRZIP, GZ, BZ2, XZ, ZST, LZ4, LZMA, COMPRESS, ISO, APPIMAGE, CAB, CPIO, XAR, AR, LHA...
CPU: uses every available thread ({} on this machine) with -mmt=on, streaming, zero-copy.
"#,
        env!("CARGO_PKG_VERSION"), crate::core::util::num_cpus());
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
        println!("arkx {} (7zip 26.02, libarchive, rayon, gtk4 {})", env!("CARGO_PKG_VERSION"), crate::core::util::num_cpus());
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
            let info = backend.detect_and_list(&path).map_err(|e| anyhow::anyhow!(e.to_string()))?;
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
            let (archives, dest_arg, here, dialog, password) = parse_extract_args(&args)?;
            if archives.is_empty() {
                eprintln!("Usage: arkx x <archive> [dest] [--here] [--dialog] [-p password]");
                std::process::exit(1);
            }
            // --dialog: chiedi una volta sola (come Ark "Estrai in...")
            let dialog_dest: Option<PathBuf> = if dialog {
                let initial = archives.first()
                    .and_then(|a| a.parent().map(|p| p.to_path_buf()))
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
                match crate::core::fm::ask_directory(&initial) {
                    Some(d) => Some(d),
                    None => {
                        eprintln!("Annullato.");
                        return Ok(());
                    }
                }
            } else {
                None
            };
            for archive in &archives {
                if !archive.exists() {
                    eprintln!("Non trovato: {}", archive.display());
                    std::process::exit(1);
                }
                let dest = if let Some(d) = &dialog_dest {
                    d.clone()
                } else if here || (dest_arg.is_none() && archives.len() > 1) {
                    // --here (modalità file-manager): estrai accanto all'archivio,
                    // non nella cwd del processo lanciato da Dolphin.
                    archive.parent()
                        .map(|p| if p.as_os_str().is_empty() { PathBuf::from(".") } else { p.to_path_buf() })
                        .unwrap_or_else(|| PathBuf::from("."))
                } else if let Some(d) = &dest_arg {
                    // Solo con un singolo archivio ha senso una dest posizionale
                    if archives.len() > 1 {
                        eprintln!("Con più archivi usa --here o --dialog invece di una singola dest.");
                        std::process::exit(1);
                    }
                    d.clone()
                } else {
                    // Compat: `arkx x archivio` → cwd (uso terminale storico)
                    // Da Dolphin usare sempre --here (vedi ServiceMenu).
                    std::env::current_dir()?
                };
                println!("Extracting {} -> {} ({} threads)...", archive.display(), dest.display(), crate::core::util::num_cpus());
                let start = std::time::Instant::now();
                backend.extract(archive, &dest, None, password.as_deref(), Some(Box::new(|p| {
                    if p.total == 0 {
                        print!("\r... {}", p.file);
                    } else {
                        print!("\r{:>3.0}% {} ({}/{})",
                            p.percent, p.file,
                            humansize::format_size(p.current, humansize::BINARY),
                            humansize::format_size(p.total, humansize::BINARY));
                    }
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                }))).map_err(|e| anyhow::anyhow!(e.to_string()))?;
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
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "-l" | "--level" => {
                        if let Some(v) = args.get(i+1) {
                            level = v.parse().unwrap_or(6);
                            i += 2;
                            continue;
                        }
                    }
                    "-p" | "--password" => {
                        if let Some(v) = args.get(i+1) {
                            password = Some(v.clone());
                            i += 2;
                            continue;
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
            println!("Creating {} ({} files, level {}, {} threads)...", dest.display(), sources.len(), level, crate::core::util::num_cpus());
            let start = std::time::Instant::now();
            backend.create(&dest, &sources, level, password.as_deref(), Some(Box::new(|p| {
                print!("\r{:>3.0}% {}", p.percent, p.file);
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }))).map_err(|e| anyhow::anyhow!(e.to_string()))?;
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

type ExtractArgs = (Vec<PathBuf>, Option<PathBuf>, bool, bool, Option<String>);

fn parse_extract_args(args: &[String]) -> anyhow::Result<ExtractArgs> {
    let mut archives = Vec::new();
    let mut dest_arg: Option<PathBuf> = None;
    let mut here = false;
    let mut dialog = false;
    let mut password: Option<String> = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--here" => here = true,
            "--dialog" | "--ask-dest" => dialog = true,
            "-p" | "--password" => {
                password = args.get(i + 1).cloned();
                i += 1;
            }
            s if s.starts_with('-') && archives.is_empty() && s != "-" => {
                // Flag sconosciuto prima degli archivi: errore chiaro
                // (dopo gli archivi, un file che inizia con - è quasi mai voluto)
                eprintln!("Unknown flag: {}", s);
            }
            _ => {
                let p = crate::core::fm::decode_input_arg(&args[i]);
                if dest_arg.is_none() && !archives.is_empty() && !here && !dialog
                    && !args[i].starts_with("file://") && {
                        // Euristica compat: `arkx x archivio dest` (dest non esistente
                        // o cartella esistente, ma non secondo archivio esistente)
                        !p.exists() || p.is_dir()
                    } && archives.len() == 1
                {
                    // Potrebbe essere la dest posizionale storica; ma se il path
                    // esiste come file regolare è più probabile un 2° archivio.
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
    Ok((archives, dest_arg, here, dialog, password))
}

/// `arkx compress` — parità Ark per i ServiceMenu Dolphin.
///
///   arkx compress --here --format=zip <file...>   # Qui (come ZIP)
///   arkx compress --dialog <file...>              # Comprimi in... (kdialog)
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
    let mut sources = Vec::new();
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--here" => here = true,
            "--dialog" => dialog = true,
            "--progress" | "--gui" => progress = true,
            "--no-progress" | "--text" => no_progress = true,
            "-f" | "--format" => {
                if let Some(v) = args.get(i + 1) {
                    format = v.clone();
                    i += 1;
                }
            }
            "--to" => {
                if let Some(v) = args.get(i + 1) {
                    to = Some(crate::core::fm::decode_input_arg(v));
                    i += 1;
                }
            }
            "-l" | "--level" => {
                if let Some(v) = args.get(i + 1) {
                    level = v.parse().unwrap_or(6).min(9);
                    i += 1;
                }
            }
            "-p" | "--password" => {
                if let Some(v) = args.get(i + 1) {
                    password = Some(v.clone());
                    i += 1;
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
        eprintln!("Usage: arkx compress [--here] [--format zip|tar.gz|7z] [--to <dest>] [--dialog] [--progress] <file...>");
        std::process::exit(1);
    }
    // Solo file locali (come Ark: disabilita su URL remoti)
    for s in &sources {
        let str = s.to_string_lossy();
        if str.starts_with("http://") || str.starts_with("https://") || str.starts_with("smb://") {
            eprintln!("Solo file locali supportati: {}", s.display());
            std::process::exit(1);
        }
        if !s.exists() {
            eprintln!("Non trovato: {}", s.display());
            std::process::exit(1);
        }
    }

    let dest = if dialog {
        // Comprimi in... (stile Ark): dialogo nativo con nome suggerito
        let suggested = crate::core::fm::default_archive_path(&sources, &format);
        match crate::core::fm::ask_save_destination(&suggested) {
            Some(d) => ensure_archive_extension(d, &format),
            None => {
                eprintln!("Annullato.");
                return Ok(());
            }
        }
    } else if let Some(t) = to {
        // --to cartella/ → crea dentro; --to senza estensione → aggiungi formato
        if t.is_dir() {
            let file = crate::core::fm::default_archive_path(&sources, &format)
                .file_name().map(|s| s.to_owned()).unwrap();
            crate::core::fm::uniquify(&t.join(file))
        } else {
            crate::core::fm::uniquify(&ensure_archive_extension(t, &format))
        }
    } else {
        // --here o default: accanto ai sorgenti, senza sovrascrivere (come Ark)
        let _ = here; // default equivale a --here
        crate::core::fm::default_archive_path(&sources, &format)
    };

    // Finestra di progresso stile app (voce Dolphin): auto-chiusura a fine
    // lavoro + notifica; senza display si ricade sul modo testuale.
    if progress && !no_progress {
        if crate::ui::fm_progress::has_display() {
            return crate::ui::fm_progress::run_compress_with_progress(dest, sources, level, password);
        }
        eprintln!("(nessun display: uso il progresso testuale)");
    }

    println!("Creating {} ({} files, level {}, {} threads)...", dest.display(), sources.len(), level, crate::core::util::num_cpus());
    let start = std::time::Instant::now();
    backend.create(&dest, &sources, level, password.as_deref(), Some(Box::new(|p| {
        print!("\r{:>3.0}% {}", p.percent, p.file);
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }))).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    println!("\nCreated in {:.2}s ({})", start.elapsed().as_secs_f32(), humansize::format_size(size, humansize::BINARY));
    crate::core::fm::notify_created(&dest);
    crate::core::fm::reveal_in_file_manager(&dest);
    Ok(())
}

/// Se l'utente ha scelto un nome senza estensione archivio, aggiungi il formato.
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
    if s.len() <= max { s.to_string() } else { format!("{}...", &s[..max-3]) }
}

use arkx::core;
use arkx::core::error::ArkxError;
use arkx::ui;

use adw::prelude::*;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    // CLI mode: skip the GUI for fast batch operations
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 {
        match args[1].as_str() {
            "x" | "extract" | "l" | "list" | "a" | "create" | "c" | "compress" | "u" | "add"
            | "update" | "r" | "rm" | "d" | "delete" | "remove" | "rn" | "rename" | "t"
            | "test" | "o" | "open" | "w" | "wipe" | "mk" | "mkdir" | "convert" | "--help"
            | "-h" | "--version" | "-V" => return run_cli(args),
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

    // Files opened from the file manager (gio open): the path is NOT in argv,
    // so queue it for the window and build only if no window exists yet.
    app.connect_open(|app, files, _hint| {
        if let Some(file) = files.first() {
            if let Some(path) = file.path() {
                arkx::ui::window::queue_open_path(path);
            }
        }
        if app.windows().is_empty() {
            ui::build_ui(app);
        }
    });

    app.run_with_args::<&str>(&[]);
    Ok(())
}

fn print_help() {
    println!(
        r#"Arkx {} — fast multi-threaded archive manager

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
               [-v <size>]         Split into volumes (.7z/.zip/.rar): e.g. 50m, 1g
                                   .zip needs Info-ZIP `zip`, .rar needs `rar` installed

  u, add <archive> <file...>       Add files to an existing archive (zip/7z/tar)
               [--to <dir>]        Entry paths relative to <dir> (default: archive root)
               [-p <password>]     Password for encrypted 7z/zip
               [--threads <N>]

  r, remove <archive> <entry...>   Delete entries from an archive (zip/7z/tar)
               [-p <password>]     Password for encrypted 7z/zip
               [--threads <N>]

  mk, mkdir <archive> <folder>     Create an empty folder inside the archive
               [-p <password>]     Password for encrypted 7z/zip
               [--threads <N>]

  c, compress [--here] [--format <zip|tar.gz|7z>]
               [--to <dest>] [--dialog] [--progress] <file...>
                                   Compress files/folders Dolphin-style:
                                   default creates <name>.<fmt> next to
                                   the sources without overwriting.
                                   --progress shows the in-app progress
                                   window (auto-closes, then notifies).
                                    (Ark parity: Compress to zip.../tar.gz.../7zip...)

  convert <src> <dest>             Re-pack one archive into another format
               [-l <0-9>]          (zip/7z/tar.* destinations); temp extraction
               [-p <password>]     Password for encrypted input/output
               [--threads <N>]

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
  arkx a movie.7z movie.mkv -v 100m                # split 100MB volumes
  arkx add archive.zip backup.txt --to docs/     # add into docs/
  arkx remove archive.zip docs/backup.txt        # delete an entry
  arkx compress --here --format=zip docs/        # docs.zip next to docs/
  arkx compress --dialog photos/                  # Compress to... (kdialog)
  arkx convert old.rar new.zip                   # re-pack into another format
  arkx extract --here download.zip               # Extract here
  arkx archive.tar.gz              # open GUI

Formats: ZIP, 7Z, RAR (extract only), TAR, TAR.GZ, TAR.BZ2, TAR.XZ, TAR.ZST, TAR.LZ4, TAR.LZMA, TAR.Z, TAR.LZIP, TAR.LZO, TAR.LRZIP, GZ, BZ2, XZ, ZST, LZ4, LZMA, COMPRESS, ISO, APPIMAGE, CAB, CPIO, XAR, AR, LHA...
CPU: uses every available thread ({} on this machine) with -mmt=on, streaming, zero-copy.
"#,
        env!("CARGO_PKG_VERSION"),
        crate::core::util::effective_threads()
    );
}

/// Single-line "percent file" progress callback for long-running CLI ops.
fn cli_progress() -> Option<Box<dyn Fn(crate::core::archive::ProgressInfo) + Send>> {
    Some(Box::new(|p| {
        print!(
            "\r{:>7} {}",
            crate::core::util::format_percent(p.percent, p.current, p.total),
            p.file
        );
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }))
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
        println!(
            "arkx {} (7zip 26.02, libarchive, rayon, gtk4 {})",
            env!("CARGO_PKG_VERSION"),
            crate::core::util::effective_threads()
        );
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
            let path = resolve_volume_path(&PathBuf::from(&args[2]));
            let info = backend.detect_and_list(&path)?;
            println!("Archive: {} ({})", info.path, info.format);
            println!(
                "Files: {}  Folders: {}  Total: {} -> {}",
                info.num_files,
                info.num_dirs,
                humansize::format_size(info.total_size, humansize::BINARY),
                humansize::format_size(info.total_packed, humansize::BINARY)
            );
            if info.has_encrypted {
                println!("⚠  Contains encrypted files");
            }
            println!("{:<60} {:>12} {:>12} Method", "Name", "Size", "Packed");
            println!("{}", "-".repeat(100));
            for e in &info.entries {
                println!(
                    "{:<60} {:>12} {:>12} {} {}",
                    crate::core::util::truncate_middle(&e.path, 60),
                    if e.is_dir {
                        "-".into()
                    } else {
                        humansize::format_size(e.size, humansize::BINARY)
                    },
                    if e.is_dir {
                        "-".into()
                    } else {
                        humansize::format_size(e.packed_size, humansize::BINARY)
                    },
                    e.method.as_deref().unwrap_or("-"),
                    if e.encrypted { "🔒" } else { "" }
                );
            }
        }
        "x" | "extract" => {
            let ExtractArgs {
                archives,
                dest_arg,
                here,
                dialog,
                progress,
                password,
                threads,
            } = parse_extract_args(&args)?;
            crate::core::util::set_thread_override(threads);
            if archives.is_empty() {
                eprintln!("Usage: arkx x <archive> [dest] [--here] [--dialog] [-p password]");
                std::process::exit(1);
            }
            // The GUI progress app handles one archive per invocation: with
            // several inputs it would silently quit after the first one.
            if progress && archives.len() > 1 {
                eprintln!("--progress supports a single archive at a time.");
                std::process::exit(1);
            }
            // --dialog: ask only once (like Ark "Extract to...")
            let dialog_dest: Option<PathBuf> = if dialog {
                let initial = archives
                    .first()
                    .and_then(|a| a.parent().map(|p| p.to_path_buf()))
                    .unwrap_or_else(|| {
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    });
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
                    archive
                        .parent()
                        .map(|p| {
                            if p.as_os_str().is_empty() {
                                PathBuf::from(".")
                            } else {
                                p.to_path_buf()
                            }
                        })
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
                        return crate::ui::fm_progress::run_extract_with_progress(
                            archive.clone(),
                            dest,
                            password.clone(),
                        );
                    }
                    eprintln!("(no display: using text progress)");
                }
                println!(
                    "Extracting {} -> {} ({} threads)...",
                    archive.display(),
                    dest.display(),
                    crate::core::util::effective_threads()
                );
                let start = std::time::Instant::now();
                backend.extract(
                    archive,
                    &dest,
                    None,
                    password.as_deref(),
                    Some(Box::new(|p| {
                        if p.total == 0 {
                            print!("\r... {}", p.file);
                        } else {
                            print!(
                                "\r{:>7} {} ({}/{})",
                                crate::core::util::format_percent(p.percent, p.current, p.total),
                                p.file,
                                humansize::format_size(p.current, humansize::BINARY),
                                humansize::format_size(p.total, humansize::BINARY)
                            );
                        }
                        use std::io::Write;
                        let _ = std::io::stdout().flush();
                    })),
                )?;
                println!("\nCompleted in {:.2}s", start.elapsed().as_secs_f32());
            }
        }
        "c" | "compress" => {
            run_compress(&backend, &args)?;
        }
        "u" | "add" | "update" => {
            if args.len() < 4 {
                eprintln!(
                    "Usage: arkx add <archive> <file...> [--to <dir>] [-p password] [--threads N]"
                );
                std::process::exit(1);
            }
            let archive = crate::core::fm::decode_input_arg(&args[2]);
            if !archive.exists() {
                eprintln!(
                    "Not found: {} (use `arkx a <dest> <file...>` to create a new archive)",
                    archive.display()
                );
                std::process::exit(1);
            }
            let mut to_dir: Option<String> = None;
            let mut password: Option<String> = None;
            let mut threads: Option<usize> = None;
            let mut files: Vec<PathBuf> = Vec::new();
            let mut i = 3;
            while i < args.len() {
                let consumed =
                    parse_common_flags(&args[i], args.get(i + 1), &mut password, &mut threads);
                if consumed > 0 {
                    i += consumed;
                    continue;
                }
                match args[i].as_str() {
                    "--to" => {
                        if let Some(v) = args.get(i + 1) {
                            let dir = crate::core::fm::decode_input_arg(v);
                            to_dir = Some(crate::core::paths::with_trailing_slash(
                                &dir.to_string_lossy(),
                            ));
                            i += 1;
                        } else {
                            eprintln!("--to requires a directory (entry paths are relative to it)");
                            std::process::exit(1);
                        }
                    }
                    s if s.starts_with("--to=") => {
                        let dir = crate::core::fm::decode_input_arg(s.trim_start_matches("--to="));
                        to_dir = Some(crate::core::paths::with_trailing_slash(
                            &dir.to_string_lossy(),
                        ));
                    }
                    s if s.starts_with('-') => {
                        eprintln!("Unknown flag: {}", s);
                        std::process::exit(1);
                    }
                    p => {
                        let f = crate::core::fm::decode_input_arg(p);
                        if !f.exists() {
                            eprintln!("Not found: {}", f.display());
                            std::process::exit(1);
                        }
                        files.push(f);
                    }
                }
                i += 1;
            }
            if files.is_empty() {
                eprintln!("No files specified");
                std::process::exit(1);
            }
            crate::core::util::set_thread_override(threads);
            // Entry names: `--to <dir>` prefixes the entries with the dir,
            // otherwise files land in the archive root.
            let sources: Vec<(PathBuf, String)> = files
                .iter()
                .map(|f| {
                    let base = f
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string();
                    let entry = match &to_dir {
                        Some(d) => crate::core::paths::normalize(&format!("{}{}", d, base)),
                        None => crate::core::paths::normalize(&base),
                    };
                    (f.clone(), entry)
                })
                .collect();
            println!(
                "Adding {} item(s) to {} ({} threads)...",
                files.len(),
                archive.display(),
                crate::core::util::effective_threads()
            );
            let start = std::time::Instant::now();
            backend.add(&archive, &sources, password.as_deref(), cli_progress())?;
            let size = std::fs::metadata(&archive).map(|m| m.len()).unwrap_or(0);
            println!(
                "\nAdded in {:.2}s ({})",
                start.elapsed().as_secs_f32(),
                humansize::format_size(size, humansize::BINARY)
            );
        }
        "r" | "rm" | "d" | "delete" | "remove" => {
            if args.len() < 4 {
                eprintln!("Usage: arkx remove <archive> <entry...> [-p password] [--threads N]");
                std::process::exit(1);
            }
            let archive = crate::core::fm::decode_input_arg(&args[2]);
            if !archive.exists() {
                eprintln!(
                    "Not found: {} (use `arkx l <archive>` to list its entries)",
                    archive.display()
                );
                std::process::exit(1);
            }
            let mut password: Option<String> = None;
            let mut threads: Option<usize> = None;
            let mut entries: Vec<String> = Vec::new();
            let mut i = 3;
            while i < args.len() {
                let consumed =
                    parse_common_flags(&args[i], args.get(i + 1), &mut password, &mut threads);
                if consumed > 0 {
                    i += consumed;
                    continue;
                }
                match args[i].as_str() {
                    s if s.starts_with('-') => {
                        eprintln!("Unknown flag: {}", s);
                        std::process::exit(1);
                    }
                    p => entries.push(
                        crate::core::fm::decode_input_arg(p)
                            .to_string_lossy()
                            .into(),
                    ),
                }
                i += 1;
            }
            if entries.is_empty() {
                eprintln!("No entries to remove");
                std::process::exit(1);
            }
            crate::core::util::set_thread_override(threads);
            println!(
                "Removing {} entry(ies) from {} ({} threads)...",
                entries.len(),
                archive.display(),
                crate::core::util::effective_threads()
            );
            let start = std::time::Instant::now();
            backend.remove(&archive, &entries, password.as_deref(), None)?;
            println!("\nRemoved in {:.2}s", start.elapsed().as_secs_f32());
        }
        "mk" | "mkdir" => {
            if args.len() < 4 {
                eprintln!("Usage: arkx mkdir <archive> <folder> [-p password]");
                std::process::exit(1);
            }
            let archive = crate::core::fm::decode_input_arg(&args[2]);
            if !archive.exists() {
                eprintln!("Not found: {}", archive.display());
                std::process::exit(1);
            }
            let raw = crate::core::fm::decode_input_arg(&args[3])
                .to_string_lossy()
                .to_string();
            let folder =
                crate::core::paths::with_trailing_slash(&crate::core::paths::normalize(&raw));
            if folder.is_empty() || folder == "/" || folder == "./" {
                eprintln!("Invalid folder name: {}", raw);
                std::process::exit(1);
            }
            let mut password: Option<String> = None;
            let mut i = 4;
            while i < args.len() {
                if args[i] == "-p" || args[i] == "--password" {
                    if let Some(v) = args.get(i + 1) {
                        password = Some(v.clone());
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            println!("Creating folder '{}' in {}...", folder, archive.display());
            let start = std::time::Instant::now();
            backend.new_folder(&archive, &folder, password.as_deref())?;
            println!("Created in {:.2}s", start.elapsed().as_secs_f32());
        }
        "rn" | "rename" => {
            if args.len() < 5 {
                eprintln!("Usage: arkx rename <archive> <old_entry> <new_entry> [-p password]");
                std::process::exit(1);
            }
            let archive = crate::core::fm::decode_input_arg(&args[2]);
            if !archive.exists() {
                eprintln!("Not found: {}", archive.display());
                std::process::exit(1);
            }
            let old_name = crate::core::fm::decode_input_arg(&args[3]);
            let new_name = crate::core::fm::decode_input_arg(&args[4]);
            let mut password: Option<String> = None;
            let mut i = 5;
            while i < args.len() {
                if args[i] == "-p" || args[i] == "--password" {
                    if let Some(v) = args.get(i + 1) {
                        password = Some(v.clone());
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            println!(
                "Renaming '{}' → '{}' in {}...",
                old_name.display(),
                new_name.display(),
                archive.display()
            );
            let start = std::time::Instant::now();
            backend.rename(
                &archive,
                &old_name.to_string_lossy(),
                &new_name.to_string_lossy(),
                password.as_deref(),
                None,
            )?;
            println!("Renamed in {:.2}s", start.elapsed().as_secs_f32());
        }
        "t" | "test" => {
            if args.len() < 3 {
                eprintln!("Usage: arkx test <archive> [entries...] [-p password]");
                std::process::exit(1);
            }
            let archive = crate::core::fm::decode_input_arg(&args[2]);
            if !archive.exists() {
                eprintln!("Not found: {}", archive.display());
                std::process::exit(1);
            }
            let mut password: Option<String> = None;
            let mut threads: Option<usize> = None;
            let mut entries: Vec<String> = Vec::new();
            let mut i = 3;
            while i < args.len() {
                let consumed =
                    parse_common_flags(&args[i], args.get(i + 1), &mut password, &mut threads);
                if consumed > 0 {
                    i += consumed;
                    continue;
                }
                if args[i] == "-p" || args[i] == "--password" {
                    if let Some(v) = args.get(i + 1) {
                        password = Some(v.clone());
                    }
                    i += 2;
                } else {
                    entries.push(
                        crate::core::fm::decode_input_arg(&args[i])
                            .to_string_lossy()
                            .into(),
                    );
                    i += 1;
                }
            }
            crate::core::util::set_thread_override(threads);
            println!("Testing {}...", archive.display());
            let start = std::time::Instant::now();
            let report = backend.test(
                &archive,
                if entries.is_empty() {
                    None
                } else {
                    Some(&entries)
                },
                password.as_deref(),
            )?;
            println!(
                "Completed in {:.2}s: {} passed, {} failed",
                start.elapsed().as_secs_f32(),
                report.passed,
                report.failed
            );
            for r in &report.results {
                let icon = if r.passed { "✅" } else { "❌" };
                println!("  {} {}", icon, r.entry);
            }
        }
        "o" | "open" => {
            if args.len() < 4 {
                eprintln!("Usage: arkx open <archive> <entry> [-p password]");
                std::process::exit(1);
            }
            let archive = crate::core::fm::decode_input_arg(&args[2]);
            if !archive.exists() {
                eprintln!("Not found: {}", archive.display());
                std::process::exit(1);
            }
            let entry = crate::core::fm::decode_input_arg(&args[3])
                .to_string_lossy()
                .to_string();
            let mut password: Option<String> = None;
            let mut i = 4;
            while i < args.len() {
                if args[i] == "-p" || args[i] == "--password" {
                    if let Some(v) = args.get(i + 1) {
                        password = Some(v.clone());
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            let temp_dir = crate::core::util::open_with_dir()?;
            let path = backend.open_with(&archive, &entry, password.as_deref(), &temp_dir)?;
            println!("Extracted to: {}", path.display());
            let mut child = match std::process::Command::new("xdg-open").arg(&path).spawn() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Failed to launch {}: {}", path.display(), e);
                    std::process::exit(1);
                }
            };
            let _ = child.wait();
            std::fs::remove_dir_all(&temp_dir).ok();
        }
        "w" | "wipe" => {
            if args.len() < 4 {
                eprintln!("Usage: arkx wipe <archive> <entry...> [-p password] [--passes N]");
                std::process::exit(1);
            }
            let archive = crate::core::fm::decode_input_arg(&args[2]);
            if !archive.exists() {
                eprintln!("Not found: {}", archive.display());
                std::process::exit(1);
            }
            let mut password: Option<String> = None;
            let mut passes = 3usize;
            let mut threads: Option<usize> = None;
            let mut entries: Vec<String> = Vec::new();
            let mut i = 3;
            while i < args.len() {
                let consumed =
                    parse_common_flags(&args[i], args.get(i + 1), &mut password, &mut threads);
                if consumed > 0 {
                    i += consumed;
                    continue;
                }
                if args[i] == "--passes" {
                    if let Some(v) = args.get(i + 1) {
                        passes = v.parse().unwrap_or(3);
                    }
                    i += 2;
                } else if let Some(v) = args[i].strip_prefix("--passes=") {
                    passes = v.parse().unwrap_or(3);
                    i += 1;
                } else {
                    entries.push(
                        crate::core::fm::decode_input_arg(&args[i])
                            .to_string_lossy()
                            .into(),
                    );
                    i += 1;
                }
            }
            crate::core::util::set_thread_override(threads);
            if entries.is_empty() {
                eprintln!("No entries specified");
                std::process::exit(1);
            }
            println!(
                "Secure-deleting {} entries from {} ({} passes)...",
                entries.len(),
                archive.display(),
                passes
            );
            let start = std::time::Instant::now();
            backend.secure_delete(&archive, &entries, passes, password.as_deref(), None)?;
            println!("Secure-deleted in {:.2}s", start.elapsed().as_secs_f32());
        }
        "a" | "create" => {
            if args.len() < 4 {
                eprintln!(
                    "Usage: arkx a <dest.zip|dest.7z|dest.tar.gz> <file...> [-l 0-9] [-p password] [-v <size>]"
                );
                std::process::exit(1);
            }
            let dest = PathBuf::from(&args[2]);
            // Collect sources (up to -l / -p / -v flags)
            let mut sources = Vec::new();
            let mut level = 6u8;
            let mut password: Option<String> = None;
            let mut threads: Option<usize> = None;
            let mut volume: Option<String> = None;
            let mut i = 3;
            while i < args.len() {
                let consumed =
                    parse_common_flags(&args[i], args.get(i + 1), &mut password, &mut threads);
                if consumed > 0 {
                    i += consumed;
                    continue;
                }
                match args[i].as_str() {
                    "-l" | "--level" => {
                        if let Some(v) = args.get(i + 1) {
                            level = parse_level_or_exit(v);
                            i += 2;
                            continue;
                        } else {
                            eprintln!("-l/--level requires a value (0-9)");
                            std::process::exit(1);
                        }
                    }
                    "-v" | "--volume-size" => {
                        if let Some(v) = args.get(i + 1) {
                            volume = Some(parse_volume_or_exit(v));
                            i += 2;
                            continue;
                        } else {
                            eprintln!("-v/--volume-size requires a value (e.g. 50m, 1g, 1000000)");
                            std::process::exit(1);
                        }
                    }
                    s if s.starts_with('-') => {
                        eprintln!("Unknown flag: {}", s);
                        std::process::exit(1);
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
            println!(
                "Creating {} ({} files, level {}, {} threads){}...",
                dest.display(),
                sources.len(),
                level,
                crate::core::util::effective_threads(),
                volume
                    .as_deref()
                    .map(|v| format!(", volume {}", v))
                    .unwrap_or_default()
            );
            let start = std::time::Instant::now();
            backend.create(
                &dest,
                &sources,
                level,
                password.as_deref(),
                volume.as_deref(),
                cli_progress(),
            )?;
            println!(
                "\nCreated in {:.2}s ({})",
                start.elapsed().as_secs_f32(),
                humansize::format_size(created_size(&dest, volume.as_deref()), humansize::BINARY)
            );
        }
        "convert" => {
            if args.len() < 4 {
                eprintln!(
                    "Usage: arkx convert <src> <dest.zip|dest.7z|dest.tar.gz> [-l 0-9] [-p password] [--threads N]"
                );
                std::process::exit(1);
            }
            let src = resolve_volume_path(&PathBuf::from(&args[2]));
            let dest = PathBuf::from(&args[3]);
            if !src.exists() {
                eprintln!("Source not found: {}", src.display());
                std::process::exit(1);
            }
            let mut level = 6u8;
            let mut password: Option<String> = None;
            let mut threads: Option<usize> = None;
            let mut i = 4;
            while i < args.len() {
                let consumed =
                    parse_common_flags(&args[i], args.get(i + 1), &mut password, &mut threads);
                if consumed > 0 {
                    i += consumed;
                    continue;
                }
                match args[i].as_str() {
                    "-l" | "--level" => {
                        if let Some(v) = args.get(i + 1) {
                            level = parse_level_or_exit(v);
                            i += 2;
                            continue;
                        }
                        eprintln!("-l/--level requires a value (0-9)");
                        std::process::exit(1);
                    }
                    s if s.starts_with('-') => {
                        eprintln!("Unknown flag: {}", s);
                        std::process::exit(1);
                    }
                    _ => {
                        eprintln!("Unexpected argument: {}", args[i]);
                        std::process::exit(1);
                    }
                }
            }
            // Single-file streams cannot hold a full tree; refuse before the
            // expensive extract so the user is not left waiting pointlessly.
            if matches!(
                crate::core::detector::detect_format(&dest),
                crate::core::detector::ArchiveFormat::Gz
                    | crate::core::detector::ArchiveFormat::Bz2
                    | crate::core::detector::ArchiveFormat::Xz
                    | crate::core::detector::ArchiveFormat::Zst
                    | crate::core::detector::ArchiveFormat::Lz4
            ) {
                eprintln!(
                    "Cannot convert to a single-file stream; use .tar.gz/.tar.bz2/.tar.xz/... instead"
                );
                std::process::exit(1);
            }
            // Refuse to clobber the very archive being converted.
            let src_abs = std::fs::canonicalize(&src).map_err(ArkxError::Io)?;
            let dest_abs = std::fs::canonicalize(&dest).ok().or_else(|| {
                dest.parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .and_then(|p| std::fs::canonicalize(p).ok())
                    .map(|p| p.join(dest.file_name().unwrap_or_default()))
            });
            if dest_abs.as_ref() == Some(&src_abs) {
                eprintln!(
                    "Refusing to overwrite the source archive: {}",
                    src.display()
                );
                std::process::exit(1);
            }
            crate::core::util::set_thread_override(threads);

            let info = backend.detect_and_list(&src)?;
            println!(
                "Converting {} ({}) -> {} (level {}, {} threads)...",
                src.display(),
                info.format,
                dest.display(),
                level,
                crate::core::util::effective_threads()
            );
            // Working dir under the system temp dir; removed on drop
            // (success or error) by the RAII guard. Same pid convention as the
            // rest of the codebase, so a leftover from a previous crash is
            // simply re-used.
            let tmp_dir = crate::core::util::TaskTempDir::new("arkx-convert")?;

            let progress = |p: crate::core::archive::ProgressInfo| {
                print!(
                    "\r  {:>7} {}",
                    crate::core::util::format_percent(p.percent, p.current, p.total),
                    p.file
                );
                use std::io::Write;
                let _ = std::io::stdout().flush();
            };
            let result = (|| -> anyhow::Result<(f32, u64)> {
                backend.extract(
                    &src,
                    &tmp_dir,
                    None,
                    password.as_deref(),
                    Some(Box::new(progress)),
                )?;
                let mut sources = Vec::new();
                for entry in std::fs::read_dir(&*tmp_dir).map_err(ArkxError::Io)? {
                    sources.push(entry.map_err(ArkxError::Io)?.path());
                }
                if sources.is_empty() {
                    return Err(anyhow::anyhow!(
                        "nothing to convert: {} is empty",
                        src.display()
                    ));
                }
                backend.create(
                    &dest,
                    &sources,
                    level,
                    password.as_deref(),
                    None,
                    Some(Box::new(progress)),
                )?;
                Ok((
                    std::time::Instant::now().elapsed().as_secs_f32(),
                    std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0),
                ))
            })();
            let (secs, size) = result?;
            println!(
                "\nSaved {} ({}) in {:.2}s",
                dest.display(),
                humansize::format_size(size, humansize::BINARY),
                secs
            );
        }
        _ => {
            eprintln!("Unknown command: {}", args[1]);
            print_help();
            std::process::exit(1);
        }
    }
    Ok(())
}

/// CLI args for `arkx x`: a struct instead of a 7-tuple so fields are not
/// position-dependent (the positional dest heuristic already tripped once).
struct ExtractArgs {
    archives: Vec<PathBuf>,
    dest_arg: Option<PathBuf>,
    here: bool,
    dialog: bool,
    progress: bool,
    password: Option<String>,
    threads: Option<usize>,
}

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
        *threads = parse_threads_or_exit(v);
        return 1;
    }
    if tok == "--threads" {
        if let Some(v) = next {
            *threads = parse_threads_or_exit(v);
            return 2;
        }
        eprintln!("--threads requires a value");
        std::process::exit(1);
    }
    0
}

/// Strict CLI `--threads` value: `0` or a non-number is rejected instead of
/// being silently downgraded to "auto".
fn parse_threads_or_exit(v: &str) -> Option<usize> {
    match v.trim().parse::<usize>() {
        Ok(n) if n > 0 => Some(n),
        _ => {
            eprintln!("Invalid --threads value: {v} (expected a positive integer)");
            std::process::exit(1);
        }
    }
}

/// Strict, clamped CLI `-l/--level` value (0-9).
fn parse_level_or_exit(v: &str) -> u8 {
    match v.trim().parse::<u8>() {
        Ok(n) => n.min(9),
        Err(_) => {
            eprintln!("Invalid -l/--level value: {v} (expected 0-9)");
            std::process::exit(1);
        }
    }
}

/// Validate a `-v/--volume-size` value: digits plus an optional unit suffix
/// (b/k/m/g). Kept as a string passed through to 7z, which understands it.
fn parse_volume_or_exit(v: &str) -> String {
    let t = v.trim().to_ascii_lowercase();
    let valid = !t.is_empty()
        && t.bytes().take_while(|b| b.is_ascii_digit()).count() > 0
        && t.bytes()
            .skip_while(|b| !b.is_ascii_alphabetic())
            .all(|b| matches!(b, b'b' | b'k' | b'm' | b'g'));
    if !valid {
        eprintln!("Invalid -v/--volume-size value: {v} (examples: 50m, 1g, 1000000)");
        std::process::exit(1);
    }
    t
}

/// Total bytes produced by `arkx a`. Multi-volume parts live beside the
/// archive; naming differs per tool: 7z `<dest>.001…`, Info-ZIP zip
/// `<dest>.z01…` + `<dest>.zip` (last part), rar `<stem>.partN.rar`.
fn created_size(dest: &std::path::Path, volume: Option<&str>) -> u64 {
    use crate::core::detector::ArchiveFormat as Fmt;
    if volume.is_none() {
        return std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    }
    let total_parts = |pattern: &dyn Fn(u32) -> std::path::PathBuf| {
        let mut total = 0u64;
        for n in 1..=999 {
            match std::fs::metadata(pattern(n)) {
                Ok(m) => total += m.len(),
                Err(_) => break,
            }
        }
        total
    };
    let base = dest.to_string_lossy().into_owned();
    match crate::core::detector::detect_format(dest) {
        Fmt::Zip => {
            let mut total = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
            total += total_parts(&|n| std::path::PathBuf::from(format!("{base}.z{n:02}")));
            total
        }
        Fmt::Rar => {
            let stem = dest
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| base.clone());
            let ext = dest
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_else(|| "rar".to_string());
            total_parts(&|n| std::path::PathBuf::from(format!("{stem}.part{n}.{ext}")))
        }
        _ => total_parts(&|n| std::path::PathBuf::from(format!("{base}.{n:03}"))),
    }
}

/// If `p` is missing but a `<p>.001` sibling exists, point at that first
/// volume: 7z multi-volume archives are named `<name>.7z.001...` and the base
/// file never exists, so the CLI can open them as naturally as the `.001`.
fn resolve_volume_path(p: &std::path::Path) -> std::path::PathBuf {
    if p.exists() {
        return p.to_path_buf();
    }
    let mut first = p.as_os_str().to_os_string();
    first.push(".001");
    let cand = std::path::PathBuf::from(first);
    if cand.exists() {
        cand
    } else {
        p.to_path_buf()
    }
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
                std::process::exit(1);
            }
            _ => {
                let p = crate::core::fm::decode_input_arg(&args[i]);
                if dest_arg.is_none()
                    && !archives.is_empty()
                    && !here
                    && !dialog
                    && !args[i].starts_with("file://")
                    && {
                        // Compat heuristic: `arkx x archive dest` (non-existent dest
                        // or existing folder, but not a second existing archive)
                        !p.exists() || p.is_dir()
                    }
                    && archives.len() == 1
                {
                    // Could be the historic positional dest; but if the path
                    // exists as a regular file it is more likely a 2nd archive.
                    if !(p.exists() && p.is_file()) {
                        dest_arg = Some(p);
                        i += 1;
                        continue;
                    }
                    archives.push(resolve_volume_path(&p));
                } else {
                    archives.push(resolve_volume_path(&p));
                }
            }
        }
        i += 1;
    }
    Ok(ExtractArgs {
        archives,
        dest_arg,
        here,
        dialog,
        progress,
        password,
        threads,
    })
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
                if let Some(v) = args.get(i + 1) {
                    level = parse_level_or_exit(v);
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
                to = Some(crate::core::fm::decode_input_arg(
                    s.trim_start_matches("--to="),
                ));
            }
            s if s.starts_with('-') => {
                eprintln!("Unknown flag: {}", s);
                std::process::exit(1);
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
                .file_name()
                .map(|s| s.to_owned())
                .unwrap_or_else(|| "archive".into());
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
            return crate::ui::fm_progress::run_compress_with_progress(
                dest, sources, level, password,
            );
        }
        eprintln!("(no display: using text progress)");
    }

    println!(
        "Creating {} ({} files, level {}, {} threads)...",
        dest.display(),
        sources.len(),
        level,
        crate::core::util::effective_threads()
    );
    let start = std::time::Instant::now();
    backend.create(
        &dest,
        &sources,
        level,
        password.as_deref(),
        None,
        cli_progress(),
    )?;
    let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    println!(
        "\nCreated in {:.2}s ({})",
        start.elapsed().as_secs_f32(),
        humansize::format_size(size, humansize::BINARY)
    );
    crate::core::fm::notify_created(&dest);
    crate::core::fm::reveal_in_file_manager(&dest);
    Ok(())
}

/// If the user chose a name without archive extension, add the format.
fn ensure_archive_extension(dest: PathBuf, format: &str) -> PathBuf {
    let ext = crate::core::fm::format_extension(format);
    let name = dest
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let has_known = [
        "zip", "7z", "tar", "tar.gz", "tar.xz", "tar.zst", "tar.bz2", "tgz",
    ]
    .iter()
    .any(|e| name.ends_with(&format!(".{}", e)));
    if has_known {
        dest
    } else {
        let mut s = dest.into_os_string();
        s.push(format!(".{}", ext));
        PathBuf::from(s)
    }
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

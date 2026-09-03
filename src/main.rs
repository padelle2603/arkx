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
            "x" | "extract" | "l" | "list" | "a" | "create" | "--help" | "-h" | "--version" | "-V" => return run_cli(args),
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
               [-p <password>]     Password if needed

  a, create <dest> <file...>       Create archive (format from extension)
               [-l <0-9>]          Compression level (default 6)
               [-p <password>]     Password (AES256 for 7z/zip)

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
  arkx archive.tar.gz              # open GUI

Formats: ZIP, 7Z, RAR (extract only), TAR, TAR.GZ, TAR.BZ2, TAR.XZ, TAR.ZST, GZ, BZ2, XZ, ZST, ISO, CAB...
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
            if args.len() < 3 {
                eprintln!("Usage: arkx x <archive> [dest] [-p password]");
                std::process::exit(1);
            }
            let archive = PathBuf::from(&args[2]);
            let dest = if args.len() >= 4 && !args[3].starts_with('-') {
                PathBuf::from(&args[3])
            } else {
                std::env::current_dir()?
            };
            let password = args.iter().position(|a| a == "-p").and_then(|i| args.get(i+1).cloned());
            println!("Extracting {} -> {} ({} threads)...", archive.display(), dest.display(), crate::core::util::num_cpus());
            let start = std::time::Instant::now();
            backend.extract(&archive, &dest, None, password.as_deref(), Some(Box::new(|p| {
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

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}...", &s[..max-3]) }
}

use crate::core::archive::{ArchiveBackend, ArchiveEntry, ArchiveInfo, ProgressInfo};
use crate::core::detector::ArchiveFormat;
use crate::core::error::{ArkxError, Result};
use crate::core::paths;
use crate::core::util::num_cpus;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

pub struct SevenZipBackend {
    bin: PathBuf,
    cancel: Arc<AtomicBool>,
}

/// Progress callback shared between the 7z reader and the dest poller.
type SharedCallback = Arc<Mutex<Box<dyn Fn(ProgressInfo) + Send>>>;

impl SevenZipBackend {
    pub fn with_cancel(cancel: Arc<AtomicBool>) -> Self {
        let bin = which_7z();
        Self { bin, cancel }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

impl crate::core::archive::ArchiveBackend for SevenZipBackend {
    fn supports(&self, fmt: &ArchiveFormat) -> bool {
        // 7z handles everything we know
        !matches!(fmt, ArchiveFormat::Unknown(_))
    }

    fn list(&self, path: &Path) -> Result<ArchiveInfo> {
        self.list_inner(path)
    }

    fn extract(
        &self,
        archive: &Path,
        dest: &Path,
        entries: Option<&[String]>,
        password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        self.extract_inner(archive, dest, entries, password, progress)
    }

    fn create(
        &self,
        dest: &Path,
        sources: &[PathBuf],
        level: u8,
        password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        self.create_inner(dest, sources, level, password, progress)
    }
}

fn which_7z() -> PathBuf {
    // Inside an AppImage the 7z binary ships next to us: prefer a `7z`
    // beside the current executable, then the system locations, then PATH.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in ["7z", "7za", "7zr"] {
                let p = dir.join(name);
                if p.is_file() {
                    return p;
                }
            }
        }
    }
    for p in ["/usr/bin/7z", "/usr/bin/7za", "/usr/bin/7zr"] {
        if Path::new(p).exists() {
            return PathBuf::from(p);
        }
    }
    PathBuf::from("7z")
}

impl SevenZipBackend {
    fn list_inner(&self, path: &Path) -> Result<ArchiveInfo> {
        // `7z l -slt` gives machine-parsable technical output
        // (-slt: technical info, -sccUTF-8: charset, -bsp0 -bso1: quiet progress)
        let output = Command::new(&self.bin)
            .args(["l", "-slt", "-sccUTF-8", "-bsp0", "-bso1"])
            .arg(path)
            .output()
            .map_err(|e| ArkxError::Backend(format!("Cannot run 7z: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let msg = format!("{}{}", stdout, stderr);
            if msg.contains("Wrong password") || msg.contains("Enter password") || msg.contains("Can not open encrypted") {
                return Err(ArkxError::WrongPassword);
            }
            if msg.contains("Can not open file as archive") || msg.contains("Is not archive") {
                return Err(ArkxError::Corrupted(msg.trim().to_string()));
            }
            return Err(ArkxError::Backend(msg.trim().to_string()));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_7z_slt(&stdout, path)
    }

    fn extract_inner(
        &self,
        archive: &Path,
        dest: &Path,
        entries: Option<&[String]>,
        password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        std::fs::create_dir_all(dest).map_err(ArkxError::Io)?;

        let threads = num_cpus();
        let mut cmd = Command::new(&self.bin);
        cmd.arg("x")
            .arg(format!("-mmt={}", threads))
            .arg("-bsp1") // progress to stdout
            .arg("-bso1")
            .arg("-y") // yes to all
            .arg(format!("-o{}", dest.display()));

        if let Some(pw) = password {
            cmd.arg(format!("-p{}", pw));
        } else {
            cmd.arg("-p"); // no password prompt
        }

        cmd.arg(archive);

        if let Some(sel) = entries {
            for e in sel {
                cmd.arg(e);
            }
        }

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| ArkxError::Backend(format!("spawn 7z: {}", e)))?;

        // Byte-based 0%→100% progress (like PeaZip/file-roller): 7z's own % with
        // -mmt=N is unreliable by upstream design (lzma2-mt buffering causes
        // 0→97 jumps and 13%/84% stalls), so it is ignored for the bar and only
        // used for the filename label. total = summed Size from `7z l -slt`
        // (filtered for subsets), done = dir_size(dest) minus the baseline
        // sampled before spawn (an archive already in dest cancels out).
        if let Some(cb) = progress {
            let total = self.extract_total(archive, entries);
            // Immediate 0% (total 0 = encrypted headers → pulsing UI, never a fake %).
            cb(ProgressInfo::new("Preparing…".to_string(), 0, total));

            let baseline = dir_size(dest);
            let cb_arc: SharedCallback = Arc::new(Mutex::new(cb));
            let done = Arc::new(AtomicU64::new(0));
            let stop = Arc::new(AtomicBool::new(false));

            // Poll dest every 250ms: the only driver of the bar.
            let cb_poll = cb_arc.clone();
            let done_poll = done.clone();
            let stop_poll = stop.clone();
            let dest_poll = dest.to_path_buf();
            let poll_handle = std::thread::spawn(move || {
                let mut last_emitted = u64::MAX;
                while !stop_poll.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(250));
                    let d = dir_size(&dest_poll).saturating_sub(baseline);
                    done_poll.store(d, Ordering::Relaxed);
                    if d != last_emitted {
                        last_emitted = d;
                        if let Ok(guard) = cb_poll.lock() {
                            guard(ProgressInfo::new("Extracting…".to_string(), d, total));
                        }
                    }
                }
            });

            // Drain stderr on a separate thread to avoid pipe deadlock (64KB)
            let stderr = child.stderr.take();
            let stderr_handle = std::thread::spawn(move || {
                if let Some(mut err) = stderr {
                    let mut buf = vec![0u8; 8192];
                    use std::io::Read;
                    while let Ok(n) = err.read(&mut buf) {
                        if n == 0 { break; }
                    }
                }
            });

            if let Some(stdout) = child.stdout.take() {
                // 7z with -bsp1 writes both filenames and percentages to stdout with '\r'.
                // Percentages are discarded (see above); only the file label is kept.
                use std::io::Read;
                let mut reader = stdout;
                let mut buf = vec![0u8; 8192];
                let mut chunk = Vec::new();
                let mut last_emit = Instant::now();
                let mut last_label = String::new();
                let emit_label = |line: &str, last_emit: &mut Instant, last_label: &mut String| {
                    let label = label_from_7z_line(line);
                    if label.is_empty() {
                        return;
                    }
                    let now_fresh = label != *last_label;
                    if now_fresh || last_emit.elapsed().as_millis() > 500 {
                        *last_emit = Instant::now();
                        *last_label = label.clone();
                        let d = done.load(Ordering::Relaxed);
                        if let Ok(guard) = cb_arc.lock() {
                            guard(ProgressInfo::new(label, d, total));
                        }
                    }
                };
                loop {
                    let n = reader.read(&mut buf).unwrap_or(0);
                    if n == 0 { break; }
                    for &b in &buf[..n] {
                        if b == b'\r' || b == b'\n' {
                            if !chunk.is_empty() {
                                if let Ok(s) = String::from_utf8(std::mem::take(&mut chunk)) {
                                    emit_label(&s, &mut last_emit, &mut last_label);
                                } else {
                                    chunk.clear();
                                }
                            }
                        } else {
                            chunk.push(b);
                        }
                    }
                }
                // Flush any remainder without terminator
                if !chunk.is_empty() {
                    if let Ok(s) = String::from_utf8(chunk) {
                        emit_label(&s, &mut last_emit, &mut last_label);
                    }
                }
            }
            stop.store(true, Ordering::Relaxed);
            let _ = poll_handle.join();
            let status = child.wait().map_err(ArkxError::Io)?;
            let _ = stderr_handle.join();
            let code = status.code().unwrap_or(-1);
            // 7z exit codes: 0=OK, 1=Warning (e.g. Headers Error on solid RAR),
            // 2=Fatal/WrongPassword
            if code == 2 {
                return Err(ArkxError::WrongPassword);
            }
            if !status.success() && code != 1 {
                return Err(ArkxError::Backend(format!("7z extract failed code {:?}", status.code())));
            } else {
                if code == 1 {
                    eprintln!("[7z] warning code 1 (Headers Error on solid archives), treated as success");
                }
                // Final 100% (the only allowed jump: last value → 100).
                if let Ok(guard) = cb_arc.lock() {
                    guard(ProgressInfo::new("Completed".to_string(), 100, 100));
                }
            }
        } else {
            // Senza progress, drena comunque output per evitare deadlock e poi wait
            let stderr_handle = std::thread::spawn({
                let mut stderr = child.stderr.take();
                move || {
                    if let Some(mut err) = stderr.take() {
                        let mut buf = vec![0u8; 8192];
                        use std::io::Read;
                        while let Ok(n) = err.read(&mut buf) {
                            if n == 0 { break; }
                        }
                    }
                }
            });
            // drain stdout if present
            if let Some(mut out) = child.stdout.take() {
                let mut buf = vec![0u8; 8192];
                use std::io::Read;
                while let Ok(n) = out.read(&mut buf) {
                    if n == 0 { break; }
                }
            }
            let status = child.wait().map_err(ArkxError::Io)?;
            let _ = stderr_handle.join();
            let code = status.code().unwrap_or(-1);
            if !status.success() && code != 1 {
                if code == 2 {
                    return Err(ArkxError::WrongPassword);
                }
                return Err(ArkxError::Backend(format!("Extraction failed (code {})", code)));
            }
            if code == 1 {
                eprintln!("[7z] warning code 1 in no-progress path, treated as success");
            }
        }
        Ok(())
    }

    /// Expected total bytes for the bar: summed Size from `7z l -slt`, filtered
    /// to the subset when requested. 0 when listing fails or headers are
    /// encrypted (→ pulsing UI, never a fake %).
    fn extract_total(&self, archive: &Path, filter: Option<&[String]>) -> u64 {
        let info = match self.list(archive) {
            Ok(i) => i,
            Err(_) => return 0,
        };
        match filter {
            None => info.total_size,
            Some(sel) => info
                .entries
                .iter()
                .filter(|e| !e.is_dir && sel.iter().any(|f| paths::entry_matches(&e.path, f)))
                .map(|e| e.size)
                .fold(0u64, |a, b| a.saturating_add(b)),
        }
    }

    fn create_inner(
        &self,
        dest: &Path,
        sources: &[PathBuf],
        level: u8,
        password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(ArkxError::Io)?;
        }
        let threads = num_cpus();
        let fmt = crate::core::detector::detect_format(dest);
        let mut cmd = Command::new(&self.bin);
        cmd.arg("a")
            .arg(format!("-mmt={}", threads))
            .arg("-y");

        // Compression level
        let mx = level.clamp(0, 9);
        cmd.arg(format!("-mx={}", mx));

        // lzma2 for 7z, deflate for zip
        match fmt {
            ArchiveFormat::SevenZip => {
                cmd.arg("-m0=lzma2");
                cmd.arg("-md=64m");
            }
            ArchiveFormat::Zip => {
                cmd.arg("-mm=Deflate");
            }
            _ => {}
        }

        if let Some(pw) = password {
            cmd.arg(format!("-p{}", pw));
            cmd.arg("-mhe=on"); // encrypt headers
        }

        cmd.arg(dest);
        for s in sources {
            cmd.arg(s);
        }

        let Some(cb) = progress else {
            // No progress sink: plain blocking run (CLI text mode drives its
            // own spinner; Ctrl-C kills the whole process tree anyway).
            let output = cmd
                .arg("-bsp0")
                .arg("-bso0")
                .output()
                .map_err(|e| ArkxError::Backend(e.to_string()))?;
            if !output.status.success() {
                let msg = String::from_utf8_lossy(&output.stderr);
                return Err(ArkxError::Backend(msg.to_string()));
            }
            return Ok(());
        };

        // Progress mode (file-manager window): stream 7z's own % like the
        // extract path does. total = input bytes so the bar, the Written
        // counter, speed and ETA stay byte-based and honest.
        cmd.arg("-bsp1").arg("-bso1");
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| ArkxError::Backend(format!("spawn 7z: {}", e)))?;

        let total = create_input_size(sources);
        cb(ProgressInfo::new("Preparing…".to_string(), 0, total));

        // Drain stderr on a separate thread to avoid pipe deadlock (64KB).
        let stderr_handle = std::thread::spawn({
            let mut stderr = child.stderr.take();
            move || {
                if let Some(mut err) = stderr.take() {
                    let mut buf = vec![0u8; 8192];
                    use std::io::Read;
                    while let Ok(n) = err.read(&mut buf) {
                        if n == 0 { break; }
                    }
                }
            }
        });

        let mut last_pct = 0u32;
        let mut cancelled = false;
        if let Some(stdout) = child.stdout.take() {
            use std::io::Read;
            let mut reader = stdout;
            let mut buf = vec![0u8; 8192];
            let mut chunk = Vec::new();
            let mut last_emit = Instant::now();
            let mut last_label = String::new();
            // Returns true when the caller should abort the read loop.
            let feed = |line: &str, last_emit: &mut Instant, last_label: &mut String, last_pct: &mut u32| {
                if self.cancelled() {
                    return true;
                }
                emit_add_line(line, total, last_emit, last_label, last_pct, &cb);
                false
            };
            'read: loop {
                if self.cancelled() {
                    cancelled = true;
                    break 'read;
                }
                let n = reader.read(&mut buf).unwrap_or(0);
                if n == 0 { break; }
                for &b in &buf[..n] {
                    if b == b'\r' || b == b'\n' {
                        if !chunk.is_empty() {
                            if let Ok(s) = String::from_utf8(std::mem::take(&mut chunk)) {
                                if feed(&s, &mut last_emit, &mut last_label, &mut last_pct) {
                                    cancelled = true;
                                    break 'read;
                                }
                            } else {
                                chunk.clear();
                            }
                        }
                    } else {
                        chunk.push(b);
                    }
                }
            }
            if !cancelled && !chunk.is_empty() {
                if let Ok(s) = String::from_utf8(chunk) {
                    if feed(&s, &mut last_emit, &mut last_label, &mut last_pct) {
                        cancelled = true;
                    }
                }
            }
        }

        if cancelled || self.cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stderr_handle.join();
            let _ = std::fs::remove_file(dest);
            return Err(ArkxError::Cancelled);
        }

        let status = child.wait().map_err(ArkxError::Io)?;
        let _ = stderr_handle.join();
        let code = status.code().unwrap_or(-1);
        if !status.success() && code != 1 {
            return Err(ArkxError::Backend(format!("7z create failed (code {})", code)));
        }
        if code == 1 {
            eprintln!("[7z] warning code 1 in create path, treated as success");
        }
        // Final 100% (the only allowed jump: last value → 100).
        cb(ProgressInfo::new("Completed".to_string(), total.max(1), total.max(1)));
        Ok(())
    }
}

/// Total input bytes for the create progress bar (best effort: unreadable
/// files and symlinks count 0 instead of failing the whole job).
fn create_input_size(sources: &[PathBuf]) -> u64 {
    fn file_size(p: &Path) -> u64 {
        if let Ok(m) = std::fs::metadata(p) {
            if m.is_file() {
                return m.len();
            }
            if m.is_dir() {
                let mut total = 0u64;
                if let Ok(walk) = std::fs::read_dir(p) {
                    for entry in walk.flatten() {
                        total = total.saturating_add(file_size(&entry.path()));
                    }
                }
                return total;
            }
        }
        0
    }
    sources.iter().map(|s| file_size(s)).fold(0u64, |a, b| a.saturating_add(b))
}

/// Handle one `7z a -bsp1` output line: forward a throttled, monotonic
/// byte-based progress event. Returns true if anything was emitted.
fn emit_add_line(
    line: &str,
    total: u64,
    last_emit: &mut Instant,
    last_label: &mut String,
    last_pct: &mut u32,
    cb: &dyn Fn(ProgressInfo),
) -> bool {
    let pct = match parse_percent(line) {
        Some(p) => (*last_pct).max(p.min(100)),
        None => *last_pct,
    };
    let label = label_from_7z_add_line(line);
    let show_label = if label.is_empty() { last_label.clone() } else { label.clone() };
    let fresh_label = !label.is_empty() && label != *last_label;
    if pct <= *last_pct && !fresh_label && last_emit.elapsed().as_millis() <= 500 {
        return false;
    }
    *last_pct = pct;
    if !label.is_empty() {
        *last_label = label;
    }
    *last_emit = Instant::now();
    let done = total * pct as u64 / 100;
    let file = if show_label.is_empty() { "Compressing…".to_string() } else { show_label };
    cb(ProgressInfo::new(file, done, total));
    true
}

/// Filename from a `7z a -bsp1` line, dropping the percentage and the
/// operation markers (`Compressing  a.txt`, `+ a.txt`, `12% + a.txt`).
fn label_from_7z_add_line(line: &str) -> String {
    let t = line.trim();
    if t.is_empty() {
        return String::new();
    }
    // Drop a leading percentage token ("12% rest..." → "rest...").
    let mut s = t;
    if let Some(ws) = s.find(char::is_whitespace) {
        let (first, rest) = s.split_at(ws);
        if first.ends_with('%') {
            s = rest.trim();
        }
    } else if s.ends_with('%') {
        return String::new();
    }
    // Status lines without a file.
    for prefix in ["Everything is Ok", "Scanning", "Creating archive"] {
        if s.starts_with(prefix) {
            return String::new();
        }
    }
    // Drive-scan lines ("0M Scan  /path").
    {
        let mut parts = s.splitn(2, char::is_whitespace);
        if let (Some(a), Some(b)) = (parts.next(), parts.next()) {
            if a.ends_with('M')
                && !a[..a.len() - 1].is_empty()
                && a[..a.len() - 1].chars().all(|c| c.is_ascii_digit())
                && b.trim_start().starts_with("Scan")
            {
                return String::new();
            }
        }
    }
    // Operation markers 7z prints while adding.
    for prefix in ["Compressing", "Adding"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.trim();
            break;
        }
    }
    // Per-file markers (`+ path`, `- path`).
    s = s.strip_prefix('+').or_else(|| s.strip_prefix('-')).map(|r| r.trim()).unwrap_or(s);
    s.to_string()
}

/// Matches a " 12%" token. The extract bar ignores 7z's % (unreliable with
/// -mmt=N due to lzma2-mt buffering — see extract()); the create bar uses it
/// clamped and monotonic, as there is no better live signal for `7z a`.
fn parse_percent(s: &str) -> Option<u32> {
    for token in s.split_whitespace() {
        if token.ends_with('%') {
            if let Ok(n) = token.trim_end_matches('%').parse::<u32>() {
                return Some(n);
            }
        }
    }
    None
}

/// Extract the filename from a `7z x -bsp1 -bso1` line, dropping the percentage.
/// Typical shapes: " 12% 3 - path/file", " 12% - path/file", "Extracting  path/file".
/// Returns "" for percentage-only lines (nothing to display).
fn label_from_7z_line(line: &str) -> String {
    let t = line.trim();
    if t.is_empty() {
        return String::new();
    }
    if let Some(pos) = t.find(" - ") {
        return t[pos + 3..].trim().to_string();
    }
    // Drop a leading percentage token ("12% rest..." → "rest...").
    let mut s = t;
    if let Some(ws) = s.find(char::is_whitespace) {
        let (first, rest) = s.split_at(ws);
        if first.ends_with('%') {
            s = rest.trim();
        }
    } else if s.ends_with('%') {
        return String::new();
    }
    // Status lines without a file ("Everything is Ok", "Extracting archive: ...").
    for prefix in ["Everything is Ok", "Extracting archive:"] {
        if s.starts_with(prefix) {
            return String::new();
        }
    }
    s.strip_prefix("Extracting")
        .map(|r| r.trim().to_string())
        .unwrap_or_else(|| s.to_string())
}

fn dir_size(path: &Path) -> u64 {
    // Recursive file-size sum under path (used with a pre-spawn baseline:
    // done = dir_size(dest) - baseline, so an archive already in dest cancels out).
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.is_file() {
            return meta.len();
        }
    }
    let mut total = 0u64;
    if let Ok(walk) = std::fs::read_dir(path) {
        for entry in walk.flatten() {
            let p = entry.path();
            if let Ok(m) = entry.metadata() {
                if m.is_file() {
                    total = total.saturating_add(m.len());
                } else if m.is_dir() {
                    total = total.saturating_add(dir_size(&p));
                }
            }
        }
    }
    total
}

fn parse_7z_slt(output: &str, archive_path: &Path) -> Result<ArchiveInfo> {
    let mut entries: Vec<ArchiveEntry> = Vec::new();
    let mut current: Option<ArchiveEntry> = None;
    let mut total_size = 0u64;
    let mut total_packed = 0u64;
    let mut has_encrypted = false;

    // 7z slt format is block-based:
    // ----------
    // Path = file.txt
    // Size = 123
    // Packed Size = 100
    // Modified = 2024-01-01 12:00:00
    // Attributes = ....
    // CRC = ....
    // Method = LZMA2:12
    // Encrypted = +
    // ...

    let mut in_entries_section = false;
    for line in output.lines() {
        let line = line.trim();
        if !in_entries_section {
            if line == "----------" {
                in_entries_section = true;
                // first entry starts here
                current = Some(ArchiveEntry {
                    path: String::new(),
                    is_dir: false,
                    size: 0,
                    packed_size: 0,
                    modified: None,
                    mode: None,
                    crc32: None,
                    method: None,
                    encrypted: false,
                });
                continue;
            }
            // Ignora header globale prima del primo ----------
            continue;
        }
        if line == "----------" {
            if let Some(entry) = current.take() {
                if !entry.path.is_empty() {
                    total_size += entry.size;
                    total_packed += entry.packed_size;
                    if entry.encrypted { has_encrypted = true; }
                    entries.push(entry);
                }
            }
            current = Some(ArchiveEntry {
                path: String::new(),
                is_dir: false,
                size: 0,
                packed_size: 0,
                modified: None,
                mode: None,
                crc32: None,
                method: None,
                encrypted: false,
            });
            continue;
        }
        if let Some(entry) = current.as_mut() {
            if let Some((k, v)) = line.split_once(" = ") {
                match k {
                    "Path" => {
                // A Path key on an already-named entry starts a new one
                        // (solid blocks without a ---------- separator)
                        if !entry.path.is_empty() {
                            // finalize previous
                            let prev = current.take().unwrap();
                            total_size += prev.size;
                            total_packed += prev.packed_size;
                            if prev.encrypted { has_encrypted = true; }
                            entries.push(prev);
                            // nuova entry
                            current = Some(ArchiveEntry {
                                path: v.to_string(),
                                is_dir: false,
                                size: 0,
                                packed_size: 0,
                                modified: None,
                                mode: None,
                                crc32: None,
                                method: None,
                                encrypted: false,
                            });
                        } else {
                            entry.path = v.to_string();
                        }
                    }
                    "Size" => entry.size = v.parse().unwrap_or(0),
                    "Packed Size" => entry.packed_size = v.parse().unwrap_or(0),
                    "Attributes" => {
                        entry.is_dir = v.contains('D');
                    }
                    "Modified" => {
                        // 2024-01-01 12:00:00
                        entry.modified = parse_7z_date(v);
                    }
                    "CRC" => {
                        if !v.is_empty() { entry.crc32 = Some(v.to_string()); }
                    }
                    "Method" => entry.method = Some(v.to_string()),
                    "Encrypted" => entry.encrypted = v == "+",
                    _ => {}
                }
            }
        }
    }
    if let Some(entry) = current.take() {
        if !entry.path.is_empty() {
            total_size += entry.size;
            total_packed += entry.packed_size;
            if entry.encrypted { has_encrypted = true; }
            entries.push(entry);
        }
    }

    // Se non abbiamo parsed nulla, prova fallback parsing semplice l
    if entries.is_empty() {
                return Err(ArkxError::Corrupted("Cannot parse archive contents (empty or protected)".into()));
    }

    let num_files = entries.iter().filter(|e| !e.is_dir).count();
    let num_dirs = entries.len() - num_files;

    Ok(ArchiveInfo {
        path: archive_path.to_string_lossy().to_string(),
        format: crate::core::detector::detect_format(archive_path).display_name().to_string(),
        entries,
        total_size,
        total_packed,
        num_files,
        num_dirs,
        has_encrypted,
        comment: None,
    })
}

fn parse_7z_date(s: &str) -> Option<chrono::DateTime<chrono::Local>> {
    // 2024-01-15 10:30:00
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .ok()
        .and_then(|ndt| ndt.and_local_timezone(chrono::Local).single())
}

#[cfg(test)]
mod tests {
    use super::{create_input_size, emit_add_line, label_from_7z_add_line, label_from_7z_line, parse_percent};
    use std::time::Instant;
    #[test]
    fn test_pct() {
        assert_eq!(parse_percent(" 12% 3 - file.txt"), Some(12));
        assert_eq!(parse_percent(" 100% - done"), Some(100));
    }
    #[test]
    fn test_label_ignores_percent() {
        assert_eq!(label_from_7z_line(" 12% 3 - dir/file.txt"), "dir/file.txt");
        assert_eq!(label_from_7z_line(" 97% - dir/file.txt"), "dir/file.txt");
        assert_eq!(label_from_7z_line("  7%"), "");
        assert_eq!(label_from_7z_line("Everything is Ok"), "");
        assert_eq!(label_from_7z_line("Extracting  dir/file.txt"), "dir/file.txt");
    }
    #[test]
    fn test_add_label() {
        assert_eq!(label_from_7z_add_line(" 12% + docs/a.txt"), "docs/a.txt");
        assert_eq!(label_from_7z_add_line("Compressing  docs/a.txt"), "docs/a.txt");
        assert_eq!(label_from_7z_add_line("Adding  docs/a.txt"), "docs/a.txt");
        assert_eq!(label_from_7z_add_line("  7%"), "");
        assert_eq!(label_from_7z_add_line("Everything is Ok"), "");
        assert_eq!(label_from_7z_add_line("Scanning the drive:"), "");
        assert_eq!(label_from_7z_add_line("0M Scan  /tmp/x"), "");
    }
    #[test]
    fn test_add_progress_is_monotonic() {
        let total = 1000u64;
        let mut last_emit = Instant::now() - std::time::Duration::from_secs(1);
        let mut last_label = String::new();
        let mut last_pct = 0u32;
        let events = std::cell::RefCell::new(Vec::new());
        let cb = |info: crate::core::archive::ProgressInfo| events.borrow_mut().push(info);
        // 30% then a regressed 12% (mt jitter): bar must not go back.
        emit_add_line(" 30% + a.txt", total, &mut last_emit, &mut last_label, &mut last_pct, &cb);
        // Force throttle expiry for the second line.
        last_emit = Instant::now() - std::time::Duration::from_secs(1);
        emit_add_line(" 12% + b.txt", total, &mut last_emit, &mut last_label, &mut last_pct, &cb);
        assert_eq!(last_pct, 30);
        let events = events.borrow();
        assert!(events.iter().all(|e| e.percent <= 30.1));
        assert_eq!(events.last().unwrap().current, 300);
    }
    #[test]
    fn test_create_input_size() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.path().join("b.txt"), vec![0u8; 50]).unwrap();
        assert_eq!(create_input_size(&[dir.path().to_path_buf()]), 150);
        assert_eq!(create_input_size(&[dir.path().join("missing")]), 0);
    }
}

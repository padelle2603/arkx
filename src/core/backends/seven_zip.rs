use crate::core::archive::{ArchiveBackend, ArchiveEntry, ArchiveInfo, ProgressInfo};
use crate::core::detector::ArchiveFormat;
use crate::core::error::{ArkxError, Result};
use crate::core::paths;
use crate::core::util::{dir_size, effective_threads};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
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

    /// True if the 7z binary exists (absolute path or in PATH).
    /// Without 7z adaptive routing stays on native instead of failing.
    pub fn is_available(&self) -> bool {
        if self.bin.components().count() > 1 {
            return self.bin.is_file();
        }
        std::env::var_os("PATH")
            .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(&self.bin).is_file()))
            .unwrap_or(false)
    }
}

impl crate::core::archive::ArchiveBackend for SevenZipBackend {
    fn supports(&self, fmt: &ArchiveFormat) -> bool {
        // 7z handles everything we know
        !matches!(fmt, ArchiveFormat::Unknown(_))
    }

    fn list(&self, path: &Path) -> Result<ArchiveInfo> {
        self.list_inner(path, None)
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

    fn add(
        &self,
        archive: &Path,
        sources: &[(PathBuf, String)],
        password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        self.add_inner(archive, sources, password, progress)
    }

    fn remove(
        &self,
        archive: &Path,
        entries: &[String],
        password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        self.remove_inner(archive, entries, password, progress)
    }

    fn rename(
        &self,
        archive: &Path,
        old_name: &str,
        new_name: &str,
        password: Option<&str>,
        _progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        let archive = super::native::absolutize(archive);
        let threads = crate::core::util::effective_threads().min(32);
        let mut cmd = Command::new(&self.bin);
        cmd.arg("rn").arg("-y").arg(format!("-mmt={}", threads));
        if let Some(pw) = password {
            cmd.arg(format!("-p{}", pw));
        }
        cmd.arg(archive.as_os_str().to_str().unwrap_or(""));
        cmd.arg(old_name);
        cmd.arg(new_name);
        let output = cmd
            .arg("-bsp0")
            .arg("-bso0")
            .output()
            .map_err(|e| ArkxError::Backend(format!("Cannot run 7z: {}", e)))?;
        if !output.status.success() {
            let msg = String::from_utf8_lossy(&output.stderr);
            return Err(ArkxError::Backend(format!(
                "7z rename failed: {}",
                msg.trim()
            )));
        }
        Ok(())
    }

    fn test(
        &self,
        archive: &Path,
        entries: Option<&[String]>,
        password: Option<&str>,
    ) -> Result<crate::core::archive::TestReport> {
        let archive = super::native::absolutize(archive);
        let mut cmd = Command::new(&self.bin);
        cmd.arg("t").arg("-y").arg("-bsp0").arg("-bso0");
        if let Some(pw) = password {
            cmd.arg(format!("-p{}", pw));
        }
        if let Some(sel) = entries {
            for e in sel {
                cmd.arg(e);
            }
        }
        cmd.arg(archive.as_os_str().to_str().unwrap_or(""));
        let output = cmd
            .output()
            .map_err(|e| ArkxError::Backend(format!("Cannot run 7z: {}", e)))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            let msg = format!("{}{}", stdout, stderr);
            return Err(ArkxError::Backend(format!(
                "7z test failed: {}",
                msg.trim()
            )));
        }
        let archive_path = archive.to_string_lossy().to_string();
        // Parse results from 7z output
        let mut results = Vec::new();
        let mut passed = 0usize;
        let mut failed = 0usize;
        for line in stdout.lines() {
            let line = line.trim();
            if line.starts_with("Everything is Ok") || line.contains("Testing") {
                continue;
            }
            if line.is_empty() {
                continue;
            }
            // Detect errors in 7z output
            if is_password_error(line)
                || line.contains("Cannot open")
                || line.contains("CRC")
                || line.starts_with("Can't open file")
                || line.contains("Error")
            {
                failed += 1;
                results.push(crate::core::archive::TestResult {
                    entry: line.to_string(),
                    is_dir: false,
                    crc32_expected: None,
                    crc32_actual: None,
                    passed: false,
                });
            }
        }
        if failed == 0 {
            passed = 1; // All OK
            results.push(crate::core::archive::TestResult {
                entry: "(all entries)".to_string(),
                is_dir: false,
                crc32_expected: None,
                crc32_actual: None,
                passed: true,
            });
        }
        Ok(crate::core::archive::TestReport {
            archive: archive_path,
            results,
            passed,
            failed,
        })
    }
}

/// True if raw 7z output indicates a password problem. Case-insensitive and
/// tolerant of wording variants across 7z releases/locales.
fn is_password_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    [
        "wrong password",
        "enter password",
        "password is incorrect",
        "password is not correct",
        "incorrect password",
        "can not open encrypted",
        "cannot open encrypted",
        "can't open encrypted",
        "cant open encrypted",
        "password?",
    ]
    .iter()
    .any(|pat| m.contains(pat))
}

/// Strip 7z banner/copyright/separator noise, keeping only substantial lines
/// so user-facing errors stop leaking the raw `7-Zip 26.xx x64 (c) …` header.
fn clean_7z_msg(raw: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    for line in raw.lines() {
        let t = line.trim();
        if t.is_empty()
            || t.starts_with("----------")
            || t.contains("Igor Pavlov")
            || t.starts_with("p7zip Version")
            || t.contains("Compilation date")
        {
            continue;
        }
        kept.push(t.to_string());
    }
    kept.join("\n")
}

/// Extract the referenced volume name from a `Missing volume` 7z message.
fn missing_volume(msg: &str) -> Option<String> {
    for line in msg.lines() {
        let t = line.trim();
        if let Some(idx) = t.find("Missing volume") {
            let name = t[idx + "Missing volume".len()..]
                .trim()
                .trim_matches(|c| c == ':' || c == ' ' || c == '\'' || c == '"')
                .trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Short user-facing message for a multi-volume archive with a missing part.
const MISSING_VOLUME_PREFIX: &str = "Missing volume:";

fn missing_volume_error(vol: &str) -> ArkxError {
    ArkxError::Corrupted(format!(
        "{MISSING_VOLUME_PREFIX} {vol} — multi-volume archive, all parts must be in the same folder"
    ))
}

/// True for the synthetic multi-volume error above: used to stop the fallback
/// chain (bsdtar cannot help an archive whose part is missing).
pub fn is_missing_volume_error(e: &ArkxError) -> bool {
    matches!(e, ArkxError::Corrupted(m) if m.starts_with(MISSING_VOLUME_PREFIX))
}

/// Classify raw 7z output into a typed error. Order matters: a `Missing volume`
/// wins over the misleading `Wrong password?` that 7z emits for data continuing
/// in the absent part, so a missing volume never re-prompts for a password.
fn classify_7z_error(msg: &str) -> ArkxError {
    if let Some(vol) = missing_volume(msg) {
        return missing_volume_error(&vol);
    }
    if is_password_error(msg) {
        return ArkxError::WrongPassword;
    }
    if msg.contains("Can not open file as archive") || msg.contains("Is not archive") {
        return ArkxError::Corrupted(clean_7z_msg(msg));
    }
    ArkxError::Backend(clean_7z_msg(msg))
}

fn which_7z() -> PathBuf {
    use std::sync::OnceLock;
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE
        .get_or_init(|| {
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
        })
        .clone()
}

impl SevenZipBackend {
    fn list_inner(&self, path: &Path, password: Option<&str>) -> Result<ArchiveInfo> {
        // `7z l -slt` gives machine-parsable technical output
        // (-slt: technical info, -sccUTF-8: charset, -bsp0 -bso1: quiet progress)
        let mut cmd = Command::new(&self.bin);
        cmd.args(["l", "-slt", "-sccUTF-8", "-bsp0", "-bso1"]);
        if let Some(pw) = password {
            cmd.arg(format!("-p{}", pw));
        }
        let output = cmd
            .arg(path)
            .output()
            .map_err(|e| ArkxError::Backend(format!("Cannot run 7z: {}", e)))?;

        if !output.status.success() && output.status.code() != Some(1) {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let msg = format!("{}{}", stdout, stderr);
            if let Some(vol) = missing_volume(&msg) {
                // Multi-volume archive with a missing part: 7z still emits the
                // full `-slt` index, so open the archive with the entries it has
                // and let extraction of uncontained files fail individually.
                // Checked before the password hint: 7z appends a misleading
                // "Wrong password?" when the payload continues in the absent part.
                eprintln!(
                    "[7z] list: missing volume \"{}\", showing partial contents",
                    vol
                );
                return parse_7z_slt(&stdout, path).map_err(|_| missing_volume_error(&vol));
            }
            let err = classify_7z_error(&msg);
            if matches!(err, ArkxError::Backend(_)) {
                // Unmapped: log the full raw output so the exact 7z message can
                // be identified from a terminal run and mapped precisely.
                eprintln!(
                    "[7z] list failed (unmapped, code {:?}); raw output:\n{}",
                    output.status.code(),
                    msg.trim()
                );
            }
            return Err(err);
        }
        if output.status.code() == Some(1) {
            // 7z exit code 1 = warning (non-fatal, e.g. trailing data after the
            // payload): the listing is still complete and usable.
            eprintln!("[7z] list warning code 1, treated as success");
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_7z_slt(&stdout, path)
    }

    /// List with a password, for header-encrypted archives that `list` alone
    /// cannot open. Wrong credentials surface as `WrongPassword`.
    pub fn list_with_password(&self, path: &Path, password: &str) -> Result<ArchiveInfo> {
        self.list_inner(path, Some(password))
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

        let threads = effective_threads().min(32);
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

        let mut child = cmd
            .spawn()
            .map_err(|e| ArkxError::Backend(format!("spawn 7z: {}", e)))?;

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
            let stderr_arc = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let stderr_clone = stderr_arc.clone();
            let stderr_handle = std::thread::spawn(move || {
                if let Some(err) = stderr {
                    let mut reader = err;
                    let mut buf = vec![0u8; 8192];
                    let mut collected = Vec::new();
                    loop {
                        match std::io::Read::read(&mut reader, &mut buf) {
                            Ok(0) => break,
                            Ok(n) => collected.extend_from_slice(&buf[..n]),
                            Err(_) => break,
                        }
                    }
                    if let Ok(mut guard) = stderr_clone.lock() {
                        *guard = collected;
                    }
                }
            });

            if let Some(stdout) = child.stdout.take() {
                // 7z with -bsp1 writes both filenames and percentages to stdout with '\r'.
                // Percentages are discarded (see above); only the file label is kept.
                let mut last_emit = Instant::now();
                let mut last_label = String::new();
                crate::core::util::read_lines_until(stdout, |line| {
                    let label = label_from_7z_line(line);
                    if label.is_empty() {
                        return false;
                    }
                    let now_fresh = label != last_label;
                    if now_fresh || last_emit.elapsed().as_millis() > 500 {
                        last_emit = Instant::now();
                        last_label = label.clone();
                        let d = done.load(Ordering::Relaxed);
                        if let Ok(guard) = cb_arc.lock() {
                            guard(ProgressInfo::new(label, d, total));
                        }
                    }
                    false
                });
            }
            stop.store(true, Ordering::Relaxed);
            let _ = poll_handle.join();
            let status = child.wait().map_err(ArkxError::Io)?;
            let _ = stderr_handle.join();
            let code = status.code().unwrap_or(-1);
            // 7z exit codes: 0=OK, 1=Warning (e.g. Headers Error on solid RAR),
            // 2=Fatal/WrongPassword
            if code == 2 {
                let stderr_text = match stderr_arc.lock() {
                    Ok(data) => String::from_utf8_lossy(&data).to_string(),
                    Err(_) => String::new(),
                };
                let err = classify_7z_error(&stderr_text);
                if matches!(err, ArkxError::WrongPassword) {
                    eprintln!(
                        "[7z] extract code 2 (password error); raw output:\n{}",
                        stderr_text.trim()
                    );
                } else if matches!(err, ArkxError::Backend(_)) {
                    eprintln!(
                        "[7z] extract failed code 2 (unmapped); raw output:\n{}",
                        stderr_text.trim()
                    );
                }
                return Err(err);
            }
            if !status.success() && code != 1 {
                return Err(ArkxError::Backend(format!(
                    "7z extract failed code {:?}",
                    status.code()
                )));
            } else {
                if code == 1 {
                    eprintln!(
                        "[7z] warning code 1 (Headers Error on solid archives), treated as success"
                    );
                }
                // Final 100% (the only allowed jump: last value → 100).
                if let Ok(guard) = cb_arc.lock() {
                    guard(ProgressInfo::new("Completed".to_string(), 100, 100));
                }
            }
        } else {
            // Without progress, still drain output to avoid deadlock then wait
            let stderr = child.stderr.take();
            let stderr_data = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let stderr_clone = stderr_data.clone();
            let stderr_handle = std::thread::spawn(move || {
                if let Some(err) = stderr {
                    let mut reader = err;
                    let mut buf = vec![0u8; 8192];
                    let mut collected = Vec::new();
                    loop {
                        match std::io::Read::read(&mut reader, &mut buf) {
                            Ok(0) => break,
                            Ok(n) => collected.extend_from_slice(&buf[..n]),
                            Err(_) => break,
                        }
                    }
                    if let Ok(mut guard) = stderr_clone.lock() {
                        *guard = collected;
                    }
                }
            });
            // drain stdout if present
            if let Some(out) = child.stdout.take() {
                crate::core::util::drain_reader(out);
            }
            let status = child.wait().map_err(ArkxError::Io)?;
            let _ = stderr_handle.join();
            let code = status.code().unwrap_or(-1);
            if !status.success() && code != 1 {
                if code == 2 {
                    let stderr_text = match stderr_data.lock() {
                        Ok(data) => String::from_utf8_lossy(&data).to_string(),
                        Err(_) => String::new(),
                    };
                    let err = classify_7z_error(&stderr_text);
                    if matches!(err, ArkxError::WrongPassword) {
                        eprintln!(
                            "[7z] extract code 2 (password error); raw output:\n{}",
                            stderr_text.trim()
                        );
                    } else if matches!(err, ArkxError::Backend(_)) {
                        eprintln!(
                            "[7z] extract failed code 2 (unmapped); raw output:\n{}",
                            stderr_text.trim()
                        );
                    }
                    return Err(err);
                }
                return Err(ArkxError::Backend(format!(
                    "Extraction failed (code {})",
                    code
                )));
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
        let threads = effective_threads().min(32);
        let fmt = crate::core::detector::detect_format(dest);
        let mut cmd = Command::new(&self.bin);
        cmd.arg("a").arg(format!("-mmt={}", threads)).arg("-y");

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
        let mut child = cmd
            .spawn()
            .map_err(|e| ArkxError::Backend(format!("spawn 7z: {}", e)))?;

        let sizes = crate::core::util::input_file_sizes(sources);
        // Total = the absolute-keyed entries of the map already walked above.
        let total: u64 = sizes
            .iter()
            .filter(|(k, _)| std::path::Path::new(k.as_str()).is_absolute())
            .map(|(_, v)| *v)
            .sum();
        cb(ProgressInfo::new("Preparing…".to_string(), 0, total));

        // Drain stderr on a separate thread to avoid pipe deadlock (64KB).
        let stderr_handle = std::thread::spawn({
            let mut stderr = child.stderr.take();
            move || {
                if let Some(err) = stderr.take() {
                    crate::core::util::drain_reader(err);
                }
            }
        });

        let cb_shared: SharedCallback = std::sync::Arc::new(Mutex::new(cb));
        let tracker: std::sync::Arc<std::sync::Mutex<CreateProgress>> =
            std::sync::Arc::new(std::sync::Mutex::new(CreateProgress::new(total, sizes)));
        let mut cancelled = false;

        // Independent poll of bytes READ by 7z (/proc/PID/io): MB granularity
        // from the first seconds even inside a single 10GB file,
        // where neither 7z's integer % nor the per-file floor would move.
        // Best-effort (Linux only): if unreadable, % + floor applies.
        let pid = child.id();
        let io_stop = std::sync::Arc::new(AtomicBool::new(false));
        let io_handle = std::thread::spawn({
            let tracker = tracker.clone();
            let cb_shared = cb_shared.clone();
            let stop = io_stop.clone();
            move || {
                let mut baseline: Option<u64> = None;
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(250));
                    let io_done = match proc_read_bytes(pid) {
                        Some(r) => {
                            let b = *baseline.get_or_insert(r);
                            Some(r.saturating_sub(b))
                        }
                        None => None,
                    };
                    if let (Some(done), Ok(mut t), Ok(g)) =
                        (io_done, tracker.lock(), cb_shared.lock())
                    {
                        t.feed_io(done, &*g);
                    }
                }
            }
        });

        if let Some(stdout) = child.stdout.take() {
            if self.cancelled() {
                cancelled = true;
            } else {
                // Returns true when the caller should abort the read loop.
                crate::core::util::read_lines_until(stdout, |line| {
                    if self.cancelled() {
                        cancelled = true;
                        return true;
                    }
                    if let (Ok(mut t), Ok(g)) = (tracker.lock(), cb_shared.lock()) {
                        t.feed(line, &*g);
                    }
                    false
                });
            }
        }
        io_stop.store(true, Ordering::Relaxed);
        let _ = io_handle.join();

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
            return Err(ArkxError::Backend(format!(
                "7z create failed (code {})",
                code
            )));
        }
        if code == 1 {
            eprintln!("[7z] warning code 1 in create path, treated as success");
        }
        // Final 100% (the only allowed jump: last value → 100).
        if let Ok(g) = cb_shared.lock() {
            g(ProgressInfo::new(
                "Completed".to_string(),
                total.max(1),
                total.max(1),
            ));
        }
        Ok(())
    }

    /// Add files to an existing archive via `7z a` (update mode, preserves all
    /// existing entries; same-name entries are replaced). Entry names are
    /// controlled via a staging dir: sources are copied to `tmp/<entry path>`
    /// and passed to 7z as cwd-relative paths, so the stored name is exactly
    /// the requested one. A root-only add (no `/` in any entry name, sources
    /// sharing one common parent) skips the staging copy entirely.
    fn add_inner(
        &self,
        archive: &Path,
        sources: &[(PathBuf, String)],
        password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        if !archive.exists() {
            return Err(ArkxError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("archive not found: {}", archive.display()),
            )));
        }
        // Run 7z from the staging dir / common parent: the archive must not be
        // resolved relatively, so absolutize it upfront.
        let archive = super::native::absolutize(archive);
        let fmt = crate::core::detector::detect_format(&archive);
        let threads = effective_threads().min(32);

        // Fast path: no staging at all when every entry is at the archive root and
        // the sources share one common parent (cwd = parent, bare names).
        // `parent()` of a bare name like "one.txt" is the *empty* path, which
        // `Command::current_dir` would reject: normalize it to ".". Any mix of
        // parents falls back to the staging dir (safe, no per-case logic).
        let names_are_root = sources.iter().all(|(_, n)| !n.contains('/'));
        let common_parent = if names_are_root {
            let parents: std::collections::HashSet<Option<&std::path::Path>> = sources
                .iter()
                .map(|(s, _)| {
                    let p = s.parent().unwrap_or(std::path::Path::new("."));
                    Some(if p.as_os_str().is_empty() {
                        std::path::Path::new(".")
                    } else {
                        p
                    })
                })
                .collect();
            if parents.len() == 1 {
                parents
                    .into_iter()
                    .next()
                    .flatten()
                    .map(|p| p.to_path_buf())
            } else {
                None
            }
        } else {
            None
        };

        let mut cmd = Command::new(&self.bin);
        cmd.arg("a").arg(format!("-mmt={}", threads)).arg("-y");
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
        }
        cmd.arg(archive.as_os_str().to_str().unwrap_or(""));

        let mut input_paths: Vec<PathBuf> = Vec::new();
        // Kept alive until the end of the function (Drop cleans the temp dir).
        let _staging = if let Some(parent) = common_parent {
            for (s, name) in sources {
                input_paths.push(s.clone());
                cmd.arg(name);
            }
            cmd.current_dir(&parent);
            None
        } else {
            let dir = Staging::new()?;
            for (s, name) in sources {
                let dest = dir.path().join(name);
                let _ = std::fs::create_dir_all(dest.parent().unwrap_or(dir.path()));
                copy_out(s, &dest)?;
                input_paths.push(dest.clone());
                cmd.arg(name);
            }
            cmd.current_dir(dir.path());
            Some(dir)
        };

        let total: u64 = crate::core::util::input_file_sizes(&input_paths)
            .iter()
            .filter(|(k, _)| std::path::Path::new(k.as_str()).is_absolute())
            .map(|(_, v)| *v)
            .sum();

        let Some(cb) = progress else {
            let output = cmd
                .arg("-bsp0")
                .arg("-bso0")
                .output()
                .map_err(|e| ArkxError::Backend(e.to_string()))?;
            if !output.status.success() {
                let msg = String::from_utf8_lossy(&output.stderr);
                return Err(ArkxError::Backend(format!("7z add failed: {}", msg.trim())));
            }
            return Ok(());
        };

        cb(ProgressInfo::new("Preparing…".to_string(), 0, total.max(1)));
        cmd.arg("-bsp1").arg("-bso1");
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| ArkxError::Backend(e.to_string()))?;

        let stderr_handle = std::thread::spawn({
            let mut stderr = child.stderr.take();
            move || {
                if let Some(err) = stderr.take() {
                    crate::core::util::drain_reader(err);
                }
            }
        });

        let cb_shared: SharedCallback = Arc::new(Mutex::new(cb));
        let tracker: Arc<Mutex<CreateProgress>> = Arc::new(Mutex::new(CreateProgress::new(
            total,
            crate::core::util::input_file_sizes(&input_paths),
        )));
        let mut cancelled = false;
        if let Some(stdout) = child.stdout.take() {
            if self.cancelled() {
                cancelled = true;
            } else {
                crate::core::util::read_lines_until(stdout, |line| {
                    if self.cancelled() {
                        cancelled = true;
                        return true;
                    }
                    if let (Ok(mut t), Ok(g)) = (tracker.lock(), cb_shared.lock()) {
                        t.feed(line, &*g);
                    }
                    false
                });
            }
        }
        if cancelled {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stderr_handle.join();
            return Err(ArkxError::Cancelled);
        }
        let status = child.wait().map_err(ArkxError::Io)?;
        let _ = stderr_handle.join();
        let code = status.code().unwrap_or(-1);
        if !status.success() && code != 1 {
            return Err(ArkxError::Backend(format!("7z add failed (code {})", code)));
        }
        if code == 1 {
            eprintln!("[7z] warning code 1 in add path, treated as success");
        }
        // Final 100% (the only allowed jump: last value → 100).
        if let Ok(g) = cb_shared.lock() {
            g(ProgressInfo::new(
                "Completed".to_string(),
                total.max(1),
                total.max(1),
            ));
        }
        Ok(())
    }

    /// Remove entries from an existing archive via `7z d` (7z and plain tar).
    /// No byte-based progress (7z d reports none): a start / done pair only.
    fn remove_inner(
        &self,
        archive: &Path,
        entries: &[String],
        password: Option<&str>,
        progress: Option<Box<dyn Fn(ProgressInfo) + Send>>,
    ) -> Result<()> {
        if !archive.exists() {
            return Err(ArkxError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("archive not found: {}", archive.display()),
            )));
        }
        if entries.is_empty() {
            return Err(ArkxError::Backend("no entries to remove".into()));
        }
        let archive = super::native::absolutize(archive);
        let threads = effective_threads().min(32);

        let mut cmd = Command::new(&self.bin);
        cmd.arg("d").arg("-y").arg(format!("-mmt={}", threads));
        if let Some(pw) = password {
            cmd.arg(format!("-p{}", pw));
        }
        cmd.arg(archive.as_os_str().to_str().unwrap_or(""));
        for name in entries {
            cmd.arg(name);
        }

        if let Some(cb) = &progress {
            cb(ProgressInfo::new("Removing…".to_string(), 0, 1));
        }
        let output = cmd
            .arg("-bsp0")
            .arg("-bso0")
            .output()
            .map_err(|e| ArkxError::Backend(e.to_string()))?;
        if let Some(cb) = &progress {
            cb(ProgressInfo::new("Completed".to_string(), 1, 1));
        }
        if !output.status.success() {
            let msg = String::from_utf8_lossy(&output.stderr);
            return Err(ArkxError::Backend(format!(
                "7z remove failed: {}",
                msg.trim()
            )));
        }
        Ok(())
    }
}

/// Temp dir used to stage added files at their archive-relative paths.
/// Self-cleaning: removed on drop, so cleanup happens on every path
/// (success, error, cancel). No extra crate (tempfile is test-only).
struct Staging(PathBuf);

impl Staging {
    fn new() -> Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let base =
            std::env::temp_dir().join(format!("arkx-add-{}-{:09}", std::process::id(), nanos));
        std::fs::create_dir_all(&base).map_err(ArkxError::Io)?;
        Ok(Staging(base))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Copy a file or an entire directory tree to `dest` (for a dir, `dest` is the
/// new root). Broken symlinks/special files are skipped silently, mirroring
/// the create path's tolerance.
fn copy_out(src: &Path, dest: &Path) -> Result<()> {
    if src.is_file() {
        std::fs::copy(src, dest).map_err(ArkxError::Io)?;
    } else if src.is_dir() {
        std::fs::create_dir_all(dest).map_err(ArkxError::Io)?;
        for entry in std::fs::read_dir(src).map_err(ArkxError::Io)? {
            let entry = entry.map_err(ArkxError::Io)?;
            copy_out(&entry.path(), &dest.join(entry.file_name()))?;
        }
    }
    Ok(())
}

/// Byte-based progress tracker for `7z a -bsp1` (create path).
///
/// 7z's % with -mmt stalls on huge files (lzma2-mt buffering):
/// on top of that, this tracker keeps a *monotonic floor* equal to
/// the sum of already completed files (when 7z moves to the next
/// `+ file` marker, the previous one is done). So the bar never stays
/// stuck at 0% for tens of minutes on multi-GB archives.
/// If nothing new arrives for >2s, it still re-emits the state
/// (keepalive: the window stays alive instead of looking dead).
struct CreateProgress {
    total: u64,
    sizes: std::collections::HashMap<String, u64>,
    completed: u64,
    current: String,
    last_pct: u32,
    last_io: u64,
    last_done: u64,
    last_emit: Instant,
    last_label: String,
}

impl CreateProgress {
    fn new(total: u64, sizes: std::collections::HashMap<String, u64>) -> Self {
        Self {
            total,
            sizes,
            completed: 0,
            current: String::new(),
            last_pct: 0,
            last_io: 0,
            last_done: 0,
            last_emit: Instant::now(),
            last_label: String::new(),
        }
    }

    /// Current estimate: max(7z %, completed-files floor, bytes read).
    fn done(&self) -> u64 {
        let from_pct = self.total * self.last_pct as u64 / 100;
        from_pct
            .max(self.completed.min(self.total))
            .max(self.last_io.min(self.total))
            .min(self.total)
    }

    fn display_label(&self) -> String {
        if self.last_label.is_empty() {
            "Compressing…".to_string()
        } else {
            self.last_label.clone()
        }
    }

    fn emit(&mut self, cb: &dyn Fn(ProgressInfo)) {
        let done = self.done();
        self.last_done = done;
        self.last_emit = Instant::now();
        cb(ProgressInfo::new(self.display_label(), done, self.total));
    }

    fn keepalive_due(&self) -> bool {
        self.last_emit.elapsed().as_millis() > 2000
    }

    /// Handle one `7z a -bsp1` output line. Returns true if emitted.
    fn feed(&mut self, line: &str, cb: &dyn Fn(ProgressInfo)) -> bool {
        if let Some(p) = parse_percent(line) {
            self.last_pct = self.last_pct.max(p.min(100));
        }
        let label = label_from_7z_add_line(line);
        if !label.is_empty() && label != self.current {
            // 7z moved on: the previous file is completed.
            if !self.current.is_empty() {
                self.completed = self
                    .completed
                    .saturating_add(self.sizes.get(&self.current).copied().unwrap_or(0));
            }
            self.current = label.clone();
        }
        let mut fresh = false;
        if !label.is_empty() && label != self.last_label {
            self.last_label = label;
            fresh = true;
        }
        if self.done() <= self.last_done && !fresh && !self.keepalive_due() {
            return false;
        }
        self.emit(cb);
        true
    }

    /// Update from bytes read by the 7z process (/proc poll). Returns true
    /// if emitted. `io_done` is already relative to the startup baseline.
    fn feed_io(&mut self, io_done: u64, cb: &dyn Fn(ProgressInfo)) -> bool {
        let capped = io_done.min(self.total);
        if capped > self.last_io {
            self.last_io = capped;
        }
        if self.done() <= self.last_done && !self.keepalive_due() {
            return false;
        }
        self.emit(cb);
        true
    }
}

/// Bytes read by the process (Linux `/proc/PID/io`): MB granularity of
/// creation progress even inside a single huge file.
/// `None` outside Linux or when the process ended (fallback to % + floor).
fn proc_read_bytes(pid: u32) -> Option<u64> {
    let text = std::fs::read_to_string(format!("/proc/{}/io", pid)).ok()?;
    parse_proc_io(&text)
}

fn parse_proc_io(s: &str) -> Option<u64> {
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("read_bytes:") {
            return rest.trim().parse().ok();
        }
    }
    None
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
    for prefix in [
        "Everything is Ok",
        "Scanning",
        "Creating archive",
        "Add new data to archive",
        "7-Zip",
        "64-bit",
        "Files read",
        "Archive size",
    ] {
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
    s = s
        .strip_prefix('+')
        .or_else(|| s.strip_prefix('-'))
        .map(|r| r.trim())
        .unwrap_or(s);
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
    let mut comment: Option<String> = None;
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
            // Archive-level comment lives in the header block.
            if line.starts_with("Comment = ") {
                let v = line.trim_start_matches("Comment = ").trim().to_string();
                if !v.is_empty() {
                    comment = Some(v);
                }
            }
            // Ignore global header before the first ----------
            continue;
        }
        if line == "----------" {
            if let Some(entry) = current.take() {
                if !entry.path.is_empty() {
                    total_size += entry.size;
                    total_packed += entry.packed_size;
                    if entry.encrypted {
                        has_encrypted = true;
                    }
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
                            if prev.encrypted {
                                has_encrypted = true;
                            }
                            entries.push(prev);
                            // new entry
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
                        if !v.is_empty() {
                            entry.crc32 = Some(v.to_string());
                        }
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
            if entry.encrypted {
                has_encrypted = true;
            }
            entries.push(entry);
        }
    }

    // If we parsed nothing, try simple fallback parsing
    if entries.is_empty() {
        return Err(ArkxError::Corrupted(
            "Cannot parse archive contents (empty or protected)".into(),
        ));
    }

    let num_files = entries.iter().filter(|e| !e.is_dir).count();
    let num_dirs = entries.len() - num_files;

    Ok(ArchiveInfo {
        path: archive_path.to_string_lossy().to_string(),
        format: crate::core::detector::detect_format(archive_path)
            .display_name()
            .to_string(),
        entries,
        total_size,
        total_packed,
        num_files,
        num_dirs,
        has_encrypted,
        comment,
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
    use super::{
        classify_7z_error, clean_7z_msg, is_missing_volume_error, is_password_error,
        label_from_7z_add_line, label_from_7z_line, missing_volume, parse_percent, parse_proc_io,
        CreateProgress,
    };
    use crate::core::error::ArkxError;
    use std::collections::HashMap;

    #[test]
    fn missing_volume_error_is_detected_but_not_other_corrupted() {
        assert!(is_missing_volume_error(&super::missing_volume_error(
            "p2.rar"
        )));
        assert!(!is_missing_volume_error(&ArkxError::WrongPassword));
        assert!(!is_missing_volume_error(&ArkxError::Corrupted(
            "Can not open file as archive".into()
        )));
    }

    #[test]
    fn missing_volume_beats_wrong_password_hint() {
        // Real 7z stderr for an encrypted multi-volume RAR whose next part is
        // absent: 7z appends a misleading "Wrong password?".
        let raw = "ERRORS:\nMissing volume : f146509ad88b71096240ddff9f13a650.rar\n\nERROR: Data Error in encrypted file. Wrong password? : TENOKE/game.iso\n";
        match classify_7z_error(raw) {
            ArkxError::Corrupted(m) => assert!(
                m.contains("f146509ad88b71096240ddff9f13a650.rar"),
                "volume name lost: {m}"
            ),
            other => panic!("expected Corrupted (missing volume), got {other:?}"),
        }
    }

    #[test]
    fn missing_volume_extracts_name() {
        assert_eq!(
            missing_volume(
                "ERROR = Missing volume : f146509ad88b71096240ddff9f13a650.rar\nWARNINGS:"
            ),
            Some("f146509ad88b71096240ddff9f13a650.rar".to_string())
        );
        assert_eq!(
            missing_volume("Missing volume : part.rar"),
            Some("part.rar".to_string())
        );
        assert_eq!(missing_volume("all good here"), None);
        assert!(
            matches!(super::missing_volume_error("x.rar"), ArkxError::Corrupted(m) if m.contains("x.rar"))
        );
    }

    #[test]
    fn is_password_error_case_insensitive() {
        assert!(is_password_error("ERROR: Wrong password"));
        assert!(is_password_error(
            "Can not open encrypted archive. Password?"
        ));
        assert!(is_password_error("enter password:"));
        assert!(is_password_error("Password is not correct"));
        assert!(!is_password_error("7-Zip (c) Igor Pavlov : file not found"));
        assert!(!is_password_error("Is not archive"));
    }

    #[test]
    fn clean_7z_msg_strips_banner() {
        let raw = concat!(
            "7-Zip 26.03 x64 (c) 1999-2024 Igor Pavlov : 2025-06-01\n",
            "p7zip Version 26.03\n",
            "Compilation date: 2025-06-01\n",
            "Scanning the drive for archives:\n",
            "----------\n",
            "file.rar\n",
            "ERROR: Can not open file as archive\n",
        );
        let clean = clean_7z_msg(raw);
        assert!(!clean.contains("Igor Pavlov"), "banner leaked: {clean}");
        assert!(!clean.contains("----------"), "separator leaked: {clean}");
        assert!(clean.contains("file.rar"), "real content lost: {clean}");
        assert!(clean.contains("ERROR:"), "error lost: {clean}");
    }
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
        assert_eq!(
            label_from_7z_line("Extracting  dir/file.txt"),
            "dir/file.txt"
        );
    }
    #[test]
    fn test_add_label() {
        assert_eq!(label_from_7z_add_line(" 12% + docs/a.txt"), "docs/a.txt");
        assert_eq!(
            label_from_7z_add_line("Compressing  docs/a.txt"),
            "docs/a.txt"
        );
        assert_eq!(label_from_7z_add_line("Adding  docs/a.txt"), "docs/a.txt");
        assert_eq!(label_from_7z_add_line("  7%"), "");
        assert_eq!(label_from_7z_add_line("Everything is Ok"), "");
        assert_eq!(label_from_7z_add_line("Scanning the drive:"), "");
        assert_eq!(label_from_7z_add_line("0M Scan  /tmp/x"), "");
        assert_eq!(
            label_from_7z_add_line("Add new data to archive: 24 folders, 318 files,"),
            ""
        );
        assert_eq!(label_from_7z_add_line("Add new data to archive:"), "");
        assert_eq!(
            label_from_7z_add_line(
                "7-Zip 26.02 (x64) : Copyright (c) 1999-2026 Igor Pavlov : 2026-06-25"
            ),
            ""
        );
        assert_eq!(
            label_from_7z_add_line("64-bit locale=it_IT.UTF-8 Threads:12 OPEN_MAX:1048576, ASM"),
            ""
        );
        assert_eq!(label_from_7z_add_line("Files read from disk: 60"), "");
        assert_eq!(
            label_from_7z_add_line("Archive size: 984497 bytes (962 KiB)"),
            ""
        );
    }
    #[test]
    fn test_add_progress_is_monotonic() {
        let total = 1000u64;
        let mut sizes = HashMap::new();
        sizes.insert("a.txt".to_string(), 300u64);
        sizes.insert("b.txt".to_string(), 700u64);
        let mut tracker = CreateProgress::new(total, sizes);
        // Backdate to skip the throttle in the first feed.
        tracker.last_emit = std::time::Instant::now() - std::time::Duration::from_secs(5);
        let events = std::cell::RefCell::new(Vec::new());
        let cb = |info: crate::core::archive::ProgressInfo| events.borrow_mut().push(info);
        // 30% then a regressed 12% (mt jitter): bar must not go back.
        tracker.feed(" 30% + a.txt", &cb);
        tracker.last_emit = std::time::Instant::now() - std::time::Duration::from_secs(5);
        tracker.feed(" 12% + b.txt", &cb);
        assert_eq!(tracker.last_pct, 30);
        let events = events.borrow();
        assert!(events.iter().all(|e| e.percent <= 30.1));
        // Floor: a.txt (300B) completed when moving to b.txt.
        assert_eq!(events.last().unwrap().current, 300);
    }
    #[test]
    fn test_add_progress_floor_rises_per_file() {
        // 318 files, 7z % stuck at 0: the floor must still rise.
        let total = 1000u64;
        let mut sizes = HashMap::new();
        sizes.insert("a.txt".to_string(), 400u64);
        sizes.insert("b.txt".to_string(), 600u64);
        let mut tracker = CreateProgress::new(total, sizes);
        tracker.last_emit = std::time::Instant::now() - std::time::Duration::from_secs(5);
        let events = std::cell::RefCell::new(Vec::new());
        let cb = |info: crate::core::archive::ProgressInfo| events.borrow_mut().push(info);
        tracker.feed("  0% + a.txt", &cb);
        tracker.last_emit = std::time::Instant::now() - std::time::Duration::from_secs(5);
        tracker.feed("  0% + b.txt", &cb);
        let events = events.borrow();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].current, 400);
    }
    #[test]
    fn test_parse_proc_io() {
        let sample = "rchar: 123\nwchar: 45\nread_bytes: 67890\nwrite_bytes: 11\n";
        assert_eq!(parse_proc_io(sample), Some(67890));
        assert_eq!(parse_proc_io("rchar: 1\n"), None);
        assert_eq!(parse_proc_io(""), None);
    }
    #[test]
    fn test_feed_io_moves_inside_huge_file() {
        // A single 10GB file, 7z % at 0, no file change:
        // bytes read must still move the bar (0.xx%).
        let total = 10_000u64;
        let mut sizes = HashMap::new();
        sizes.insert("huge.bin".to_string(), 10_000u64);
        let mut tracker = CreateProgress::new(total, sizes);
        tracker.last_emit = std::time::Instant::now() - std::time::Duration::from_secs(5);
        let events = std::cell::RefCell::new(Vec::new());
        let cb = |info: crate::core::archive::ProgressInfo| events.borrow_mut().push(info);
        tracker.feed("  0% + huge.bin", &cb);
        tracker.last_emit = std::time::Instant::now() - std::time::Duration::from_secs(5);
        assert!(tracker.feed_io(42, &cb));
        assert_eq!(events.borrow().last().unwrap().current, 42);
        // Monotonic even if /proc reports less (PID reuse): never goes back.
        tracker.last_emit = std::time::Instant::now() - std::time::Duration::from_secs(5);
        assert!(tracker.feed_io(10, &cb));
        assert_eq!(events.borrow().last().unwrap().current, 42);
    }

    #[test]
    fn add_updates_archive_and_stages_nested_entries() {
        use crate::core::archive::ArchiveBackend as _;
        use std::sync::{atomic::AtomicBool, Arc};
        let b = super::SevenZipBackend::with_cancel(Arc::new(AtomicBool::new(false)));
        if !b.is_available() {
            eprintln!("(7z unavailable, skipping)");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.7z");
        let src = dir.path().join("folder");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("old.txt"), b"old").unwrap();
        b.create(&dest, std::slice::from_ref(&src), 6, None, None)
            .unwrap();

        let new = dir.path().join("new.txt");
        std::fs::write(&new, b"new").unwrap();
        // Root add (no staging) plus a nested entry (staging dir path).
        b.add(
            &dest,
            &[
                (new.clone(), "folder/new.txt".to_string()),
                (new, "new_sub/deep.txt".to_string()),
            ],
            None,
            None,
        )
        .unwrap();

        let info = b.list(&dest).unwrap();
        let names: Vec<&str> = info.entries.iter().map(|e| e.path.as_str()).collect();
        assert!(names.contains(&"folder/old.txt"), "lost old: {:?}", names);
        assert!(
            names.contains(&"folder/new.txt"),
            "missing new: {:?}",
            names
        );
        assert!(
            names.contains(&"new_sub/deep.txt"),
            "missing staged: {:?}",
            names
        );

        let out = dir.path().join("out");
        b.extract(&dest, &out, None, None, None).unwrap();
        assert_eq!(std::fs::read(out.join("folder/new.txt")).unwrap(), b"new");
        assert_eq!(std::fs::read(out.join("new_sub/deep.txt")).unwrap(), b"new");
        // Staging dir must be cleaned up on success.
        let leftovers: Vec<_> = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.starts_with(&format!("arkx-add-{}", std::process::id()))
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "staging dirs left behind: {:?}",
            leftovers
        );
    }

    #[test]
    fn list_with_password_unlocks_header_encryption() {
        use crate::core::archive::ArchiveBackend as _;
        use std::sync::{atomic::AtomicBool, Arc};
        let b = super::SevenZipBackend::with_cancel(Arc::new(AtomicBool::new(false)));
        if !b.is_available() {
            eprintln!("(7z unavailable, skipping)");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("secret.7z");
        let src = dir.path().join("a.txt");
        std::fs::write(&src, b"top secret").unwrap();
        // create with a password always turns on -mhe=on (header encryption).
        b.create(&dest, std::slice::from_ref(&src), 6, Some("s3cret"), None)
            .unwrap();

        assert!(matches!(b.list(&dest), Err(ArkxError::WrongPassword)));
        assert!(matches!(
            b.list_with_password(&dest, "wrong"),
            Err(ArkxError::WrongPassword)
        ));
        let info = b.list_with_password(&dest, "s3cret").unwrap();
        assert!(info.has_encrypted, "encrypted archive not flagged");
        assert!(
            info.entries.iter().any(|e| e.path == "a.txt"),
            "entries missing after unlock: {:?}",
            info.entries
        );
    }

    #[test]
    fn remove_updates_7z_archive() {
        use crate::core::archive::ArchiveBackend as _;
        use std::sync::{atomic::AtomicBool, Arc};
        let b = super::SevenZipBackend::with_cancel(Arc::new(AtomicBool::new(false)));
        if !b.is_available() {
            eprintln!("(7z unavailable, skipping)");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.7z");
        let src = dir.path().join("folder");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("keep.txt"), b"keep").unwrap();
        std::fs::write(src.join("drop.txt"), b"drop").unwrap();
        b.create(&dest, std::slice::from_ref(&src), 6, None, None)
            .unwrap();

        b.remove(&dest, &["folder/drop.txt".to_string()], None, None)
            .unwrap();

        let info = b.list(&dest).unwrap();
        let names: Vec<&str> = info.entries.iter().map(|e| e.path.as_str()).collect();
        assert!(names.contains(&"folder/keep.txt"), "lost keep: {:?}", names);
        assert!(
            !names.contains(&"folder/drop.txt"),
            "drop still present: {:?}",
            names
        );
        let out = dir.path().join("out");
        b.extract(&dest, &out, None, None, None).unwrap();
        assert_eq!(std::fs::read(out.join("folder/keep.txt")).unwrap(), b"keep");
    }
}

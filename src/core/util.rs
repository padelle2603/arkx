//! Small shared runtime helpers: adaptive thread counts and memory info.
//!
//! Nothing is hardcoded for a specific machine: everything scales on CPU and RAM
//! detected at runtime, with explicit override (`--threads` / `ARKX_THREADS`).

use crate::core::archive::{ArchiveInfo, EntryHashes};
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Global override (0 = auto). Set by `--threads`, read by backends.
static THREAD_OVERRIDE: AtomicUsize = AtomicUsize::new(0);

/// Sets the thread cap from CLI (`None` = auto).
pub fn set_thread_override(n: Option<usize>) {
    THREAD_OVERRIDE.store(n.unwrap_or(0), Ordering::Relaxed);
}

pub fn thread_override() -> Option<usize> {
    match THREAD_OVERRIDE.load(Ordering::Relaxed) {
        0 => None,
        n => Some(n),
    }
}

/// Number of logical CPUs, with a sane fallback.
pub fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .max(1)
}

/// Available RAM in MiB (`/proc/meminfo` on Linux, `None` elsewhere/on error).
pub fn available_memory_mb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("MemAvailable:") {
                let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
                return Some(kb / 1024);
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Parse `--threads 8` / `--threads=8` / `ARKX_THREADS=8`.
pub fn parse_threads_value(s: &str) -> Option<usize> {
    s.trim().parse::<usize>().ok().filter(|&n| n >= 1)
}

/// Pure testable core: how many threads to use given CPUs and request.
/// - the request (CLI > env > auto) is a cap, never a target above CPUs;
/// - on small machines (<=4 CPUs) keep 1 thread free for the system;
/// - result always >= 1.
pub fn clamp_threads(cpus: usize, requested: Option<usize>) -> usize {
    let cpus = cpus.max(1);
    let mut n = requested.unwrap_or(cpus).clamp(1, cpus);
    if cpus <= 4 && n > 1 {
        n -= 1;
    }
    n.max(1)
}

/// Effective threads: CLI override > `ARKX_THREADS` > auto.
pub fn effective_threads() -> usize {
    let env_req = std::env::var("ARKX_THREADS")
        .ok()
        .and_then(|s| parse_threads_value(&s));
    clamp_threads(num_cpus(), thread_override().or(env_req))
}

/// Workers for RAM-hungry codecs (zstd MT): `~256MiB` per worker.
/// On low-RAM machines the count drops instead of going OOM;
/// without RAM info the plain thread count applies.
pub fn memory_capped_workers(threads: usize, mb_per_worker: u64) -> usize {
    let threads = threads.max(1);
    match available_memory_mb() {
        Some(avail) => {
            let by_mem = (avail / 2 / mb_per_worker.max(1)).max(1) as usize;
            threads.min(by_mem).max(1)
        }
        None => threads,
    }
}

/// zstd workers: effective threads with RAM cap.
pub fn zstd_workers() -> u32 {
    memory_capped_workers(effective_threads(), 256).min(u32::MAX as usize) as u32
}

/// Threshold above which an archive is "large" and deserves parallel backends/codecs.
/// Scales with RAM (default 100MiB without info), with floor and ceiling
/// so small machines don't fork too early and large ones don't
/// stay single-threaded for too long.
pub fn big_archive_threshold_bytes() -> u64 {
    const MIN: u64 = 32 * 1024 * 1024;
    const MAX: u64 = 256 * 1024 * 1024;
    match available_memory_mb() {
        Some(mb) => (mb * 1024 * 1024 / 256).clamp(MIN, MAX),
        None => 100 * 1024 * 1024,
    }
}

/// Threshold above which zip creation switches to multithreaded 7z (total input).
pub fn zip_seven_threshold_bytes() -> u64 {
    const MIN: u64 = 64 * 1024 * 1024;
    const MAX: u64 = 1024 * 1024 * 1024;
    match available_memory_mb() {
        Some(mb) => (mb * 1024 * 1024 / 32).clamp(MIN, MAX),
        None => 256 * 1024 * 1024,
    }
}

/// Free bytes on the filesystem containing `path` (`df`, best-effort).
/// `None` if undeterminable: never block a job for this.
pub fn filesystem_free_bytes(path: &std::path::Path) -> Option<u64> {
    let anchor = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."))
    };
    let out = std::process::Command::new("df")
        .args(["-B1", "--output=avail"])
        .arg(&anchor)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_df_avail(&String::from_utf8_lossy(&out.stdout))
}

/// Parse `df -B1 --output=avail` (header + one byte line).
pub fn parse_df_avail(s: &str) -> Option<u64> {
    s.lines().nth(1)?.split_whitespace().next()?.parse().ok()
}

/// Format a percentage with adaptive decimals and locale separator:
/// `<1%` → 2 decimals (`0.42%`), `1–99%` → 1 decimal, `100%` whole.
/// Never early "100%": if `current < total` the cap is `99.99%`.
/// With Italian/European locale use the comma, otherwise the dot.
pub fn format_percent(pct: f32, current: u64, total: u64) -> String {
    format_percent_with(pct, current, total, decimal_comma())
}

/// Pure (testable) version with explicit separator.
pub fn format_percent_with(pct: f32, current: u64, total: u64, comma: bool) -> String {
    let pct = pct.clamp(0.0, 100.0);
    let raw = if pct < 1.0 {
        format!("{:.2}%", pct)
    } else if pct < 100.0 {
        format!("{:.1}%", pct)
    } else {
        "100%".to_string()
    };
    let mut s = if comma {
        raw.replacen('.', ",", 1)
    } else {
        raw
    };
    // Never "100%" (or "100.0%" from rounding) on an incomplete job.
    if total > 0 && current < total && (s == "100%" || s == "100.0%" || s == "100,0%") {
        s = if comma {
            "99,9%".to_string()
        } else {
            "99.9%".to_string()
        };
    }
    s
}

/// True if the locale wants the decimal comma (it/fr/de/es/pt/nl...).
pub fn decimal_comma() -> bool {
    decimal_comma_for(
        &std::env::var("LC_NUMERIC").unwrap_or_default(),
        &std::env::var("LANG").unwrap_or_default(),
    )
}

fn decimal_comma_for(lc_numeric: &str, lang: &str) -> bool {
    let loc = if lc_numeric.is_empty() || lc_numeric == "C" || lc_numeric == "POSIX" {
        lang.to_lowercase()
    } else {
        lc_numeric.to_lowercase()
    };
    loc.starts_with("it")
        || loc.starts_with("fr")
        || loc.starts_with("de")
        || loc.starts_with("es")
        || loc.starts_with("pt")
        || loc.starts_with("nl")
        || loc.starts_with("el")
}

/// Best-effort estimate of input bytes (follows symlinks like the backend;
/// unreadable entries count as 0 instead of failing the job).
pub fn total_input_size(sources: &[std::path::PathBuf]) -> u64 {
    // The map has dual keys (absolute + relative): count only absolute ones.
    input_file_sizes(sources)
        .iter()
        .filter(|(k, _)| std::path::Path::new(k.as_str()).is_absolute())
        .map(|(_, v)| *v)
        .sum()
}

/// Absolute-path → bytes map for every file under `sources` (parallel).
/// Used by creation progress: when 7z moves to the next file,
/// the previous file's bytes are done (honest monotonic floor,
/// even when 7z's % with -mmt stalls on huge files).
pub fn input_file_sizes(sources: &[std::path::PathBuf]) -> std::collections::HashMap<String, u64> {
    use rayon::prelude::*;
    use std::collections::HashMap;
    sources
        .par_iter()
        .map(|s| input_sizes_one(s.as_path()))
        .reduce(HashMap::new, |mut a: HashMap<String, u64>, b| {
            a.extend(b);
            a
        })
}

fn input_sizes_one(p: &std::path::Path) -> std::collections::HashMap<String, u64> {
    let mut map = std::collections::HashMap::new();
    // 7z prints names relativized to the source parent (not absolute):
    // index both forms or the progress floor always misses.
    let base = p.parent();
    input_sizes_walk(p, base, &mut map);
    map
}

fn input_sizes_walk(
    p: &std::path::Path,
    base: Option<&std::path::Path>,
    map: &mut std::collections::HashMap<String, u64>,
) {
    // `symlink_metadata` (no-follow): a symlink that points back to a parent
    // dir must not be walked, or the size estimation recurses forever.
    let meta = match std::fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(_) => return,
    };
    if meta.is_file() {
        let abs = p.to_string_lossy().to_string();
        map.insert(abs, meta.len());
        if let Some(b) = base {
            if let Ok(rel) = p.strip_prefix(b) {
                let r = rel.to_string_lossy().to_string();
                if !r.is_empty() {
                    map.insert(r, meta.len());
                }
            }
        }
        return;
    }
    if !meta.is_dir() {
        return;
    }
    if let Ok(walk) = std::fs::read_dir(p) {
        for entry in walk.flatten() {
            input_sizes_walk(&entry.path(), base, map);
        }
    }
}

/// Ellipsize a string in the middle so the tail stays visible (e.g. long
/// archive paths in the status bar). Operates on chars so it never splits a
/// UTF-8 codepoint.
pub fn truncate_middle(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let half = (max.saturating_sub(3)) / 2;
    let chars: Vec<char> = s.chars().collect();
    let start: String = chars[..half].iter().collect();
    let end: String = chars[chars.len() - half..].iter().collect();
    format!("{}...{}", start, end)
}

/// Read `r` to EOF, discarding everything (drain a child pipe to avoid a
/// 64KB deadlock when the parent never reads).
pub fn drain_reader(mut r: impl Read) {
    let mut buf = vec![0u8; 8192];
    while let Ok(n) = r.read(&mut buf) {
        if n == 0 {
            break;
        }
    }
}

/// Drain an optional child pipe on a dedicated thread (avoids the 64KB pipe
/// deadlock when the parent reads the other pipe itself).
pub fn spawn_drain_pipe(r: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        if let Some(r) = r {
            drain_reader(r);
        }
    })
}

/// Sum of the sizes of the non-directory entries of `info` that match any of
/// the filters in `sel` (`None` = all). Drives the extract progress total.
pub fn sum_selected(info: &ArchiveInfo, sel: Option<&[String]>) -> u64 {
    match sel {
        None => info.total_size,
        Some(sel) => {
            // Normalize the selectors once so the per-entry match below never
            // re-normalizes (no allocations in the hot loop).
            let sels = crate::core::paths::normalize_sel(sel);
            info.entries
                .iter()
                .filter(|e| {
                    !e.is_dir
                        && sels
                            .iter()
                            .any(|s| crate::core::paths::entry_matches_norm(&e.path, s))
                })
                .map(|e| e.size)
                .fold(0u64, |a, b| a.saturating_add(b))
        }
    }
}

/// Count of the non-directory entries of `info` that match any of the filters
/// in `sel` (`None` = all). Drives the extract progress bar when the backend
/// reports per-entry completion (7z/bsdtar), avoiding a per-tick filesystem
/// walk of the destination tree.
pub fn count_selected(info: &ArchiveInfo, sel: Option<&[String]>) -> u64 {
    match sel {
        None => info.entries.iter().filter(|e| !e.is_dir).count() as u64,
        Some(sel) => {
            let sels = crate::core::paths::normalize_sel(sel);
            info.entries
                .iter()
                .filter(|e| {
                    !e.is_dir
                        && sels
                            .iter()
                            .any(|s| crate::core::paths::entry_matches_norm(&e.path, s))
                })
                .count() as u64
        }
    }
}

/// SHA-256 + MD5 digests of a byte stream, computed in a single pass.
pub fn hash_reader(mut r: impl Read) -> crate::core::error::Result<EntryHashes> {
    use md5::Md5;
    use sha2::{Digest, Sha256};
    let mut sha256 = Sha256::new();
    let mut md5 = Md5::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = r
            .read(&mut buf)
            .map_err(crate::core::error::ArkxError::Io)?;
        if n == 0 {
            break;
        }
        sha256.update(&buf[..n]);
        md5.update(&buf[..n]);
    }
    Ok(EntryHashes {
        sha256: format!("{:x}", sha256.finalize()),
        md5: format!("{:x}", md5.finalize()),
    })
}

/// SHA-256 + MD5 digests of a file on disk.
pub fn hash_file(path: &Path) -> crate::core::error::Result<EntryHashes> {
    let f = std::fs::File::open(path).map_err(crate::core::error::ArkxError::Io)?;
    hash_reader(std::io::BufReader::new(f))
}

/// Read `r` streaming, splitting on `\r`/`\n`, invoking `on_line(&str)` per
/// complete line (UTF-8-sensitive: a partial codepoint is dropped). Return
/// `true` from `on_line` to stop early (used for cancellation).
/// `reader` is consumed; a trailing unterminated line is still delivered.
/// Reads are buffered (64 KiB) and lines are borrowed from one no-realloc
/// accumulation buffer, so per-line allocation is avoided: safe for the
/// `\r`-refreshed label streams of 7z/bsdtar progress output.
pub fn read_lines_until(reader: impl Read, mut on_line: impl FnMut(&str) -> bool) {
    let mut reader = BufReader::with_capacity(64 * 1024, reader);
    let mut buf = vec![0u8; 64 * 1024];
    let mut chunk: Vec<u8> = Vec::new();
    loop {
        let n = reader.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            if b == b'\r' || b == b'\n' {
                if !chunk.is_empty() {
                    if let Ok(s) = std::str::from_utf8(&chunk) {
                        if on_line(s) {
                            return;
                        }
                    }
                    chunk.clear();
                }
            } else {
                chunk.push(b);
            }
        }
    }
    if !chunk.is_empty() {
        if let Ok(s) = std::str::from_utf8(&chunk) {
            on_line(s);
        }
    }
}

/// Unique per-invocation temp dir for "open with", auto-removed after a delay
/// so the launched app has time to read the extracted entry. The unique name
/// also stops concurrent CLI/GUI opens from colliding in one shared dir.
pub fn open_with_dir() -> Result<std::path::PathBuf, crate::core::error::ArkxError> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("arkx-open-{}-{}", std::process::id(), nanos));
    std::fs::create_dir_all(&dir).map_err(crate::core::error::ArkxError::Io)?;
    let cleanup = dir.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(60));
        std::fs::remove_dir_all(&cleanup).ok();
    });
    Ok(dir)
}

/// Named staging dir under the system temp dir, removed on drop (best-effort)
/// so early `?` returns cannot leak it. A leftover from a previous crash with
/// the same pid is wiped on creation. Used by convert, paste and new-folder.
#[derive(Debug)]
pub struct TaskTempDir(std::path::PathBuf);

impl TaskTempDir {
    pub fn new(prefix: &str) -> Result<Self, crate::core::error::ArkxError> {
        let dir = std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).ok();
        }
        std::fs::create_dir_all(&dir).map_err(crate::core::error::ArkxError::Io)?;
        Ok(Self(dir))
    }
}

impl std::ops::Deref for TaskTempDir {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TaskTempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Final "Completed" progress marker. Unknown total (0 bytes) reports a clean
/// 100/100 instead of 0/0, so the bar always ends on a full state.
pub fn completed(total: u64) -> crate::core::archive::ProgressInfo {
    if total > 0 {
        crate::core::archive::ProgressInfo::new("Completed".to_string(), total, total)
    } else {
        crate::core::archive::ProgressInfo::new("Completed".to_string(), 100, 100)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_middle_keeps_tail() {
        assert_eq!(truncate_middle("short", 10), "short");
        assert_eq!(truncate_middle("abcdefghijklmnop", 10), "abc...nop");
        // Multi-byte chars never split.
        assert_eq!(truncate_middle("àààààbcdefghij", 10), "ààà...hij");
    }

    #[test]
    fn hash_reader_matches_known_vectors() {
        // NIST/RFC vector: SHA-256("abc") and MD5("abc").
        let h = hash_reader(&b"abc"[..]).unwrap();
        assert_eq!(
            h.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(h.md5, "900150983cd24fb0d6963f7d28e17f72");
        // Streams longer than one read buffer still hash correctly.
        let big = vec![0x5A_u8; 300_000];
        let h2 = hash_reader(&big[..]).unwrap();
        let mut expect = sha2::Sha256::new();
        use sha2::Digest;
        expect.update(&big);
        assert_eq!(h2.sha256, format!("{:x}", expect.finalize()));
    }

    #[cfg(unix)]
    #[test]
    fn input_sizes_skips_symlink_loops() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a"), vec![1u8; 7]).unwrap();
        // Symlink that points back to `root` itself: walking it must skip,
        // not recurse forever.
        std::os::unix::fs::symlink(&root, root.join("loop")).unwrap();
        let mut map = std::collections::HashMap::new();
        input_sizes_walk(&root, Some(dir.path()), &mut map);
        assert_eq!(
            map.len(),
            2,
            "only `root/a` (abs+rel); loop skipped: {:?}",
            map
        );
        assert_eq!(map.get("root/a"), Some(&7)); // rel key (strip base)
        assert_eq!(map.get(root.join("a").to_str().unwrap()), Some(&7)); // abs key
    }

    #[test]
    fn read_lines_splits_on_cr_and_lf() {
        let mut got: Vec<String> = Vec::new();
        read_lines_until(&b"one\rtwo\nthree\r\nfour"[..], |l| {
            got.push(l.to_string());
            false
        });
        assert_eq!(got, vec!["one", "two", "three", "four"]);
    }

    #[test]
    fn read_lines_early_stop() {
        let mut got: Vec<String> = Vec::new();
        read_lines_until(&b"a\nb\nc"[..], |l| {
            got.push(l.to_string());
            l == "b"
        });
        assert_eq!(got, vec!["a", "b"]);
    }

    #[test]
    fn clamp_prefers_request_capped_to_cpus() {
        assert_eq!(clamp_threads(12, None), 12);
        assert_eq!(clamp_threads(12, Some(4)), 4);
        assert_eq!(clamp_threads(12, Some(999)), 12);
        assert_eq!(clamp_threads(12, Some(0)), 1);
    }

    #[test]
    fn clamp_reserves_one_on_small_machines() {
        assert_eq!(clamp_threads(4, None), 3);
        assert_eq!(clamp_threads(2, None), 1);
        assert_eq!(clamp_threads(1, None), 1);
        // Low explicit request stays intact.
        assert_eq!(clamp_threads(4, Some(1)), 1);
    }

    #[test]
    fn parse_threads_values() {
        assert_eq!(parse_threads_value("8"), Some(8));
        assert_eq!(parse_threads_value(" 4 "), Some(4));
        assert_eq!(parse_threads_value("0"), None);
        assert_eq!(parse_threads_value("abc"), None);
        assert_eq!(parse_threads_value(""), None);
    }

    #[test]
    fn override_roundtrip() {
        let prev = thread_override();
        set_thread_override(Some(3));
        assert_eq!(thread_override(), Some(3));
        // The override never exceeds real CPUs.
        assert!(effective_threads() <= num_cpus());
        set_thread_override(prev);
        assert_eq!(thread_override(), prev);
    }

    #[test]
    fn thresholds_have_sane_bounds() {
        let t = big_archive_threshold_bytes();
        assert!(
            (32 * 1024 * 1024..=256 * 1024 * 1024).contains(&t),
            "t={}",
            t
        );
        let z = zip_seven_threshold_bytes();
        assert!(
            (64 * 1024 * 1024..=1024 * 1024 * 1024).contains(&z),
            "z={}",
            z
        );
    }

    #[test]
    fn input_size_counts_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.bin"), vec![7u8; 1000]).unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("b.bin"), vec![7u8; 500]).unwrap();
        assert_eq!(total_input_size(&[dir.path().to_path_buf()]), 1500);
        assert_eq!(total_input_size(&[dir.path().join("missing")]), 0);
        let map = input_file_sizes(&[dir.path().to_path_buf()]);
        // Dual keys (absolute + parent-relative) per file.
        assert_eq!(map.len(), 4);
        assert_eq!(total_input_size(&[dir.path().to_path_buf()]), 1500);
    }

    #[test]
    fn parse_df_avail_reads_bytes() {
        assert_eq!(parse_df_avail("Avail\n123456789\n"), Some(123456789));
        assert_eq!(parse_df_avail("Avail\n"), None);
        assert_eq!(parse_df_avail(""), None);
    }

    #[test]
    fn percent_uses_two_decimals_below_one() {
        assert_eq!(format_percent_with(0.42, 42, 10000, false), "0.42%");
        assert_eq!(format_percent_with(0.42, 42, 10000, true), "0,42%");
        assert_eq!(format_percent_with(0.0, 0, 100, true), "0,00%");
    }

    #[test]
    fn percent_uses_one_decimal_up_to_100() {
        assert_eq!(format_percent_with(4.25, 425, 10000, true), "4,2%");
        assert_eq!(format_percent_with(99.95, 9995, 10000, false), "99.9%");
        assert_eq!(format_percent_with(100.0, 100, 100, true), "100%");
    }

    #[test]
    fn percent_never_shows_100_early() {
        assert_eq!(format_percent_with(99.999, 999, 1000, false), "99.9%");
        assert_eq!(format_percent_with(100.0, 999, 1000, true), "99,9%");
        assert_eq!(format_percent_with(100.0, 1000, 1000, true), "100%");
        // total 0 (unknown): no cap applicable.
        assert_eq!(format_percent_with(100.0, 0, 0, false), "100%");
    }

    #[test]
    fn comma_locales() {
        assert!(decimal_comma_for("", "it_IT.UTF-8"));
        assert!(decimal_comma_for("", "de_DE.UTF-8"));
        assert!(!decimal_comma_for("", "en_US.UTF-8"));
        assert!(!decimal_comma_for("", "C"));
        assert!(decimal_comma_for("it_IT.UTF-8", "en_US.UTF-8"));
    }
}

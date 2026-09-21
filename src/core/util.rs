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

/// Threads an extraction will actually use, for the CLI log line.
/// An explicit `--threads` always wins; otherwise the Conservative profile
/// caps the job to a single thread.
pub fn extraction_log_threads() -> usize {
    let capped = crate::core::config::extraction_thread_cap();
    if let Some(n) = thread_override() {
        return n;
    }
    let effective = effective_threads();
    capped.map(|cap| effective.min(cap)).unwrap_or(effective)
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
/// The Balanced tier scales with RAM (default 100MiB without info); `Fast`
/// routes earlier (25 MiB) for zip→7z parallel extraction; `Conservative`
/// never routes to 7z (single-threaded native only).
pub fn big_archive_threshold_bytes() -> u64 {
    use crate::core::config::ExtractTier;
    match crate::core::config::extraction() {
        ExtractTier::Fast => 25 * 1024 * 1024,
        ExtractTier::Conservative => u64::MAX,
        ExtractTier::Balanced => big_archive_threshold_balanced(),
    }
}

/// Balanced (RAM-scaled) zip→7z threshold for extraction.
fn big_archive_threshold_balanced() -> u64 {
    const MIN: u64 = 32 * 1024 * 1024;
    const MAX: u64 = 256 * 1024 * 1024;
    match available_memory_mb() {
        Some(mb) => (mb * 1024 * 1024 / 256).clamp(MIN, MAX),
        None => 100 * 1024 * 1024,
    }
}

/// Threshold above which zip creation switches to multithreaded 7z (total input).
/// `Small` routes earlier (64 MiB) for the best ratio; `Fast` keeps zip native
/// (parallel rayon writer) by never switching to 7z.
pub fn zip_seven_threshold_bytes() -> u64 {
    use crate::core::config::CompressTier;
    match crate::core::config::compression() {
        CompressTier::Fast => u64::MAX,
        CompressTier::Small => 64 * 1024 * 1024,
        CompressTier::Balanced => zip_seven_threshold_balanced(),
    }
}

/// Balanced (RAM-scaled) zip→7z threshold for creation.
fn zip_seven_threshold_balanced() -> u64 {
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
        // A `read` that returns `Interrupted` is not EOF: retry it instead of
        // treating it as end-of-stream (would truncate 7z/bsdtar output).
        let n = loop {
            match reader.read(&mut buf) {
                Ok(n) => break n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break 0,
            }
        };
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

/// Burst-smoothing progress governor.
///
/// UI events are meant to fire on a fixed cadence (the io-poll thread), while
/// the underlying counters arrive in irregular bursts (7z's `%`, kernel
/// `/proc` byte counters, per-file floors). Driving the bar straight from
/// those bursts is what produces the "freeze → jump → freeze" behaviour: a
/// fast first read slams the bar to ~50%, a solid-block flush jumps it, then
/// it stays frozen for minutes.
///
/// `display` is monotonic and converges toward `raw` with an exponential
/// catch-up that is additionally bounded per tick, so:
///   - a single-sample spike never leaps more than a few % of `total`;
///   - a long stall followed by a huge `raw` move still climbs stepwise;
///   - sustained fast streams are tracked (the lag stays small).
///
/// `nudge` never regresses and never overshoots `total` (the caller may still
/// jump straight to 100 via `complete`, the one allowed end-of-job transition).
#[derive(Debug)]
pub struct Smoother {
    total: u64,
    display: u64,
    last: std::time::Instant,
    tau: f64,
    /// Max fraction of the remaining gap closed by a single tick, even when
    /// that tick comes after a long freeze (0.04 → at most 4% of `total`).
    max_rate: f64,
    /// Liveness floor: even a tiny `raw` advance must move the bar (e.g. a
    /// single huge file whose true signal only crept by 0.1%).
    min_step: u64,
    /// Hard per-tick ceiling on `display` advance (absolute bytes), so a
    /// near-continuous counter can never leap more than this even right after
    /// a long freeze (u64::MAX = unclamped, the historical behavior).
    abs_cap: u64,
}

impl Smoother {
    pub fn new(total: u64) -> Self {
        Self {
            total,
            display: 0,
            last: std::time::Instant::now(),
            tau: 0.8,
            max_rate: 0.04,
            min_step: (total / 500).max(1),
            abs_cap: u64::MAX,
        }
    }

    /// Tracking profile for byte-granular counters (native zip) that arrive
    /// nearly continuously: fast catch-up with a ~10%-of-total per-tick cap,
    /// so a whole-file read burst can't leap but a genuinely fast stream (Fast
    /// tier easily moves 50-100%/s) is sketched in real time instead of being
    /// throttled to a crawl and then jumping at the end.
    pub fn lively(total: u64) -> Self {
        Self {
            total,
            display: 0,
            last: std::time::Instant::now(),
            tau: 0.4,
            max_rate: 0.15,
            min_step: (total / 3000).max(1),
            abs_cap: (total / 10).max(1),
        }
    }

    /// Current displayed value (monotonic, in [0, total]).
    pub fn value(&self) -> u64 {
        self.display
    }

    /// Advance toward `raw` (coerced to [0, total]) using wall-clock time.
    pub fn nudge(&mut self, raw: u64) {
        self.nudge_at(raw, std::time::Instant::now());
    }

    /// Testable core of `nudge` with an explicit timestamp.
    pub fn nudge_at(&mut self, raw: u64, now: std::time::Instant) {
        let raw = raw.min(self.total);
        if raw <= self.display {
            // Idle while not moving forwards; do not consume the clock so a
            // sparse-but-filled burst is still clamped by `max_rate`.
            return;
        }
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        let rate = 1.0 - (-dt / self.tau).exp();
        let gap = raw - self.display;
        let gain = (gap as f64 * rate)
            .min(gap as f64 * self.max_rate)
            .max(self.min_step as f64)
            .min(self.abs_cap as f64);
        self.display = ((self.display as f64) + gain).min(raw as f64) as u64;
    }

    /// Force the final state (only the end of a job may jump to 100).
    pub fn complete(&mut self) -> u64 {
        self.display = self.total;
        self.display
    }
}

/// Staged progress target for the native-parallel zip merge phase. After every
/// chunk has been read and deflated (`anchor` = value reached by the read
/// counter), the merged compressed bytes drive the remaining `total - anchor`
/// linearly. Monotonic in `merged`, ends at `total` when the merge is done, so
/// the bar keeps moving through a phase that would otherwise be silent.
pub fn merge_progress_target(anchor: u64, total: u64, merged: u64, merge_total: u64) -> u64 {
    if merge_total == 0 {
        return anchor.min(total);
    }
    let anchor = anchor.min(total);
    let remaining = total - anchor;
    let done = merged.min(merge_total);
    anchor + (remaining as u128 * done as u128 / merge_total as u128) as u64
}

#[cfg(test)]
mod smoother_tests {
    use super::{merge_progress_target, Smoother};
    use std::time::{Duration, Instant};

    fn now_at(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn never_regresses() {
        let mut s = Smoother::new(10_000);
        let mut prev = 0;
        let mut t = 0;
        for raw in [0, 5_000, 4_000, 8_000, 2_000, 10_000] {
            t += 250;
            s.nudge_at(raw, now_at(t));
            assert!(s.value() >= prev, "regressed after raw={raw}");
            prev = s.value();
        }
        assert!(s.value() <= 10_000);
    }

    #[test]
    fn single_sample_spike_is_bounded() {
        // A 46% burst in the very first sample must not leap: ≤4% of total.
        let mut s = Smoother::new(10_000);
        s.nudge_at(4_600, now_at(250));
        assert!(s.value() <= 400, "spike not clamped: {}", s.value());
    }

    #[test]
    fn long_stall_then_jump_climbs_stepwise() {
        // 90% arrives after a 10s freeze: still bounded per tick (≤4%).
        let mut s = Smoother::new(10_000);
        s.nudge_at(0, now_at(0));
        s.nudge_at(9_000, now_at(10_000));
        assert!(s.value() <= 400, "stall jump not clamped: {}", s.value());
    }

    #[test]
    fn catches_up_toward_raw() {
        let mut s = Smoother::new(10_000);
        let mut t = 0;
        let mut last = 0;
        for _ in 0..100 {
            t += 250;
            s.nudge_at(5_000, now_at(t));
            assert!(s.value() >= last);
            last = s.value();
        }
        assert_eq!(last, 5_000, "did not converge");
    }

    #[test]
    fn never_overshoots_total_and_completes() {
        let mut s = Smoother::new(10_000);
        s.nudge_at(99_999, now_at(250));
        assert!(s.value() <= 10_000);
        assert_eq!(s.complete(), 10_000);
    }

    #[test]
    fn lively_caps_single_tick_to_abs() {
        // A 50% burst in one tick must move the bar ≤~10% of total, not 50%.
        let mut s = Smoother::lively(10_000);
        s.nudge_at(5_000, now_at(100));
        assert!(s.value() <= 1_000, "lively spike not capped: {}", s.value());
    }

    #[test]
    fn merge_progress_target_is_monotonic_and_reaches_total() {
        const TOTAL: u64 = 10_000;
        let anchor = 8_000;
        let merge_total = 100;
        let mut prev = merge_progress_target(anchor, TOTAL, 0, merge_total);
        assert_eq!(prev, anchor, "start must equal the read-phase anchor");
        for merged in 1..=merge_total {
            let v = merge_progress_target(anchor, TOTAL, merged, merge_total);
            assert!(v >= prev, "regressed {v} < {prev}");
            prev = v;
        }
        assert_eq!(prev, TOTAL, "must reach total when the merge completes");
    }

    #[test]
    fn merge_progress_target_edge_cases() {
        // Unknown merge (empty payload): hold the anchor, never panic.
        assert_eq!(merge_progress_target(3_000, 10_000, 5, 0), 3_000);
        // Anchor already at total: stays at total regardless of merged.
        assert_eq!(merge_progress_target(10_000, 10_000, 0, 50), 10_000);
        // Anchor above total (raw counter raced ahead): clamped to total.
        assert_eq!(merge_progress_target(12_000, 10_000, 25, 50), 10_000);
        // Excess merged bytes are clamped, still fine at total.
        assert_eq!(merge_progress_target(8_000, 10_000, 200, 100), 10_000);
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
        // Balanced (default): the RAM-scaled thresholds are inside their bounds.
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
    fn thresholds_follow_profiles() {
        use crate::core::config::{apply, CompressTier, ExtractTier};
        // Extraction: Fast routes early (25 MiB), Conservative never (u64::MAX).
        apply(ExtractTier::Fast, CompressTier::Balanced);
        assert_eq!(big_archive_threshold_bytes(), 25 * 1024 * 1024);
        apply(ExtractTier::Conservative, CompressTier::Balanced);
        assert_eq!(big_archive_threshold_bytes(), u64::MAX);
        // Compression: Small routes from 64 MiB, Fast never switches to 7z.
        apply(ExtractTier::Balanced, CompressTier::Small);
        assert_eq!(zip_seven_threshold_bytes(), 64 * 1024 * 1024);
        apply(ExtractTier::Balanced, CompressTier::Fast);
        assert_eq!(zip_seven_threshold_bytes(), u64::MAX);
        // Restore defaults so other tests run pinned to balanced behavior.
        apply(ExtractTier::Balanced, CompressTier::Balanced);
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

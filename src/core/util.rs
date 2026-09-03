//! Small shared runtime helpers.

/// Number of logical CPUs, with a sane fallback.
pub fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

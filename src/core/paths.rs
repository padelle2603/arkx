//! Shared archive-entry path helpers.
//!
//! Archives mix `./`, leading `/` and missing trailing slashes. Every
//! backend and the browser normalize the same way through these helpers.

/// Strip `./` prefixes and leading `/` (e.g. `./a/b` → `a/b`).
pub fn normalize(p: &str) -> String {
    let mut s = p.trim().to_string();
    while s.starts_with("./") {
        s = s[2..].to_string();
    }
    while s.starts_with('/') {
        s = s[1..].to_string();
    }
    s
}

/// Ensure a trailing `/` for directory paths (`""` stays `""`).
pub fn with_trailing_slash(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    if s.ends_with('/') {
        s.to_string()
    } else {
        format!("{}/", s)
    }
}

/// True if archive entry `entry` is the selected path `sel` or lives under it
/// (both sides normalized; `sel` may name a file or a directory).
pub fn entry_matches(entry: &str, sel: &str) -> bool {
    let ep = normalize(entry);
    let f = normalize(sel);
    let dir = f.trim_end_matches('/');
    ep == f || ep == format!("{}/", dir) || ep.starts_with(&format!("{}/", dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_prefixes() {
        assert_eq!(normalize("./a/b"), "a/b");
        assert_eq!(normalize("/a/b"), "a/b");
        assert_eq!(normalize("a/b"), "a/b");
    }

    #[test]
    fn matches_files_and_dirs() {
        assert!(entry_matches("a/b.txt", "a/b.txt"));
        assert!(entry_matches("a/b.txt", "a/"));
        assert!(entry_matches("a/b/c.txt", "a"));
        assert!(!entry_matches("ab.txt", "a"));
    }
}

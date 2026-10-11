//! Shared archive-entry path helpers.
//!
//! Archives mix `./`, leading `/` and missing trailing slashes. Every
//! backend and the browser normalize the same way through these helpers.

/// Strip `./` prefixes and leading `/` (e.g. `./a/b` → `a/b`).
/// Single allocation: the prefixes are cut from the trimmed slice, not by
/// rebuilding the string on every iteration. Called once per entry inside
/// every backend hot loop.
pub fn normalize(p: &str) -> String {
    // Every leading slash, not just one: `//etc/passwd` must not survive as an
    // absolute path, or `dest.join(entry)` would escape the destination.
    let s = p.trim().trim_start_matches('/');
    let mut s = s;
    while let Some(rest) = s.strip_prefix("./") {
        s = rest;
    }
    s.to_string()
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

/// Pre-normalized (dir-form, no trailing slash) single selection.
fn normalize_sel_one(s: &str) -> String {
    normalize(s).trim_end_matches('/').to_string()
}

/// Pre-normalize a whole selection once, so hot matching loops compare without
/// re-normalizing the selector per entry (no per-comparison allocations).
pub fn normalize_sel(sels: &[String]) -> Vec<String> {
    sels.iter().map(|s| normalize_sel_one(s)).collect()
}

/// Match an already-normalized entry against one pre-normalized selector.
/// `sel` must come from [`normalize_sel`] (dir-form, no trailing slash).
pub fn entry_matches_norm(entry: &str, sel: &str) -> bool {
    entry == sel || (entry.starts_with(sel) && entry.as_bytes().get(sel.len()) == Some(&b'/'))
}

/// Prefix an entry name with `./` when it starts with `-`, so a hostile
/// archive entry is never parsed as a CLI switch of the external tool.
pub fn entry_arg(e: &str) -> String {
    if e.starts_with('-') {
        format!("./{e}")
    } else {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_prefixes() {
        assert_eq!(normalize("./a/b"), "a/b");
        assert_eq!(normalize("/a/b"), "a/b");
        assert_eq!(normalize("a/b"), "a/b");
        assert_eq!(normalize("././a/b"), "a/b");
        // A doubled slash must not survive as an absolute path.
        assert_eq!(normalize("//etc/passwd"), "etc/passwd");
    }

    #[test]
    fn matches_files_and_dirs() {
        let sels = normalize_sel(&["a/b.txt".to_string()]);
        assert!(entry_matches_norm("a/b.txt", &sels[0]));
        let sels = normalize_sel(&["a/".to_string()]);
        assert!(entry_matches_norm("a/b.txt", &sels[0]));
        let sels = normalize_sel(&["a".to_string()]);
        assert!(entry_matches_norm("a/b/c.txt", &sels[0]));
        assert!(!entry_matches_norm("ab.txt", &sels[0]));
    }

    #[test]
    fn matching_survives_dir_form() {
        let cases: &[(&str, &str, bool)] = &[
            ("a/b.txt", "a/b.txt", true),
            ("a/b.txt", "a/", true),
            ("a/b/c.txt", "a", true),
            ("ab.txt", "a", false),
            ("a", "a", true),
            ("a/b/", "a", true),
            ("./a/b.txt", "a", true),
            ("/x/y", "x/", true),
        ];
        for (entry, sel, expected) in cases {
            let sels = normalize_sel(&[sel.to_string()]);
            assert_eq!(
                entry_matches_norm(&normalize(entry), &sels[0]),
                *expected,
                "mismatch for {:?} vs {:?}",
                entry,
                sel
            );
        }
    }
}

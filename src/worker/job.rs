use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum JobKind {
    List {
        path: PathBuf,
        /// Password for header-encrypted archives (7z/RAR): the archive can
        /// only be listed when the header password is known.
        password: Option<String>,
    },
    Extract {
        archive: PathBuf,
        dest: PathBuf,
        entries: Option<Vec<String>>,
        password: Option<String>,
        /// Known byte total for the progress bar (GUI-derived from the loaded
        /// listing). Lets tar/7z/bsdtar skip their own pre-listing pass.
        total_bytes: Option<u64>,
        /// Known non-directory entry count, the per-entry progress bar total
        /// (GUI-derived from the loaded listing).
        total_entries: Option<u64>,
    },
    Add {
        archive: PathBuf,
        sources: Vec<(PathBuf, String)>,
        password: Option<String>,
    },
    NewFolder {
        archive: PathBuf,
        /// Full folder entry path inside the archive, trailing slash included.
        name: String,
        password: Option<String>,
    },
    Remove {
        archive: PathBuf,
        entries: Vec<String>,
        password: Option<String>,
    },
    Rename {
        archive: PathBuf,
        old_name: String,
        new_name: String,
        password: Option<String>,
    },
    Test {
        archive: PathBuf,
        entries: Option<Vec<String>>,
        password: Option<String>,
    },
    Paste {
        /// Target archive (same as `source_archive` for in-archive moves).
        archive: PathBuf,
        /// Destination directory inside the archive (e.g. "docs/", empty = root).
        dest: String,
        /// Archive the pasted entries currently live in.
        source_archive: PathBuf,
        /// Full entry paths to paste.
        entries: Vec<String>,
        /// `true` = cut (move: originals are removed after the copy), `false` = copy.
        cut: bool,
        password: Option<String>,
    },
    OpenWith {
        archive: PathBuf,
        entry: String,
        password: Option<String>,
    },
    SecureDelete {
        archive: PathBuf,
        entries: Vec<String>,
        passes: usize,
        password: Option<String>,
    },
    SetComment {
        archive: PathBuf,
        comment: String,
    },
    EntryHash {
        archive: PathBuf,
        entry: String,
        password: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct Job {
    pub kind: JobKind,
}

#[derive(Debug, Clone)]
pub enum JobResult {
    List(crate::core::archive::ArchiveInfo),
    Extract,
    Add,
    NewFolder,
    Remove,
    Rename,
    Test(crate::core::archive::TestReport),
    OpenWith(std::path::PathBuf),
    Paste,
    SecureDelete,
    Comment,
    EntryHash {
        entry: String,
        sha256: String,
        md5: String,
    },
}

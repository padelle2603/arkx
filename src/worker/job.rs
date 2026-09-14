use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum JobKind {
    List {
        path: PathBuf,
    },
    Extract {
        archive: PathBuf,
        dest: PathBuf,
        entries: Option<Vec<String>>,
        password: Option<String>,
    },
    Add {
        archive: PathBuf,
        sources: Vec<(PathBuf, String)>,
        password: Option<String>,
    },
    Remove {
        archive: PathBuf,
        entries: Vec<String>,
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
    Remove,
}

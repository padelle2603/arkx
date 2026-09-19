use crate::core::archive::ProgressInfo;
use crate::core::backends::BackendManager;
use crate::core::error::ArkxError;
use crate::worker::{Job, JobKind, JobResult};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;

/// Non-blocking worker pool: one dispatcher thread + channel.
/// The UI submits jobs via Sender and receives progress/results via Receiver.
/// Each job runs on its own thread with cancellation via AtomicBool.

#[derive(Debug)]
pub enum WorkerEvent {
    Started {
        kind: String,
    },
    Progress {
        info: ProgressInfo,
    },
    Finished {
        result: Result<JobResult, ArkxError>,
    },
    Error {
        err: ArkxError,
    },
}

pub struct WorkerPool {
    tx: Sender<Job>,
    rx: Receiver<WorkerEvent>,
    /// Live cancel flags, one per running job: cancelling must not reset the
    /// flag of a sibling job when this one finishes.
    cancel_flags: Arc<Mutex<Vec<Arc<AtomicBool>>>>,
}

impl WorkerPool {
    pub fn new() -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (evt_tx, evt_rx) = mpsc::channel::<WorkerEvent>();
        let cancel_flags = Arc::new(Mutex::new(Vec::<Arc<AtomicBool>>::new()));

        // Dispatcher thread — per-job threads so a pipe deadlock (7z) never
        // blocks dispatch. Progress stays byte-based (see backends).
        let flags = cancel_flags.clone();
        thread::spawn(move || {
            while let Ok(job) = job_rx.recv() {
                let kind_str = match &job.kind {
                    JobKind::List { .. } => "list",
                    JobKind::Extract { .. } => "extract",
                    JobKind::Add { .. } => "add",
                    JobKind::NewFolder { .. } => "add",
                    JobKind::Remove { .. } => "remove",
                    JobKind::Rename { .. } => "rename",
                    JobKind::Test { .. } => "test",
                    JobKind::Paste { .. } => "paste",
                    JobKind::OpenWith { .. } => "open-with",
                    JobKind::SecureDelete { .. } => "secure-delete",
                    JobKind::SetComment { .. } => "comment",
                }
                .to_string();
                let cancel_flag = Arc::new(AtomicBool::new(false));
                flags
                    .lock()
                    .expect("pool cancel flags")
                    .push(cancel_flag.clone());
                let _ = evt_tx.send(WorkerEvent::Started { kind: kind_str });
                let evt_tx_clone = evt_tx.clone();
                let evt_tx_inner = evt_tx.clone();
                let job_flags = flags.clone();
                // Run the job on its own thread so dispatch never blocks.
                thread::spawn(move || {
                    let backend = BackendManager::new();
                    let cancel_for_job = cancel_flag.clone();
                    let result = Self::execute_job(
                        &backend,
                        job,
                        move |info| {
                            if cancel_for_job.load(Ordering::Relaxed) {
                                return;
                            }
                            let _ = evt_tx_clone.send(WorkerEvent::Progress { info });
                        },
                        cancel_flag.clone(),
                    );

                    match result {
                        Ok(res) => {
                            let _ = evt_tx_inner.send(WorkerEvent::Finished { result: Ok(res) });
                        }
                        Err(e) => {
                            // Object-level races are gone (own flag): the job is
                            // cancelled only when THIS flag is set.
                            let was_cancelled = matches!(e, ArkxError::Cancelled)
                                || cancel_flag.load(Ordering::Relaxed);
                            if was_cancelled {
                                let _ = evt_tx_inner.send(WorkerEvent::Error {
                                    err: ArkxError::Cancelled,
                                });
                            } else {
                                let _ = evt_tx_inner.send(WorkerEvent::Finished { result: Err(e) });
                            }
                        }
                    }
                    job_flags
                        .lock()
                        .expect("pool cancel flags")
                        .retain(|f| !Arc::ptr_eq(f, &cancel_flag));
                });
            }
        });

        Self {
            tx: job_tx,
            rx: evt_rx,
            cancel_flags,
        }
    }

    fn execute_job<F>(
        backend: &BackendManager,
        job: Job,
        progress_cb: F,
        cancel: Arc<AtomicBool>,
    ) -> Result<JobResult, crate::core::error::ArkxError>
    where
        F: Fn(ProgressInfo) + Send + 'static,
    {
        // Wrap progress with a cancel check.
        let wrapped = move |info: ProgressInfo| {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            progress_cb(info);
        };

        match job.kind {
            JobKind::List { path, password } => {
                let info = match password.as_deref() {
                    Some(p) => backend.list_with_password(&path, p)?,
                    None => backend.detect_and_list(&path)?,
                };
                Ok(JobResult::List(info))
            }
            JobKind::Extract {
                archive,
                dest,
                entries,
                password,
            } => {
                backend.extract(
                    &archive,
                    &dest,
                    entries.as_deref(),
                    password.as_deref(),
                    Some(Box::new(wrapped)),
                )?;
                Ok(JobResult::Extract)
            }
            JobKind::Add {
                archive,
                sources,
                password,
            } => {
                backend.add(
                    &archive,
                    &sources,
                    password.as_deref(),
                    Some(Box::new(wrapped)),
                )?;
                Ok(JobResult::Add)
            }
            JobKind::Remove {
                archive,
                entries,
                password,
            } => {
                backend.remove(
                    &archive,
                    &entries,
                    password.as_deref(),
                    Some(Box::new(wrapped)),
                )?;
                Ok(JobResult::Remove)
            }
            JobKind::NewFolder {
                archive,
                name,
                password,
            } => {
                backend.new_folder(&archive, &name, password.as_deref())?;
                Ok(JobResult::Add)
            }
            JobKind::Rename {
                archive,
                old_name,
                new_name,
                password,
            } => {
                backend.rename(
                    &archive,
                    &old_name,
                    &new_name,
                    password.as_deref(),
                    Some(Box::new(wrapped)),
                )?;
                Ok(JobResult::Rename)
            }
            JobKind::Test {
                archive,
                entries,
                password,
            } => {
                let report = backend.test(&archive, entries.as_deref(), password.as_deref())?;
                Ok(JobResult::Test(report))
            }
            JobKind::OpenWith {
                archive,
                entry,
                password,
            } => {
                let temp_dir = crate::core::util::open_with_dir()?;
                let path = backend.open_with(&archive, &entry, password.as_deref(), &temp_dir)?;
                // Launch the system default handler for the extracted entry.
                let spawned = std::process::Command::new("xdg-open")
                    .arg(&path)
                    .spawn()
                    .map_err(|e| ArkxError::Backend(format!("Cannot run xdg-open: {}", e)))?;
                let _ = spawned;
                Ok(JobResult::OpenWith(path))
            }
            JobKind::SecureDelete {
                archive,
                entries,
                passes,
                password,
            } => {
                backend.secure_delete(
                    &archive,
                    &entries,
                    passes,
                    password.as_deref(),
                    Some(Box::new(wrapped)),
                )?;
                Ok(JobResult::SecureDelete)
            }
            JobKind::SetComment { archive, comment } => {
                backend.set_comment(&archive, &comment)?;
                Ok(JobResult::Comment)
            }
            JobKind::Paste {
                archive,
                dest,
                source_archive,
                entries,
                cut,
                password,
            } => {
                let dest_dir = dest.trim_end_matches('/');
                // Cut+paste onto the original folder is a no-op.
                if cut
                    && entries
                        .iter()
                        .all(|e| e.rsplit_once('/').map(|(d, _)| d).unwrap_or("") == dest_dir)
                {
                    return Ok(JobResult::Paste);
                }
                let temp_dir =
                    std::env::temp_dir().join(format!("arkx-paste-{}", std::process::id()));
                std::fs::create_dir_all(&temp_dir).map_err(ArkxError::Io)?;
                let result = (|| -> std::result::Result<(), ArkxError> {
                    backend.extract(
                        &source_archive,
                        &temp_dir,
                        Some(&entries),
                        password.as_deref(),
                        None,
                    )?;
                    let mut sources: Vec<(PathBuf, String)> = Vec::new();
                    for e in &entries {
                        let leaf: &str = e.rsplit('/').next().unwrap_or(e);
                        let tmp_path = temp_dir.join(e);
                        let new_name = if cut {
                            leaf.to_string()
                        } else {
                            let base = std::path::Path::new(leaf);
                            let stem = base
                                .file_stem()
                                .unwrap_or(base.as_os_str())
                                .to_string_lossy();
                            match base.extension() {
                                Some(ext) => format!("{} copy.{}", stem, ext.to_string_lossy()),
                                None => format!("{} copy", stem),
                            }
                        };
                        let internal = if dest_dir.is_empty() {
                            new_name
                        } else {
                            format!("{}/{}", dest_dir, new_name)
                        };
                        sources.push((tmp_path, internal));
                    }
                    backend.add(&archive, &sources, password.as_deref(), None)?;
                    if cut {
                        backend.remove(&archive, &entries, password.as_deref(), None)?;
                    }
                    Ok(())
                })();
                std::fs::remove_dir_all(&temp_dir).ok();
                result?;
                Ok(JobResult::Paste)
            }
        }
    }

    pub fn submit(&mut self, kind: JobKind) {
        let job = Job { kind };
        let _ = self.tx.send(job);
    }

    pub fn try_recv(&self) -> Option<WorkerEvent> {
        self.rx.try_recv().ok()
    }

    pub fn cancel_all(&self) {
        for f in self.cancel_flags.lock().expect("pool cancel flags").iter() {
            f.store(true, Ordering::Relaxed);
        }
    }
}

impl Default for WorkerPool {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

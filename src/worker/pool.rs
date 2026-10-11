use crate::core::archive::ProgressInfo;
use crate::core::backends::BackendManager;
use crate::core::error::ArkxError;
use crate::worker::{Job, JobKind, JobResult};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
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
        /// Job kind that failed, same labels as `Started`. The GUI needs it:
        /// a cancelled job and a superseded background check both arrive here,
        /// and only the kind tells them apart.
        kind: String,
        err: ArkxError,
    },
}

pub struct WorkerPool {
    tx: Sender<Job>,
    rx: Receiver<WorkerEvent>,
    /// Live cancel flags, one per running job: cancelling must not reset the
    /// flag of a sibling job when this one finishes.
    cancel_flags: Arc<Mutex<Vec<Arc<AtomicBool>>>>,
    /// Cancel generation, bumped by `cancel_all`. Jobs carry the generation
    /// they were submitted in, so a cancel drops the jobs already queued
    /// without poisoning the ones submitted afterwards (a plain latch would).
    epoch: Arc<AtomicU64>,
    /// Cancel flag of the running background job (the open-time integrity
    /// check), so a foreground job can supersede it without touching anything
    /// else — two full passes over one archive only fight for the same disk.
    background: Arc<Mutex<Option<Arc<AtomicBool>>>>,
}

impl WorkerPool {
    pub fn new() -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (evt_tx, evt_rx) = mpsc::channel::<WorkerEvent>();
        let cancel_flags = Arc::new(Mutex::new(Vec::<Arc<AtomicBool>>::new()));
        let epoch = Arc::new(AtomicU64::new(0));
        let background = Arc::new(Mutex::new(None::<Arc<AtomicBool>>));

        // Dispatcher thread — per-job threads so a pipe deadlock (7z) never
        // blocks dispatch. Progress stays byte-based (see backends).
        let flags = cancel_flags.clone();
        let current_epoch = epoch.clone();
        let bg_track = background.clone();
        thread::spawn(move || {
            while let Ok(Job { kind: job, epoch }) = job_rx.recv() {
                let kind_str = match &job {
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
                    JobKind::EntryHash { .. } => "hash",
                }
                .to_string();
                if epoch < current_epoch.load(Ordering::Relaxed) {
                    let _ = evt_tx.send(WorkerEvent::Error {
                        kind: kind_str,
                        err: ArkxError::Cancelled,
                    });
                    continue;
                }
                let cancel_flag = Arc::new(AtomicBool::new(false));
                flags
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(cancel_flag.clone());
                // Only the open-time integrity check runs in the background;
                // everything else is foreground and may supersede it.
                let is_background = kind_str == "test";
                if is_background {
                    *bg_track.lock().unwrap_or_else(|e| e.into_inner()) = Some(cancel_flag.clone());
                }
                let _ = evt_tx.send(WorkerEvent::Started {
                    kind: kind_str.clone(),
                });
                let evt_tx_clone = evt_tx.clone();
                let evt_tx_inner = evt_tx.clone();
                let job_flags = flags.clone();
                let job_bg = bg_track.clone();
                // Run the job on its own thread so dispatch never blocks.
                thread::spawn(move || {
                    // Share the job's cancel flag with the backend so `Cancel`
                    // genuinely aborts extract/create child processes, not just
                    // the progress events.
                    let backend = BackendManager::with_cancel(cancel_flag.clone());
                    let cancel_for_job = cancel_flag.clone();
                    let result = Self::execute_job(
                        &backend,
                        Job { kind: job, epoch },
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
                                    kind: kind_str,
                                    err: ArkxError::Cancelled,
                                });
                            } else {
                                let _ = evt_tx_inner.send(WorkerEvent::Finished { result: Err(e) });
                            }
                        }
                    }
                    job_flags
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .retain(|f| !Arc::ptr_eq(f, &cancel_flag));
                    if is_background {
                        let mut bg = job_bg.lock().unwrap_or_else(|e| e.into_inner());
                        if bg.as_ref().is_some_and(|f| Arc::ptr_eq(f, &cancel_flag)) {
                            *bg = None;
                        }
                    }
                });
            }
        });

        Self {
            tx: job_tx,
            rx: evt_rx,
            cancel_flags,
            epoch,
            background,
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
                total_bytes,
                total_entries,
            } => {
                backend.extract_with_total(
                    &archive,
                    &dest,
                    entries.as_deref(),
                    password.as_deref(),
                    total_bytes.zip(total_entries),
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
                Ok(JobResult::NewFolder)
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
            } => match backend.test(&archive, entries.as_deref(), password.as_deref()) {
                Ok(report) => Ok(JobResult::Test(report)),
                // Only a format the backend cannot verify is a quiet skip: a
                // corrupt archive must surface, or the user reads "no report"
                // as "the archive is fine".
                Err(ArkxError::UnsupportedFormat(_)) => Ok(JobResult::TestSkipped),
                Err(e) => Err(e),
            },
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
            JobKind::EntryHash {
                archive,
                entry,
                password,
            } => {
                let h = backend.entry_hashes(&archive, &entry, password.as_deref())?;
                Ok(JobResult::EntryHash {
                    entry,
                    sha256: h.sha256,
                    md5: h.md5,
                })
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
                let temp_dir = crate::core::util::TaskTempDir::new("arkx-paste")?;
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
                        // `e` comes from the untrusted archive: refuse it before
                        // it becomes a staging path.
                        let tmp_path = BackendManager::extracted_entry_path(&temp_dir, e)
                            .ok_or_else(|| ArkxError::InvalidInput(e.clone()))?;
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
                result?;
                Ok(JobResult::Paste)
            }
        }
    }

    pub fn submit(&mut self, kind: JobKind) {
        // A foreground job supersedes the background integrity check: it would
        // read the very archive this one is about to read or rewrite.
        self.cancel_background();
        let epoch = self.epoch.load(Ordering::Relaxed);
        let _ = self.tx.send(Job { kind, epoch });
    }

    pub fn try_recv(&self) -> Option<WorkerEvent> {
        self.rx.try_recv().ok()
    }

    /// Stop the background job (the open-time integrity check) only. No-op
    /// when none is running: the flag is cleared by its own job thread.
    pub fn cancel_background(&self) {
        if let Some(f) = self
            .background
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            f.store(true, Ordering::Relaxed);
        }
    }

    pub fn cancel_all(&self) {
        // Generation bump, not a latch: jobs submitted after this cancel must
        // still run, and the ones already queued must not.
        self.epoch.fetch_add(1, Ordering::Relaxed);
        for f in self
            .cancel_flags
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
        {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_test_skips_an_unverifiable_format() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("disc.iso");
        std::fs::write(&archive, b"payload").unwrap();
        let result = WorkerPool::execute_job(
            &BackendManager::new(),
            Job {
                kind: JobKind::Test {
                    archive,
                    entries: None,
                    password: None,
                },
                epoch: 0,
            },
            |_| {},
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert!(matches!(result, JobResult::TestSkipped));
    }

    #[test]
    fn automatic_test_reports_a_corrupt_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("broken.zip");
        std::fs::write(&archive, b"not a zip").unwrap();
        let result = WorkerPool::execute_job(
            &BackendManager::new(),
            Job {
                kind: JobKind::Test {
                    archive,
                    entries: None,
                    password: None,
                },
                epoch: 0,
            },
            |_| {},
            Arc::new(AtomicBool::new(false)),
        );
        assert!(
            result.is_err(),
            "a corrupt archive must surface, not vanish into a skip"
        );
    }

    #[test]
    fn a_cancel_does_not_poison_the_pool() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("plain.txt");
        std::fs::write(&archive, b"x").unwrap();
        let mut pool = WorkerPool::new();
        pool.cancel_all();
        pool.submit(JobKind::SetComment {
            archive,
            comment: "after a cancel".to_string(),
        });
        // A latch would drop this job: Started is only sent for accepted jobs.
        let started = std::thread::spawn(move || {
            for _ in 0..200 {
                if let Some(WorkerEvent::Started { .. }) = pool.try_recv() {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            false
        })
        .join()
        .unwrap();
        assert!(started, "job submitted after cancel_all never ran");
    }
}

use crate::core::archive::ProgressInfo;
use crate::core::backends::BackendManager;
use crate::worker::{Job, JobKind, JobResult};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;

/// Non-blocking worker pool: one dispatcher thread + channel.
/// The UI submits jobs via Sender and receives progress/results via Receiver.
/// Each job runs on its own thread with cancellation via AtomicBool.

#[derive(Debug, Clone)]
pub enum WorkerEvent {
    Started { kind: String },
    Progress { info: ProgressInfo },
    Finished { result: Result<JobResult, String> },
    Error { msg: String },
}

pub struct WorkerPool {
    tx: Sender<Job>,
    rx: Receiver<WorkerEvent>,
    cancel_flag: Arc<AtomicBool>,
}

impl WorkerPool {
    pub fn new() -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (evt_tx, evt_rx) = mpsc::channel::<WorkerEvent>();
        let cancel_flag = Arc::new(AtomicBool::new(false));

        // Dispatcher thread — per-job threads so a pipe deadlock (7z) never
        // blocks dispatch. Progress stays byte-based (see backends).
        let cancel_clone = cancel_flag.clone();
        thread::spawn(move || {
            while let Ok(job) = job_rx.recv() {
                let kind_str = match &job.kind {
                    JobKind::List { .. } => "list",
                    JobKind::Extract { .. } => "extract",
                    JobKind::Add { .. } => "add",
                    JobKind::Remove { .. } => "remove",
                }
                .to_string();
                let _ = evt_tx.send(WorkerEvent::Started { kind: kind_str });
                let evt_tx_clone = evt_tx.clone();
                let evt_tx_inner = evt_tx.clone();
                let cancel_flag_inner = cancel_clone.clone();
                // Run the job on its own thread so dispatch never blocks.
                thread::spawn(move || {
                    let backend = BackendManager::new();
                    let cancel_for_job = cancel_flag_inner.clone();
                    let result = Self::execute_job(
                        &backend,
                        job,
                        move |info| {
                            if cancel_for_job.load(Ordering::Relaxed) {
                                return;
                            }
                            let _ = evt_tx_clone.send(WorkerEvent::Progress { info });
                        },
                        cancel_flag_inner.clone(),
                    );

                    match result {
                        Ok(res) => {
                            cancel_flag_inner.store(false, Ordering::Relaxed);
                            let _ = evt_tx_inner.send(WorkerEvent::Finished { result: Ok(res) });
                        }
                        Err(e) => {
                            let msg = e.to_string();
                            // Snapshot the cancel state BEFORE clearing it, so the
                            // next job after a cancel is not mistaken for one.
                            let was_cancelled = msg.contains("Cancelled")
                                || cancel_flag_inner.load(Ordering::Relaxed);
                            cancel_flag_inner.store(false, Ordering::Relaxed);
                            if was_cancelled {
                                let _ = evt_tx_inner.send(WorkerEvent::Error {
                                    msg: "Cancelled".into(),
                                });
                            } else {
                                let _ =
                                    evt_tx_inner.send(WorkerEvent::Finished { result: Err(msg) });
                            }
                        }
                    }
                });
            }
        });

        Self {
            tx: job_tx,
            rx: evt_rx,
            cancel_flag,
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
            JobKind::List { path } => {
                let info = backend.detect_and_list(&path)?;
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
        self.cancel_flag.store(true, Ordering::Relaxed);
    }
}

impl Default for WorkerPool {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
    }
}

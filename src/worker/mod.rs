//! Background job execution.
//!
//! Defines the job types ([`job`]) and the shared worker pool ([`pool`]) that
//! runs archive operations off the UI thread and keeps them cancellable.

pub mod job;
pub mod pool;

pub use job::{Job, JobKind, JobResult};
pub use pool::{WorkerEvent, WorkerPool};

//! File operations: jobs running on the main loop over async GIO, with progress, conflicts and undo.

pub mod archive;
mod conflict;
mod job;
mod manager;
mod walk;

pub use job::{Job, JobKind, JobStatus, name};
pub use manager::JobManager;
pub(crate) use walk::children;

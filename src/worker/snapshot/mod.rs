use uuid::Uuid;

use crate::contracts::worker::WorkerAssignment;

mod export;
mod manager;
mod remote;

#[cfg(test)]
mod tests;

pub use export::WorldArchive;
pub use remote::WorkerObjectClient;

#[derive(Clone)]
pub struct SnapshotManager {
    pub(super) store: std::sync::Arc<dyn object_store::ObjectStore>,
    pub(super) worker_client: WorkerObjectClient,
    pub(super) worker_assignment: Option<(Uuid, WorkerAssignment)>,
}

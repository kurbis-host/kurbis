use std::sync::Arc;

use object_store::ObjectStore;
use sqlx::PgPool;

mod export;
mod gc;
mod manager;

pub use export::WorldArchive;
pub use gc::GarbageCollection;

#[derive(Clone)]
pub struct SnapshotStore {
    pool: PgPool,
    store: Arc<dyn ObjectStore>,
}

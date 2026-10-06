use object_store::{ObjectStore, path::Path};

/// Delete an object idempotently. Backends may report `NotFound` for a key
/// removed by an earlier attempt or external cleanup; treating that as success
/// lets durable cleanup progress to its database acknowledgement.
pub async fn delete_if_present(store: &dyn ObjectStore, path: &Path) -> object_store::Result<()> {
    match store.delete(path).await {
        Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_local_object_deletion_is_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            object_store::local::LocalFileSystem::new_with_prefix(directory.path()).unwrap();
        let path = Path::from("missing/world.tar.gz");
        delete_if_present(&store, &path).await.unwrap();
        delete_if_present(&store, &path).await.unwrap();
    }
}

use super::storage::ensure_storage_limit;
use crate::model::SERVER_STORAGE_BYTES;

#[test]
fn storage_limit_rejects_overage() {
    assert!(ensure_storage_limit(SERVER_STORAGE_BYTES).is_ok());
    assert!(ensure_storage_limit(SERVER_STORAGE_BYTES + 1).is_err());
}

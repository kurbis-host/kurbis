use crate::snapshot::files::safe_relative_path;

#[test]
fn rejects_snapshot_path_traversal() {
    assert!(safe_relative_path("../secret").is_err());
    assert!(safe_relative_path("/etc/passwd").is_err());
    assert!(safe_relative_path("world/region/r.0.0.mca").is_ok());
}

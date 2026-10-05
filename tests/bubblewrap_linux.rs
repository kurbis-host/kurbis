#![cfg(target_os = "linux")]

use std::path::PathBuf;

use kurbis_hoster::ci::execution::{
    BUBBLEWRAP_GUARANTEE, BubblewrapBuildRuntime, CgroupDelegation,
};

/// Exercises the real namespace probe when a Linux worker publishes its
/// operator-owned immutable fixtures. The test is intentionally a no-op on
/// developer machines without the fixture paths; production preflight must
/// still fail closed rather than silently selecting Docker.
#[tokio::test]
async fn real_bubblewrap_namespace_probe_reports_cgroup_status() {
    let Some(bwrap) = std::env::var_os("CI_BUBBLEWRAP_PATH") else {
        return;
    };
    let Some(rootfs) = std::env::var_os("CI_BUBBLEWRAP_ROOTFS") else {
        return;
    };
    let Some(toolchain) = std::env::var_os("CI_BUBBLEWRAP_TOOLCHAIN") else {
        return;
    };
    let Some(preparer) = std::env::var_os("CI_BUBBLEWRAP_SOURCE_PREPARER") else {
        return;
    };
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is required"));
    let workspace = home.join(".cache/kurbis/bubblewrap-integration");
    let runtime = BubblewrapBuildRuntime::new(
        bwrap,
        rootfs,
        toolchain,
        workspace,
        preparer,
        "operator-toolchain",
    )
    .expect("configured bubblewrap fixtures must pass fail-closed validation");
    let report = runtime
        .preflight()
        .await
        .expect("bubblewrap namespace and network invariants must hold");
    assert_eq!(report.guarantee, BUBBLEWRAP_GUARANTEE);
    assert!(!report.bwrap_version.is_empty());
    assert_eq!(report.rootfs_manifest.len(), 64);
    assert_eq!(report.toolchain_manifest.len(), 64);
    assert!(matches!(
        report.cgroup_delegation,
        CgroupDelegation::Available | CgroupDelegation::Missing | CgroupDelegation::Unavailable
    ));
}

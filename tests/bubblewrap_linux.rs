#![cfg(target_os = "linux")]

use std::path::PathBuf;

use kurbis_hoster::ci::execution::{
    BUBBLEWRAP_GUARANTEE, BubblewrapBuildRuntime, CgroupDelegation,
};

/// Exercises the real namespace probe when a Linux worker publishes its
/// operator-owned immutable fixtures. The test is intentionally a no-op on
/// developer machines without Bubblewrap configuration; once Bubblewrap is
/// requested or any fixture is configured, every fixture and the exact mounted
/// workspace must be present. Production preflight still fails closed rather
/// than silently selecting Docker.
#[tokio::test]
async fn real_bubblewrap_namespace_probe_reports_cgroup_status() {
    let fixture_variables = [
        "CI_BUBBLEWRAP_PATH",
        "CI_BUBBLEWRAP_ROOTFS",
        "CI_BUBBLEWRAP_TOOLCHAIN",
        "CI_BUBBLEWRAP_WORKSPACE",
        "CI_BUBBLEWRAP_SOURCE_PREPARER",
    ];
    let bubblewrap_requested = std::env::var("CI_BUILDER_BACKEND").as_deref() == Ok("bubblewrap");
    let fixtures_configured = fixture_variables
        .iter()
        .any(|name| std::env::var_os(name).is_some());
    if !bubblewrap_requested && !fixtures_configured {
        return;
    }

    let bwrap = std::env::var_os("CI_BUBBLEWRAP_PATH")
        .expect("CI_BUBBLEWRAP_PATH is required for Bubblewrap integration tests");
    let rootfs = std::env::var_os("CI_BUBBLEWRAP_ROOTFS")
        .expect("CI_BUBBLEWRAP_ROOTFS is required for Bubblewrap integration tests");
    let toolchain = std::env::var_os("CI_BUBBLEWRAP_TOOLCHAIN")
        .expect("CI_BUBBLEWRAP_TOOLCHAIN is required for Bubblewrap integration tests");
    let workspace = PathBuf::from(
        std::env::var_os("CI_BUBBLEWRAP_WORKSPACE")
            .expect("CI_BUBBLEWRAP_WORKSPACE is required for Bubblewrap integration tests"),
    );
    let preparer = std::env::var_os("CI_BUBBLEWRAP_SOURCE_PREPARER")
        .expect("CI_BUBBLEWRAP_SOURCE_PREPARER is required for Bubblewrap integration tests");
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

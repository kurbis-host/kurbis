use super::*;
use async_trait::async_trait;
use tempfile::tempdir;

#[derive(Clone)]
struct StubBuildRuntime {
    toolchain: String,
    report: BuildProcessReport,
}
#[async_trait]
impl RootlessBuildRuntime for StubBuildRuntime {
    fn toolchain_version(&self) -> &str {
        &self.toolchain
    }
    async fn verify_sandbox(&self, _: SandboxLimits) -> Result<(), BuildPipelineError> {
        Ok(())
    }
    async fn compile(
        &self,
        _: &[u8],
        _: &[u8],
        out: &Path,
        _: &str,
        _: SandboxLimits,
    ) -> Result<BuildProcessReport, BuildPipelineError> {
        fs::write(out.join("pumpkin"), fake_elf(62)).unwrap();
        fs::write(out.join("kurbis-control.wasm"), b"\0asm\x01\0\0\0code").unwrap();
        Ok(self.report.clone())
    }
}

fn fake_elf(machine: u16) -> Vec<u8> {
    let mut bytes = vec![0; 64];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes
}

#[test]
fn builder_identity_binds_compiler_and_target_to_the_exact_image_digest() {
    let image = format!("registry.example/ci/rust@sha256:{}", "f".repeat(64));
    let identity = toolchain_identity(&image, "1.98.1").unwrap();
    assert_eq!(identity, format!("rustc-1.98.1@sha256:{}", "f".repeat(64)));
    assert!(toolchain_identity("rust:1.98", "1.98.1").is_err());
    assert!(rustc_release_from_identity("registry.example/ci/rust@sha256:abc", &identity).is_err());
    assert_eq!(
        docker_platform("x86_64-unknown-linux-gnu").unwrap(),
        "linux/amd64"
    );
    assert_eq!(
        docker_platform("aarch64-unknown-linux-gnu").unwrap(),
        "linux/arm64"
    );
    assert!(docker_platform("aarch64-apple-darwin").is_err());
}

#[test]
fn target_mapping_rejects_non_linux_and_unknown_architectures() {
    assert_eq!(
        rust_target("x86_64-linux").unwrap(),
        "x86_64-unknown-linux-gnu"
    );
    assert_eq!(
        rust_target("aarch64-linux").unwrap(),
        "aarch64-unknown-linux-gnu"
    );
    assert!(rust_target("aarch64-darwin").is_err());
    assert!(rust_target("mips-linux").is_err());
}

#[tokio::test]
async fn controlled_builder_rejects_a_toolchain_cache_key_mismatch() {
    let root = tempdir().unwrap();
    let plugin = root.path().join("plugins/kurbis-control");
    fs::create_dir_all(plugin.join("src")).unwrap();
    fs::write(
        plugin.join("Cargo.toml"),
        "[package]\nname='kurbis-control'\n",
    )
    .unwrap();
    fs::write(plugin.join("Cargo.lock"), "version = 4\n").unwrap();
    let runtime = Arc::new(StubBuildRuntime {
        toolchain: "rust-1.98@sha256:test".into(),
        report: BuildProcessReport::default(),
    });
    let builder = RustPumpkinBuilder::new(runtime, &plugin).unwrap();
    let key = BuildKey {
        repository: "kurbis-host/Pumpkin".into(),
        commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
        patch_fingerprint: "b".repeat(64),
        toolchain_version: "native".into(),
        target_architecture: "x86_64-linux".into(),
    };
    assert!(
        builder
            .compile_and_package(Vec::new(), &key)
            .await
            .unwrap_err()
            .to_string()
            .contains("toolchain identity")
    );
}

#[tokio::test]
async fn package_contains_only_validated_runtime_files_and_metadata() {
    let root = tempdir().unwrap();
    let binary = root.path().join("pumpkin");
    let plugin = root.path().join("plugin.wasm");
    fs::write(&binary, fake_elf(62)).unwrap();
    fs::write(&plugin, b"\0asm\x01\0\0\0code").unwrap();
    let key = BuildKey {
        repository: "kurbis-host/Pumpkin".into(),
        commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
        patch_fingerprint: "b".repeat(64),
        toolchain_version: "rust-1.98@sha256:abc".into(),
        target_architecture: "x86_64-linux".into(),
    };
    validate_pumpkin_binary(&binary, "x86_64-linux").unwrap();
    validate_wasm_plugin(&plugin).unwrap();
    let artifact = package_artifact(&binary, &plugin, root.path(), &key, "cargo ok\n").unwrap();
    assert!(artifact.starts_with(&[0x1f, 0x8b]));
    assert!(artifact.len() < MAX_ARTIFACT_BYTES);
}

#[test]
fn executable_log_capture_is_bounded_to_the_tail() {
    assert_eq!(tail(&"x".repeat(20), 8), "x".repeat(8));
}

#[test]
fn only_the_compilation_workspace_explicitly_allows_exec() {
    assert!(
        workspace_tmpfs_options(true)
            .split(',')
            .any(|flag| flag == "exec")
    );
    assert!(
        workspace_tmpfs_options(false)
            .split(',')
            .any(|flag| flag == "noexec")
    );
    assert!(
        !workspace_tmpfs_options(false)
            .split(',')
            .any(|flag| flag == "exec")
    );
}

use std::{fs, io::Read, path::Path};

use flate2::{Compression, write::GzEncoder};
use serde::Serialize;

use super::super::archive::MAX_ARTIFACT_BYTES;
use crate::ci::{BuildKey, BuildPipelineError};

pub(super) fn rust_target(architecture: &str) -> Result<&'static str, BuildPipelineError> {
    match architecture {
        "x86_64-linux" | "x86_64-unknown-linux-gnu" => Ok("x86_64-unknown-linux-gnu"),
        "aarch64-linux" | "aarch64-unknown-linux-gnu" => Ok("aarch64-unknown-linux-gnu"),
        _ => Err(BuildPipelineError::Compile(format!(
            "unsupported native Linux build architecture {architecture}"
        ))),
    }
}

pub(super) fn validate_pumpkin_binary(
    path: &Path,
    architecture: &str,
) -> Result<(), BuildPipelineError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        BuildPipelineError::Package(format!("Pumpkin build output is missing: {error}"))
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() == 0 {
        return Err(BuildPipelineError::Package(
            "Pumpkin output is not a regular non-empty file".into(),
        ));
    }
    if metadata.len() > MAX_ARTIFACT_BYTES as u64 {
        return Err(BuildPipelineError::Package(
            "Pumpkin output exceeds the artifact size limit".into(),
        ));
    }
    let mut file = fs::File::open(path)
        .map_err(|error| BuildPipelineError::Package(format!("read Pumpkin output: {error}")))?;
    let mut header = [0u8; 20];
    file.read_exact(&mut header).map_err(|error| {
        BuildPipelineError::Package(format!("Pumpkin output is truncated: {error}"))
    })?;
    let machine = u16::from_le_bytes([header[18], header[19]]);
    let expected = match architecture {
        "x86_64-linux" | "x86_64-unknown-linux-gnu" => 62,
        "aarch64-linux" | "aarch64-unknown-linux-gnu" => 183,
        _ => {
            return Err(BuildPipelineError::Package(
                "unsupported native Linux target".into(),
            ));
        }
    };
    if &header[..4] != b"\x7fELF" || header[4] != 2 || header[5] != 1 || machine != expected {
        return Err(BuildPipelineError::Package(
            "Pumpkin output is not a 64-bit little-endian ELF for the requested architecture"
                .into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_wasm_plugin(path: &Path) -> Result<(), BuildPipelineError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        BuildPipelineError::Package(format!("Kurbis control plugin is missing: {error}"))
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() < 8 {
        return Err(BuildPipelineError::Package(
            "Kurbis control output is not a valid WebAssembly file".into(),
        ));
    }
    let mut file = fs::File::open(path).map_err(|error| {
        BuildPipelineError::Package(format!("read Kurbis control plugin: {error}"))
    })?;
    let mut header = [0u8; 4];
    file.read_exact(&mut header).map_err(|error| {
        BuildPipelineError::Package(format!("Kurbis control plugin is truncated: {error}"))
    })?;
    if header != *b"\0asm" {
        return Err(BuildPipelineError::Package(
            "Kurbis control output has an invalid Wasm header".into(),
        ));
    }
    Ok(())
}

#[derive(Serialize)]
struct ArtifactMetadata<'a> {
    format_version: u32,
    repository: &'a str,
    commit_sha: &'a str,
    patch_fingerprint: &'a str,
    toolchain_version: &'a str,
    target_architecture: &'a str,
}

pub(super) fn package_artifact(
    binary: &Path,
    plugin: &Path,
    build_output: &Path,
    key: &BuildKey,
    build_log: &str,
) -> Result<Vec<u8>, BuildPipelineError> {
    let metadata = ArtifactMetadata {
        format_version: 1,
        repository: &key.repository,
        commit_sha: &key.commit_sha,
        patch_fingerprint: &key.patch_fingerprint,
        toolchain_version: &key.toolchain_version,
        target_architecture: &key.target_architecture,
    };
    let metadata = serde_json::to_vec(&metadata).map_err(|error| {
        BuildPipelineError::Package(format!("serialize artifact metadata: {error}"))
    })?;
    let mut output = Vec::new();
    let encoder = GzEncoder::new(&mut output, Compression::default());
    let mut tar = tar::Builder::new(encoder);
    tar.mode(tar::HeaderMode::Deterministic);
    for (name, path, bytes) in [
        ("pumpkin", Some(binary), None),
        ("kurbis-control.wasm", Some(plugin), None),
        ("metadata.json", None, Some(metadata.as_slice())),
        ("BUILD.log", None, Some(build_log.as_bytes())),
        (
            "LICENSE",
            None,
            Some(include_bytes!("../../../../runtime/pumpkin/LICENSE").as_slice()),
        ),
        (
            "REVISION",
            None,
            Some(include_bytes!("../../../../runtime/pumpkin/REVISION").as_slice()),
        ),
    ] {
        let size = match (path, bytes) {
            (Some(path), _) => fs::metadata(path)
                .map_err(|error| {
                    BuildPipelineError::Package(format!("inspect artifact {name}: {error}"))
                })?
                .len(),
            (_, Some(bytes)) => bytes.len() as u64,
            _ => 0,
        };
        let mut header = tar::Header::new_gnu();
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_mode(if name == "pumpkin" { 0o755 } else { 0o644 });
        header.set_size(size);
        header.set_cksum();
        let result = if let Some(path) = path {
            tar.append_data(
                &mut header,
                name,
                fs::File::open(path).map_err(|error| {
                    BuildPipelineError::Package(format!("open artifact {name}: {error}"))
                })?,
            )
        } else {
            tar.append_data(&mut header, name, bytes.unwrap_or_default())
        };
        result.map_err(|error| {
            BuildPipelineError::Package(format!("package artifact {name}: {error}"))
        })?;
    }
    let encoder = tar
        .into_inner()
        .map_err(|error| BuildPipelineError::Package(format!("finish artifact tar: {error}")))?;
    encoder
        .finish()
        .map_err(|error| BuildPipelineError::Package(format!("finish artifact gzip: {error}")))?;
    if output.is_empty() || output.len() > MAX_ARTIFACT_BYTES {
        return Err(BuildPipelineError::Package(
            "packaged artifact exceeds its size limit".into(),
        ));
    }
    let _ = build_output;
    Ok(output)
}

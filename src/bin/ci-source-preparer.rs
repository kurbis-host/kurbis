use std::{
    fs::OpenOptions,
    io::{Read, Write},
    path::Path,
};

use anyhow::{Context, ensure};
use kurbis_hoster::ci::{CheckedInPumpkinPatcher, MAX_COMPRESSED_SOURCE_BYTES, PatchApplier};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    kurbis_hoster::init_tracing("kurbis_hoster=warn");
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.len() == 1 && arguments[0] == "--print-fingerprint" {
        println!("{}", kurbis_hoster::ci::checked_in_patch_fingerprint());
        return Ok(());
    }
    ensure!(
        arguments.len() == 3,
        "usage: ci-source-preparer SOURCE.tar.gz OUTPUT.tar.gz PATCH_FINGERPRINT (or --print-fingerprint)"
    );
    let input = Path::new(&arguments[0]);
    let output = Path::new(&arguments[1]);
    let fingerprint = arguments[2]
        .to_str()
        .context("patch fingerprint must be UTF-8")?;
    let metadata = std::fs::symlink_metadata(input)
        .with_context(|| format!("inspect source archive {}", input.display()))?;
    ensure!(
        metadata.file_type().is_file() && metadata.len() <= MAX_COMPRESSED_SOURCE_BYTES as u64,
        "source archive is not a regular file or exceeds the compressed-size limit"
    );
    let mut archive = Vec::with_capacity(metadata.len() as usize);
    let mut source_file = OpenOptions::new();
    source_file.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        source_file.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    source_file
        .open(input)
        .with_context(|| format!("open source archive {}", input.display()))?
        .take(MAX_COMPRESSED_SOURCE_BYTES as u64 + 1)
        .read_to_end(&mut archive)
        .context("read source archive")?;
    ensure!(
        archive.len() as u64 == metadata.len() && archive.len() <= MAX_COMPRESSED_SOURCE_BYTES,
        "source archive changed or exceeded its compressed-size limit"
    );

    let patched = CheckedInPumpkinPatcher
        .apply(archive, fingerprint)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;
    ensure!(
        !patched.is_empty() && patched.len() <= MAX_COMPRESSED_SOURCE_BYTES,
        "patched source archive exceeds the compressed-size limit"
    );
    let mut destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .with_context(|| format!("create patched source archive {}", output.display()))?;
    destination.write_all(&patched)?;
    destination.sync_all()?;
    Ok(())
}

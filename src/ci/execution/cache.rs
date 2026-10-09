//! Private, bounded compiler-output cache used only as an optimization.
//!
//! The cache is never a source of truth: every hit is checked against the full
//! build identity and the restored outputs still go through the normal ELF and
//! Wasm validators. A corrupt or incomplete entry is discarded and compilation
//! continues from scratch.

use std::{
    env, fs,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

use super::archive::MAX_ARTIFACT_BYTES;
use crate::ci::BuildKey;

const CACHE_ENV: &str = "CI_BUILD_CACHE_DIR";
const CACHE_SIZE_ENV: &str = "CI_BUILD_CACHE_MAX_BYTES";
const CACHE_SCHEMA: &str = "kurbis-build-cache-v1";
const DEFAULT_CACHE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MIN_CACHE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const LOCK_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const STALE_LOCK_AFTER: Duration = Duration::from_secs(2 * 60 * 60);
const STALE_TEMP_AFTER: Duration = Duration::from_secs(2 * 60 * 60);
const PRUNE_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_METADATA_BYTES: u64 = 64 * 1024;
const MAX_BUILD_LOG_BYTES: u64 = 512 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct BuildCache {
    root: PathBuf,
    max_bytes: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct CacheIdentity {
    pub cache_key: String,
    pub source_digest: String,
    pub plugin_digest: String,
    pub runtime_digest: String,
    pub target: String,
    pub build_key: BuildKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheLookup {
    Miss,
    Hit,
    Corrupt,
}

#[derive(Debug)]
pub(crate) struct BuildCacheLease {
    path: PathBuf,
    _file: File,
}

#[derive(Debug, Serialize, Deserialize)]
struct EntryMetadata {
    schema: String,
    cache_key: String,
    source_digest: String,
    plugin_digest: String,
    runtime_digest: String,
    target: String,
    repository: String,
    commit_sha: String,
    patch_fingerprint: String,
    toolchain_version: String,
    target_architecture: String,
    pumpkin_bytes: u64,
    pumpkin_digest: String,
    plugin_bytes: u64,
    plugin_output_digest: String,
    build_log: String,
}

impl BuildCache {
    pub(crate) fn from_env() -> io::Result<Option<Self>> {
        let Some(root) = env::var_os(CACHE_ENV) else {
            return Ok(None);
        };
        let root = private_cache_root(Path::new(&root))?;
        let max_bytes = parse_cache_size(env::var(CACHE_SIZE_ENV).ok().as_deref())?;
        let entries = root.join("entries");
        let locks = root.join("locks");
        fs::create_dir_all(&entries)?;
        fs::create_dir_all(&locks)?;
        protect_directory(&entries)?;
        protect_directory(&locks)?;
        Ok(Some(Self { root, max_bytes }))
    }

    pub(crate) fn identity(
        build_key: &BuildKey,
        runtime_identity: &str,
        target: &str,
        source: &[u8],
        plugin: &[u8],
    ) -> CacheIdentity {
        let source_digest = blake3::hash(source).to_hex().to_string();
        let plugin_digest = blake3::hash(plugin).to_hex().to_string();
        let runtime_digest = blake3::hash(runtime_identity.as_bytes())
            .to_hex()
            .to_string();
        let mut hasher = blake3::Hasher::new();
        hasher.update(CACHE_SCHEMA.as_bytes());
        for field in [
            build_key.repository.as_str(),
            build_key.commit_sha.as_str(),
            build_key.patch_fingerprint.as_str(),
            build_key.toolchain_version.as_str(),
            build_key.target_architecture.as_str(),
            target,
            source_digest.as_str(),
            plugin_digest.as_str(),
            runtime_digest.as_str(),
        ] {
            hasher.update(&(field.len() as u64).to_be_bytes());
            hasher.update(field.as_bytes());
        }
        CacheIdentity {
            cache_key: hasher.finalize().to_hex().to_string(),
            source_digest,
            plugin_digest,
            runtime_digest,
            target: target.to_owned(),
            build_key: build_key.clone(),
        }
    }

    pub(crate) async fn acquire(&self, identity: &CacheIdentity) -> io::Result<BuildCacheLease> {
        let path = self.lock_path(identity);
        let root = path
            .parent()
            .ok_or_else(|| io::Error::other("cache lock path has no parent"))?;
        fs::create_dir_all(root)?;
        protect_directory(root)?;
        timeout(LOCK_TIMEOUT, async {
            loop {
                match OpenOptions::new().write(true).create_new(true).open(&path) {
                    Ok(mut file) => {
                        protect_file(&file)?;
                        writeln!(
                            file,
                            "pid={} created={}",
                            std::process::id(),
                            unix_timestamp()
                        )?;
                        file.sync_all()?;
                        return Ok(BuildCacheLease { path, _file: file });
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        if lock_is_stale(&path)? {
                            let _ = fs::remove_file(&path);
                            continue;
                        }
                        sleep(Duration::from_millis(250)).await;
                    }
                    Err(error) => return Err(error),
                }
            }
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "build cache lock timed out"))?
    }

    pub(crate) fn restore(
        &self,
        identity: &CacheIdentity,
        output_dir: &Path,
    ) -> io::Result<CacheLookup> {
        let entry = self.entry_path(identity);
        let metadata_path = entry.join("metadata.json");
        let metadata = match read_json::<EntryMetadata>(&metadata_path, MAX_METADATA_BYTES) {
            Ok(Some(metadata)) => metadata,
            Ok(None) => return Ok(CacheLookup::Miss),
            Err(_) => {
                self.remove_entry(&entry)?;
                return Ok(CacheLookup::Corrupt);
            }
        };
        if !metadata_matches(&metadata, identity) {
            self.remove_entry(&entry)?;
            return Ok(CacheLookup::Corrupt);
        }
        fs::create_dir_all(output_dir)?;
        let pumpkin = entry.join("pumpkin");
        let plugin = entry.join("kurbis-control.wasm");
        if copy_checked(
            &pumpkin,
            &output_dir.join("pumpkin"),
            metadata.pumpkin_bytes,
            &metadata.pumpkin_digest,
        )
        .is_err()
            || copy_checked(
                &plugin,
                &output_dir.join("kurbis-control.wasm"),
                metadata.plugin_bytes,
                &metadata.plugin_output_digest,
            )
            .is_err()
        {
            let _ = fs::remove_file(output_dir.join("pumpkin"));
            let _ = fs::remove_file(output_dir.join("kurbis-control.wasm"));
            self.remove_entry(&entry)?;
            return Ok(CacheLookup::Corrupt);
        }
        Ok(CacheLookup::Hit)
    }

    pub(crate) fn invalidate(&self, identity: &CacheIdentity) -> io::Result<()> {
        self.remove_entry(&self.entry_path(identity))
    }

    pub(crate) fn store(
        &self,
        identity: &CacheIdentity,
        output_dir: &Path,
        build_log: &str,
    ) -> io::Result<()> {
        let pumpkin = output_dir.join("pumpkin");
        let plugin = output_dir.join("kurbis-control.wasm");
        let pumpkin_digest = copy_input_digest(&pumpkin)?;
        let plugin_digest = copy_input_digest(&plugin)?;
        let pumpkin_bytes = fs::metadata(&pumpkin)?.len();
        let plugin_bytes = fs::metadata(&plugin)?.len();
        let metadata = EntryMetadata {
            schema: CACHE_SCHEMA.to_owned(),
            cache_key: identity.cache_key.clone(),
            source_digest: identity.source_digest.clone(),
            plugin_digest: identity.plugin_digest.clone(),
            runtime_digest: identity.runtime_digest.clone(),
            target: identity.target.clone(),
            repository: identity.build_key.repository.clone(),
            commit_sha: identity.build_key.commit_sha.clone(),
            patch_fingerprint: identity.build_key.patch_fingerprint.clone(),
            toolchain_version: identity.build_key.toolchain_version.clone(),
            target_architecture: identity.build_key.target_architecture.clone(),
            pumpkin_bytes,
            pumpkin_digest,
            plugin_bytes,
            plugin_output_digest: plugin_digest,
            build_log: bounded_log(build_log),
        };
        let entries = self.root.join("entries");
        let temporary = entries.join(format!(
            ".tmp-{}-{}",
            identity.cache_key,
            Uuid::new_v4().simple()
        ));
        fs::create_dir(&temporary)?;
        protect_directory(&temporary)?;
        let result = (|| {
            copy_atomic(&pumpkin, &temporary.join("pumpkin"))?;
            copy_atomic(&plugin, &temporary.join("kurbis-control.wasm"))?;
            write_json(&temporary.join("metadata.json"), &metadata)?;
            let entry = self.entry_path(identity);
            if entry.exists() || fs::symlink_metadata(&entry).is_ok() {
                self.remove_entry(&entry)?;
            }
            fs::rename(&temporary, entry)?;
            self.prune()
        })();
        if result.is_err() {
            let _ = self.remove_entry(&temporary);
        }
        result
    }

    fn entry_path(&self, identity: &CacheIdentity) -> PathBuf {
        self.root.join("entries").join(&identity.cache_key)
    }

    fn lock_path(&self, identity: &CacheIdentity) -> PathBuf {
        self.root
            .join("locks")
            .join(format!("{}.lock", identity.cache_key))
    }

    fn remove_entry(&self, path: &Path) -> io::Result<()> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
                fs::remove_file(path)
            }
            Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
            Ok(_) => Err(io::Error::other("unsupported build cache entry type")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn prune(&self) -> io::Result<()> {
        let _prune_lock = match self.acquire_prune_lock()? {
            Some(lock) => lock,
            None => return Ok(()),
        };
        self.prune_locked(SystemTime::now())
    }

    fn acquire_prune_lock(&self) -> io::Result<Option<BuildCacheLease>> {
        let path = self.root.join("locks/prune.lock");
        let deadline = SystemTime::now() + PRUNE_LOCK_TIMEOUT;
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    protect_file(&file)?;
                    writeln!(
                        file,
                        "pid={} created={}",
                        std::process::id(),
                        unix_timestamp()
                    )?;
                    file.sync_all()?;
                    return Ok(Some(BuildCacheLease { path, _file: file }));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(&path)? {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    if SystemTime::now() >= deadline {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn prune_locked(&self, now: SystemTime) -> io::Result<()> {
        let entries = self.root.join("entries");
        let mut candidates = Vec::new();
        let mut total = 0u64;
        for item in fs::read_dir(&entries)? {
            let item = item?;
            let path = item.path();
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.is_dir() {
                continue;
            }
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let size = directory_size(&path)?;
            total = total.saturating_add(size);
            let name = item.file_name().to_string_lossy().into_owned();
            if let Some(key) = name
                .strip_prefix(".tmp-")
                .and_then(|value| value.split_once('-').map(|pair| pair.0))
            {
                let lock = self.root.join("locks").join(format!("{key}.lock"));
                let active = match fs::metadata(&lock) {
                    Ok(_) if !lock_is_stale(&lock)? => true,
                    Ok(_) => {
                        let _ = fs::remove_file(&lock);
                        false
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                    Err(error) => return Err(error),
                };
                if !active && now.duration_since(modified).unwrap_or_default() >= STALE_TEMP_AFTER {
                    self.remove_entry(&path)?;
                    total = total.saturating_sub(size);
                }
                continue;
            }
            candidates.push((modified, path, size));
        }
        candidates.sort_by_key(|(modified, _, _)| *modified);
        for (_, path, size) in candidates {
            if total <= self.max_bytes {
                break;
            }
            let key = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if !key.is_empty() && self.root.join("locks").join(format!("{key}.lock")).exists() {
                continue;
            }
            self.remove_entry(&path)?;
            total = total.saturating_sub(size);
        }
        Ok(())
    }
}

impl Drop for BuildCacheLease {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn metadata_matches(metadata: &EntryMetadata, identity: &CacheIdentity) -> bool {
    metadata.schema == CACHE_SCHEMA
        && metadata.cache_key == identity.cache_key
        && metadata.source_digest == identity.source_digest
        && metadata.plugin_digest == identity.plugin_digest
        && metadata.runtime_digest == identity.runtime_digest
        && metadata.target == identity.target
        && metadata.repository == identity.build_key.repository
        && metadata.commit_sha == identity.build_key.commit_sha
        && metadata.patch_fingerprint == identity.build_key.patch_fingerprint
        && metadata.toolchain_version == identity.build_key.toolchain_version
        && metadata.target_architecture == identity.build_key.target_architecture
}

fn copy_checked(
    source: &Path,
    destination: &Path,
    expected_size: u64,
    expected_digest: &str,
) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > MAX_ARTIFACT_BYTES as u64
        || metadata.len() != expected_size
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid build cache output",
        ));
    }
    let temporary = destination.with_extension(format!("cache-{}", Uuid::new_v4().simple()));
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let mut hasher = blake3::Hasher::new();
    let copied = io::copy(
        &mut HashingReader {
            reader: &mut input,
            hasher: &mut hasher,
        },
        &mut output,
    )?;
    output.sync_all()?;
    if copied != expected_size || hasher.finalize().to_hex().as_str() != expected_digest {
        let _ = fs::remove_file(&temporary);
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "build cache digest mismatch",
        ));
    }
    fs::rename(temporary, destination)
}

struct HashingReader<'a> {
    reader: &'a mut File,
    hasher: &'a mut blake3::Hasher,
}
impl Read for HashingReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.reader.read(buffer)?;
        self.hasher.update(&buffer[..read]);
        Ok(read)
    }
}

fn copy_atomic(source: &Path, destination: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > MAX_ARTIFACT_BYTES as u64
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid build output for cache",
        ));
    }
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    io::copy(&mut input, &mut output)?;
    output.sync_all()
}

fn copy_input_digest(path: &Path) -> io::Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > MAX_ARTIFACT_BYTES as u64
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid build output for cache",
        ));
    }
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    io::copy(
        &mut file,
        &mut HashingWriter {
            hasher: &mut hasher,
        },
    )?;
    Ok(hasher.finalize().to_hex().to_string())
}

struct HashingWriter<'a> {
    hasher: &'a mut blake3::Hasher,
}
impl Write for HashingWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.hasher.update(buffer);
        Ok(buffer.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path, limit: u64) -> io::Result<Option<T>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid build cache metadata",
        ));
    }
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(&bytes)?;
    file.sync_all()
}

fn directory_size(path: &Path) -> io::Result<u64> {
    let mut total = 0u64;
    for item in fs::read_dir(path)? {
        let item = item?;
        let metadata = fs::symlink_metadata(item.path())?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            total = total.saturating_add(directory_size(&item.path())?);
        } else {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

pub(crate) fn private_cache_root(path: &Path) -> io::Result<PathBuf> {
    if path.as_os_str().is_empty() || !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "CI_BUILD_CACHE_DIR must be absolute",
        ));
    }
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    let home = fs::canonicalize(home)?;
    if path == Path::new("/tmp") || path.starts_with("/tmp/") {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "build cache may not use public /tmp",
        ));
    }
    fs::create_dir_all(path)?;
    let root = fs::canonicalize(path)?;
    if root == home || !root.starts_with(&home) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "build cache must be below HOME",
        ));
    }
    protect_directory(&root)?;
    Ok(root)
}

fn protect_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "build cache directory ownership is invalid",
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn protect_file(file: &File) -> io::Result<()> {
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

fn lock_is_stale(path: &Path) -> io::Result<bool> {
    let metadata = fs::metadata(path)?;
    Ok(SystemTime::now()
        .duration_since(metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH))
        .unwrap_or_default()
        > STALE_LOCK_AFTER)
}

pub(crate) fn configured_cache_size() -> io::Result<u64> {
    parse_cache_size(env::var(CACHE_SIZE_ENV).ok().as_deref())
}

pub(crate) fn cache_volume_name(identity: &str) -> String {
    let digest = blake3::hash(identity.as_bytes()).to_hex().to_string();
    format!("kurbis-ci-sccache-{}", &digest[..32])
}

fn parse_cache_size(value: Option<&str>) -> io::Result<u64> {
    let bytes = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.parse::<u64>())
        .transpose()
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{CACHE_SIZE_ENV} must be an integer byte count"),
            )
        })?
        .unwrap_or(DEFAULT_CACHE_BYTES);
    if !(MIN_CACHE_BYTES..=MAX_CACHE_BYTES).contains(&bytes) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{CACHE_SIZE_ENV} must be between {MIN_CACHE_BYTES} and {MAX_CACHE_BYTES} bytes"
            ),
        ));
    }
    Ok(bytes)
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn bounded_log(log: &str) -> String {
    let bytes = log.as_bytes();
    if bytes.len() <= MAX_BUILD_LOG_BYTES as usize {
        return log.to_owned();
    }
    let start = bytes.len() - MAX_BUILD_LOG_BYTES as usize;
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn key() -> BuildKey {
        BuildKey::new(
            "Pumpkin-MC/Pumpkin",
            "0123456789abcdef0123456789abcdef01234567",
            "a".repeat(64),
            "rust",
            "x86_64-linux",
        )
        .unwrap()
    }

    #[test]
    fn identity_changes_for_source_plugin_runtime_and_patch() {
        let key = key();
        let first = BuildCache::identity(
            &key,
            "runtime-a",
            "x86_64-unknown-linux-gnu",
            b"source",
            b"plugin",
        );
        assert_ne!(
            first.cache_key,
            BuildCache::identity(
                &key,
                "runtime-a",
                "x86_64-unknown-linux-gnu",
                b"source-2",
                b"plugin"
            )
            .cache_key
        );
        assert_ne!(
            first.cache_key,
            BuildCache::identity(
                &key,
                "runtime-b",
                "x86_64-unknown-linux-gnu",
                b"source",
                b"plugin"
            )
            .cache_key
        );
        let mut changed = key.clone();
        changed.patch_fingerprint = "b".repeat(64);
        assert_ne!(
            first.cache_key,
            BuildCache::identity(
                &changed,
                "runtime-a",
                "x86_64-unknown-linux-gnu",
                b"source",
                b"plugin"
            )
            .cache_key
        );
    }

    #[tokio::test]
    async fn per_key_lock_serializes_concurrent_writers() {
        let home = tempdir().unwrap();
        let root = home.path().join("cache");
        fs::create_dir_all(root.join("entries")).unwrap();
        fs::create_dir_all(root.join("locks")).unwrap();
        let cache = BuildCache {
            root,
            max_bytes: DEFAULT_CACHE_BYTES,
        };
        let identity = BuildCache::identity(
            &key(),
            "runtime",
            "x86_64-unknown-linux-gnu",
            b"source",
            b"plugin",
        );
        let first = cache.acquire(&identity).await.unwrap();
        let second_cache = cache.clone();
        let second_identity = identity.clone();
        let waiting = tokio::spawn(async move { second_cache.acquire(&second_identity).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), waiting)
                .await
                .is_err()
        );
        drop(first);
        let second = tokio::time::timeout(Duration::from_secs(1), cache.acquire(&identity))
            .await
            .unwrap()
            .unwrap();
        drop(second);
    }

    #[test]
    fn prune_serializes_accounting_and_evicts_old_entries_at_the_bound() {
        let home = tempdir().unwrap();
        let root = home.path().join("cache");
        fs::create_dir_all(root.join("entries")).unwrap();
        fs::create_dir_all(root.join("locks")).unwrap();
        let cache = BuildCache {
            root: root.clone(),
            max_bytes: 8,
        };
        for (name, bytes) in [("old", b"123456".as_slice()), ("new", b"abcdef".as_slice())] {
            let entry = root.join("entries").join(name);
            fs::create_dir(&entry).unwrap();
            fs::write(entry.join("payload"), bytes).unwrap();
        }
        cache.prune_locked(SystemTime::now()).unwrap();
        let remaining = fs::read_dir(root.join("entries")).unwrap().count();
        assert_eq!(
            remaining, 1,
            "prune must account for all entries under its lock"
        );
    }

    #[tokio::test]
    async fn stale_temporary_entries_are_removed_without_deleting_active_entries() {
        let home = tempdir().unwrap();
        let root = home.path().join("cache");
        fs::create_dir_all(root.join("entries")).unwrap();
        fs::create_dir_all(root.join("locks")).unwrap();
        let cache = BuildCache {
            root: root.clone(),
            max_bytes: DEFAULT_CACHE_BYTES,
        };
        let identity = BuildCache::identity(
            &key(),
            "runtime",
            "x86_64-unknown-linux-gnu",
            b"source",
            b"plugin",
        );
        let stale = root
            .join("entries")
            .join(format!(".tmp-{}-old", "d".repeat(64)));
        fs::create_dir(&stale).unwrap();
        fs::write(stale.join("partial"), b"partial").unwrap();
        let active = root
            .join("entries")
            .join(format!(".tmp-{}-active", identity.cache_key));
        fs::create_dir(&active).unwrap();
        fs::write(active.join("partial"), b"partial").unwrap();
        let _lease = cache.acquire(&identity).await.unwrap();
        cache
            .prune_locked(SystemTime::now() + STALE_TEMP_AFTER + Duration::from_secs(1))
            .unwrap();
        assert!(!stale.exists());
        assert!(active.exists());
    }

    #[tokio::test]
    async fn cache_hit_restores_outputs_and_corruption_is_a_miss() {
        let home = tempdir().unwrap();
        let root = home.path().join("cache");
        fs::create_dir_all(root.join("entries")).unwrap();
        fs::create_dir_all(root.join("locks")).unwrap();
        let cache = BuildCache {
            root,
            max_bytes: DEFAULT_CACHE_BYTES,
        };
        let identity = BuildCache::identity(
            &key(),
            "runtime",
            "x86_64-unknown-linux-gnu",
            b"source",
            b"plugin",
        );
        let output = home.path().join("output");
        fs::create_dir(&output).unwrap();
        fs::write(output.join("pumpkin"), b"pumpkin").unwrap();
        fs::write(output.join("kurbis-control.wasm"), b"wasm").unwrap();
        cache.store(&identity, &output, "timing").unwrap();
        let restored = home.path().join("restored");
        assert_eq!(
            cache.restore(&identity, &restored).unwrap(),
            CacheLookup::Hit
        );
        assert_eq!(fs::read(restored.join("pumpkin")).unwrap(), b"pumpkin");
        fs::write(cache.entry_path(&identity).join("pumpkin"), b"tampered").unwrap();
        assert_eq!(
            cache.restore(&identity, &home.path().join("bad")).unwrap(),
            CacheLookup::Corrupt
        );
    }
}

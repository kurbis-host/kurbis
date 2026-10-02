use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::{RuntimeConfig, WorkerConfig};
use crate::worker::control_plane::WorkerControl;

const PLUGIN_NAME: &str = "kurbis-control";

#[derive(Clone)]
pub struct ServerAccess {
    control_plane_url: String,
    public_base_url: String,
    allow_insecure_local_http: bool,
    plugin_wasm: Option<PathBuf>,
    enabled: bool,
    control_plane: WorkerControl,
    root: PathBuf,
}

impl ServerAccess {
    pub fn new(config: &WorkerConfig, control_plane: WorkerControl) -> Result<Self> {
        crate::worker::control_plane::validate_control_plane_url(
            &config.control_plane_url,
            config.allow_insecure_local_control_plane_http,
        )?;
        Ok(Self {
            control_plane_url: config.control_plane_url.trim_end_matches('/').to_owned(),
            public_base_url: config.public_base_url.trim_end_matches('/').to_owned(),
            allow_insecure_local_http: config.allow_insecure_local_control_plane_http,
            plugin_wasm: config.plugin_wasm.clone(),
            enabled: config.plugin_wasm.is_some()
                || matches!(&config.runtime, RuntimeConfig::Docker { .. }),
            control_plane,
            root: config.root.clone(),
        })
    }

    pub async fn prepare_plugin(
        &self,
        server_id: Uuid,
        generation: i64,
        server_dir: &Path,
    ) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let capability = self.ensure_capability(server_id, generation).await?;
        if let Some(plugin_wasm) = &self.plugin_wasm {
            copy_plugin_binary(server_dir, plugin_wasm)?;
        }
        write_plugin_config(
            server_dir,
            &PluginConfig {
                api_base_url: self.public_base_url.clone(),
                allow_insecure_local_http: self.allow_insecure_local_http,
                server_id,
                capability,
            },
        )
    }

    pub fn remove_capability(&self, server_id: Uuid) -> Result<()> {
        match fs::remove_file(self.capability_path(server_id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("remove server capability"),
        }
    }

    async fn ensure_capability(&self, server_id: Uuid, generation: i64) -> Result<String> {
        let current = read_capability(&self.capability_path(server_id)).ok();
        let endpoint = reqwest::Url::parse(&format!(
            "{}/internal/worker/server-capabilities",
            self.control_plane_url
        ))?;
        let response = self
            .control_plane
            .authorized_request(reqwest::Method::POST, endpoint)?
            .json(&serde_json::json!({
                "server_id": server_id,
                "generation": generation,
                "current_capability": current,
            }))
            .send()
            .await
            .context("request current server capability")?;
        if !response.status().is_success() {
            bail!(
                "control plane rejected the current server placement ({})",
                response.status()
            );
        }
        let response: CapabilityResponse = response
            .json()
            .await
            .context("decode server capability response")?;
        if !valid_token(&response.capability) {
            bail!("control plane returned an invalid server capability");
        }
        write_capability(&self.capability_path(server_id), &response.capability)?;
        Ok(response.capability)
    }

    fn capability_path(&self, server_id: Uuid) -> PathBuf {
        self.root
            .join("capabilities")
            .join(format!("{server_id}.token"))
    }
}

#[derive(Debug, Deserialize)]
struct CapabilityResponse {
    capability: String,
}

#[derive(Debug, Serialize)]
struct PluginConfig {
    api_base_url: String,
    allow_insecure_local_http: bool,
    server_id: Uuid,
    capability: String,
}

fn valid_token(token: &str) -> bool {
    token.len() == 43
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn read_capability(path: &Path) -> std::io::Result<String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > 128 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid capability file",
        ));
    }
    let mut value = String::new();
    file.read_to_string(&mut value)?;
    let value = value.trim_end_matches('\n').to_owned();
    if !valid_token(&value) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid capability value",
        ));
    }
    Ok(value)
}

fn write_capability(path: &Path, capability: &str) -> Result<()> {
    let parent = path.parent().context("capability path has no parent")?;
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .context("capability path has no filename")?
            .to_string_lossy(),
        Uuid::new_v4()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&temporary)?;
    file.write_all(capability.as_bytes())?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn write_plugin_config(server_dir: &Path, config: &PluginConfig) -> Result<()> {
    let root = open_dir(server_dir)?;
    let plugins = ensure_dir_at(&root, "plugins")?;
    let data = ensure_dir_at(&plugins, "data")?;
    let plugin_data = ensure_dir_at(&data, PLUGIN_NAME)?;
    atomic_replace_at(
        &plugin_data,
        "config.json",
        &serde_json::to_vec(config)?,
        0o600,
    )
}

fn copy_plugin_binary(server_dir: &Path, source: &Path) -> Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(source)
        .with_context(|| format!("open built-in plugin at {}", source.display()))?;
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= 64 * 1024 * 1024,
        "built-in plugin is not a bounded regular file"
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((64 * 1024 * 1024 + 1) as u64)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 == metadata.len(),
        "built-in plugin changed while it was copied"
    );

    let root = open_dir(server_dir)?;
    let plugins = ensure_dir_at(&root, "plugins")?;
    atomic_replace_at(&plugins, "kurbis-control.wasm", &bytes, 0o644)
        .context("install managed Kurbis plugin")
}

fn atomic_replace_at(directory: &File, name: &str, bytes: &[u8], mode: u32) -> Result<()> {
    let directory_metadata = directory.metadata()?;
    let temporary = format!(".{name}.{}.tmp", Uuid::new_v4());
    let temporary_c = std::ffi::CString::new(temporary.as_str())?;
    let name_c = std::ffi::CString::new(name)?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            temporary_c.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .context("create managed plugin temporary file");
    }
    let write_result = (|| -> Result<()> {
        let mut file = unsafe { File::from_raw_fd(fd) };
        if let Some((uid, gid)) = managed_file_owner(
            unsafe { libc::geteuid() },
            directory_metadata.uid(),
            directory_metadata.gid(),
        ) && unsafe { libc::fchown(file.as_raw_fd(), uid, gid) } < 0
        {
            return Err(std::io::Error::last_os_error())
                .context("set managed plugin file ownership");
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        if unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                temporary_c.as_ptr(),
                directory.as_raw_fd(),
                name_c.as_ptr(),
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error()).context("publish managed plugin file");
        }
        Ok(())
    })();
    if write_result.is_err() {
        unsafe {
            libc::unlinkat(directory.as_raw_fd(), temporary_c.as_ptr(), 0);
        }
    }
    write_result
}

fn managed_file_owner(
    worker_uid: u32,
    directory_uid: u32,
    directory_gid: u32,
) -> Option<(u32, u32)> {
    (worker_uid == 0).then_some((directory_uid, directory_gid))
}

fn open_dir(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

fn open_dir_at(parent: &File, name: &str) -> std::io::Result<File> {
    let name = std::ffi::CString::new(name).expect("static path component");
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn ensure_dir_at(parent: &File, name: &str) -> Result<File> {
    let name_c = std::ffi::CString::new(name).expect("static path component");
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name_c.as_ptr(), 0o770) };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error).context("create plugin data directory");
        }
    }
    open_dir_at(parent, name).context("open plugin data directory")
}

#[cfg(test)]
mod tests {
    use super::managed_file_owner;

    #[test]
    fn root_worker_publishes_managed_files_with_the_parent_directory_owner() {
        assert_eq!(managed_file_owner(0, 2613, 2613), Some((2613, 2613)));
        assert_eq!(managed_file_owner(501, 2613, 2613), None);
    }
}

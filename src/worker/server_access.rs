use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::{RuntimeConfig, WorkerConfig};
use crate::worker::{
    control_plane::WorkerControl,
    managed_files::{atomic_replace_at, ensure_dir_at, open_dir},
    sandbox::{
        PUMPKIN_GID, PUMPKIN_UID, ensure_rootless_volume_owner, rootless_container_identity,
    },
};

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
    runtime_api_proxy: bool,
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
            runtime_api_proxy: matches!(&config.runtime, RuntimeConfig::Docker { .. }),
        })
    }

    pub async fn prepare_plugin(
        &self,
        server_id: Uuid,
        generation: i64,
        server_dir: &Path,
    ) -> Result<Option<String>> {
        if !self.enabled {
            return Ok(None);
        }
        let capability = self.ensure_capability(server_id, generation).await?;
        if let Some(plugin_wasm) = &self.plugin_wasm {
            copy_plugin_binary(server_dir, plugin_wasm)?;
        }
        write_plugin_config(
            server_dir,
            &PluginConfig {
                api_base_url: if self.runtime_api_proxy {
                    crate::runtime_api_proxy::SERVICE_BASE_URL.to_owned()
                } else {
                    self.public_base_url.clone()
                },
                allow_insecure_local_http: self.runtime_api_proxy || self.allow_insecure_local_http,
                server_id,
                capability: capability.clone(),
            },
        )?;
        // prepare_plugin writes the managed WASM and JSON as the worker account.
        // Rootless Docker maps Pumpkin's uid 2613 through the subordinate-id
        // range, so reconcile ownership after these writes and before the
        // runtime bind-mounts the tree. Otherwise hosted-entrypoint cannot
        // replace the plugin file and exits with a misleading permission error.
        if self.runtime_api_proxy {
            let (uid, gid) = rootless_container_identity(PUMPKIN_UID, PUMPKIN_GID).await?;
            ensure_rootless_volume_owner(server_dir, uid, gid).await?;
        }
        Ok(Some(capability))
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

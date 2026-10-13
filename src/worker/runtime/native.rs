use std::os::unix::{fs::PermissionsExt, process::CommandExt};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncSeekExt},
    process::{Child, Command},
    sync::Mutex,
    time::{sleep, timeout},
};
use uuid::Uuid;

use super::{
    ServerRuntime,
    settings::{AccountLookups, PermissionLevel, render_pumpkin_config},
};
use crate::{model::PlacementRecord, pumpkin_control::PumpkinControl};

#[derive(Serialize, Deserialize)]
struct ProcessRecord {
    pid: u32,
    generation: i64,
    marker: String,
}

/// Host processes survive worker restarts. A unique argv marker fences PID reuse.
pub struct NativeRuntime {
    binary: PathBuf,
    root: PathBuf,
    sockets: PathBuf,
    worker_control_key: String,
    public_address: IpAddr,
    signal_address: IpAddr,
    children: Mutex<HashMap<Uuid, Child>>,
}

impl NativeRuntime {
    pub async fn new(
        binary: PathBuf,
        root: PathBuf,
        worker_id: Uuid,
        token: String,
        public_address: IpAddr,
        signal_address: IpAddr,
    ) -> Result<Self> {
        let binary = fs::canonicalize(binary)
            .await
            .context("PUMPKIN_BINARY must name a built host-control runtime")?;
        if !fs::metadata(&binary).await?.is_file() {
            bail!("PUMPKIN_BINARY is not a file");
        }
        let sockets = PathBuf::from(format!("/tmp/kurbis-native-{}", worker_id.simple()));
        fs::create_dir_all(&sockets).await?;
        fs::set_permissions(&sockets, std::fs::Permissions::from_mode(0o700)).await?;
        Ok(Self {
            binary,
            root,
            sockets,
            worker_control_key: token,
            public_address,
            signal_address,
            children: Mutex::new(HashMap::new()),
        })
    }

    fn directory(&self, id: Uuid) -> PathBuf {
        self.root.join("servers").join(id.to_string())
    }
    fn record_path(&self, id: Uuid) -> PathBuf {
        self.directory(id).join("run/native-process.json")
    }

    async fn record(&self, id: Uuid) -> Result<Option<ProcessRecord>> {
        match fs::read(self.record_path(id)).await {
            Ok(bytes) => {
                let record: ProcessRecord =
                    serde_json::from_slice(&bytes).context("invalid native process identity")?;
                if record.pid == 0
                    || !record.marker.starts_with("kurbis-native-")
                    || record.marker.contains(char::is_whitespace)
                {
                    bail!("invalid native process marker or PID");
                }
                Ok(Some(record))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    async fn matches_process(record: &ProcessRecord) -> Result<bool> {
        let output = Command::new("ps")
            .args(["-p", &record.pid.to_string(), "-o", "command="])
            .output()
            .await?;
        if !output.status.success() {
            if output.status.code() == Some(1) {
                return Ok(false);
            }
            bail!(
                "native process inspection failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()
            == Some(record.marker.as_str()))
    }
}

#[async_trait]
impl ServerRuntime for NativeRuntime {
    async fn control_for(&self, server_dir: &Path) -> Result<PumpkinControl> {
        let server_id: Uuid = server_dir
            .file_name()
            .and_then(|name| name.to_str())
            .context("native server directory has no UUID")?
            .parse()?;
        let generation = fs::read_to_string(runtime_generation_path(&self.root, server_id))
            .await?
            .trim()
            .parse::<i64>()?;
        Ok(PumpkinControl::new(
            crate::worker::sandbox::scoped_runtime_token(
                &self.worker_control_key,
                server_id,
                generation,
            )?,
        ))
    }

    fn socket_path(&self, server_dir: &Path) -> Result<PathBuf> {
        let id: Uuid = server_dir
            .file_name()
            .and_then(|s| s.to_str())
            .context("native server directory has no UUID")?
            .parse()?;
        Ok(self.sockets.join(format!("{}.sock", id.simple())))
    }

    async fn start(
        &self,
        server_id: Uuid,
        placement: &PlacementRecord,
        server_dir: &Path,
        settings: &Value,
        _api_capability: Option<&str>,
    ) -> Result<()> {
        if let Some(generation) = self.server_generation(server_id).await? {
            if generation != placement.generation {
                bail!("native runtime already owns generation {generation}");
            }
            return self
                .wait_ready(server_id, server_dir, Duration::from_secs(120))
                .await;
        }
        fs::create_dir_all(server_dir.join("run")).await?;
        fs::write(
            server_dir.join("pumpkin.toml"),
            render_pumpkin_config(
                settings,
                AccountLookups::Direct,
                PermissionLevel::Player,
                placement.java_port.try_into()?,
                placement.bedrock_port.try_into()?,
                SocketAddr::new(self.signal_address, placement.signal_port.try_into()?),
                self.public_address,
                placement.bedrock_port.try_into()?,
            ),
        )
        .await?;
        persist_runtime_generation(&self.root, server_id, placement.generation).await?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(server_dir.join("run/native.log"))?;
        let marker = format!("kurbis-native-{}", Uuid::new_v4().simple());
        let mut command = Command::new(&self.binary);
        command.as_std_mut().arg0(&marker);
        let mut child = command
            .current_dir(server_dir)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .env("TZ", "UTC")
            .env("PUMPKIN_PROCESS_MARKER", &marker)
            .env("PUMPKIN_HOST_CONTROL_SOCKET", self.socket_path(server_dir)?)
            .env(
                "PUMPKIN_HOST_CONTROL_TOKEN",
                crate::worker::sandbox::scoped_runtime_token(
                    &self.worker_control_key,
                    server_id,
                    placement.generation,
                )?,
            )
            .kill_on_drop(false)
            .spawn()?;
        let record = ProcessRecord {
            pid: child.id().context("native process omitted PID")?,
            generation: placement.generation,
            marker,
        };
        let temporary = self.record_path(server_id).with_extension("json.tmp");
        let persist = async {
            fs::write(&temporary, serde_json::to_vec(&record)?).await?;
            fs::rename(&temporary, self.record_path(server_id)).await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if let Err(error) = persist {
            child.kill().await?;
            return Err(error).context("could not persist native runtime identity");
        }
        self.children.lock().await.insert(server_id, child);
        self.wait_ready(server_id, server_dir, Duration::from_secs(120))
            .await
    }

    async fn server_generation(&self, server_id: Uuid) -> Result<Option<i64>> {
        let Some(record) = self.record(server_id).await? else {
            return Ok(None);
        };
        let mut children = self.children.lock().await;
        if let Some(child) = children.get_mut(&server_id) {
            if child.try_wait()?.is_none() {
                return Ok(Some(record.generation));
            }
            children.remove(&server_id);
            return Ok(None);
        }
        drop(children);
        Ok(Self::matches_process(&record)
            .await?
            .then_some(record.generation))
    }

    async fn logs_since(&self, server_id: Uuid, _since: &str) -> Result<Vec<String>> {
        let run = self.directory(server_id).join("run");
        let mut file = match fs::File::open(run.join("native.log")).await {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let cursor = run.join("native-log-offset");
        let length = file.metadata().await?.len();
        let offset = match fs::read_to_string(&cursor).await {
            Ok(value) => value
                .trim()
                .parse::<u64>()
                .context("invalid native log cursor")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        };
        let offset = if offset <= length { offset } else { 0 };
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        let mut bytes = Vec::new();
        file.take(256 * 1024).read_to_end(&mut bytes).await?;
        let complete = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |end| end + 1);
        // A single oversized line must not pin the cursor forever. Preserve
        // short incomplete lines, but consume a full bounded read as a fragment.
        let consumed = if complete == 0 && bytes.len() == 256 * 1024 {
            bytes.len()
        } else {
            complete
        };
        let temporary = cursor.with_extension("tmp");
        fs::write(&temporary, (offset + consumed as u64).to_string()).await?;
        fs::rename(temporary, &cursor).await?;
        Ok(String::from_utf8_lossy(&bytes[..consumed])
            .lines()
            .map(str::to_owned)
            .collect())
    }

    async fn remove_runtime(&self, server_id: Uuid) -> Result<()> {
        if let Some(mut child) = self.children.lock().await.remove(&server_id) {
            if child.try_wait()?.is_none() {
                child.kill().await?;
            }
        } else if let Some(record) = self.record(server_id).await?
            && Self::matches_process(&record).await?
        {
            // Never signal a PID whose current argv belongs to another process.
            let output = Command::new("kill")
                .args(["-KILL", &record.pid.to_string()])
                .output()
                .await?;
            if !output.status.success() && Self::matches_process(&record).await? {
                bail!("native runtime termination failed");
            }
        }
        timeout(Duration::from_secs(10), async {
            while self.server_generation(server_id).await?.is_some() {
                sleep(Duration::from_millis(100)).await;
            }
            Ok::<(), anyhow::Error>(())
        })
        .await
        .context("native runtime removal not confirmed")??;
        for capability_file in [
            runtime_generation_path(&self.root, server_id),
            self.root
                .join("runtime-generations")
                .join(format!("{server_id}.tmp")),
        ] {
            match fs::remove_file(capability_file).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        match fs::remove_file(self.record_path(server_id)).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }
}

fn runtime_generation_path(root: &Path, server_id: Uuid) -> PathBuf {
    root.join("runtime-generations").join(server_id.to_string())
}

async fn persist_runtime_generation(root: &Path, server_id: Uuid, generation: i64) -> Result<()> {
    if generation <= 0 {
        bail!("runtime generation must be positive");
    }
    let directory = root.join("runtime-generations");
    fs::create_dir_all(&directory).await?;
    let path = runtime_generation_path(root, server_id);
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, generation.to_string()).await?;
    fs::rename(temporary, path).await?;
    Ok(())
}

#[cfg(test)]
mod tests;

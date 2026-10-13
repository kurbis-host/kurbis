use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use tokio::{process::Command, time::timeout};
use uuid::Uuid;

#[cfg(test)]
use crate::model::SERVER_STORAGE_BYTES;

const DOCKER_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const PROBE_SCRIPT: &str = r#"
set -eu
[ "$(id -u)" -eq 0 ]
awk -v expected="$EXPECTED_HOST_UID" '$1 == 0 && (($2 == expected && expected > 0) || (expected == 0 && $2 > 0)) && $3 == 1 { root = 1 } $1 == 1 && $2 > 0 && $3 >= 65536 { range = 1 } END { exit !(root && range) }' /proc/self/uid_map
awk -v expected="$EXPECTED_HOST_GID" '$1 == 0 && (($2 == expected && expected > 0) || (expected == 0 && $2 > 0)) && $3 == 1 { root = 1 } $1 == 1 && $2 > 0 && $3 >= 65536 { range = 1 } END { exit !(root && range) }' /proc/self/gid_map
read -r memory_limit < /sys/fs/cgroup/memory.max
[ "$memory_limit" -gt 0 ] && [ "$memory_limit" -le "$EXPECTED_MEMORY_BYTES" ]
read -r swap_limit < /sys/fs/cgroup/memory.swap.max
[ "$swap_limit" -eq 0 ]
read -r cpu_quota cpu_period < /sys/fs/cgroup/cpu.max
[ "$cpu_quota" -gt 0 ] && [ "$cpu_period" -gt 0 ]
[ $((cpu_quota * 1000)) -le $((cpu_period * EXPECTED_CPU_MILLIS)) ]
read -r pid_limit < /sys/fs/cgroup/pids.max
[ "$pid_limit" -gt 0 ] && [ "$pid_limit" -le "$EXPECTED_PID_LIMIT" ]
grep -Eq '^NoNewPrivs:[[:space:]]+1$' /proc/self/status
grep -Eq '^CapEff:[[:space:]]+0+$' /proc/self/status
awk '$5 == "/" { if ("," $6 "," ~ /,ro,/) found = 1 } END { exit !found }' /proc/self/mountinfo

awk 'NR > 1 && $1 != "lo" { route = 1 } END { exit route }' /proc/net/route
awk '$10 != "lo" { route = 1 } END { exit route }' /proc/net/ipv6_route
"#;

const STORAGE_PROBE_SCRIPT: &str = r#"
set -eu
[ -w /work ]
if dd if=/dev/zero of=/work/kurbis-quota-probe bs=1M count=96 conv=fsync status=none 2>/dev/null; then
    exit 90
fi
written=$(stat -c %s /work/kurbis-quota-probe)
[ "$written" -le "$EXPECTED_DISK_BYTES" ]
[ "$written" -ge 62914560 ]
"#;

const MEMORY_PRESSURE_PROBE_SCRIPT: &str = r#"
set -eu
[ "$(cat /sys/fs/cgroup/memory.max)" = 33554432 ]
[ "$(cat /sys/fs/cgroup/memory.swap.max)" = 0 ]
before=$(awk '$1 == "oom_kill" { print $2 }' /sys/fs/cgroup/memory.events)
if dd if=/dev/zero of=/dev/shm/kurbis-memory-pressure bs=1M count=64 conv=fsync status=none 2>/dev/null; then
    echo "memory pressure escaped the 32 MiB cgroup limit" >&2
    exit 90
fi
after=$(awk '$1 == "oom_kill" { print $2 }' /sys/fs/cgroup/memory.events)
[ -n "$before" ] && [ -n "$after" ] && [ "$after" -gt "$before" ]
echo "MEMORY_OOM_KILL_PROBE_PASSED before=$before after=$after"
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkloadLimits {
    pub memory_bytes: u64,
    pub cpu_millis: u64,
    pub pid_limit: u32,
    pub disk_bytes: u64,
}

impl WorkloadLimits {
    pub const fn preview_builder() -> Self {
        Self {
            memory_bytes: 8 * 1024 * 1024 * 1024,
            cpu_millis: 2000,
            pid_limit: 512,
            disk_bytes: 5 * 1024 * 1024 * 1024,
        }
    }

    pub const fn server(memory_bytes: u64, disk_bytes: u64) -> Self {
        Self {
            memory_bytes,
            cpu_millis: 1000,
            pid_limit: 512,
            disk_bytes,
        }
    }

    pub fn validate(self) -> Result<Self> {
        if self.memory_bytes == 0
            || self.cpu_millis == 0
            || self.pid_limit == 0
            || self.disk_bytes == 0
        {
            bail!("all rootless workload limits must be non-zero");
        }
        Ok(self)
    }
}

#[derive(Clone, Copy)]
pub struct DockerSandbox;

fn cpu_quota_for(cpu_millis: u64) -> Result<String> {
    if cpu_millis == 0 {
        bail!("CPU quota must be non-zero");
    }
    let quota = cpu_millis
        .checked_mul(100)
        .context("CPU quota exceeds the Docker cgroup range")?;
    i64::try_from(quota).context("CPU quota exceeds the Docker cgroup range")?;
    Ok(quota.to_string())
}

impl DockerSandbox {
    pub async fn verify_with_limits(&self, image: &str, limits: WorkloadLimits) -> Result<()> {
        let limits = limits.validate()?;
        ensure_linux_worker()?;
        let (daemon_uid, daemon_gid) = rootless_daemon_identity().await?;
        let security_options =
            docker_output(["info", "--format", "{{json .SecurityOptions}}"]).await?;
        let options: Vec<String> = serde_json::from_str(security_options.trim())
            .context("Docker returned invalid security options")?;
        if !options.iter().any(|option| option == "name=rootless") {
            bail!("Docker daemon is not running in verified rootless mode");
        }

        let cgroup_version = docker_output(["info", "--format", "{{.CgroupVersion}}"]).await?;
        if cgroup_version.trim() != "2" {
            bail!("rootless Docker must use cgroup v2 for hard workload quotas");
        }
        verify_host_cgroup_controllers()?;
        self.verify_enforcement(image, limits, daemon_uid, daemon_gid, None)
            .await
    }

    /// Verify a dedicated rootless Docker builder reached over SSH. Unlike a
    /// worker daemon it cannot provide a local bind-mount socket identity, so
    /// the live container verifies a non-root user-namespace mapping itself.
    pub async fn verify_builder_with_limits_at(
        &self,
        image: &str,
        limits: WorkloadLimits,
        docker_host: &str,
        expected_architecture: &str,
    ) -> Result<()> {
        let limits = limits.validate()?;
        ensure_linux_worker()?;
        validate_docker_host(docker_host)?;
        let security_options = docker_output_at(
            Some(docker_host),
            ["info", "--format", "{{json .SecurityOptions}}"],
        )
        .await?;
        let options: Vec<String> = serde_json::from_str(security_options.trim())
            .context("builder Docker returned invalid security options")?;
        if !options.iter().any(|option| option == "name=rootless") {
            bail!("CI builder Docker daemon is not running in verified rootless mode");
        }
        let cgroup_version = docker_output_at(
            Some(docker_host),
            ["info", "--format", "{{.CgroupVersion}}"],
        )
        .await?;
        if cgroup_version.trim() != "2" {
            bail!("CI builder Docker daemon must use cgroup v2");
        }
        let architecture = docker_output_at(
            Some(docker_host),
            ["info", "--format", "{{.Architecture}}/{{.OSType}}"],
        )
        .await?;
        let actual = architecture.trim();
        let expected = format!("{expected_architecture}/linux");
        if actual != expected {
            bail!("CI builder architecture mismatch: expected {expected}, got {actual}");
        }
        self.verify_enforcement(image, limits, 0, 0, Some(docker_host))
            .await?;
        verify_buildkit_resource_enforcement(image, Some(docker_host)).await
    }

    async fn verify_enforcement(
        &self,
        image: &str,
        limits: WorkloadLimits,
        daemon_uid: u32,
        daemon_gid: u32,
        docker_host: Option<&str>,
    ) -> Result<()> {
        if image.trim().is_empty() {
            bail!("a pinned runtime image is required for sandbox verification");
        }
        let memory = limits.memory_bytes.to_string();
        let cpu_quota = cpu_quota_for(limits.cpu_millis)?;
        let pids = limits.pid_limit.to_string();
        let disk = limits.disk_bytes.to_string();
        let output = timeout(
            DOCKER_COMMAND_TIMEOUT,
            docker_command(docker_host)
                .args([
                    "run",
                    "--rm",
                    "--pull=never",
                    "--network=none",
                    "--memory",
                    &memory,
                    "--memory-swap",
                    &memory,
                    "--read-only",
                    "--tmpfs",
                    &format!(
                        "/work:rw,noexec,nosuid,nodev,size={},nr_inodes=1310720,mode=0700",
                        limits.disk_bytes
                    ),
                    "--cpu-period",
                    "100000",
                    "--cpu-quota",
                    &cpu_quota,
                    "--pids-limit",
                    &pids,
                    "--user",
                    "0:0",
                    "--security-opt",
                    "no-new-privileges:true",
                    "--cap-drop",
                    "ALL",
                    "--env",
                    &format!("EXPECTED_HOST_UID={daemon_uid}"),
                    "--env",
                    &format!("EXPECTED_HOST_GID={daemon_gid}"),
                    "--env",
                    &format!("EXPECTED_MEMORY_BYTES={memory}"),
                    "--env",
                    &format!("EXPECTED_CPU_MILLIS={}", limits.cpu_millis),
                    "--env",
                    &format!("EXPECTED_PID_LIMIT={pids}"),
                    "--env",
                    &format!("EXPECTED_DISK_BYTES={disk}"),
                    "--tmpfs",
                    "/tmp:rw,noexec,nosuid,nodev,size=4m",
                    "--entrypoint",
                    "/bin/sh",
                    image,
                    "-ec",
                    PROBE_SCRIPT,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("rootless Docker sandbox verification timed out")??;
        if !output.status.success() {
            bail!(
                "rootless Docker failed the live cgroup/isolation probe ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        self.verify_tmpfs_quota(image, docker_host).await?;
        self.verify_memory_limit_enforcement(image, docker_host)
            .await
    }

    async fn verify_memory_limit_enforcement(
        &self,
        image: &str,
        docker_host: Option<&str>,
    ) -> Result<()> {
        const MEMORY: &str = "33554432";
        let name = format!("kurbis-memory-probe-{}", Uuid::new_v4().simple());
        let output = timeout(
            DOCKER_COMMAND_TIMEOUT,
            docker_command(docker_host)
                .args([
                    "run",
                    "--name",
                    &name,
                    "--pull=never",
                    "--network=none",
                    "--memory",
                    MEMORY,
                    "--memory-swap",
                    MEMORY,
                    "--cpu-period",
                    "100000",
                    "--cpu-quota",
                    "10000",
                    "--pids-limit",
                    "16",
                    "--user",
                    "0:0",
                    "--read-only",
                    "--shm-size",
                    "64m",
                    "--tmpfs",
                    "/tmp:rw,noexec,nosuid,nodev,size=4m",
                    "--security-opt",
                    "no-new-privileges:true",
                    "--cap-drop",
                    "ALL",
                    "--entrypoint",
                    "/bin/sh",
                    image,
                    "-ec",
                    MEMORY_PRESSURE_PROBE_SCRIPT,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .output(),
        )
        .await;
        let output = match output {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                let _ = remove_probe_container(docker_host, &name).await;
                return Err(error).context("run cgroup memory pressure probe");
            }
            Err(_) => {
                let _ = remove_probe_container(docker_host, &name).await;
                bail!("cgroup memory pressure probe timed out");
            }
        };
        let oom_killed = docker_output_at(
            docker_host,
            ["inspect", "--format", "{{.State.OOMKilled}}", &name],
        )
        .await
        .unwrap_or_default();
        let _ = remove_probe_container(docker_host, &name).await;
        let output_text = String::from_utf8_lossy(&output.stdout);
        if output.status.success() && output_text.contains("MEMORY_OOM_KILL_PROBE_PASSED") {
            return Ok(());
        }
        if oom_killed.trim() == "true" {
            return Ok(());
        }
        bail!(
            "rootless Docker did not enforce the 32 MiB/no-swap memory limit ({}; OOMKilled={}): {}{}",
            output.status,
            oom_killed.trim(),
            String::from_utf8_lossy(&output.stderr).trim(),
            output_text
        )
    }

    async fn verify_tmpfs_quota(&self, image: &str, docker_host: Option<&str>) -> Result<()> {
        let disk = (64 * 1024 * 1024_u64).to_string();
        let output = timeout(
            DOCKER_COMMAND_TIMEOUT,
            docker_command(docker_host)
                .args([
                    "run",
                    "--rm",
                    "--pull=never",
                    "--network=none",
                    "--memory",
                    "268435456",
                    "--memory-swap",
                    "268435456",
                    "--cpu-period",
                    "100000",
                    "--cpu-quota",
                    "25000",
                    "--pids-limit",
                    "16",
                    "--user",
                    "0:0",
                    "--read-only",
                    "--tmpfs",
                    &format!("/work:rw,noexec,nosuid,nodev,size={disk},nr_inodes=65536,mode=0700"),
                    "--security-opt",
                    "no-new-privileges:true",
                    "--cap-drop",
                    "ALL",
                    "--tmpfs",
                    "/tmp:rw,noexec,nosuid,nodev,size=4m",
                    "--env",
                    &format!("EXPECTED_DISK_BYTES={disk}"),
                    "--entrypoint",
                    "/bin/sh",
                    image,
                    "-ec",
                    STORAGE_PROBE_SCRIPT,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("rootless Docker bounded workspace probe timed out")??;
        if !output.status.success() {
            bail!(
                "rootless Docker did not enforce the read-only-root/bounded-workspace disk limit ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    pub async fn create_isolated_network(
        &self,
        server_id: Uuid,
        generation: i64,
    ) -> Result<String> {
        let name = workload_network_name(server_id, generation);
        let server_label = format!("kurbis.server_id={server_id}");
        let generation_label = format!("kurbis.generation={generation}");
        let output = Command::new("docker")
            .args([
                "network",
                "create",
                "--driver",
                "bridge",
                "--internal",
                "--opt",
                "com.docker.network.bridge.enable_icc=true",
                "--opt",
                "com.docker.network.bridge.enable_ip_masquerade=false",
                "--label",
                &server_label,
                "--label",
                &generation_label,
                &name,
            ])
            .stdin(Stdio::null())
            .output()
            .await?;
        if !output.status.success() {
            bail!(
                "failed to create isolated rootless workload network {name}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        self.verify_isolated_network(&name, &[]).await?;
        Ok(name)
    }

    pub async fn verify_isolated_network(
        &self,
        name: &str,
        expected_containers: &[String],
    ) -> Result<()> {
        let output = docker_output(["network", "inspect", "--format", "{{json .}}", name]).await?;
        let network: Value = serde_json::from_str(output.trim())
            .context("Docker returned invalid workload network metadata")?;
        verify_isolated_network_metadata(&network, expected_containers)
    }

    pub async fn remove_isolated_network(&self, server_id: Uuid, generation: i64) -> Result<()> {
        let name = workload_network_name(server_id, generation);
        self.remove_network_name(&name).await
    }

    /// Remove every generation-labelled network left by a crashed or forcibly
    /// deleted runtime. Names are accepted only when they match our UUID and
    /// positive-generation grammar; Docker labels alone are not trusted.
    pub async fn remove_isolated_networks(&self, server_id: Uuid) -> Result<()> {
        let label = format!("kurbis.server_id={server_id}");
        let output = timeout(
            DOCKER_COMMAND_TIMEOUT,
            Command::new("docker")
                .args([
                    "network",
                    "ls",
                    "--filter",
                    &format!("label={label}"),
                    "--format",
                    "{{.Name}}",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("Docker network cleanup timed out")??;
        if !output.status.success() {
            bail!(
                "failed to enumerate workload networks for {server_id}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let prefix = format!("kurbis-{}-g", server_id.simple());
        for name in String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
        {
            if let Some(generation) = name
                .strip_prefix(&prefix)
                .and_then(|value| value.parse::<i64>().ok())
                && generation > 0
            {
                self.remove_network_name(name).await?;
            }
        }
        Ok(())
    }

    async fn remove_network_name(&self, name: &str) -> Result<()> {
        let output = timeout(
            DOCKER_COMMAND_TIMEOUT,
            Command::new("docker")
                .args(["network", "rm", name])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("Docker network removal timed out")??;
        if output.status.success() || is_docker_not_found(&output.stderr) {
            return Ok(());
        }
        bail!(
            "failed to remove isolated workload network {name}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }

    pub fn run_security_args(
        &self,
        limits: WorkloadLimits,
        user: &str,
        network: &str,
        read_only: bool,
    ) -> Result<Vec<String>> {
        let limits = limits.validate()?;
        let cpu_quota = cpu_quota_for(limits.cpu_millis)?;
        let mut args = vec![
            "--network".into(),
            network.into(),
            "--memory".into(),
            limits.memory_bytes.to_string(),
            "--memory-swap".into(),
            limits.memory_bytes.to_string(),
            "--cpu-period".into(),
            "100000".into(),
            "--cpu-quota".into(),
            cpu_quota,
            "--pids-limit".into(),
            limits.pid_limit.to_string(),
            "--user".into(),
            user.into(),
            // Docker defaults PID, IPC, and UTS to private namespaces. Avoid
            // passing the non-existent `--pid=private`/`--uts=private` modes;
            // only cgroupns exposes a portable explicit `private` switch.
            "--cgroupns=private".into(),
            "--security-opt".into(),
            "no-new-privileges:true".into(),
            "--cap-drop".into(),
            "ALL".into(),
            "--ulimit".into(),
            "core=0:0".into(),
            "--tmpfs".into(),
            "/tmp:rw,noexec,nosuid,nodev,size=64m".into(),
            "--shm-size".into(),
            "64m".into(),
            "--log-driver".into(),
            "local".into(),
            "--log-opt".into(),
            "max-size=10m".into(),
            "--log-opt".into(),
            "max-file=3".into(),
        ];
        if !read_only {
            bail!("sandboxed workloads require a read-only container root filesystem");
        }
        args.push("--read-only".into());
        Ok(args)
    }
}

fn verify_isolated_network_metadata(network: &Value, expected_containers: &[String]) -> Result<()> {
    let internal = network.get("Internal").and_then(Value::as_bool) == Some(true);
    let ipv6 = network.get("EnableIPv6").and_then(Value::as_bool) == Some(true);
    let options = network.get("Options").and_then(Value::as_object);
    let icc_enabled = options
        .and_then(|options| options.get("com.docker.network.bridge.enable_icc"))
        .and_then(Value::as_str)
        == Some("true");
    let masquerade_disabled = options
        .and_then(|options| options.get("com.docker.network.bridge.enable_ip_masquerade"))
        .and_then(Value::as_str)
        == Some("false");
    let connected = network
        .get("Containers")
        .and_then(Value::as_object)
        .map(|containers| {
            containers
                .values()
                .filter_map(|container| container.get("Name").and_then(Value::as_str))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut connected = connected;
    connected.sort();
    let mut expected = expected_containers.to_vec();
    expected.sort();
    let bridge = network.get("Driver").and_then(Value::as_str) == Some("bridge");
    if !bridge || !internal || ipv6 || !icc_enabled || !masquerade_disabled || connected != expected
    {
        bail!(
            "workload network policy is not enforced (bridge={bridge}, internal={internal}, ipv6={ipv6}, icc_enabled={icc_enabled}, masquerade_disabled={masquerade_disabled}, attached={connected:?})"
        );
    }
    Ok(())
}

pub fn scoped_runtime_token(worker_key: &str, server_id: Uuid, generation: i64) -> Result<String> {
    if worker_key.len() < 32 || generation <= 0 {
        bail!("runtime capability derivation requires a strong worker key and positive generation");
    }
    let key = blake3::derive_key(
        "kurbis worker host-control capability key v1",
        worker_key.as_bytes(),
    );
    let mut hasher = blake3::Hasher::new_keyed(&key);
    hasher.update(b"pumpkin-server-capability\0");
    hasher.update(server_id.as_bytes());
    hasher.update(&generation.to_be_bytes());
    Ok(hasher.finalize().to_hex().to_string())
}

pub fn scoped_ci_preview_capability(
    worker_key: &str,
    preview_id: Uuid,
    generation: i64,
) -> Result<String> {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    if worker_key.len() < 32 || generation <= 0 {
        bail!("CI capability derivation requires a strong worker key and positive generation");
    }
    let key = blake3::derive_key("kurbis CI preview capability key v1", worker_key.as_bytes());
    let mut hasher = blake3::Hasher::new_keyed(&key);
    hasher.update(b"ci-preview-self-capability\0");
    hasher.update(preview_id.as_bytes());
    hasher.update(&generation.to_be_bytes());
    Ok(URL_SAFE_NO_PAD.encode(hasher.finalize().as_bytes()))
}

pub fn workload_network_name(server_id: Uuid, generation: i64) -> String {
    format!("kurbis-{}-g{}", server_id.simple(), generation)
}

pub fn scoped_runtime_environment(
    worker_key: &str,
    server_id: Uuid,
    generation: i64,
) -> Result<[String; 2]> {
    Ok([
        "PUMPKIN_HOST_CONTROL_SOCKET=/pumpkin/run/host-control.sock".to_owned(),
        format!(
            "PUMPKIN_HOST_CONTROL_TOKEN={}",
            scoped_runtime_token(worker_key, server_id, generation)?
        ),
    ])
}

pub fn scoped_runtime_environment_args(
    worker_key: &str,
    server_id: Uuid,
    generation: i64,
) -> Result<[String; 4]> {
    let environment = scoped_runtime_environment(worker_key, server_id, generation)?;
    Ok([
        "--env".to_owned(),
        environment[0].clone(),
        "--env".to_owned(),
        environment[1].clone(),
    ])
}

pub async fn validate_quota_volume(path: &Path, expected_bytes: u64) -> Result<()> {
    if expected_bytes == 0 {
        bail!("workload disk quota must be non-zero");
    }
    ensure_linux_worker()?;
    let canonical_path =
        std::fs::canonicalize(path).context("cannot resolve the workload quota mount path")?;
    let output = timeout(
        DOCKER_COMMAND_TIMEOUT,
        Command::new("mountpoint")
            .arg("-q")
            .arg(&canonical_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("workload quota mount probe timed out")??;
    if !output.status.success() {
        bail!("workload storage is not an isolated hard-quota mount");
    }

    #[cfg(target_os = "linux")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")
            .context("cannot verify workload quota mount source")?;
        verify_dedicated_loop_mount(&mountinfo, &canonical_path)?;
        let path = CString::new(canonical_path.as_os_str().as_bytes())
            .context("workload storage path contains a NUL byte")?;
        let mut stats = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: statfs writes one initialized statfs value for a valid C path.
        let result = unsafe { libc::statfs(path.as_ptr(), stats.as_mut_ptr()) };
        if result != 0 {
            return Err(std::io::Error::last_os_error())
                .context("inspect workload filesystem quota");
        }
        // SAFETY: statfs returned success, so the output structure is initialized.
        let stats = unsafe { stats.assume_init() };
        const EXT4_SUPER_MAGIC: libc::c_long = 0xEF53;
        if stats.f_type != EXT4_SUPER_MAGIC {
            bail!("workload storage must be a dedicated ext4 quota filesystem");
        }
        let bytes = (stats.f_blocks as u64)
            .checked_mul(stats.f_bsize as u64)
            .context("workload filesystem size overflow")?;
        if bytes == 0 || bytes > expected_bytes {
            bail!("workload filesystem quota is not enforced: {bytes} > {expected_bytes}");
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (path, expected_bytes);
        bail!("hard workload disk quotas require a Linux worker")
    }
}

#[cfg(any(target_os = "linux", test))]
fn verify_dedicated_loop_mount(mountinfo: &str, target: &Path) -> Result<()> {
    let target = target
        .to_str()
        .context("workload quota mount path must be UTF-8")?;
    let mut selected = None;
    let mut selected_device = None;
    let mut mounts = Vec::new();
    for line in mountinfo.lines() {
        let Some((mount, filesystem)) = line.split_once(" - ") else {
            continue;
        };
        let fields = mount.split_ascii_whitespace().collect::<Vec<_>>();
        let filesystem = filesystem.split_ascii_whitespace().collect::<Vec<_>>();
        if fields.len() < 6 || filesystem.len() < 2 {
            continue;
        }
        let mountpoint = unescape_mount_field(fields[4]);
        let root = unescape_mount_field(fields[3]);
        let source = unescape_mount_field(filesystem[1]);
        let device = fields[2];
        if filesystem[0] == "ext4" && source.starts_with("/dev/loop") {
            mounts.push((device.to_owned(), mountpoint.clone(), root.clone()));
        }
        if mountpoint == target {
            selected = Some((filesystem[0], source, root));
            selected_device = Some(device.to_owned());
        }
    }
    let Some((filesystem, source, root)) = selected else {
        bail!("workload storage path is not a mounted filesystem");
    };
    let loop_suffix = source.strip_prefix("/dev/loop").unwrap_or_default();
    if filesystem != "ext4"
        || root != "/"
        || loop_suffix.is_empty()
        || !loop_suffix.bytes().all(|byte| byte.is_ascii_digit())
    {
        bail!("workload storage must be a dedicated ext4 loopback filesystem");
    }
    let selected_device = selected_device.context("workload loopback device is missing")?;
    if mounts.iter().any(|(device, mountpoint, root)| {
        device == &selected_device && (mountpoint != target || root != "/")
    }) {
        bail!("workload loopback filesystem is mounted at more than one path");
    }
    Ok(())
}

#[cfg(any(target_os = "linux", test))]
fn unescape_mount_field(value: &str) -> String {
    value
        .replace("\\134", "\\\\")
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
}

fn ensure_linux_worker() -> Result<()> {
    #[cfg(not(target_os = "linux"))]
    bail!("rootless workload sandbox requires a Linux worker host");
    #[cfg(target_os = "linux")]
    Ok(())
}

impl DockerSandbox {
    /// Root in Docker's user namespace maps to this unprivileged host identity.
    /// Using that mapped root lets the runtime own its isolated bind volume while
    /// `--cap-drop=ALL` and `no-new-privileges` deny host-level escalation.
    pub async fn rootless_daemon_identity(&self) -> Result<(u32, u32)> {
        rootless_daemon_identity().await
    }

    pub async fn rootless_container_identity(&self, uid: u32, gid: u32) -> Result<(u32, u32)> {
        rootless_container_identity(uid, gid).await
    }
}

/// The unprivileged account Pumpkin runs as inside every workload container.
///
/// Workload storage is handed to this account's host-side mapping before it
/// is bind-mounted, so the container user and the storage owner must come
/// from the same constants: with all capabilities dropped, any other
/// container user (including root) cannot write to that storage.
pub const PUMPKIN_UID: u32 = 2613;
pub const PUMPKIN_GID: u32 = 2613;
pub const PUMPKIN_USER: &str = "2613:2613";

pub async fn ensure_rootless_volume_owner(path: &Path, uid: u32, gid: u32) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .context("cannot inspect rootless workload storage")?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!("rootless workload storage must be a real directory");
    }
    // The top-level directory can already be owned by the rootless daemon while
    // restored snapshots contain root-owned descendants. Always reconcile the
    // complete tree before bind-mounting it into a workload; checking only the
    // root directory lets hosted-entrypoint fail later with a misleading
    // permission error when it installs managed plugins.
    let status = timeout(
        DOCKER_COMMAND_TIMEOUT,
        Command::new("chown")
            .args(["-R", "--no-dereference"])
            .arg(format!("{uid}:{gid}"))
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .status(),
    )
    .await
    .context("workload storage ownership change timed out")??;
    if !status.success() {
        bail!("failed to assign workload storage to the rootless Docker user");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub async fn rootless_docker_socket() -> Result<PathBuf> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let endpoint = docker_output([
        "context",
        "inspect",
        "--format",
        "{{(index .Endpoints \"docker\").Host}}",
    ])
    .await?;
    let endpoint = endpoint.trim();
    let socket = endpoint
        .strip_prefix("unix://")
        .context("production workloads require a local Unix Docker socket, not a remote daemon")?;
    let path = PathBuf::from(socket);
    let metadata = tokio::fs::metadata(&path)
        .await
        .context("cannot inspect the local rootless Docker socket")?;
    ensure!(
        metadata.file_type().is_socket() && metadata.uid() != 0,
        "Docker socket must be a non-root-owned rootless daemon socket"
    );
    Ok(path)
}

#[cfg(target_os = "linux")]
pub async fn rootless_container_identity(
    container_uid: u32,
    container_gid: u32,
) -> Result<(u32, u32)> {
    let endpoint = docker_output([
        "context",
        "inspect",
        "--format",
        "{{(index .Endpoints \"docker\").Host}}",
    ])
    .await?;
    let socket = endpoint
        .trim()
        .strip_prefix("unix://")
        .context("production workloads require a local Unix Docker socket")?;
    // Ask the kernel how the daemon's user namespace maps ids instead of
    // recomputing it from /etc/subuid: rootless Docker maps container id 0 to
    // the daemon's own account and ids from 1 onto the subordinate range, so
    // "range start + id" is off by one and hands the storage to a different
    // account than the one the workload runs as.
    let pid_file = Path::new(socket).with_file_name("docker.pid");
    let pid = tokio::fs::read_to_string(&pid_file)
        .await
        .with_context(|| {
            format!(
                "cannot read rootless Docker pid from {}",
                pid_file.display()
            )
        })?;
    let pid = pid.trim();
    let uid_map = tokio::fs::read_to_string(format!("/proc/{pid}/uid_map")).await?;
    let gid_map = tokio::fs::read_to_string(format!("/proc/{pid}/gid_map")).await?;
    let uid = host_id_for(&uid_map, container_uid)
        .context("rootless Docker user namespace does not map the workload uid")?;
    let gid = host_id_for(&gid_map, container_gid)
        .context("rootless Docker user namespace does not map the workload gid")?;
    Ok((uid, gid))
}

#[cfg(not(target_os = "linux"))]
pub async fn rootless_container_identity(
    _container_uid: u32,
    _container_gid: u32,
) -> Result<(u32, u32)> {
    bail!("rootless workload sandbox requires a Linux worker host")
}

/// The host id that a user namespace's `uid_map`/`gid_map` assigns to an id
/// inside it. Each line is `<first inside id> <first host id> <count>`.
#[cfg(any(target_os = "linux", test))]
fn host_id_for(id_map: &str, inside: u32) -> Option<u32> {
    id_map.lines().find_map(|line| {
        let mut fields = line.split_whitespace().map(str::parse::<u32>);
        let (first_inside, first_host, count) = (
            fields.next()?.ok()?,
            fields.next()?.ok()?,
            fields.next()?.ok()?,
        );
        let offset = inside.checked_sub(first_inside)?;
        (offset < count)
            .then(|| first_host.checked_add(offset))
            .flatten()
    })
}

#[cfg(not(target_os = "linux"))]
pub async fn rootless_docker_socket() -> Result<PathBuf> {
    bail!("rootless workload sandbox requires a Linux worker host")
}

#[cfg(target_os = "linux")]
async fn rootless_daemon_identity() -> Result<(u32, u32)> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    let endpoint = docker_output([
        "context",
        "inspect",
        "--format",
        "{{(index .Endpoints \"docker\").Host}}",
    ])
    .await?;
    let endpoint = endpoint.trim();
    let socket = endpoint
        .strip_prefix("unix://")
        .context("production workloads require a local Unix Docker socket, not a remote daemon")?;
    let metadata = tokio::fs::metadata(socket)
        .await
        .context("cannot inspect the local rootless Docker socket")?;
    let worker_uid = unsafe { libc::geteuid() };
    if !metadata.file_type().is_socket()
        || metadata.uid() == 0
        || (worker_uid != 0 && metadata.uid() != worker_uid)
        || metadata.mode() & 0o002 != 0
    {
        bail!(
            "Docker socket must be a non-root-owned, non-writable rootless daemon socket accessible to the worker"
        );
    }
    // RootlessKit maps container uid/gid 0 to the daemon user's real uid and
    // primary gid. The socket's group may instead be a subordinate host gid,
    // so do not mistake the socket inode's group for the namespace root map.
    let mut passwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut buffer = vec![0u8; 16 * 1024];
    let mut result = std::ptr::null_mut();
    // SAFETY: getpwuid_r initializes passwd/result on success and the provided
    // scratch buffer remains alive until the returned gid is copied below.
    let status = unsafe {
        libc::getpwuid_r(
            metadata.uid(),
            passwd.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() {
        bail!("cannot resolve the rootless Docker socket owner's primary group");
    }
    // SAFETY: getpwuid_r returned a non-null pointer into the initialized passwd.
    let daemon_gid = unsafe { passwd.assume_init().pw_gid };
    Ok((metadata.uid(), daemon_gid))
}

#[cfg(not(target_os = "linux"))]
async fn rootless_daemon_identity() -> Result<(u32, u32)> {
    bail!("rootless workload sandbox requires a Linux worker host")
}

#[cfg(target_os = "linux")]
fn verify_host_cgroup_controllers() -> Result<()> {
    let controllers = std::fs::read_to_string("/sys/fs/cgroup/cgroup.controllers")
        .context("cannot read the host cgroup v2 controller list")?;
    for required in ["cpu", "memory", "pids"] {
        if !controllers
            .split_ascii_whitespace()
            .any(|name| name == required)
        {
            bail!("host cgroup v2 controller {required} is unavailable");
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn verify_host_cgroup_controllers() -> Result<()> {
    bail!("rootless workload sandbox requires Linux cgroup v2")
}

fn validate_docker_host(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value).context("invalid CI builder Docker host")?;
    if url.scheme() != "ssh"
        || url.host_str().is_none()
        || url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("CI builder Docker host must be ssh://user@host[:port]");
    }
    Ok(())
}

async fn verify_buildkit_resource_enforcement(
    image: &str,
    docker_host: Option<&str>,
) -> Result<()> {
    let directory = tempfile::Builder::new()
        .prefix("kurbis-buildkit-cgroup-probe-")
        .tempdir()
        .context("create BuildKit resource probe context")?;
    let name = format!("kurbis-buildkit-probe-{}", Uuid::new_v4().simple());
    let dockerfile = r#"ARG PROBE_IMAGE
FROM ${PROBE_IMAGE}
ARG EXPECTED_MEMORY_BYTES
ARG EXPECTED_CPU_MILLIS
RUN set -eu; \
    memory=$(cat /sys/fs/cgroup/memory.max); test "$memory" -gt 0; test "$memory" -le "$EXPECTED_MEMORY_BYTES"; \
    test "$(cat /sys/fs/cgroup/memory.swap.max)" -eq 0; \
    read -r quota period < /sys/fs/cgroup/cpu.max; test "$quota" -gt 0; test "$period" -gt 0; \
    test $((quota * 1000)) -le $((period * EXPECTED_CPU_MILLIS)); \
    before=$(awk '$1 == "oom_kill" { print $2 }' /sys/fs/cgroup/memory.events); \
    if dd if=/dev/zero of=/dev/shm/kurbis-buildkit-memory-probe bs=1M count=128 conv=fsync status=none 2>/dev/null; then \
        echo "BuildKit memory cap did not stop cgroup pressure" >&2; exit 90; \
    fi; \
    after=$(awk '$1 == "oom_kill" { print $2 }' /sys/fs/cgroup/memory.events); \
    test -n "$before"; test -n "$after"; test "$after" -gt "$before"; \
    echo "BUILDKIT_CGROUP_LIMITS_PASSED memory=$memory swap=$(cat /sys/fs/cgroup/memory.swap.max) cpu=$quota/$period oom_kill=$before->$after"
"#;
    tokio::fs::write(directory.path().join("Dockerfile"), dockerfile)
        .await
        .context("write BuildKit resource probe Dockerfile")?;

    let memory = "33554432";
    let quota = "25000";
    let period = "100000";
    let mut command = docker_command(docker_host);
    let output = timeout(
        DOCKER_COMMAND_TIMEOUT,
        command
            .args([
                "buildx",
                "build",
                "--pull=false",
                "--no-cache",
                "--network=none",
                "--shm-size",
                "64m",
                "--resource",
                &format!("memory={memory}"),
                "--resource",
                &format!("memory-swap={memory}"),
                "--resource",
                &format!("cpu-period={period}"),
                "--resource",
                &format!("cpu-quota={quota}"),
                "--build-arg",
                &format!("PROBE_IMAGE={image}"),
                "--build-arg",
                &format!("EXPECTED_MEMORY_BYTES={memory}"),
                "--build-arg",
                "EXPECTED_CPU_MILLIS=250",
                "--tag",
                &name,
                "--load",
                directory
                    .path()
                    .to_str()
                    .context("BuildKit probe path is not UTF-8")?,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("BuildKit cgroup resource probe timed out")??;
    let _ = remove_probe_image(docker_host, &name).await;
    if !output.status.success() {
        bail!(
            "Docker BuildKit did not prove per-RUN memory/swap/CPU enforcement; upgrade or configure the selected BuildKit builder before running builds ({}): {}{}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
            String::from_utf8_lossy(&output.stdout)
        );
    }
    let log = String::from_utf8_lossy(&output.stdout);
    if !log.contains("BUILDKIT_CGROUP_LIMITS_PASSED") {
        bail!("Docker BuildKit resource probe completed without its cgroup enforcement marker");
    }
    Ok(())
}

async fn remove_probe_image(docker_host: Option<&str>, image: &str) -> Result<()> {
    let output = timeout(
        DOCKER_COMMAND_TIMEOUT,
        docker_command(docker_host)
            .args(["image", "rm", image])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("remove BuildKit resource probe image timed out")??;
    if output.status.success() || is_docker_not_found(&output.stderr) {
        Ok(())
    } else {
        bail!(
            "remove BuildKit resource probe image: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}

async fn remove_probe_container(docker_host: Option<&str>, name: &str) -> Result<()> {
    let output = timeout(
        DOCKER_COMMAND_TIMEOUT,
        docker_command(docker_host)
            .args(["rm", "--force", name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("remove cgroup memory pressure probe timed out")??;
    if output.status.success() || is_docker_not_found(&output.stderr) {
        Ok(())
    } else {
        bail!(
            "remove cgroup memory pressure probe: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}

fn docker_command(docker_host: Option<&str>) -> Command {
    let mut command = Command::new("docker");
    if let Some(host) = docker_host {
        command.arg("--host").arg(host);
    }
    command
}

async fn docker_output_at<const N: usize>(
    docker_host: Option<&str>,
    args: [&str; N],
) -> Result<String> {
    let output = timeout(
        DOCKER_COMMAND_TIMEOUT,
        docker_command(docker_host)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("Docker security probe timed out")??;
    if !output.status.success() {
        bail!(
            "Docker security probe failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn docker_output<const N: usize>(args: [&str; N]) -> Result<String> {
    let output = timeout(
        DOCKER_COMMAND_TIMEOUT,
        Command::new("docker")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("Docker security probe timed out")??;
    if !output.status.success() {
        bail!(
            "Docker security probe failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn is_docker_not_found(stderr: &[u8]) -> bool {
    let stderr = String::from_utf8_lossy(stderr);
    stderr.contains("not found") || stderr.contains("No such network")
}

#[cfg(test)]
#[path = "sandbox/tests.rs"]
mod tests;

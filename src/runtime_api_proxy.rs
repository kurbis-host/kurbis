//! Narrow HTTP relay for managed Pumpkin plugins.
//!
//! Workload containers stay on an internal, non-egress Docker network.  A
//! per-runtime proxy sidecar is the only container attached to a second
//! egress network.  This process forwards only the server-scoped (or CI
//! preview-scoped) plugin routes below, and forwards the plugin capability
//! unchanged to the control plane.  It never receives a worker, gateway, or
//! database credential.

use std::{
    env,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    os::unix::fs::FileTypeExt,
    process::Stdio,
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::State,
    http::{HeaderMap, Method, Request, StatusCode, header},
    response::Response,
    routing::any,
};
use futures::StreamExt;
use reqwest::{Client, Url, redirect::Policy};
use tokio::{
    net::TcpListener,
    process::Command,
    time::{Instant, sleep, timeout},
};
use uuid::Uuid;

pub const SERVICE_HOSTNAME: &str = "kurbis-runtime-api-proxy";
pub const SERVICE_PORT: u16 = 8080;
#[cfg(target_os = "linux")]
const PROXY_UID: libc::uid_t = 65_532;
#[cfg(target_os = "linux")]
const PROXY_GID: libc::gid_t = 65_532;
pub const SERVICE_BASE_URL: &str = "http://kurbis-runtime-api-proxy:8080";

const MAX_REQUEST_BYTES: usize = 128 * 1024;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(10);
const UPSTREAM_HEALTH_INTERVAL: Duration = Duration::from_secs(5);
const UPSTREAM_RESOLUTION_TIMEOUT: Duration = Duration::from_secs(45);
const MAX_UPSTREAM_ADDRESSES: usize = 32;
const IP_TABLE_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_GET_ATTEMPTS: usize = 2;
const SCOPE_SERVER: &str = "server";
const SCOPE_CI: &str = "ci";
pub(crate) const PROXY_SCOPE_HEADER: &str = "x-kurbis-proxy-scope";
pub(crate) const PROXY_ID_HEADER: &str = "x-kurbis-proxy-id";
pub(crate) const RELAY_SOCKET_ENV: &str = "KURBIS_RUNTIME_API_PROXY_RELAY_SOCKET";

#[derive(Clone, Debug)]
struct ProxyConfig {
    listen: SocketAddr,
    upstream: Url,
    scope: Scope,
    expected_id: Uuid,
    expected_capability_hash: [u8; 32],
    relay_socket: Option<std::path::PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Scope {
    Server,
    Ci,
}

impl Scope {
    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            SCOPE_SERVER => Ok(Self::Server),
            SCOPE_CI => Ok(Self::Ci),
            _ => bail!("runtime API proxy scope must be server or ci"),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Server => SCOPE_SERVER,
            Self::Ci => SCOPE_CI,
        }
    }
}

#[derive(Clone)]
struct ProxyState {
    config: ProxyConfig,
    client: Client,
    upstream_healthy: Arc<AtomicBool>,
}

/// Start a capability-bound proxy attached to exactly one runtime network and
/// the configured egress network. The workload itself is never attached to the
/// egress network.
#[allow(clippy::too_many_arguments)]
pub async fn start_container(
    image: &str,
    scope: &str,
    scoped_id: Uuid,
    generation: i64,
    capability: &str,
    upstream: &str,
    runtime_network: &str,
    egress_network: &str,
    allow_insecure_upstream: bool,
    relay_socket: Option<&std::path::Path>,
) -> Result<()> {
    ensure!(
        generation > 0,
        "runtime API proxy generation must be positive"
    );
    let name = container_name(scoped_id, generation);
    if relay_socket.is_none() {
        ensure!(
            runtime_network != egress_network,
            "runtime and proxy egress networks must differ"
        );
    }
    ensure!(
        valid_token(capability),
        "runtime API proxy capability has invalid format"
    );
    ensure!(
        !image.trim().is_empty()
            && !image.starts_with('-')
            && image.bytes().all(|byte| byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':' | b'@')),
        "runtime API proxy image reference is invalid"
    );
    for network in [runtime_network, egress_network] {
        if relay_socket.is_some() && network == egress_network {
            continue;
        }
        ensure!(
            !network.starts_with('-')
                && !network.is_empty()
                && network
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')),
            "runtime API proxy network name is invalid"
        );
    }
    Scope::parse(scope)?;
    let upstream_url = validate_upstream_url(upstream, allow_insecure_upstream)?;
    if relay_socket.is_none() {
        let internal_egress = verify_egress_network(egress_network).await?;
        validate_runtime_dependency_configuration(internal_egress, &upstream_url)?;
    } else {
        let metadata = tokio::fs::metadata(relay_socket.expect("checked above")).await?;
        ensure!(
            metadata.file_type().is_socket(),
            "runtime API relay socket is unavailable"
        );
    }

    let generation_label = format!("kurbis.generation={generation}");
    let runtime_label = format!("kurbis.runtime_api_proxy={scoped_id}");
    let allow_insecure = allow_insecure_upstream.to_string();
    let listen = format!("0.0.0.0:{SERVICE_PORT}");
    let hash = blake3::hash(capability.as_bytes()).to_hex().to_string();
    let mut docker_args = vec![
        "run".to_owned(),
        "--detach".to_owned(),
        "--name".to_owned(),
        name.clone(),
        "--restart".to_owned(),
        "no".to_owned(),
        "--network".to_owned(),
        runtime_network.to_owned(),
        "--network-alias".to_owned(),
        SERVICE_HOSTNAME.to_owned(),
        "--memory".to_owned(),
        "128m".to_owned(),
        "--memory-swap".to_owned(),
        "128m".to_owned(),
        "--cpus".to_owned(),
        "0.25".to_owned(),
        "--pids-limit".to_owned(),
        "64".to_owned(),
        "--user".to_owned(),
        "0:0".to_owned(),
    ];
    if relay_socket.is_none() {
        docker_args.extend(["--cap-add".into(), "NET_ADMIN".into()]);
    }
    docker_args.extend([
        "--cap-add".into(),
        "SETUID".into(),
        "--cap-add".into(),
        "SETGID".into(),
        "--cap-add".into(),
        "SETPCAP".into(),
        "--read-only".into(),
        "--security-opt".into(),
        "no-new-privileges:true".into(),
        "--cap-drop".into(),
        "ALL".into(),
        "--tmpfs".into(),
        "/tmp:rw,noexec,nosuid,nodev,size=4m".into(),
        "--health-cmd".into(),
        "/usr/local/bin/runtime-api-proxy --health-check".into(),
        "--health-interval".into(),
        "1s".into(),
        "--health-timeout".into(),
        "1s".into(),
        "--health-retries".into(),
        "45".into(),
        "--health-start-period".into(),
        "45s".into(),
        "--label".into(),
        generation_label.clone(),
        "--label".into(),
        runtime_label.clone(),
        "--env".into(),
        format!("KURBIS_RUNTIME_API_PROXY_LISTEN={listen}"),
        "--env".into(),
        format!("KURBIS_RUNTIME_API_PROXY_UPSTREAM={upstream}"),
        "--env".into(),
        format!("KURBIS_RUNTIME_API_PROXY_ALLOW_INSECURE={allow_insecure}"),
        "--env".into(),
        format!("KURBIS_RUNTIME_API_PROXY_SCOPE={scope}"),
        "--env".into(),
        format!("KURBIS_RUNTIME_API_PROXY_ID={scoped_id}"),
        "--env".into(),
        format!("KURBIS_RUNTIME_API_PROXY_CAPABILITY_HASH={hash}"),
    ]);
    if let Some(relay_socket) = relay_socket {
        let relay_dir = relay_socket
            .parent()
            .context("runtime API relay socket has no parent")?;
        docker_args.extend([
            "--env".into(),
            format!(
                "{RELAY_SOCKET_ENV}={}",
                "/run/kurbis-runtime-api/relay.sock"
            ),
            "--mount".into(),
            format!(
                "type=bind,source={},target=/run/kurbis-runtime-api,readonly",
                relay_dir.display()
            ),
        ]);
    }
    docker_args.extend([
        "--entrypoint".into(),
        "/usr/local/bin/runtime-api-proxy".into(),
        image.to_owned(),
    ]);
    let output = timeout(
        Duration::from_secs(30),
        Command::new("docker")
            .args(&docker_args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let output = match output {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            let _ = remove_container(scoped_id, generation).await;
            return Err(error).context("start runtime API proxy container");
        }
        Err(error) => {
            let _ = remove_container(scoped_id, generation).await;
            return Err(error).context("runtime API proxy container startup timed out");
        }
    };
    if !output.status.success() {
        bail!(
            "failed to start runtime API proxy {name}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    if relay_socket.is_none() {
        let connect = match timeout(
            Duration::from_secs(30),
            Command::new("docker")
                .args(["network", "connect", egress_network, &name])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .output(),
        )
        .await
        {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                let _ = remove_container(scoped_id, generation).await;
                return Err(error).context("attach runtime API proxy to its egress network");
            }
            Err(error) => {
                let _ = remove_container(scoped_id, generation).await;
                return Err(error)
                    .context("attach runtime API proxy to its egress network timed out");
            }
        };
        if !connect.status.success() {
            let _ = remove_container(scoped_id, generation).await;
            bail!(
                "failed to attach runtime API proxy to egress network {egress_network}: {}",
                String::from_utf8_lossy(&connect.stderr).trim()
            );
        }
    }

    // Readiness is owned by the proxy process: its egress policy is installed
    // before it accepts traffic, and /healthz remains unavailable until the
    // fixed upstream origin responds with the expected status.
    let health_result = timeout(Duration::from_secs(60), async {
        loop {
            let output = Command::new("docker")
                .args([
                    "inspect",
                    "--format",
                    "{{if .State.Health}}{{.State.Health.Status}}{{else}}missing{{end}}",
                    &name,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
                .await?;
            let health = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if health == "healthy" {
                return Ok::<(), anyhow::Error>(());
            }
            if health == "unhealthy" || health == "missing" {
                bail!("runtime API proxy {name} did not become healthy");
            }
            sleep(Duration::from_millis(250)).await;
        }
    })
    .await;
    if let Err(error) = match health_result {
        Ok(result) => result.context("runtime API proxy did not become healthy"),
        Err(error) => {
            Err(error).context("runtime API proxy did not become healthy within 60 seconds")
        }
    } {
        let _ = remove_container(scoped_id, generation).await;
        return Err(error);
    }
    Ok(())
}

/// Verify the worker-side resources required before a server operation can
/// start.  The worker and Dokploy Compose stack intentionally use different
/// Docker daemons in production, so a network or proxy image that exists in
/// Compose is not evidence that the rootless worker daemon can use it.  Run
/// this check while the worker is booting (before it acquires its lease), not
/// after a player has already been left in `starting`.
pub async fn verify_runtime_dependencies(
    proxy_image: &str,
    egress_network: &str,
    upstream: &str,
    allow_insecure_upstream: bool,
    relay_socket: Option<&std::path::Path>,
) -> Result<()> {
    let upstream = validate_upstream_url(upstream, allow_insecure_upstream)?;
    if relay_socket.is_none() {
        let internal = verify_egress_network(egress_network).await?;
        validate_runtime_dependency_configuration(internal, &upstream)?;
    } else {
        let metadata = tokio::fs::metadata(relay_socket.expect("checked above")).await?;
        ensure!(
            metadata.file_type().is_socket(),
            "runtime API relay socket is unavailable"
        );
    }

    let output = timeout(
        Duration::from_secs(15),
        Command::new("docker")
            .args(["image", "inspect", proxy_image, "--format", "{{.Id}}"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("inspect runtime API proxy image timed out")??;
    if !output.status.success() {
        bail!(
            "runtime API proxy image {proxy_image} is unavailable to the worker Docker daemon: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    ensure!(
        !String::from_utf8_lossy(&output.stdout).trim().is_empty(),
        "runtime API proxy image {proxy_image} has no immutable image ID"
    );
    Ok(())
}

fn validate_runtime_dependency_configuration(internal: bool, upstream: &Url) -> Result<()> {
    ensure!(
        internal || upstream.scheme() == "https",
        "a non-internal runtime API egress network requires an HTTPS control-plane upstream"
    );
    Ok(())
}

async fn verify_egress_network(name: &str) -> Result<bool> {
    let output = timeout(
        Duration::from_secs(10),
        Command::new("docker")
            .args(["network", "inspect", "--format", "{{json .}}", name])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("inspect runtime API egress network timed out")??;
    if !output.status.success() {
        bail!(
            "runtime API egress network {name} is unavailable: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let network: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("Docker returned invalid runtime API egress network metadata")?;
    validate_egress_network_metadata(&network)
}

fn validate_egress_network_metadata(network: &serde_json::Value) -> Result<bool> {
    let driver = network.get("Driver").and_then(serde_json::Value::as_str);
    let internal = network
        .get("Internal")
        .and_then(serde_json::Value::as_bool)
        .context("runtime API egress network has no Internal state")?;
    let ipv6 = network
        .get("EnableIPv6")
        .and_then(serde_json::Value::as_bool);
    ensure!(
        driver == Some("bridge") && ipv6 == Some(false),
        "runtime API egress network must be an IPv4-only bridge"
    );
    Ok(internal)
}

async fn remove_named_container(name: &str) -> Result<()> {
    let output = timeout(
        Duration::from_secs(30),
        Command::new("docker")
            .args(["rm", "--force", name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("runtime API proxy removal timed out")??;
    if output.status.success() || is_docker_not_found(&output.stderr) {
        return Ok(());
    }
    bail!(
        "failed to remove runtime API proxy {name}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

async fn inspect_container_labels(
    name: &str,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>> {
    let output = timeout(
        Duration::from_secs(15),
        Command::new("docker")
            .args(["inspect", "--format", "{{json .Config.Labels}}", name])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("inspect runtime API proxy timed out")??;
    if !output.status.success() {
        if is_docker_not_found(&output.stderr) {
            return Ok(None);
        }
        bail!(
            "failed to inspect runtime API proxy {name}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let labels: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("Docker returned invalid runtime API proxy labels")?;
    Ok(labels.as_object().cloned())
}

fn proxy_generation(
    labels: &serde_json::Map<String, serde_json::Value>,
    scoped_id: Uuid,
) -> Result<i64> {
    let expected_id = scoped_id.to_string();
    ensure!(
        labels
            .get("kurbis.runtime_api_proxy")
            .and_then(serde_json::Value::as_str)
            == Some(expected_id.as_str()),
        "refusing to remove runtime API proxy with a mismatched scoped-id label"
    );
    let generation = labels
        .get("kurbis.generation")
        .and_then(serde_json::Value::as_str)
        .context("runtime API proxy has no valid generation label")?
        .parse::<i64>()
        .context("runtime API proxy has an invalid generation label")?;
    ensure!(
        generation > 0,
        "runtime API proxy generation must be positive"
    );
    Ok(generation)
}

/// Remove one generation's proxy only after checking both ownership labels.
/// The generation is part of the Docker name, so an old worker cannot target a
/// newer proxy by reusing a server/preview-only name.
pub async fn remove_container(scoped_id: Uuid, generation: i64) -> Result<()> {
    ensure!(
        generation > 0,
        "runtime API proxy generation must be positive"
    );
    let name = container_name(scoped_id, generation);
    let Some(labels) = inspect_container_labels(&name).await? else {
        return Ok(());
    };
    verify_exact_proxy_generation(&labels, scoped_id, generation)?;
    remove_named_container(&name).await
}

fn verify_exact_proxy_generation(
    labels: &serde_json::Map<String, serde_json::Value>,
    scoped_id: Uuid,
    expected_generation: i64,
) -> Result<()> {
    ensure!(
        proxy_generation(labels, scoped_id)? == expected_generation,
        "refusing to remove runtime API proxy with a mismatched generation label"
    );
    Ok(())
}

fn proxy_generation_within_fence(
    labels: &serde_json::Map<String, serde_json::Value>,
    scoped_id: Uuid,
    max_generation: i64,
) -> Result<Option<i64>> {
    let generation = proxy_generation(labels, scoped_id)?;
    Ok((generation <= max_generation).then_some(generation))
}

/// Remove all proxy generations up to a caller's durable fence. Newer
/// generations are intentionally left untouched; every candidate is inspected
/// by Docker ID immediately before removal and must carry both ownership labels.
pub async fn remove_containers_through(scoped_id: Uuid, max_generation: i64) -> Result<()> {
    ensure!(
        max_generation > 0,
        "runtime API proxy cleanup fence must be positive"
    );
    let filter = format!("label=kurbis.runtime_api_proxy={scoped_id}");
    let output = timeout(
        Duration::from_secs(30),
        Command::new("docker")
            .args(["ps", "-aq", "--filter", &filter])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("list runtime API proxies timed out")??;
    if !output.status.success() {
        bail!(
            "failed to list runtime API proxies: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    for id in String::from_utf8_lossy(&output.stdout).split_ascii_whitespace() {
        let Some(labels) = inspect_container_labels(id).await? else {
            continue;
        };
        if proxy_generation_within_fence(&labels, scoped_id, max_generation)?.is_some() {
            // Remove by immutable Docker ID, not by a mutable generation-less
            // name. If a concurrent worker replaces a container, the ID cannot
            // be rebound to that newer generation between inspect and remove.
            remove_named_container(id).await?;
        }
    }
    Ok(())
}

pub fn container_name(scoped_id: Uuid, generation: i64) -> String {
    format!("kurbis-api-proxy-{}-g{generation}", scoped_id.simple())
}

/// Install destination-pinned egress policy before binding a listener. DNS is
/// retried because Docker attaches the egress network after starting this
/// container; once resolved, every address is validated, pinned into reqwest,
/// and allowed through the namespace firewall before the process drops root.
pub async fn run_from_env() -> Result<()> {
    let config = ProxyConfig::from_env()?;
    let addresses = if config.relay_socket.is_some() {
        Vec::new()
    } else {
        let addresses = resolve_upstream_until_available(&config.upstream).await?;
        validate_resolved_addresses(&config.upstream, &addresses)?;
        install_egress_firewall(&config.upstream, &addresses).await?;
        addresses
    };
    drop_proxy_privileges()?;

    let client = if let Some(socket) = config.relay_socket.as_ref() {
        Client::builder()
            .unix_socket(socket.clone())
            .no_proxy()
            .redirect(Policy::none())
            .timeout(UPSTREAM_TIMEOUT)
            .build()
            .context("build Unix-socket runtime API relay client")?
    } else {
        build_pinned_client(&config.upstream, &addresses)?
    };
    let upstream_healthy = Arc::new(AtomicBool::new(false));
    let state = ProxyState {
        config,
        client,
        upstream_healthy: upstream_healthy.clone(),
    };
    tokio::spawn(upstream_health_monitor(
        state.config.upstream.clone(),
        state.client.clone(),
        upstream_healthy,
    ));

    let listener = TcpListener::bind(state.config.listen)
        .await
        .context("bind runtime API proxy listener")?;
    tracing::info!(listen = %state.config.listen, upstream = %state.config.upstream, scope = ?state.config.scope, "runtime API proxy ready with destination-restricted egress");
    axum::serve(listener, proxy_router(state))
        .await
        .context("runtime API proxy server failed")
}

async fn resolve_upstream_until_available(upstream: &Url) -> Result<Vec<IpAddr>> {
    let deadline = Instant::now() + UPSTREAM_RESOLUTION_TIMEOUT;
    loop {
        match resolve_upstream_addresses(upstream).await {
            Ok(addresses) => return Ok(addresses),
            Err(error) if Instant::now() < deadline => {
                tracing::debug!(error = %error, "waiting for configured upstream DNS to become available");
                sleep(Duration::from_millis(250)).await;
            }
            Err(error) => return Err(error).context("resolve configured runtime API upstream"),
        }
    }
}

async fn resolve_upstream_addresses(upstream: &Url) -> Result<Vec<IpAddr>> {
    let host = upstream
        .host_str()
        .context("runtime API upstream has no host")?;
    if let Ok(address) = IpAddr::from_str(host) {
        return Ok(vec![address]);
    }
    let port = upstream
        .port_or_known_default()
        .context("runtime API upstream has no port")?;
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("resolve configured upstream host {host}"))?
        .map(|address| address.ip())
        .filter(IpAddr::is_ipv4)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    ensure!(
        !addresses.is_empty() && addresses.len() <= MAX_UPSTREAM_ADDRESSES,
        "configured runtime API upstream returned no addresses or too many addresses"
    );
    Ok(addresses)
}

fn validate_resolved_addresses(upstream: &Url, addresses: &[IpAddr]) -> Result<()> {
    ensure!(
        !addresses.is_empty() && addresses.len() <= MAX_UPSTREAM_ADDRESSES,
        "configured runtime API upstream returned no addresses or too many addresses"
    );
    ensure!(
        addresses.iter().all(IpAddr::is_ipv4),
        "runtime API upstream must resolve to IPv4 addresses because proxy egress IPv6 is disabled"
    );
    let allow_local =
        upstream.scheme() == "http" && is_loopback_or_local_http_host(upstream.host_str());
    for address in addresses {
        ensure!(
            if allow_local {
                is_local_upstream_address(*address)
            } else {
                is_global_unicast_address(*address)
            },
            "configured runtime API upstream resolved to a forbidden address"
        );
    }
    Ok(())
}

fn is_local_upstream_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            address.is_loopback() || address.is_private() || address.is_link_local()
        }
        IpAddr::V6(address) => {
            address.is_loopback() || address.is_unique_local() || address.is_unicast_link_local()
        }
    }
}

/// Conservative public-unicast test for DNS answers used by the remote HTTPS
/// control plane. Reject special-purpose, translation, documentation,
/// benchmarking, private, local, multicast, and unspecified ranges.
fn is_global_unicast_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_global_unicast_ipv4(address),
        IpAddr::V6(address) => {
            if let Some(mapped) = address.to_ipv4_mapped() {
                return is_global_unicast_ipv4(mapped);
            }
            let segments = address.segments();
            let in_global_unicast_space = segments[0] & 0xe000 == 0x2000;
            let in_2001_special_purpose = segments[0] == 0x2001 && segments[1] <= 0x01ff;
            let in_documentation_range = segments[0] == 0x2001 && segments[1] == 0x0db8
                || segments[0] == 0x3fff && segments[1] & 0xfff0 == 0;
            let is_6to4 = segments[0] == 0x2002;
            in_global_unicast_space
                && !in_2001_special_purpose
                && !in_documentation_range
                && !is_6to4
                && !address.is_unspecified()
                && !address.is_loopback()
                && !address.is_multicast()
                && !address.is_unique_local()
                && !address.is_unicast_link_local()
        }
    }
}

fn is_global_unicast_ipv4(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    let in_shared_address_space = octets[0] == 100 && (64..=127).contains(&octets[1]);
    let in_special_192_0_0 = octets[0] == 192 && octets[1] == 0 && octets[2] == 0;
    let in_benchmark_range = octets[0] == 198 && matches!(octets[1], 18 | 19);
    let in_documentation_range = (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113);
    !address.is_private()
        && !address.is_loopback()
        && !address.is_link_local()
        && !address.is_multicast()
        && !address.is_broadcast()
        && !address.is_unspecified()
        && !in_shared_address_space
        && !in_special_192_0_0
        && !in_benchmark_range
        && !in_documentation_range
        && octets[0] != 0
        && octets[0] < 224
}

fn build_pinned_client(upstream: &Url, addresses: &[IpAddr]) -> Result<Client> {
    let host = upstream
        .host_str()
        .context("runtime API upstream has no host")?;
    let port = upstream
        .port_or_known_default()
        .context("runtime API upstream has no port")?;
    let resolved = addresses
        .iter()
        .copied()
        .map(|address| SocketAddr::new(address, port))
        .collect::<Vec<_>>();
    Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .timeout(UPSTREAM_TIMEOUT)
        .resolve_to_addrs(host, &resolved)
        .build()
        .context("build destination-pinned runtime API HTTP client")
}

async fn upstream_health_monitor(upstream: Url, client: Client, healthy: Arc<AtomicBool>) {
    let mut health_url = upstream;
    health_url.set_path("/healthz");
    health_url.set_query(None);
    health_url.set_fragment(None);
    loop {
        let result = timeout(
            Duration::from_secs(5),
            client.get(health_url.clone()).send(),
        )
        .await;
        let ready =
            matches!(result, Ok(Ok(response)) if response.status() == StatusCode::NO_CONTENT);
        healthy.store(ready, Ordering::Release);
        if !ready {
            tracing::warn!(upstream = %health_url, "configured runtime API upstream is not healthy; proxy remains unready");
        }
        sleep(UPSTREAM_HEALTH_INTERVAL).await;
    }
}

pub async fn health_check() -> Result<()> {
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()?
        .get(format!("http://127.0.0.1:{SERVICE_PORT}/healthz"))
        .send()
        .await
        .context("runtime API proxy health request failed")?;
    ensure!(
        response.status() == StatusCode::NO_CONTENT,
        "runtime API proxy is not ready"
    );
    Ok(())
}

fn proxy_router(state: ProxyState) -> Router {
    Router::new()
        .route("/", any(handle))
        .fallback(handle)
        .with_state(state)
}

async fn install_egress_firewall(upstream: &Url, addresses: &[IpAddr]) -> Result<()> {
    let port = upstream
        .port_or_known_default()
        .context("runtime API upstream has no port")?;
    ensure!(port != 53, "runtime API upstream may not use a DNS port");
    ensure!(
        !addresses.is_empty() && addresses.len() <= MAX_UPSTREAM_ADDRESSES,
        "runtime API upstream address set is empty or too large"
    );
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "runtime API proxy egress firewall requires container-namespace root"
    );

    // Flush only this container's OUTPUT chain. A DROP default policy is set
    // before any allow rule so every intermediate failure stays fail-closed.
    for (program, ipv6) in [("/usr/sbin/iptables", false), ("/usr/sbin/ip6tables", true)] {
        run_firewall_command(program, &["-w", "-F", "OUTPUT"]).await?;
        run_firewall_command(program, &["-w", "-P", "OUTPUT", "DROP"]).await?;
        if ipv6 {
            verify_firewall_output_policy(program, 0).await?;
            continue;
        }

        // Docker's embedded resolver is commonly 127.0.0.11. Block DNS before
        // loopback allowances to prevent arbitrary DNS tunneling after startup.
        for protocol in ["udp", "tcp"] {
            run_firewall_command(
                program,
                &[
                    "-w", "-A", "OUTPUT", "-p", protocol, "--dport", "53", "-j", "REJECT",
                ],
            )
            .await?;
        }
        // Permit return packets only for flows already admitted below or the
        // local health connection. DNS has already been explicitly rejected.
        run_firewall_command(
            program,
            &[
                "-w",
                "-A",
                "OUTPUT",
                "-m",
                "conntrack",
                "--ctstate",
                "ESTABLISHED,RELATED",
                "-j",
                "ACCEPT",
            ],
        )
        .await?;
        // The local health client connects to the process itself; its SYN-ACK
        // is covered by the established-state rule above.
        run_firewall_command(
            program,
            &[
                "-w",
                "-A",
                "OUTPUT",
                "-o",
                "lo",
                "-d",
                "127.0.0.1/32",
                "-p",
                "tcp",
                "--dport",
                "8080",
                "-j",
                "ACCEPT",
            ],
        )
        .await?;
        for address in addresses {
            let address = address.to_string();
            run_firewall_command(
                program,
                &[
                    "-w",
                    "-A",
                    "OUTPUT",
                    "-d",
                    &address,
                    "-p",
                    "tcp",
                    "--dport",
                    &port.to_string(),
                    "-j",
                    "ACCEPT",
                ],
            )
            .await?;
        }
        verify_firewall_output_policy(program, addresses.len() + 4).await?;
    }
    Ok(())
}

async fn run_firewall_command(program: &str, arguments: &[&str]) -> Result<()> {
    let output = timeout(
        IP_TABLE_COMMAND_TIMEOUT,
        Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .with_context(|| format!("{program} command timed out"))??;
    ensure!(
        output.status.success(),
        "{program} {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
            .trim()
            .chars()
            .take(1024)
            .collect::<String>()
    );
    Ok(())
}

async fn verify_firewall_output_policy(program: &str, expected_rules: usize) -> Result<()> {
    let output = timeout(
        IP_TABLE_COMMAND_TIMEOUT,
        Command::new(program)
            .args(["-S", "OUTPUT"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .with_context(|| format!("verify {program} output policy timed out"))??;
    ensure!(
        output.status.success(),
        "failed to read {program} output policy"
    );
    let rules = String::from_utf8_lossy(&output.stdout);
    ensure!(
        rules.lines().any(|line| line == "-P OUTPUT DROP")
            && rules
                .lines()
                .filter(|line| line.starts_with("-A OUTPUT "))
                .count()
                == expected_rules,
        "{program} output policy does not match the fail-closed rule set"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn drop_proxy_privileges() -> Result<()> {
    unsafe {
        ensure!(libc::geteuid() == 0, "proxy privilege drop requires uid 0");
        // Drop every capability from the bounding set before changing IDs.
        for capability in 0..=63 {
            if libc::prctl(libc::PR_CAPBSET_DROP, capability, 0, 0, 0) != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EINVAL) {
                    return Err(error).context("drop runtime API proxy capability bounding set");
                }
            }
        }
        if libc::setgroups(0, std::ptr::null()) != 0 {
            return Err(std::io::Error::last_os_error())
                .context("clear runtime API proxy supplementary groups");
        }
        if libc::setgid(PROXY_GID) != 0 {
            return Err(std::io::Error::last_os_error())
                .context("drop runtime API proxy group privileges");
        }
        if libc::setuid(PROXY_UID) != 0 {
            return Err(std::io::Error::last_os_error())
                .context("drop runtime API proxy user privileges");
        }
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(std::io::Error::last_os_error())
                .context("prevent runtime API proxy privilege regain");
        }
        ensure!(
            libc::geteuid() == PROXY_UID && libc::getegid() == PROXY_GID,
            "runtime API proxy privilege drop did not take effect"
        );
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn drop_proxy_privileges() -> Result<()> {
    bail!("runtime API proxy egress policy requires Linux")
}

pub fn validate_upstream_url(value: &str, allow_insecure: bool) -> Result<Url> {
    let upstream = Url::parse(value).context("invalid runtime API proxy upstream URL")?;
    ensure!(
        matches!(upstream.scheme(), "http" | "https")
            && upstream.host_str().is_some()
            && upstream.username().is_empty()
            && upstream.password().is_none()
            && upstream.query().is_none()
            && upstream.fragment().is_none()
            && upstream.path() == "/",
        "runtime API proxy upstream must be an HTTP(S) origin without credentials or a path"
    );
    ensure!(
        upstream.scheme() == "https"
            || (allow_insecure
                && upstream.scheme() == "http"
                && is_loopback_or_local_http_host(upstream.host_str())),
        "runtime API proxy upstream must use HTTPS; local HTTP requires an explicit opt-in"
    );
    Ok(upstream)
}

impl ProxyConfig {
    fn from_env() -> Result<Self> {
        let listen = env::var("KURBIS_RUNTIME_API_PROXY_LISTEN")
            .unwrap_or_else(|_| format!("0.0.0.0:{SERVICE_PORT}"))
            .parse()
            .context("invalid KURBIS_RUNTIME_API_PROXY_LISTEN")?;
        let upstream = validate_upstream_url(
            &env::var("KURBIS_RUNTIME_API_PROXY_UPSTREAM")
                .context("KURBIS_RUNTIME_API_PROXY_UPSTREAM is required")?,
            env::var("KURBIS_RUNTIME_API_PROXY_ALLOW_INSECURE").is_ok_and(|value| value == "true"),
        )?;
        let scope = Scope::parse(
            &env::var("KURBIS_RUNTIME_API_PROXY_SCOPE")
                .context("KURBIS_RUNTIME_API_PROXY_SCOPE is required")?,
        )?;
        let expected_id = env::var("KURBIS_RUNTIME_API_PROXY_ID")
            .context("KURBIS_RUNTIME_API_PROXY_ID is required")?
            .parse()
            .context("invalid runtime API proxy scope ID")?;
        let hash = env::var("KURBIS_RUNTIME_API_PROXY_CAPABILITY_HASH")
            .context("KURBIS_RUNTIME_API_PROXY_CAPABILITY_HASH is required")?;
        let hash = hex_decode_32(&hash).context("invalid runtime API proxy capability hash")?;
        let relay_socket = env::var(RELAY_SOCKET_ENV)
            .ok()
            .map(std::path::PathBuf::from);
        Ok(Self {
            listen,
            upstream,
            scope,
            expected_id,
            expected_capability_hash: hash,
            relay_socket,
        })
    }
}

async fn handle(State(state): State<ProxyState>, request: Request<Body>) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    if path == "/healthz" && method == Method::GET {
        return status(if state.upstream_healthy.load(Ordering::Acquire) {
            StatusCode::NO_CONTENT
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        });
    }
    if request.uri().authority().is_some()
        || !route_allowed(state.config.scope, &method, &path, state.config.expected_id)
    {
        return status(StatusCode::NOT_FOUND);
    }

    let Some(token) = bearer(request.headers()) else {
        return status(StatusCode::UNAUTHORIZED);
    };
    if [header::CONTENT_TYPE, header::ACCEPT]
        .into_iter()
        .any(|name| request.headers().get_all(name).iter().count() > 1)
    {
        return status(StatusCode::BAD_REQUEST);
    }
    if !valid_token(token)
        || !constant_time_hash_eq(
            blake3::hash(token.as_bytes()).as_bytes(),
            &state.config.expected_capability_hash,
        )
    {
        return status(StatusCode::UNAUTHORIZED);
    }

    let query = request.uri().query().map(str::to_owned);
    let mut headers = request.headers().clone();
    let upstream = match upstream_url(&state.config.upstream, &path, query.as_deref()) {
        Ok(url) => url,
        Err(_) => return status(StatusCode::BAD_GATEWAY),
    };
    let body = match to_bytes(request.into_body(), MAX_REQUEST_BYTES).await {
        Ok(body) => body,
        Err(_) => return status(StatusCode::PAYLOAD_TOO_LARGE),
    };
    headers.insert(
        PROXY_SCOPE_HEADER,
        axum::http::HeaderValue::from_static(state.config.scope.as_str()),
    );
    headers.insert(
        PROXY_ID_HEADER,
        axum::http::HeaderValue::from_str(&state.config.expected_id.to_string())
            .expect("uuid header is valid"),
    );
    let result = forward(&state, method, headers, body, upstream).await;
    match result {
        Ok(response) => response,
        Err(ForwardError::Timeout) => status(StatusCode::GATEWAY_TIMEOUT),
        Err(ForwardError::Unavailable) => status(StatusCode::BAD_GATEWAY),
        Err(ForwardError::ResponseTooLarge) => status(StatusCode::BAD_GATEWAY),
    }
}

pub(crate) fn route_allowed(scope: Scope, method: &Method, path: &str, expected_id: Uuid) -> bool {
    let segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    match scope {
        Scope::Server => {
            if segments == ["api", "v1", "server", "login-links"] {
                return method == Method::POST;
            }
            if segments.len() < 4 || segments[..3] != ["api", "v1", "servers"] {
                return false;
            }
            if segments[3] != expected_id.to_string() {
                return false;
            }
            match segments.get(4..) {
                Some([]) => matches!(*method, Method::GET | Method::PATCH),
                Some(["players" | "logs" | "activity" | "snapshots" | "runtime-stats" | "ai"]) => {
                    *method == Method::GET
                }
                Some(["whitelist"]) => {
                    matches!(*method, Method::GET | Method::POST | Method::DELETE)
                }
                Some(["commands"] | ["ai", "jobs" | "sync"]) => *method == Method::POST,
                Some(["ai", "jobs", job_id, "cancel"]) => {
                    *method == Method::POST && Uuid::parse_str(job_id).is_ok()
                }
                _ => false,
            }
        }
        Scope::Ci => {
            if segments.len() < 4 || segments[..3] != ["internal", "ci", "previews"] {
                return false;
            }
            if segments[3] != expected_id.to_string() {
                return false;
            }
            match segments.get(4..) {
                Some([]) => matches!(*method, Method::GET | Method::PATCH),
                Some(["players"]) => *method == Method::GET,
                Some(["commands"]) => *method == Method::POST,
                _ => false,
            }
        }
    }
}

fn upstream_url(base: &Url, path: &str, query: Option<&str>) -> Result<Url> {
    let mut url = base.clone();
    url.set_path(path);
    url.set_query(query);
    Ok(url)
}

#[derive(Debug)]
enum ForwardError {
    Timeout,
    Unavailable,
    ResponseTooLarge,
}

async fn forward(
    state: &ProxyState,
    method: Method,
    incoming_headers: HeaderMap,
    body: Bytes,
    upstream: Url,
) -> Result<Response, ForwardError> {
    let attempts = if method == Method::GET {
        MAX_GET_ATTEMPTS
    } else {
        1
    };
    for attempt in 0..attempts {
        let mut request = state.client.request(method.clone(), upstream.clone());
        request = request.header(
            header::AUTHORIZATION,
            incoming_headers
                .get(header::AUTHORIZATION)
                .expect("checked bearer header"),
        );
        for name in [header::CONTENT_TYPE, header::ACCEPT] {
            if let Some(value) = incoming_headers.get(&name) {
                request = request.header(name, value);
            }
        }
        match request.body(body.clone()).send().await {
            Ok(response) => {
                let status_code = response.status();
                let headers = response.headers().clone();
                let mut stream = response.bytes_stream();
                let mut bytes = Vec::with_capacity(MAX_RESPONSE_BYTES.min(16 * 1024));
                let mut retryable_error = None;
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(chunk) => {
                            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                                return Err(ForwardError::ResponseTooLarge);
                            }
                            bytes.extend_from_slice(&chunk);
                        }
                        Err(error) => {
                            retryable_error = Some(if error.is_timeout() {
                                ForwardError::Timeout
                            } else {
                                ForwardError::Unavailable
                            });
                            break;
                        }
                    }
                }
                if let Some(error) = retryable_error {
                    if attempt + 1 < attempts
                        && matches!(error, ForwardError::Timeout | ForwardError::Unavailable)
                    {
                        continue;
                    }
                    return Err(error);
                }
                return Ok(copy_response(status_code, &headers, Bytes::from(bytes)));
            }
            Err(error) if (error.is_timeout() || error.is_connect()) && attempt + 1 < attempts => {}
            Err(error) if error.is_timeout() => return Err(ForwardError::Timeout),
            Err(_) => return Err(ForwardError::Unavailable),
        }
    }
    Err(ForwardError::Unavailable)
}

fn copy_response(status_code: reqwest::StatusCode, source: &HeaderMap, body: Bytes) -> Response {
    let status = StatusCode::from_u16(status_code.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    for name in [header::CONTENT_TYPE, header::CACHE_CONTROL] {
        if let Some(value) = source.get(&name) {
            response.headers_mut().insert(name, value.clone());
        }
    }
    response
}

fn is_docker_not_found(stderr: &[u8]) -> bool {
    let stderr = String::from_utf8_lossy(stderr);
    stderr.contains("No such object")
        || stderr.contains("No such container")
        || stderr.contains("not found")
}

fn status(status: StatusCode) -> Response {
    Response::builder()
        .status(status)
        .body(Body::empty())
        .expect("static status response is valid")
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    if headers.get_all(header::AUTHORIZATION).iter().count() != 1 {
        return None;
    }
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

fn constant_time_hash_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

pub(crate) fn valid_token(token: &str) -> bool {
    token.len() == 43
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[allow(clippy::chunks_exact_to_as_chunks)]
fn hex_decode_32(value: &str) -> Result<[u8; 32]> {
    ensure!(value.len() == 64, "capability hash must be 32-byte hex");
    let mut bytes = [0_u8; 32];
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = (hex_nibble(chunk[0])? << 4) | hex_nibble(chunk[1])?;
    }
    Ok(bytes)
}

fn hex_nibble(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => bail!("invalid hexadecimal digit"),
    }
}

fn is_loopback_or_local_http_host(host: Option<&str>) -> bool {
    host.is_some_and(|host| {
        host.eq_ignore_ascii_case("host.docker.internal")
            || host.eq_ignore_ascii_case("control-plane")
            || host.eq_ignore_ascii_case("runtime-api-relay")
            || host.eq_ignore_ascii_case("localhost")
            || host == "127.0.0.1"
            || host == "[::1]"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| match address {
                    std::net::IpAddr::V4(address) => address.is_loopback() || address.is_private(),
                    std::net::IpAddr::V6(address) => {
                        address.is_loopback() || address.is_unique_local()
                    }
                })
    })
}

#[cfg(test)]
#[path = "runtime_api_proxy/tests.rs"]
mod tests;

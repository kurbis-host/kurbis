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

#[derive(Clone, Debug)]
struct ProxyConfig {
    listen: SocketAddr,
    upstream: Url,
    scope: Scope,
    expected_id: Uuid,
    expected_capability_hash: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scope {
    Server,
    Ci,
}

impl Scope {
    fn parse(value: &str) -> Result<Self> {
        match value {
            SCOPE_SERVER => Ok(Self::Server),
            SCOPE_CI => Ok(Self::Ci),
            _ => bail!("runtime API proxy scope must be server or ci"),
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
    name: &str,
    scope: &str,
    scoped_id: Uuid,
    generation: i64,
    capability: &str,
    upstream: &str,
    runtime_network: &str,
    egress_network: &str,
    allow_insecure_upstream: bool,
) -> Result<()> {
    ensure!(
        generation > 0,
        "runtime API proxy generation must be positive"
    );
    ensure!(
        runtime_network != egress_network,
        "runtime and proxy egress networks must differ"
    );
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
    let internal_egress = verify_egress_network(egress_network).await?;
    ensure!(
        internal_egress || upstream_url.scheme() == "https",
        "non-internal runtime API egress requires an HTTPS control-plane upstream"
    );

    let generation_label = format!("kurbis.generation={generation}");
    let runtime_label = format!("kurbis.runtime_api_proxy={scoped_id}");
    let allow_insecure = allow_insecure_upstream.to_string();
    let listen = format!("0.0.0.0:{SERVICE_PORT}");
    let hash = blake3::hash(capability.as_bytes()).to_hex().to_string();
    let output = timeout(
        Duration::from_secs(30),
        Command::new("docker")
            .args([
                "run",
                "--detach",
                "--name",
                name,
                "--restart",
                "no",
                "--network",
                runtime_network,
                "--network-alias",
                SERVICE_HOSTNAME,
                "--memory",
                "128m",
                "--memory-swap",
                "128m",
                "--cpus",
                "0.25",
                "--pids-limit",
                "64",
                // Root is container-namespace root only. The process installs its
                // namespace-local OUTPUT policy, then irreversibly drops privileges.
                "--user",
                "0:0",
                "--cap-add",
                "NET_ADMIN",
                "--cap-add",
                "SETUID",
                "--cap-add",
                "SETGID",
                "--cap-add",
                "SETPCAP",
                "--read-only",
                "--security-opt",
                "no-new-privileges:true",
                "--cap-drop",
                "ALL",
                "--tmpfs",
                "/tmp:rw,noexec,nosuid,nodev,size=4m",
                "--health-cmd",
                "/usr/local/bin/runtime-api-proxy --health-check",
                "--health-interval",
                "1s",
                "--health-timeout",
                "1s",
                "--health-retries",
                "45",
                "--health-start-period",
                "45s",
                "--label",
                &generation_label,
                "--label",
                &runtime_label,
                "--env",
                &format!("KURBIS_RUNTIME_API_PROXY_LISTEN={listen}"),
                "--env",
                &format!("KURBIS_RUNTIME_API_PROXY_UPSTREAM={upstream}"),
                "--env",
                &format!("KURBIS_RUNTIME_API_PROXY_ALLOW_INSECURE={allow_insecure}"),
                "--env",
                &format!("KURBIS_RUNTIME_API_PROXY_SCOPE={scope}"),
                "--env",
                &format!("KURBIS_RUNTIME_API_PROXY_ID={scoped_id}"),
                "--env",
                &format!("KURBIS_RUNTIME_API_PROXY_CAPABILITY_HASH={hash}"),
                "--entrypoint",
                "/usr/local/bin/runtime-api-proxy",
                image,
            ])
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
            let _ = remove_container(name).await;
            return Err(error).context("start runtime API proxy container");
        }
        Err(error) => {
            let _ = remove_container(name).await;
            return Err(error).context("runtime API proxy container startup timed out");
        }
    };
    if !output.status.success() {
        bail!(
            "failed to start runtime API proxy {name}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let connect = match timeout(
        Duration::from_secs(30),
        Command::new("docker")
            .args(["network", "connect", egress_network, name])
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
            let _ = remove_container(name).await;
            return Err(error).context("attach runtime API proxy to its egress network");
        }
        Err(error) => {
            let _ = remove_container(name).await;
            return Err(error).context("attach runtime API proxy to its egress network timed out");
        }
    };
    if !connect.status.success() {
        let _ = remove_container(name).await;
        bail!(
            "failed to attach runtime API proxy to egress network {egress_network}: {}",
            String::from_utf8_lossy(&connect.stderr).trim()
        );
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
                    name,
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
        let _ = remove_container(name).await;
        return Err(error);
    }
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

pub async fn remove_container(name: &str) -> Result<()> {
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

pub fn container_name(scoped_id: Uuid) -> String {
    format!("kurbis-api-proxy-{}", scoped_id.simple())
}

/// Install destination-pinned egress policy before binding a listener. DNS is
/// retried because Docker attaches the egress network after starting this
/// container; once resolved, every address is validated, pinned into reqwest,
/// and allowed through the namespace firewall before the process drops root.
pub async fn run_from_env() -> Result<()> {
    let config = ProxyConfig::from_env()?;
    let addresses = resolve_upstream_until_available(&config.upstream).await?;
    validate_resolved_addresses(&config.upstream, &addresses)?;
    install_egress_firewall(&config.upstream, &addresses).await?;
    drop_proxy_privileges()?;

    let client = build_pinned_client(&config.upstream, &addresses)?;
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
        Ok(Self {
            listen,
            upstream,
            scope,
            expected_id,
            expected_capability_hash: hash,
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
    let headers = request.headers().clone();
    let upstream = match upstream_url(&state.config.upstream, &path, query.as_deref()) {
        Ok(url) => url,
        Err(_) => return status(StatusCode::BAD_GATEWAY),
    };
    let body = match to_bytes(request.into_body(), MAX_REQUEST_BYTES).await {
        Ok(body) => body,
        Err(_) => return status(StatusCode::PAYLOAD_TOO_LARGE),
    };
    let result = forward(&state, method, headers, body, upstream).await;
    match result {
        Ok(response) => response,
        Err(ForwardError::Timeout) => status(StatusCode::GATEWAY_TIMEOUT),
        Err(ForwardError::Unavailable) => status(StatusCode::BAD_GATEWAY),
        Err(ForwardError::ResponseTooLarge) => status(StatusCode::BAD_GATEWAY),
    }
}

fn route_allowed(scope: Scope, method: &Method, path: &str, expected_id: Uuid) -> bool {
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
                Some([resource]) => match *resource {
                    "players" | "logs" | "activity" | "snapshots" => *method == Method::GET,
                    "whitelist" => matches!(*method, Method::GET | Method::POST | Method::DELETE),
                    "commands" => *method == Method::POST,
                    _ => false,
                },
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

fn valid_token(token: &str) -> bool {
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

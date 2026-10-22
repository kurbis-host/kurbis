use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use tokio::{
    process::Command,
    time::{sleep, timeout},
};
use uuid::Uuid;

use super::{GatewayIngressPolicy, PublishedWorkloadPorts, docker_publish_binding};
use crate::worker::sandbox::DockerSandbox;

const JAVA_PORT: u16 = 25565;
const BEDROCK_PORT: u16 = 19132;

pub fn container_name(runtime_id: Uuid, generation: i64) -> Result<String> {
    ensure!(generation > 0, "ingress relay generation must be positive");
    Ok(format!(
        "kurbis-ingress-{}-g{generation}",
        runtime_id.simple()
    ))
}

/// Whether the relay's own health check currently passes. That check is a
/// Java status ping to the Pumpkin backend the relay forwards to.
pub async fn is_healthy(runtime_id: Uuid, generation: i64) -> Result<bool> {
    let name = container_name(runtime_id, generation)?;
    let output = Command::new("docker")
        .args(["inspect", "--format", "{{.State.Health.Status}}", &name])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    Ok(output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "healthy")
}

pub fn network_name(runtime_id: Uuid, generation: i64) -> Result<String> {
    ensure!(
        generation > 0,
        "ingress network generation must be positive"
    );
    Ok(format!(
        "kurbis-ingress-{}-g{generation}",
        runtime_id.simple()
    ))
}

/// Create the per-runtime public-facing bridge. It is deliberately separate
/// from the internal workload bridge and is attached only to the trusted relay.
pub async fn create_network(runtime_id: Uuid, generation: i64) -> Result<String> {
    let name = network_name(runtime_id, generation)?;
    let server_label = format!("kurbis.server_id={runtime_id}");
    let generation_label = format!("kurbis.generation={generation}");
    let output = Command::new("docker")
        .args([
            "network",
            "create",
            "--driver",
            "bridge",
            "--opt",
            "com.docker.network.bridge.enable_icc=false",
            "--opt",
            "com.docker.network.bridge.enable_ip_masquerade=false",
            "--label",
            &server_label,
            "--label",
            &generation_label,
            "--label",
            "kurbis.network=ingress",
            &name,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await?;
    if !output.status.success() {
        anyhow::bail!(
            "failed to create rootless ingress network {name}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    verify_network(&name, runtime_id, generation, &[]).await?;
    Ok(name)
}

pub async fn verify_network(
    name: &str,
    runtime_id: Uuid,
    generation: i64,
    expected_containers: &[String],
) -> Result<()> {
    ensure!(
        name == network_name(runtime_id, generation)?,
        "ingress network name does not match runtime generation"
    );
    let output = Command::new("docker")
        .args(["network", "inspect", "--format", "{{json .}}", name])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await?;
    ensure!(
        output.status.success(),
        "failed to inspect rootless ingress network {name}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let network: Value = serde_json::from_slice(&output.stdout)
        .context("Docker returned invalid ingress network metadata")?;
    verify_network_metadata(&network, runtime_id, generation, expected_containers)
}

fn verify_network_metadata(
    network: &Value,
    runtime_id: Uuid,
    generation: i64,
    expected_containers: &[String],
) -> Result<()> {
    let name = network.get("Name").and_then(Value::as_str);
    let driver = network.get("Driver").and_then(Value::as_str);
    let internal = network.get("Internal").and_then(Value::as_bool);
    let ipv6 = network.get("EnableIPv6").and_then(Value::as_bool);
    let options = network.get("Options").and_then(Value::as_object);
    let labels = network.get("Labels").and_then(Value::as_object);
    let icc_disabled = options
        .and_then(|values| values.get("com.docker.network.bridge.enable_icc"))
        .and_then(Value::as_str)
        == Some("false");
    let masquerade_disabled = options
        .and_then(|values| values.get("com.docker.network.bridge.enable_ip_masquerade"))
        .and_then(Value::as_str)
        == Some("false");
    let labels_match = labels.is_some_and(|values| {
        values.get("kurbis.server_id").and_then(Value::as_str) == Some(&runtime_id.to_string())
            && values.get("kurbis.generation").and_then(Value::as_str)
                == Some(&generation.to_string())
            && values.get("kurbis.network").and_then(Value::as_str) == Some("ingress")
    });
    let mut connected = network
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
    connected.sort();
    let mut expected = expected_containers.to_vec();
    expected.sort();
    ensure!(
        name == Some(network_name(runtime_id, generation)?.as_str())
            && driver == Some("bridge")
            && internal == Some(false)
            && ipv6 == Some(false)
            && icc_disabled
            && masquerade_disabled
            && labels_match
            && connected == expected,
        "rootless public ingress network policy is not enforced (driver={driver:?}, internal={internal:?}, ipv6={ipv6:?}, icc_disabled={icc_disabled}, masquerade_disabled={masquerade_disabled}, labels_match={labels_match}, attached={connected:?})"
    );
    Ok(())
}

/// Resolve the IPv4 endpoint from the runtime's exact internal network rather
/// than trusting a user-provided name or a DNS search result.
pub async fn container_ipv4(container: &str, internal_network: &str) -> Result<Ipv4Addr> {
    let output = Command::new("docker")
        .args([
            "inspect",
            "--format",
            "{{json .NetworkSettings.Networks}}",
            container,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await?;
    ensure!(
        output.status.success(),
        "failed to inspect ingress backend {container}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let networks: Value = serde_json::from_slice(&output.stdout)
        .context("Docker returned invalid runtime network metadata")?;
    let ip = networks
        .get(internal_network)
        .and_then(|network| network.get("IPAddress"))
        .and_then(Value::as_str)
        .context("runtime is not connected to its exact internal network")?
        .parse::<Ipv4Addr>()
        .context("runtime internal network has no IPv4 address")?;
    ensure!(
        !ip.is_unspecified() && !ip.is_loopback() && !ip.is_multicast(),
        "invalid runtime endpoint address"
    );
    Ok(ip)
}

/// Start a port-publishing, per-runtime TCP/UDP relay. The process has no
/// account/control-plane secrets, and its own network namespace firewall only
/// allows new connections to this generation's exact Pumpkin IPv4 endpoint.
pub async fn start_container(
    image: &str,
    runtime_id: Uuid,
    generation: i64,
    backend: Ipv4Addr,
    internal_network: &str,
    ingress_network: &str,
    ports: PublishedWorkloadPorts,
) -> Result<()> {
    ensure!(
        generation > 0,
        "ingress relay requires a positive generation"
    );
    ensure!(
        image.bytes().all(|byte| byte.is_ascii_alphanumeric()
            || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':' | b'@')),
        "ingress relay image reference is invalid"
    );
    ensure!(
        !internal_network.is_empty()
            && !internal_network.starts_with('-')
            && internal_network
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
            && !ingress_network.is_empty()
            && !ingress_network.starts_with('-')
            && ingress_network
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')),
        "ingress relay network name is invalid"
    );
    let name = container_name(runtime_id, generation)?;
    let network = network_name(runtime_id, generation)?;
    ensure!(
        network == ingress_network,
        "ingress relay network does not match runtime generation"
    );
    let java =
        docker_publish_binding(ports.java_tcp.ip(), ports.java_tcp.port(), JAVA_PORT, "tcp")?;
    let bedrock = docker_publish_binding(
        ports.bedrock_udp.ip(),
        ports.bedrock_udp.port(),
        BEDROCK_PORT,
        "udp",
    )?;
    let signal = docker_publish_binding(
        ports.bedrock_signal_tcp.ip(),
        ports.bedrock_signal_tcp.port(),
        BEDROCK_PORT,
        "tcp",
    )?;
    let generation_label = format!("kurbis.generation={generation}");
    let runtime_label = format!("kurbis.ingress_relay={runtime_id}");
    let server_label = format!("kurbis.server_id={runtime_id}");
    let backend = backend.to_string();
    let mut docker_args = [
        "run",
        "--detach",
        "--name",
        &name,
        "--restart",
        "no",
        "--network",
        ingress_network,
        "--memory",
        "256m",
        "--memory-swap",
        "256m",
        "--cpus",
        "0.5",
        "--pids-limit",
        "64",
        // PID 1 must reap the health-check execs Docker kills on timeout;
        // without it they stay zombies and exhaust the PID limit.
        "--init",
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
        "/usr/local/bin/runtime-api-proxy --ingress-health-check",
        "--health-interval",
        "2s",
        "--health-timeout",
        "2s",
        "--health-retries",
        "10",
        "--health-start-period",
        "120s",
        "--health-start-interval",
        crate::runtime_api_proxy::HEALTH_START_INTERVAL,
        "--label",
        &generation_label,
        "--label",
        &runtime_label,
        "--label",
        &server_label,
        "--env",
        &format!("KURBIS_INGRESS_BACKEND_IPV4={backend}"),
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    // One publish per host port. Rootless Docker forwards every published
    // address to the same loopback port inside its network namespace, so
    // publishing the Java port a second time on host loopback for health
    // probes made the second proxy wait for a port the first one held, time
    // out, and leave a forward behind that blocked every later start.
    // Readiness is taken from the relay's own health check instead.
    push_publish(&mut docker_args, &java);
    push_publish(&mut docker_args, &bedrock);
    push_publish(&mut docker_args, &signal);
    docker_args.extend(
        [
            "--entrypoint",
            "/usr/local/bin/runtime-api-proxy",
            image,
            "--workload-ingress",
        ]
        .map(str::to_owned),
    );
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
    .await
    .context("ingress relay startup timed out")??;
    ensure!(
        output.status.success(),
        "failed to start rootless ingress relay {name}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );

    let connect = timeout(
        Duration::from_secs(30),
        Command::new("docker")
            .args(["network", "connect", internal_network, &name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("attach ingress relay to internal workload network timed out")??;
    if !connect.status.success() {
        let _ = remove_container(runtime_id, generation).await;
        anyhow::bail!(
            "failed to attach ingress relay to internal network: {}",
            String::from_utf8_lossy(&connect.stderr).trim()
        );
    }

    if let Err(error) = wait_healthy(&name, Duration::from_secs(180)).await {
        let _ = remove_container(runtime_id, generation).await;
        return Err(error)
            .context("rootless ingress relay failed its Java backend readiness check");
    }
    Ok(())
}

fn push_publish(args: &mut Vec<String>, binding: &str) {
    args.push("--publish".to_owned());
    args.push(binding.to_owned());
}

/// Atomically recreate a runtime's published relay and host admission policy.
#[allow(clippy::too_many_arguments)]
pub async fn ensure_runtime_ingress(
    policy: &GatewayIngressPolicy,
    sandbox: &DockerSandbox,
    image: &str,
    runtime_id: Uuid,
    generation: i64,
    internal_network: &str,
    backend_container: &str,
    internal_peers: &[String],
    ports: PublishedWorkloadPorts,
) -> Result<()> {
    let operation = async {
        cleanup_ingress_through(policy, runtime_id, generation).await?;
        sandbox
            .verify_isolated_network(internal_network, internal_peers)
            .await?;
        let external_network = create_network(runtime_id, generation).await?;
        policy.install(runtime_id, generation, ports).await?;
        let backend = container_ipv4(backend_container, internal_network).await?;
        start_container(
            image,
            runtime_id,
            generation,
            backend,
            internal_network,
            &external_network,
            ports,
        )
        .await?;
        let relay = container_name(runtime_id, generation)?;
        let mut expected_peers = internal_peers.to_vec();
        expected_peers.push(relay.clone());
        sandbox
            .verify_isolated_network(internal_network, &expected_peers)
            .await?;
        verify_network(&external_network, runtime_id, generation, &[relay]).await?;
        verify_container(
            runtime_id,
            generation,
            &[internal_network.to_owned(), external_network],
            ports,
        )
        .await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(error) = operation {
        cleanup_ingress_through(policy, runtime_id, generation).await?;
        return Err(error);
    }
    Ok(())
}

async fn wait_healthy(name: &str, max_wait: Duration) -> Result<()> {
    timeout(max_wait, async {
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
            match String::from_utf8_lossy(&output.stdout).trim() {
                "healthy" => return Ok::<(), anyhow::Error>(()),
                "unhealthy" | "missing" => {
                    anyhow::bail!("ingress relay {name} is unhealthy or has no health check")
                }
                _ => sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .context("ingress relay health check did not become ready")?
}

pub async fn verify_container(
    runtime_id: Uuid,
    generation: i64,
    expected_networks: &[String],
    ports: PublishedWorkloadPorts,
) -> Result<()> {
    let name = container_name(runtime_id, generation)?;
    let output = Command::new("docker")
        .args(["inspect", "--format", "{{json .}}", &name])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await?;
    ensure!(
        output.status.success(),
        "failed to inspect ingress relay {name}"
    );
    let container: Value =
        serde_json::from_slice(&output.stdout).context("Docker returned invalid relay metadata")?;
    let state = container.pointer("/State/Running").and_then(Value::as_bool) == Some(true);
    let labels = container
        .pointer("/Config/Labels")
        .and_then(Value::as_object);
    let labels_match = labels.is_some_and(|labels| {
        labels.get("kurbis.server_id").and_then(Value::as_str) == Some(&runtime_id.to_string())
            && labels.get("kurbis.generation").and_then(Value::as_str)
                == Some(&generation.to_string())
            && labels.get("kurbis.ingress_relay").and_then(Value::as_str)
                == Some(&runtime_id.to_string())
    });
    verify_container_security(&container, runtime_id, generation, ports)?;
    let mut attached = container
        .pointer("/NetworkSettings/Networks")
        .and_then(Value::as_object)
        .map(|networks| networks.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut expected = expected_networks.to_vec();
    attached.sort();
    expected.sort();
    ensure!(
        state && labels_match && attached == expected,
        "ingress relay identity or network membership is not enforced (running={state}, labels_match={labels_match}, attached={attached:?})"
    );
    Ok(())
}

fn verify_container_security(
    container: &Value,
    runtime_id: Uuid,
    generation: i64,
    ports: PublishedWorkloadPorts,
) -> Result<()> {
    let config = container.pointer("/Config").and_then(Value::as_object);
    let host = container.pointer("/HostConfig").and_then(Value::as_object);
    ensure!(
        config.is_some_and(|value| value.get("User").and_then(Value::as_str) == Some("0:0")),
        "ingress relay must start with namespace root only to install its local firewall"
    );
    let host = host.context("ingress relay has no HostConfig")?;
    ensure!(
        host.get("ReadonlyRootfs").and_then(Value::as_bool) == Some(true),
        "ingress relay root filesystem is not read-only"
    );
    ensure!(
        host.get("Memory").and_then(Value::as_i64) == Some(268_435_456),
        "ingress relay memory limit is not 256 MiB"
    );
    ensure!(
        host.get("MemorySwap").and_then(Value::as_i64) == Some(268_435_456),
        "ingress relay swap is not disabled"
    );
    ensure!(
        host.get("NanoCpus").and_then(Value::as_i64) == Some(500_000_000),
        "ingress relay CPU limit is not 0.5 CPU"
    );
    ensure!(
        host.get("PidsLimit").and_then(Value::as_i64) == Some(64),
        "ingress relay PID limit is not 64"
    );
    ensure!(
        host.get("Init").and_then(Value::as_bool) == Some(true),
        "ingress relay has no init process to reap health-check children"
    );
    let capabilities = host
        .get("CapAdd")
        .and_then(Value::as_array)
        .context("ingress relay CapAdd is missing")?;
    let mut capabilities = capabilities
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    capabilities.sort_unstable();
    ensure!(
        capabilities == ["NET_ADMIN", "SETGID", "SETPCAP", "SETUID"],
        "ingress relay has unexpected capabilities"
    );
    let dropped = host
        .get("CapDrop")
        .and_then(Value::as_array)
        .context("ingress relay CapDrop is missing")?;
    ensure!(
        dropped.iter().any(|value| value.as_str() == Some("ALL")),
        "ingress relay does not drop all unspecified capabilities"
    );
    let security = host
        .get("SecurityOpt")
        .and_then(Value::as_array)
        .context("ingress relay SecurityOpt is missing")?;
    ensure!(
        security
            .iter()
            .any(|value| value.as_str() == Some("no-new-privileges:true")),
        "ingress relay is missing no-new-privileges"
    );
    ensure!(
        host.get("RestartPolicy")
            .and_then(|policy| policy.get("Name"))
            .and_then(Value::as_str)
            == Some("no"),
        "ingress relay must be generation-supervised, not auto-restarted"
    );

    let expected = [
        (
            "25565/tcp",
            vec![PublishedBinding::for_socket(
                ports.java_tcp,
                JAVA_PORT,
                "tcp",
            )],
        ),
        (
            "19132/udp",
            vec![PublishedBinding::for_socket(
                ports.bedrock_udp,
                BEDROCK_PORT,
                "udp",
            )],
        ),
        (
            "19132/tcp",
            vec![PublishedBinding::for_socket(
                ports.bedrock_signal_tcp,
                BEDROCK_PORT,
                "tcp",
            )],
        ),
    ];
    let actual = host
        .get("PortBindings")
        .and_then(Value::as_object)
        .context("ingress relay host port bindings are missing")?;
    ensure!(
        actual.len() == expected.len(),
        "ingress relay published unexpected container ports"
    );
    for (container_port, mut bindings) in expected {
        bindings.sort();
        let value = actual
            .get(container_port)
            .and_then(Value::as_array)
            .context("ingress relay port binding is missing")?;
        let mut observed = value
            .iter()
            .map(|binding| PublishedBinding {
                ip: binding
                    .get("HostIp")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .parse()
                    .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
                port: binding
                    .get("HostPort")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .parse()
                    .unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        observed.sort();
        ensure!(
            observed == bindings,
            "ingress relay published an unexpected host binding for {container_port}"
        );
    }
    let env = config
        .and_then(|value| value.get("Env"))
        .and_then(Value::as_array)
        .context("ingress relay environment is missing")?;
    for entry in env.iter().filter_map(Value::as_str) {
        let key = entry.split_once('=').map(|(key, _)| key).unwrap_or(entry);
        ensure!(
            matches!(key, "PATH" | "HOME" | "KURBIS_INGRESS_BACKEND_IPV4"),
            "ingress relay received an unexpected environment variable"
        );
    }
    let mounts = host
        .get("Mounts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    ensure!(
        mounts.is_empty(),
        "ingress relay must not receive host filesystem mounts"
    );
    let _ = (runtime_id, generation);
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PublishedBinding {
    ip: IpAddr,
    port: u16,
}

impl PublishedBinding {
    fn for_socket(socket: SocketAddr, _container_port: u16, _protocol: &str) -> Self {
        Self {
            ip: socket.ip(),
            port: socket.port(),
        }
    }
}

pub async fn remove_container(runtime_id: Uuid, generation: i64) -> Result<()> {
    let name = container_name(runtime_id, generation)?;
    let output = timeout(
        Duration::from_secs(30),
        Command::new("docker")
            .args(["inspect", "--format", "{{json .Config.Labels}}", &name])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("rootless ingress relay inspection timed out")??;
    if !output.status.success() {
        if String::from_utf8_lossy(&output.stderr).contains("No such object")
            || String::from_utf8_lossy(&output.stderr).contains("No such container")
        {
            return Ok(());
        }
        anyhow::bail!(
            "failed to inspect rootless ingress relay {name}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let labels: Value = serde_json::from_slice(&output.stdout)
        .context("Docker returned invalid ingress relay labels")?;
    ensure!(
        labels.get("kurbis.server_id").and_then(Value::as_str) == Some(&runtime_id.to_string())
            && labels.get("kurbis.generation").and_then(Value::as_str)
                == Some(&generation.to_string())
            && labels.get("kurbis.ingress_relay").and_then(Value::as_str)
                == Some(&runtime_id.to_string()),
        "refusing to remove ingress relay with a mismatched runtime generation"
    );
    let removed = timeout(
        Duration::from_secs(30),
        Command::new("docker")
            .args(["rm", "--force", &name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("rootless ingress relay removal timed out")??;
    ensure!(
        removed.status.success(),
        "failed to remove rootless ingress relay {name}: {}",
        String::from_utf8_lossy(&removed.stderr).trim()
    );
    Ok(())
}

async fn remove_relays_through(runtime_id: Uuid, max_generation: i64) -> Result<()> {
    ensure!(
        max_generation > 0,
        "ingress cleanup needs a positive maximum generation"
    );
    let label = format!("kurbis.ingress_relay={runtime_id}");
    let output = Command::new("docker")
        .args([
            "ps",
            "--all",
            "--quiet",
            "--filter",
            &format!("label={label}"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await?;
    ensure!(
        output.status.success(),
        "failed to enumerate ingress relays for {runtime_id}"
    );
    let expected_relay = runtime_id.to_string();
    for id in String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        let inspected = Command::new("docker")
            .args(["inspect", "--format", "{{json .Config.Labels}}", id])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;
        ensure!(
            inspected.status.success(),
            "failed to inspect ingress relay {id}"
        );
        let labels: Value = serde_json::from_slice(&inspected.stdout)
            .context("Docker returned invalid ingress relay labels")?;
        ensure!(
            labels.get("kurbis.server_id").and_then(Value::as_str) == Some(&expected_relay)
                && labels.get("kurbis.ingress_relay").and_then(Value::as_str)
                    == Some(&expected_relay),
            "refusing to remove ingress relay with mismatched ownership labels"
        );
        let generation = labels
            .get("kurbis.generation")
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<i64>().ok())
            .context("ingress relay has an invalid generation label")?;
        ensure!(
            generation > 0 && generation <= max_generation,
            "refusing stale ingress cleanup of newer/invalid relay generation {generation}"
        );
        let removed = Command::new("docker")
            .args(["rm", "--force", id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await?;
        ensure!(
            removed.status.success(),
            "failed to remove ingress relay generation {generation}: {}",
            String::from_utf8_lossy(&removed.stderr).trim()
        );
    }
    Ok(())
}

pub async fn cleanup_ingress_through(
    policy: &GatewayIngressPolicy,
    runtime_id: Uuid,
    max_generation: i64,
) -> Result<()> {
    remove_relays_through(runtime_id, max_generation).await?;
    remove_networks(runtime_id, max_generation).await?;
    policy
        .remove_generations_through(runtime_id, max_generation)
        .await?;
    Ok(())
}

pub async fn remove_networks(runtime_id: Uuid, max_generation: i64) -> Result<()> {
    ensure!(
        max_generation > 0,
        "ingress cleanup needs a positive maximum generation"
    );
    let label = format!("kurbis.server_id={runtime_id}");
    let output = Command::new("docker")
        .args([
            "network",
            "ls",
            "--filter",
            &format!("label={label}"),
            "--filter",
            "label=kurbis.network=ingress",
            "--format",
            "{{.Name}}",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await?;
    ensure!(
        output.status.success(),
        "failed to enumerate ingress networks for {runtime_id}"
    );
    let prefix = format!("kurbis-ingress-{}-g", runtime_id.simple());
    let mut networks = Vec::new();
    for name in String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
    {
        let Some(generation) = name
            .strip_prefix(&prefix)
            .and_then(|suffix| suffix.parse::<i64>().ok())
        else {
            continue;
        };
        ensure!(
            generation > 0,
            "invalid ingress network generation in {name}"
        );
        let inspected = Command::new("docker")
            .args(["network", "inspect", "--format", "{{json .}}", name])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;
        ensure!(
            inspected.status.success(),
            "failed to inspect ingress network {name}"
        );
        let metadata: Value = serde_json::from_slice(&inspected.stdout)
            .context("Docker returned invalid ingress network metadata")?;
        let labels = metadata.get("Labels").and_then(Value::as_object);
        ensure!(
            labels.is_some_and(
                |labels| labels.get("kurbis.server_id").and_then(Value::as_str)
                    == Some(&runtime_id.to_string())
                    && labels.get("kurbis.generation").and_then(Value::as_str)
                        == Some(&generation.to_string())
                    && labels.get("kurbis.network").and_then(Value::as_str) == Some("ingress")
            ),
            "refusing to remove ingress network with mismatched labels: {name}"
        );
        ensure!(
            generation <= max_generation,
            "refusing stale ingress cleanup of newer generation {generation}"
        );
        networks.push(name.to_owned());
    }
    for name in networks {
        let removed = Command::new("docker")
            .args(["network", "rm", &name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await?;
        if !removed.status.success()
            && !String::from_utf8_lossy(&removed.stderr).contains("not found")
        {
            anyhow::bail!(
                "failed to remove ingress network {name}: {}",
                String::from_utf8_lossy(&removed.stderr).trim()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_scopes_names_and_publish_bindings_are_exact() {
        let id = Uuid::from_u128(7);
        assert_eq!(
            container_name(id, 4).unwrap(),
            format!("kurbis-ingress-{}-g4", id.simple())
        );
        assert_ne!(
            container_name(id, 4).unwrap(),
            container_name(id, 5).unwrap()
        );
        let ports = PublishedWorkloadPorts {
            java_tcp: "192.0.2.10:23001".parse().unwrap(),
            bedrock_udp: "192.0.2.10:23002".parse().unwrap(),
            bedrock_signal_tcp: "192.0.2.11:23003".parse().unwrap(),
        };
        let network = serde_json::json!({
            "Config": {"User":"0:0", "Env":["KURBIS_INGRESS_BACKEND_IPV4=172.20.0.2"]},
            "HostConfig": {
                "ReadonlyRootfs":true, "Memory":268435456, "MemorySwap":268435456,
                "NanoCpus":500000000, "PidsLimit":64, "Init":true,
                "CapAdd":["NET_ADMIN","SETUID","SETGID","SETPCAP"], "CapDrop":["ALL"],
                "SecurityOpt":["no-new-privileges:true"], "RestartPolicy":{"Name":"no"},
                "PortBindings":{
                    "25565/tcp":[{"HostIp":"192.0.2.10","HostPort":"23001"}],
                    "19132/udp":[{"HostIp":"192.0.2.10","HostPort":"23002"}],
                    "19132/tcp":[{"HostIp":"192.0.2.11","HostPort":"23003"}]
                }, "Mounts":[]
            }
        });
        assert!(verify_container_security(&network, id, 4, ports).is_ok());
        let mut tampered = network.clone();
        tampered["HostConfig"]["PortBindings"]["19132/udp"][0]["HostIp"] =
            serde_json::json!("0.0.0.0");
        assert!(verify_container_security(&tampered, id, 4, ports).is_err());
        let mut without_init = network.clone();
        without_init["HostConfig"]["Init"] = serde_json::json!(false);
        assert!(verify_container_security(&without_init, id, 4, ports).is_err());
        let mut escaped = network.clone();
        escaped["HostConfig"]["PortBindings"]["19132/tcp"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"HostIp":"0.0.0.0","HostPort":"19132"}));
        assert!(verify_container_security(&escaped, id, 4, ports).is_err());
    }

    #[test]
    fn ingress_firewall_allow_sources_match_the_actual_bound_host_address() {
        let gateway: IpAddr = "192.0.2.1".parse().unwrap();
        let ports = PublishedWorkloadPorts {
            java_tcp: "192.0.2.10:23001".parse().unwrap(),
            bedrock_udp: "192.0.2.10:23002".parse().unwrap(),
            bedrock_signal_tcp: "192.0.2.11:23003".parse().unwrap(),
        };
        let bindings = crate::worker::ingress::ingress_bindings(gateway, ports);
        // Exactly the three published sockets, each open to the gateway only.
        assert_eq!(bindings.len(), 3);
        assert!(bindings.iter().all(|binding| binding.2 == gateway));
    }

    #[test]
    fn ingress_network_membership_is_per_runtime_and_requires_no_egress_or_peer_sharing() {
        let id = Uuid::from_u128(9);
        let mut valid = serde_json::json!({
            "Name": network_name(id, 3).unwrap(), "Driver":"bridge", "Internal":false, "EnableIPv6":false,
            "Options":{"com.docker.network.bridge.enable_icc":"false", "com.docker.network.bridge.enable_ip_masquerade":"false"},
            "Labels":{"kurbis.server_id":id.to_string(), "kurbis.generation":"3", "kurbis.network":"ingress"},
            "Containers":{"relay":{"Name":container_name(id, 3).unwrap()}}
        });
        assert!(verify_network_metadata(&valid, id, 3, &[container_name(id, 3).unwrap()]).is_ok());
        valid["Containers"]["rogue"] = serde_json::json!({"Name":"untrusted"});
        assert!(verify_network_metadata(&valid, id, 3, &[container_name(id, 3).unwrap()]).is_err());
        valid["Containers"].as_object_mut().unwrap().remove("rogue");
        valid["Options"]["com.docker.network.bridge.enable_ip_masquerade"] =
            serde_json::json!("true");
        assert!(verify_network_metadata(&valid, id, 3, &[container_name(id, 3).unwrap()]).is_err());
    }
}

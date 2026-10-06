use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    process::Command,
    sync::{Semaphore, mpsc},
    task::JoinSet,
    time::{Instant, sleep, timeout, timeout_at},
};

const JAVA_PORT: u16 = 25565;
const BEDROCK_PORT: u16 = 19132;
const TCP_MAX_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const UDP_SESSION_IDLE: Duration = Duration::from_secs(60);
const MAX_TCP_SESSIONS: usize = 512;
const MAX_UDP_SESSIONS: usize = 256;
const UDP_QUEUE_PACKETS: usize = 8;
const MAX_UDP_PACKET_BYTES: usize = 4096;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn health_check() -> Result<()> {
    let backend: Ipv4Addr = std::env::var("KURBIS_INGRESS_BACKEND_IPV4")?
        .parse()
        .context("ingress relay backend address is invalid")?;
    timeout(
        Duration::from_secs(3),
        java_status_probe(SocketAddr::V4(SocketAddrV4::new(backend, JAVA_PORT))),
    )
    .await
    .context("Pumpkin Java status handshake timed out")??;
    Ok(())
}

pub async fn run_from_env() -> Result<()> {
    if !cfg!(target_os = "linux") {
        anyhow::bail!("workload ingress relay requires Linux namespace firewall support");
    }
    let backend: Ipv4Addr = std::env::var("KURBIS_INGRESS_BACKEND_IPV4")?
        .parse()
        .context("ingress relay backend address is invalid")?;
    ensure!(
        !backend.is_unspecified() && !backend.is_loopback() && !backend.is_multicast(),
        "invalid ingress relay backend address"
    );
    wait_for_relay_routes(backend).await?;
    install_relay_firewall(backend).await?;
    drop_relay_privileges()?;
    verify_relay_privileges()?;

    let java = TcpListener::bind((Ipv4Addr::UNSPECIFIED, JAVA_PORT)).await?;
    let signal = TcpListener::bind((Ipv4Addr::UNSPECIFIED, BEDROCK_PORT)).await?;
    let bedrock = Arc::new(UdpSocket::bind((Ipv4Addr::UNSPECIFIED, BEDROCK_PORT)).await?);
    tracing::info!("rootless workload ingress relay ready with fixed-backend network policy");
    tokio::try_join!(
        serve_tcp(java, SocketAddr::V4(SocketAddrV4::new(backend, JAVA_PORT))),
        serve_tcp(
            signal,
            SocketAddr::V4(SocketAddrV4::new(backend, BEDROCK_PORT))
        ),
        serve_udp(
            bedrock,
            SocketAddr::V4(SocketAddrV4::new(backend, BEDROCK_PORT))
        ),
    )?;
    Ok(())
}

async fn serve_tcp(listener: TcpListener, target: SocketAddr) -> Result<()> {
    let sessions = Arc::new(Semaphore::new(MAX_TCP_SESSIONS));
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (client, _) = accepted?;
                let Ok(permit) = sessions.clone().try_acquire_owned() else { continue; };
                tasks.spawn(async move {
                    let _permit = permit;
                    let result = timeout(TCP_CONNECT_TIMEOUT, TcpStream::connect(target)).await
                        .context("fixed Pumpkin backend connection timed out")??;
                    let mut client = client;
                    let mut backend = result;
                    client.set_nodelay(true)?;
                    backend.set_nodelay(true)?;
                    let started = Instant::now();
                    let _ = timeout(TCP_MAX_LIFETIME.saturating_sub(started.elapsed()), tokio::io::copy_bidirectional(&mut client, &mut backend)).await;
                    Ok::<(), anyhow::Error>(())
                });
            }
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(error) = result { tracing::debug!(?error, "ingress relay TCP session failed"); }
            }
        }
    }
}

async fn serve_udp(listener: Arc<UdpSocket>, target: SocketAddr) -> Result<()> {
    let mut sessions = HashMap::<SocketAddr, UdpSession>::new();
    let mut tasks = JoinSet::new();
    let mut next_id = 0_u64;
    let mut sweep = tokio::time::interval(Duration::from_secs(5));
    let mut packet = vec![0_u8; MAX_UDP_PACKET_BYTES + 1];
    loop {
        tokio::select! {
            result = listener.recv_from(&mut packet) => {
                let (length, peer) = result?;
                if length == 0 || length > MAX_UDP_PACKET_BYTES { continue; }
                if sessions.get(&peer).is_some_and(|session| session.sender.is_closed()) {
                    sessions.remove(&peer);
                }
                if !sessions.contains_key(&peer) {
                    if sessions.len() >= MAX_UDP_SESSIONS { continue; }
                    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
                    socket.connect(target).await?;
                    next_id = next_id.wrapping_add(1);
                    let id = next_id;
                    let (sender, receiver) = mpsc::channel::<Bytes>(UDP_QUEUE_PACKETS);
                    let outgoing = listener.clone();
                    tasks.spawn(async move {
                        udp_session(socket, outgoing, peer, receiver).await;
                        (peer, id)
                    });
                    sessions.insert(peer, UdpSession { id, sender, last_input: Instant::now() });
                }
                let session = sessions.get_mut(&peer).expect("UDP session inserted");
                session.last_input = Instant::now();
                let _ = session.sender.try_send(Bytes::copy_from_slice(&packet[..length]));
            }
            Some(completed) = tasks.join_next(), if !tasks.is_empty() => {
                if let Ok((peer, id)) = completed
                    && sessions.get(&peer).is_some_and(|session| session.id == id) {
                    sessions.remove(&peer);
                }
            }
            _ = sweep.tick() => {
                sessions.retain(|_, session| session.last_input.elapsed() < UDP_SESSION_IDLE && !session.sender.is_closed());
            }
        }
    }
}

struct UdpSession {
    id: u64,
    sender: mpsc::Sender<Bytes>,
    last_input: Instant,
}

async fn udp_session(
    backend: UdpSocket,
    public: Arc<UdpSocket>,
    peer: SocketAddr,
    mut incoming: mpsc::Receiver<Bytes>,
) {
    let mut response = vec![0u8; MAX_UDP_PACKET_BYTES + 1];
    let mut deadline = Instant::now() + UDP_SESSION_IDLE;
    loop {
        tokio::select! {
            _ = sleep(deadline.saturating_duration_since(Instant::now())) => return,
            packet = incoming.recv() => {
                let Some(packet) = packet else { return; };
                if timeout_at(deadline, backend.send(&packet)).await.is_err() { return; }
                deadline = Instant::now() + UDP_SESSION_IDLE;
            }
            received = backend.recv(&mut response) => {
                let Ok(length) = received else { return; };
                if length == 0 || length > MAX_UDP_PACKET_BYTES { continue; }
                if timeout_at(deadline, public.send_to(&response[..length], peer)).await.is_err() { return; }
                deadline = Instant::now() + UDP_SESSION_IDLE;
            }
        }
    }
}

async fn java_status_probe(address: SocketAddr) -> Result<()> {
    let mut stream = timeout(Duration::from_secs(2), TcpStream::connect(address)).await??;
    let host = "kurbis-ingress-health";
    let mut handshake = Vec::new();
    write_varint(0, &mut handshake);
    write_varint(777, &mut handshake);
    write_varint(host.len() as i32, &mut handshake);
    handshake.extend_from_slice(host.as_bytes());
    handshake.extend_from_slice(&JAVA_PORT.to_be_bytes());
    write_varint(1, &mut handshake);
    let mut frame = Vec::new();
    write_varint(handshake.len() as i32, &mut frame);
    frame.extend(handshake);
    stream.write_all(&frame).await?;
    let mut request = Vec::new();
    write_varint(0, &mut request);
    let mut frame = Vec::new();
    write_varint(request.len() as i32, &mut frame);
    frame.extend(request);
    stream.write_all(&frame).await?;
    let size = read_varint(&mut stream).await?;
    ensure!(
        (1..=65_536).contains(&size),
        "invalid Minecraft status frame"
    );
    let mut response = vec![0; size as usize];
    stream.read_exact(&mut response).await?;
    let mut offset = 0;
    ensure!(
        read_varint_bytes(&response, &mut offset)? == 0,
        "unexpected Minecraft status packet"
    );
    let length = read_varint_bytes(&response, &mut offset)?;
    ensure!(
        length > 0 && (length as usize) <= response.len().saturating_sub(offset),
        "invalid Minecraft status JSON"
    );
    let status: Value = serde_json::from_slice(&response[offset..offset + length as usize])?;
    ensure!(
        status.get("version").is_some() && status.get("players").is_some(),
        "Minecraft status response is incomplete"
    );
    Ok(())
}

async fn read_varint(stream: &mut TcpStream) -> Result<i32> {
    let mut result = 0i32;
    for shift in (0..35).step_by(7) {
        let byte = stream.read_u8().await?;
        result |= i32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
    }
    anyhow::bail!("Minecraft VarInt is too long")
}

fn read_varint_bytes(bytes: &[u8], offset: &mut usize) -> Result<i32> {
    let mut result = 0i32;
    for shift in (0..35).step_by(7) {
        let byte = *bytes.get(*offset).context("truncated Minecraft VarInt")?;
        *offset += 1;
        result |= i32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
    }
    anyhow::bail!("Minecraft VarInt is too long")
}

fn write_varint(mut value: i32, bytes: &mut Vec<u8>) {
    loop {
        if value & !0x7f == 0 {
            bytes.push(value as u8);
            return;
        }
        bytes.push(((value & 0x7f) | 0x80) as u8);
        value = ((value as u32) >> 7) as i32;
    }
}

async fn wait_for_relay_routes(backend: Ipv4Addr) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match relay_interfaces(backend) {
            Ok((public_interface, _backend_interface)) => {
                ensure!(
                    public_interface == "eth0",
                    "default public ingress route is not attached to eth0"
                );
                return Ok(());
            }
            Err(error) if Instant::now() < deadline => {
                tracing::debug!(
                    ?error,
                    "waiting for rootless relay's isolated backend interface"
                );
                sleep(Duration::from_millis(100)).await;
            }
            Err(error) => {
                return Err(error).context("verify rootless ingress relay network interfaces");
            }
        }
    }
}

fn relay_interfaces(backend: Ipv4Addr) -> Result<(String, String)> {
    let routes = std::fs::read_to_string("/proc/net/route").context("read relay IPv4 routes")?;
    relay_interfaces_from_route_table(&routes, backend)
}

fn relay_interfaces_from_route_table(routes: &str, backend: Ipv4Addr) -> Result<(String, String)> {
    let mut records = Vec::new();
    for line in routes.lines().skip(1) {
        let columns = line.split_whitespace().collect::<Vec<_>>();
        if columns.len() < 8 {
            continue;
        }
        let decode = |raw: &str| -> Result<u32> {
            let value = u32::from_str_radix(raw, 16).context("invalid proc route address")?;
            // /proc/net/route renders IPv4 words in native byte order. Convert
            // the parsed word back to network order before comparing addresses.
            Ok(u32::from_be(value))
        };
        records.push((
            columns[0].to_owned(),
            decode(columns[1])?,
            decode(columns[7])?,
        ));
    }
    let best = |address: Ipv4Addr| -> Option<String> {
        let destination = u32::from_be_bytes(address.octets());
        records
            .iter()
            .filter(|(_, network, mask)| destination & mask == *network)
            .max_by_key(|(_, _, mask)| mask.count_ones())
            .map(|(interface, _, _)| interface.clone())
    };
    let public_interface = records
        .iter()
        .find(|(_, network, mask)| *network == 0 && *mask == 0)
        .map(|(interface, _, _)| interface.clone())
        .context("no default route for the published ingress interface")?;
    let backend_interface =
        best(backend).context("no route for Pumpkin's fixed internal IPv4 endpoint")?;
    ensure!(
        backend_interface != public_interface,
        "Pumpkin backend did not route over its isolated secondary interface (public={public_interface}, backend={backend_interface})"
    );
    Ok((public_interface, backend_interface))
}

async fn install_relay_firewall(backend: Ipv4Addr) -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "ingress relay namespace firewall requires container root"
    );
    let backend = backend.to_string();
    // Start fail-closed for both directions before flushing or appending rules.
    // Docker connects the isolated public bridge first, so its published-host
    // proxy reaches eth0; the later internal workload network is eth1 and is
    // intentionally never permitted to open ingress-relay listeners.
    for program in ["/usr/sbin/iptables", "/usr/sbin/ip6tables"] {
        run_firewall(program, &["-w", "-P", "INPUT", "DROP"]).await?;
        run_firewall(program, &["-w", "-P", "OUTPUT", "DROP"]).await?;
        run_firewall(program, &["-w", "-F", "INPUT"]).await?;
        run_firewall(program, &["-w", "-F", "OUTPUT"]).await?;
        let ipv6 = program.ends_with("ip6tables");
        if ipv6 {
            verify_firewall_chain(program, "INPUT", 0).await?;
            verify_firewall_chain(program, "OUTPUT", 0).await?;
            continue;
        }
        run_firewall(program, &["-w", "-A", "INPUT", "-i", "lo", "-j", "ACCEPT"]).await?;
        run_firewall(
            program,
            &[
                "-w",
                "-A",
                "INPUT",
                "-m",
                "conntrack",
                "--ctstate",
                "ESTABLISHED,RELATED",
                "-j",
                "ACCEPT",
            ],
        )
        .await?;
        for (protocol, port) in [
            ("tcp", JAVA_PORT),
            ("udp", BEDROCK_PORT),
            ("tcp", BEDROCK_PORT),
        ] {
            let port = port.to_string();
            run_firewall(
                program,
                &[
                    "-w",
                    "-A",
                    "INPUT",
                    "-i",
                    "eth0",
                    "-p",
                    protocol,
                    "--dport",
                    &port,
                    "-m",
                    "conntrack",
                    "--ctstate",
                    "NEW",
                    "-j",
                    "ACCEPT",
                ],
            )
            .await?;
        }
        run_firewall(
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
        for (protocol, port) in [
            ("tcp", JAVA_PORT),
            ("udp", BEDROCK_PORT),
            ("tcp", BEDROCK_PORT),
        ] {
            let port = port.to_string();
            run_firewall(
                program,
                &[
                    "-w", "-A", "OUTPUT", "-d", &backend, "-p", protocol, "--dport", &port, "-j",
                    "ACCEPT",
                ],
            )
            .await?;
        }
        verify_firewall_chain(program, "INPUT", 5).await?;
        verify_firewall_chain(program, "OUTPUT", 4).await?;
        verify_output_allowlist(program, &backend).await?;
    }
    Ok(())
}

async fn verify_firewall_chain(program: &str, chain: &str, expected_rules: usize) -> Result<()> {
    let output = timeout(
        COMMAND_TIMEOUT,
        Command::new(program)
            .args(["-S", chain])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    ensure!(
        output.status.success(),
        "cannot inspect {program} {chain} policy"
    );
    let rules = String::from_utf8_lossy(&output.stdout);
    ensure!(
        rules.lines().any(|line| line == format!("-P {chain} DROP"))
            && rules
                .lines()
                .filter(|line| line.starts_with(&format!("-A {chain} ")))
                .count()
                == expected_rules,
        "{program} {chain} policy does not match fixed ingress-relay firewall rules"
    );
    if chain == "INPUT" && program.ends_with("iptables") {
        ensure!(
            rules
                .lines()
                .filter(|line| line.starts_with("-A INPUT -i eth0 -p "))
                .count()
                == 3
                && !rules
                    .lines()
                    .any(|line| line.starts_with("-A INPUT -i eth1 ")),
            "ingress relay is not restricted to published-port ingress on its public bridge"
        );
    }
    Ok(())
}

async fn verify_output_allowlist(program: &str, backend: &str) -> Result<()> {
    let output = timeout(
        COMMAND_TIMEOUT,
        Command::new(program)
            .args(["-S", "OUTPUT"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    ensure!(
        output.status.success(),
        "cannot inspect ingress relay OUTPUT allowlist"
    );
    let rules = String::from_utf8_lossy(&output.stdout);
    let observed = rules
        .lines()
        .filter(|line| line.starts_with("-A OUTPUT "))
        .map(str::to_owned)
        .collect::<std::collections::HashSet<_>>();
    let expected = if program.ends_with("ip6tables") {
        std::collections::HashSet::from([
            "-A OUTPUT -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT".to_owned(),
        ])
    } else {
        std::collections::HashSet::from([
            "-A OUTPUT -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT".to_owned(),
            format!("-A OUTPUT -d {backend}/32 -p tcp -m tcp --dport {JAVA_PORT} -j ACCEPT"),
            format!("-A OUTPUT -d {backend}/32 -p udp -m udp --dport {BEDROCK_PORT} -j ACCEPT"),
            format!("-A OUTPUT -d {backend}/32 -p tcp -m tcp --dport {BEDROCK_PORT} -j ACCEPT"),
        ])
    };
    ensure!(
        observed == expected,
        "ingress relay OUTPUT policy permits an unapproved egress destination or port"
    );
    Ok(())
}

async fn run_firewall(program: &str, args: &[&str]) -> Result<()> {
    let output = timeout(
        COMMAND_TIMEOUT,
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    ensure!(
        output.status.success(),
        "{program} {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

fn verify_relay_privileges() -> Result<()> {
    use anyhow::ensure;
    let status = std::fs::read_to_string("/proc/self/status")?;
    let uid = status
        .lines()
        .find_map(|line| {
            line.strip_prefix("Uid:")
                .and_then(|value| value.split_whitespace().next())
        })
        .context("relay process status has no Uid")?
        .parse::<u32>()?;
    let effective_caps = status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:").map(str::trim))
        .context("relay process status has no CapEff")?;
    let no_new_privileges = status
        .lines()
        .find_map(|line| line.strip_prefix("NoNewPrivs:").map(str::trim));
    ensure!(uid == 65_532, "ingress relay is not running as uid 65532");
    ensure!(
        effective_caps == "0000000000000000",
        "ingress relay retained effective capabilities"
    );
    ensure!(
        no_new_privileges == Some("1"),
        "ingress relay lost no-new-privileges"
    );
    Ok(())
}

fn drop_relay_privileges() -> Result<()> {
    use anyhow::ensure;
    unsafe {
        ensure!(
            libc::geteuid() == 0,
            "ingress relay privilege drop requires namespace root"
        );
        ensure!(
            libc::setgroups(0, std::ptr::null()) == 0,
            "failed to clear ingress relay supplementary groups"
        );
        ensure!(
            libc::setgid(65_532) == 0,
            "failed to drop ingress relay group"
        );
        ensure!(
            libc::setuid(65_532) == 0,
            "failed to drop ingress relay user"
        );
        ensure!(
            libc::geteuid() == 65_532 && libc::getegid() == 65_532,
            "ingress relay did not drop root privileges"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOTLESS_ROUTES: &str = r#"Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT
eth0 00000000 0100000A 0003 0 0 0 00000000 0 0 0
eth0 0000000A 00000000 0001 0 0 0 00FFFFFF 0 0 0
eth1 000014AC 00000000 0001 0 0 0 0000FFFF 0 0 0
"#;

    #[test]
    fn public_default_route_and_backend_route_must_be_separate() {
        assert_eq!(
            relay_interfaces_from_route_table(ROOTLESS_ROUTES, "172.20.0.5".parse().unwrap())
                .unwrap(),
            ("eth0".to_owned(), "eth1".to_owned())
        );
        assert!(
            relay_interfaces_from_route_table(ROOTLESS_ROUTES, "10.0.0.10".parse().unwrap())
                .is_err()
        );
        assert!(
            relay_interfaces_from_route_table(
                "Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT\n",
                "172.20.0.5".parse().unwrap()
            )
            .is_err()
        );
    }
}

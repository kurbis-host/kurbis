use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::{Instant, sleep},
};

use crate::{
    config::GatewayConfig,
    gateway::{
        route_wait::{RouteLease, RouteProtocol},
        routes::RouteCache,
    },
    model::{ObservedState, RouteTarget},
};

const MAX_HANDSHAKE_FRAME: usize = 8 * 1024;
const ROUTE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
const CLIENT_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1);

#[derive(Debug)]
struct Handshake {
    protocol: i32,
    hostname: String,
    next_state: i32,
    framed: Vec<u8>,
}

pub async fn run(config: GatewayConfig, routes: RouteCache) -> Result<()> {
    let listener = TcpListener::bind(config.java_listen).await?;
    tracing::info!(listen = %config.java_listen, "Java gateway listening");
    let config = Arc::new(config);
    loop {
        let (stream, peer) = listener.accept().await?;
        let routes = routes.clone();
        let config = config.clone();
        tokio::spawn(async move {
            if let Err(error) = handle(stream, &config, &routes).await {
                tracing::debug!(%peer, ?error, "Java gateway connection ended");
            }
        });
    }
}

async fn handle(mut client: TcpStream, config: &GatewayConfig, routes: &RouteCache) -> Result<()> {
    let handshake = match read_handshake(&mut client).await {
        Ok(handshake) => handshake,
        Err(error) => {
            tracing::debug!(?error, "malformed Java handshake");
            return Ok(());
        }
    };
    match handshake.next_state {
        1 => {
            let accepted_server_address = routes.accepts_server_address(&handshake.hostname);
            let target = routes.resolve(&handshake.hostname).await?;
            handle_status(
                &mut client,
                &handshake,
                accepted_server_address,
                target.as_ref(),
            )
            .await
        }
        2 => {
            if !routes.accepts_server_address(&handshake.hostname) {
                return send_login_disconnect(&mut client, "Unknown server address").await;
            }
            handle_login(client, config, routes, handshake).await
        }
        _ => Ok(()),
    }
}

async fn handle_login(
    client: TcpStream,
    config: &GatewayConfig,
    routes: &RouteCache,
    handshake: Handshake,
) -> Result<()> {
    let target = routes.resolve(&handshake.hostname).await?;
    let Some(target) = target else {
        // An accepted but unregistered hostname still needs the waiting
        // runtime: it authenticates the player and performs admission/account
        // creation before the first server can be selected.
        return proxy_to_waiting(client, config, handshake).await;
    };

    if target.account_suspended {
        let mut client = client;
        return send_login_disconnect(
            &mut client,
            "Server suspended. Contact the hosting operator.",
        )
        .await;
    }

    if java_lease(&target).is_some() {
        let current = routes.resolve_current(&handshake.hostname).await?;
        let Some(current) = current else {
            let mut client = client;
            return send_login_disconnect(&mut client, "Unknown or deleted server address.").await;
        };
        if let Some(lease) = java_lease(&current) {
            return proxy_to_backend(client, handshake, lease).await;
        }
        return wait_for_backend(client, config, routes, handshake, current).await;
    }

    if target.state == ObservedState::Failed {
        let mut client = client;
        return send_login_disconnect(
            &mut client,
            "Server failed to start. Retry it from the panel.",
        )
        .await;
    }

    if target.state == ObservedState::Online || target.worker_healthy == Some(false) {
        let mut client = client;
        return send_login_disconnect(
            &mut client,
            "Server is temporarily unavailable. Please reconnect shortly.",
        )
        .await;
    }

    wait_for_backend(client, config, routes, handshake, target).await
}

async fn proxy_to_waiting(
    mut client: TcpStream,
    config: &GatewayConfig,
    handshake: Handshake,
) -> Result<()> {
    let mut waiting = match connect_backend(config.waiting_java_address).await {
        Ok(waiting) => waiting,
        Err(error) => {
            tracing::warn!(?error, "waiting server is unreachable");
            return send_login_disconnect(
                &mut client,
                "Waiting server is temporarily unavailable. Please reconnect shortly.",
            )
            .await;
        }
    };
    waiting.write_all(&handshake.framed).await?;
    let _ = tokio::io::copy_bidirectional(&mut client, &mut waiting).await?;
    Ok(())
}

async fn proxy_to_backend(
    mut client: TcpStream,
    handshake: Handshake,
    lease: RouteLease,
) -> Result<()> {
    let backend_address = lease.socket_addr();
    let mut backend = match connect_backend(backend_address).await {
        Ok(backend) => backend,
        Err(error) => {
            tracing::debug!(server_id = %lease.server_id, generation = lease.generation,
                backend = %backend_address, ?error, "Java backend is unreachable");
            return send_login_disconnect(
                &mut client,
                "Server is temporarily unavailable. Please reconnect shortly.",
            )
            .await;
        }
    };
    backend.write_all(&handshake.framed).await?;
    let _ = tokio::io::copy_bidirectional(&mut client, &mut backend).await?;
    Ok(())
}

async fn wait_for_backend(
    mut client: TcpStream,
    config: &GatewayConfig,
    routes: &RouteCache,
    handshake: Handshake,
    mut target: RouteTarget,
) -> Result<()> {
    let deadline = Instant::now() + config.wake_timeout;
    let mut wake_requested = false;

    loop {
        if target.account_suspended {
            return send_login_disconnect(
                &mut client,
                "Server suspended. Contact the hosting operator.",
            )
            .await;
        }
        if target.state == ObservedState::Failed {
            return send_login_disconnect(
                &mut client,
                "Server failed to start. Retry it from the panel.",
            )
            .await;
        }
        if java_lease(&target).is_some() {
            target = match routes.resolve_current(&handshake.hostname).await? {
                Some(current) => current,
                None => {
                    return send_login_disconnect(
                        &mut client,
                        "Unknown or deleted server address.",
                    )
                    .await;
                }
            };
        }
        if let Some(lease) = java_lease(&target) {
            return proxy_to_backend(client, handshake, lease).await;
        }
        if target.state == ObservedState::Online || target.worker_healthy == Some(false) {
            return send_login_disconnect(
                &mut client,
                "Server is temporarily unavailable. Please reconnect shortly.",
            )
            .await;
        }

        if !wake_requested && wake_allowed(target.state) {
            if let Err(error) = crate::gateway::wake::request_wake(config, target.server_id).await {
                tracing::warn!(server_id = %target.server_id, ?error, "failed to wake Java route");
                return send_login_disconnect(
                    &mut client,
                    "Server could not be started. Please reconnect shortly.",
                )
                .await;
            }
            wake_requested = true;
        }

        let now = Instant::now();
        if now >= deadline {
            return send_login_disconnect(
                &mut client,
                "Server is taking too long to start. Please reconnect shortly.",
            )
            .await;
        }
        sleep((deadline - now).min(ROUTE_POLL_INTERVAL)).await;
        if client_disconnected(&client).await {
            return Ok(());
        }

        target = match routes.resolve(&handshake.hostname).await? {
            Some(target) => target,
            None => {
                return send_login_disconnect(&mut client, "Unknown or deleted server address.")
                    .await;
            }
        };
    }
}

fn java_lease(target: &RouteTarget) -> Option<RouteLease> {
    RouteLease::from_target(target, RouteProtocol::Java)
}

fn wake_allowed(state: ObservedState) -> bool {
    matches!(
        state,
        ObservedState::Sleeping
            | ObservedState::Queued
            | ObservedState::Restoring
            | ObservedState::Starting
            | ObservedState::Stopping
            | ObservedState::Snapshotting
    )
}

async fn client_disconnected(client: &TcpStream) -> bool {
    let mut probe = [0_u8; 1];
    match tokio::time::timeout(CLIENT_PROBE_TIMEOUT, client.peek(&mut probe)).await {
        Ok(Ok(0)) | Ok(Err(_)) => true,
        Ok(Ok(_)) | Err(_) => false,
    }
}

async fn connect_backend(address: impl tokio::net::ToSocketAddrs) -> Result<TcpStream> {
    Ok(tokio::time::timeout(
        std::time::Duration::from_secs(5),
        TcpStream::connect(address),
    )
    .await
    .context("backend connection timed out")??)
}

fn status_info(
    accepted_server_address: bool,
    target: Option<&RouteTarget>,
) -> (&'static str, String) {
    match target {
        None if accepted_server_address => ("ready", "Join to create your server".to_owned()),
        None => ("unknown", "Unknown server address".to_owned()),
        Some(target) if target.account_suspended => ("suspended", "Server suspended".to_owned()),
        Some(target) => match target.state {
            ObservedState::Online if target.worker_address.is_some() => {
                ("online", "Online".to_owned())
            }
            ObservedState::Online => ("unavailable", "Worker unavailable".to_owned()),
            ObservedState::Queued => ("queued", "Queued for capacity".to_owned()),
            ObservedState::Restoring | ObservedState::Starting
                if target.worker_healthy == Some(false) =>
            {
                ("unavailable", "Assigned worker unavailable".to_owned())
            }
            ObservedState::Restoring | ObservedState::Starting => {
                ("starting", "Starting server".to_owned())
            }
            ObservedState::Failed => ("failed", "Server failed to start".to_owned()),
            ObservedState::Resetting => ("starting", "Resetting server".to_owned()),
            state => ("sleeping", format!("{state:?}")),
        },
    }
}

async fn handle_status(
    client: &mut TcpStream,
    handshake: &Handshake,
    accepted_server_address: bool,
    target: Option<&RouteTarget>,
) -> Result<()> {
    let request = read_frame(client, 1024).await?;
    let mut cursor = 0;
    let packet_id = read_varint(&request, &mut cursor)?;
    if packet_id != 0 {
        bail!("unexpected status request packet id {packet_id}");
    }
    let (state, description) = status_info(accepted_server_address, target);
    let status = json!({
        "version": {"name": format!("kurbis ({state})"), "protocol": handshake.protocol},
        "players": {"max": 0, "online": 0, "sample": []},
        "description": {"text": description}
    });
    let mut packet = Vec::new();
    write_varint(0, &mut packet);
    write_string(&status.to_string(), &mut packet)?;
    write_frame(client, &packet).await?;

    if let Ok(ping) = read_frame(client, 1024).await {
        let mut cursor = 0;
        if read_varint(&ping, &mut cursor).ok() == Some(1) && ping.len().saturating_sub(cursor) == 8
        {
            write_frame(client, &ping).await?;
        }
    }
    Ok(())
}

async fn send_login_disconnect(client: &mut TcpStream, message: &str) -> Result<()> {
    let mut packet = Vec::new();
    write_varint(0, &mut packet);
    write_string(&json!({"text": message}).to_string(), &mut packet)?;
    write_frame(client, &packet).await
}

async fn read_handshake(stream: &mut TcpStream) -> Result<Handshake> {
    let body = read_frame(stream, MAX_HANDSHAKE_FRAME).await?;
    let mut cursor = 0;
    if read_varint(&body, &mut cursor)? != 0 {
        bail!("first packet is not a handshake");
    }
    let protocol = read_varint(&body, &mut cursor)?;
    let hostname = read_string(&body, &mut cursor, 255)?;
    if hostname.contains('\0') {
        bail!("forwarded or NUL-delimited hostnames are not accepted");
    }
    if cursor + 2 > body.len() {
        bail!("handshake omitted server port");
    }
    cursor += 2;
    let next_state = read_varint(&body, &mut cursor)?;
    if cursor != body.len() {
        bail!("handshake has trailing bytes");
    }
    let mut framed = Vec::new();
    write_varint(i32::try_from(body.len())?, &mut framed);
    framed.extend_from_slice(&body);
    Ok(Handshake {
        protocol,
        hostname,
        next_state,
        framed,
    })
}

async fn read_frame(stream: &mut TcpStream, max: usize) -> Result<Vec<u8>> {
    let length = read_varint_stream(stream).await?;
    let length = usize::try_from(length).context("negative packet length")?;
    if length > max {
        bail!("packet frame exceeds limit");
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await?;
    Ok(body)
}

async fn write_frame(stream: &mut TcpStream, body: &[u8]) -> Result<()> {
    let mut frame = Vec::with_capacity(body.len() + 5);
    write_varint(i32::try_from(body.len())?, &mut frame);
    frame.extend_from_slice(body);
    stream.write_all(&frame).await?;
    Ok(())
}

async fn read_varint_stream(stream: &mut TcpStream) -> Result<i32> {
    let mut value = 0_u32;
    for index in 0..5 {
        let byte = stream.read_u8().await?;
        value |= u32::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            return Ok(value as i32);
        }
    }
    bail!("VarInt exceeds five bytes")
}

fn read_varint(bytes: &[u8], cursor: &mut usize) -> Result<i32> {
    let mut value = 0_u32;
    for index in 0..5 {
        let byte = *bytes.get(*cursor).context("truncated VarInt")?;
        *cursor += 1;
        value |= u32::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            return Ok(value as i32);
        }
    }
    bail!("VarInt exceeds five bytes")
}

fn write_varint(value: i32, output: &mut Vec<u8>) {
    let mut value = value as u32;
    loop {
        if value & !0x7f == 0 {
            output.push(value as u8);
            return;
        }
        output.push(((value & 0x7f) | 0x80) as u8);
        value >>= 7;
    }
}

fn read_string(bytes: &[u8], cursor: &mut usize, max_chars: usize) -> Result<String> {
    let byte_len =
        usize::try_from(read_varint(bytes, cursor)?).context("negative string length")?;
    if byte_len > max_chars * 4 || cursor.saturating_add(byte_len) > bytes.len() {
        bail!("invalid string length");
    }
    let value = std::str::from_utf8(&bytes[*cursor..*cursor + byte_len])?.to_owned();
    if value.chars().count() > max_chars {
        bail!("string exceeds character limit");
    }
    *cursor += byte_len;
    Ok(value)
}

fn write_string(value: &str, output: &mut Vec<u8>) -> Result<()> {
    write_varint(i32::try_from(value.len())?, output);
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_unregistered_address_has_creation_motd() {
        let (state, description) = status_info(true, None);
        assert_eq!(state, "ready");
        assert_eq!(description, "Join to create your server");
    }

    #[test]
    fn invalid_address_keeps_unknown_motd() {
        let (state, description) = status_info(false, None);
        assert_eq!(state, "unknown");
        assert_eq!(description, "Unknown server address");
    }

    #[tokio::test]
    async fn login_is_authenticated_by_the_waiting_runtime_before_transfer() {
        let waiting_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = GatewayConfig {
            common: crate::config::CommonConfig {
                database_url: String::new(),
            },
            java_listen: "127.0.0.1:0".parse().unwrap(),
            bedrock_listen: "127.0.0.1:0".parse().unwrap(),
            bedrock_public_port: 19132,
            bedrock_tls_certificate: "/unused/test-cert.pem".into(),
            bedrock_tls_private_key: "/unused/test-key.pem".into(),
            waiting_java_address: waiting_listener.local_addr().unwrap(),
            waiting_bedrock_address: "127.0.0.1:0".parse().unwrap(),
            waiting_signal_address: "127.0.0.1:19134".parse().unwrap(),
            waiting_control_socket: "/nonexistent/control.sock".into(),
            waiting_control_token: "test".into(),
            control_plane_url: "http://127.0.0.1:0".into(),
            internal_token: "test".into(),
            root_domain: "example.test".into(),
            wake_timeout: std::time::Duration::from_secs(90),
            allow_offline_waiting_for_tests: false,
        };
        let public_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(public_listener.local_addr().unwrap());
        let (accepted, client) = tokio::join!(public_listener.accept(), client);
        let (accepted, _) = accepted.unwrap();
        let mut client = client.unwrap();

        let mut handshake_body = Vec::new();
        write_varint(0, &mut handshake_body);
        write_varint(777, &mut handshake_body);
        write_string("friends.example.test", &mut handshake_body).unwrap();
        handshake_body.extend_from_slice(&25565_u16.to_be_bytes());
        write_varint(2, &mut handshake_body);
        let mut framed = Vec::new();
        write_varint(handshake_body.len() as i32, &mut framed);
        framed.extend_from_slice(&handshake_body);
        let expected_handshake = framed.clone();
        let handshake = Handshake {
            protocol: 777,
            hostname: "friends.example.test".into(),
            next_state: 2,
            framed,
        };

        let waiting = tokio::spawn(async move {
            let (mut backend, _) = waiting_listener.accept().await.unwrap();
            let received = read_frame(&mut backend, MAX_HANDSHAKE_FRAME).await.unwrap();
            let mut received_framed = Vec::new();
            write_varint(received.len() as i32, &mut received_framed);
            received_framed.extend_from_slice(&received);
            assert_eq!(received_framed, expected_handshake);
            send_login_disconnect(&mut backend, "waiting fixture response")
                .await
                .unwrap();
            backend.shutdown().await.unwrap();
        });
        client.shutdown().await.unwrap();
        proxy_to_waiting(accepted, &config, handshake)
            .await
            .unwrap();
        waiting.await.unwrap();

        let packet = read_frame(&mut client, 1024).await.unwrap();
        let mut cursor = 0;
        assert_eq!(read_varint(&packet, &mut cursor).unwrap(), 0);
        let message: serde_json::Value =
            serde_json::from_str(&read_string(&packet, &mut cursor, 255).unwrap()).unwrap();
        assert_eq!(message["text"], "waiting fixture response");
        assert_eq!(cursor, packet.len());
    }

    #[tokio::test]
    async fn unavailable_waiting_runtime_returns_a_protocol_disconnect() {
        let unavailable = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = unavailable.local_addr().unwrap();
        drop(unavailable);
        let config = GatewayConfig {
            common: crate::config::CommonConfig {
                database_url: String::new(),
            },
            java_listen: "127.0.0.1:0".parse().unwrap(),
            bedrock_listen: "127.0.0.1:0".parse().unwrap(),
            bedrock_public_port: 19132,
            bedrock_tls_certificate: "/unused/test-cert.pem".into(),
            bedrock_tls_private_key: "/unused/test-key.pem".into(),
            waiting_java_address: address,
            waiting_bedrock_address: "127.0.0.1:0".parse().unwrap(),
            waiting_signal_address: "127.0.0.1:19134".parse().unwrap(),
            waiting_control_socket: "/nonexistent/control.sock".into(),
            waiting_control_token: "test".into(),
            control_plane_url: "http://127.0.0.1:0".into(),
            internal_token: "test".into(),
            root_domain: "example.test".into(),
            wake_timeout: std::time::Duration::from_secs(90),
            allow_offline_waiting_for_tests: false,
        };
        let public_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(public_listener.local_addr().unwrap());
        let (accepted, client) = tokio::join!(public_listener.accept(), client);
        let (accepted, _) = accepted.unwrap();
        let mut client = client.unwrap();
        let handshake = Handshake {
            protocol: 777,
            hostname: "friends.example.test".into(),
            next_state: 2,
            framed: Vec::new(),
        };

        proxy_to_waiting(accepted, &config, handshake)
            .await
            .unwrap();

        let packet = read_frame(&mut client, 1024).await.unwrap();
        let mut cursor = 0;
        assert_eq!(read_varint(&packet, &mut cursor).unwrap(), 0);
        let message: serde_json::Value =
            serde_json::from_str(&read_string(&packet, &mut cursor, 255).unwrap()).unwrap();
        assert_eq!(
            message["text"],
            "Waiting server is temporarily unavailable. Please reconnect shortly."
        );
    }

    fn route_target(state: ObservedState) -> RouteTarget {
        RouteTarget {
            server_id: uuid::Uuid::new_v4(),
            subdomain: "friends".into(),
            state,
            account_suspended: false,
            generation: 1,
            worker_healthy: Some(true),
            worker_valid_until: None,
            worker_address: Some("127.0.0.1".into()),
            java_port: Some(25565),
            bedrock_port: Some(19132),
        }
    }

    #[test]
    fn only_healthy_online_routes_bypass_waiting() {
        assert_eq!(
            java_lease(&route_target(ObservedState::Online)).map(|lease| lease.socket_addr()),
            Some("127.0.0.1:25565".parse().unwrap())
        );
        assert!(java_lease(&route_target(ObservedState::Starting)).is_none());

        let mut unhealthy = route_target(ObservedState::Online);
        unhealthy.worker_healthy = Some(false);
        assert!(java_lease(&unhealthy).is_none());

        let mut suspended = route_target(ObservedState::Online);
        suspended.account_suspended = true;
        assert!(java_lease(&suspended).is_none());
    }

    #[test]
    fn startup_states_request_one_idempotent_wake() {
        for state in [
            ObservedState::Sleeping,
            ObservedState::Queued,
            ObservedState::Restoring,
            ObservedState::Starting,
            ObservedState::Stopping,
            ObservedState::Snapshotting,
        ] {
            assert!(wake_allowed(state), "{state:?} should request a wake");
        }
        assert!(!wake_allowed(ObservedState::Online));
        assert!(!wake_allowed(ObservedState::Resetting));
        assert!(!wake_allowed(ObservedState::Failed));
    }

    #[tokio::test]
    async fn online_backend_receives_original_handshake_without_waiting_hop() {
        let backend_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_address = backend_listener.local_addr().unwrap();
        let client_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let outbound = TcpStream::connect(client_listener.local_addr().unwrap());
        let (accepted, outbound) = tokio::join!(client_listener.accept(), outbound);
        let (accepted, _) = accepted.unwrap();
        let mut client = outbound.unwrap();

        let handshake = Handshake {
            protocol: 777,
            hostname: "friends.example.test".into(),
            next_state: 2,
            framed: vec![0x05, 0x00, 0x01, 0x02, 0x03, 0x04],
        };
        let expected = handshake.framed.clone();
        let backend = tokio::spawn(async move {
            let (mut stream, _) = backend_listener.accept().await.unwrap();
            let mut received = vec![0; expected.len()];
            stream.read_exact(&mut received).await.unwrap();
            assert_eq!(received, expected);
            send_login_disconnect(&mut stream, "direct backend fixture")
                .await
                .unwrap();
        });

        let mut target = route_target(ObservedState::Online);
        target.java_port = Some(backend_address.port());
        let lease = java_lease(&target).expect("direct Java lease");
        client.shutdown().await.unwrap();
        proxy_to_backend(accepted, handshake, lease).await.unwrap();
        backend.await.unwrap();

        let packet = read_frame(&mut client, 1024).await.unwrap();
        let mut cursor = 0;
        assert_eq!(read_varint(&packet, &mut cursor).unwrap(), 0);
        let message: serde_json::Value =
            serde_json::from_str(&read_string(&packet, &mut cursor, 255).unwrap()).unwrap();
        assert_eq!(message["text"], "direct backend fixture");
    }

    #[test]
    fn parses_java_handshake() {
        let mut body = Vec::new();
        write_varint(0, &mut body);
        write_varint(774, &mut body);
        write_string("alpha.play.example.com", &mut body).unwrap();
        body.extend_from_slice(&25565_u16.to_be_bytes());
        write_varint(2, &mut body);

        let mut cursor = 0;
        assert_eq!(read_varint(&body, &mut cursor).unwrap(), 0);
        assert_eq!(read_varint(&body, &mut cursor).unwrap(), 774);
        assert_eq!(
            read_string(&body, &mut cursor, 255).unwrap(),
            "alpha.play.example.com"
        );
    }

    #[test]
    fn rejects_oversized_varint() {
        let mut cursor = 0;
        assert!(read_varint(&[0x80; 5], &mut cursor).is_err());
    }
}

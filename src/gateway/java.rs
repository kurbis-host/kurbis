use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

use crate::{
    config::GatewayConfig,
    gateway::{routes::RouteCache, wake::request_wake},
    model::{ObservedState, RouteTarget},
};

const MAX_HANDSHAKE_FRAME: usize = 8 * 1024;

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
    let target = routes.resolve(&handshake.hostname).await?;
    match handshake.next_state {
        1 => handle_status(&mut client, &handshake, target.as_ref()).await,
        2 => handle_login(client, config, handshake, target).await,
        _ => Ok(()),
    }
}

async fn handle_login(
    mut client: TcpStream,
    config: &GatewayConfig,
    handshake: Handshake,
    target: Option<RouteTarget>,
) -> Result<()> {
    let Some(target) = target else {
        return send_login_disconnect(&mut client, "Unknown server address").await;
    };

    if target.account_suspended {
        return send_login_disconnect(
            &mut client,
            "Server suspended. Contact the hosting operator.",
        )
        .await;
    }

    if target.state == ObservedState::Online {
        if target.worker_address.is_none() {
            return send_login_disconnect(
                &mut client,
                "Server is temporarily unavailable because its worker is unreachable.",
            )
            .await;
        }
    } else {
        if target.state == ObservedState::Failed {
            return send_login_disconnect(
                &mut client,
                "Server failed to start. Retry it from the panel.",
            )
            .await;
        }
        if target.worker_healthy == Some(false)
            && matches!(
                target.state,
                ObservedState::Restoring
                    | ObservedState::Starting
                    | ObservedState::Stopping
                    | ObservedState::Snapshotting
            )
        {
            return send_login_disconnect(
                &mut client,
                "Server is temporarily unavailable because its assigned worker is unreachable.",
            )
            .await;
        }
        if matches!(
            target.state,
            ObservedState::Sleeping
                | ObservedState::Queued
                | ObservedState::Stopping
                | ObservedState::Snapshotting
        ) && let Err(error) = request_wake(config, &handshake.hostname).await
        {
            tracing::warn!(?error, "gateway wake request failed");
            return send_login_disconnect(
                &mut client,
                "Server is temporarily unavailable. Please reconnect shortly.",
            )
            .await;
        }
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
        return Ok(());
    }

    let address = target
        .worker_address
        .context("online route has no worker address")?;
    let port = target.java_port.context("online route has no Java port")?;
    let mut backend = match connect_backend((address.as_str(), port)).await {
        Ok(backend) => backend,
        Err(error) => {
            tracing::warn!(?error, "assigned backend is unreachable");
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

async fn connect_backend(address: impl tokio::net::ToSocketAddrs) -> Result<TcpStream> {
    Ok(tokio::time::timeout(
        std::time::Duration::from_secs(5),
        TcpStream::connect(address),
    )
    .await
    .context("backend connection timed out")??)
}

async fn handle_status(
    client: &mut TcpStream,
    handshake: &Handshake,
    target: Option<&RouteTarget>,
) -> Result<()> {
    let request = read_frame(client, 1024).await?;
    let mut cursor = 0;
    let packet_id = read_varint(&request, &mut cursor)?;
    if packet_id != 0 {
        bail!("unexpected status request packet id {packet_id}");
    }
    let (state, description) = match target {
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
            state => ("sleeping", format!("{state:?}")),
        },
    };
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

    #[tokio::test]
    async fn login_failures_receive_protocol_disconnects() {
        let config = GatewayConfig {
            common: crate::config::CommonConfig {
                database_url: String::new(),
            },
            java_listen: "127.0.0.1:0".parse().unwrap(),
            bedrock_listen: "127.0.0.1:0".parse().unwrap(),
            waiting_java_address: "127.0.0.1:0".parse().unwrap(),
            waiting_control_socket: "/nonexistent/control.sock".into(),
            waiting_control_token: "test".into(),
            control_plane_url: "http://127.0.0.1:0".into(),
            internal_token: "test".into(),
            root_domain: "example.test".into(),
            wake_timeout: std::time::Duration::from_secs(90),
        };
        for (state, suspended, expected) in [
            (
                ObservedState::Sleeping,
                true,
                "Server suspended. Contact the hosting operator.",
            ),
            (
                ObservedState::Online,
                false,
                "Server is temporarily unavailable. Please reconnect shortly.",
            ),
            (
                ObservedState::Sleeping,
                false,
                "Server is temporarily unavailable. Please reconnect shortly.",
            ),
            (
                ObservedState::Starting,
                false,
                "Waiting server is temporarily unavailable. Please reconnect shortly.",
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut client = TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap();
            let (accepted, _) = listener.accept().await.unwrap();
            let target = RouteTarget {
                server_id: uuid::Uuid::new_v4(),
                subdomain: "suspended".into(),
                state,
                account_suspended: suspended,
                worker_healthy: None,
                worker_valid_until: None,
                worker_address: Some("127.0.0.1".into()),
                java_port: Some(0),
                bedrock_port: None,
            };
            let handshake = Handshake {
                protocol: 777,
                hostname: "suspended.example.test".into(),
                next_state: 2,
                framed: Vec::new(),
            };
            handle_login(accepted, &config, handshake, Some(target))
                .await
                .unwrap();
            let packet = read_frame(&mut client, 1024).await.unwrap();
            let mut cursor = 0;
            assert_eq!(read_varint(&packet, &mut cursor).unwrap(), 0);
            let message: serde_json::Value =
                serde_json::from_str(&read_string(&packet, &mut cursor, 255).unwrap()).unwrap();
            assert_eq!(message["text"], expected);
            assert_eq!(cursor, packet.len());
        }
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

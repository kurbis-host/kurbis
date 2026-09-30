//! Bedrock UDP entrypoint.
//!
//! Bedrock's transport stack differs from Java's TCP protocol.  The gateway
//! keeps the UDP entrypoint separate so the protocol/login implementation can
//! evolve with Pumpkin's supported NetherNet transport without coupling it to
//! the Java proxy.

use std::net::SocketAddr;

use anyhow::Result;
use serde::Deserialize;
use tokio::net::UdpSocket;

use crate::{config::GatewayConfig, gateway::routes::RouteCache};

const MAX_PACKET: usize = 2048;

pub async fn run(config: GatewayConfig, _routes: RouteCache) -> Result<()> {
    let socket = UdpSocket::bind(config.bedrock_listen).await?;
    tracing::info!(listen = %config.bedrock_listen, "Bedrock gateway UDP entrypoint listening");

    let mut buffer = [0_u8; MAX_PACKET];
    loop {
        let (length, peer) = socket.recv_from(&mut buffer).await?;
        if let Err(error) = handle_datagram(&socket, peer, &buffer[..length]).await {
            tracing::debug!(%peer, ?error, "Bedrock gateway datagram rejected");
        }
    }
}

async fn handle_datagram(_socket: &UdpSocket, peer: SocketAddr, packet: &[u8]) -> Result<()> {
    // The Bedrock login protocol is not a raw UDP proxy. The actual session is
    // negotiated through Pumpkin's supported NetherNet transport. Until that
    // transport is terminated by the gateway, do not pretend arbitrary UDP
    // bytes are a valid Bedrock response.
    if packet.is_empty() {
        return Ok(());
    }
    tracing::debug!(%peer, bytes = packet.len(), "Bedrock transport handshake received before gateway transport integration");
    Ok(())
}

/// Shape of the payload embedded in the Bedrock login JWT.
///
/// Pumpkin currently reads this from `ClientData` during login. Keeping the
/// extraction logic here lets the gateway share the same hostname-routing
/// boundary once the NetherNet session layer is added.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ClientData {
    pub server_address: String,
}

pub fn server_address_from_client_data(payload: &[u8]) -> Result<String> {
    Ok(serde_json::from_slice::<ClientData>(payload)?.server_address)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bedrock_server_address_from_client_data() {
        let payload = br#"{"ServerAddress":"kurbis.example.com"}"#;
        assert_eq!(
            server_address_from_client_data(payload).unwrap(),
            "kurbis.example.com"
        );
    }
}

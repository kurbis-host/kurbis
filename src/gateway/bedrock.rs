//! Per-client RakNet transport into the waiting Pumpkin server. Pumpkin owns
//! login/authentication and exposes the decoded ServerAddress through host RPC;
//! `waiting` resolves that address and sends the edition's native transfer.

use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use tokio::{
    net::UdpSocket,
    task::{AbortHandle, JoinSet},
    time::{Instant, interval},
};

use crate::config::GatewayConfig;

const MAX_SESSIONS: usize = 2048;
const SESSION_IDLE: Duration = Duration::from_secs(60);
const OFFLINE_MAGIC: [u8; 16] = [
    0x00, 0xff, 0xff, 0x00, 0xfe, 0xfe, 0xfe, 0xfe, 0xfd, 0xfd, 0xfd, 0xfd, 0x12, 0x34, 0x56, 0x78,
];

struct Session {
    id: u64,
    socket: Arc<UdpSocket>,
    last_input: Instant,
    replies: AbortHandle,
}

pub async fn run(config: GatewayConfig) -> Result<()> {
    if config.waiting_bedrock_address == config.bedrock_listen
        || config.waiting_bedrock_address.ip().is_unspecified()
        || config.waiting_bedrock_address.port() == 0
    {
        bail!("waiting Bedrock address must identify a separate reachable RakNet listener");
    }
    let socket = Arc::new(UdpSocket::bind(config.bedrock_listen).await?);
    tracing::info!(listen = %config.bedrock_listen, waiting = %config.waiting_bedrock_address,
        "Bedrock RakNet gateway listening");
    relay(socket, config.waiting_bedrock_address).await
}

async fn relay(public: Arc<UdpSocket>, waiting: SocketAddr) -> Result<()> {
    let mut sessions = HashMap::<SocketAddr, Session>::new();
    // Dropping the gateway aborts all reply loops instead of leaking sockets.
    let mut replies = JoinSet::new();
    let mut sweep = interval(Duration::from_secs(5));
    let mut next_id = 0_u64;
    let mut packet = vec![0_u8; 65535];
    loop {
        tokio::select! {
            received = public.recv_from(&mut packet) => {
                let (length, peer) = received?;
                if !sessions.contains_key(&peer) {
                    // Only valid offline discovery/open requests may allocate a
                    // transport session. Pumpkin validates the remaining protocol.
                    if sessions.len() >= MAX_SESSIONS || !opens_session(&packet[..length]) {
                        continue;
                    }
                    let bind = if waiting.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
                    let backend = Arc::new(UdpSocket::bind(bind).await?);
                    backend.connect(waiting).await?;
                    next_id = next_id.wrapping_add(1);
                    let id = next_id;
                    let incoming = backend.clone();
                    let outgoing = public.clone();
                    let handle = replies.spawn(async move {
                        (peer, id, forward_replies(incoming, outgoing, peer).await)
                    });
                    sessions.insert(peer, Session {
                        id, socket: backend, last_input: Instant::now(), replies: handle,
                    });
                }
                let session = sessions.get_mut(&peer).expect("session allocated");
                session.last_input = Instant::now();
                if let Err(error) = session.socket.send(&packet[..length]).await {
                    tracing::debug!(%peer, ?error, "Bedrock waiting transport unavailable");
                    if let Some(session) = sessions.remove(&peer) { session.replies.abort(); }
                }
            }
            completed = replies.join_next(), if !replies.is_empty() => {
                if let Some(Ok((peer, id, result))) = completed {
                    if let Err(error) = result {
                        tracing::debug!(%peer, ?error, "Bedrock reply transport closed");
                    }
                    // A delayed completion from an expired socket cannot remove
                    // its replacement session for the same client endpoint.
                    if sessions.get(&peer).is_some_and(|session| session.id == id) {
                        sessions.remove(&peer);
                    }
                }
            }
            _ = sweep.tick() => {
                sessions.retain(|_, session| {
                    if session.last_input.elapsed() >= SESSION_IDLE {
                        session.replies.abort();
                        false
                    } else { true }
                });
            }
        }
    }
}

async fn forward_replies(
    backend: Arc<UdpSocket>,
    public: Arc<UdpSocket>,
    peer: SocketAddr,
) -> Result<()> {
    let mut packet = vec![0_u8; 65535];
    loop {
        let length = backend
            .recv(&mut packet)
            .await
            .context("receive waiting RakNet packet")?;
        public
            .send_to(&packet[..length], peer)
            .await
            .context("send public RakNet packet")?;
    }
}

fn opens_session(packet: &[u8]) -> bool {
    let magic_offset = match packet.first() {
        Some(0x01 | 0x02) if packet.len() >= 33 => 9, // Timestamp, magic, client GUID.
        Some(0x05) if packet.len() >= 18 => 1,        // Magic and protocol version.
        _ => return false,
    };
    packet.get(magic_offset..magic_offset + OFFLINE_MAGIC.len()) == Some(&OFFLINE_MAGIC)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_request() -> Vec<u8> {
        let mut packet = vec![0x05];
        packet.extend(OFFLINE_MAGIC);
        packet.push(11);
        packet.resize(1200, 0);
        packet
    }

    #[tokio::test]
    async fn two_clients_have_distinct_raknet_endpoints_and_reply_isolation() {
        let waiting = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let public = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let address = public.local_addr().unwrap();
        let task = tokio::spawn(relay(public, waiting.local_addr().unwrap()));
        let clients = [
            UdpSocket::bind("127.0.0.1:0").await.unwrap(),
            UdpSocket::bind("127.0.0.1:0").await.unwrap(),
        ];
        let mut buffer = [0_u8; 1500];
        let mut endpoints = Vec::new();
        for (index, client) in clients.iter().enumerate() {
            client.connect(address).await.unwrap();
            let mut open = open_request();
            open[1199] = index as u8;
            client.send(&open).await.unwrap();
            let (length, endpoint) =
                tokio::time::timeout(Duration::from_secs(2), waiting.recv_from(&mut buffer))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(&buffer[..length], open);
            endpoints.push(endpoint);
        }
        assert_ne!(endpoints[0], endpoints[1]);
        for index in (0..2).rev() {
            waiting
                .send_to(&[0x80, index as u8], endpoints[index])
                .await
                .unwrap();
            let length =
                tokio::time::timeout(Duration::from_secs(2), clients[index].recv(&mut buffer))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(&buffer[..length], &[0x80, index as u8]);
        }
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }

    #[test]
    fn malformed_and_connected_packets_cannot_allocate_sessions() {
        for packet in [vec![], vec![0x05], vec![0x05; 32], vec![0x80; 32]] {
            assert!(!opens_session(&packet));
        }
        assert!(opens_session(&open_request()));
        let mut ping = vec![0x01];
        ping.extend(42_i64.to_be_bytes());
        ping.extend(OFFLINE_MAGIC);
        ping.extend(1_u64.to_be_bytes());
        assert!(opens_session(&ping));
    }
}

//! Per-client RakNet transport into the waiting Pumpkin server. Pumpkin owns
//! login/authentication and exposes the decoded ServerAddress through host RPC;
//! `waiting` resolves that address and sends the edition's native transfer.

use std::{
    collections::HashMap,
    future::Future,
    io,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::Arc,
    task::{Context as TaskContext, Poll},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{ConnectInfo, State, connect_info::Connected},
    http::{HeaderMap, Method, StatusCode, Uri, header, uri::Authority},
    response::{IntoResponse, Response},
    serve::Listener,
};
use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use serde::Deserialize;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc},
    task::{AbortHandle, JoinSet},
    time::{Instant, interval, sleep, timeout},
};
use tokio_rustls::{TlsAcceptor, rustls::ServerConfig, server::TlsStream};
use uuid::Uuid;

use crate::{
    config::GatewayConfig,
    model::{is_private_signal_address, subdomain_from_hostname},
};

const MAX_SESSIONS: usize = 2048;
const MAX_SIGNAL_BODY: usize = 1 << 20;
const MAX_SIGNAL_ROUTE_BYTES: usize = 8 << 10;
const MAX_TLS_HANDSHAKES: usize = 128;
const SESSION_IDLE: Duration = Duration::from_secs(60);
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
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
    let tcp = TcpListener::bind(config.bedrock_listen).await?;
    let tls = load_tls_acceptor(&config)?;
    let listener = TlsListener::new(tcp, tls);
    let signal_state = Arc::new(SignalProxy::new(config.clone())?);
    let app = Router::new()
        .fallback(proxy_signal)
        .with_state(signal_state);
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SignalPeer>(),
    );

    tracing::info!(listen = %config.bedrock_listen, waiting = %config.waiting_bedrock_address,
        tls_certificate = %config.bedrock_tls_certificate.display(),
        "Bedrock RakNet and TLS signaling gateway listening");
    tokio::try_join!(relay(socket, config.waiting_bedrock_address), async {
        server
            .await
            .context("Bedrock TLS signaling listener stopped")
    },)?;
    Ok(())
}

fn load_tls_acceptor(config: &GatewayConfig) -> Result<TlsAcceptor> {
    let certificate = std::fs::File::open(&config.bedrock_tls_certificate).with_context(|| {
        format!(
            "open Bedrock TLS certificate at {}",
            config.bedrock_tls_certificate.display()
        )
    })?;
    let certificates = rustls_pemfile::certs(&mut std::io::BufReader::new(certificate))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    anyhow::ensure!(
        !certificates.is_empty(),
        "Bedrock TLS certificate bundle is empty"
    );

    let key_file = std::fs::File::open(&config.bedrock_tls_private_key).with_context(|| {
        format!(
            "open Bedrock TLS private key at {}",
            config.bedrock_tls_private_key.display()
        )
    })?;
    let key = rustls_pemfile::private_key(&mut std::io::BufReader::new(key_file))?
        .context("Bedrock TLS private key file contains no supported PEM key")?;
    let mut tls = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .context("configure Bedrock TLS certificate and private key")?;
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(tls)))
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignalRoute {
    server_id: Uuid,
    generation: i64,
    signal_address: String,
    signal_port: u16,
    worker_address: String,
    bedrock_port: u16,
}

#[derive(Clone)]
struct SignalProxy {
    config: GatewayConfig,
    http: reqwest::Client,
}

#[derive(Clone, Debug)]
struct SignalPeer {
    address: SocketAddr,
    server_name: Option<String>,
}

impl<'a> Connected<axum::serve::IncomingStream<'a, TlsListener>> for SignalPeer {
    fn connect_info(stream: axum::serve::IncomingStream<'a, TlsListener>) -> Self {
        stream.remote_addr().clone()
    }
}

impl SignalProxy {
    fn new(config: GatewayConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self { config, http })
    }

    async fn target(&self, hostname: &str) -> Result<SocketAddr, StatusCode> {
        if let Some(server_id) = server_id_from_signal_hostname(hostname, &self.config.root_domain)
        {
            let url = format!(
                "{}/internal/bedrock-signal/{server_id}",
                self.config.control_plane_url.trim_end_matches('/')
            );
            let response = self
                .http
                .get(url)
                .bearer_auth(&self.config.internal_token)
                .send()
                .await
                .map_err(|error| {
                    tracing::warn!(%server_id, ?error, "Bedrock signal assignment lookup failed");
                    StatusCode::SERVICE_UNAVAILABLE
                })?;
            if !response.status().is_success() {
                tracing::debug!(%server_id, status = %response.status(), "Bedrock signal assignment is unavailable");
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            }
            let body = read_bounded_response_body(response, MAX_SIGNAL_ROUTE_BYTES).await?;
            let route: SignalRoute = serde_json::from_slice(&body).map_err(|error| {
                tracing::warn!(%server_id, ?error, "invalid Bedrock signal assignment response");
                StatusCode::BAD_GATEWAY
            })?;
            let target = validate_signal_route(&route, server_id)?;
            tracing::debug!(%server_id, generation = route.generation,
                worker = %route.worker_address, bedrock_port = route.bedrock_port,
                signal = %target,
                "resolved current Bedrock signal assignment");
            return Ok(target);
        }

        if subdomain_from_hostname(hostname, &self.config.root_domain).is_some() {
            return Ok(self.config.waiting_bedrock_address);
        }

        Err(StatusCode::NOT_FOUND)
    }
}

fn validate_signal_route(
    route: &SignalRoute,
    expected_server_id: Uuid,
) -> Result<SocketAddr, StatusCode> {
    if route.server_id != expected_server_id || route.generation < 1 {
        return Err(StatusCode::BAD_GATEWAY);
    }
    let signal_ip = route
        .signal_address
        .parse::<IpAddr>()
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let worker_ip = route
        .worker_address
        .parse::<IpAddr>()
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let invalid_public_ip = match worker_ip {
        IpAddr::V4(address) => {
            address.is_unspecified() || address.is_broadcast() || address.is_multicast()
        }
        IpAddr::V6(address) => address.is_unspecified() || address.is_multicast(),
    };
    if !is_private_signal_address(&route.signal_address)
        || route.signal_port <= 1024
        || route.bedrock_port <= 1024
        || route.signal_port == route.bedrock_port
        || invalid_public_ip
    {
        return Err(StatusCode::BAD_GATEWAY);
    }
    Ok(SocketAddr::new(signal_ip, route.signal_port))
}

async fn proxy_signal(
    State(proxy): State<Arc<SignalProxy>>,
    ConnectInfo(peer): ConnectInfo<SignalPeer>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    body: Body,
) -> Response {
    if uri.query().is_some() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let path = uri.path();
    let valid_path =
        path == "/v1/join" || path.strip_prefix("/v1/join/").is_some_and(valid_network_id);
    if !valid_path {
        return StatusCode::NOT_FOUND.into_response();
    }

    let body = match to_bytes(body, MAX_SIGNAL_BODY).await {
        Ok(body) => body,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if method == Method::GET {
        if path != "/v1/join" || !body.is_empty() {
            return StatusCode::BAD_REQUEST.into_response();
        }
    } else if method == Method::POST {
        if path == "/v1/join"
            || !content_type
                .split(';')
                .next()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/sdp"))
        {
            return StatusCode::BAD_REQUEST.into_response();
        }
        if body.is_empty() {
            return StatusCode::BAD_REQUEST.into_response();
        }
    } else {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }

    let Some(hostname) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| normalize_signal_authority(value, proxy.config.bedrock_public_port))
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if peer.server_name.as_deref() != Some(hostname.as_str()) {
        tracing::debug!(peer = %peer.address, host = %hostname, sni = ?peer.server_name,
            "Bedrock TLS authority did not match the requested host");
        return StatusCode::MISDIRECTED_REQUEST.into_response();
    }
    let target = match proxy.target(&hostname).await {
        Ok(target) => target,
        Err(status) => return status.into_response(),
    };
    let backend_url = format!("http://{target}{path}");
    let mut request = proxy
        .http
        .request(method, backend_url)
        .header(header::ACCEPT, "application/json, application/sdp")
        .header(header::CACHE_CONTROL, "no-store");
    if !body.is_empty() {
        request = request
            .header(header::CONTENT_TYPE, "application/sdp")
            .body(body);
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%target, ?error, "Bedrock signal backend request failed");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let (status, content_type, response_body) = match read_bounded_signal_response(response).await {
        Ok(response) => response,
        Err(status) => return status.into_response(),
    };

    let mut output = Response::builder().status(status);
    if let Some(value) = content_type.filter(|value| {
        value == "application/json"
            || value == "application/sdp"
            || value.as_bytes().starts_with(b"text/plain")
    }) {
        output = output.header(header::CONTENT_TYPE, value);
    }
    output = output.header(header::CACHE_CONTROL, "no-store");
    output
        .body(Body::from(response_body))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

async fn read_bounded_signal_response(
    response: reqwest::Response,
) -> Result<(StatusCode, Option<axum::http::HeaderValue>, Bytes), StatusCode> {
    let status = response.status();
    let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
    let body = read_bounded_response_body(response, MAX_SIGNAL_BODY).await?;
    Ok((status, content_type, body))
}

async fn read_bounded_response_body(
    response: reqwest::Response,
    limit: usize,
) -> Result<Bytes, StatusCode> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(StatusCode::BAD_GATEWAY);
    }
    let mut stream = response.bytes_stream();
    let mut body = BytesMut::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            tracing::warn!(?error, "Bedrock signaling response stream failed");
            StatusCode::BAD_GATEWAY
        })?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(StatusCode::BAD_GATEWAY);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body.freeze())
}

fn valid_network_id(value: &str) -> bool {
    if value.is_empty() || value.len() > 384 {
        return false;
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut offset = 0;
    while offset < bytes.len() {
        let byte = if bytes[offset] == b'%' {
            let Some(high) = bytes.get(offset + 1).and_then(|value| hex_digit(*value)) else {
                return false;
            };
            let Some(low) = bytes.get(offset + 2).and_then(|value| hex_digit(*value)) else {
                return false;
            };
            offset += 3;
            high * 16 + low
        } else {
            let byte = bytes[offset];
            offset += 1;
            if !byte.is_ascii_graphic() {
                return false;
            }
            byte
        };
        if matches!(byte, b'/' | b'\\' | b'?' | b'#' | 0..=31 | 127) {
            return false;
        }
        decoded.push(byte);
    }
    std::str::from_utf8(&decoded).is_ok()
}

fn normalize_signal_authority(value: &str, public_port: u16) -> Option<String> {
    let authority = value.parse::<Authority>().ok()?;
    if authority.as_str().contains('@') {
        return None;
    }
    if authority.port_u16().is_some_and(|port| port != public_port) {
        return None;
    }
    normalize_signal_hostname(authority.host())
}

fn normalize_signal_hostname(value: &str) -> Option<String> {
    let value = value.strip_suffix('.').unwrap_or(value);
    if value.ends_with('.') {
        return None;
    }
    let hostname = value.to_ascii_lowercase();
    if hostname.is_empty() || hostname.len() > 253 {
        return None;
    }
    if hostname.split('.').any(|label| {
        label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    }) {
        return None;
    }
    Some(hostname)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn server_id_from_signal_hostname(hostname: &str, root_domain: &str) -> Option<Uuid> {
    let hostname = hostname.trim_end_matches('.').to_ascii_lowercase();
    let root_domain = root_domain.trim_end_matches('.').to_ascii_lowercase();
    let suffix = format!(".{root_domain}");
    let label = hostname.strip_suffix(&suffix)?;
    let raw_id = label.strip_prefix("s-")?;
    if raw_id.contains('.') {
        return None;
    }
    let server_id = Uuid::parse_str(raw_id).ok()?;
    (label == format!("s-{server_id}")).then_some(server_id)
}

struct TlsListener {
    tcp: TcpListener,
    acceptor: TlsAcceptor,
    ready_tx: mpsc::Sender<(TlsConnection, SignalPeer)>,
    ready_rx: mpsc::Receiver<(TlsConnection, SignalPeer)>,
    handshakes: Arc<Semaphore>,
}

impl TlsListener {
    fn new(tcp: TcpListener, acceptor: TlsAcceptor) -> Self {
        let (ready_tx, ready_rx) = mpsc::channel(MAX_TLS_HANDSHAKES);
        Self {
            tcp,
            acceptor,
            ready_tx,
            ready_rx,
            handshakes: Arc::new(Semaphore::new(MAX_TLS_HANDSHAKES)),
        }
    }
}

struct TlsConnection {
    stream: TlsStream<TcpStream>,
    _permit: OwnedSemaphorePermit,
}

impl AsyncRead for TlsConnection {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_read(context, buffer)
    }
}

impl AsyncWrite for TlsConnection {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write(context, buffer)
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(context)
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(context)
    }
}

impl Listener for TlsListener {
    type Io = TlsConnection;
    type Addr = SignalPeer;

    #[allow(clippy::manual_async_fn)]
    fn accept(&mut self) -> impl Future<Output = (Self::Io, Self::Addr)> + Send {
        async move {
            loop {
                tokio::select! {
                    Some(connection) = self.ready_rx.recv() => return connection,
                    accepted = self.tcp.accept() => match accepted {
                        Ok((stream, address)) => {
                            let Ok(permit) = self.handshakes.clone().try_acquire_owned() else {
                                tracing::debug!(%address, "Bedrock TLS connection limit reached");
                                continue;
                            };
                            let acceptor = self.acceptor.clone();
                            let ready = self.ready_tx.clone();
                            tokio::spawn(async move {
                                match timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                                    Ok(Ok(stream)) => {
                                        let server_name = stream
                                            .get_ref()
                                            .1
                                            .server_name()
                                            .map(str::to_owned);
                                        let peer = SignalPeer { address, server_name };
                                        let _ = ready.send((TlsConnection { stream, _permit: permit }, peer)).await;
                                    }
                                    Ok(Err(error)) => tracing::debug!(%address, ?error, "Bedrock TLS handshake rejected"),
                                    Err(_) => tracing::debug!(%address, "Bedrock TLS handshake timed out"),
                                }
                            });
                        }
                        Err(error) => {
                            tracing::warn!(?error, "Bedrock TLS accept failed");
                            sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(SignalPeer {
            address: self.tcp.local_addr()?,
            server_name: None,
        })
    }
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

    #[test]
    fn server_alias_is_canonical_and_scoped_to_the_root_domain() {
        let server_id = Uuid::new_v4();
        let alias = format!("s-{server_id}.mc.example.test");
        assert_eq!(
            server_id_from_signal_hostname(&alias, "mc.example.test"),
            Some(server_id)
        );
        assert_eq!(
            server_id_from_signal_hostname(&alias.to_ascii_uppercase(), "mc.example.test"),
            Some(server_id)
        );
        assert_eq!(
            server_id_from_signal_hostname(&alias, "other.example.test"),
            None
        );
        assert_eq!(
            server_id_from_signal_hostname(&format!("nested.{alias}"), "mc.example.test"),
            None
        );
        assert_eq!(
            server_id_from_signal_hostname("s-not-a-uuid.mc.example.test", "mc.example.test"),
            None
        );
    }

    #[test]
    fn network_id_accepts_opaque_segments_but_not_path_escape() {
        assert!(valid_network_id("1234567890"));
        assert!(valid_network_id("550e8400-e29b-41d4-a716-446655440000"));
        assert!(valid_network_id("client%3Aplatform"));
        assert!(!valid_network_id(""));
        assert!(!valid_network_id("client/other"));
        assert!(!valid_network_id("client%2Fother"));
        assert!(!valid_network_id("client%ZZ"));
        assert!(!valid_network_id("client%00"));
    }

    #[test]
    fn signaling_authority_is_strict_and_uses_the_public_port() {
        assert_eq!(
            normalize_signal_authority("Live.MC.Example.Test:19132", 19132),
            Some("live.mc.example.test".into())
        );
        assert_eq!(
            normalize_signal_authority("live.mc.example.test", 19132),
            Some("live.mc.example.test".into())
        );
        for authority in [
            "live.mc.example.test:19133",
            "user@live.mc.example.test",
            "live..mc.example.test",
            "live.mc.example.test..",
            "-live.mc.example.test",
            "live.mc.example.test.",
            "live.mc.example.test:0",
            "[::1]:19132",
        ] {
            if authority == "live.mc.example.test." {
                assert_eq!(
                    normalize_signal_authority(authority, 19132),
                    Some("live.mc.example.test".into())
                );
            } else {
                assert_eq!(
                    normalize_signal_authority(authority, 19132),
                    None,
                    "accepted malformed authority {authority}"
                );
            }
        }
    }

    #[test]
    fn signal_target_requires_a_current_private_and_distinct_endpoint() {
        let server_id = Uuid::new_v4();
        let valid = SignalRoute {
            server_id,
            generation: 3,
            signal_address: "127.0.0.1".into(),
            signal_port: 46002,
            worker_address: "192.0.2.20".into(),
            bedrock_port: 46001,
        };
        assert_eq!(
            validate_signal_route(&valid, server_id).unwrap(),
            "127.0.0.1:46002".parse::<SocketAddr>().unwrap()
        );

        let invalid = [
            SignalRoute {
                generation: 0,
                ..valid.clone()
            },
            SignalRoute {
                server_id: Uuid::new_v4(),
                ..valid.clone()
            },
            SignalRoute {
                signal_address: "8.8.8.8".into(),
                ..valid.clone()
            },
            SignalRoute {
                signal_address: "0.0.0.0".into(),
                ..valid.clone()
            },
            SignalRoute {
                signal_port: 1024,
                ..valid.clone()
            },
            SignalRoute {
                bedrock_port: 1024,
                ..valid.clone()
            },
            SignalRoute {
                bedrock_port: valid.signal_port,
                ..valid.clone()
            },
            SignalRoute {
                worker_address: "0.0.0.0".into(),
                ..valid.clone()
            },
            SignalRoute {
                worker_address: "224.0.0.1".into(),
                ..valid.clone()
            },
        ];
        for route in invalid {
            assert_eq!(
                validate_signal_route(&route, server_id),
                Err(StatusCode::BAD_GATEWAY)
            );
        }
    }

    #[tokio::test]
    async fn signaling_response_limit_rejects_declared_and_chunked_overflow() {
        use std::convert::Infallible;

        use axum::routing::get;

        let app = Router::new()
            .route(
                "/declared",
                get(|| async {
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(Body::from(vec![0_u8; MAX_SIGNAL_BODY + 1]))
                        .unwrap()
                }),
            )
            .route(
                "/chunked",
                get(|| async {
                    let chunks = futures::stream::iter([
                        Ok::<_, Infallible>(bytes::Bytes::from(vec![0; MAX_SIGNAL_BODY / 2])),
                        Ok::<_, Infallible>(bytes::Bytes::from(vec![1; MAX_SIGNAL_BODY / 2])),
                        Ok::<_, Infallible>(bytes::Bytes::from(vec![2; 1])),
                    ]);
                    Response::builder()
                        .status(StatusCode::OK)
                        .header(header::CONTENT_TYPE, "application/sdp")
                        .body(Body::from_stream(chunks))
                        .unwrap()
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::new();
        for path in ["declared", "chunked"] {
            let response = client
                .get(format!("http://{address}/{path}"))
                .send()
                .await
                .unwrap();
            assert_eq!(
                read_bounded_signal_response(response).await.unwrap_err(),
                StatusCode::BAD_GATEWAY,
                "accepted oversized {path} backend response"
            );
        }
        let resolver_response = client
            .get(format!("http://{address}/chunked"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            read_bounded_response_body(resolver_response, MAX_SIGNAL_ROUTE_BYTES)
                .await
                .unwrap_err(),
            StatusCode::BAD_GATEWAY,
            "accepted oversized assignment JSON response"
        );
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn client_session_forwards_raknet_stun_and_dtls_without_cross_routing() {
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

        let mut stun_request = vec![0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xa4, 0x42];
        stun_request.extend([0x11; 12]);
        clients[0].send(&stun_request).await.unwrap();
        let (length, stun_endpoint) =
            tokio::time::timeout(Duration::from_secs(2), waiting.recv_from(&mut buffer))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(stun_endpoint, endpoints[0]);
        assert_eq!(&buffer[..length], stun_request);
        let mut stun_response = stun_request.clone();
        stun_response[1] = 0x01;
        waiting
            .send_to(&stun_response, stun_endpoint)
            .await
            .unwrap();
        let length = tokio::time::timeout(Duration::from_secs(2), clients[0].recv(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buffer[..length], stun_response);

        let dtls_client_hello = [22, 0xfe, 0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0];
        clients[0].send(&dtls_client_hello).await.unwrap();
        let (length, dtls_endpoint) =
            tokio::time::timeout(Duration::from_secs(2), waiting.recv_from(&mut buffer))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(dtls_endpoint, endpoints[0]);
        assert_eq!(&buffer[..length], dtls_client_hello);
        let dtls_server_hello = [22, 0xfe, 0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0];
        waiting
            .send_to(&dtls_server_hello, dtls_endpoint)
            .await
            .unwrap();
        let length = tokio::time::timeout(Duration::from_secs(2), clients[0].recv(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buffer[..length], dtls_server_hello);

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

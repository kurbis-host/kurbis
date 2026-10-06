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
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc},
    task::{AbortHandle, JoinSet},
    time::{Instant, interval, sleep, timeout},
};
use tokio_rustls::{TlsAcceptor, rustls::ServerConfig, server::TlsStream};
use uuid::Uuid;

use crate::{
    config::GatewayConfig,
    model::{is_private_signal_address, subdomain_from_hostname},
    routing::BedrockSignalTarget,
};

const MAX_SESSIONS: usize = 2048;
const MAX_SIGNAL_BODY: usize = 1 << 20;
const MAX_SIGNAL_ROUTE_BYTES: usize = 8 << 10;
const MAX_TLS_HANDSHAKES: usize = 128;
const SESSION_IDLE: Duration = Duration::from_secs(60);
const SIGNAL_ROUTE_IDLE: Duration = Duration::from_secs(60);
const MAX_SIGNAL_ROUTE_LEASES: usize = 4096;
const MAX_ICE_UFRAG_BYTES: usize = 256;
const STUN_MAGIC: [u8; 4] = [0x21, 0x12, 0xa4, 0x42];
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const OFFLINE_MAGIC: [u8; 16] = [
    0x00, 0xff, 0xff, 0x00, 0xfe, 0xfe, 0xfe, 0xfe, 0xfd, 0xfd, 0xfd, 0xfd, 0x12, 0x34, 0x56, 0x78,
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct SignalRoute {
    server_id: Option<Uuid>,
    generation: Option<i64>,
    hostname: Option<String>,
    signal: SocketAddr,
    bedrock: SocketAddr,
}

impl SignalRoute {
    fn waiting(config: &GatewayConfig) -> Self {
        Self {
            server_id: None,
            generation: None,
            hostname: None,
            signal: config.waiting_signal_address,
            bedrock: config.waiting_bedrock_address,
        }
    }
}

struct Session {
    id: u64,
    socket: Arc<UdpSocket>,
    route: SignalRoute,
    last_input: Instant,
    replies: AbortHandle,
}

#[derive(Clone)]
struct RouteRegistry {
    state: Arc<Mutex<RouteRegistryState>>,
}

struct RouteRegistryState {
    next_id: u64,
    by_ufrag: HashMap<String, SignalRouteLease>,
    // Highest generation observed from a successful signaling lookup in this
    // process. This fences active sessions once a replacement is observed, but
    // is not an atomic/live control-plane lease: without a newer lookup or a
    // control-plane push, an already-active UDP session cannot learn that its
    // backend placement was revoked.
    generations: HashMap<Uuid, i64>,
}

#[derive(Clone)]
struct SignalRouteLease {
    id: u64,
    source_ip: IpAddr,
    route: SignalRoute,
    expires: Instant,
}

impl RouteRegistry {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(RouteRegistryState {
                next_id: 0,
                by_ufrag: HashMap::new(),
                generations: HashMap::new(),
            })),
        }
    }

    async fn register(&self, source_ip: IpAddr, route: SignalRoute, ufrags: Vec<String>) -> bool {
        if ufrags.is_empty() || ufrags.iter().any(|ufrag| !valid_ice_ufrag(ufrag)) {
            return false;
        }
        let mut state = self.state.lock().await;
        purge_expired_routes(&mut state);
        if let (Some(server_id), Some(generation)) = (route.server_id, route.generation) {
            if state
                .generations
                .get(&server_id)
                .is_some_and(|current| *current > generation)
            {
                return false;
            }
            if state
                .generations
                .insert(server_id, generation)
                .is_some_and(|current| current < generation)
            {
                state.by_ufrag.retain(|_, lease| {
                    !(lease.route.server_id == Some(server_id)
                        && lease
                            .route
                            .generation
                            .is_some_and(|value| value < generation))
                });
            }
        }
        let mut unique = ufrags;
        unique.sort_unstable();
        unique.dedup();
        let additional = unique
            .iter()
            .filter(|ufrag| !state.by_ufrag.contains_key(*ufrag))
            .count();
        if additional > MAX_SIGNAL_ROUTE_LEASES.saturating_sub(state.by_ufrag.len()) {
            return false;
        }
        state.next_id = state.next_id.wrapping_add(1);
        let id = state.next_id;
        let expires = Instant::now() + SIGNAL_ROUTE_IDLE;
        for ufrag in unique {
            state.by_ufrag.insert(
                ufrag,
                SignalRouteLease {
                    id,
                    source_ip,
                    route: route.clone(),
                    expires,
                },
            );
        }
        true
    }

    async fn route_for_packet(&self, peer: SocketAddr, packet: &[u8]) -> Option<SignalRoute> {
        let mut state = self.state.lock().await;
        purge_expired_routes(&mut state);
        let mut candidates = stun_username_tokens(packet)
            .into_iter()
            .filter_map(|ufrag| state.by_ufrag.get(&ufrag).cloned());
        let lease = candidates.next()?;
        if lease.source_ip == peer.ip()
            && candidates.all(|other| other.id == lease.id && other.source_ip == peer.ip())
            && route_generation_is_current(&state, &lease.route)
        {
            Some(lease.route)
        } else {
            None
        }
    }

    async fn is_current(&self, route: &SignalRoute) -> bool {
        let mut state = self.state.lock().await;
        if !route_generation_is_current(&state, route) {
            return false;
        }
        if route.server_id.is_none() {
            return true;
        }
        let current = state.by_ufrag.values().any(|lease| lease.route == *route);
        if current {
            // Route leases are idle leases, not absolute session lifetimes. A
            // live UDP session refreshes its credentials on every packet;
            // inactive sessions are still removed by the relay sweep.
            let expires = Instant::now() + SIGNAL_ROUTE_IDLE;
            for lease in state.by_ufrag.values_mut() {
                if lease.route == *route {
                    lease.expires = expires;
                }
            }
        }
        current
    }
}

fn purge_expired_routes(state: &mut RouteRegistryState) {
    let now = Instant::now();
    state.by_ufrag.retain(|_, lease| lease.expires > now);
}

fn route_generation_is_current(state: &RouteRegistryState, route: &SignalRoute) -> bool {
    // This is intentionally an observed-generation fence, not proof that the
    // control plane still considers the assignment active. The signal API
    // returns a point-in-time assignment and current UDP sessions do not
    // re-query it on every datagram; hard revocation requires a pushed or
    // atomic backend-validated lease, which the current protocol lacks.
    route
        .server_id
        .zip(route.generation)
        .is_none_or(|(server_id, generation)| {
            state
                .generations
                .get(&server_id)
                .is_none_or(|current| *current == generation)
        })
}

pub async fn run(config: GatewayConfig) -> Result<()> {
    if config.waiting_bedrock_address == config.bedrock_listen
        || config.waiting_bedrock_address.ip().is_unspecified()
        || config.waiting_bedrock_address.port() == 0
    {
        bail!("waiting Bedrock address must identify a separate reachable RakNet listener");
    }
    if config.waiting_signal_address.ip().is_unspecified()
        || config.waiting_signal_address.port() == 0
        || !is_private_signal_address(&config.waiting_signal_address.ip().to_string())
    {
        bail!("waiting signal address must identify a reachable private HTTP listener");
    }
    let socket = Arc::new(UdpSocket::bind(config.bedrock_listen).await?);
    let tcp = TcpListener::bind(config.bedrock_listen).await?;
    let tls = load_tls_acceptor(&config)?;
    let listener = TlsListener::new(tcp, tls);
    let routes = RouteRegistry::new();
    let signal_state = Arc::new(SignalProxy::new(config.clone(), routes.clone())?);
    let app = Router::new()
        .fallback(proxy_signal)
        .with_state(signal_state);
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SignalPeer>(),
    );

    tracing::info!(listen = %config.bedrock_listen,
        waiting_bedrock = %config.waiting_bedrock_address,
        waiting_signal = %config.waiting_signal_address,
        tls_certificate = %config.bedrock_tls_certificate.display(),
        "Bedrock RakNet and TLS signaling gateway listening");
    tokio::try_join!(
        relay(socket, SignalRoute::waiting(&config), routes),
        async {
            server
                .await
                .context("Bedrock TLS signaling listener stopped")
        },
    )?;
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

#[derive(Clone)]
struct SignalProxy {
    config: GatewayConfig,
    http: reqwest::Client,
    routes: RouteRegistry,
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
    fn new(config: GatewayConfig, routes: RouteRegistry) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            config,
            http,
            routes,
        })
    }

    async fn route(&self, hostname: &str) -> Result<SignalRoute, StatusCode> {
        let ci_address = is_ci_hostname(hostname);
        if ci_address {
            match crate::gateway::wake::request_ci_preview_admission(&self.config, hostname).await {
                Ok(crate::ci::CiPreviewState::Running) => {}
                Ok(_) => return Ok(SignalRoute::waiting(&self.config)),
                Err(error) => {
                    tracing::debug!(%hostname, ?error, "CI preview is not ready for Bedrock signaling");
                    return Ok(SignalRoute::waiting(&self.config));
                }
            }
        }
        let (url, expected_server_id, fallback_to_waiting) = if ci_address {
            (
                format!(
                    "{}/internal/bedrock-signal/ci/{}",
                    self.config.control_plane_url.trim_end_matches('/'),
                    percent_encode_path(hostname),
                ),
                None,
                true,
            )
        } else if let Some(server_id) =
            server_id_from_signal_hostname(hostname, &self.config.root_domain)
        {
            (
                format!(
                    "{}/internal/bedrock-signal/{server_id}",
                    self.config.control_plane_url.trim_end_matches('/')
                ),
                Some(server_id),
                false,
            )
        } else if let Some(subdomain) = subdomain_from_hostname(hostname, &self.config.root_domain)
        {
            (
                format!(
                    "{}/internal/bedrock-signal/by-subdomain/{subdomain}",
                    self.config.control_plane_url.trim_end_matches('/')
                ),
                None,
                true,
            )
        } else {
            return Err(StatusCode::NOT_FOUND);
        };

        let response = match self
            .http
            .get(url)
            .bearer_auth(&self.config.internal_token)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(%hostname, ?error, "Bedrock signal assignment lookup failed");
                return if fallback_to_waiting {
                    Ok(SignalRoute::waiting(&self.config))
                } else {
                    Err(StatusCode::SERVICE_UNAVAILABLE)
                };
            }
        };
        if !response.status().is_success() {
            tracing::debug!(%hostname, status = %response.status(),
                fallback_to_waiting, "Bedrock signal assignment is unavailable");
            return if fallback_to_waiting {
                Ok(SignalRoute::waiting(&self.config))
            } else {
                Err(StatusCode::SERVICE_UNAVAILABLE)
            };
        }
        let body = read_bounded_response_body(response, MAX_SIGNAL_ROUTE_BYTES).await?;
        let route: BedrockSignalTarget = serde_json::from_slice(&body).map_err(|error| {
            tracing::warn!(%hostname, ?error, "invalid Bedrock signal assignment response");
            StatusCode::BAD_GATEWAY
        })?;
        let expected_server_id = expected_server_id.unwrap_or(route.server_id);
        let mut target = validate_signal_target(&route, expected_server_id)?;
        target.hostname = Some(hostname.to_owned());
        tracing::debug!(%expected_server_id, generation = route.generation,
            worker = %route.worker_address, bedrock_port = route.bedrock_port,
            signal = %target.signal, bedrock = %target.bedrock,
            "resolved current Bedrock signal assignment");
        Ok(target)
    }

    #[cfg(test)]
    async fn target(&self, hostname: &str) -> Result<SocketAddr, StatusCode> {
        Ok(self.route(hostname).await?.signal)
    }
}

fn is_ci_hostname(hostname: &str) -> bool {
    crate::routing::ci_subdomain_from_hostname(hostname, crate::ci::CI_ROOT_DOMAIN).is_some()
}

fn percent_encode_path(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

fn validate_signal_target(
    route: &BedrockSignalTarget,
    expected_server_id: Uuid,
) -> Result<SignalRoute, StatusCode> {
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
    Ok(SignalRoute {
        server_id: Some(expected_server_id),
        generation: Some(route.generation),
        hostname: None,
        signal: SocketAddr::new(signal_ip, route.signal_port),
        bedrock: SocketAddr::new(worker_ip, route.bedrock_port),
    })
}

#[cfg(test)]
fn validate_signal_route(
    route: &BedrockSignalTarget,
    expected_server_id: Uuid,
) -> Result<SocketAddr, StatusCode> {
    Ok(validate_signal_target(route, expected_server_id)?.signal)
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
    let route = match proxy.route(&hostname).await {
        Ok(route) => route,
        Err(status) => return status.into_response(),
    };
    let backend_url = format!("http://{}{path}", route.signal);
    let mut request = proxy
        .http
        .request(method.clone(), backend_url)
        .header(header::ACCEPT, "application/json, application/sdp")
        .header(header::CACHE_CONTROL, "no-store");
    if !body.is_empty() {
        request = request
            .header(header::CONTENT_TYPE, "application/sdp")
            .body(body.clone());
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(signal = %route.signal, ?error, "Bedrock signal backend request failed");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let (status, content_type, response_body) = match read_bounded_signal_response(response).await {
        Ok(response) => response,
        Err(status) => return status.into_response(),
    };

    if method == Method::POST
        && status.is_success()
        && let Some(network_id) = path.strip_prefix("/v1/join/")
    {
        let mut ufrags = extract_ice_ufrags(&body);
        ufrags.extend(extract_ice_ufrags(&response_body));
        if !proxy
            .routes
            .register(peer.address.ip(), route, ufrags)
            .await
        {
            tracing::debug!(peer = %peer.address, network_id, "Bedrock signal route was not registered");
        }
    }

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

fn extract_ice_ufrags(sdp: &[u8]) -> Vec<String> {
    let Ok(sdp) = std::str::from_utf8(sdp) else {
        return Vec::new();
    };
    sdp.lines()
        .filter_map(|line| line.strip_prefix("a=ice-ufrag:"))
        .map(str::trim)
        .filter(|value| valid_ice_ufrag(value))
        .map(str::to_owned)
        .collect()
}

fn valid_ice_ufrag(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ICE_UFRAG_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b':' && byte != b'/' && byte != b'\\')
}

fn stun_username_tokens(packet: &[u8]) -> Vec<String> {
    if packet.len() < 20 || packet[0] & 0xc0 != 0 || packet.get(4..8) != Some(&STUN_MAGIC) {
        return Vec::new();
    }
    let message_length = u16::from_be_bytes([packet[2], packet[3]]) as usize;
    let message_end = 20usize.saturating_add(message_length);
    if message_end > packet.len() {
        return Vec::new();
    }
    let mut offset = 20;
    let mut tokens = Vec::new();
    while offset + 4 <= message_end {
        let attribute_type = u16::from_be_bytes([packet[offset], packet[offset + 1]]);
        let attribute_length =
            u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]) as usize;
        let value_start = offset + 4;
        let value_end = value_start.saturating_add(attribute_length);
        if value_end > message_end {
            return Vec::new();
        }
        if attribute_type == 0x0006
            && let Ok(username) = std::str::from_utf8(&packet[value_start..value_end])
        {
            tokens.extend(
                username
                    .split(':')
                    .map(str::trim)
                    .filter(|value| valid_ice_ufrag(value))
                    .map(str::to_owned),
            );
        }
        offset = value_start.saturating_add((attribute_length + 3) & !3);
    }
    tokens
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

async fn relay(public: Arc<UdpSocket>, waiting: SignalRoute, routes: RouteRegistry) -> Result<()> {
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
                let packet = &packet[..length];
                if !sessions.contains_key(&peer) {
                    let Some(route) = routes.route_for_packet(peer, packet).await.or_else(|| {
                        opens_session(packet).then_some(waiting.clone())
                    }) else {
                        // NetherNet must present an ICE username bound by the
                        // authenticated TLS signaling exchange. Unknown STUN,
                        // DTLS, and arbitrary UDP packets fail closed rather
                        // than being sent to another server's runtime.
                        continue;
                    };
                    if sessions.len() >= MAX_SESSIONS || !routes.is_current(&route).await {
                        continue;
                    }
                    let bind = if route.bedrock.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
                    let backend = Arc::new(UdpSocket::bind(bind).await?);
                    backend.connect(route.bedrock).await?;
                    next_id = next_id.wrapping_add(1);
                    let id = next_id;
                    let incoming = backend.clone();
                    let outgoing = public.clone();
                    let handle = replies.spawn(async move {
                        (peer, id, forward_replies(incoming, outgoing, peer).await)
                    });
                    sessions.insert(peer, Session {
                        id,
                        socket: backend,
                        route,
                        last_input: Instant::now(),
                        replies: handle,
                    });
                }
                let stale = if let Some(session) = sessions.get(&peer) {
                    !routes.is_current(&session.route).await
                } else {
                    true
                };
                if stale {
                    if let Some(session) = sessions.remove(&peer) {
                        session.replies.abort();
                    }
                    continue;
                }
                let session = sessions.get_mut(&peer).expect("session allocated");
                session.last_input = Instant::now();
                if let Err(error) = session.socket.send(packet).await {
                    tracing::debug!(%peer, target = %session.route.bedrock, ?error,
                        "Bedrock backend transport unavailable");
                    if let Some(session) = sessions.remove(&peer) { session.replies.abort(); }
                }
            }
            completed = replies.join_next(), if !replies.is_empty() => {
                if let Some(Ok((peer, id, result))) = completed {
                    if let Err(error) = result {
                        tracing::debug!(%peer, ?error, "Bedrock backend reply transport closed");
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
    use std::{path::PathBuf, time::Duration};

    use axum::Json;

    use super::*;
    use tokio_rustls::rustls::{ClientConfig, RootCertStore, pki_types::ServerName};

    fn open_request() -> Vec<u8> {
        let mut packet = vec![0x05];
        packet.extend(OFFLINE_MAGIC);
        packet.push(11);
        packet.resize(1200, 0);
        packet
    }

    fn direct_route(server_id: Uuid, generation: i64, bedrock: SocketAddr) -> SignalRoute {
        SignalRoute {
            server_id: Some(server_id),
            generation: Some(generation),
            hostname: None,
            signal: "127.0.0.1:46002".parse().unwrap(),
            bedrock,
        }
    }

    fn stun_request(username: &str) -> Vec<u8> {
        let username = username.as_bytes();
        let padded = (username.len() + 3) & !3;
        let mut packet = vec![
            0x00,
            0x01,
            0,
            0,
            STUN_MAGIC[0],
            STUN_MAGIC[1],
            STUN_MAGIC[2],
            STUN_MAGIC[3],
        ];
        packet.extend([0x11; 12]);
        packet[2..4].copy_from_slice(&(u16::try_from(4 + padded).unwrap()).to_be_bytes());
        packet.extend(0x0006_u16.to_be_bytes());
        packet.extend(u16::try_from(username.len()).unwrap().to_be_bytes());
        packet.extend(username);
        packet.resize(packet.len() + padded - username.len(), 0);
        packet
    }

    fn gateway_config(control_plane_url: String) -> GatewayConfig {
        GatewayConfig {
            common: crate::config::CommonConfig {
                database_url: String::new(),
            },
            java_listen: "127.0.0.1:0".parse().unwrap(),
            java_public_port: 25565,
            bedrock_listen: "127.0.0.1:0".parse().unwrap(),
            bedrock_public_port: 19132,
            bedrock_public_address: "127.0.0.1".parse().unwrap(),
            bedrock_tls_certificate: PathBuf::new(),
            bedrock_tls_private_key: PathBuf::new(),
            waiting_java_address: "127.0.0.1:25566".parse().unwrap(),
            waiting_bedrock_address: "127.0.0.1:19133".parse().unwrap(),
            waiting_signal_address: "127.0.0.1:19134".parse().unwrap(),
            waiting_control_socket: PathBuf::new(),
            waiting_control_token: "test".into(),
            control_plane_url,
            internal_token: "signal-test-token".into(),
            root_domain: "mc.example.test".into(),
            wake_timeout: Duration::from_secs(90),
            allow_offline_waiting_for_tests: false,
        }
    }

    async fn signal_request_status(
        proxy: Arc<SignalProxy>,
        server_name: Option<String>,
    ) -> StatusCode {
        let hostname = "s-00000000-0000-0000-0000-000000000001.mc.example.test";
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, format!("{hostname}:19132").parse().unwrap());
        proxy_signal(
            State(proxy),
            ConnectInfo(SignalPeer {
                address: "127.0.0.1:50000".parse().unwrap(),
                server_name,
            }),
            headers,
            Method::GET,
            Uri::from_static("/v1/join"),
            Body::empty(),
        )
        .await
        .status()
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
        let valid = BedrockSignalTarget {
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
            BedrockSignalTarget {
                generation: 0,
                ..valid.clone()
            },
            BedrockSignalTarget {
                server_id: Uuid::new_v4(),
                ..valid.clone()
            },
            BedrockSignalTarget {
                signal_address: "8.8.8.8".into(),
                ..valid.clone()
            },
            BedrockSignalTarget {
                signal_address: "0.0.0.0".into(),
                ..valid.clone()
            },
            BedrockSignalTarget {
                signal_port: 1024,
                ..valid.clone()
            },
            BedrockSignalTarget {
                bedrock_port: 1024,
                ..valid.clone()
            },
            BedrockSignalTarget {
                bedrock_port: valid.signal_port,
                ..valid.clone()
            },
            BedrockSignalTarget {
                worker_address: "0.0.0.0".into(),
                ..valid.clone()
            },
            BedrockSignalTarget {
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
    async fn signaling_rejects_missing_or_mismatched_sni_before_assignment_lookup() {
        let proxy = Arc::new(
            SignalProxy::new(
                gateway_config("http://127.0.0.1:0".into()),
                RouteRegistry::new(),
            )
            .unwrap(),
        );
        assert_eq!(
            signal_request_status(proxy.clone(), Some("other.mc.example.test".into()),).await,
            StatusCode::MISDIRECTED_REQUEST
        );
        assert_eq!(
            signal_request_status(proxy.clone(), None).await,
            StatusCode::MISDIRECTED_REQUEST
        );
        assert_eq!(
            signal_request_status(
                proxy,
                Some("s-00000000-0000-0000-0000-000000000001.mc.example.test".into()),
            )
            .await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn waiting_host_signal_uses_its_separate_backend_listener() {
        let proxy = SignalProxy::new(
            gateway_config("http://127.0.0.1:8080".into()),
            RouteRegistry::new(),
        )
        .unwrap();

        assert_eq!(
            proxy.target("waiting.mc.example.test").await.unwrap(),
            "127.0.0.1:19134".parse::<SocketAddr>().unwrap()
        );
    }

    #[tokio::test]
    async fn signal_assignment_decodes_the_shared_contract_and_validates_endpoint() {
        use axum::{extract::Path, routing::get};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/internal/bedrock-signal/{server_id}",
            get(|Path(server_id): Path<Uuid>| async move {
                Json(BedrockSignalTarget {
                    server_id,
                    worker_address: "192.0.2.20".into(),
                    bedrock_port: 46001,
                    signal_address: "127.0.0.1".into(),
                    signal_port: 46002,
                    generation: 3,
                })
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let proxy = SignalProxy::new(
            gateway_config(format!("http://{address}")),
            RouteRegistry::new(),
        )
        .unwrap();
        let server_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let hostname = format!("s-{server_id}.mc.example.test");

        assert_eq!(
            proxy.target(&hostname).await.unwrap(),
            "127.0.0.1:46002".parse::<SocketAddr>().unwrap()
        );

        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn normal_server_hostname_resolves_direct_signal_assignment() {
        use axum::{extract::Path, routing::get};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_id = Uuid::new_v4();
        let app = Router::new().route(
            "/internal/bedrock-signal/by-subdomain/{subdomain}",
            get(move |Path(subdomain): Path<String>| {
                let server_id = server_id;
                async move {
                    assert_eq!(subdomain, "friends");
                    Json(BedrockSignalTarget {
                        server_id,
                        worker_address: "192.0.2.20".into(),
                        bedrock_port: 46001,
                        signal_address: "127.0.0.1".into(),
                        signal_port: 46002,
                        generation: 3,
                    })
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let proxy = SignalProxy::new(
            gateway_config(format!("http://{address}")),
            RouteRegistry::new(),
        )
        .unwrap();

        let route = proxy
            .route("friends.mc.example.test")
            .await
            .expect("normal server hostname should resolve directly");
        assert_eq!(route.server_id, Some(server_id));
        assert_eq!(route.generation, Some(3));
        assert_eq!(route.signal, "127.0.0.1:46002".parse().unwrap());
        assert_eq!(route.bedrock, "192.0.2.20:46001".parse().unwrap());
        assert_eq!(route.hostname.as_deref(), Some("friends.mc.example.test"));

        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn tls_listener_extracts_negotiated_sni_for_the_host_authority_gate() {
        use std::io::BufReader;

        use tokio_rustls::TlsConnector;

        let mut config = gateway_config("http://127.0.0.1:0".into());
        config.bedrock_tls_certificate = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/bedrock-gateway-test-cert.pem");
        config.bedrock_tls_private_key = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/bedrock-gateway-test-key.pem");
        let tls_acceptor = load_tls_acceptor(&config).unwrap();
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = tcp.local_addr().unwrap();
        let mut listener = TlsListener::new(tcp, tls_acceptor);
        let accepted = tokio::spawn(async move {
            let (_connection, peer) = listener.accept().await;
            peer
        });

        let mut certificate_reader = BufReader::new(
            include_bytes!("../../tests/fixtures/bedrock-gateway-test-cert.pem").as_slice(),
        );
        let certificate = rustls_pemfile::certs(&mut certificate_reader)
            .next()
            .expect("test certificate")
            .unwrap();
        let mut roots = RootCertStore::empty();
        roots.add(certificate).unwrap();
        let client_config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(client_config));
        let hostname = "s-00000000-0000-0000-0000-000000000001.mc.example.test";
        let stream = TcpStream::connect(address).await.unwrap();
        let _client = connector
            .connect(ServerName::try_from(hostname.to_owned()).unwrap(), stream)
            .await
            .unwrap();

        let peer = tokio::time::timeout(Duration::from_secs(2), accepted)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(peer.server_name.as_deref(), Some(hostname));
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
        let waiting_route = SignalRoute {
            server_id: None,
            generation: None,
            hostname: None,
            signal: waiting.local_addr().unwrap(),
            bedrock: waiting.local_addr().unwrap(),
        };
        let task = tokio::spawn(relay(public, waiting_route, RouteRegistry::new()));
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

    #[tokio::test]
    async fn signal_route_registry_uses_ice_credentials_and_fences_generations() {
        let registry = RouteRegistry::new();
        let server_id = Uuid::new_v4();
        let old_route = direct_route(server_id, 2, "127.0.0.1:46001".parse().unwrap());
        let current_route = direct_route(server_id, 3, "127.0.0.1:46003".parse().unwrap());
        assert!(
            registry
                .register(
                    "127.0.0.1".parse().unwrap(),
                    old_route.clone(),
                    vec!["old-client".into(), "old-server".into()]
                )
                .await
        );
        assert!(
            registry
                .register(
                    "127.0.0.1".parse().unwrap(),
                    current_route.clone(),
                    vec!["new-client".into(), "new-server".into()]
                )
                .await
        );
        assert_eq!(
            registry
                .route_for_packet(
                    "127.0.0.1:50000".parse().unwrap(),
                    &stun_request("old-server:old-client")
                )
                .await,
            None
        );
        assert_eq!(
            registry
                .route_for_packet(
                    "127.0.0.1:50000".parse().unwrap(),
                    &stun_request("new-server:new-client")
                )
                .await,
            Some(current_route.clone())
        );
        assert_eq!(
            registry
                .route_for_packet(
                    "127.0.0.2:50000".parse().unwrap(),
                    &stun_request("new-server:new-client")
                )
                .await,
            None
        );
        assert!(
            !registry
                .register(
                    "127.0.0.1".parse().unwrap(),
                    old_route,
                    vec!["stale-client".into()]
                )
                .await
        );
        assert_eq!(
            registry
                .route_for_packet(
                    "127.0.0.1:50000".parse().unwrap(),
                    &stun_request("stale-client")
                )
                .await,
            None
        );
    }

    #[tokio::test]
    async fn active_udp_session_stops_forwarding_after_newer_generation_is_observed() {
        let old_worker = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let replacement_worker = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let public = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let registry = RouteRegistry::new();
        let server_id = Uuid::new_v4();
        let old_route = direct_route(server_id, 4, old_worker.local_addr().unwrap());
        let newer_route = direct_route(server_id, 5, replacement_worker.local_addr().unwrap());
        assert!(
            registry
                .register(
                    "127.0.0.1".parse().unwrap(),
                    old_route,
                    vec!["old-active".into()]
                )
                .await
        );
        let waiting_route = SignalRoute {
            server_id: None,
            generation: None,
            hostname: None,
            signal: replacement_worker.local_addr().unwrap(),
            bedrock: replacement_worker.local_addr().unwrap(),
        };
        let public_address = public.local_addr().unwrap();
        let relay_task = tokio::spawn(relay(public, waiting_route, registry.clone()));
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client
            .send_to(&stun_request("server:old-active"), public_address)
            .await
            .unwrap();
        let mut buffer = [0_u8; 1500];
        let (length, _) =
            tokio::time::timeout(Duration::from_secs(2), old_worker.recv_from(&mut buffer))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(&buffer[..length], stun_request("server:old-active"));

        assert!(
            registry
                .register(
                    "127.0.0.1".parse().unwrap(),
                    newer_route,
                    vec!["new-active".into()]
                )
                .await
        );
        // Once the relay observes the replacement generation, the existing
        // client endpoint must not keep sending through its stale socket or be
        // silently migrated to the replacement worker.
        client.send_to(&[0x80, 0x44], public_address).await.unwrap();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                old_worker.recv_from(&mut buffer)
            )
            .await
            .is_err()
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                replacement_worker.recv_from(&mut buffer)
            )
            .await
            .is_err()
        );

        relay_task.abort();
        assert!(relay_task.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn active_route_refreshes_its_idle_lease() {
        let registry = RouteRegistry::new();
        let route = direct_route(Uuid::new_v4(), 1, "127.0.0.1:46001".parse().unwrap());
        assert!(
            registry
                .register(
                    "127.0.0.1".parse().unwrap(),
                    route.clone(),
                    vec!["active".into()]
                )
                .await
        );
        {
            let mut state = registry.state.lock().await;
            state.by_ufrag.get_mut("active").unwrap().expires =
                Instant::now() - Duration::from_secs(1);
        }
        assert!(registry.is_current(&route).await);
        let state = registry.state.lock().await;
        assert!(state.by_ufrag["active"].expires > Instant::now());
    }

    #[tokio::test]
    async fn signal_route_registry_expires_unmatched_ice_credentials() {
        let registry = RouteRegistry::new();
        let route = direct_route(Uuid::new_v4(), 1, "127.0.0.1:46001".parse().unwrap());
        assert!(
            registry
                .register(
                    "127.0.0.1".parse().unwrap(),
                    route.clone(),
                    vec!["expiring".into()]
                )
                .await
        );
        {
            let mut state = registry.state.lock().await;
            state.by_ufrag.get_mut("expiring").unwrap().expires =
                Instant::now() - Duration::from_secs(1);
        }
        assert_eq!(
            registry
                .route_for_packet(
                    "127.0.0.1:50000".parse().unwrap(),
                    &stun_request("expiring")
                )
                .await,
            None
        );
        assert!(!registry.is_current(&route).await);
    }

    #[tokio::test]
    async fn relay_forwards_a_signaled_route_to_the_assigned_worker() {
        let worker = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let waiting = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let public = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let registry = RouteRegistry::new();
        let route = SignalRoute {
            server_id: Some(Uuid::new_v4()),
            generation: Some(1),
            hostname: None,
            signal: "127.0.0.1:46002".parse().unwrap(),
            bedrock: worker.local_addr().unwrap(),
        };
        assert!(
            registry
                .register(
                    "127.0.0.1".parse().unwrap(),
                    route.clone(),
                    vec!["worker".into()]
                )
                .await
        );
        let waiting_route = SignalRoute {
            server_id: None,
            generation: None,
            hostname: None,
            signal: waiting.local_addr().unwrap(),
            bedrock: waiting.local_addr().unwrap(),
        };
        let public_address = public.local_addr().unwrap();
        let task = tokio::spawn(relay(public, waiting_route, registry));
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let request = stun_request("server:worker");
        client.send_to(&request, public_address).await.unwrap();
        let mut buffer = [0_u8; 1500];
        let (length, worker_peer) =
            tokio::time::timeout(Duration::from_secs(2), worker.recv_from(&mut buffer))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(&buffer[..length], request);
        let response = [0x80, 0x01, 0x00, 0x00];
        worker.send_to(&response, worker_peer).await.unwrap();
        let (length, _) =
            tokio::time::timeout(Duration::from_secs(2), client.recv_from(&mut buffer))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(&buffer[..length], response);
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

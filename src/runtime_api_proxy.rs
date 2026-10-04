//! Narrow HTTP relay for managed Pumpkin plugins.
//!
//! Workload containers stay on an internal, non-egress Docker network.  A
//! per-runtime proxy sidecar is the only container attached to a second
//! egress network.  This process forwards only the server-scoped (or CI
//! preview-scoped) plugin routes below, and forwards the plugin capability
//! unchanged to the control plane.  It never receives a worker, gateway, or
//! database credential.

use std::{env, net::SocketAddr, process::Stdio, time::Duration};

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
    time::{sleep, timeout},
};
use uuid::Uuid;

pub const SERVICE_HOSTNAME: &str = "kurbis-runtime-api-proxy";
pub const SERVICE_PORT: u16 = 8080;
pub const SERVICE_BASE_URL: &str = "http://kurbis-runtime-api-proxy:8080";

const MAX_REQUEST_BYTES: usize = 128 * 1024;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(10);
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
}

/// Start a capability-bound proxy attached to exactly one runtime network and
/// the configured egress network. The workload itself is never attached to the
/// egress network.
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
                "--user",
                "65532:65532",
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
                "20",
                "--health-start-period",
                "2s",
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

    let health_result = timeout(Duration::from_secs(30), async {
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
            Err(error).context("runtime API proxy did not become healthy within 30 seconds")
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
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    ensure!(
        driver == Some("bridge") && !ipv6,
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
        "runtime API proxy is unhealthy"
    );
    Ok(())
}

pub async fn run_from_env() -> Result<()> {
    let config = ProxyConfig::from_env()?;
    let client = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .timeout(UPSTREAM_TIMEOUT)
        .build()
        .context("build runtime API proxy HTTP client")?;
    let state = ProxyState { config, client };
    let listener = TcpListener::bind(state.config.listen)
        .await
        .context("bind runtime API proxy listener")?;
    tracing::info!(listen = %state.config.listen, upstream = %state.config.upstream, scope = ?state.config.scope, "runtime API proxy ready");
    axum::serve(listener, proxy_router(state))
        .await
        .context("runtime API proxy server failed")
}

fn proxy_router(state: ProxyState) -> Router {
    Router::new()
        .route("/", any(handle))
        .fallback(handle)
        .with_state(state)
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
        return Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .expect("static health response is valid");
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
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::net::TcpListener;

    fn id() -> Uuid {
        Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap()
    }

    #[test]
    fn normal_scope_allows_only_server_capability_routes() {
        let expected = id();
        for (method, path) in [
            (Method::GET, format!("/api/v1/servers/{expected}")),
            (Method::PATCH, format!("/api/v1/servers/{expected}")),
            (Method::GET, format!("/api/v1/servers/{expected}/players")),
            (Method::GET, format!("/api/v1/servers/{expected}/logs")),
            (Method::GET, format!("/api/v1/servers/{expected}/activity")),
            (Method::GET, format!("/api/v1/servers/{expected}/snapshots")),
            (Method::GET, format!("/api/v1/servers/{expected}/whitelist")),
            (
                Method::POST,
                format!("/api/v1/servers/{expected}/whitelist"),
            ),
            (
                Method::DELETE,
                format!("/api/v1/servers/{expected}/whitelist"),
            ),
            (Method::POST, format!("/api/v1/servers/{expected}/commands")),
            (Method::POST, "/api/v1/server/login-links".to_owned()),
        ] {
            assert!(
                route_allowed(Scope::Server, &method, &path, expected),
                "{method} {path}"
            );
        }
    }

    #[test]
    fn normal_scope_denies_sibling_account_and_worker_routes() {
        let expected = id();
        for (method, path) in [
            (
                Method::GET,
                format!("/api/v1/servers/{}/players", Uuid::new_v4()),
            ),
            (Method::GET, "/api/v1/account".to_owned()),
            (Method::GET, format!("/api/v1/servers/{expected}/exports")),
            (
                Method::POST,
                "/internal/worker/control/heartbeat".to_owned(),
            ),
            (Method::GET, format!("/internal/ci/previews/{expected}")),
            (Method::GET, format!("/api/v1/servers/{expected}/operators")),
        ] {
            assert!(
                !route_allowed(Scope::Server, &method, &path, expected),
                "{method} {path}"
            );
        }
    }

    #[test]
    fn ci_scope_is_preview_only_and_cannot_reach_account_routes() {
        let expected = id();
        assert!(route_allowed(
            Scope::Ci,
            &Method::GET,
            &format!("/internal/ci/previews/{expected}"),
            expected
        ));
        assert!(route_allowed(
            Scope::Ci,
            &Method::PATCH,
            &format!("/internal/ci/previews/{expected}"),
            expected
        ));
        assert!(route_allowed(
            Scope::Ci,
            &Method::GET,
            &format!("/internal/ci/previews/{expected}/players"),
            expected
        ));
        assert!(route_allowed(
            Scope::Ci,
            &Method::POST,
            &format!("/internal/ci/previews/{expected}/commands"),
            expected
        ));
        assert!(!route_allowed(
            Scope::Ci,
            &Method::GET,
            "/api/v1/account",
            expected
        ));
        assert!(!route_allowed(
            Scope::Ci,
            &Method::GET,
            &format!("/api/v1/servers/{expected}"),
            expected
        ));
        assert!(!route_allowed(
            Scope::Ci,
            &Method::GET,
            &format!("/internal/ci/previews/{}/players", Uuid::new_v4()),
            expected
        ));
    }

    #[tokio::test]
    async fn capability_proxy_forwards_allowed_request_and_blocks_scope_escape() {
        async fn upstream(
            State(count): State<Arc<AtomicUsize>>,
            request: Request<Body>,
        ) -> Response {
            count.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.method(), Method::GET);
            assert_eq!(
                request.headers().get(header::AUTHORIZATION).unwrap(),
                "Bearer AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            );
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"players":[]}"#))
                .unwrap()
        }

        let forwarded = Arc::new(AtomicUsize::new(0));
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_address = upstream_listener.local_addr().unwrap();
        let count = forwarded.clone();
        tokio::spawn(async move {
            axum::serve(
                upstream_listener,
                Router::new().fallback(upstream).with_state(count),
            )
            .await
            .unwrap();
        });

        let expected_id = id();
        let token = "A".repeat(43);
        let config = ProxyConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            upstream: Url::parse(&format!("http://{upstream_address}")).unwrap(),
            scope: Scope::Server,
            expected_id,
            expected_capability_hash: *blake3::hash(token.as_bytes()).as_bytes(),
        };
        let proxy_listener = TcpListener::bind(config.listen).await.unwrap();
        let proxy_address = proxy_listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            axum::serve(
                proxy_listener,
                proxy_router(ProxyState {
                    config,
                    client: Client::builder().redirect(Policy::none()).build().unwrap(),
                }),
            )
            .await
            .unwrap();
        });
        let client = reqwest::Client::new();
        let own_url = format!("http://{proxy_address}/api/v1/servers/{expected_id}/players");
        let own = client
            .get(&own_url)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(own.status(), StatusCode::OK);
        assert_eq!(
            own.json::<serde_json::Value>().await.unwrap(),
            serde_json::json!({"players": []}),
        );
        assert_eq!(forwarded.load(Ordering::SeqCst), 1);

        for path in [
            "/api/v1/account".to_owned(),
            format!("/api/v1/servers/{}/players", Uuid::new_v4()),
            "/internal/worker/control/heartbeat".to_owned(),
        ] {
            let response = client
                .get(format!("http://{proxy_address}{path}"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
        let wrong_capability = client
            .get(&own_url)
            .bearer_auth("B".repeat(43))
            .send()
            .await
            .unwrap();
        assert_eq!(wrong_capability.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            forwarded.load(Ordering::SeqCst),
            1,
            "denied requests never reach the control plane"
        );
        proxy_task.abort();
    }

    #[test]
    fn proxy_egress_requires_ipv4_bridge_and_remote_https() {
        let internal = serde_json::json!({
            "Driver": "bridge",
            "Internal": true,
            "EnableIPv6": false,
        });
        assert_eq!(validate_egress_network_metadata(&internal).unwrap(), true);
        let mut external = internal;
        external["Internal"] = serde_json::Value::Bool(false);
        assert_eq!(validate_egress_network_metadata(&external).unwrap(), false);
        assert!(validate_upstream_url("https://control.example", false).is_ok());
        assert!(validate_upstream_url("http://control.example", true).is_err());
        for invalid in [
            serde_json::json!({"Driver":"bridge", "Internal":true, "EnableIPv6":true}),
            serde_json::json!({"Driver":"overlay", "Internal":true, "EnableIPv6":false}),
        ] {
            assert!(validate_egress_network_metadata(&invalid).is_err());
        }
    }

    #[test]
    fn origin_and_upstream_validation_are_strict() {
        let base = Url::parse("https://control.example").unwrap();
        assert_eq!(
            upstream_url(&base, "/api/v1/account", Some("x=1"))
                .unwrap()
                .as_str(),
            "https://control.example/api/v1/account?x=1"
        );
        assert!(validate_upstream_url("https://control.example", false).is_ok());
        assert!(validate_upstream_url("http://localhost:8080", false).is_err());
        assert!(validate_upstream_url("http://172.18.0.1:8080", true).is_ok());
        assert!(validate_upstream_url("http://control-plane:8080", true).is_ok());
        assert!(validate_upstream_url("http://control.example", true).is_err());
        assert!(validate_upstream_url("https://control.example/prefix", false).is_err());
        assert!(validate_upstream_url("https://user@control.example", false).is_err());
        assert!(hex_decode_32(&"a".repeat(64)).is_ok());
        assert!(hex_decode_32("not-a-hash").is_err());
    }
}

//! Private worker-local HTTP relay for managed Pumpkin plugin APIs.
//!
//! The rootless Docker network is intentionally isolated from the rootful
//! Dokploy Compose network. Proxy sidecars therefore reach the control plane
//! through this Unix-domain socket, not Docker DNS, host networking, or a
//! public listener. Only API-proxy containers receive a read-only bind mount
//! of the relay directory; Pumpkin workloads do not.

use std::{
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::State,
    http::{Method, Request, StatusCode, header},
    response::Response,
    routing::any,
};
use futures::StreamExt;
use reqwest::{Client, Url, redirect::Policy};
use tokio::{net::UnixListener, task::JoinHandle, time::timeout};
use uuid::Uuid;

use crate::{
    runtime_api_proxy::{
        PROXY_ID_HEADER, PROXY_SCOPE_HEADER, Scope as ProxyScope, route_allowed, valid_token,
    },
    worker::control_plane::validate_control_plane_url,
};

const RELAY_SOCKET_FILENAME: &str = "relay.sock";
const RELAY_CONTAINER_DIRECTORY: &str = "/run/kurbis-runtime-api";
const MAX_REQUEST_BYTES: usize = 128 * 1024;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const SOCKET_START_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct WorkerRuntimeApiRelay {
    inner: Arc<RelayInner>,
}

struct RelayInner {
    socket_path: PathBuf,
    task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone)]
struct RelayState {
    upstream: Url,
    client: Client,
}

impl WorkerRuntimeApiRelay {
    /// Start the one worker-local listener. Its parent is the rootless Docker
    /// socket directory so the daemon can mount it, while the parent runtime
    /// directory remains private to that unprivileged daemon account.
    pub async fn start(
        docker_socket: &Path,
        upstream: &str,
        allow_insecure_local_http: bool,
    ) -> Result<Self> {
        ensure!(
            docker_socket.is_absolute(),
            "rootless Docker socket path must be absolute"
        );
        let upstream = validate_control_plane_url(upstream, allow_insecure_local_http)?;
        let parent = docker_socket
            .parent()
            .context("rootless Docker socket has no parent directory")?;
        let socket_directory = parent.join("kurbis-runtime-api");
        ensure_private_socket_directory(&socket_directory, docker_socket).await?;
        let socket_path = socket_directory.join(RELAY_SOCKET_FILENAME);
        remove_stale_socket(&socket_path)?;

        let listener = UnixListener::bind(&socket_path).with_context(|| {
            format!(
                "bind worker runtime API relay socket {}",
                socket_path.display()
            )
        })?;
        set_socket_permissions(&socket_path)?;
        let state = RelayState {
            upstream: upstream.clone(),
            client: Client::builder()
                .no_proxy()
                .redirect(Policy::none())
                .timeout(REQUEST_TIMEOUT)
                .build()
                .context("build worker runtime API relay client")?,
        };
        let task = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, relay_router(state)).await {
                tracing::error!(?error, "worker runtime API relay stopped");
            }
        });
        let relay = Self {
            inner: Arc::new(RelayInner {
                socket_path,
                task: tokio::sync::Mutex::new(Some(task)),
            }),
        };
        relay
            .check_health()
            .await
            .context("verify worker-local managed-plugin API relay")?;
        tracing::info!(
            socket = %relay.socket_path().display(),
            upstream = %upstream,
            "worker-local managed-plugin API relay is ready"
        );
        Ok(relay)
    }

    pub fn socket_path(&self) -> &Path {
        &self.inner.socket_path
    }

    pub fn container_socket_path() -> &'static str {
        // The directory is bind-mounted read-only into proxy sidecars. Mounting
        // the directory (rather than a socket inode) lets a worker process
        // restart safely replace a stale socket while existing proxies see the
        // new listener at the same path.
        " /run/kurbis-runtime-api/relay.sock".trim()
    }

    pub fn container_directory(&self) -> &'static str {
        RELAY_CONTAINER_DIRECTORY
    }

    pub async fn check_health(&self) -> Result<()> {
        let response = timeout(
            SOCKET_START_TIMEOUT,
            Client::builder()
                .unix_socket(self.inner.socket_path.clone())
                .no_proxy()
                .redirect(Policy::none())
                .timeout(SOCKET_START_TIMEOUT)
                .build()?
                .get("http://runtime-api-relay/healthz")
                .send(),
        )
        .await
        .context("worker runtime API relay health check timed out")??;
        ensure!(
            response.status() == StatusCode::NO_CONTENT,
            "worker runtime API relay returned unexpected health status {}",
            response.status()
        );
        Ok(())
    }
}

impl Drop for RelayInner {
    fn drop(&mut self) {
        if let Some(task) = self.task.get_mut().take() {
            task.abort();
        }
        if let Ok(metadata) = fs::symlink_metadata(&self.socket_path)
            && metadata.file_type().is_socket()
        {
            let _ = fs::remove_file(&self.socket_path);
        }
    }
}

async fn ensure_private_socket_directory(directory: &Path, docker_socket: &Path) -> Result<()> {
    let socket_metadata = tokio::fs::metadata(docker_socket)
        .await
        .context("inspect rootless Docker socket for API relay permissions")?;
    ensure!(
        socket_metadata.file_type().is_socket() && socket_metadata.uid() != 0,
        "runtime API relay requires a non-root-owned rootless Docker socket"
    );
    let daemon_uid = socket_metadata.uid();
    let mut passwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut buffer = vec![0u8; 16 * 1024];
    let mut result = std::ptr::null_mut();
    // SAFETY: getpwuid_r initializes passwd/result on success and buffer lives
    // through the copy of the primary group below.
    let status = unsafe {
        libc::getpwuid_r(
            daemon_uid,
            passwd.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    ensure!(
        status == 0 && !result.is_null(),
        "cannot resolve the rootless Docker socket owner's primary group"
    );
    // SAFETY: getpwuid_r returned a non-null pointer to initialized passwd.
    let daemon_gid = unsafe { passwd.assume_init().pw_gid };

    if let Ok(metadata) = fs::symlink_metadata(directory) {
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "worker runtime API relay directory must be a real directory"
        );
        ensure!(
            metadata.uid() == daemon_uid && metadata.gid() == daemon_gid,
            "worker runtime API relay directory has unexpected ownership"
        );
    } else {
        fs::create_dir(directory).with_context(|| {
            format!("create runtime API relay directory {}", directory.display())
        })?;
        chown_path(directory, daemon_uid, daemon_gid)?;
    }
    // The owning rootless daemon user can mount and update the socket. Other
    // host users cannot enter its /run/user/<uid> parent. Sidecars receive only
    // this read-only directory mount; execute-only access is enough to connect
    // to the world-writable socket inode as the dropped proxy uid.
    fs::set_permissions(directory, fs::Permissions::from_mode(0o711))?;
    Ok(())
}

fn set_socket_permissions(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect runtime API relay socket {}", path.display()))?;
    ensure!(
        metadata.file_type().is_socket(),
        "worker runtime API relay path is not a Unix socket"
    );
    // The socket directory is private to the rootless Docker account and the
    // proxy is the only container granted the read-only mount. World access on
    // the socket inode is required because the proxy irreversibly drops to its
    // unprivileged in-container uid before making requests.
    chown_path(path, metadata.uid(), metadata.gid())?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o666))?;
    Ok(())
}

fn remove_stale_socket(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            fs::remove_file(path)
                .with_context(|| format!("remove stale runtime API socket {}", path.display()))?;
        }
        Ok(_) => bail!(
            "refusing to replace non-socket runtime API path {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect stale runtime API relay socket"),
    }
    Ok(())
}

#[cfg(unix)]
fn chown_path(path: &Path, uid: u32, gid: u32) -> Result<()> {
    use std::ffi::CString;
    let path = CString::new(path.as_os_str().as_bytes())
        .context("runtime API relay path contains a NUL byte")?;
    // SAFETY: path is a valid NUL-terminated C string and chown does not retain it.
    let result = unsafe { libc::chown(path.as_ptr(), uid, gid) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()).context("set runtime API relay ownership")
    }
}

fn relay_router(state: RelayState) -> Router {
    Router::new()
        .route("/", any(handle_request))
        .fallback(any(handle_request))
        .with_state(state)
}

async fn handle_request(State(state): State<RelayState>, request: Request<Body>) -> Response {
    if request.method() == Method::GET
        && request.uri().path() == "/healthz"
        && request.uri().query().is_none()
    {
        return match state
            .client
            .get(state.upstream.join("/healthz").expect("valid path"))
            .send()
            .await
        {
            Ok(result) if result.status() == StatusCode::NO_CONTENT => {
                empty_response(StatusCode::NO_CONTENT)
            }
            _ => empty_response(StatusCode::SERVICE_UNAVAILABLE),
        };
    }
    if request.uri().authority().is_some() {
        return empty_response(StatusCode::NOT_FOUND);
    }

    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let Some(scope) = request
        .headers()
        .get(PROXY_SCOPE_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| ProxyScope::parse(value).ok())
    else {
        return empty_response(StatusCode::NOT_FOUND);
    };
    let Some(expected_id) = request
        .headers()
        .get(PROXY_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Uuid::parse_str(value).ok())
    else {
        return empty_response(StatusCode::NOT_FOUND);
    };
    if !route_allowed(scope, &method, &path, expected_id) {
        return empty_response(StatusCode::NOT_FOUND);
    }
    let Some(token) = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|token| valid_token(token))
        .map(str::to_owned)
    else {
        return empty_response(StatusCode::UNAUTHORIZED);
    };
    if [header::CONTENT_TYPE, header::ACCEPT]
        .into_iter()
        .any(|name| request.headers().get_all(name).iter().count() > 1)
    {
        return empty_response(StatusCode::BAD_REQUEST);
    }

    let query = request.uri().query().map(str::to_owned);
    let target = match upstream_url(&state.upstream, &path, query.as_deref()) {
        Ok(target) => target,
        Err(_) => return empty_response(StatusCode::BAD_GATEWAY),
    };
    let incoming_headers = request.headers().clone();
    let body = match to_bytes(request.into_body(), MAX_REQUEST_BYTES).await {
        Ok(body) => body,
        Err(_) => return empty_response(StatusCode::PAYLOAD_TOO_LARGE),
    };
    let mut outgoing = state
        .client
        .request(method, target)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    for name in [header::CONTENT_TYPE, header::ACCEPT] {
        if let Some(value) = incoming_headers.get(&name) {
            outgoing = outgoing.header(name, value);
        }
    }
    match outgoing.body(body).send().await {
        Ok(upstream) => {
            let status_code = upstream.status();
            let source_headers = upstream.headers().clone();
            let mut stream = upstream.bytes_stream();
            let mut bytes = Vec::with_capacity(MAX_RESPONSE_BYTES.min(16 * 1024));
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(chunk) if bytes.len().saturating_add(chunk.len()) <= MAX_RESPONSE_BYTES => {
                        bytes.extend_from_slice(&chunk);
                    }
                    _ => return empty_response(StatusCode::BAD_GATEWAY),
                }
            }
            let mut result = empty_response(status_code);
            for name in [header::CONTENT_TYPE, header::CACHE_CONTROL] {
                if let Some(value) = source_headers.get(&name) {
                    result.headers_mut().insert(name, value.clone());
                }
            }
            *result.body_mut() = Body::from(Bytes::from(bytes));
            result
        }
        Err(_) => empty_response(StatusCode::BAD_GATEWAY),
    }
}

fn upstream_url(base: &Url, path: &str, query: Option<&str>) -> Result<Url> {
    let mut url = base.clone();
    url.set_path(path);
    url.set_query(query);
    Ok(url)
}

fn empty_response(status: StatusCode) -> Response {
    Response::builder()
        .status(status)
        .body(Body::empty())
        .expect("static status response is valid")
}

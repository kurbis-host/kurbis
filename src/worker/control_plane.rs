use std::{borrow::Cow, net::IpAddr, str::FromStr};

use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, Method, RequestBuilder, Url, header, redirect::Policy};
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

mod api;

use crate::contracts::worker::WorkerAssignment;

const MAX_WORKER_ERROR_BYTES: usize = 1_000;

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EmptyRequest {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationRetryOutcome {
    Scheduled,
    Exhausted,
    NoLongerCurrent,
}

#[derive(Debug, Clone)]
pub struct WorkerControl {
    base_url: Url,
    client: Client,
    authorization: header::HeaderValue,
    lease_id: std::sync::Arc<std::sync::RwLock<Option<Uuid>>>,
}

impl WorkerControl {
    pub fn new(
        control_plane_url: &str,
        api_token: &str,
        allow_insecure_local_http: bool,
    ) -> Result<Self> {
        let base_url = validate_control_plane_url(control_plane_url, allow_insecure_local_http)?;
        ensure!(
            api_token.len() == 43
                && api_token
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
            "worker API token has an invalid format"
        );
        let mut authorization = header::HeaderValue::from_str(&format!("Bearer {api_token}"))
            .context("worker API token is not a valid header value")?;
        authorization.set_sensitive(true);
        Ok(Self {
            base_url,
            client: Client::builder().redirect(Policy::none()).build()?,
            authorization,
            lease_id: std::sync::Arc::new(std::sync::RwLock::new(None)),
        })
    }

    pub fn new_with_lease(
        control_plane_url: &str,
        api_token: &str,
        allow_insecure_local_http: bool,
        lease_id: Uuid,
    ) -> Result<Self> {
        let control = Self::new(control_plane_url, api_token, allow_insecure_local_http)?;
        control.set_lease_id(lease_id);
        Ok(control)
    }

    pub fn set_lease_id(&self, lease_id: Uuid) {
        *self.lease_id.write().expect("worker lease lock poisoned") = Some(lease_id);
    }

    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Build an authenticated request only for the configured control-plane origin.
    pub fn authorized_request(&self, method: Method, url: Url) -> Result<RequestBuilder> {
        ensure!(
            same_origin(&self.base_url, &url),
            "authenticated request target is outside the control-plane origin"
        );
        let mut request = self
            .client
            .request(method, url)
            .header(header::AUTHORIZATION, self.authorization.clone());
        if let Some(lease_id) = *self.lease_id.read().expect("worker lease lock poisoned") {
            request = request.header("x-worker-lease", lease_id.to_string());
        }
        Ok(request)
    }

    fn endpoint(&self, path: &str) -> Result<Url> {
        let mut url = self.base_url.clone();
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("control-plane URL cannot accept a worker route"))?;
        segments.pop_if_empty();
        segments.extend(path.trim_start_matches('/').split('/'));
        drop(segments);
        Ok(url)
    }

    async fn post_json<T: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<R> {
        let response = self
            .authorized_request(Method::POST, self.endpoint(path)?)?
            .json(body)
            .send()
            .await
            .context("send worker control-plane request")?;
        let response = require_success(path, response).await?;
        response
            .json::<R>()
            .await
            .context("decode worker control-plane response")
    }

    async fn post_no_content<T: Serialize>(&self, path: &str, body: &T) -> Result<()> {
        let response = self
            .authorized_request(Method::POST, self.endpoint(path)?)?
            .json(body)
            .send()
            .await
            .context("send worker control-plane request")?;
        require_success(path, response).await?;
        Ok(())
    }

    async fn get_json<R: DeserializeOwned>(&self, path: &str) -> Result<R> {
        let response = self
            .authorized_request(Method::GET, self.endpoint(path)?)?
            .send()
            .await
            .context("send worker control-plane request")?;
        require_success(path, response)
            .await?
            .json::<R>()
            .await
            .context("decode worker control-plane response")
    }

    async fn get_json_with_assignment<R: DeserializeOwned>(
        &self,
        path: &str,
        assignment: WorkerAssignment,
    ) -> Result<R> {
        let mut url = self.endpoint(path)?;
        {
            let mut query = url.query_pairs_mut();
            match assignment {
                WorkerAssignment::Placement { generation } => {
                    query.append_pair("assignment", "placement");
                    query.append_pair("generation", &generation.to_string());
                }
                WorkerAssignment::Operation {
                    operation_id,
                    generation,
                    attempt,
                } => {
                    query.append_pair("assignment", "operation");
                    query.append_pair("operation_id", &operation_id.to_string());
                    query.append_pair("generation", &generation.to_string());
                    query.append_pair("attempt", &attempt.to_string());
                }
                WorkerAssignment::Export {
                    export_id,
                    generation,
                    attempt,
                } => {
                    query.append_pair("assignment", "export");
                    query.append_pair("export_id", &export_id.to_string());
                    query.append_pair("generation", &generation.to_string());
                    query.append_pair("attempt", &attempt.to_string());
                }
            }
        }
        let response = self
            .authorized_request(Method::GET, url)?
            .send()
            .await
            .context("send worker control-plane request")?;
        require_success(path, response)
            .await?
            .json::<R>()
            .await
            .context("decode worker control-plane response")
    }
}

pub fn validate_control_plane_url(value: &str, allow_insecure_local_http: bool) -> Result<Url> {
    let url = Url::parse(value).context("invalid control-plane URL")?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "control-plane URL must be an HTTP(S) origin or path without credentials, query, or fragment"
    );
    if url.scheme() == "http" {
        ensure!(
            allow_insecure_local_http && is_loopback_or_localhost(&url),
            "control-plane HTTP is allowed only for explicitly enabled local loopback development"
        );
    }
    Ok(url)
}

fn is_loopback_or_localhost(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || IpAddr::from_str(host.trim_matches(['[', ']']))
                .is_ok_and(|address| address.is_loopback())
    })
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

/// Keep worker-generated diagnostics within the control-plane request limit.
///
/// Runtime failures can include unbounded subprocess output. The API rejects
/// oversized error fields deliberately, so every client path carrying a
/// worker-generated diagnostic must enforce the same boundary before sending.
fn bounded_worker_error(error: &str) -> Cow<'_, str> {
    if error.len() <= MAX_WORKER_ERROR_BYTES {
        return Cow::Borrowed(error);
    }

    const SUFFIX: &str = "...[truncated]";
    let prefix_limit = MAX_WORKER_ERROR_BYTES - SUFFIX.len();
    let mut end = prefix_limit;
    while !error.is_char_boundary(end) {
        end -= 1;
    }
    Cow::Owned(format!("{}{}", &error[..end], SUFFIX))
}

async fn require_success(path: &str, response: reqwest::Response) -> Result<reqwest::Response> {
    let status = response.status();
    if !status.is_success() {
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "<unreadable response body>".to_owned());
        bail!(
            "worker control-plane request {path} failed with status {status}: {}",
            body.trim()
        );
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use reqwest::{Method, Url};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::timeout,
    };

    use super::{
        MAX_WORKER_ERROR_BYTES, WorkerControl, bounded_worker_error, same_origin,
        validate_control_plane_url,
    };

    #[test]
    fn control_plane_urls_require_tls_or_explicit_loopback_http() {
        assert!(validate_control_plane_url("https://control.example", false).is_ok());
        assert!(validate_control_plane_url("http://localhost:8080", false).is_err());
        assert!(validate_control_plane_url("http://127.0.0.1:8080", true).is_ok());
        assert!(validate_control_plane_url("http://[::1]:8080", true).is_ok());
        assert!(validate_control_plane_url("http://control.example", true).is_err());
        assert!(validate_control_plane_url("http://192.168.1.5", true).is_err());
        assert!(validate_control_plane_url("https://user:secret@control.example", false).is_err());
    }

    #[test]
    fn authenticated_requests_cannot_escape_the_control_plane_origin() {
        let worker =
            WorkerControl::new("https://control.example/base", &"a".repeat(43), false).unwrap();
        let local =
            Url::parse("https://control.example/base/internal/worker/control/heartbeat").unwrap();
        let other_port = Url::parse("https://control.example:444/base/steal").unwrap();
        let other_host = Url::parse("https://attacker.example/steal").unwrap();
        let downgrade = Url::parse("http://control.example/base/steal").unwrap();
        assert!(same_origin(worker.base_url(), &local));
        assert!(worker.authorized_request(Method::POST, local).is_ok());
        assert!(worker.authorized_request(Method::POST, other_port).is_err());
        assert!(worker.authorized_request(Method::POST, other_host).is_err());
        assert!(worker.authorized_request(Method::POST, downgrade).is_err());
    }

    #[test]
    fn worker_error_payloads_are_bounded_before_control_plane_requests() {
        let long = format!("Pumpkin readiness failed: {}", "x".repeat(2_000));
        let bounded = bounded_worker_error(&long);
        assert!(bounded.len() <= MAX_WORKER_ERROR_BYTES);
        assert!(bounded.ends_with("...[truncated]"));
        assert!(bounded.starts_with("Pumpkin readiness failed: "));

        let unicode = "é".repeat(MAX_WORKER_ERROR_BYTES);
        let bounded_unicode = bounded_worker_error(&unicode);
        assert!(bounded_unicode.len() <= MAX_WORKER_ERROR_BYTES);
        assert!(bounded_unicode.ends_with("...[truncated]"));
    }

    #[tokio::test]
    async fn authenticated_control_requests_never_follow_redirects() {
        let first = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let second = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let first_address = first.local_addr().unwrap();
        let second_address = second.local_addr().unwrap();
        let response = tokio::spawn(async move {
            let (mut socket, _) = first.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut part = [0; 512];
                let read = socket.read(&mut part).await.unwrap();
                assert_ne!(read, 0);
                request.extend_from_slice(&part[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let headers = String::from_utf8_lossy(&request).to_ascii_lowercase();
            assert!(headers.contains("authorization: bearer "));
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: http://{second_address}/redirected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let worker =
            WorkerControl::new(&format!("http://{first_address}"), &"a".repeat(43), true).unwrap();

        assert!(worker.heartbeat().await.is_err());
        response.await.unwrap();
        assert!(
            timeout(Duration::from_millis(150), second.accept())
                .await
                .is_err()
        );
    }
}

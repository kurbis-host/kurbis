use std::fmt;

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use futures::{StreamExt, stream};
use object_store::{
    Attributes, GetOptions, GetResult, GetResultPayload, ListResult, MultipartUpload, ObjectMeta,
    ObjectStore, PutMultipartOptions, PutOptions, PutPayload, PutResult, Result as StoreResult,
    UploadPart, path::Path,
};
use reqwest::{Method, StatusCode, Url, header};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{
    contracts::worker::{WorkerAssignment, WorkerSnapshotReservation},
    model::{SnapshotReason, SnapshotRecord},
    snapshot::WorkerObjectKey,
    worker::control_plane::WorkerControl,
};

const MULTIPART_CHANNEL_DEPTH: usize = 2;

#[derive(Debug, Clone)]
pub struct WorkerObjectClient {
    control: WorkerControl,
}

impl WorkerObjectClient {
    pub fn from_control_plane(control: WorkerControl) -> Self {
        Self { control }
    }

    pub fn new_with_lease(
        control_plane_url: &str,
        api_token: &str,
        allow_insecure_local_http: bool,
        lease_id: Uuid,
    ) -> Result<Self> {
        Ok(Self {
            control: WorkerControl::new_with_lease(
                control_plane_url,
                api_token,
                allow_insecure_local_http,
                lease_id,
            )?,
        })
    }

    pub async fn current_snapshot(
        &self,
        server_id: Uuid,
        assignment: WorkerAssignment,
    ) -> Result<Option<SnapshotRecord>> {
        self.control
            .current_snapshot_metadata(server_id, assignment)
            .await
    }

    pub async fn report_snapshot_cache_hint(
        &self,
        server_id: Uuid,
        assignment: WorkerAssignment,
        snapshot_generation: i64,
    ) -> Result<()> {
        self.control
            .report_snapshot_cache_hint(server_id, assignment, snapshot_generation)
            .await
    }

    pub async fn snapshot(
        &self,
        server_id: Uuid,
        assignment: WorkerAssignment,
        generation: i64,
    ) -> Result<SnapshotRecord> {
        self.control
            .snapshot_metadata(server_id, assignment, generation)
            .await
    }

    pub async fn reserve_snapshot(
        &self,
        server_id: Uuid,
        assignment: WorkerAssignment,
        reason: SnapshotReason,
    ) -> Result<WorkerSnapshotReservation> {
        let WorkerAssignment::Operation {
            operation_id,
            generation,
            attempt,
        } = assignment
        else {
            anyhow::bail!("snapshot allocation requires a running worker operation")
        };
        self.control
            .reserve_snapshot(server_id, operation_id, generation, attempt, reason)
            .await
    }

    pub async fn publish_snapshot_metadata(
        &self,
        server_id: Uuid,
        assignment: WorkerAssignment,
        snapshot_generation: i64,
        manifest_key: &str,
        size_bytes: i64,
        checksum: &str,
    ) -> Result<()> {
        let WorkerAssignment::Operation {
            operation_id,
            generation,
            attempt,
        } = assignment
        else {
            anyhow::bail!("snapshot publication requires a running worker operation")
        };
        self.control
            .publish_snapshot_metadata(
                server_id,
                operation_id,
                generation,
                attempt,
                snapshot_generation,
                manifest_key,
                size_bytes,
                checksum,
            )
            .await
    }

    pub async fn delete_snapshot_manifests(
        &self,
        server_id: Uuid,
        assignment: WorkerAssignment,
    ) -> Result<()> {
        let WorkerAssignment::Operation {
            operation_id,
            generation,
            attempt,
        } = assignment
        else {
            anyhow::bail!("snapshot cleanup requires a running worker operation")
        };
        self.control
            .delete_snapshot_manifests(server_id, operation_id, generation, attempt)
            .await
    }

    pub async fn export_is_running(
        &self,
        export_id: Uuid,
        generation: i64,
        attempt: i32,
    ) -> Result<bool> {
        self.control
            .export_is_running(export_id, generation, attempt)
            .await
    }

    pub fn scoped(&self, server_id: Uuid, assignment: WorkerAssignment) -> WorkerObjectStore {
        WorkerObjectStore {
            client: self.clone(),
            server_id,
            assignment: Some(assignment),
        }
    }

    pub fn unscoped(&self) -> WorkerObjectStore {
        WorkerObjectStore {
            client: self.clone(),
            server_id: Uuid::nil(),
            assignment: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkerObjectStore {
    client: WorkerObjectClient,
    server_id: Uuid,
    assignment: Option<WorkerAssignment>,
}

impl fmt::Display for WorkerObjectStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("worker-object-api")
    }
}

impl WorkerObjectStore {
    fn endpoint(&self, key: &Path) -> StoreResult<Url> {
        let key = WorkerObjectKey::parse(key.as_ref()).map_err(generic_error)?;
        let assignment = self
            .assignment
            .ok_or_else(|| generic_error("worker object request has no assignment scope"))?;
        let mut url = self.client.control.base_url().clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| generic_error("control plane URL cannot accept a worker route"))?;
            segments.pop_if_empty();
            let server_id = self.server_id.to_string();
            segments.extend([
                "internal",
                "worker",
                "servers",
                server_id.as_str(),
                "objects",
            ]);
            segments.extend(key.path().as_ref().split('/'));
        }
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
        Ok(url)
    }

    fn validate_options(options: &GetOptions) -> StoreResult<()> {
        if options.range.is_some()
            || options.if_match.is_some()
            || options.if_none_match.is_some()
            || options.if_modified_since.is_some()
            || options.if_unmodified_since.is_some()
            || options.version.is_some()
        {
            return Err(not_supported("conditional and ranged worker object reads"));
        }
        Ok(())
    }

    async fn send(
        &self,
        method: Method,
        location: &Path,
        body: Option<reqwest::Body>,
        content_length: Option<u64>,
    ) -> StoreResult<reqwest::Response> {
        let url = self.endpoint(location)?;
        let mut request = self
            .client
            .control
            .authorized_request(method, url)
            .map_err(generic_error)?;
        if let Some(content_length) = content_length {
            request = request.header(header::CONTENT_LENGTH, content_length);
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        let response = request.send().await.map_err(generic_error)?;
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let error = format!("worker object request failed with status {status}");
        if status == StatusCode::NOT_FOUND {
            return Err(object_store::Error::NotFound {
                path: location.to_string(),
                source: error.into(),
            });
        }
        Err(generic_error(error))
    }

    fn meta(location: &Path, size: u64, e_tag: Option<String>) -> ObjectMeta {
        ObjectMeta {
            location: location.clone(),
            last_modified: Utc::now(),
            size,
            e_tag,
            version: None,
        }
    }

    fn result_from_response(response: reqwest::Response) -> StoreResult<PutResult> {
        Ok(PutResult {
            e_tag: response
                .headers()
                .get(header::ETAG)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            version: None,
        })
    }
}

#[async_trait]
impl ObjectStore for WorkerObjectStore {
    async fn put_opts(
        &self,
        location: &Path,
        payload: PutPayload,
        opts: PutOptions,
    ) -> StoreResult<PutResult> {
        if opts.mode != object_store::PutMode::Overwrite {
            return Err(not_supported("non-overwrite worker object writes"));
        }
        if opts.tags != Default::default() || !opts.attributes.is_empty() {
            return Err(not_supported("worker object metadata writes"));
        }
        let length = payload.content_length() as u64;
        let body = reqwest::Body::wrap_stream(stream::iter(
            payload.into_iter().map(Ok::<_, std::io::Error>),
        ));
        let response = self
            .send(Method::PUT, location, Some(body), Some(length))
            .await?;
        Self::result_from_response(response)
    }

    async fn put_multipart_opts(
        &self,
        location: &Path,
        opts: PutMultipartOptions,
    ) -> StoreResult<Box<dyn MultipartUpload>> {
        if opts.tags != Default::default() || !opts.attributes.is_empty() {
            return Err(not_supported("worker multipart metadata writes"));
        }
        let (sender, receiver) = mpsc::channel(MULTIPART_CHANNEL_DEPTH);
        let body = reqwest::Body::wrap_stream(stream::unfold(receiver, |mut receiver| async {
            receiver
                .recv()
                .await
                .map(|bytes| (Ok::<_, std::io::Error>(bytes), receiver))
        }));
        let url = self.endpoint(location)?;
        let location = location.to_string();
        let control = self.client.control.clone();
        let request = tokio::spawn(async move {
            let response = control
                .authorized_request(Method::PUT, url)
                .map_err(generic_error)?
                .body(body)
                .send()
                .await
                .map_err(generic_error)?;
            if response.status().is_success() {
                Ok(response)
            } else if response.status() == StatusCode::NOT_FOUND {
                Err(object_store::Error::NotFound {
                    path: location.to_string(),
                    source: "worker object was not found".into(),
                })
            } else {
                Err(generic_error(format!(
                    "worker multipart request failed with status {}",
                    response.status()
                )))
            }
        });
        Ok(Box::new(WorkerMultipartUpload {
            sender: Some(sender),
            request: Some(request),
        }))
    }

    async fn get_opts(&self, location: &Path, options: GetOptions) -> StoreResult<GetResult> {
        Self::validate_options(&options)?;
        let method = if options.head {
            Method::HEAD
        } else {
            Method::GET
        };
        let response = self.send(method, location, None, None).await?;
        let size = response
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| generic_error("worker object response omitted its content length"))?;
        let e_tag = response
            .headers()
            .get(header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let meta = Self::meta(location, size, e_tag);
        let payload = if options.head {
            GetResultPayload::Stream(stream::empty().boxed())
        } else {
            GetResultPayload::Stream(
                response
                    .bytes_stream()
                    .map(|result| result.map_err(generic_error))
                    .boxed(),
            )
        };
        Ok(GetResult {
            payload,
            meta,
            range: 0..size,
            attributes: Attributes::new(),
        })
    }

    async fn delete(&self, location: &Path) -> StoreResult<()> {
        self.send(Method::DELETE, location, None, None).await?;
        Ok(())
    }

    fn list(
        &self,
        _prefix: Option<&Path>,
    ) -> futures::stream::BoxStream<'static, StoreResult<ObjectMeta>> {
        stream::once(async { Err(not_supported("worker object listing")) }).boxed()
    }

    async fn list_with_delimiter(&self, _prefix: Option<&Path>) -> StoreResult<ListResult> {
        Err(not_supported("worker object listing"))
    }

    async fn copy(&self, _from: &Path, _to: &Path) -> StoreResult<()> {
        Err(not_supported("worker object copy"))
    }

    async fn copy_if_not_exists(&self, _from: &Path, _to: &Path) -> StoreResult<()> {
        Err(not_supported("worker object copy"))
    }
}

#[derive(Debug)]
struct WorkerMultipartUpload {
    sender: Option<mpsc::Sender<Bytes>>,
    request: Option<tokio::task::JoinHandle<StoreResult<reqwest::Response>>>,
}

#[async_trait]
impl MultipartUpload for WorkerMultipartUpload {
    fn put_part(&mut self, data: PutPayload) -> UploadPart {
        let sender = self.sender.clone();
        Box::pin(async move {
            let Some(sender) = sender else {
                return Err(generic_error("worker multipart upload is closed"));
            };
            for bytes in data {
                sender
                    .send(bytes)
                    .await
                    .map_err(|_| generic_error("worker object request stopped receiving data"))?;
            }
            Ok(())
        })
    }

    async fn complete(&mut self) -> StoreResult<PutResult> {
        self.sender.take();
        let request = self
            .request
            .take()
            .ok_or_else(|| generic_error("worker multipart upload is already closed"))?;
        let response = request.await.map_err(generic_error)??;
        WorkerObjectStore::result_from_response(response)
    }

    async fn abort(&mut self) -> StoreResult<()> {
        self.sender.take();
        if let Some(request) = self.request.take() {
            request.abort();
        }
        Ok(())
    }
}

impl Drop for WorkerMultipartUpload {
    fn drop(&mut self) {
        if let Some(request) = self.request.take() {
            request.abort();
        }
        self.sender.take();
    }
}

fn generic_error(error: impl std::fmt::Display + Send + Sync + 'static) -> object_store::Error {
    object_store::Error::Generic {
        store: "worker-object-api",
        source: Box::new(DisplayError(error.to_string())),
    }
}

#[derive(Debug)]
struct DisplayError(String);

impl fmt::Display for DisplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DisplayError {}

fn not_supported(operation: &'static str) -> object_store::Error {
    object_store::Error::NotSupported {
        source: operation.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, time::Duration};

    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode, header},
        routing::any,
    };
    use futures::StreamExt;
    use object_store::{ObjectStore, path::Path};
    use tokio::{net::TcpListener, sync::mpsc};
    use uuid::Uuid;

    use crate::contracts::worker::WorkerAssignment;

    use super::WorkerObjectClient;

    fn client(url: &str, token: &str, allow_http: bool) -> anyhow::Result<WorkerObjectClient> {
        WorkerObjectClient::new_with_lease(url, token, allow_http, Uuid::new_v4())
    }

    #[test]
    fn client_rejects_non_http_urls_and_malformed_worker_tokens() {
        assert!(client("file:///tmp/store", &"a".repeat(43), true).is_err());
        assert!(client("https://user:pass@example.test", &"a".repeat(43), false).is_err());
        assert!(client("https://example.test", "shared-internal-token", false).is_err());
        assert!(client("http://example.test", &"a".repeat(43), true).is_err());
        assert!(client("http://127.0.0.1:8080", &"a".repeat(43), false).is_err());
        assert!(client("http://127.0.0.1:8080", &"a".repeat(43), true).is_ok());
    }

    #[tokio::test]
    async fn every_unscoped_or_unsupported_operation_fails_closed() {
        let client = client("https://example.test", &"a".repeat(43), false).unwrap();
        let store = client.unscoped();
        let key = Path::from(format!("chunks/{}", "a".repeat(64)));
        assert!(store.head(&key).await.is_err());
        assert!(
            store
                .put(&key, bytes::Bytes::from_static(b"x").into())
                .await
                .is_err()
        );
        assert!(store.delete(&key).await.is_err());
        assert!(store.list(None).next().await.unwrap().is_err());
        assert!(
            store
                .copy(&key, &Path::from(format!("chunks/{}", "b".repeat(64))))
                .await
                .is_err()
        );

        let _scoped = client.scoped(
            Uuid::new_v4(),
            WorkerAssignment::Operation {
                operation_id: Uuid::new_v4(),
                generation: 4,
                attempt: 1,
            },
        );
    }

    #[tokio::test]
    async fn worker_bearer_is_not_forwarded_across_redirects() {
        let (capture, mut captured) = mpsc::channel::<Option<String>>(1);
        let capture_app = Router::new().route(
            "/capture",
            any(move |request: Request<Body>| {
                let capture = capture.clone();
                async move {
                    let authorization = request
                        .headers()
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned);
                    let _ = capture.send(authorization).await;
                    StatusCode::NO_CONTENT
                }
            }),
        );
        let capture_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let capture_address: SocketAddr = capture_listener.local_addr().unwrap();
        let capture_task = tokio::spawn(async move {
            axum::serve(capture_listener, capture_app).await.unwrap();
        });

        let redirect_url = format!("http://{capture_address}/capture");
        let redirect_app = Router::new().fallback(move || {
            let redirect_url = redirect_url.clone();
            async move {
                (
                    StatusCode::TEMPORARY_REDIRECT,
                    [(header::LOCATION, redirect_url)],
                )
            }
        });
        let redirect_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let redirect_address: SocketAddr = redirect_listener.local_addr().unwrap();
        let redirect_task = tokio::spawn(async move {
            axum::serve(redirect_listener, redirect_app).await.unwrap();
        });

        let client = client(&format!("http://{redirect_address}"), &"a".repeat(43), true).unwrap();
        let store = client.scoped(
            Uuid::new_v4(),
            WorkerAssignment::Placement { generation: 1 },
        );
        let key = Path::from(format!("chunks/{}", "a".repeat(64)));
        assert!(store.get(&key).await.is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(150), captured.recv())
                .await
                .is_err()
        );

        redirect_task.abort();
        capture_task.abort();
    }
}

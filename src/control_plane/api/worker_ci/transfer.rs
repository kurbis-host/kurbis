//! Bounded worker downloads fenced by database authorization through chunk release.
//!
//! Each output chunk carries a PostgreSQL transaction holding shared locks on
//! the worker credential, runtime assignment, and referenced artifact/snapshot.
//! The transaction is released only when Axum polls for the next body chunk (or
//! drops the body), so revocation and reassignment cannot commit between an
//! authorization read and that chunk's release.

use std::{
    future::Future,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use crate::ci::{CiPreviewRuntimeRepository, runtime::CiWorldSnapshot};
use axum::{body::Body, http::HeaderMap};
use bytes::{Bytes, BytesMut};
use futures::{StreamExt, stream};
use sqlx::{Postgres, Transaction};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use super::{AppState, worker_auth};

const AUTHORIZED_CHUNK_BYTES: usize = 8 * 1024 * 1024;
const TRANSFER_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_TRANSFER_TIME: Duration = Duration::from_secs(2 * 60 * 60);

/// A read transaction whose row locks authorize one emitted body chunk.
pub(super) struct AuthorizationFence {
    transaction: Transaction<'static, Postgres>,
}

impl AuthorizationFence {
    async fn release(self) -> Result<(), sqlx::Error> {
        self.transaction.commit().await
    }
}

pub(super) struct ArtifactDownloadAuthorization<'a> {
    pub state: &'a AppState,
    pub headers: &'a HeaderMap,
    pub preview_id: Uuid,
    pub worker_id: Uuid,
    pub generation: i64,
    pub lease_token: Uuid,
    pub expected_key: &'a str,
    pub expected_size: i64,
    pub expected_digest: &'a str,
}

pub(super) async fn artifact_authorization_fence(
    request: ArtifactDownloadAuthorization<'_>,
) -> Option<AuthorizationFence> {
    let mut tx = request.state.pool.begin().await.ok()?;
    if worker_auth::lock_authenticated_worker_id(&mut tx, request.headers)
        .await
        .ok()?
        != request.worker_id
    {
        return None;
    }
    let current = CiPreviewRuntimeRepository::lock_artifact_download(
        &mut tx,
        request.preview_id,
        request.worker_id,
        request.generation,
        request.lease_token,
    )
    .await
    .ok()??;
    if current.0 != request.expected_key
        || current.1 != request.expected_size
        || current.2 != request.expected_digest
    {
        return None;
    }
    Some(AuthorizationFence { transaction: tx })
}

pub(super) async fn world_snapshot_authorization_fence(
    state: &AppState,
    headers: &HeaderMap,
    preview_id: Uuid,
    worker_id: Uuid,
    generation: i64,
    lease_token: Uuid,
    expected_snapshot: &CiWorldSnapshot,
) -> Option<AuthorizationFence> {
    let mut tx = state.pool.begin().await.ok()?;
    if worker_auth::lock_authenticated_worker_id(&mut tx, headers)
        .await
        .ok()?
        != worker_id
    {
        return None;
    }
    let current = CiPreviewRuntimeRepository::lock_world_snapshot_download(
        &mut tx,
        preview_id,
        worker_id,
        generation,
        lease_token,
    )
    .await
    .ok()??;
    if &current != expected_snapshot {
        return None;
    }
    Some(AuthorizationFence { transaction: tx })
}

struct AuthorizedChunk {
    bytes: Bytes,
    acknowledged: oneshot::Sender<()>,
    valid: Arc<AtomicBool>,
}

/// Wraps an object-store stream with bounded buffering and integrity checks.
/// The callback returns a DB-backed fence, not a boolean: its locks remain held
/// until the corresponding response chunk has been handed off to Axum.
pub(super) fn body_with_reauthorization<F, Fut>(
    source: futures::stream::BoxStream<'static, object_store::Result<Bytes>>,
    expected_size: u64,
    expected_digest: String,
    authorize: F,
) -> Body
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Option<AuthorizationFence>> + Send + 'static,
{
    let (sender, receiver) = mpsc::channel(1);
    tokio::spawn(async move {
        stream_authorized(source, sender, expected_size, expected_digest, authorize).await;
    });
    Body::from_stream(stream::unfold(
        (receiver, None::<oneshot::Sender<()>>, false),
        |(mut receiver, previous_acknowledgement, done)| async move {
            if done {
                return None;
            }
            if let Some(acknowledged) = previous_acknowledgement
                && acknowledged.send(()).is_err()
            {
                return Some((
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "worker download chunk was not acknowledged before its authorization expired",
                    )),
                    (receiver, None, true),
                ));
            }
            receiver.recv().await.map(|item| match item {
                Ok(chunk) if chunk.valid.load(Ordering::Acquire) => {
                    (Ok(chunk.bytes), (receiver, Some(chunk.acknowledged), false))
                }
                Ok(_) => (
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "worker download authorization expired before chunk release",
                    )),
                    (receiver, None, true),
                ),
                Err(error) => (Err(error), (receiver, None, true)),
            })
        },
    ))
}

async fn stream_authorized<F, Fut>(
    mut source: futures::stream::BoxStream<'static, object_store::Result<Bytes>>,
    sender: mpsc::Sender<Result<AuthorizedChunk, io::Error>>,
    expected_size: u64,
    expected_digest: String,
    authorize: F,
) where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Option<AuthorizationFence>> + Send + 'static,
{
    let deadline = tokio::time::Instant::now() + MAX_TRANSFER_TIME;
    let mut total = 0_u64;
    let mut hasher = blake3::Hasher::new();
    let mut pending = None;

    loop {
        let mut buffer = BytesMut::with_capacity(AUTHORIZED_CHUNK_BYTES);
        let mut eof = false;
        while buffer.len() < AUTHORIZED_CHUNK_BYTES {
            let (chunk, from_source) = if let Some(chunk) = pending.take() {
                (chunk, false)
            } else {
                match next_source_chunk(&sender, &mut source, &deadline).await {
                    Ok(Some(chunk)) => (chunk, true),
                    Ok(None) => {
                        eof = true;
                        break;
                    }
                    Err(error) => {
                        send_body_error(&sender, &deadline, error).await;
                        return;
                    }
                }
            };

            if from_source {
                total = match total.checked_add(chunk.len() as u64) {
                    Some(total) if total <= expected_size => total,
                    _ => {
                        send_body_error(
                            &sender,
                            &deadline,
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                "worker download exceeded its declared size",
                            ),
                        )
                        .await;
                        return;
                    }
                };
                hasher.update(&chunk);
            }

            let room = AUTHORIZED_CHUNK_BYTES - buffer.len();
            let take = room.min(chunk.len());
            buffer.extend_from_slice(&chunk[..take]);
            if take < chunk.len() {
                pending = Some(chunk.slice(take..));
            }
        }

        // A full buffer can still be the final one. Look ahead when the
        // declared size has arrived so size/digest validation happens before
        // those last bytes are released, even for an exact chunk-size object.
        if !eof && pending.is_none() && total == expected_size {
            match next_source_chunk(&sender, &mut source, &deadline).await {
                Ok(None) => eof = true,
                Ok(Some(_)) => {
                    send_body_error(
                        &sender,
                        &deadline,
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "worker download exceeded its declared size",
                        ),
                    )
                    .await;
                    return;
                }
                Err(error) => {
                    send_body_error(&sender, &deadline, error).await;
                    return;
                }
            }
        }

        if buffer.is_empty() && eof {
            if let Err(error) =
                verify_complete(total, expected_size, hasher.clone(), &expected_digest)
            {
                send_body_error(&sender, &deadline, error).await;
                return;
            }
            return;
        }
        if buffer.is_empty() {
            continue;
        }

        // Never expose the final chunk until all bytes and the digest have been
        // verified. Earlier chunks are each released under their own fence.
        if eof
            && let Err(error) =
                verify_complete(total, expected_size, hasher.clone(), &expected_digest)
        {
            send_body_error(&sender, &deadline, error).await;
            return;
        }

        let permit = match reserve_with_timeout(&sender, &deadline).await {
            Some(permit) => permit,
            None => return,
        };
        let Some(fence) = authorize_with_timeout(&sender, &deadline, &authorize).await else {
            drop(permit);
            send_body_error(&sender, &deadline, authorization_error()).await;
            return;
        };
        let (acknowledged, acknowledgement) = oneshot::channel();
        let valid = Arc::new(AtomicBool::new(true));
        permit.send(Ok(AuthorizedChunk {
            bytes: buffer.freeze(),
            acknowledged,
            valid: valid.clone(),
        }));
        let acknowledgement = tokio::time::timeout_at(
            deadline,
            tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, acknowledgement),
        );
        tokio::pin!(acknowledgement);
        let acknowledged = tokio::select! {
            _ = sender.closed() => return,
            result = &mut acknowledgement => matches!(result, Ok(Ok(Ok(())))),
        };
        if !acknowledged {
            valid.store(false, Ordering::Release);
        }
        // Keep the shared DB locks until the body is polled again, proving the
        // previous bounded chunk has been consumed. No later chunk is read or
        // authorized until this fence is released.
        if fence.release().await.is_err() {
            send_body_error(
                &sender,
                &deadline,
                io::Error::other("worker download authorization fence could not be released"),
            )
            .await;
            return;
        }
        if !acknowledged || eof {
            return;
        }
    }
}

async fn next_source_chunk(
    sender: &mpsc::Sender<Result<AuthorizedChunk, io::Error>>,
    source: &mut futures::stream::BoxStream<'static, object_store::Result<Bytes>>,
    deadline: &tokio::time::Instant,
) -> Result<Option<Bytes>, io::Error> {
    let next = tokio::time::timeout_at(
        *deadline,
        tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, source.next()),
    );
    tokio::pin!(next);
    tokio::select! {
        _ = sender.closed() => Err(io::Error::new(io::ErrorKind::BrokenPipe, "worker download client disconnected")),
        result = &mut next => match result {
            Ok(Ok(Some(Ok(chunk)))) => Ok(Some(chunk)),
            Ok(Ok(Some(Err(error)))) => Err(io::Error::other(error.to_string())),
            Ok(Ok(None)) => Ok(None),
            Ok(Err(_)) | Err(_) => Err(transfer_timeout_io()),
        }
    }
}

async fn reserve_with_timeout<'a>(
    sender: &'a mpsc::Sender<Result<AuthorizedChunk, io::Error>>,
    deadline: &tokio::time::Instant,
) -> Option<mpsc::Permit<'a, Result<AuthorizedChunk, io::Error>>> {
    let reserve = tokio::time::timeout_at(
        *deadline,
        tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, sender.reserve()),
    );
    tokio::pin!(reserve);
    tokio::select! {
        _ = sender.closed() => None,
        result = &mut reserve => match result {
            Ok(Ok(Ok(permit))) => Some(permit),
            Ok(Ok(Err(_))) | Ok(Err(_)) | Err(_) => None,
        }
    }
}

async fn authorize_with_timeout<F, Fut>(
    sender: &mpsc::Sender<Result<AuthorizedChunk, io::Error>>,
    deadline: &tokio::time::Instant,
    authorize: &F,
) -> Option<AuthorizationFence>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Option<AuthorizationFence>> + Send + 'static,
{
    let authorization = authorize();
    tokio::pin!(authorization);
    let result = tokio::time::timeout_at(
        *deadline,
        tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, &mut authorization),
    );
    tokio::pin!(result);
    tokio::select! {
        _ = sender.closed() => None,
        authorization = &mut result => match authorization {
            Ok(Ok(fence)) => fence,
            Ok(Err(_)) | Err(_) => None,
        },
    }
}

async fn send_body_error(
    sender: &mpsc::Sender<Result<AuthorizedChunk, io::Error>>,
    deadline: &tokio::time::Instant,
    error: io::Error,
) {
    if let Some(permit) = reserve_with_timeout(sender, deadline).await {
        permit.send(Err(error));
    }
}

fn verify_complete(
    total: u64,
    expected_size: u64,
    hasher: blake3::Hasher,
    expected_digest: &str,
) -> Result<(), io::Error> {
    if total != expected_size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worker download ended before its declared size",
        ));
    }
    if hasher.finalize().to_hex().as_str() != expected_digest {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worker download failed digest verification",
        ));
    }
    Ok(())
}

fn authorization_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "worker download authorization failed",
    )
}

fn transfer_timeout_io() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "worker download exceeded its idle or total time limit",
    )
}

use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
};
use futures::{Stream, stream};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, postgres::PgListener};
use tokio::time::sleep;
use uuid::Uuid;

use crate::model::{OperationRecord as DbOperation, SnapshotRecord as DbSnapshot};

use super::activity::{EventRow, ServerEventKind};
use super::internal::{require_server_owner, server_by_id};
use super::logs::LogEntry;
use super::players::player_list;
use super::server::server_view;
use super::types::{Operation, Server, Snapshot};
use super::whitelist::whitelist_state;
use super::{ApiError, AppState};
use super::{PlayerPresence, Whitelist};

#[derive(Debug, Deserialize)]
pub(super) struct EventsQuery {
    pub(super) after_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ServerEvent {
    pub id: i64,
    #[serde(flatten)]
    pub payload: ServerEventPayload,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEventPayload {
    Server {
        data: Box<Server>,
    },
    Operation {
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<Operation>,
    },
    Snapshot {
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<Snapshot>,
    },
    Log {
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<LogEntry>,
    },
    Players {
        data: Vec<PlayerPresence>,
    },
    Whitelist {
        data: Whitelist,
    },
}

pub(super) async fn server_event_payload(
    state: &AppState,
    row: &EventRow,
    server_id: Uuid,
) -> Result<ServerEvent, ApiError> {
    let payload = match ServerEventKind::try_from(row.event_type.as_str())? {
        ServerEventKind::Server => {
            let server = server_by_id(&state.pool, server_id).await?.ok_or_else(|| {
                ApiError::with_code(
                    StatusCode::NOT_FOUND,
                    "server_not_found",
                    "server not found",
                )
            })?;
            ServerEventPayload::Server {
                data: Box::new(server_view(&state.pool, server, &state.config.root_domain).await?),
            }
        }
        ServerEventKind::Operation => {
            let operation = sqlx::query_as::<_, DbOperation>(
                "SELECT id, server_id, worker_id, kind, status, generation, attempt, payload,
                        last_error, created_at, updated_at
                 FROM operations
                 WHERE server_id = $1
                 ORDER BY updated_at DESC, created_at DESC LIMIT 1",
            )
            .bind(server_id)
            .fetch_optional(&state.pool)
            .await?
            .map(Operation::from);
            ServerEventPayload::Operation { data: operation }
        }
        ServerEventKind::Snapshot => {
            let snapshot = sqlx::query_as::<_, DbSnapshot>(
                "SELECT id, server_id, generation, manifest_key, size_bytes, checksum, reason,
                        complete, created_at
                 FROM snapshots
                 WHERE server_id = $1 AND complete
                 ORDER BY generation DESC LIMIT 1",
            )
            .bind(server_id)
            .fetch_optional(&state.pool)
            .await?
            .map(Snapshot::from);
            ServerEventPayload::Snapshot { data: snapshot }
        }
        ServerEventKind::Log => {
            // server_events intentionally stores only the durable event cursor
            // and kind. It cannot identify the exact source log row, so avoid
            // attaching a potentially newer line during replay. The client
            // catches up losslessly through /logs?after_id=.
            ServerEventPayload::Log { data: None }
        }
        ServerEventKind::Players => ServerEventPayload::Players {
            data: player_list(&state.pool, server_id).await?,
        },
        ServerEventKind::Whitelist => ServerEventPayload::Whitelist {
            data: whitelist_state(&state.pool, server_id).await?,
        },
    };
    Ok(ServerEvent {
        id: row.id,
        payload,
    })
}

struct SseState {
    state: Arc<AppState>,
    listener: PgListener,
    headers: HeaderMap,
    server_id: Uuid,
    cursor: i64,
}

pub(super) async fn next_event_row(
    pool: &PgPool,
    server_id: Uuid,
    cursor: i64,
) -> Result<Option<EventRow>, sqlx::Error> {
    sqlx::query_as::<_, EventRow>(
        "SELECT e.id, e.event_type, e.created_at
         FROM server_events e
         WHERE e.server_id = $1 AND e.id > $2
         ORDER BY e.id ASC LIMIT 1",
    )
    .bind(server_id)
    .bind(cursor)
    .fetch_optional(pool)
    .await
}

pub(super) fn event_cursor(headers: &HeaderMap, query: &EventsQuery) -> Result<i64, ApiError> {
    let cursor = if let Some(value) = headers.get("last-event-id") {
        value
            .to_str()
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .ok_or_else(|| {
                ApiError::with_code(
                    StatusCode::BAD_REQUEST,
                    "invalid_cursor",
                    "Last-Event-ID must be a non-negative integer",
                )
            })?
    } else {
        query.after_id.unwrap_or(0)
    };
    if cursor < 0 {
        return Err(ApiError::with_code(
            StatusCode::BAD_REQUEST,
            "invalid_cursor",
            "event cursor must be non-negative",
        ));
    }
    Ok(cursor)
}

pub(super) async fn server_events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(server_id): Path<Uuid>,
    Query(query): Query<EventsQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let server = require_server_owner(&state, &headers, server_id).await?;
    let cursor = event_cursor(&headers, &query)?;

    // LISTEN before the first durable read. A commit between these two steps is
    // visible either in the catch-up query or as a notification, so reconnects
    // cannot create an event gap.
    let mut listener = PgListener::connect(&state.config.common.database_url).await?;
    listener.listen("server_events").await?;

    let stream_state = SseState {
        state,
        listener,
        headers,
        server_id: server.id,
        cursor,
    };
    let output = stream::unfold(stream_state, |mut stream_state| async move {
        loop {
            if let Err(error) = require_server_owner(
                &stream_state.state,
                &stream_state.headers,
                stream_state.server_id,
            )
            .await
            {
                tracing::debug!(
                    ?error,
                    "closing server event stream after authorization failed"
                );
                return None;
            }

            match next_event_row(
                &stream_state.state.pool,
                stream_state.server_id,
                stream_state.cursor,
            )
            .await
            {
                Ok(Some(row)) => {
                    let kind = match ServerEventKind::try_from(row.event_type.as_str()) {
                        Ok(kind) => kind,
                        Err(error) => {
                            tracing::error!(?error, "failed to decode durable server event");
                            return None;
                        }
                    };
                    let payload = match server_event_payload(
                        &stream_state.state,
                        &row,
                        stream_state.server_id,
                    )
                    .await
                    {
                        Ok(payload) => payload,
                        Err(error) => {
                            tracing::warn!(?error, "failed to build server event payload");
                            return None;
                        }
                    };
                    let data = match serde_json::to_string(&payload) {
                        Ok(data) => data,
                        Err(error) => {
                            tracing::error!(?error, "failed to serialize server event payload");
                            return None;
                        }
                    };
                    stream_state.cursor = row.id;
                    let event = Event::default()
                        .id(row.id.to_string())
                        .event(kind.as_str())
                        .data(data);
                    return Some((Ok(event), stream_state));
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(?error, "failed to read durable server events");
                    return None;
                }
            }

            tokio::select! {
                notification = stream_state.listener.recv() => {
                    match notification {
                        Ok(notification) => {
                            let is_target = notification
                                .payload()
                                .split_once(':')
                                .and_then(|(server_id, _)| Uuid::parse_str(server_id).ok())
                                .is_some_and(|server_id| server_id == stream_state.server_id);
                            if is_target {
                                continue;
                            }
                        }
                        Err(error) => {
                            tracing::warn!(?error, "Postgres SSE listener disconnected");
                            return None;
                        }
                    }
                }
                _ = sleep(Duration::from_secs(30)) => {
                    // Re-run the durable read and authorization check even if this
                    // server has been quiet, so suspension closes existing streams.
                }
            }
        }
    });

    Ok(Sse::new(output).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

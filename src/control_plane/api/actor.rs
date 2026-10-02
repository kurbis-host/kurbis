use std::{ops::Deref, sync::Arc};

use axum::{
    Json,
    body::Body,
    extract::{FromRequest, FromRequestParts, Path, Query, rejection::JsonRejection},
    http::{HeaderMap, Request, request::Parts},
};
use garde::Validate;
use schemars::JsonSchema;
use serde::{Deserialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::control_plane::auth::CurrentUser;

use super::{ApiError, AppState, internal, worker_auth};

/// The authenticated principal for API commands.
///
/// A worker is identified exclusively from its bearer credential.  The
/// command payload never carries a worker id, which prevents a valid worker
/// credential from being confused with another worker's placement.
#[derive(Debug, Clone)]
pub enum ActorContext {
    Owner(CurrentUser),
    OwnerWithServerSessions {
        user: CurrentUser,
        sessions: Vec<super::server_access::ServerSessionScope>,
    },
    Worker {
        worker_id: Uuid,
        lease_id: Uuid,
    },
    ServerSessions(Vec<super::server_access::ServerSessionScope>),
    ServerCapability {
        server_id: Uuid,
        token_hash: Vec<u8>,
    },
    Gateway,
}

macro_rules! api_headers {
    ($name:ident, $schemes:expr) => {
        #[derive(Debug, Clone)]
        pub struct $name(pub HeaderMap);

        impl Deref for $name {
            type Target = HeaderMap;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl FromRequestParts<Arc<AppState>> for $name {
            type Rejection = ApiError;

            async fn from_request_parts(
                parts: &mut Parts,
                _state: &Arc<AppState>,
            ) -> Result<Self, Self::Rejection> {
                Ok(Self(parts.headers.clone()))
            }
        }

        impl aide::operation::OperationInput for $name {
            fn operation_input(
                _context: &mut aide::generate::GenContext,
                operation: &mut aide::openapi::Operation,
            ) {
                for scheme in $schemes {
                    operation.security.push(
                        [(scheme.to_owned(), Vec::<String>::new())]
                            .into_iter()
                            .collect(),
                    );
                }
            }
        }
    };
}

api_headers!(ApiAccountHeaders, ["accountSessionCookie"]);
api_headers!(
    ApiServerHeaders,
    [
        "accountSessionCookie",
        "serverGrantCookie",
        "serverCapabilityBearer"
    ]
);
api_headers!(
    ApiAccountOrServerGrantHeaders,
    ["accountSessionCookie", "serverGrantCookie"]
);
api_headers!(ApiServerGrantHeaders, ["serverGrantCookie"]);
api_headers!(ApiServerCapabilityHeaders, ["serverCapabilityBearer"]);

impl ActorContext {
    pub fn owner(&self) -> Option<&CurrentUser> {
        match self {
            Self::Owner(user) | Self::OwnerWithServerSessions { user, .. } => Some(user),
            Self::Worker { .. }
            | Self::ServerSessions(_)
            | Self::ServerCapability { .. }
            | Self::Gateway => None,
        }
    }

    pub fn worker_id(&self) -> Option<Uuid> {
        match self {
            Self::Owner(_)
            | Self::OwnerWithServerSessions { .. }
            | Self::ServerSessions(_)
            | Self::ServerCapability { .. }
            | Self::Gateway => None,
            Self::Worker { worker_id, .. } => Some(*worker_id),
        }
    }

    pub fn worker_lease_id(&self) -> Option<Uuid> {
        match self {
            Self::Worker { lease_id, .. } => Some(*lease_id),
            _ => None,
        }
    }

    pub fn server_session_scope(
        &self,
        server_id: Uuid,
    ) -> Option<&super::server_access::ServerSessionScope> {
        let sessions = match self {
            Self::OwnerWithServerSessions { sessions, .. } | Self::ServerSessions(sessions) => {
                sessions
            }
            _ => return None,
        };
        sessions
            .iter()
            .find(|session| session.server_id == server_id)
    }

    pub fn server_capability(&self, server_id: Uuid) -> Option<&[u8]> {
        match self {
            Self::ServerCapability {
                server_id: scoped_server_id,
                token_hash,
            } if *scoped_server_id == server_id => Some(token_hash),
            _ => None,
        }
    }

    pub fn is_gateway(&self) -> bool {
        matches!(self, Self::Gateway)
    }
}

impl FromRequestParts<Arc<AppState>> for ActorContext {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let is_bearer = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("Bearer "));
        if is_bearer {
            if let Ok((worker_id, lease_id)) =
                worker_auth::authenticated_worker_identity(state, &parts.headers).await
            {
                return Ok(Self::Worker {
                    worker_id,
                    lease_id,
                });
            }
            if let Some((server_id, token_hash)) =
                super::server_access::instance_capability_scope(&state.pool, &parts.headers).await?
            {
                return Ok(Self::ServerCapability {
                    server_id,
                    token_hash,
                });
            }
            let expected_gateway_token = format!("Bearer {}", state.config.internal_token);
            if parts
                .headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                == Some(expected_gateway_token.as_str())
            {
                return Ok(Self::Gateway);
            }
            return Err(worker_auth::invalid_worker_token());
        }

        let user = crate::control_plane::auth::authenticate(&state.pool, &parts.headers)
            .await
            .ok();
        let sessions =
            super::server_access::server_session_scopes(&state.pool, &parts.headers).await?;
        if let Some(user) = user {
            return Ok(if sessions.is_empty() {
                Self::Owner(user)
            } else {
                Self::OwnerWithServerSessions { user, sessions }
            });
        }
        if !sessions.is_empty() {
            return Ok(Self::ServerSessions(sessions));
        }
        Err(ApiError::with_code(
            axum::http::StatusCode::UNAUTHORIZED,
            "authentication_required",
            "authentication required",
        ))
    }
}

/// JSON extractor that validates the deserialized DTO at the HTTP boundary.
#[derive(Debug, Clone)]
pub struct ApiJson<T>(pub T);

impl<T> Deref for ApiJson<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> FromRequest<Arc<AppState>> for ApiJson<T>
where
    T: DeserializeOwned + Validate + Send + 'static,
    T::Context: Default + Send,
{
    type Rejection = ApiError;

    async fn from_request(
        request: Request<Body>,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let Json(value) = Json::<T>::from_request(request, state)
            .await
            .map_err(|rejection| match rejection {
                JsonRejection::JsonDataError(_) => ApiError::with_code(
                    axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_request",
                    "request body does not match this endpoint's input schema",
                ),
                JsonRejection::MissingJsonContentType(_) => ApiError::with_code(
                    axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    "json_content_type_required",
                    "request body must use application/json",
                ),
                JsonRejection::JsonSyntaxError(_) | JsonRejection::BytesRejection(_) => {
                    ApiError::with_code(
                        axum::http::StatusCode::BAD_REQUEST,
                        "invalid_json",
                        "request body is not valid JSON for this endpoint",
                    )
                }
                other => ApiError::with_code(
                    other.status(),
                    "invalid_json",
                    "request body could not be decoded for this endpoint",
                ),
            })?;
        value.validate().map_err(|_| {
            ApiError::with_code(
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
                "request body failed validation",
            )
        })?;
        Ok(Self(value))
    }
}

impl<T> aide::operation::OperationInput for ApiJson<T>
where
    T: JsonSchema,
{
    fn operation_input(
        context: &mut aide::generate::GenContext,
        operation: &mut aide::openapi::Operation,
    ) {
        <Json<T> as aide::operation::OperationInput>::operation_input(context, operation);
    }
}

/// Path extractor with one structured rejection shape across API handlers.
#[derive(Debug, Clone)]
pub struct ApiPath<T>(pub T);

impl<T> FromRequestParts<Arc<AppState>> for ApiPath<T>
where
    T: DeserializeOwned + Send,
{
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|Path(value)| Self(value))
            .map_err(|_| {
                ApiError::with_code(
                    axum::http::StatusCode::BAD_REQUEST,
                    "invalid_path",
                    "path parameters are invalid",
                )
            })
    }
}

impl<T> aide::operation::OperationInput for ApiPath<T>
where
    T: JsonSchema,
{
    fn operation_input(
        context: &mut aide::generate::GenContext,
        operation: &mut aide::openapi::Operation,
    ) {
        <Path<T> as aide::operation::OperationInput>::operation_input(context, operation);
    }
}

/// Query extractor with one structured rejection shape across API handlers.
#[derive(Debug, Clone)]
pub struct ApiQuery<T>(pub T);

impl<T> FromRequestParts<Arc<AppState>> for ApiQuery<T>
where
    T: DeserializeOwned + Send,
{
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|_| {
                ApiError::with_code(
                    axum::http::StatusCode::BAD_REQUEST,
                    "invalid_query",
                    "query parameters are invalid",
                )
            })
    }
}

impl<T> aide::operation::OperationInput for ApiQuery<T>
where
    T: JsonSchema,
{
    fn operation_input(
        context: &mut aide::generate::GenContext,
        operation: &mut aide::openapi::Operation,
    ) {
        <Query<T> as aide::operation::OperationInput>::operation_input(context, operation);
    }
}

/// A server id extracted from the request path together with the canonical
/// server row.  Authorization remains actor-specific and is performed by the
/// handler/command service; this extractor only normalizes path lookup and the
/// not-found response.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ServerIdPath {
    pub server_id: Uuid,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct OAuthProviderPath {
    pub provider: crate::config::OAuthProvider,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ServerExportPath {
    pub server_id: Uuid,
    pub export_id: Uuid,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AccountSessionPath {
    pub session_id: Uuid,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ServerScopedSessionPath {
    pub session_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct ServerContext {
    pub id: Uuid,
    pub record: crate::model::ServerRecord,
}

impl FromRequestParts<Arc<AppState>> for ServerContext {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let Path(path) = Path::<ServerIdPath>::from_request_parts(parts, state)
            .await
            .map_err(|_| {
                ApiError::with_code(
                    axum::http::StatusCode::BAD_REQUEST,
                    "invalid_server_id",
                    "server id is invalid",
                )
            })?;
        let record = internal::server_by_id(&state.pool, path.server_id)
            .await?
            .ok_or_else(|| {
                ApiError::with_code(
                    axum::http::StatusCode::NOT_FOUND,
                    "server_not_found",
                    "server not found",
                )
            })?;
        Ok(Self {
            id: path.server_id,
            record,
        })
    }
}

impl aide::operation::OperationInput for ServerContext {
    fn operation_input(
        context: &mut aide::generate::GenContext,
        operation: &mut aide::openapi::Operation,
    ) {
        <Path<ServerIdPath> as aide::operation::OperationInput>::operation_input(
            context, operation,
        );
    }
}

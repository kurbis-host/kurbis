use std::{sync::LazyLock, time::Duration};

use anyhow::{Result, bail};
use uuid::Uuid;

use crate::{config::GatewayConfig, contracts::lifecycle::ServerCommandRequest};

/// One HTTP client for every control-plane call. Building a client loads the
/// system's root certificates, which took longer than the wake request itself
/// when each join built its own.
static CONTROL_PLANE: LazyLock<reqwest::Client> = LazyLock::new(reqwest::Client::new);

pub async fn request_wake(config: &GatewayConfig, server_id: Uuid) -> Result<()> {
    let status = send_wake(config, server_id).await?;
    if !status.is_success() {
        bail!("control plane rejected wake with {status}");
    }
    Ok(())
}

/// Ask the control plane to start a server whose last start failed.
///
/// Returns `false` when the control plane turns the retry down for now: it
/// spaces out join-triggered retries of a failing server, and a start is not
/// admitted until the worker has confirmed the failed runtime is gone.
pub async fn request_failed_server_retry(config: &GatewayConfig, server_id: Uuid) -> Result<bool> {
    let status = send_wake(config, server_id).await?;
    if status.is_success() {
        return Ok(true);
    }
    if status == reqwest::StatusCode::CONFLICT {
        return Ok(false);
    }
    bail!("control plane rejected the retry with {status}");
}

async fn send_wake(config: &GatewayConfig, server_id: Uuid) -> Result<reqwest::StatusCode> {
    let url = format!(
        "{}/api/v1/servers/{server_id}/commands",
        config.control_plane_url.trim_end_matches('/'),
    );
    let response = CONTROL_PLANE
        .post(url)
        .bearer_auth(&config.internal_token)
        .json(&ServerCommandRequest::Start {
            reason: Some("gateway_join".to_owned()),
        })
        .timeout(Duration::from_secs(10))
        .send()
        .await?;
    Ok(response.status())
}

pub async fn request_ci_preview_admission(
    config: &GatewayConfig,
    hostname: &str,
) -> Result<crate::ci::CiPreviewState> {
    let url = format!(
        "{}/internal/ci/previews/admit",
        config.control_plane_url.trim_end_matches('/')
    );
    let response = CONTROL_PLANE
        .post(url)
        .bearer_auth(&config.internal_token)
        .json(&serde_json::json!({ "hostname": hostname }))
        .timeout(Duration::from_secs(10))
        .send()
        .await?;
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap_or_default();
    let state = body
        .get("state")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("CI admission response omitted state"))?
        .parse()
        .map_err(|error: crate::ci::InvalidCiValue| anyhow::anyhow!(error))?;
    if !status.is_success() && status != reqwest::StatusCode::ACCEPTED {
        // Terminal preview states are returned with 503 to stop runtime
        // admission, but remain structured so the lobby can show why.
        if !matches!(
            state,
            crate::ci::CiPreviewState::Failed
                | crate::ci::CiPreviewState::Expired
                | crate::ci::CiPreviewState::Stopping
        ) {
            bail!("control plane rejected CI preview admission with {status}");
        }
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, path::PathBuf, time::Duration};

    use axum::{
        Json, Router,
        extract::{Path, State},
        http::{HeaderMap, StatusCode, header},
        routing::post,
    };
    use tokio::net::TcpListener;
    use uuid::Uuid;

    use super::*;

    #[derive(Clone)]
    struct WakeRequest {
        token: String,
        server_id: Uuid,
    }

    async fn accept_wake(
        State(request): State<WakeRequest>,
        Path(server_id): Path<Uuid>,
        headers: HeaderMap,
        Json(command): Json<ServerCommandRequest>,
    ) -> StatusCode {
        let expected_authorization = format!("Bearer {}", request.token);
        let is_gateway_join = matches!(
            command,
            ServerCommandRequest::Start {
                reason: Some(reason)
            } if reason == "gateway_join"
        );
        if headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            != Some(expected_authorization.as_str())
            || server_id != request.server_id
            || !is_gateway_join
        {
            return StatusCode::UNAUTHORIZED;
        }
        StatusCode::ACCEPTED
    }

    fn config(control_plane_url: String) -> GatewayConfig {
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
            internal_token: "wake-test-token".into(),
            root_domain: "example.test".into(),
            wake_timeout: Duration::from_secs(90),
            allow_offline_waiting_for_tests: false,
        }
    }

    #[tokio::test]
    async fn wake_request_posts_canonical_gateway_command_with_internal_bearer() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address: SocketAddr = listener.local_addr().unwrap();
        let server_id = Uuid::new_v4();
        let expected = WakeRequest {
            token: "wake-test-token".into(),
            server_id,
        };
        let app = Router::new()
            .route("/api/v1/servers/{server_id}/commands", post(accept_wake))
            .with_state(expected);
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        request_wake(&config(format!("http://{address}")), server_id)
            .await
            .unwrap();

        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn wake_request_surfaces_control_plane_rejection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/api/v1/servers/{server_id}/commands",
            post(|| async { StatusCode::NOT_FOUND }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let error = request_wake(&config(format!("http://{address}")), Uuid::new_v4())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("404"));

        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn failed_server_retry_is_deferred_by_a_conflict_and_fails_on_other_rejections() {
        for (status, expected) in [
            (StatusCode::OK, Some(true)),
            (StatusCode::CONFLICT, Some(false)),
            (StatusCode::FORBIDDEN, None),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let app = Router::new().route(
                "/api/v1/servers/{server_id}/commands",
                post(move || async move { status }),
            );
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

            let outcome =
                request_failed_server_retry(&config(format!("http://{address}")), Uuid::new_v4())
                    .await;
            assert_eq!(outcome.ok(), expected, "{status}");

            server.abort();
            assert!(server.await.unwrap_err().is_cancelled());
        }
    }
}

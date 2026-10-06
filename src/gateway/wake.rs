use std::time::Duration;

use anyhow::{Result, bail};
use uuid::Uuid;

use crate::{config::GatewayConfig, contracts::lifecycle::ServerCommandRequest};

pub async fn request_wake(config: &GatewayConfig, server_id: Uuid) -> Result<()> {
    let url = format!(
        "{}/api/v1/servers/{server_id}/commands",
        config.control_plane_url.trim_end_matches('/'),
    );
    let response = reqwest::Client::new()
        .post(url)
        .bearer_auth(&config.internal_token)
        .json(&ServerCommandRequest::Start {
            reason: Some("gateway_join".to_owned()),
        })
        .timeout(Duration::from_secs(10))
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("control plane rejected wake with {}", response.status());
    }
    Ok(())
}

pub async fn request_ci_preview_admission(
    config: &GatewayConfig,
    hostname: &str,
) -> Result<crate::ci::CiPreviewState> {
    let url = format!(
        "{}/internal/ci/previews/admit",
        config.control_plane_url.trim_end_matches('/')
    );
    let response = reqwest::Client::new()
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
}

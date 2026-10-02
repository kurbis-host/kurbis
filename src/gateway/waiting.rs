use std::{collections::HashMap, time::Duration};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::time::{Instant, sleep};
use uuid::Uuid;

use crate::{
    config::GatewayConfig,
    gateway::{routes::RouteCache, wake::request_wake},
    model::{ObservedState, RouteTarget},
    pumpkin_control::{PlayerEdition, PumpkinControl, VerifiedMinecraftIdentity, WaitingPlayer},
    routing::{AdmissionRequest, AdmissionResponse},
};

struct WaitingState {
    startup_since: Option<Instant>,
    last_notice: Option<String>,
    last_wake: Option<Instant>,
    transfer_sent: bool,
    login_link_sent: bool,
    admission_target: Option<RouteTarget>,
    admission_hostname: Option<String>,
    admission_complete: bool,
}

impl WaitingState {
    fn new() -> Self {
        Self {
            startup_since: None,
            last_notice: None,
            last_wake: None,
            transfer_sent: false,
            login_link_sent: false,
            admission_target: None,
            admission_hostname: None,
            admission_complete: false,
        }
    }

    fn startup_timed_out(
        &mut self,
        observed: ObservedState,
        now: Instant,
        limit: Duration,
    ) -> bool {
        if observed == ObservedState::Queued {
            // Capacity waiting has no startup deadline. Give the actual
            // restore/start flow its full budget when admission succeeds.
            self.startup_since = None;
            return false;
        }
        let since = self.startup_since.get_or_insert(now);
        now.duration_since(*since) >= limit
    }
}

pub async fn run(config: GatewayConfig, routes: RouteCache) -> Result<()> {
    let control = PumpkinControl::new(config.waiting_control_token.clone());
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let (java_online_authentication, bedrock_online_authentication) = control
        .online_authentication(&config.waiting_control_socket)
        .await?;
    let authentication_enabled = java_online_authentication && bedrock_online_authentication;
    if !authentication_enabled
        && !(cfg!(debug_assertions) && config.allow_offline_waiting_for_tests)
    {
        bail!("waiting Pumpkin must require online Java and Bedrock authentication");
    }
    if !authentication_enabled {
        tracing::warn!("offline waiting authentication is enabled for a disposable test fixture");
    }
    tracing::info!(
        control_socket = %config.waiting_control_socket.display(),
        bedrock_listen = %config.bedrock_listen,
        "waiting Pumpkin gateway is ready for Java and Bedrock players"
    );

    let mut states = HashMap::<Uuid, WaitingState>::new();
    loop {
        match control.players(&config.waiting_control_socket).await {
            Ok(players) => {
                let present = players
                    .iter()
                    .map(|player| player.connection_id)
                    .collect::<std::collections::HashSet<_>>();
                states.retain(|id, _| present.contains(id));
                for player in players {
                    let state = states
                        .entry(player.connection_id)
                        .or_insert_with(WaitingState::new);
                    if let Err(error) =
                        handle_player(&config, &routes, &control, &http, &player, state).await
                    {
                        tracing::warn!(player_id = %player.id, ?error, "failed to route waiting player");
                    }
                }
            }
            Err(error) => tracing::error!(?error, "failed to query waiting Pumpkin players"),
        }
        sleep(Duration::from_millis(500)).await;
    }
}

async fn handle_player(
    config: &GatewayConfig,
    routes: &RouteCache,
    control: &PumpkinControl,
    http: &reqwest::Client,
    player: &WaitingPlayer,
    state: &mut WaitingState,
) -> Result<()> {
    if state.transfer_sent {
        return Ok(());
    }
    // Bedrock exposes its player before resource-pack/world initialization is
    // complete. Sending a disconnect or notice at that point can race login.
    // Apply the same readiness boundary to all outbound routing actions.
    if !player.ready_for_transfer {
        return Ok(());
    }
    if !routes.accepts_server_address(&player.server_address) {
        control
            .kick(
                &config.waiting_control_socket,
                player.id,
                "Unknown or deleted server address.",
            )
            .await?;
        return Ok(());
    }

    if !state.login_link_sent
        && let Some(identity) = player.verified_identity.as_ref()
    {
        let url =
            create_minecraft_login_link(config, http, identity, player.id, &player.name).await?;
        control
            .message(
                &config.waiting_control_socket,
                player.id,
                &format!("Sign in to kurbis: {url}"),
            )
            .await?;
        state.login_link_sent = true;
    }

    if !state.admission_complete {
        match request_admission(config, http, player).await {
            Ok(admission) => {
                if let Some(message) = admission.private_message.as_deref()
                    && let Err(error) = control
                        .message(&config.waiting_control_socket, player.id, message)
                        .await
                {
                    tracing::warn!(player_id = %player.id, ?error, "failed to deliver private server redirect notice");
                }
                state.admission_hostname = Some(format!(
                    "{}.{}",
                    admission.target.subdomain, config.root_domain
                ));
                state.admission_target = Some(admission.target);
                state.admission_complete = true;
            }
            Err(error) => {
                if let Some(status) = error.status()
                    && status.is_client_error()
                {
                    control
                        .kick(
                            &config.waiting_control_socket,
                            player.id,
                            admission_error_message(status),
                        )
                        .await?;
                    return Ok(());
                }
                return Err(error.into());
            }
        }
    }

    let admitted_hostname = state
        .admission_hostname
        .as_deref()
        .context("admission did not set a server address")?;
    let target = routes
        .resolve(admitted_hostname)
        .await?
        .or_else(|| state.admission_target.clone());

    if target
        .as_ref()
        .is_some_and(|target| target.account_suspended)
    {
        control
            .kick(
                &config.waiting_control_socket,
                player.id,
                "Server suspended. Contact the hosting operator.",
            )
            .await?;
        return Ok(());
    }

    let Some(target) = target else {
        control
            .kick(
                &config.waiting_control_socket,
                player.id,
                "Unknown or deleted server address.",
            )
            .await?;
        return Ok(());
    };

    if target.state == ObservedState::Failed {
        control
            .kick(
                &config.waiting_control_socket,
                player.id,
                "Server failed to start. Retry it from the panel.",
            )
            .await?;
        return Ok(());
    }

    if target.worker_healthy == Some(false)
        && matches!(
            target.state,
            ObservedState::Online
                | ObservedState::Restoring
                | ObservedState::Starting
                | ObservedState::Stopping
                | ObservedState::Snapshotting
                | ObservedState::Resetting
        )
    {
        control
            .kick(
                &config.waiting_control_socket,
                player.id,
                "Server is temporarily unavailable because its assigned worker is unreachable.",
            )
            .await?;
        return Ok(());
    }

    if target.state == ObservedState::Online {
        return transfer(config, control, player, state, &target).await;
    }

    if matches!(
        target.state,
        ObservedState::Sleeping
            | ObservedState::Queued
            | ObservedState::Stopping
            | ObservedState::Snapshotting
    ) && state
        .last_wake
        .is_none_or(|last| last.elapsed() >= Duration::from_secs(5))
    {
        request_wake(config, target.server_id).await?;
        state.last_wake = Some(Instant::now());
    }

    let notice = notice_for(target.state);
    if state.last_notice.as_deref() != Some(notice) {
        control
            .message(&config.waiting_control_socket, player.id, notice)
            .await?;
        state.last_notice = Some(notice.to_owned());
    }

    if state.startup_timed_out(target.state, Instant::now(), config.wake_timeout) {
        control
            .kick(
                &config.waiting_control_socket,
                player.id,
                "Server is taking too long to start. Please reconnect shortly.",
            )
            .await?;
    }
    Ok(())
}

#[derive(Serialize)]
struct MinecraftLinkRequest<'a> {
    identity: &'a VerifiedMinecraftIdentity,
    minecraft_player_id: Uuid,
    display_name: &'a str,
}

async fn request_admission(
    config: &GatewayConfig,
    http: &reqwest::Client,
    player: &WaitingPlayer,
) -> Result<AdmissionResponse, reqwest::Error> {
    http.post(format!(
        "{}/internal/admission",
        config.control_plane_url.trim_end_matches('/')
    ))
    .bearer_auth(&config.internal_token)
    .json(&AdmissionRequest {
        hostname: player.server_address.clone(),
        identity: player.verified_identity.clone(),
        minecraft_player_id: player.verified_identity.as_ref().map(|_| player.id),
    })
    .send()
    .await?
    .error_for_status()?
    .json()
    .await
}

fn admission_error_message(status: reqwest::StatusCode) -> &'static str {
    match status {
        reqwest::StatusCode::FORBIDDEN => "This account is suspended or cannot create a server.",
        reqwest::StatusCode::NOT_FOUND => {
            "Unknown server address. Link this Minecraft account before creating a server."
        }
        reqwest::StatusCode::CONFLICT => "Server admission is changing. Please reconnect shortly.",
        reqwest::StatusCode::UNPROCESSABLE_ENTITY => "Minecraft identity could not be verified.",
        _ => "Server admission was rejected. Please reconnect shortly.",
    }
}

#[derive(Deserialize)]
struct MinecraftLinkResponse {
    url: String,
}

async fn create_minecraft_login_link(
    config: &GatewayConfig,
    http: &reqwest::Client,
    identity: &VerifiedMinecraftIdentity,
    minecraft_player_id: Uuid,
    display_name: &str,
) -> Result<String> {
    let response = http
        .post(format!(
            "{}/internal/minecraft-login-links",
            config.control_plane_url.trim_end_matches('/')
        ))
        .bearer_auth(&config.internal_token)
        .json(&MinecraftLinkRequest {
            identity,
            minecraft_player_id,
            display_name,
        })
        .send()
        .await?
        .error_for_status()?
        .json::<MinecraftLinkResponse>()
        .await?;
    Ok(response.url)
}

async fn transfer(
    config: &GatewayConfig,
    control: &PumpkinControl,
    player: &WaitingPlayer,
    state: &mut WaitingState,
    target: &RouteTarget,
) -> Result<()> {
    let (host, port) = transfer_target(config, target, player.edition)
        .ok_or_else(|| anyhow::anyhow!("online route has no address for player edition"))?;
    control
        .transfer(&config.waiting_control_socket, player.id, &host, port)
        .await?;
    state.transfer_sent = true;
    tracing::info!(
        player_id = %player.id,
        player_name = %player.name,
        edition = ?player.edition,
        %host,
        port,
        "transferred waiting player to ready backend"
    );
    Ok(())
}

fn notice_for(state: ObservedState) -> &'static str {
    match state {
        ObservedState::Queued => "Queued for capacity — your place is reserved.",
        ObservedState::Restoring => "Restoring your world…",
        ObservedState::Starting => "Starting your server…",
        ObservedState::Stopping | ObservedState::Snapshotting => "Saving before restart…",
        ObservedState::Resetting => "Resetting your server…",
        ObservedState::Sleeping => "Waking your server…",
        _ => "Preparing your server…",
    }
}

fn transfer_target(
    config: &GatewayConfig,
    target: &RouteTarget,
    edition: PlayerEdition,
) -> Option<(String, u16)> {
    match edition {
        PlayerEdition::Java => Some((
            target.worker_address.as_deref()?.to_owned(),
            target.java_port?,
        )),
        PlayerEdition::Bedrock => Some((
            format!("s-{}.{}", target.server_id, config.root_domain),
            config.bedrock_public_port,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_queue_does_not_consume_the_restore_start_timeout() {
        let mut state = WaitingState::new();
        let now = Instant::now();
        let limit = Duration::from_secs(90);
        assert!(!state.startup_timed_out(ObservedState::Sleeping, now, limit));
        assert!(!state.startup_timed_out(ObservedState::Queued, now + limit, limit));
        let admitted = now + Duration::from_secs(600);
        assert!(!state.startup_timed_out(ObservedState::Restoring, admitted, limit));
        assert!(!state.startup_timed_out(
            ObservedState::Starting,
            admitted + limit - Duration::from_secs(1),
            limit
        ));
        assert!(state.startup_timed_out(ObservedState::Starting, admitted + limit, limit));
        assert!(!state.startup_timed_out(ObservedState::Queued, admitted + limit, limit));
        assert!(!state.startup_timed_out(ObservedState::Restoring, admitted + limit * 2, limit));
    }

    fn online_target() -> RouteTarget {
        RouteTarget {
            server_id: Uuid::new_v4(),
            subdomain: "kurbis".into(),
            state: ObservedState::Online,
            account_suspended: false,
            worker_healthy: Some(true),
            worker_valid_until: None,
            worker_address: Some("203.0.113.10".into()),
            java_port: Some(25570),
            bedrock_port: Some(19140),
        }
    }

    #[test]
    fn bedrock_transfer_uses_gateway_alias_while_java_stays_direct_to_worker() {
        let config = GatewayConfig {
            common: crate::config::CommonConfig {
                database_url: String::new(),
            },
            java_listen: "127.0.0.1:25565".parse().unwrap(),
            bedrock_listen: "127.0.0.1:19132".parse().unwrap(),
            bedrock_public_port: 19132,
            bedrock_tls_certificate: "/unused/test-cert.pem".into(),
            bedrock_tls_private_key: "/unused/test-key.pem".into(),
            waiting_java_address: "127.0.0.1:25566".parse().unwrap(),
            waiting_bedrock_address: "127.0.0.1:19133".parse().unwrap(),
            waiting_control_socket: "/unused/control.sock".into(),
            waiting_control_token: "test".into(),
            control_plane_url: "http://127.0.0.1:8080".into(),
            internal_token: "test".into(),
            root_domain: "mc.example.test".into(),
            wake_timeout: Duration::from_secs(90),
            allow_offline_waiting_for_tests: false,
        };
        let target = online_target();
        assert_eq!(
            transfer_target(&config, &target, PlayerEdition::Java),
            Some(("203.0.113.10".into(), 25570))
        );
        assert_eq!(
            transfer_target(&config, &target, PlayerEdition::Bedrock),
            Some((format!("s-{}.mc.example.test", target.server_id), 19132))
        );
    }

    #[test]
    fn waiting_notices_distinguish_queue_restore_and_start() {
        assert_eq!(
            notice_for(ObservedState::Queued),
            "Queued for capacity — your place is reserved."
        );
        assert_eq!(
            notice_for(ObservedState::Restoring),
            "Restoring your world…"
        );
        assert_eq!(notice_for(ObservedState::Starting), "Starting your server…");
    }
}

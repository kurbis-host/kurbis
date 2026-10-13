use std::{collections::HashMap, time::Duration};

use anyhow::{Context, Result, bail};
use tokio::time::{Instant, sleep};
use uuid::Uuid;

use crate::{
    ci::{CI_ROOT_DOMAIN, CiPreviewState, ci_preview_chat_message, ci_preview_status_message},
    config::GatewayConfig,
    gateway::{
        routes::RouteCache,
        wake::{request_ci_preview_admission, request_failed_server_retry, request_wake},
    },
    model::{ObservedState, RouteTarget},
    pumpkin_control::{PlayerEdition, PumpkinControl, WaitingPlayer},
    routing::{AdmissionRequest, AdmissionResponse, CiRouteTarget},
};

const CI_ADMISSION_RETRY_INTERVAL: Duration = Duration::from_secs(5);
/// How often a waiting player re-asks for a failed server to be restarted.
const FAILED_RETRY_INTERVAL: Duration = Duration::from_secs(5);

struct WaitingState {
    startup_since: Option<Instant>,
    last_notice: Option<String>,
    last_wake: Option<Instant>,
    transfer_sent: bool,
    admission_target: Option<RouteTarget>,
    admission_hostname: Option<String>,
    admission_complete: bool,
    /// The verified player owns the server they are waiting for.
    owner: bool,
    /// Admission already saw the failed start, so an owner has been given
    /// the panel login link for it.
    admitted_while_failed: bool,
    last_failed_retry: Option<Instant>,
    last_ci_admission: Option<Instant>,
    ci_admission_state: Option<CiPreviewState>,
    /// CI chat notices are keyed by lifecycle state, not rendered text.
    /// Build logs can change while a state is active and must not spam one
    /// waiting connection. A reconnect gets a fresh WaitingState.
    last_ci_state: Option<CiPreviewState>,
}

impl WaitingState {
    fn new() -> Self {
        Self {
            startup_since: None,
            last_notice: None,
            last_wake: None,
            transfer_sent: false,
            admission_target: None,
            admission_hostname: None,
            admission_complete: false,
            owner: false,
            admitted_while_failed: false,
            last_failed_retry: None,
            last_ci_admission: None,
            ci_admission_state: None,
            last_ci_state: None,
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
    let mut route_changes = routes.changes();
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
        // A route change (a server coming online) moves its waiting players
        // on at once; the tick picks up players who have just arrived.
        tokio::select! {
            _ = sleep(Duration::from_millis(500)) => {}
            _ = route_changes.changed() => {}
        }
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
    if routes.accepts_ci_address(&player.server_address, CI_ROOT_DOMAIN) {
        return handle_ci_player(config, routes, control, player, state).await;
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
                state.owner = admission.owner;
                state.admitted_while_failed = admission.target.state == ObservedState::Failed;
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
        return handle_failed_server(config, control, player, state, &target).await;
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

    send_notice_once(config, control, player, state, notice_for(target.state)).await?;

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

async fn send_notice_once(
    config: &GatewayConfig,
    control: &PumpkinControl,
    player: &WaitingPlayer,
    state: &mut WaitingState,
    notice: &str,
) -> Result<()> {
    if state.last_notice.as_deref() != Some(notice) {
        control
            .message(&config.waiting_control_socket, player.id, notice)
            .await?;
        state.last_notice = Some(notice.to_owned());
    }
    Ok(())
}

/// Restart a server whose last start failed, on behalf of the joining player.
///
/// A failed server used to be reachable only through the panel's retry
/// button. An owner who registered by joining has no panel access until their
/// server runs, so the join itself has to be able to bring it back.
async fn handle_failed_server(
    config: &GatewayConfig,
    control: &PumpkinControl,
    player: &WaitingPlayer,
    state: &mut WaitingState,
    target: &RouteTarget,
) -> Result<()> {
    if !state.admitted_while_failed {
        // The start failed while this player was already waiting. Admit them
        // again so an owner receives the panel login link for the failure.
        state.admitted_while_failed = true;
        state.admission_complete = false;
        return Ok(());
    }
    if state
        .last_failed_retry
        .is_some_and(|last| last.elapsed() < FAILED_RETRY_INTERVAL)
    {
        return Ok(());
    }
    state.last_failed_retry = Some(Instant::now());
    if request_failed_server_retry(config, target.server_id).await? {
        // The new attempt gets the full startup budget.
        state.startup_since = None;
        return send_notice_once(
            config,
            control,
            player,
            state,
            "Your server failed to start last time. Trying again…",
        )
        .await;
    }
    if state.owner {
        // The owner stays so the login link in chat remains usable; the
        // retry above repeats and succeeds once the control plane allows it.
        return send_notice_once(
            config,
            control,
            player,
            state,
            "Your server failed to start. It is retried automatically every few minutes while you wait here.",
        )
        .await;
    }
    control
        .kick(
            &config.waiting_control_socket,
            player.id,
            "Server failed to start. It is retried automatically; reconnect in a few minutes.",
        )
        .await?;
    Ok(())
}

async fn handle_ci_player(
    config: &GatewayConfig,
    routes: &RouteCache,
    control: &PumpkinControl,
    player: &WaitingPlayer,
    state: &mut WaitingState,
) -> Result<()> {
    if state
        .last_ci_admission
        .is_none_or(|last| last.elapsed() >= CI_ADMISSION_RETRY_INTERVAL)
    {
        state.ci_admission_state =
            Some(request_ci_preview_admission(config, &player.server_address).await?);
        state.last_ci_admission = Some(Instant::now());
    }

    let target = routes
        .resolve_ci(&player.server_address, CI_ROOT_DOMAIN)
        .await?;
    let preview_state = target
        .as_ref()
        .map(|target| target.state)
        .or(state.ci_admission_state);

    if let Some(target) = target.as_ref()
        && target.state == CiPreviewState::Running
        && let Some((host, port)) = ci_transfer_target(config, player, target)
    {
        // Java players should see the final running transition in chat before
        // the transfer packet moves them to the canonical public route. The
        // Bedrock waiting flow intentionally keeps its existing transfer-only
        // behavior; Java is the protocol with the lobby/chat requirement.
        if player.edition == PlayerEdition::Java {
            send_java_ci_notice(control, config, player, state, target.state, target).await?;
        }
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
            preview = %target.preview_id,
            generation = target.runtime_generation,
            "transferred waiting player to running CI preview"
        );
        return Ok(());
    }

    match preview_state {
        Some(CiPreviewState::Failed) => {
            if player.edition == PlayerEdition::Java
                && let Some(target) = target.as_ref()
            {
                send_java_ci_notice(
                    control,
                    config,
                    player,
                    state,
                    CiPreviewState::Failed,
                    target,
                )
                .await?;
            }
            control
                .kick(
                    &config.waiting_control_socket,
                    player.id,
                    &ci_disconnect_message(
                        CiPreviewState::Failed,
                        target
                            .as_ref()
                            .and_then(|target| target.failure_message.as_deref()),
                        target
                            .as_ref()
                            .map_or("", |target| target.build_log.as_str()),
                    ),
                )
                .await?;
        }
        Some(CiPreviewState::Expired) | None => {
            if player.edition == PlayerEdition::Java
                && let Some(current) = preview_state
                && let Some(target) = target.as_ref()
            {
                send_java_ci_notice(control, config, player, state, current, target).await?;
            }
            control
                .kick(
                    &config.waiting_control_socket,
                    player.id,
                    "CI preview expired or the address is unknown. Check the alias and reconnect.",
                )
                .await?;
        }
        Some(current) => {
            if player.edition == PlayerEdition::Java {
                if let Some(target) = target.as_ref() {
                    send_java_ci_notice(control, config, player, state, current, target).await?;
                } else if state.last_ci_state != Some(current) {
                    let notice = ci_preview_chat_message(current, None, "");
                    control
                        .message(&config.waiting_control_socket, player.id, &notice)
                        .await?;
                    state.last_ci_state = Some(current);
                }
            } else {
                let notice = ci_notice(current);
                if state.last_notice.as_deref() != Some(notice) {
                    control
                        .message(&config.waiting_control_socket, player.id, notice)
                        .await?;
                    state.last_notice = Some(notice.to_owned());
                }
            }
        }
    }
    Ok(())
}

fn ci_transfer_target(
    config: &GatewayConfig,
    player: &WaitingPlayer,
    target: &CiRouteTarget,
) -> Option<(String, u16)> {
    match player.edition {
        PlayerEdition::Java => Some((target.hostname.clone(), config.java_public_port)),
        PlayerEdition::Bedrock => Some((target.hostname.clone(), config.bedrock_public_port)),
    }
}

fn ci_notice(state: CiPreviewState) -> &'static str {
    ci_preview_status_message(state).chat_message
}

fn ci_disconnect_message(
    state: CiPreviewState,
    failure_message: Option<&str>,
    build_log: &str,
) -> String {
    let message = ci_preview_chat_message(state, failure_message, build_log);
    format!("{message}\nReconnect after the CI build is fixed.")
}

fn java_ci_notice_for_transition(
    state: &WaitingState,
    current: CiPreviewState,
    target: &CiRouteTarget,
) -> Option<String> {
    (state.last_ci_state != Some(current)).then(|| {
        ci_preview_chat_message(
            current,
            target.failure_message.as_deref(),
            &target.build_log,
        )
    })
}

async fn send_java_ci_notice(
    control: &PumpkinControl,
    config: &GatewayConfig,
    player: &WaitingPlayer,
    state: &mut WaitingState,
    current: CiPreviewState,
    target: &CiRouteTarget,
) -> Result<()> {
    let Some(notice) = java_ci_notice_for_transition(state, current, target) else {
        return Ok(());
    };
    control
        .message(&config.waiting_control_socket, player.id, &notice)
        .await?;
    state.last_ci_state = Some(current);
    Ok(())
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
        display_name: player
            .verified_identity
            .as_ref()
            .map(|_| player.name.clone()),
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
            "Unknown server address. Join with online Minecraft authentication enabled to create it."
        }
        reqwest::StatusCode::CONFLICT => "Server admission is changing. Please reconnect shortly.",
        reqwest::StatusCode::UNPROCESSABLE_ENTITY => "Minecraft identity could not be verified.",
        _ => "Server admission was rejected. Please reconnect shortly.",
    }
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
            format!("{}.{}", target.subdomain, config.root_domain),
            config.java_public_port,
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
    fn java_ci_notice_is_once_per_state_transition_even_when_logs_change() {
        let mut waiting = WaitingState::new();
        let target = CiRouteTarget {
            preview_id: Uuid::new_v4(),
            hostname: "commit-preview.pumpkin.kurbis.host".into(),
            repository: "Pumpkin-MC/Pumpkin".into(),
            commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
            kind: crate::ci::CiRefKind::Commit,
            state: CiPreviewState::Compiling,
            build_id: Uuid::new_v4(),
            runtime_generation: 0,
            failure_message: None,
            build_log: "compile started".into(),
            expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            worker_address: None,
            java_port: None,
            bedrock_port: None,
        };
        let first = java_ci_notice_for_transition(&waiting, target.state, &target)
            .expect("first state sends a notice");
        assert!(first.contains("compile started"));
        waiting.last_ci_state = Some(target.state);

        let mut changed_log = target.clone();
        changed_log.build_log = "compile progressed".into();
        assert!(
            java_ci_notice_for_transition(&waiting, changed_log.state, &changed_log).is_none(),
            "log updates within a state must not spam a Java connection"
        );

        assert!(
            java_ci_notice_for_transition(&waiting, CiPreviewState::Packaging, &changed_log)
                .is_some()
        );
    }

    #[test]
    fn ci_waiting_lobby_covers_every_public_preview_state() {
        let cases = [
            (
                CiPreviewState::Lobby,
                "CI preview queued for its first build…",
            ),
            (
                CiPreviewState::Queued,
                "CI preview queued for its first build…",
            ),
            (CiPreviewState::Fetching, "Fetching the exact CI commit…"),
            (
                CiPreviewState::Patching,
                "Applying Kurbis integration patches…",
            ),
            (CiPreviewState::Compiling, "Compiling the CI preview…"),
            (CiPreviewState::Packaging, "Packaging the CI preview…"),
            (CiPreviewState::Booting, "Starting the CI preview runtime…"),
            (CiPreviewState::Ready, "Starting the CI preview runtime…"),
            (
                CiPreviewState::Running,
                "CI preview is ready. Transferring you now…",
            ),
            (CiPreviewState::Stopping, "CI preview is restarting…"),
            (CiPreviewState::Failed, "CI preview build failed."),
            (CiPreviewState::Expired, "CI preview expired."),
        ];
        for (state, expected) in cases {
            assert_eq!(ci_notice(state), expected);
        }
    }

    #[tokio::test]
    async fn first_join_admission_carries_verified_identity_and_display_name() {
        use axum::{Json, routing::post};
        use tokio::net::TcpListener;
        use tokio::sync::Mutex;

        let captured = std::sync::Arc::new(Mutex::new(None));
        let app = axum::Router::new().route(
            "/internal/admission",
            post({
                let captured = captured.clone();
                move |Json(request): Json<AdmissionRequest>| {
                    let captured = captured.clone();
                    async move {
                        *captured.lock().await = Some(request);
                        Json(AdmissionResponse {
                            target: RouteTarget {
                                server_id: Uuid::new_v4(),
                                subdomain: "steve".into(),
                                state: ObservedState::Sleeping,
                                account_suspended: false,
                                generation: 0,
                                worker_healthy: None,
                                worker_valid_until: None,
                                worker_address: None,
                                java_port: None,
                                bedrock_port: None,
                            },
                            private_message: None,
                            created: true,
                            owner: true,
                        })
                    }
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let player_id = Uuid::new_v4();
        let identity = crate::pumpkin_control::VerifiedMinecraftIdentity::Java { uuid: player_id };
        let player = WaitingPlayer {
            connection_id: Uuid::new_v4(),
            id: player_id,
            name: "Steve".into(),
            edition: PlayerEdition::Java,
            server_address: "steve.mc.example.test".into(),
            ready_for_transfer: true,
            verified_identity: Some(identity.clone()),
        };
        let config = GatewayConfig {
            common: crate::config::CommonConfig {
                database_url: String::new(),
            },
            java_listen: "127.0.0.1:0".parse().unwrap(),
            java_public_port: 25565,
            bedrock_listen: "127.0.0.1:0".parse().unwrap(),
            bedrock_public_port: 19132,
            bedrock_public_address: "127.0.0.1".parse().unwrap(),
            bedrock_tls_certificate: "/unused/test-cert.pem".into(),
            bedrock_tls_private_key: "/unused/test-key.pem".into(),
            waiting_java_address: "127.0.0.1:25566".parse().unwrap(),
            waiting_bedrock_address: "127.0.0.1:19133".parse().unwrap(),
            waiting_signal_address: "127.0.0.1:19134".parse().unwrap(),
            waiting_control_socket: "/unused/control.sock".into(),
            waiting_control_token: "test".into(),
            control_plane_url: format!("http://{address}"),
            internal_token: "test".into(),
            root_domain: "mc.example.test".into(),
            wake_timeout: Duration::from_secs(90),
            allow_offline_waiting_for_tests: false,
        };

        let admission = request_admission(&config, &reqwest::Client::new(), &player)
            .await
            .unwrap();
        assert!(admission.created);
        let request = captured.lock().await.take().unwrap();
        assert_eq!(request.hostname, "steve.mc.example.test");
        assert_eq!(request.identity, Some(identity));
        assert_eq!(request.minecraft_player_id, Some(player_id));
        assert_eq!(request.display_name.as_deref(), Some("Steve"));

        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }

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
            generation: 1,
            worker_healthy: Some(true),
            worker_valid_until: None,
            worker_address: Some("203.0.113.10".into()),
            java_port: Some(25570),
            bedrock_port: Some(19140),
        }
    }

    #[test]
    fn bedrock_and_java_transfers_use_the_shared_gateway_addresses() {
        let config = GatewayConfig {
            common: crate::config::CommonConfig {
                database_url: String::new(),
            },
            java_listen: "127.0.0.1:25565".parse().unwrap(),
            java_public_port: 25565,
            bedrock_listen: "127.0.0.1:19132".parse().unwrap(),
            bedrock_public_port: 19132,
            bedrock_public_address: "127.0.0.1".parse().unwrap(),
            bedrock_tls_certificate: "/unused/test-cert.pem".into(),
            bedrock_tls_private_key: "/unused/test-key.pem".into(),
            waiting_java_address: "127.0.0.1:25566".parse().unwrap(),
            waiting_bedrock_address: "127.0.0.1:19133".parse().unwrap(),
            waiting_signal_address: "127.0.0.1:19134".parse().unwrap(),
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
            Some((format!("{}.mc.example.test", target.subdomain), 25565))
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

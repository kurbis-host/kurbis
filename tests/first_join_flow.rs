//! Full control-plane/gateway admission-to-transfer regression. The fixture injects a
//! player only after the same `WaitingPlayer` contract Pumpkin emits following
//! successful Java online-mode or Bedrock/Xbox authentication. It runs the actual
//! HTTP admission, route cache, wake endpoint, waiting-player loop, and lifecycle
//! database transitions; only Pumpkin's Unix host-control transport and worker
//! process are controlled fixtures. `complete_start` is the worker's readiness
//! acknowledgement boundary, and the test proves transfer is withheld until it.

mod common;

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use common::{api_state, test_pool};
use kurbis_hoster::{
    config::{CommonConfig, GatewayConfig},
    control_plane::{
        api::router,
        lifecycle::{
            claim_operation, complete_start, fail_operation, healthy_worker_template,
            heartbeat_worker, mark_runtime_lost,
        },
    },
    gateway::{routes::RouteCache, waiting},
    model::{ObservedState, OperationKind},
    pumpkin_control::{PlayerEdition, VerifiedMinecraftIdentity, WaitingPlayer},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, UnixListener},
    sync::Mutex,
    time::{sleep, timeout},
};
use uuid::Uuid;

const INTERNAL_TOKEN: &str = "first-join-flow-internal-token";
const CONTROL_TOKEN: &str = "first-join-flow-pumpkin-control-token";
const ROOT_DOMAIN: &str = "mc.example.test";

static DB_TEST_LOCK: Mutex<()> = Mutex::const_new(());

#[derive(Default)]
struct FakeHostControlState {
    player: Option<WaitingPlayer>,
    messages: Vec<String>,
    chat_messages: Vec<String>,
    transfers: Vec<(Uuid, String, u16)>,
    kicks: Vec<(Uuid, String)>,
    status_requests: usize,
    empty_player_polls: usize,
    player_polls: usize,
}

async fn spawn_host_control(
    socket: PathBuf,
    token: &'static str,
    state: Arc<Mutex<FakeHostControlState>>,
) -> tokio::task::JoinHandle<()> {
    let listener = UnixListener::bind(socket).unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let state = state.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut line = Vec::new();
                let mut reader = BufReader::new(read);
                if reader.read_until(b'\n', &mut line).await.is_err() {
                    return;
                }
                let request: Value = match serde_json::from_slice(&line) {
                    Ok(request) => request,
                    Err(_) => return,
                };
                if request["token"] != token {
                    let _ = write
                        .write_all(b"{\"ok\":false,\"error\":\"unauthorized\"}\n")
                        .await;
                    return;
                }
                let response = match request["action"].as_str() {
                    Some("status") => {
                        state.lock().await.status_requests += 1;
                        json!({
                            "ok": true,
                            "online_authentication": {"java": true, "bedrock": true}
                        })
                    }
                    Some("players") => {
                        let mut state = state.lock().await;
                        state.player_polls += 1;
                        if state.player.is_none() {
                            state.empty_player_polls += 1;
                        }
                        json!({
                            "ok": true,
                            "waiting_players": state.player.iter().cloned().collect::<Vec<_>>()
                        })
                    }
                    Some("message") => {
                        let mut state = state.lock().await;
                        if let Some(message) = request["message"].as_str() {
                            state.messages.push(message.to_owned());
                            if request["chat"].as_bool() == Some(true) {
                                state.chat_messages.push(message.to_owned());
                            }
                        }
                        json!({"ok": true})
                    }
                    Some("transfer") => {
                        let mut state = state.lock().await;
                        let player_id = request["player_id"]
                            .as_str()
                            .and_then(|id| Uuid::parse_str(id).ok());
                        let host = request["host"].as_str().map(str::to_owned);
                        let port = request["port"]
                            .as_u64()
                            .and_then(|port| u16::try_from(port).ok());
                        if let (Some(player_id), Some(host), Some(port)) = (player_id, host, port) {
                            state.transfers.push((player_id, host, port));
                        }
                        json!({"ok": true})
                    }
                    Some("kick") => {
                        let mut state = state.lock().await;
                        let player_id = request["player_id"]
                            .as_str()
                            .and_then(|id| Uuid::parse_str(id).ok());
                        if let (Some(player_id), Some(message)) =
                            (player_id, request["message"].as_str())
                        {
                            state.kicks.push((player_id, message.to_owned()));
                            if state
                                .player
                                .as_ref()
                                .is_some_and(|player| player.id == player_id)
                            {
                                state.player = None;
                            }
                        }
                        json!({"ok": true})
                    }
                    _ => json!({"ok": false, "error": "unsupported_fixture_action"}),
                };
                let mut bytes = serde_json::to_vec(&response).unwrap();
                bytes.push(b'\n');
                let _ = write.write_all(&bytes).await;
            });
        }
    })
}

fn gateway_config(control_plane_url: String, waiting_control_socket: PathBuf) -> GatewayConfig {
    GatewayConfig {
        common: CommonConfig {
            database_url: std::env::var("TEST_DATABASE_URL").expect("disposable TEST_DATABASE_URL"),
        },
        java_listen: "127.0.0.1:0".parse().unwrap(),
        java_public_port: 25565,
        bedrock_listen: "127.0.0.1:0".parse().unwrap(),
        bedrock_public_address: "127.0.0.1".parse().unwrap(),
        bedrock_public_port: 19132,
        bedrock_tls_certificate: PathBuf::new(),
        bedrock_tls_private_key: PathBuf::new(),
        waiting_java_address: "127.0.0.1:25566".parse().unwrap(),
        waiting_bedrock_address: "127.0.0.1:19133".parse().unwrap(),
        waiting_signal_address: "127.0.0.1:19134".parse().unwrap(),
        waiting_control_socket,
        waiting_control_token: CONTROL_TOKEN.into(),
        control_plane_url,
        internal_token: INTERNAL_TOKEN.into(),
        root_domain: ROOT_DOMAIN.into(),
        wake_timeout: Duration::from_secs(20),
        allow_offline_waiting_for_tests: false,
    }
}

async fn start_services(
    pool: PgPool,
    control_state: Arc<Mutex<FakeHostControlState>>,
) -> (
    RouteCache,
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
    PathBuf,
) {
    let control_plane = router(api_state(
        pool.clone(),
        INTERNAL_TOKEN,
        Arc::new(object_store::memory::InMemory::new()),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    let control_plane_task = tokio::spawn(async move {
        axum::serve(listener, control_plane).await.unwrap();
    });

    let socket = std::env::temp_dir().join(format!(
        "kj-{}.sock",
        &Uuid::new_v4().simple().to_string()[..8]
    ));
    let host_control_task =
        spawn_host_control(socket.clone(), CONTROL_TOKEN, control_state.clone()).await;
    let routes = RouteCache::new(pool, ROOT_DOMAIN.into());
    let listener = routes.invalidation_listener().await.unwrap();
    let invalidation_routes = routes.clone();
    let invalidation_task = tokio::spawn(async move {
        invalidation_routes
            .listen_for_invalidations(listener)
            .await
            .unwrap();
    });
    let config = gateway_config(format!("http://{address}"), socket.clone());
    let waiting_routes = routes.clone();
    let waiting_task = tokio::spawn(async move {
        waiting::run(config, waiting_routes).await.unwrap();
    });

    // The waiting runtime requires its authenticated status request before it
    // starts polling players; wait for the fixture to observe that real request.
    timeout(Duration::from_secs(5), async {
        loop {
            if control_state.lock().await.status_requests > 0 {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("waiting gateway completed authenticated host-control startup");

    (
        routes,
        control_plane_task,
        host_control_task,
        invalidation_task,
        waiting_task,
        socket,
    )
}

async fn wait_for_server(pool: &PgPool, subdomain: &str) -> (Uuid, Uuid, ObservedState) {
    timeout(Duration::from_secs(10), async {
        loop {
            if let Some(row) = sqlx::query_as::<_, (Uuid, Uuid, ObservedState)>(
                "SELECT id, owner_id, observed_state FROM servers WHERE subdomain = $1",
            )
            .bind(subdomain)
            .fetch_optional(pool)
            .await
            .unwrap()
            {
                return row;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("verified first join created a server")
}

async fn wait_for_start_operation(pool: &PgPool, server_id: Uuid) -> i64 {
    timeout(Duration::from_secs(10), async {
        loop {
            let result: Option<i64> = sqlx::query_scalar(
                "SELECT generation FROM operations WHERE server_id = $1 AND kind = 'start' AND status = 'pending'",
            )
            .bind(server_id)
            .fetch_optional(pool)
            .await
            .unwrap();
            if let Some(generation) = result {
                return generation;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("gateway wake enqueued a worker start")
}

async fn wait_for_control_event(
    state: &Arc<Mutex<FakeHostControlState>>,
    predicate: impl Fn(&FakeHostControlState) -> bool,
    description: &str,
) {
    timeout(Duration::from_secs(10), async {
        loop {
            let guard = state.lock().await;
            if predicate(&guard) {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {description}"));
}

async fn assert_single_start(pool: &PgPool, server_id: Uuid, expected: i64) {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations WHERE server_id = $1 AND kind = 'start'",
    )
    .bind(server_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(count, expected, "start admission must be idempotent");
}

fn player(edition: PlayerEdition) -> (WaitingPlayer, VerifiedMinecraftIdentity, String) {
    let player_id = Uuid::new_v4();
    let (identity, subject) = match edition {
        PlayerEdition::Java => (
            VerifiedMinecraftIdentity::Java { uuid: player_id },
            player_id.to_string(),
        ),
        PlayerEdition::Bedrock => {
            let xuid = Uuid::new_v4().as_u64_pair().0.to_string();
            (
                VerifiedMinecraftIdentity::Bedrock { xuid: xuid.clone() },
                xuid,
            )
        }
    };
    (
        WaitingPlayer {
            connection_id: Uuid::new_v4(),
            id: player_id,
            name: "FirstJoinPlayer".into(),
            edition,
            server_address: format!(
                "first-{}.{}",
                &player_id.simple().to_string()[..12],
                ROOT_DOMAIN
            ),
            ready_for_transfer: true,
            verified_identity: Some(identity.clone()),
        },
        identity,
        subject,
    )
}

async fn provision_worker(pool: &PgPool) -> Uuid {
    let worker_id = Uuid::new_v4();
    heartbeat_worker(
        pool,
        &healthy_worker_template(
            worker_id,
            format!("first-join-worker-{worker_id}"),
            "127.0.0.1".into(),
            8 * 1024 * 1024 * 1024,
            40 * 1024 * 1024 * 1024,
            3_000,
            30_000,
            30_020,
        ),
    )
    .await
    .unwrap();
    worker_id
}

async fn wait_for_starting_route(pool: &PgPool, server_id: Uuid) {
    timeout(Duration::from_secs(10), async {
        loop {
            let state: ObservedState =
                sqlx::query_scalar("SELECT observed_state FROM servers WHERE id = $1")
                    .bind(server_id)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            if matches!(state, ObservedState::Restoring | ObservedState::Starting) {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("first join's wake reached worker startup state");
}

async fn finish_services(
    pool: &PgPool,
    owner_id: Uuid,
    worker_id: Uuid,
    tasks: Vec<tokio::task::JoinHandle<()>>,
) {
    for task in tasks {
        task.abort();
        let _ = task.await;
    }
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(owner_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workers WHERE id = $1")
        .bind(worker_id)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn first_verified_java_and_bedrock_join_wake_and_transfer_without_reconnect() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    for edition in [PlayerEdition::Java, PlayerEdition::Bedrock] {
        let (waiting_player, identity, subject) = player(edition);
        let hostname = waiting_player.server_address.clone();
        let player_id = waiting_player.id;
        let worker_id = provision_worker(&pool).await;
        let control_state = Arc::new(Mutex::new(FakeHostControlState {
            player: Some(waiting_player),
            ..FakeHostControlState::default()
        }));
        let (
            routes,
            control_plane_task,
            host_control_task,
            invalidation_task,
            waiting_task,
            socket,
        ) = start_services(pool.clone(), control_state.clone()).await;
        // Starting the listener in start_services also launches the waiting
        // gateway. Its first call creates the user/server from the verified
        // Minecraft identity and requests a wake for this same connection.
        let (server_id, owner_id, state) = wait_for_server(
            &pool,
            hostname.strip_suffix(&format!(".{ROOT_DOMAIN}")).unwrap(),
        )
        .await;
        assert!(matches!(
            state,
            ObservedState::Sleeping | ObservedState::Restoring | ObservedState::Starting
        ));
        let generation = wait_for_start_operation(&pool, server_id).await;
        wait_for_starting_route(&pool, server_id).await;
        assert_single_start(&pool, server_id, 1).await;
        {
            let state = control_state.lock().await;
            assert!(
                state.transfers.is_empty(),
                "must not transfer before worker readiness"
            );
            assert!(state.kicks.is_empty());
            assert!(
                state.messages.iter().any(|message| matches!(
                    message.as_str(),
                    "Waking your server…" | "Restoring your world…" | "Starting your server…"
                )),
                "the uninterrupted connection should remain held in the waiting runtime"
            );
            assert!(
                !state.chat_messages.is_empty(),
                "gateway lifecycle status should request a chat copy"
            );
        }

        let operation = claim_operation(&pool, worker_id)
            .await
            .unwrap()
            .expect("worker claims first-join start");
        assert_eq!(operation.server_id, server_id);
        assert_eq!(operation.generation, generation);
        assert_eq!(operation.kind, OperationKind::Start);
        complete_start(&pool, &operation, 0).await.unwrap();
        routes.invalidate_all().await;

        let (expected_host, expected_port): (String, u16) = match edition {
            // The waiting gateway transfers to the canonical public route,
            // not the worker's private address/leased port. This keeps the
            // connection behind the shared Gateway and mirrors production.
            PlayerEdition::Java => (hostname.clone(), 25565),
            PlayerEdition::Bedrock => (format!("s-{server_id}.{ROOT_DOMAIN}"), 19132),
        };
        wait_for_control_event(
            &control_state,
            |state| state.transfers.len() == 1,
            "a single transfer after readiness",
        )
        .await;
        {
            let state = control_state.lock().await;
            assert_eq!(
                state.transfers,
                vec![(player_id, expected_host.clone(), expected_port)]
            );
            assert!(state.kicks.is_empty());
        }
        let (observed, active, current_generation): (ObservedState, bool, i64) = sqlx::query_as(
            "SELECT s.observed_state, p.active, p.generation FROM servers s JOIN placements p ON p.server_id = s.id WHERE s.id = $1",
        )
        .bind(server_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            (observed, active, current_generation),
            (ObservedState::Online, true, generation)
        );
        assert_single_start(&pool, server_id, 1).await;

        let (provider, expected_subject) = match identity {
            VerifiedMinecraftIdentity::Java { uuid } => ("minecraft_java", uuid.to_string()),
            VerifiedMinecraftIdentity::Bedrock { xuid } => ("minecraft_bedrock", xuid),
        };
        assert_eq!(subject, expected_subject);
        let registered_subject: Option<String> = sqlx::query_scalar(
            "SELECT subject FROM identities WHERE user_id = $1 AND provider = $2",
        )
        .bind(owner_id)
        .bind(provider)
        .fetch_optional(&pool)
        .await
        .unwrap();
        assert_eq!(registered_subject.as_deref(), Some(subject.as_str()));
        let browser_sessions: i64 =
            sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
                .bind(owner_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            browser_sessions, 0,
            "first server creation must not require web UI login"
        );

        // Observe another poll of the same still-present authenticated
        // connection. A second transfer would mean state was lost/replayed.
        let poll_count = control_state.lock().await.player_polls;
        timeout(Duration::from_secs(5), async {
            loop {
                if control_state.lock().await.player_polls > poll_count {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("waiting gateway continued polling after the transfer");
        assert_eq!(control_state.lock().await.transfers.len(), 1);
        finish_services(
            &pool,
            owner_id,
            worker_id,
            vec![
                control_plane_task,
                host_control_task,
                invalidation_task,
                waiting_task,
            ],
        )
        .await;
        let _ = tokio::fs::remove_file(&socket).await;
    }
}

#[tokio::test]
async fn failed_start_keeps_the_owner_in_the_lobby_with_a_login_link_until_the_join_retries_it() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (waiting_player, _identity, _subject) = player(PlayerEdition::Java);
    let hostname = waiting_player.server_address.clone();
    let worker_id = provision_worker(&pool).await;
    let control_state = Arc::new(Mutex::new(FakeHostControlState {
        player: Some(waiting_player),
        ..FakeHostControlState::default()
    }));
    let (routes, control_plane_task, host_control_task, invalidation_task, waiting_task, socket) =
        start_services(pool.clone(), control_state.clone()).await;
    let (server_id, owner_id, _) = wait_for_server(
        &pool,
        hostname.strip_suffix(&format!(".{ROOT_DOMAIN}")).unwrap(),
    )
    .await;
    let generation = wait_for_start_operation(&pool, server_id).await;
    wait_for_starting_route(&pool, server_id).await;
    let first_start = claim_operation(&pool, worker_id).await.unwrap().unwrap();
    fail_operation(&pool, &first_start, "fixture Pumpkin failed readiness")
        .await
        .unwrap();
    routes.invalidate_all().await;
    // The first-join player owns the server and has no panel access yet.
    // Instead of being disconnected they stay in the lobby and are handed a
    // panel login link for the failure.
    wait_for_control_event(
        &control_state,
        |state| {
            state
                .messages
                .iter()
                .any(|message| message.contains("/auth/server#token="))
        },
        "owner login link after the failed start",
    )
    .await;
    wait_for_control_event(
        &control_state,
        |state| {
            state
                .messages
                .iter()
                .any(|message| message.contains("retried automatically"))
        },
        "deferred retry notice",
    )
    .await;
    {
        let state = control_state.lock().await;
        assert!(state.transfers.is_empty());
        assert!(state.kicks.is_empty(), "{:?}", state.kicks);
    }
    // The failed runtime is not confirmed gone and the failure is seconds
    // old, so the waiting join must not blindly start another copy.
    assert_single_start(&pool, server_id, 1).await;

    // The worker confirms cleanup and the retry spacing elapses.
    mark_runtime_lost(
        &pool,
        worker_id,
        server_id,
        generation,
        "fixture runtime removal confirmed",
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE operations SET updated_at = now() - interval '61 seconds' WHERE server_id = $1",
    )
    .bind(server_id)
    .execute(&pool)
    .await
    .unwrap();
    routes.invalidate_all().await;

    // The same waiting connection now restarts the server by itself: nobody
    // opens the panel and the player does not have to reconnect.
    let second_start = timeout(Duration::from_secs(20), async {
        loop {
            if let Some(operation) = claim_operation(&pool, worker_id).await.unwrap() {
                break operation;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the waiting join retries the failed start");
    assert_eq!(second_start.generation, generation + 1);
    complete_start(&pool, &second_start, 0).await.unwrap();
    routes.invalidate_all().await;
    wait_for_control_event(
        &control_state,
        |state| state.transfers.len() == 1,
        "transfer after the join-triggered retry",
    )
    .await;
    assert!(control_state.lock().await.kicks.is_empty());
    let starts: Vec<(String, i64)> = sqlx::query_as(
        "SELECT status::text, generation FROM operations WHERE server_id = $1 AND kind = 'start' ORDER BY generation",
    )
    .bind(server_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        starts,
        vec![
            ("failed".into(), generation),
            ("succeeded".into(), generation + 1)
        ]
    );
    finish_services(
        &pool,
        owner_id,
        worker_id,
        vec![
            control_plane_task,
            host_control_task,
            invalidation_task,
            waiting_task,
        ],
    )
    .await;
    let _ = tokio::fs::remove_file(&socket).await;
}

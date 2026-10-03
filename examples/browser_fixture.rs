//! Local-only browser test services. The panel/API/Postgres stay real; this
//! fixture replaces the external OAuth provider and Docker/Pumpkin worker.
use anyhow::{Result, bail};
use axum::{
    Form, Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::Redirect,
    routing::{get, post},
};
use kurbis_hoster::{
    config::ObjectStoreConfig,
    control_plane::lifecycle::{
        Lifecycle, claim_operation, complete_delete, complete_reset, complete_restore,
        complete_snapshot, complete_start, complete_stop, deactivate_placement_after_stop,
        healthy_worker_template, heartbeat_worker, validate_delete,
    },
    control_plane::snapshots::SnapshotStore,
    db,
    model::{OperationKind, SnapshotReason},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Clone)]
struct Fixture {
    pool: PgPool,
    lifecycle: Lifecycle,
    worker: Uuid,
    panel_origin: String,
    codes: Arc<Mutex<HashMap<String, (String, String)>>>,
    tokens: Arc<Mutex<HashMap<String, (String, String)>>>,
    identity: Arc<Mutex<String>>,
    paused: Arc<Mutex<bool>>,
    email_verified: Arc<Mutex<bool>>,
    snapshots: SnapshotStore,
    root: std::path::PathBuf,
}
#[derive(Deserialize)]
struct Authorization {
    redirect_uri: String,
    state: String,
    client_id: String,
    scope: String,
}
#[derive(Deserialize)]
struct Token {
    code: String,
    client_id: String,
    client_secret: String,
    redirect_uri: String,
}
#[derive(Deserialize)]
struct Capacity {
    available: bool,
}
#[derive(Deserialize)]
struct Identity {
    subject: String,
}
#[derive(Deserialize)]
struct Pause {
    paused: bool,
}
#[derive(Deserialize)]
struct EmailVerification {
    verified: bool,
}
#[derive(Deserialize)]
struct ConsoleExecution {
    server_id: Uuid,
    command_id: Uuid,
    #[serde(default = "default_console_success")]
    succeeded: bool,
    #[serde(default)]
    output: Vec<String>,
    #[serde(default)]
    error: Option<String>,
}

fn default_console_success() -> bool {
    true
}

async fn authorize(
    State(f): State<Fixture>,
    Query(q): Query<Authorization>,
) -> Result<axum::response::Response, StatusCode> {
    use axum::response::IntoResponse;
    let provider =
        oauth_provider(&q.redirect_uri, &f.panel_origin).ok_or(StatusCode::BAD_REQUEST)?;
    if q.client_id != client_id(provider) || q.scope != scopes(provider) || q.state.len() < 32 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let code = Uuid::new_v4().to_string();
    let subject = f.identity.lock().await.clone();
    f.codes
        .lock()
        .await
        .insert(code.clone(), (provider.to_owned(), subject));
    if *f.paused.lock().await {
        return Ok(Json(json!({"state":q.state})).into_response());
    }
    let mut url = reqwest::Url::parse(&q.redirect_uri).unwrap();
    url.query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &q.state);
    Ok(Redirect::temporary(url.as_str()).into_response())
}
async fn token(State(f): State<Fixture>, Form(q): Form<Token>) -> Result<Json<Value>, StatusCode> {
    let provider =
        oauth_provider(&q.redirect_uri, &f.panel_origin).ok_or(StatusCode::UNAUTHORIZED)?;
    if q.client_id != client_id(provider) || q.client_secret != client_secret(provider) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let (code_provider, subject) = f
        .codes
        .lock()
        .await
        .remove(&q.code)
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if code_provider != provider {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let access_token = Uuid::new_v4().to_string();
    f.tokens
        .lock()
        .await
        .insert(access_token.clone(), (provider.to_owned(), subject));
    Ok(Json(json!({"access_token":access_token})))
}
async fn profile(
    State(f): State<Fixture>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let (provider, subject) = f
        .tokens
        .lock()
        .await
        .get(token)
        .cloned()
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let name = if subject == "browser-user" {
        "Browser Player"
    } else {
        "Other Player"
    };
    let email = if subject == "browser-user" {
        "browser@example.test".to_owned()
    } else {
        format!("{subject}@example.test")
    };
    let email_verified = *f.email_verified.lock().await;
    let profile = match provider.as_str() {
        "google" => json!({
            "sub": subject,
            "name": name,
            "email": email,
            "email_verified": email_verified
        }),
        "discord" => json!({
            "id": subject,
            "username": name,
            "global_name": name,
            "email": email,
            "verified": email_verified
        }),
        _ => return Err(StatusCode::UNAUTHORIZED),
    };
    Ok(Json(profile))
}

fn scopes(provider: &str) -> &'static str {
    match provider {
        "google" => "openid email profile",
        "discord" => "identify email",
        _ => "",
    }
}

fn oauth_provider(redirect_uri: &str, panel_origin: &str) -> Option<&'static str> {
    let url = reqwest::Url::parse(redirect_uri).ok()?;
    if url.origin().ascii_serialization() != panel_origin {
        return None;
    }
    match url.path() {
        "/auth/google/callback" => Some("google"),
        "/auth/discord/callback" => Some("discord"),
        _ => None,
    }
}

fn client_id(provider: &str) -> &'static str {
    match provider {
        "google" => "browser-google-client",
        "discord" => "browser-discord-client",
        _ => "",
    }
}

fn client_secret(provider: &str) -> &'static str {
    match provider {
        "google" => "browser-google-secret",
        "discord" => "browser-discord-secret",
        _ => "",
    }
}
async fn identity(State(f): State<Fixture>, Json(q): Json<Identity>) -> StatusCode {
    *f.identity.lock().await = q.subject;
    StatusCode::NO_CONTENT
}
async fn pause(State(f): State<Fixture>, Json(q): Json<Pause>) -> StatusCode {
    *f.paused.lock().await = q.paused;
    StatusCode::NO_CONTENT
}
async fn email_verification(
    State(f): State<Fixture>,
    Json(q): Json<EmailVerification>,
) -> StatusCode {
    *f.email_verified.lock().await = q.verified;
    StatusCode::NO_CONTENT
}
async fn reset(State(f): State<Fixture>) -> Result<StatusCode, String> {
    sqlx::query("DELETE FROM minecraft_login_tokens")
        .execute(&f.pool)
        .await
        .map_err(err)?;
    sqlx::query("DELETE FROM server_instance_capabilities")
        .execute(&f.pool)
        .await
        .map_err(err)?;
    sqlx::query("DELETE FROM placements")
        .execute(&f.pool)
        .await
        .map_err(err)?;
    sqlx::query("DELETE FROM users")
        .execute(&f.pool)
        .await
        .map_err(err)?;
    sqlx::query("DELETE FROM workers WHERE id = $1")
        .bind(f.worker)
        .execute(&f.pool)
        .await
        .map_err(err)?;
    f.codes.lock().await.clear();
    f.tokens.lock().await.clear();
    *f.identity.lock().await = "browser-user".into();
    *f.paused.lock().await = false;
    *f.email_verified.lock().await = true;
    Ok(StatusCode::NO_CONTENT)
}
async fn capacity(State(f): State<Fixture>, Json(q): Json<Capacity>) -> Result<StatusCode, String> {
    let mut worker = healthy_worker_template(
        f.worker,
        "browser-worker".into(),
        "127.0.0.1".into(),
        8 * 1024 * 1024 * 1024,
        40 * 1024 * 1024 * 1024,
        4000,
        32000,
        32100,
    );
    worker.draining = !q.available;
    heartbeat_worker(&f.pool, &worker).await.map_err(err)?;
    f.lifecycle.drain_queue_once().await.map_err(err)?;
    Ok(StatusCode::NO_CONTENT)
}
async fn execute_console_command(
    State(f): State<Fixture>,
    Json(request): Json<ConsoleExecution>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if request.output.len() > 100
        || request.output.iter().any(|line| line.len() > 4096)
        || request.output.iter().map(String::len).sum::<usize>() > 16 * 1024
    {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "console fixture output is too large".into(),
        ));
    }
    if !request.succeeded && request.error.as_deref().unwrap_or_default().is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "failed fixture execution requires an error".into(),
        ));
    }

    let mut tx = f.pool.begin().await.map_err(fixture_error)?;
    let claimed = sqlx::query_as::<_, (i64, i32, String)>(
        "UPDATE console_commands c
         SET status = 'running', attempt = attempt + 1,
             claimed_at = now(), updated_at = now()
         FROM servers s, placements p
         WHERE c.id = $1 AND c.server_id = $2 AND c.status = 'queued'
           AND s.id = c.server_id AND s.generation = c.generation
           AND s.desired_state = 'running' AND s.observed_state = 'online'
           AND p.server_id = s.id AND p.generation = s.generation
           AND p.worker_id = c.worker_id AND p.active
         RETURNING c.generation, c.attempt, c.command",
    )
    .bind(request.command_id)
    .bind(request.server_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(fixture_error)?;
    let Some((generation, attempt, command_text)) = claimed else {
        return Err((
            StatusCode::CONFLICT,
            "no eligible queued console command".into(),
        ));
    };

    let error = if request.succeeded {
        None
    } else {
        request.error.as_deref()
    };
    let completion = sqlx::query(
        "UPDATE console_commands
         SET status = $4::console_command_status, output = $5, error = $6,
             completed_at = now(), updated_at = now()
         WHERE id = $1 AND server_id = $2 AND generation = $3 AND status = 'running'",
    )
    .bind(request.command_id)
    .bind(request.server_id)
    .bind(generation)
    .bind(if request.succeeded {
        "succeeded"
    } else {
        "failed"
    })
    .bind(&request.output)
    .bind(error)
    .execute(&mut *tx)
    .await
    .map_err(fixture_error)?;
    if completion.rows_affected() != 1 {
        return Err((
            StatusCode::CONFLICT,
            "console command completion was fenced".into(),
        ));
    }

    let log_lines = if request.succeeded {
        request.output
    } else {
        vec![format!(
            "! /{command_text} failed: {}",
            error.unwrap_or("runtime failure")
        )]
    };
    for line in &log_lines {
        sqlx::query("INSERT INTO server_logs(server_id, line) VALUES ($1, $2)")
            .bind(request.server_id)
            .bind(line)
            .execute(&mut *tx)
            .await
            .map_err(fixture_error)?;
    }
    tx.commit().await.map_err(fixture_error)?;
    Ok(Json(json!({
        "id": request.command_id,
        "status": if request.succeeded { "succeeded" } else { "failed" },
        "generation": generation,
        "attempt": attempt,
        "output": log_lines,
    })))
}

fn fixture_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

async fn tick(State(f): State<Fixture>) -> Result<Json<Value>, String> {
    let Some(op) = claim_operation(&f.pool, f.worker).await.map_err(err)? else {
        return Ok(Json(json!({"operation":null})));
    };
    let root = f.root.join(op.server_id.to_string());
    match op.kind {
        OperationKind::Start => {
            let world = root.join("world");
            tokio::fs::create_dir_all(&world).await.map_err(err)?;
            if !world.join("level.dat").exists() {
                tokio::fs::write(world.join("level.dat"), b"browser-fixture-world")
                    .await
                    .map_err(err)?;
            }
            complete_start(&f.pool, &op, 21).await.map_err(err)?;
            sqlx::query(
                "INSERT INTO server_logs(server_id, line) VALUES($1, 'Fixture runtime ready')",
            )
            .bind(op.server_id)
            .execute(&f.pool)
            .await
            .map_err(err)?;
        }
        OperationKind::Stop | OperationKind::Restart => {
            deactivate_placement_after_stop(&f.pool, f.worker, &op)
                .await
                .map_err(err)?;
            let snapshot = f
                .snapshots
                .create(op.server_id, &root, SnapshotReason::Shutdown)
                .await
                .map_err(err)?;
            publish_fixture_snapshot(
                &f.pool,
                op.server_id,
                snapshot.generation,
                snapshot.size_bytes,
            )
            .await?;
            complete_stop(&f.pool, &op, snapshot.generation, snapshot.size_bytes)
                .await
                .map_err(err)?;
        }
        OperationKind::Delete => {
            validate_delete(&f.pool, &op).await.map_err(err)?;
            f.snapshots
                .delete_server_manifests(op.server_id)
                .await
                .map_err(err)?;
            complete_delete(&f.pool, &op).await.map_err(err)?;
        }
        OperationKind::Snapshot => {
            let snapshot = f
                .snapshots
                .create(op.server_id, &root, SnapshotReason::Manual)
                .await
                .map_err(err)?;
            publish_fixture_snapshot(
                &f.pool,
                op.server_id,
                snapshot.generation,
                snapshot.size_bytes,
            )
            .await?;
            complete_snapshot(&f.pool, &op, snapshot.generation)
                .await
                .map_err(err)?;
        }
        OperationKind::Restore => {
            let target = op
                .payload
                .get("target_generation")
                .and_then(Value::as_i64)
                .ok_or_else(|| "restore operation omitted target_generation".to_owned())?;
            match tokio::fs::remove_dir_all(&root).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(err(error)),
            }
            tokio::fs::create_dir_all(&root).await.map_err(err)?;
            f.snapshots
                .restore_generation(op.server_id, target, &root)
                .await
                .map_err(err)?;
            let snapshot = f
                .snapshots
                .create(op.server_id, &root, SnapshotReason::Restore)
                .await
                .map_err(err)?;
            publish_fixture_snapshot(
                &f.pool,
                op.server_id,
                snapshot.generation,
                snapshot.size_bytes,
            )
            .await?;
            complete_restore(
                &f.pool,
                &op,
                target,
                snapshot.generation,
                snapshot.size_bytes,
            )
            .await
            .map_err(err)?;
        }
        OperationKind::Reset => {
            let was_running = op
                .payload
                .get("was_running")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mode = op
                .payload
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("world");
            tokio::fs::create_dir_all(&root).await.map_err(err)?;
            f.snapshots
                .create(op.server_id, &root, SnapshotReason::Reset)
                .await
                .map_err(err)?;
            if mode == "full" {
                match tokio::fs::remove_dir_all(&root).await {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(err(error)),
                }
            } else {
                match tokio::fs::remove_dir_all(root.join("world")).await {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(err(error)),
                }
            }
            tokio::fs::create_dir_all(&root).await.map_err(err)?;
            let snapshot = f
                .snapshots
                .create(op.server_id, &root, SnapshotReason::Reset)
                .await
                .map_err(err)?;
            publish_fixture_snapshot(
                &f.pool,
                op.server_id,
                snapshot.generation,
                snapshot.size_bytes,
            )
            .await?;
            complete_reset(
                &f.pool,
                &op,
                snapshot.generation,
                snapshot.size_bytes,
                was_running,
            )
            .await
            .map_err(err)?;
        }
    }
    f.lifecycle.drain_queue_once().await.map_err(err)?;
    Ok(Json(json!({"operation":op.kind})))
}

async fn publish_fixture_snapshot(
    pool: &PgPool,
    server_id: Uuid,
    generation: i64,
    size_bytes: i64,
) -> Result<(), String> {
    sqlx::query(
        "UPDATE servers SET current_snapshot_generation = $2, storage_bytes = $3
         WHERE id = $1",
    )
    .bind(server_id)
    .bind(generation)
    .bind(size_bytes)
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(err)
}
fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[tokio::main]
async fn main() -> Result<()> {
    let url = std::env::var("TEST_DATABASE_URL")?;
    if !reqwest::Url::parse(&url)?.path().ends_with("_e2e") {
        bail!("Browser fixture requires a disposable database ending in _e2e");
    }
    let pool = db::connect(&url).await?;
    db::migrate(&pool).await?;
    let panel_origin =
        std::env::var("PANEL_ORIGIN").unwrap_or_else(|_| "http://127.0.0.1:18082".to_owned());
    let root = std::path::PathBuf::from(std::env::var("TEST_ARTIFACT_ROOT")?);
    let object_store = ObjectStoreConfig::from_env()?.build().await?;
    let snapshots = SnapshotStore::new(pool.clone(), object_store);
    let fixture = Fixture {
        lifecycle: Lifecycle::new(pool.clone()),
        pool,
        worker: Uuid::parse_str("a89080d0-ad46-4ecd-a53f-2c09105f3200")?,
        panel_origin,
        codes: Arc::new(Mutex::new(HashMap::new())),
        tokens: Arc::new(Mutex::new(HashMap::new())),
        identity: Arc::new(Mutex::new("browser-user".into())),
        paused: Arc::new(Mutex::new(false)),
        email_verified: Arc::new(Mutex::new(true)),
        snapshots,
        root: root.join("worlds"),
    };
    let app = Router::new()
        .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
        .route("/authorize", get(authorize))
        .route("/token", post(token))
        .route("/userinfo", get(profile))
        .route("/fixture/reset", post(reset))
        .route("/fixture/capacity", post(capacity))
        .route("/fixture/identity", post(identity))
        .route("/fixture/pause", post(pause))
        .route("/fixture/email-verification", post(email_verification))
        .route("/fixture/tick", post(tick))
        .route("/fixture/console/execute", post(execute_console_command))
        .with_state(fixture);
    let listen =
        std::env::var("BROWSER_FIXTURE_LISTEN").unwrap_or_else(|_| "127.0.0.1:18081".to_owned());
    let address: std::net::SocketAddr = listen.parse()?;
    if !address.ip().is_loopback() {
        bail!("Browser fixture must listen on loopback");
    }
    axum::serve(tokio::net::TcpListener::bind(address).await?, app).await?;
    Ok(())
}

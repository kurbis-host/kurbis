use std::{net::SocketAddr, sync::Arc};

use super::events::{EventsQuery, event_cursor, next_event_row};
use super::types::{ServerSettings, ServerSettingsPatch};
use super::{AppState, router};
use crate::model::subdomain_from_hostname;
use axum::{
    body::{Body, to_bytes},
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header},
    response::IntoResponse,
};
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::Mutex;
use tower::ServiceExt;
use uuid::Uuid;

use crate::{
    config::{CommonConfig, ControlPlaneConfig, OAuthConfig, OAuthProviderConfig},
    control_plane::lifecycle::Lifecycle,
    db,
};

static DB_TEST_LOCK: Mutex<()> = Mutex::const_new(());

#[test]
fn openapi_matches_the_public_route_inventory_and_security_contracts() {
    let document = serde_json::to_value(super::openapi_document()).unwrap();
    let paths = document["paths"].as_object().unwrap();
    let schemas = document["components"]["schemas"].as_object().unwrap();
    let schemes = document["components"]["securitySchemes"]
        .as_object()
        .unwrap();
    assert!(schemes.contains_key("accountSessionCookie"));
    assert!(schemes.contains_key("serverGrantCookie"));
    assert!(schemes.contains_key("serverCapabilityBearer"));
    assert!(schemes.contains_key("workerBearer"));
    assert!(schemes.contains_key("workerLease"));
    assert!(schemes.contains_key("gatewayBearer"));

    for (path, item) in paths {
        let required: Vec<&str> = path
            .split('/')
            .filter_map(|part| part.strip_prefix('{')?.strip_suffix('}'))
            .collect();
        for (method, operation) in item.as_object().unwrap() {
            if !matches!(method.as_str(), "get" | "post" | "put" | "patch" | "delete") {
                continue;
            }
            let path_parameters = operation["parameters"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|parameter| parameter["in"] == "path")
                .map(|parameter| parameter["name"].as_str().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                path_parameters.len(),
                required.len(),
                "path parameter count for {method} {path}"
            );
            for name in &required {
                assert!(
                    path_parameters.contains(name),
                    "missing {name} on {method} {path}"
                );
            }
            for requirement in operation["security"].as_array().into_iter().flatten() {
                for scheme in requirement.as_object().unwrap().keys() {
                    assert!(
                        schemes.contains_key(scheme),
                        "undefined security scheme {scheme}"
                    );
                }
            }
            for status in [
                "400", "401", "403", "404", "409", "415", "422", "429", "500", "503",
            ] {
                if let Some(response) = operation["responses"].get(status) {
                    assert_eq!(
                        response["content"]["application/json"]["schema"]["$ref"],
                        "#/components/schemas/ApiErrorResponse",
                        "structured API error schema for {method} {path} {status}"
                    );
                }
            }
        }
    }

    assert!(paths.contains_key("/api/v1/servers/{server_id}/commands"));
    assert!(paths.contains_key("/api/v1/servers/{server_id}/console/commands"));
    assert!(paths.contains_key("/api/v1/servers/{server_id}/console/commands/{command_id}"));
    assert!(schemas.contains_key("ConsoleCommandRequest"));
    assert!(schemas.contains_key("ConsoleCommandResponse"));
    assert!(paths.contains_key("/api/v1/servers/{server_id}/operators"));
    for method in ["get", "post", "delete"] {
        let security = paths["/api/v1/servers/{server_id}/operators"][method]["security"]
            .as_array()
            .expect("browser security requirements are documented");
        for requirement in security {
            assert!(
                !requirement
                    .as_object()
                    .unwrap()
                    .contains_key("serverCapabilityBearer"),
                "operator {method} must not be available to runtime capabilities"
            );
        }
    }
    for (path, operation) in [
        (
            "/api/v1/servers/{server_id}/operators",
            &paths["/api/v1/servers/{server_id}/operators"]["post"],
        ),
        (
            "/api/v1/servers/{server_id}/whitelist",
            &paths["/api/v1/servers/{server_id}/whitelist"]["post"],
        ),
    ] {
        let schema = operation["requestBody"]["content"]["application/json"]["schema"]["$ref"]
            .as_str()
            .unwrap();
        let schema_name = schema.strip_prefix("#/components/schemas/").unwrap();
        assert_eq!(
            schemas[schema_name]["required"],
            serde_json::json!(["name"]),
            "name-only mutation schema for {path}"
        );
        assert!(
            schemas[schema_name]["properties"]
                .get("player_id")
                .is_none(),
            "player mutation must not trust caller UUIDs for {path}"
        );
    }
    for removed in [
        "/api/v1/servers/{server_id}/actions",
        "/api/v1/servers/{server_id}/reset",
        "/api/v1/servers/{server_id}/snapshots/{generation}/restore",
        "/internal/wake/{hostname}",
        "/internal/worker/control/servers/{server_id}/stop",
        "/internal/worker/control/servers/{server_id}/snapshot",
    ] {
        assert!(
            !paths.contains_key(removed),
            "legacy lifecycle route {removed} leaked into OpenAPI"
        );
    }
    assert!(schemas.contains_key("ServerCommandRequest"));
    assert!(schemas.contains_key("CommandResponse"));
    assert!(schemas.contains_key("ApiErrorResponse"));
    assert!(
        !schemas["WorldExport"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("archive_key")
    );
    assert!(
        paths["/api/v1/servers/{server_id}/events"]["get"]["responses"]["200"]["content"]
            .get("text/event-stream")
            .is_some()
    );
    assert!(paths["/api/v1/servers/{server_id}/exports/{export_id}/download"]["get"]["responses"]["200"]["content"].get("application/zip").is_some());
}

#[tokio::test]
async fn lifecycle_errors_keep_state_conflicts_distinct_from_infrastructure_failures() {
    let rejected = super::lifecycle_error(anyhow::Error::new(super::LifecycleFailure::Conflict(
        "internal placement detail".to_owned(),
    )))
    .into_response();
    assert_eq!(rejected.status(), StatusCode::CONFLICT);
    let rejected_body = to_bytes(rejected.into_body(), 4096).await.unwrap();
    let rejected_body: Value = serde_json::from_slice(&rejected_body).unwrap();
    assert_eq!(rejected_body["code"], "invalid_lifecycle_state");
    assert_eq!(
        rejected_body["message"],
        "the command conflicts with the server's current state"
    );
    assert!(
        !rejected_body
            .to_string()
            .contains("internal placement detail")
    );

    let infrastructure =
        super::lifecycle_error(anyhow::anyhow!("database disconnect detail")).into_response();
    assert_eq!(infrastructure.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let infrastructure_body = to_bytes(infrastructure.into_body(), 4096).await.unwrap();
    let infrastructure_body: Value = serde_json::from_slice(&infrastructure_body).unwrap();
    assert_eq!(infrastructure_body["code"], "lifecycle_service_error");
    assert_eq!(
        infrastructure_body["message"],
        "the lifecycle service could not complete the request"
    );
    assert!(
        !infrastructure_body
            .to_string()
            .contains("database disconnect detail")
    );

    for (failure, status, code) in [
        (
            super::LifecycleCommandError::NotFound,
            StatusCode::NOT_FOUND,
            "lifecycle_resource_not_found",
        ),
        (
            super::LifecycleCommandError::Forbidden,
            StatusCode::FORBIDDEN,
            "lifecycle_command_forbidden",
        ),
        (
            super::LifecycleCommandError::Invalid,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_lifecycle_command",
        ),
        (
            super::LifecycleCommandError::Unavailable,
            StatusCode::SERVICE_UNAVAILABLE,
            "lifecycle_capacity_unavailable",
        ),
    ] {
        let response = super::lifecycle_error(failure).into_response();
        assert_eq!(response.status(), status);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["code"], code);
    }
}

async fn test_pool() -> PgPool {
    let database_url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL must point to a disposable Postgres test database");
    let pool = db::connect(&database_url)
        .await
        .expect("connect to test Postgres");
    db::migrate(&pool)
        .await
        .expect("apply test database migrations");
    pool
}

fn test_state(pool: PgPool) -> Arc<AppState> {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is configured");
    Arc::new(AppState {
        ci_build_coordinator: None,
        ci_build_identity: None,
        lifecycle: Lifecycle::new(pool.clone()),
        pool,
        config: ControlPlaneConfig {
            common: CommonConfig { database_url },
            listen: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            public_base_url: "https://kurbis.example".to_owned(),
            root_domain: "mc.example".to_owned(),
            internal_token: "test-internal-token".to_owned(),
            ci_github_webhook_secret: Some(crate::config::SecretString::new("test-webhook-secret")),
            oauth: OAuthConfig {
                google: OAuthProviderConfig {
                    client_id: "google-client".to_owned(),
                    client_secret: "google-secret".to_owned(),
                    authorize_url: "https://id.example/authorize".to_owned(),
                    token_url: "https://id.example/token".to_owned(),
                    userinfo_url: "https://id.example/userinfo".to_owned(),
                    scopes: "openid email profile".to_owned(),
                },
                discord: OAuthProviderConfig {
                    client_id: "discord-client".to_owned(),
                    client_secret: "discord-secret".to_owned(),
                    authorize_url: "https://id.example/authorize".to_owned(),
                    token_url: "https://id.example/token".to_owned(),
                    userinfo_url: "https://id.example/userinfo".to_owned(),
                    scopes: "identify email".to_owned(),
                },
            },
        },
        http: reqwest::Client::new(),
        object_store: Arc::new(object_store::memory::InMemory::new()),
    })
}

async fn create_user_session(pool: &PgPool, status: &str) -> (Uuid, String) {
    let user_id = Uuid::new_v4();
    let token = format!("api-test-session-{user_id}");
    sqlx::query("INSERT INTO users(id, display_name, account_status) VALUES($1, 'API Test', $2)")
        .bind(user_id)
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO identities(id, user_id, provider, subject, display_name)
         VALUES($1, $2, 'google', $3, 'API Test')",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(format!("api-test-{user_id}"))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO sessions(id, token_hash, user_id) VALUES($1, $2, $3)")
        .bind(Uuid::new_v4())
        .bind(blake3::hash(token.as_bytes()).as_bytes().to_vec())
        .bind(user_id)
        .execute(pool)
        .await
        .unwrap();
    (user_id, token)
}

async fn create_server(pool: &PgPool, owner_id: Uuid) -> Uuid {
    let server_id = Uuid::new_v4();
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)")
        .bind(server_id)
        .bind(owner_id)
        .bind(format!("api-{}", &server_id.simple().to_string()[..12]))
        .execute(pool)
        .await
        .unwrap();
    server_id
}

fn authenticated_request(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(header::COOKIE, format!("kurbis_session={token}"))
        .body(Body::empty())
        .unwrap()
}

fn json_request(
    method: Method,
    uri: &str,
    origin: Option<&str>,
    session: Option<&str>,
    body: Value,
) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json");
    if let Some(origin) = origin {
        request = request.header(header::ORIGIN, origin);
    }
    if let Some(session) = session {
        request = request.header(header::COOKIE, format!("kurbis_session={session}"));
    }
    request.body(Body::from(body.to_string())).unwrap()
}

async fn mint_minecraft_link(app: &axum::Router, identity: Value) -> Value {
    let minecraft_player_id = test_profile_id(&identity);
    let response = app
        .clone()
        .oneshot(
            Request::post("/internal/minecraft-login-links")
                .header(header::AUTHORIZATION, "Bearer test-internal-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"identity":identity,"minecraft_player_id":minecraft_player_id,"display_name":"Minecraft Player"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap()
}

fn test_profile_id(identity: &Value) -> Uuid {
    if let Some(value) = identity.get("uuid").and_then(Value::as_str) {
        return value.parse().unwrap();
    }
    let numeric = identity
        .get("xuid")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<u128>().ok())
        .unwrap_or(1);
    Uuid::from_u128(numeric.max(1))
}

fn minecraft_token(link: &Value) -> String {
    let url = reqwest::Url::parse(link["url"].as_str().unwrap()).unwrap();
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.path(), "/auth/minecraft");
    url.fragment()
        .unwrap()
        .strip_prefix("token=")
        .unwrap()
        .to_owned()
}

#[test]
fn extracts_only_one_valid_label() {
    assert_eq!(
        subdomain_from_hostname("abc.play.example.com", "play.example.com"),
        Some("abc".into())
    );
    assert_eq!(
        subdomain_from_hostname("abc.def.play.example.com", "play.example.com"),
        None
    );
    assert_eq!(
        subdomain_from_hostname("play.example.com", "play.example.com"),
        None
    );
    assert_eq!(
        subdomain_from_hostname("AbC.PLAY.EXAMPLE.COM", "play.example.com"),
        Some("abc".into())
    );
}

#[test]
fn settings_patch_preserves_unspecified_values() {
    let mut settings: ServerSettings = serde_json::from_value(serde_json::json!({
        "motd": "hello",
        "difficulty": "Normal",
        "gamemode": "Survival",
        "view_distance": 10,
        "simulation_distance": 8,
        "hardcore": false,
        "whitelist": false
    }))
    .unwrap();
    let patch: ServerSettingsPatch = serde_json::from_value(serde_json::json!({
        "view_distance": 16,
        "whitelist": true
    }))
    .unwrap();
    settings.merge(patch);
    assert_eq!(
        serde_json::to_value(settings).unwrap(),
        serde_json::json!({
            "motd": "hello",
            "difficulty": "Normal",
            "gamemode": "Survival",
            "view_distance": 16,
            "simulation_distance": 8,
            "hardcore": false,
            "whitelist": true
        })
    );
}

#[test]
fn last_event_id_takes_precedence_over_query_cursor() {
    let mut headers = HeaderMap::new();
    headers.insert("last-event-id", HeaderValue::from_static("42"));
    let query = EventsQuery { after_id: Some(7) };
    assert_eq!(event_cursor(&headers, &query).unwrap(), 42);
}

#[test]
fn event_cursor_rejects_negative_values() {
    let headers = HeaderMap::new();
    let query = EventsQuery { after_id: Some(-1) };
    assert!(event_cursor(&headers, &query).is_err());
}

#[tokio::test]
async fn suspended_session_is_visible_but_control_plane_access_is_forbidden() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (user_id, token) = create_user_session(&pool, "suspended").await;
    let app = router(test_state(pool.clone()));

    let session_response = app
        .clone()
        .oneshot(authenticated_request("/api/v1/session", &token))
        .await
        .unwrap();
    assert_eq!(session_response.status(), StatusCode::OK);

    let server_response = app
        .clone()
        .oneshot(authenticated_request("/api/v1/servers", &token))
        .await
        .unwrap();
    assert_eq!(server_response.status(), StatusCode::FORBIDDEN);

    let legacy_response = app
        .oneshot(authenticated_request("/api/me", &token))
        .await
        .unwrap();
    assert_eq!(legacy_response.status(), StatusCode::NOT_FOUND);

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn server_read_is_scoped_to_the_authenticated_owner() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_a, token_a) = create_user_session(&pool, "active").await;
    let (owner_b, _) = create_user_session(&pool, "active").await;
    let server_id = create_server(&pool, owner_b).await;
    let app = router(test_state(pool.clone()));

    let response = app
        .oneshot(authenticated_request(
            &format!("/api/v1/servers/{server_id}"),
            &token_a,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(vec![owner_a, owner_b])
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn durable_event_catch_up_preserves_cursor_and_server_scope() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner, _) = create_user_session(&pool, "active").await;
    let (other_owner, _) = create_user_session(&pool, "active").await;
    let server_id = create_server(&pool, owner).await;
    let baseline: i64 =
        sqlx::query_scalar("SELECT max(id) FROM server_events WHERE server_id = $1")
            .bind(server_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let first: i64 = sqlx::query_scalar(
        "INSERT INTO server_events(server_id, event_type)
             VALUES($1, 'log') RETURNING id",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let second: i64 = sqlx::query_scalar(
        "INSERT INTO server_events(server_id, event_type)
             VALUES($1, 'players') RETURNING id",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    let row = next_event_row(&pool, server_id, baseline)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.id, first);
    let row = next_event_row(&pool, server_id, first)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.id, second);
    assert!(
        next_event_row(&pool, Uuid::new_v4(), baseline)
            .await
            .unwrap()
            .is_none()
    );

    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(vec![owner, other_owner])
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn minecraft_login_tokens_are_private_one_use_origin_bound_and_expiring() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let state = test_state(pool.clone());
    let app = router(state);
    let java_uuid = Uuid::new_v4();
    let link = mint_minecraft_link(&app, serde_json::json!({"kind":"java","uuid":java_uuid})).await;
    let token = minecraft_token(&link);
    let token_hash = blake3::hash(token.as_bytes()).as_bytes().to_vec();
    let forged_uuid = Uuid::new_v4();

    let forged = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/internal/minecraft-login-links")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "identity":{"kind":"java","uuid":forged_uuid},
                        "minecraft_player_id":forged_uuid,
                        "display_name":"Forged Player"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forged.status(), StatusCode::UNAUTHORIZED);
    for invalid_xuid in ["0", "abc", "000", "123456789012345678901"] {
        let invalid = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/internal/minecraft-login-links")
                    .header(header::AUTHORIZATION, "Bearer test-internal-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "identity":{"kind":"bedrock","xuid":invalid_xuid},
                            "display_name":"Bedrock Player"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    let get = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/auth/minecraft/redeem")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::METHOD_NOT_ALLOWED);
    let cross_origin = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/auth/minecraft/redeem",
            Some("https://evil.example"),
            None,
            serde_json::json!({"token":token}),
        ))
        .await
        .unwrap();
    assert_eq!(cross_origin.status(), StatusCode::FORBIDDEN);
    let token_remains: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM minecraft_login_tokens WHERE token_hash=$1)",
    )
    .bind(&token_hash)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        token_remains,
        "an origin rejection must not consume the token"
    );

    let redeemed = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/auth/minecraft/redeem",
            Some("https://kurbis.example"),
            None,
            serde_json::json!({"token":token}),
        ))
        .await
        .unwrap();
    assert_eq!(redeemed.status(), StatusCode::NO_CONTENT);
    let cookie = redeemed
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with("kurbis_session="))
        .unwrap();
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("Secure"));
    assert!(cookie.contains("SameSite=Lax"));
    assert!(cookie.contains("Max-Age=2592000"));

    let user_id: Uuid = sqlx::query_scalar(
        "SELECT user_id FROM identities WHERE provider='minecraft_java' AND subject=$1",
    )
    .bind(java_uuid.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    let account_sessions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id=$1")
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(account_sessions, 1);
    let replay = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/auth/minecraft/redeem",
            Some("https://kurbis.example"),
            None,
            serde_json::json!({"token":minecraft_token(&link)}),
        ))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);

    let expired_link = mint_minecraft_link(
        &app,
        serde_json::json!({"kind":"bedrock","xuid":"2533274790395904"}),
    )
    .await;
    let expired_token = minecraft_token(&expired_link);
    sqlx::query(
        "UPDATE minecraft_login_tokens SET expires_at=now()-interval '1 second'
         WHERE token_hash=$1",
    )
    .bind(blake3::hash(expired_token.as_bytes()).as_bytes().to_vec())
    .execute(&pool)
    .await
    .unwrap();
    let expired = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/auth/minecraft/redeem",
            Some("https://kurbis.example"),
            None,
            serde_json::json!({"token":expired_token}),
        ))
        .await
        .unwrap();
    assert_eq!(expired.status(), StatusCode::UNAUTHORIZED);
    let expired_identity_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM identities
         WHERE provider = 'minecraft_bedrock' AND subject = '2533274790395904'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(expired_identity_count, 0);

    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM minecraft_login_tokens")
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn signed_in_minecraft_redemption_merges_the_browser_account_and_all_worlds() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (browser_user, browser_token) = create_user_session(&pool, "active").await;
    let bedrock_xuid = "2533274790395904";
    let minecraft_user = crate::control_plane::identity::attach_minecraft_identity(
        &pool,
        "minecraft_bedrock",
        bedrock_xuid,
        test_profile_id(&serde_json::json!({"xuid":bedrock_xuid})),
        "Bedrock Player",
        None,
    )
    .await
    .unwrap();
    let browser_server = create_server(&pool, browser_user).await;
    let minecraft_server = create_server(&pool, minecraft_user).await;
    let old_session = format!("minecraft-source-{minecraft_user}");
    sqlx::query("INSERT INTO sessions(id,token_hash,user_id) VALUES($1,$2,$3)")
        .bind(Uuid::new_v4())
        .bind(blake3::hash(old_session.as_bytes()).as_bytes().to_vec())
        .bind(minecraft_user)
        .execute(&pool)
        .await
        .unwrap();
    let app = router(test_state(pool.clone()));
    let link = mint_minecraft_link(
        &app,
        serde_json::json!({"kind":"bedrock","xuid":bedrock_xuid}),
    )
    .await;
    let redeemed = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/auth/minecraft/redeem",
            Some("https://kurbis.example"),
            Some(&browser_token),
            serde_json::json!({"token":minecraft_token(&link)}),
        ))
        .await
        .unwrap();
    assert_eq!(redeemed.status(), StatusCode::NO_CONTENT);
    let owners: Vec<(Uuid, bool)> =
        sqlx::query_as("SELECT id, is_default FROM servers WHERE owner_id=$1 ORDER BY id")
            .bind(browser_user)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(owners.len(), 2);
    assert!(
        owners
            .iter()
            .any(|(id, default)| *id == browser_server && *default)
    );
    assert!(
        owners
            .iter()
            .any(|(id, default)| *id == minecraft_server && !*default)
    );
    let moved_sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id=$1")
        .bind(browser_user)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(moved_sessions, 2);
    let source_exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id=$1)")
        .bind(minecraft_user)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!source_exists);

    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(browser_user)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn account_sessions_are_persistent_revocable_and_renewed_on_visits() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (user_id, current_token) = create_user_session(&pool, "active").await;
    let other_token = format!("other-session-{user_id}");
    let other_session_id = Uuid::new_v4();
    sqlx::query("INSERT INTO sessions(id,token_hash,user_id) VALUES($1,$2,$3)")
        .bind(other_session_id)
        .bind(blake3::hash(other_token.as_bytes()).as_bytes().to_vec())
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE sessions SET created_at=now()-interval '500 days' WHERE user_id=$1")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    let app = router(test_state(pool.clone()));

    let current_session = app
        .clone()
        .oneshot(authenticated_request("/api/v1/session", &current_token))
        .await
        .unwrap();
    assert_eq!(current_session.status(), StatusCode::OK);
    let renewed_cookie = current_session
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with("kurbis_session="))
        .unwrap();
    assert!(renewed_cookie.contains("Max-Age=2592000"));
    let last_seen_recent: bool = sqlx::query_scalar(
        "SELECT last_seen_at > now()-interval '10 seconds' FROM sessions WHERE token_hash=$1",
    )
    .bind(blake3::hash(current_token.as_bytes()).as_bytes().to_vec())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(last_seen_recent);

    let account = app
        .clone()
        .oneshot(authenticated_request("/api/v1/account", &current_token))
        .await
        .unwrap();
    assert_eq!(account.status(), StatusCode::OK);
    let account: Value =
        serde_json::from_slice(&to_bytes(account.into_body(), 8192).await.unwrap()).unwrap();
    let current = account["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|session| session["current"] == true)
        .unwrap();
    let current_id = current["id"].as_str().unwrap().to_owned();
    assert_eq!(account["sessions"].as_array().unwrap().len(), 2);

    let revoked_other = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/api/v1/account/sessions/{other_session_id}"))
                .header(header::COOKIE, format!("kurbis_session={current_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoked_other.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        app.clone()
            .oneshot(authenticated_request("/api/v1/session", &other_token))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    let revoked_current = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/api/v1/account/sessions/{current_id}"))
                .header(header::COOKIE, format!("kurbis_session={current_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoked_current.status(), StatusCode::NO_CONTENT);
    let expired_cookie = revoked_current
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with("kurbis_session="))
        .unwrap();
    assert!(expired_cookie.contains("Max-Age=0"));
    assert_eq!(
        app.oneshot(authenticated_request("/api/v1/session", &current_token))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
}

mod common;

use std::{str::FromStr, sync::Arc};

use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use common::{api_state, create_user, test_pool};
use kurbis_hoster::control_plane::api::router;
use object_store::memory::InMemory;
use serde_json::{Value, json};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tokio::sync::Mutex;
use tower::ServiceExt;
use uuid::Uuid;

static DB_TEST_LOCK: Mutex<()> = Mutex::const_new(());

struct TestWorker {
    id: Uuid,
    role: String,
    api_token: String,
    pool: PgPool,
}

struct Fixture {
    admin: PgPool,
    worker_a: TestWorker,
    worker_b: TestWorker,
    owner_a: Uuid,
    owner_b: Uuid,
    server_a: Uuid,
    server_a_second_world: Uuid,
    server_b: Uuid,
    app: axum::Router,
}

#[tokio::test]
async fn authenticated_worker_roles_are_row_scoped_and_fenced_by_assignment_generation() {
    let _serial = DB_TEST_LOCK.lock().await;
    let fixture = Fixture::create().await;

    assert_worker_rows(&fixture).await;
    assert_database_permissions(&fixture).await;
    assert_capability_identity(&fixture).await;
    assert_generation_fence(&fixture).await;

    fixture.cleanup().await;
}

impl Fixture {
    async fn create() -> Self {
        let admin = test_pool().await;
        let worker_a_id = Uuid::new_v4();
        let worker_b_id = Uuid::new_v4();
        let worker_a = register_worker(&admin, worker_a_id, "10.20.0.11").await;
        let worker_b = register_worker(&admin, worker_b_id, "10.20.0.12").await;

        let owner_a = create_user(
            &admin,
            "google",
            &format!("worker-isolation-google-{worker_a_id}"),
            "Owner A",
            "active",
            None,
        )
        .await;
        let owner_b = create_user(
            &admin,
            "discord",
            &format!("worker-isolation-discord-{worker_b_id}"),
            "Owner B",
            "active",
            None,
        )
        .await;
        let player_a = Uuid::new_v4();
        let player_b = Uuid::new_v4();
        add_minecraft_identity(&admin, owner_a, player_a, "Minecraft Owner A").await;
        add_minecraft_identity(&admin, owner_b, player_b, "Minecraft Owner B").await;

        let server_a = insert_server(&admin, owner_a, "worker-a").await;
        let server_a_second_world = insert_server(&admin, owner_a, "worker-a-second").await;
        let server_b = insert_server(&admin, owner_b, "worker-b").await;
        for (server_id, worker_id, java_port) in [
            (server_a, worker_a.id, 25000),
            (server_a_second_world, worker_b.id, 25010),
            (server_b, worker_b.id, 25020),
        ] {
            seed_assigned_server(&admin, server_id, worker_id, java_port).await;
        }

        let app = router(api_state(
            admin.clone(),
            "worker-isolation-gateway-token",
            Arc::new(InMemory::new()),
        ));
        Self {
            admin,
            worker_a,
            worker_b,
            owner_a,
            owner_b,
            server_a,
            server_a_second_world,
            server_b,
            app,
        }
    }

    async fn cleanup(self) {
        self.worker_a.pool.close().await;
        self.worker_b.pool.close().await;
        for role in [&self.worker_a.role, &self.worker_b.role] {
            let terminate = sqlx::query_scalar::<_, bool>(
                "SELECT pg_terminate_backend(pid)
                 FROM pg_stat_activity WHERE usename = $1 AND pid <> pg_backend_pid()",
            )
            .bind(role)
            .fetch_all(&self.admin)
            .await
            .unwrap();
            let _ = terminate;
            sqlx::query("DELETE FROM worker_auth.credentials WHERE database_role = $1::name")
                .bind(role)
                .execute(&self.admin)
                .await
                .unwrap();
            let revoke = sqlx::query_scalar::<_, String>(
                "SELECT format('REVOKE worker_runtime FROM %I', $1::text)",
            )
            .bind(role)
            .fetch_one(&self.admin)
            .await
            .unwrap();
            sqlx::raw_sql(&revoke).execute(&self.admin).await.unwrap();
            let drop_role =
                sqlx::query_scalar::<_, String>("SELECT format('DROP ROLE %I', $1::text)")
                    .bind(role)
                    .fetch_one(&self.admin)
                    .await
                    .unwrap();
            sqlx::raw_sql(&drop_role)
                .execute(&self.admin)
                .await
                .unwrap();
        }
        sqlx::query("DELETE FROM users WHERE id IN ($1, $2)")
            .bind(self.owner_a)
            .bind(self.owner_b)
            .execute(&self.admin)
            .await
            .unwrap();
        sqlx::query("DELETE FROM workers WHERE id IN ($1, $2)")
            .bind(self.worker_a.id)
            .bind(self.worker_b.id)
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

async fn register_worker(pool: &PgPool, worker_id: Uuid, signal_address: &str) -> TestWorker {
    let role = format!("worker_test_{}", worker_id.simple());
    let password = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let api_token = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let create_role = sqlx::query_scalar::<_, String>(
        "SELECT format(
             'CREATE ROLE %I LOGIN INHERIT PASSWORD %L NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS',
             $1::text, $2::text
         )",
    )
    .bind(&role)
    .bind(&password)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::raw_sql(&create_role).execute(pool).await.unwrap();
    let grant =
        sqlx::query_scalar::<_, String>("SELECT format('GRANT worker_runtime TO %I', $1::text)")
            .bind(&role)
            .fetch_one(pool)
            .await
            .unwrap();
    sqlx::raw_sql(&grant).execute(pool).await.unwrap();

    sqlx::query(
        "INSERT INTO workers(id, name, address, signal_address, total_memory_bytes,
                            total_disk_bytes, total_cpu_millis, port_min, port_max)
         VALUES($1, $2, '203.0.113.9', $3, 8589934592, 42949672960, 4000, 25000, 25100)",
    )
    .bind(worker_id)
    .bind(format!("worker-isolation-{worker_id}"))
    .bind(signal_address)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO worker_auth.credentials(worker_id, api_token_hash, database_role)
         VALUES($1, $2, $3::name)",
    )
    .bind(worker_id)
    .bind(blake3::hash(api_token.as_bytes()).as_bytes().to_vec())
    .bind(&role)
    .execute(pool)
    .await
    .unwrap();

    let admin_url = std::env::var("TEST_DATABASE_URL").unwrap();
    let options = PgConnectOptions::from_str(&admin_url)
        .unwrap()
        .username(&role)
        .password(&password);
    let worker_pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options)
        .await
        .unwrap();
    TestWorker {
        id: worker_id,
        role,
        api_token,
        pool: worker_pool,
    }
}

async fn add_minecraft_identity(pool: &PgPool, owner_id: Uuid, player_id: Uuid, name: &str) {
    sqlx::query(
        "INSERT INTO identities(id, user_id, provider, subject, display_name, minecraft_player_id)
         VALUES($1, $2, 'minecraft_java', $3, $4, $5)",
    )
    .bind(Uuid::new_v4())
    .bind(owner_id)
    .bind(player_id.to_string())
    .bind(name)
    .bind(player_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_server(pool: &PgPool, owner_id: Uuid, label: &str) -> Uuid {
    let server_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO servers(id, owner_id, subdomain, generation, current_snapshot_generation)
         VALUES($1, $2, $3, 1, 1)",
    )
    .bind(server_id)
    .bind(owner_id)
    .bind(format!("{label}-{}", &server_id.simple().to_string()[..12]))
    .execute(pool)
    .await
    .unwrap();
    server_id
}

async fn seed_assigned_server(pool: &PgPool, server_id: Uuid, worker_id: Uuid, java_port: i32) {
    sqlx::query(
        "INSERT INTO placements(server_id, worker_id, generation, lease_token,
                                java_port, bedrock_port, signal_port, active)
         VALUES($1, $2, 1, $3, $4, $5, $6, true)",
    )
    .bind(server_id)
    .bind(worker_id)
    .bind(Uuid::new_v4())
    .bind(java_port)
    .bind(java_port + 1)
    .bind(java_port + 2)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes,
                               checksum, reason, complete)
         VALUES($1,$2,1,$3,100,'test-checksum','manual',true)",
    )
    .bind(Uuid::new_v4())
    .bind(server_id)
    .bind(format!(
        "servers/{server_id}/snapshots/1-test-checksum.json"
    ))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO server_logs(server_id, line) VALUES($1, 'seeded log')")
        .bind(server_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO server_players(server_id, player_id, name, edition)
         VALUES($1,$2,'Player','java')",
    )
    .bind(server_id)
    .bind(Uuid::new_v4())
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO operations(id, server_id, worker_id, kind, status, generation)
         VALUES($1,$2,$3,'snapshot','pending',1)",
    )
    .bind(Uuid::new_v4())
    .bind(server_id)
    .bind(worker_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO world_exports(id, server_id, requested_by, status, stage,
                                   source_generation, worker_id)
         SELECT $1, $2, s.owner_id, 'pending', 'ready', 1, $3
         FROM servers s WHERE s.id = $2",
    )
    .bind(Uuid::new_v4())
    .bind(server_id)
    .bind(worker_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO whitelist_entries(server_id, player_id, name)
         VALUES($1,$2,'Whitelisted')",
    )
    .bind(server_id)
    .bind(Uuid::new_v4())
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO worker_snapshot_cache(worker_id, server_id, generation)
         VALUES($1,$2,1)",
    )
    .bind(worker_id)
    .bind(server_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn assert_worker_rows(fixture: &Fixture) {
    let identity_a: Option<Uuid> = sqlx::query_scalar("SELECT worker_auth.current_worker_id()")
        .fetch_one(&fixture.worker_a.pool)
        .await
        .unwrap();
    assert_eq!(identity_a, Some(fixture.worker_a.id));
    let visible_a: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM servers ORDER BY id")
        .fetch_all(&fixture.worker_a.pool)
        .await
        .unwrap();
    assert_eq!(visible_a, vec![fixture.server_a]);

    let visible_b: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM servers ORDER BY id")
        .fetch_all(&fixture.worker_b.pool)
        .await
        .unwrap();
    let mut expected_b = vec![fixture.server_a_second_world, fixture.server_b];
    expected_b.sort();
    assert_eq!(visible_b, expected_b);

    let identities_a: Vec<(String, String)> =
        sqlx::query_as("SELECT provider, display_name FROM identities ORDER BY display_name")
            .fetch_all(&fixture.worker_a.pool)
            .await
            .unwrap();
    assert_eq!(
        identities_a,
        vec![("minecraft_java".to_owned(), "Minecraft Owner A".to_owned())]
    );
    let identities_b: Vec<(String, String)> =
        sqlx::query_as("SELECT provider, display_name FROM identities ORDER BY display_name")
            .fetch_all(&fixture.worker_b.pool)
            .await
            .unwrap();
    assert_eq!(
        identities_b,
        vec![
            ("minecraft_java".to_owned(), "Minecraft Owner A".to_owned()),
            ("minecraft_java".to_owned(), "Minecraft Owner B".to_owned()),
        ]
    );

    for (table, server_id) in [
        ("placements", fixture.server_a_second_world),
        ("snapshots", fixture.server_a_second_world),
        ("server_logs", fixture.server_b),
        ("server_players", fixture.server_b),
        ("whitelist_entries", fixture.server_b),
    ] {
        let query = format!("SELECT count(*) FROM {table} WHERE server_id = $1");
        let visible: i64 = sqlx::query_scalar(&query)
            .bind(server_id)
            .fetch_one(&fixture.worker_a.pool)
            .await
            .unwrap();
        assert_eq!(visible, 0, "worker A saw {table} for unassigned server");
    }

    let worker_row: Option<Uuid> = sqlx::query_scalar("SELECT id FROM workers")
        .fetch_optional(&fixture.worker_a.pool)
        .await
        .unwrap();
    assert_eq!(worker_row, Some(fixture.worker_a.id));
    let assigned_operations: Vec<Uuid> =
        sqlx::query_scalar("SELECT server_id FROM operations ORDER BY server_id")
            .fetch_all(&fixture.worker_a.pool)
            .await
            .unwrap();
    assert_eq!(assigned_operations, vec![fixture.server_a]);
    let assigned_exports: Vec<Uuid> =
        sqlx::query_scalar("SELECT server_id FROM world_exports ORDER BY server_id")
            .fetch_all(&fixture.worker_a.pool)
            .await
            .unwrap();
    assert_eq!(assigned_exports, vec![fixture.server_a]);
    let spoofed_identity: Option<Uuid> = sqlx::query_scalar(
        "SELECT worker_auth.current_worker_id()
         FROM (SELECT set_config('app.worker_id', $1, false)) configured",
    )
    .bind(fixture.worker_b.id.to_string())
    .fetch_one(&fixture.worker_a.pool)
    .await
    .unwrap();
    assert_eq!(spoofed_identity, Some(fixture.worker_a.id));
}

async fn assert_database_permissions(fixture: &Fixture) {
    assert!(
        sqlx::query("SELECT id FROM users")
            .fetch_all(&fixture.worker_a.pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("SELECT worker_id FROM worker_auth.credentials")
            .fetch_all(&fixture.worker_a.pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE placements SET worker_id = $2 WHERE server_id = $1")
            .bind(fixture.server_a)
            .bind(fixture.worker_b.id)
            .execute(&fixture.worker_a.pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE servers SET observed_state = 'failed' WHERE id = $1")
            .bind(fixture.server_a)
            .execute(&fixture.worker_a.pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE workers SET address = '198.51.100.99' WHERE id = $1")
            .bind(fixture.worker_a.id)
            .execute(&fixture.worker_a.pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("INSERT INTO server_logs(server_id, line) VALUES($1, 'cross-server')")
            .bind(fixture.server_b)
            .execute(&fixture.worker_a.pool)
            .await
            .is_err()
    );
    let changed =
        sqlx::query("UPDATE server_operator_state SET applied_revision = 99 WHERE server_id = $1")
            .bind(fixture.server_a_second_world)
            .execute(&fixture.worker_a.pool)
            .await
            .unwrap();
    assert_eq!(changed.rows_affected(), 0);
    assert!(
        sqlx::query("CREATE TABLE public.worker_must_not_create_objects(id integer)")
            .execute(&fixture.worker_a.pool)
            .await
            .is_err()
    );

    let mut connection = fixture.worker_a.pool.acquire().await.unwrap();
    sqlx::query("SET ROLE worker_runtime")
        .execute(&mut *connection)
        .await
        .unwrap();
    let role_identity: Option<Uuid> = sqlx::query_scalar("SELECT worker_auth.current_worker_id()")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(role_identity, Some(fixture.worker_a.id));
    sqlx::query("RESET ROLE")
        .execute(&mut *connection)
        .await
        .unwrap();
    assert!(
        sqlx::query(&format!("SET ROLE {}", fixture.worker_b.role))
            .execute(&mut *connection)
            .await
            .is_err()
    );
    assert!(
        sqlx::query(&format!(
            "SET SESSION AUTHORIZATION {}",
            fixture.worker_b.role
        ))
        .execute(&mut *connection)
        .await
        .is_err()
    );
}

async fn assert_capability_identity(fixture: &Fixture) {
    let response = fixture
        .app
        .clone()
        .oneshot(worker_request(
            &fixture.worker_a.api_token,
            json!({"server_id":fixture.server_a,"generation":1}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let capability = json_body(response).await["capability"]
        .as_str()
        .unwrap()
        .to_owned();

    let spoofed_worker = fixture
        .app
        .clone()
        .oneshot(worker_request(
            &fixture.worker_a.api_token,
            json!({
                "server_id":fixture.server_a,
                "worker_id":fixture.worker_b.id,
                "generation":1
            }),
        ))
        .await
        .unwrap();
    assert!(spoofed_worker.status().is_client_error());

    let cross_server = fixture
        .app
        .clone()
        .oneshot(worker_request(
            &fixture.worker_a.api_token,
            json!({"server_id":fixture.server_b,"generation":1}),
        ))
        .await
        .unwrap();
    assert_eq!(cross_server.status(), StatusCode::CONFLICT);

    let shared_secret = fixture
        .app
        .clone()
        .oneshot(worker_request(
            "worker-isolation-gateway-token",
            json!({
                "server_id":fixture.server_a,
                "generation":1
            }),
        ))
        .await
        .unwrap();
    assert_eq!(shared_secret.status(), StatusCode::UNAUTHORIZED);

    let moved_server = fixture.server_a;
    sqlx::query("UPDATE servers SET generation = 2 WHERE id = $1")
        .bind(moved_server)
        .execute(&fixture.admin)
        .await
        .unwrap();
    sqlx::query("UPDATE placements SET generation = 2, worker_id = $2 WHERE server_id = $1")
        .bind(moved_server)
        .bind(fixture.worker_b.id)
        .execute(&fixture.admin)
        .await
        .unwrap();
    let old_worker = fixture
        .app
        .clone()
        .oneshot(worker_request(
            &fixture.worker_a.api_token,
            json!({"server_id":moved_server,"generation":1,"current_capability":capability}),
        ))
        .await
        .unwrap();
    assert_eq!(old_worker.status(), StatusCode::CONFLICT);
    let new_worker = fixture
        .app
        .clone()
        .oneshot(worker_request(
            &fixture.worker_b.api_token,
            json!({"server_id":moved_server,"generation":2,"current_capability":capability}),
        ))
        .await
        .unwrap();
    assert_eq!(new_worker.status(), StatusCode::OK);
    let rotated = json_body(new_worker).await["capability"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(rotated, capability);

    sqlx::query("UPDATE worker_auth.credentials SET revoked_at = now() WHERE worker_id = $1")
        .bind(fixture.worker_a.id)
        .execute(&fixture.admin)
        .await
        .unwrap();
    let revoked_identity: Option<Uuid> =
        sqlx::query_scalar("SELECT worker_auth.current_worker_id()")
            .fetch_one(&fixture.worker_a.pool)
            .await
            .unwrap();
    assert_eq!(revoked_identity, None);
    let revoked_capability_request = fixture
        .app
        .clone()
        .oneshot(worker_request(
            &fixture.worker_a.api_token,
            json!({"server_id":fixture.server_a,"generation":1}),
        ))
        .await
        .unwrap();
    assert_eq!(
        revoked_capability_request.status(),
        StatusCode::UNAUTHORIZED
    );
}

async fn assert_generation_fence(fixture: &Fixture) {
    let visible_after_move: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM servers ORDER BY id")
        .fetch_all(&fixture.worker_a.pool)
        .await
        .unwrap();
    assert!(visible_after_move.is_empty());
    let visible_for_new_worker: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM servers ORDER BY id")
            .fetch_all(&fixture.worker_b.pool)
            .await
            .unwrap();
    let mut expected = vec![
        fixture.server_a,
        fixture.server_a_second_world,
        fixture.server_b,
    ];
    expected.sort();
    assert_eq!(visible_for_new_worker, expected);
}

fn worker_request(token: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/internal/worker/server-capabilities")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

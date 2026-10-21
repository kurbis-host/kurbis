use super::support::*;
use kurbis_hoster::config::{AdminIdentity, OAuthProvider};
use kurbis_hoster::control_plane::maintenance::WorkerMaintenance;
use kurbis_hoster::fleet_stats;

/// An account signed in with a Google identity, and its session token.
async fn google_session(pool: &PgPool, subject: &str, status: &str) -> (Uuid, String) {
    let user_id = create_user(
        pool,
        "google",
        subject,
        "Admin Test",
        status,
        Some(&format!("{subject}@example.test")),
    )
    .await;
    let token = format!("admin-test-session-{user_id}");
    sqlx::query("INSERT INTO sessions(id, token_hash, user_id) VALUES($1, $2, $3)")
        .bind(Uuid::new_v4())
        .bind(blake3::hash(token.as_bytes()).as_bytes().to_vec())
        .bind(user_id)
        .execute(pool)
        .await
        .unwrap();
    (user_id, token)
}

struct Fixture {
    app: axum::Router,
    pool: PgPool,
    admin_id: Uuid,
    admin_token: String,
    owner_id: Uuid,
    owner_token: String,
    server_id: Uuid,
}

async fn fixture() -> Fixture {
    let pool = test_pool().await;
    let subject = format!("admin-{}", Uuid::new_v4());
    let (admin_id, admin_token) = google_session(&pool, &subject, "active").await;
    let (owner_id, owner_token) = user_session(&pool, "active").await;
    let server_id = create_test_server(&pool, owner_id, "admin").await;
    let app = router(api_state_with_admins(
        pool.clone(),
        "api-v1-test-token",
        Arc::new(object_store::memory::InMemory::new()),
        vec![AdminIdentity {
            provider: OAuthProvider::Google,
            subject,
        }],
    ));
    Fixture {
        app,
        pool,
        admin_id,
        admin_token,
        owner_id,
        owner_token,
        server_id,
    }
}

async fn get(app: &axum::Router, uri: &str, token: Option<&str>) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(request(Method::GET, uri, token, None))
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

const ADMIN_ROUTES: &[&str] = &[
    "/api/v1/admin/overview",
    "/api/v1/admin/fleet?range=week",
    "/api/v1/admin/servers",
    "/api/v1/admin/accounts",
    "/api/v1/admin/workers",
    "/api/v1/admin/failures?range=month",
    "/api/v1/admin/ci",
    "/api/v1/admin/ai",
    "/api/v1/admin/growth",
];

#[tokio::test]
async fn the_dashboard_exists_only_for_active_accounts_with_an_admin_identity() {
    let _serial = DB_TEST_LOCK.lock().await;
    let fixture = fixture().await;
    let app = &fixture.app;

    for route in ADMIN_ROUTES {
        assert_eq!(
            get(app, route, None).await.0,
            StatusCode::NOT_FOUND,
            "{route} anonymous"
        );
        assert_eq!(
            get(app, route, Some(&fixture.owner_token)).await.0,
            StatusCode::NOT_FOUND,
            "{route} for an ordinary account"
        );
        let (status, body) = get(app, route, Some(&fixture.admin_token)).await;
        assert_eq!(status, StatusCode::OK, "{route} for the admin: {body}");
    }
    let server_detail = format!("/api/v1/admin/servers/{}", fixture.server_id);
    let account_detail = format!("/api/v1/admin/accounts/{}", fixture.owner_id);
    for route in [&server_detail, &account_detail] {
        assert_eq!(
            get(app, route, Some(&fixture.owner_token)).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get(app, route, Some(&fixture.admin_token)).await.0,
            StatusCode::OK
        );
    }

    let (_, session) = get(app, "/api/v1/session", Some(&fixture.admin_token)).await;
    assert_eq!(session["is_admin"], true);
    let (_, session) = get(app, "/api/v1/session", Some(&fixture.owner_token)).await;
    assert_eq!(session["is_admin"], false);

    // Suspending the admin's account closes the dashboard.
    sqlx::query("UPDATE users SET account_status = 'suspended' WHERE id = $1")
        .bind(fixture.admin_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(
        get(app, "/api/v1/admin/overview", Some(&fixture.admin_token))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (_, session) = get(app, "/api/v1/session", Some(&fixture.admin_token)).await;
    assert_eq!(session["is_admin"], false);
}

#[tokio::test]
async fn an_admin_reads_any_server_but_changes_nothing() {
    let _serial = DB_TEST_LOCK.lock().await;
    let fixture = fixture().await;
    let app = &fixture.app;
    let server = fixture.server_id;
    let (_, stranger_token) = user_session(&fixture.pool, "active").await;

    for path in [
        "",
        "/logs",
        "/activity",
        "/snapshots",
        "/exports",
        "/players",
        "/runtime-stats",
        "/whitelist",
        "/operators",
        "/plugins",
        "/ai",
    ] {
        let uri = format!("/api/v1/servers/{server}{path}");
        let (status, body) = get(app, &uri, Some(&fixture.admin_token)).await;
        assert_eq!(status, StatusCode::OK, "admin reads {uri}: {body}");
        assert_eq!(
            get(app, &uri, Some(&stranger_token)).await.0,
            StatusCode::NOT_FOUND,
            "a stranger cannot read {uri}"
        );
        assert_eq!(
            get(app, &uri, Some(&fixture.owner_token)).await.0,
            StatusCode::OK,
            "the owner still reads {uri}"
        );
    }

    let writes = [
        (
            Method::PATCH,
            format!("/api/v1/servers/{server}"),
            json!({"settings": {"motd": "changed by an admin"}}),
        ),
        (
            Method::POST,
            format!("/api/v1/servers/{server}/commands"),
            json!({"command": "start"}),
        ),
        (
            Method::POST,
            format!("/api/v1/servers/{server}/whitelist"),
            json!({"name": "Notch"}),
        ),
        (
            Method::POST,
            format!("/api/v1/servers/{server}/operators"),
            json!({"name": "Notch"}),
        ),
        (
            Method::POST,
            format!("/api/v1/servers/{server}/console/commands"),
            json!({"request_id": Uuid::new_v4(), "command": "say hi"}),
        ),
        (
            Method::POST,
            format!("/api/v1/servers/{server}/exports"),
            Value::Null,
        ),
        (
            Method::PUT,
            format!("/api/v1/servers/{server}/ai/settings"),
            json!({"enabled": true, "model": null, "effort": null, "monthly_budget_micros": null}),
        ),
    ];
    for (method, uri, body) in writes {
        let response = app
            .clone()
            .oneshot(request(
                method.clone(),
                &uri,
                Some(&fixture.admin_token),
                (!body.is_null()).then_some(body),
            ))
            .await
            .unwrap();
        assert!(
            matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED
            ),
            "admin {method} {uri} answered {}",
            response.status()
        );
    }
    let motd: String =
        sqlx::query_scalar("SELECT coalesce(settings->>'motd', '') FROM servers WHERE id = $1")
            .bind(server)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_ne!(motd, "changed by an admin");
    let (operations, exports, commands): (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM operations WHERE server_id = $1),
                (SELECT count(*) FROM world_exports WHERE server_id = $1),
                (SELECT count(*) FROM console_commands WHERE server_id = $1)",
    )
    .bind(server)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!((operations, exports, commands), (0, 0, 0));
}

#[tokio::test]
async fn the_dashboard_finds_failed_servers_their_failures_and_their_owners() {
    let _serial = DB_TEST_LOCK.lock().await;
    let fixture = fixture().await;
    let app = &fixture.app;
    let server = fixture.server_id;
    let subdomain: String = sqlx::query_scalar("SELECT subdomain FROM servers WHERE id = $1")
        .bind(server)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    let marker = format!("restore failed for chunk {}", Uuid::new_v4());
    sqlx::query(
        "UPDATE servers SET observed_state = 'failed', generation = 3, last_error = $2 WHERE id = $1",
    )
    .bind(server)
    .bind(&marker)
    .execute(&fixture.pool)
    .await
    .unwrap();
    for attempt in 0..3 {
        sqlx::query(
            "INSERT INTO operations(id, server_id, kind, status, generation, attempt, last_error)
             VALUES($1, $2, 'start', 'failed', 3, $3, $4)",
        )
        .bind(Uuid::new_v4())
        .bind(server)
        .bind(attempt)
        .bind(format!(
            "worker 10.0.0.{attempt} timed out after {attempt}0 seconds"
        ))
        .execute(&fixture.pool)
        .await
        .unwrap();
    }

    let (_, page) = get(
        app,
        &format!("/api/v1/admin/servers?filter=failed&search={subdomain}"),
        Some(&fixture.admin_token),
    )
    .await;
    assert_eq!(page["total"], 1, "{page}");
    let row = &page["items"][0];
    assert_eq!(row["id"], server.to_string());
    assert_eq!(row["observed_state"], "failed");
    assert_eq!(row["owner_id"], fixture.owner_id.to_string());
    assert_eq!(row["last_error"], marker);
    let (_, page) = get(
        app,
        &format!("/api/v1/admin/servers?filter=online&search={subdomain}"),
        Some(&fixture.admin_token),
    )
    .await;
    assert_eq!(page["total"], 0);
    let (_, page) = get(
        app,
        &format!("/api/v1/admin/servers?search={server}&limit=1"),
        Some(&fixture.admin_token),
    )
    .await;
    assert_eq!(page["items"][0]["id"], server.to_string(), "found by id");

    let (_, detail) = get(
        app,
        &format!("/api/v1/admin/servers/{server}?range=week"),
        Some(&fixture.admin_token),
    )
    .await;
    assert_eq!(detail["operations"].as_array().unwrap().len(), 3);
    assert_eq!(detail["operations"][0]["kind"], "start");
    assert!(detail["placement"].is_null());

    let (_, failures) = get(app, "/api/v1/admin/failures", Some(&fixture.admin_token)).await;
    let groups = failures["groups"].as_array().unwrap();
    let timeouts = groups
        .iter()
        .find(|group| {
            group["source"] == "operation"
                && group["server_id"] == server.to_string()
                && group["signature"] == "worker <n>.<n>.<n>.<n> timed out after <n> seconds"
        })
        .unwrap_or_else(|| panic!("the three timeouts are one group: {failures}"));
    assert_eq!(timeouts["count"], 3);
    assert_eq!(timeouts["subdomain"], subdomain);
    assert!(
        groups
            .iter()
            .any(|group| group["source"] == "server" && group["message"] == marker),
        "the failed server itself is listed"
    );

    let (_, accounts) = get(
        app,
        &format!("/api/v1/admin/accounts?search={}", fixture.owner_id),
        Some(&fixture.admin_token),
    )
    .await;
    assert_eq!(accounts["total"], 1);
    assert_eq!(accounts["items"][0]["servers"], 1);
    assert_eq!(accounts["items"][0]["identities"][0]["provider"], "google");
    let (_, account) = get(
        app,
        &format!("/api/v1/admin/accounts/{}", fixture.owner_id),
        Some(&fixture.admin_token),
    )
    .await;
    assert_eq!(account["is_admin"], false);
    assert_eq!(account["servers"][0]["id"], server.to_string());
    assert_eq!(account["sessions"].as_array().unwrap().len(), 1);
    let (_, admin) = get(
        app,
        &format!("/api/v1/admin/accounts/{}", fixture.admin_id),
        Some(&fixture.admin_token),
    )
    .await;
    assert_eq!(admin["is_admin"], true);

    let (_, overview) = get(app, "/api/v1/admin/overview", Some(&fixture.admin_token)).await;
    assert!(overview["servers"]["failed"].as_i64().unwrap() >= 1);
    assert!(overview["operations"]["failed_last_day"].as_i64().unwrap() >= 3);
    assert!(
        overview["storage"]["largest_tables"]
            .as_array()
            .unwrap()
            .iter()
            .any(|table| table["name"] == "servers")
    );
}

#[tokio::test]
async fn workers_list_their_servers_and_the_minute_pass_keeps_history() {
    let _serial = DB_TEST_LOCK.lock().await;
    let fixture = fixture().await;
    let app = &fixture.app;
    let server = fixture.server_id;
    let worker_id = Uuid::new_v4();
    let worker = healthy_worker_template(
        worker_id,
        format!("admin-worker-{worker_id}"),
        "127.0.0.1".to_owned(),
        8 * 1024 * 1024 * 1024,
        40 * 1024 * 1024 * 1024,
        4000,
        36000,
        36100,
    );
    heartbeat_worker(&fixture.pool, &worker).await.unwrap();
    sqlx::query(
        "UPDATE servers SET generation = 1, observed_state = 'online', desired_state = 'running'
         WHERE id = $1",
    )
    .bind(server)
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO placements(server_id, worker_id, generation, lease_token,
                                java_port, bedrock_port, signal_port, active)
         VALUES($1, $2, 1, $3, 36000, 36001, 36002, true)",
    )
    .bind(server)
    .bind(worker_id)
    .bind(Uuid::new_v4())
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO server_runtime_stats(server_id, generation, stats)
         VALUES($1, 1, $2)",
    )
    .bind(server)
    .bind(json!({
        "generation": 1, "cpu_percent": 50.0, "cpu_limit_millis": 1000,
        "memory_used_bytes": 1024, "memory_limit_bytes": 4096,
        "disk_used_bytes": 1, "disk_limit_bytes": 2, "tps": 19.0,
    }))
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO server_players(server_id, player_id, name, edition) VALUES($1, $2, 'Alex', 'java')",
    )
    .bind(server)
    .bind(Uuid::new_v4())
    .execute(&fixture.pool)
    .await
    .unwrap();
    // Samples past their retention, and a wake today.
    sqlx::query(
        "INSERT INTO server_runtime_samples(server_id, minute, players_online)
         VALUES($1, date_trunc('minute', now()) - interval '8 days', 1)",
    )
    .bind(server)
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO fleet_stats(minute, servers_online, players_online)
         VALUES(date_trunc('minute', now()) - interval '31 days', 1, 1),
               (date_trunc('minute', now()) - interval '29 days', 1, 1)
         ON CONFLICT(minute) DO NOTHING",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO operations(id, server_id, kind, status, generation) VALUES($1, $2, 'start', 'succeeded', 1)",
    )
    .bind(Uuid::new_v4())
    .bind(server)
    .execute(&fixture.pool)
    .await
    .unwrap();
    let wakes_before: i32 = sqlx::query_scalar(
        "SELECT coalesce((SELECT wakes FROM daily_stats WHERE day = (now() AT TIME ZONE 'UTC')::date), 0)",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();

    fleet_stats::record_minute(&fixture.pool).await.unwrap();
    fleet_stats::record_minute(&fixture.pool).await.unwrap();

    type Sample = (i32, Option<i64>, Option<i64>, Option<f64>);
    let samples: Vec<Sample> = sqlx::query_as(
        "SELECT players_online, cpu_used_millis, memory_used_bytes, tps
         FROM server_runtime_samples WHERE server_id = $1 ORDER BY minute",
    )
    .bind(server)
    .fetch_all(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(
        samples,
        vec![(1, Some(500), Some(1024), Some(19.0))],
        "one sample this minute; the 8-day-old one is gone"
    );
    let fleet_days: Vec<i64> = sqlx::query_scalar(
        "SELECT (extract(epoch FROM now() - minute) / 86400)::bigint FROM fleet_stats
         WHERE minute < now() - interval '1 day' ORDER BY minute",
    )
    .fetch_all(&fixture.pool)
    .await
    .unwrap();
    assert!(fleet_days.contains(&29), "a month of fleet samples is kept");
    assert!(fleet_days.iter().all(|days| *days <= 30));
    let wakes_after: i32 = sqlx::query_scalar(
        "SELECT wakes FROM daily_stats WHERE day = (now() AT TIME ZONE 'UTC')::date",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(
        wakes_after,
        wakes_before + 1,
        "today's roll-up counts the wake"
    );

    let (_, workers) = get(app, "/api/v1/admin/workers", Some(&fixture.admin_token)).await;
    let listed = workers
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == worker_id.to_string())
        .unwrap();
    assert_eq!(listed["live"], true);
    assert_eq!(listed["cpu_used_millis"], 500);
    assert_eq!(listed["memory_used_bytes"], 1024);
    assert_eq!(listed["servers"][0]["id"], server.to_string());
    assert_eq!(listed["servers"][0]["players_online"], 1);

    let (_, detail) = get(
        app,
        &format!("/api/v1/admin/servers/{server}?range=hour"),
        Some(&fixture.admin_token),
    )
    .await;
    assert_eq!(detail["placement"]["worker_id"], worker_id.to_string());
    assert_eq!(detail["server"]["worker_name"], worker.name);
    assert_eq!(detail["samples"].as_array().unwrap().len(), 1);
    assert_eq!(detail["samples"][0]["cpu_used_millis"], 500);

    let (_, fleet) = get(
        app,
        "/api/v1/admin/fleet?range=month",
        Some(&fixture.admin_token),
    )
    .await;
    assert!(fleet.as_array().unwrap().len() >= 2, "{fleet}");
    let (_, growth) = get(app, "/api/v1/admin/growth", Some(&fixture.admin_token)).await;
    let today = growth["days"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(today["wakes"], wakes_after);
    assert!(today["peak_servers_online"].as_i64().unwrap() >= 1);
    let cohort = growth["cohorts"].as_array().unwrap().last().unwrap();
    assert!(cohort["accounts"].as_i64().unwrap() >= 2);
    assert!(cohort["returned"][0].as_i64().unwrap() >= 1, "{growth}");

    // A live worker left behind would take the starts of later suites sharing the database.
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(fixture.owner_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workers WHERE id = $1")
        .bind(worker_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn maintenance_forgets_old_console_lines_and_events() {
    let _serial = DB_TEST_LOCK.lock().await;
    let fixture = fixture().await;
    let server = fixture.server_id;
    // Both tables only append, so the oldest rows have the lowest ids: put these below
    // every row already there.
    let first_log: i64 = sqlx::query_scalar("SELECT coalesce(min(id), 1000) - 10 FROM server_logs")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    for (offset, days) in [(0, 20), (1, 10), (2, 0)] {
        sqlx::query(
            "INSERT INTO server_logs(id, server_id, line, created_at)
             VALUES($1, $2, $3, now() - make_interval(days => $4))",
        )
        .bind(first_log + offset)
        .bind(server)
        .bind(format!("line from {days} days ago"))
        .bind(days)
        .execute(&fixture.pool)
        .await
        .unwrap();
    }
    let first_event: i64 =
        sqlx::query_scalar("SELECT coalesce(min(id), 1000) - 10 FROM server_events")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    for (offset, days) in [(0, 8), (1, 6)] {
        sqlx::query(
            "INSERT INTO server_events(id, server_id, event_type, created_at)
             VALUES($1, $2, 'server', now() - make_interval(days => $3))",
        )
        .bind(first_event + offset)
        .bind(server)
        .bind(days)
        .execute(&fixture.pool)
        .await
        .unwrap();
    }

    let report = WorkerMaintenance::new(
        fixture.pool.clone(),
        Arc::new(object_store::memory::InMemory::new()),
    )
    .run_once()
    .await
    .unwrap();
    assert!(report.server_logs_deleted >= 1);
    assert!(report.server_events_deleted >= 1);
    assert!(report.did_work());

    let lines: Vec<String> =
        sqlx::query_scalar("SELECT line FROM server_logs WHERE server_id = $1 ORDER BY id")
            .bind(server)
            .fetch_all(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(lines, ["line from 10 days ago", "line from 0 days ago"]);
    let events: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM server_events WHERE server_id = $1 AND id < $2 + 10 ORDER BY id",
    )
    .bind(server)
    .bind(first_event)
    .fetch_all(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(events, [first_event + 1], "the 8-day-old event is gone");
}

#[tokio::test]
async fn cohorts_count_returns_by_calendar_week() {
    let _serial = DB_TEST_LOCK.lock().await;
    let fixture = fixture().await;
    let app = &fixture.app;
    // Signed up late on a Sunday three weeks ago, woken early on the Monday after: a
    // return in week 1, though it came two hours after signing up.
    let signed_up = "date_trunc('week', now() AT TIME ZONE 'UTC') - interval '2 weeks 1 hour'";
    let week: chrono::NaiveDate =
        sqlx::query_scalar(&format!("SELECT date_trunc('week', {signed_up})::date"))
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    let cohort = |growth: &Value| -> Vec<i64> {
        growth["cohorts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|cohort| cohort["week"] == week.to_string())
            .map(|cohort| serde_json::from_value::<Vec<i64>>(cohort["returned"].clone()).unwrap())
            .unwrap_or_else(|| vec![0; 4])
    };
    let (_, before) = get(app, "/api/v1/admin/growth", Some(&fixture.admin_token)).await;
    let before = cohort(&before);

    let (user, _) = user_session(&fixture.pool, "active").await;
    sqlx::query(&format!(
        "UPDATE users SET created_at = ({signed_up}) AT TIME ZONE 'UTC' WHERE id = $1"
    ))
    .bind(user)
    .execute(&fixture.pool)
    .await
    .unwrap();
    let server = create_test_server(&fixture.pool, user, "cohort").await;
    sqlx::query(&format!(
        "INSERT INTO operations(id, server_id, kind, status, generation, created_at)
         VALUES($1, $2, 'start', 'succeeded', 1, ({signed_up} + interval '2 hours') AT TIME ZONE 'UTC')"
    ))
    .bind(Uuid::new_v4())
    .bind(server)
    .execute(&fixture.pool)
    .await
    .unwrap();

    let (_, after) = get(app, "/api/v1/admin/growth", Some(&fixture.admin_token)).await;
    let after = cohort(&after);
    assert_eq!(after.len(), 4, "weeks 0 to 3 have begun: {after:?}");
    assert_eq!(after[0], before[0], "no return in the signup week");
    assert_eq!(after[1], before[1] + 1, "a return in the week after");
}

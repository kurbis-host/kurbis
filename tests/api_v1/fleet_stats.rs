use super::support::*;
use kurbis_hoster::fleet_stats::{self, FleetUsage};

const GIB: i64 = 1024 * 1024 * 1024;

async fn insert_server(
    tx: &mut sqlx::PgConnection,
    owner_id: Uuid,
    observed: &str,
    desired: &str,
    generation: i64,
) -> Uuid {
    let server_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO servers(id, owner_id, subdomain, observed_state, desired_state, generation)
         VALUES($1, $2, $3, $4::server_observed_state, $5::server_desired_state, $6)",
    )
    .bind(server_id)
    .bind(owner_id)
    .bind(format!("fleet-{}", &server_id.simple().to_string()[..12]))
    .bind(observed)
    .bind(desired)
    .bind(generation)
    .execute(&mut *tx)
    .await
    .unwrap();
    server_id
}

async fn insert_sample(
    tx: &mut sqlx::PgConnection,
    server_id: Uuid,
    generation: i64,
    age_seconds: f64,
    stats: Value,
) {
    sqlx::query(
        "INSERT INTO server_runtime_stats(server_id, generation, stats, measured_at)
         VALUES($1, $2, $3, now() - make_interval(secs => $4))",
    )
    .bind(server_id)
    .bind(generation)
    .bind(stats)
    .bind(age_seconds)
    .execute(&mut *tx)
    .await
    .unwrap();
}

async fn insert_player(tx: &mut sqlx::PgConnection, server_id: Uuid) {
    sqlx::query(
        "INSERT INTO server_players(server_id, player_id, name, edition) VALUES($1, $2, 'Alex', 'java')",
    )
    .bind(server_id)
    .bind(Uuid::new_v4())
    .execute(&mut *tx)
    .await
    .unwrap();
}

async fn insert_worker(
    tx: &mut sqlx::PgConnection,
    cpu_millis: i64,
    memory_bytes: i64,
    age_seconds: f64,
) {
    let worker_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workers(id, name, address, total_memory_bytes, total_disk_bytes, total_cpu_millis,
                             heartbeat_at, signal_address)
         VALUES($1, $2, '127.0.0.1', $3, $4, $5, now() - make_interval(secs => $6), '127.0.0.1:1')",
    )
    .bind(worker_id)
    .bind(format!("fleet-worker-{worker_id}"))
    .bind(memory_bytes)
    .bind(100 * GIB)
    .bind(cpu_millis)
    .bind(age_seconds)
    .execute(&mut *tx)
    .await
    .unwrap();
}

fn stats(
    generation: i64,
    cpu_percent: f64,
    cpu_limit_millis: i64,
    memory_used_bytes: i64,
    tps: f64,
) -> Value {
    json!({
        "generation": generation,
        "cpu_percent": cpu_percent,
        "cpu_limit_millis": cpu_limit_millis,
        "memory_used_bytes": memory_used_bytes,
        "memory_limit_bytes": 2 * GIB,
        "disk_used_bytes": 1,
        "disk_limit_bytes": 2,
        "tps": tps,
    })
}

#[tokio::test]
async fn fleet_usage_counts_online_servers_their_fresh_current_samples_and_live_workers() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_id, _) = user_session(&pool, "active").await;
    // Everything other tests left online or alive is set aside for this transaction only.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE servers SET observed_state = 'sleeping' WHERE observed_state = 'online'")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE workers SET heartbeat_at = now() - interval '1 hour'")
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        fleet_stats::usage(&mut *tx).await.unwrap(),
        FleetUsage {
            servers_online: 0,
            players_online: 0,
            cpu_used_millis: None,
            cpu_total_millis: None,
            memory_used_bytes: None,
            memory_total_bytes: None,
            tps: None,
        },
        "nothing running is no usage, not zero usage"
    );

    // Two running servers with fresh samples of their current generation.
    let first = insert_server(&mut tx, owner_id, "online", "running", 1).await;
    insert_sample(&mut tx, first, 1, 1.0, stats(1, 50.0, 2000, GIB, 20.0)).await;
    insert_player(&mut tx, first).await;
    insert_player(&mut tx, first).await;
    let second = insert_server(&mut tx, owner_id, "online", "running", 2).await;
    insert_sample(&mut tx, second, 2, 1.0, stats(2, 25.0, 4000, GIB / 2, 18.0)).await;
    // Running, but its sample is from the generation before, or too old: no usage of its own.
    let restarted = insert_server(&mut tx, owner_id, "online", "running", 3).await;
    insert_sample(&mut tx, restarted, 2, 1.0, stats(2, 100.0, 4000, GIB, 1.0)).await;
    let quiet = insert_server(&mut tx, owner_id, "online", "running", 1).await;
    insert_sample(&mut tx, quiet, 1, 120.0, stats(1, 100.0, 4000, GIB, 1.0)).await;
    // Asleep: neither it nor anyone listed on it counts, whatever its last sample says.
    let asleep = insert_server(&mut tx, owner_id, "sleeping", "stopped", 1).await;
    insert_sample(&mut tx, asleep, 1, 1.0, stats(1, 100.0, 4000, GIB, 1.0)).await;
    insert_player(&mut tx, asleep).await;
    // One live worker and one gone quiet.
    insert_worker(&mut tx, 8000, 16 * GIB, 1.0).await;
    insert_worker(&mut tx, 4000, 8 * GIB, 120.0).await;

    assert_eq!(
        fleet_stats::usage(&mut *tx).await.unwrap(),
        FleetUsage {
            servers_online: 4,
            players_online: 2,
            cpu_used_millis: Some(1000 + 1000),
            cpu_total_millis: Some(8000),
            memory_used_bytes: Some(GIB + GIB / 2),
            memory_total_bytes: Some(16 * GIB),
            tps: Some(19.0),
        }
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn fleet_stats_are_public_cacheable_and_carry_their_history() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let app = router(state(pool.clone()));
    let read = || async {
        let response = app
            .clone()
            .oneshot(request(Method::GET, "/api/v1/stats", None, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "no session is needed");
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "public, max-age=10"
        );
        json_body(response).await
    };

    let before = read().await;
    let (owner_id, _) = user_session(&pool, "active").await;
    create_test_server(&pool, owner_id, "fleet").await;
    create_test_server(&pool, owner_id, "fleet").await;
    let deleted = create_test_server(&pool, owner_id, "fleet").await;
    sqlx::query("UPDATE servers SET desired_state = 'deleted' WHERE id = $1")
        .bind(deleted)
        .execute(&pool)
        .await
        .unwrap();
    // A minute inside the two hours, and one outside them.
    for minutes in [5, 200] {
        sqlx::query(
            "INSERT INTO fleet_stats(minute, servers_online, players_online, tps)
             VALUES(date_trunc('minute', now()) - make_interval(mins => $1), 7, 11, 19.5)
             ON CONFLICT(minute) DO UPDATE SET servers_online = 7, players_online = 11, tps = 19.5",
        )
        .bind(minutes)
        .execute(&pool)
        .await
        .unwrap();
    }

    let after = read().await;
    assert_eq!(
        after["servers_created"].as_i64().unwrap(),
        before["servers_created"].as_i64().unwrap() + 2,
        "a deleted server is not counted"
    );
    let days = after["servers_by_day"].as_array().unwrap();
    assert_eq!(days.len(), 30);
    assert!(
        days.windows(2)
            .all(|pair| pair[0].as_i64() <= pair[1].as_i64())
    );
    assert_eq!(days.last().unwrap(), &after["servers_created"]);

    let history = after["history"].as_array().unwrap();
    let minutes: Vec<chrono::DateTime<chrono::Utc>> = history
        .iter()
        .map(|sample| sample["minute"].as_str().unwrap().parse().unwrap())
        .collect();
    assert!(
        minutes.windows(2).all(|pair| pair[0] < pair[1]),
        "oldest first"
    );
    let now = chrono::Utc::now();
    assert!(
        minutes
            .iter()
            .all(|minute| *minute > now - chrono::Duration::minutes(121))
    );
    let five_ago = history
        .iter()
        .zip(&minutes)
        .find(|(_, minute)| {
            (now - **minute).num_minutes() >= 4 && (now - **minute).num_minutes() <= 6
        })
        .map(|(sample, _)| sample)
        .expect("the sample five minutes ago is in the history");
    assert_eq!(five_ago["servers_online"], 7);
    assert_eq!(five_ago["players_online"], 11);
    assert_eq!(five_ago["tps"], 19.5);
    assert!(five_ago["cpu_used_millis"].is_null());
}

#[tokio::test]
async fn a_minute_is_sampled_once_and_a_month_is_kept() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    // Start well inside a minute, so both samples below fall in the same one.
    let second: f64 = sqlx::query_scalar("SELECT extract(second FROM now())::double precision")
        .fetch_one(&pool)
        .await
        .unwrap();
    if second > 50.0 {
        tokio::time::sleep(Duration::from_secs_f64(61.0 - second)).await;
    }
    sqlx::query(
        "INSERT INTO fleet_stats(minute, servers_online, players_online)
         VALUES(date_trunc('minute', now()) - interval '31 days', 1, 1)
         ON CONFLICT(minute) DO NOTHING",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO fleet_stats(minute, servers_online, players_online)
         VALUES(date_trunc('minute', now()), 4242, 4242)
         ON CONFLICT(minute) DO UPDATE SET servers_online = 4242, players_online = 4242",
    )
    .execute(&pool)
    .await
    .unwrap();

    fleet_stats::record_minute(&pool).await.unwrap();
    fleet_stats::record_minute(&pool).await.unwrap();

    let (count, online): (i64, i32) = sqlx::query_as(
        "SELECT count(*), max(servers_online) FROM fleet_stats WHERE minute = date_trunc('minute', now())",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (count, online),
        (1, 4242),
        "a minute already sampled is left as it was"
    );
    let old: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM fleet_stats WHERE minute < now() - interval '30 days'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(old, 0);
}

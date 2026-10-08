use super::support::*;

fn sample_stats(generation: i64, cpu_percent: f64) -> Value {
    json!({
        "generation": generation,
        "cpu_percent": cpu_percent,
        "cpu_limit_millis": 500,
        "memory_used_bytes": 2048,
        "memory_limit_bytes": 4096,
        "disk_used_bytes": 8192,
        "disk_limit_bytes": 16384,
        "tps": 19.5,
        "measured_at": "2000-01-01T00:00:00Z",
    })
}

fn assert_stats_body(body: Value, generation: i64, cpu_percent: f64) {
    assert_eq!(body["generation"], generation);
    assert_eq!(body["cpu_percent"], cpu_percent);
    assert_eq!(body["cpu_limit_millis"], 500);
    assert_eq!(body["memory_used_bytes"], 2048);
    assert_eq!(body["memory_limit_bytes"], 4096);
    assert_eq!(body["disk_used_bytes"], 8192);
    assert_eq!(body["disk_limit_bytes"], 16384);
    assert_eq!(body["tps"], 19.5);
    assert!(body["measured_at"].as_str().is_some());
}

async fn sync_stats(
    app: &axum::Router,
    server_id: Uuid,
    token: &str,
    generation: i64,
    stats: Value,
) -> axum::response::Response {
    app.clone()
        .oneshot(bearer_request(
            Method::POST,
            &format!("/internal/worker/control/servers/{server_id}/runtime-stats"),
            token,
            Some(json!({"generation": generation, "stats": stats})),
        ))
        .await
        .unwrap()
}

#[tokio::test]
async fn runtime_stats_read_and_sync_are_authorized_generation_fenced_and_replace_by_generation() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_id, owner_token) = user_session(&pool, "active").await;
    let (other_owner_id, other_owner_token) = user_session(&pool, "active").await;
    let server_id = create_test_server(&pool, owner_id, "runtime-stats").await;
    let worker_id = Uuid::new_v4();
    let worker = healthy_worker_template(
        worker_id,
        format!("runtime-stats-worker-{worker_id}"),
        "127.0.0.1".to_owned(),
        8 * 1024 * 1024 * 1024,
        40 * 1024 * 1024 * 1024,
        4000,
        24000,
        24100,
    );
    heartbeat_worker(&pool, &worker).await.unwrap();
    let worker_token = register_worker_api_token(&pool, worker_id).await;
    sqlx::query("UPDATE servers SET generation = 1 WHERE id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO placements(server_id, worker_id, generation, lease_token,
                                java_port, bedrock_port, signal_port, active)
         VALUES($1, $2, 1, $3, 24000, 24001, 24002, true)",
    )
    .bind(server_id)
    .bind(worker_id)
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .unwrap();

    let app = router(state(pool.clone()));
    let stats_uri = format!("/api/v1/servers/{server_id}/runtime-stats");

    let anonymous = app
        .clone()
        .oneshot(request(Method::GET, &stats_uri, None, None))
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

    let foreign_owner = app
        .clone()
        .oneshot(request(
            Method::GET,
            &stats_uri,
            Some(&other_owner_token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(foreign_owner.status(), StatusCode::NOT_FOUND);

    let no_sample = app
        .clone()
        .oneshot(request(Method::GET, &stats_uri, Some(&owner_token), None))
        .await
        .unwrap();
    assert_eq!(no_sample.status(), StatusCode::OK);
    assert!(json_body(no_sample).await.is_null());

    let unknown_worker = "A".repeat(43);
    let unauthenticated_sync =
        sync_stats(&app, server_id, &unknown_worker, 1, sample_stats(1, 31.25)).await;
    assert_eq!(unauthenticated_sync.status(), StatusCode::UNAUTHORIZED);

    let persisted = sample_stats(1, 31.25);
    let accepted = sync_stats(&app, server_id, &worker_token, 1, persisted.clone()).await;
    assert_eq!(accepted.status(), StatusCode::NO_CONTENT);

    let owner_read = app
        .clone()
        .oneshot(request(Method::GET, &stats_uri, Some(&owner_token), None))
        .await
        .unwrap();
    assert_eq!(owner_read.status(), StatusCode::OK);
    let owner_body = json_body(owner_read).await;
    assert_stats_body(owner_body.clone(), 1, 31.25);
    let accepted_at = chrono::DateTime::parse_from_rfc3339(
        owner_body["measured_at"]
            .as_str()
            .expect("timestamp is serialized"),
    )
    .unwrap();
    assert!(accepted_at > chrono::Utc::now() - chrono::Duration::seconds(10));
    let (stored_at, nested_at): (chrono::DateTime<chrono::Utc>, Option<String>) = sqlx::query_as(
        "SELECT measured_at, stats->>'measured_at' FROM server_runtime_stats WHERE server_id=$1",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(nested_at, None);
    assert_eq!(stored_at.timestamp(), accepted_at.timestamp());

    let foreign_read = app
        .clone()
        .oneshot(request(
            Method::GET,
            &stats_uri,
            Some(&other_owner_token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(foreign_read.status(), StatusCode::NOT_FOUND);

    let stale_generation =
        sync_stats(&app, server_id, &worker_token, 2, sample_stats(2, 88.0)).await;
    assert_eq!(stale_generation.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(stale_generation).await["code"],
        "worker_assignment_invalid"
    );

    let invalid_generation =
        sync_stats(&app, server_id, &worker_token, 0, sample_stats(0, 88.0)).await;
    assert_eq!(
        invalid_generation.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        json_body(invalid_generation).await["code"],
        "invalid_runtime_stats"
    );

    let stored_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM server_runtime_stats WHERE server_id = $1 AND generation = 1",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        stored_count, 1,
        "rejected samples must not mutate the current row"
    );

    let mismatched_sample =
        sync_stats(&app, server_id, &worker_token, 1, sample_stats(2, 88.0)).await;
    assert_eq!(mismatched_sample.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json_body(mismatched_sample).await["code"],
        "invalid_runtime_stats"
    );

    sqlx::query("UPDATE placements SET active = false WHERE server_id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();
    let inactive_placement =
        sync_stats(&app, server_id, &worker_token, 1, sample_stats(1, 77.0)).await;
    assert_eq!(inactive_placement.status(), StatusCode::CONFLICT);
    sqlx::query("UPDATE placements SET active = true WHERE server_id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();

    sqlx::query("UPDATE servers SET generation = 2 WHERE id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE placements SET generation = 2 WHERE server_id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();

    let invisible_old_sample = app
        .clone()
        .oneshot(request(Method::GET, &stats_uri, Some(&owner_token), None))
        .await
        .unwrap();
    assert!(json_body(invisible_old_sample).await.is_null());

    let old_worker_generation =
        sync_stats(&app, server_id, &worker_token, 1, sample_stats(1, 50.0)).await;
    assert_eq!(old_worker_generation.status(), StatusCode::CONFLICT);

    let current = sample_stats(2, 62.5);
    let accepted_current = sync_stats(&app, server_id, &worker_token, 2, current.clone()).await;
    assert_eq!(accepted_current.status(), StatusCode::NO_CONTENT);
    let current_read = app
        .clone()
        .oneshot(request(Method::GET, &stats_uri, Some(&owner_token), None))
        .await
        .unwrap();
    assert_stats_body(json_body(current_read).await, 2, 62.5);

    sqlx::query(
        "UPDATE server_runtime_stats SET measured_at = now() - interval '31 seconds'
         WHERE server_id = $1",
    )
    .bind(server_id)
    .execute(&pool)
    .await
    .unwrap();
    let stale = app
        .clone()
        .oneshot(request(Method::GET, &stats_uri, Some(&owner_token), None))
        .await
        .unwrap();
    assert!(json_body(stale).await.is_null());

    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(vec![owner_id, other_owner_id])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workers WHERE id = $1")
        .bind(worker_id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn tab_stats_setting_defaults_off_and_is_independently_toggleable() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_id, owner_token) = user_session(&pool, "active").await;
    let (other_owner_id, other_owner_token) = user_session(&pool, "active").await;
    let server_id = create_test_server(&pool, owner_id, "tab-stats").await;
    let app = router(state(pool.clone()));
    let uri = format!("/api/v1/servers/{server_id}");

    let initial = app
        .clone()
        .oneshot(request(Method::GET, &uri, Some(&owner_token), None))
        .await
        .unwrap();
    assert_eq!(initial.status(), StatusCode::OK);
    assert_eq!(json_body(initial).await["settings"]["tab_stats"], false);

    let enabled = app
        .clone()
        .oneshot(request(
            Method::PATCH,
            &uri,
            Some(&owner_token),
            Some(json!({"settings":{"tab_stats":true}})),
        ))
        .await
        .unwrap();
    assert_eq!(enabled.status(), StatusCode::OK);
    assert_eq!(json_body(enabled).await["settings"]["tab_stats"], true);

    let unrelated_setting = app
        .clone()
        .oneshot(request(
            Method::PATCH,
            &uri,
            Some(&owner_token),
            Some(json!({"settings":{"motd":"Keep stats enabled"}})),
        ))
        .await
        .unwrap();
    assert_eq!(unrelated_setting.status(), StatusCode::OK);
    let settings = json_body(unrelated_setting).await["settings"].clone();
    assert_eq!(settings["motd"], "Keep stats enabled");
    assert_eq!(settings["tab_stats"], true);

    let disabled = app
        .clone()
        .oneshot(request(
            Method::PATCH,
            &uri,
            Some(&owner_token),
            Some(json!({"settings":{"tab_stats":false}})),
        ))
        .await
        .unwrap();
    assert_eq!(disabled.status(), StatusCode::OK);
    assert_eq!(json_body(disabled).await["settings"]["tab_stats"], false);

    let invalid_value = app
        .clone()
        .oneshot(request(
            Method::PATCH,
            &uri,
            Some(&owner_token),
            Some(json!({"settings":{"tab_stats":"yes"}})),
        ))
        .await
        .unwrap();
    assert_eq!(invalid_value.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let foreign_patch = app
        .clone()
        .oneshot(request(
            Method::PATCH,
            &uri,
            Some(&other_owner_token),
            Some(json!({"settings":{"tab_stats":true}})),
        ))
        .await
        .unwrap();
    assert_eq!(foreign_patch.status(), StatusCode::NOT_FOUND);

    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(vec![owner_id, other_owner_id])
        .execute(&pool)
        .await
        .unwrap();
}

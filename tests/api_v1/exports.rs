use super::support::*;

#[tokio::test]
async fn world_exports_require_a_retained_source_and_enforce_ownership_cancellation_and_expiry() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_id, token) = user_session(&pool, "active").await;
    let (_, other_token) = user_session(&pool, "active").await;
    let server_id = Uuid::new_v4();
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1,$2,$3)")
        .bind(server_id)
        .bind(owner_id)
        .bind(format!("export-{}", &server_id.simple().to_string()[..12]))
        .execute(&pool)
        .await
        .unwrap();
    let app_state = state(pool.clone());
    let object_store = app_state.object_store.clone();
    let app = router(app_state);
    let endpoint = format!("/api/v1/servers/{server_id}/exports");

    let uninitialized = app
        .clone()
        .oneshot(request(Method::POST, &endpoint, Some(&token), None))
        .await
        .unwrap();
    assert_eq!(uninitialized.status(), StatusCode::CONFLICT);
    let export_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM world_exports WHERE server_id = $1")
            .bind(server_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(export_count, 0);

    sqlx::query(
        "INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete)
         VALUES($1, $2, 1, 'export-test-manifest', 100, 'manifest-checksum', 'manual', true)",
    )
    .bind(Uuid::new_v4())
    .bind(server_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE servers SET current_snapshot_generation = 1 WHERE id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();

    let created = app
        .clone()
        .oneshot(request(Method::POST, &endpoint, Some(&token), None))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::ACCEPTED);
    let created = json_body(created).await;
    let export_id = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();
    assert_eq!(created["source_generation"], 1);
    assert_eq!(created["status"], "pending");

    let foreign = app
        .clone()
        .oneshot(request(
            Method::GET,
            &format!("{endpoint}/{export_id}"),
            Some(&other_token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND);

    let cancelled = app
        .clone()
        .oneshot(request(
            Method::DELETE,
            &format!("{endpoint}/{export_id}"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(json_body(cancelled).await["status"], "cancelled");

    let completed_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO world_exports(
            id, server_id, requested_by, status, stage, source_generation
         ) VALUES($1, $2, $3, 'pending', 'ready', 1)",
    )
    .bind(completed_id)
    .bind(server_id)
    .bind(owner_id)
    .execute(&pool)
    .await
    .unwrap();
    let key = format!("exports/{completed_id}/1.zip");
    let archive = Bytes::from_static(b"zip64 archive test fixture");
    object_store
        .put(
            &physical_object_path(server_id, &key).unwrap(),
            archive.clone().into(),
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE world_exports SET status = 'completed', stage = 'completed',
             archive_key = $2, archive_size = $3, archive_checksum = $4,
             expires_at = now() + interval '24 hours'
         WHERE id = $1",
    )
    .bind(completed_id)
    .bind(&key)
    .bind(archive.len() as i64)
    .bind(blake3::hash(&archive).to_hex().to_string())
    .execute(&pool)
    .await
    .unwrap();

    let download = app
        .clone()
        .oneshot(request(
            Method::GET,
            &format!("{endpoint}/{completed_id}/download"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(download.status(), StatusCode::OK);
    assert_eq!(download.headers()[header::CONTENT_TYPE], "application/zip");
    assert_eq!(to_bytes(download.into_body(), 1024).await.unwrap(), archive);

    let foreign_download = app
        .clone()
        .oneshot(request(
            Method::GET,
            &format!("{endpoint}/{completed_id}/download"),
            Some(&other_token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(foreign_download.status(), StatusCode::NOT_FOUND);

    sqlx::query("UPDATE world_exports SET expires_at = now() - interval '1 second' WHERE id = $1")
        .bind(completed_id)
        .execute(&pool)
        .await
        .unwrap();
    let expired = app
        .oneshot(request(
            Method::GET,
            &format!("{endpoint}/{completed_id}/download"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(expired.status(), StatusCode::CONFLICT);

    sqlx::query("DELETE FROM users WHERE id IN (SELECT user_id FROM identities WHERE subject LIKE 'api-v1-test-%')")
        .execute(&pool)
        .await
        .unwrap();
}

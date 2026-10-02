use super::support::*;

#[tokio::test]
async fn every_server_route_requires_ownership_and_legacy_singular_routes_are_gone() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_id, _) = user_session(&pool, "active").await;
    let (foreign_owner_id, foreign_token) = user_session(&pool, "active").await;
    let server_id = Uuid::new_v4();
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1,$2,$3)")
        .bind(server_id)
        .bind(owner_id)
        .bind(format!(
            "isolation-{}",
            &server_id.simple().to_string()[..12]
        ))
        .execute(&pool)
        .await
        .unwrap();

    let export_id = Uuid::new_v4();
    let player_id = Uuid::new_v4();
    let prefix = format!("/api/v1/servers/{server_id}");
    let app = router(state(pool.clone()));
    let protected_routes = [
        (Method::GET, prefix.clone(), None),
        (
            Method::PATCH,
            prefix.clone(),
            Some(json!({"settings":{"motd":"foreign"}})),
        ),
        (
            Method::POST,
            format!("{prefix}/commands"),
            Some(json!({"command":"start"})),
        ),
        (
            Method::POST,
            format!("{prefix}/commands"),
            Some(json!({"command":"reset", "mode":"world"})),
        ),
        (
            Method::POST,
            format!("{prefix}/commands"),
            Some(json!({"command":"delete"})),
        ),
        (Method::GET, format!("{prefix}/events"), None),
        (Method::GET, format!("{prefix}/logs"), None),
        (Method::GET, format!("{prefix}/activity"), None),
        (Method::GET, format!("{prefix}/snapshots"), None),
        (
            Method::POST,
            format!("{prefix}/commands"),
            Some(json!({"command":"restore", "generation":1})),
        ),
        (Method::GET, format!("{prefix}/exports"), None),
        (Method::POST, format!("{prefix}/exports"), None),
        (Method::GET, format!("{prefix}/exports/{export_id}"), None),
        (
            Method::DELETE,
            format!("{prefix}/exports/{export_id}"),
            None,
        ),
        (
            Method::GET,
            format!("{prefix}/exports/{export_id}/download"),
            None,
        ),
        (Method::GET, format!("{prefix}/players"), None),
        (Method::GET, format!("{prefix}/whitelist"), None),
        (
            Method::POST,
            format!("{prefix}/whitelist"),
            Some(json!({"player_id":player_id,"name":"Foreign"})),
        ),
        (
            Method::DELETE,
            format!("{prefix}/whitelist?player_id={player_id}"),
            None,
        ),
    ];

    for (method, uri, body) in protected_routes {
        let response = app
            .clone()
            .oneshot(request(method, &uri, Some(&foreign_token), body))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "foreign account must not access {uri}"
        );
    }

    for (method, uri) in [
        (Method::GET, "/api/v1/server"),
        (Method::POST, "/api/v1/server"),
        (Method::GET, "/api/v1/server/actions"),
        (Method::GET, "/api/v1/server/events"),
        (Method::GET, "/api/v1/server/logs"),
        (Method::GET, "/api/v1/server/activity"),
        (Method::GET, "/api/v1/server/snapshots"),
        (Method::POST, "/api/v1/server/snapshots/1/restore"),
        (Method::GET, "/api/v1/server/players"),
        (Method::GET, "/api/v1/server/whitelist"),
    ] {
        let response = app
            .clone()
            .oneshot(request(method, uri, Some(&foreign_token), None))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "legacy route {uri}"
        );
    }

    for (uri, body) in [
        (format!("{prefix}/actions"), json!({"action":"start"})),
        (format!("{prefix}/reset"), json!({"mode":"world"})),
        (format!("{prefix}/snapshots/1/restore"), json!({})),
    ] {
        let response = app
            .clone()
            .oneshot(request(
                Method::POST,
                &uri,
                Some(&foreign_token),
                Some(body),
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "legacy lifecycle route {uri}"
        );
    }
    let delete_server = app
        .clone()
        .oneshot(request(Method::DELETE, &prefix, Some(&foreign_token), None))
        .await
        .unwrap();
    assert_eq!(delete_server.status(), StatusCode::METHOD_NOT_ALLOWED);
    let old_wake = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/internal/wake/removed.example.test")
                .header(header::AUTHORIZATION, "Bearer api-v1-test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(old_wake.status(), StatusCode::NOT_FOUND);

    let (desired_state, whitelist_count, export_count): (String, i64, i64) = sqlx::query_as(
        "SELECT s.desired_state::text,
                (SELECT count(*) FROM whitelist_entries WHERE server_id = s.id),
                (SELECT count(*) FROM world_exports WHERE server_id = s.id)
         FROM servers s WHERE s.id = $1",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(desired_state, "stopped");
    assert_eq!(whitelist_count, 0);
    assert_eq!(export_count, 0);

    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind([owner_id, foreign_owner_id].as_slice())
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn gateway_commands_are_restricted_to_idempotent_join_starts() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_id, owner_token) = user_session(&pool, "active").await;
    let server_id = Uuid::new_v4();
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)")
        .bind(server_id)
        .bind(owner_id)
        .bind(format!("gateway-{}", &server_id.simple().to_string()[..12]))
        .execute(&pool)
        .await
        .unwrap();
    let app = router(state(pool.clone()));
    let uri = format!("/api/v1/servers/{server_id}/commands");
    let gateway_request = |body: Value| {
        Request::builder()
            .method(Method::POST)
            .uri(&uri)
            .header(header::AUTHORIZATION, "Bearer api-v1-test-token")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };

    let accepted = app
        .clone()
        .oneshot(gateway_request(
            json!({"command":"start", "reason":"gateway_join"}),
        ))
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let accepted_state = json_body(accepted).await["observed_state"].clone();
    assert!(matches!(
        accepted_state.as_str(),
        Some("queued" | "restoring")
    ));

    let repeated = app
        .clone()
        .oneshot(gateway_request(
            json!({"command":"start", "reason":"gateway_join"}),
        ))
        .await
        .unwrap();
    assert_eq!(repeated.status(), StatusCode::ACCEPTED);
    let (start_request_count, active_start_count): (i64, i64) = sqlx::query_as(
        "SELECT
            (SELECT count(*) FROM start_requests WHERE server_id = $1),
            (SELECT count(*) FROM operations WHERE server_id = $1 AND kind = 'start'
             AND status IN ('pending', 'running'))",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(start_request_count + active_start_count, 1);

    for body in [
        json!({"command":"start", "reason":"manual"}),
        json!({"command":"stop"}),
        json!({"command":"reset", "mode":"world"}),
        json!({"command":"delete"}),
    ] {
        let rejected = app.clone().oneshot(gateway_request(body)).await.unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    }
    let destructive_operations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations
         WHERE server_id = $1 AND kind IN ('reset', 'delete')",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(destructive_operations, 0);

    let owner_gateway_reason = app
        .clone()
        .oneshot(request(
            Method::POST,
            &uri,
            Some(&owner_token),
            Some(json!({"command":"start", "reason":"gateway_join"})),
        ))
        .await
        .unwrap();
    assert_eq!(
        owner_gateway_reason.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(owner_id)
        .execute(&pool)
        .await
        .unwrap();
}

use super::support::*;

#[tokio::test]
async fn persistent_scoped_grants_join_account_servers_and_revoke_rows_independently() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (account_owner_id, account_token) = user_session(&pool, "active").await;
    let scoped_owner_one = create_user(
        &pool,
        "google",
        &format!("scoped-one-{}", Uuid::new_v4()),
        "Scoped One",
        "active",
        None,
    )
    .await;
    let scoped_owner_two = create_user(
        &pool,
        "google",
        &format!("scoped-two-{}", Uuid::new_v4()),
        "Scoped Two",
        "active",
        None,
    )
    .await;
    let scoped_owner_three = create_user(
        &pool,
        "google",
        &format!("scoped-three-{}", Uuid::new_v4()),
        "Scoped Three",
        "active",
        None,
    )
    .await;
    let account_server = create_test_server(&pool, account_owner_id, "scope-account").await;
    let scoped_server_one = create_test_server(&pool, scoped_owner_one, "scope-one").await;
    let scoped_server_two = create_test_server(&pool, scoped_owner_two, "scope-two").await;
    let scoped_server_three = create_test_server(&pool, scoped_owner_three, "scope-three").await;
    let foreign_server = create_test_server(&pool, scoped_owner_three, "scope-foreign").await;
    sqlx::query("UPDATE servers SET is_default = true WHERE id = $1")
        .bind(account_server)
        .execute(&pool)
        .await
        .unwrap();

    let grant_id = Uuid::new_v4();
    let grant_token = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    sqlx::query(
        "INSERT INTO server_session_grants(id, token_hash, created_at)
         VALUES($1, $2, now() - interval '10 years')",
    )
    .bind(grant_id)
    .bind(blake3::hash(grant_token.as_bytes()).as_bytes().to_vec())
    .execute(&pool)
    .await
    .unwrap();
    let session_one = Uuid::new_v4();
    let session_two = Uuid::new_v4();
    let session_three = Uuid::new_v4();
    for (session_id, server_id) in [
        (session_one, scoped_server_one),
        (session_two, scoped_server_two),
        (session_three, scoped_server_three),
    ] {
        sqlx::query(
            "INSERT INTO server_sessions(id, grant_id, server_id, created_at)
             VALUES($1, $2, $3, now() - interval '10 years')",
        )
        .bind(session_id)
        .bind(grant_id)
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();
    }

    let app = router(state(pool.clone()));
    let combined_cookies =
        format!("kurbis_session={account_token}; kurbis_server_session={grant_token}");
    let accessible = app
        .clone()
        .oneshot(request_with_cookies(
            Method::GET,
            "/api/v1/servers",
            &combined_cookies,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(accessible.status(), StatusCode::OK);
    let accessible = json_body(accessible).await;
    let by_id = |id: Uuid| {
        accessible
            .as_array()
            .unwrap()
            .iter()
            .find(|server| server["id"] == id.to_string())
            .cloned()
            .unwrap()
    };
    assert_eq!(accessible.as_array().unwrap().len(), 4);
    assert_eq!(by_id(account_server)["is_default"], true);
    assert_eq!(by_id(scoped_server_one)["is_default"], false);
    assert_eq!(by_id(scoped_server_two)["is_default"], false);
    assert_eq!(by_id(scoped_server_three)["is_default"], false);

    let scoped_only = app
        .clone()
        .oneshot(request_with_cookies(
            Method::GET,
            "/api/v1/servers",
            &format!("kurbis_server_session={grant_token}"),
            None,
        ))
        .await
        .unwrap();
    let scoped_only = json_body(scoped_only).await;
    assert_eq!(scoped_only.as_array().unwrap().len(), 3);
    assert!(
        scoped_only
            .as_array()
            .unwrap()
            .iter()
            .all(|server| server["is_default"] == false)
    );

    let stale_account_cookie = app
        .clone()
        .oneshot(request_with_cookies(
            Method::GET,
            &format!("/api/v1/servers/{scoped_server_two}"),
            &format!("kurbis_session=stale; kurbis_server_session={grant_token}"),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(stale_account_cookie.status(), StatusCode::OK);

    let worker_id = Uuid::new_v4();
    let worker = healthy_worker_template(
        worker_id,
        format!("scoped-grant-worker-{worker_id}"),
        "127.0.0.1".to_owned(),
        8 * 1024 * 1024 * 1024,
        40 * 1024 * 1024 * 1024,
        4000,
        35000,
        35100,
    );
    heartbeat_worker(&pool, &worker).await.unwrap();

    let issued = app
        .clone()
        .oneshot(request_with_cookies(
            Method::POST,
            &format!("/api/v1/servers/{scoped_server_one}/commands"),
            &combined_cookies,
            Some(json!({"command":"start"})),
        ))
        .await
        .unwrap();
    assert_eq!(issued.status(), StatusCode::ACCEPTED);

    let reset = app
        .clone()
        .oneshot(request_with_cookies(
            Method::POST,
            &format!("/api/v1/servers/{scoped_server_two}/commands"),
            &combined_cookies,
            Some(json!({"command":"reset", "mode":"world"})),
        ))
        .await
        .unwrap();
    assert_eq!(reset.status(), StatusCode::ACCEPTED);
    assert_eq!(json_body(reset).await["observed_state"], "resetting");

    let delete = app
        .clone()
        .oneshot(request_with_cookies(
            Method::POST,
            &format!("/api/v1/servers/{scoped_server_three}/commands"),
            &combined_cookies,
            Some(json!({"command":"delete"})),
        ))
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::ACCEPTED);

    let cross_server_delete = app
        .clone()
        .oneshot(request_with_cookies(
            Method::POST,
            &format!("/api/v1/servers/{foreign_server}/commands"),
            &combined_cookies,
            Some(json!({"command":"delete"})),
        ))
        .await
        .unwrap();
    assert_eq!(cross_server_delete.status(), StatusCode::NOT_FOUND);
    let cross_server_reset = app
        .clone()
        .oneshot(request_with_cookies(
            Method::POST,
            &format!("/api/v1/servers/{foreign_server}/commands"),
            &combined_cookies,
            Some(json!({"command":"reset", "mode":"full"})),
        ))
        .await
        .unwrap();
    assert_eq!(cross_server_reset.status(), StatusCode::NOT_FOUND);

    let grant_rows = app
        .clone()
        .oneshot(request_with_cookies(
            Method::GET,
            "/api/v1/server/sessions",
            &combined_cookies,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(json_body(grant_rows).await.as_array().unwrap().len(), 2);

    sqlx::query("UPDATE users SET account_status = 'suspended' WHERE id = $1")
        .bind(scoped_owner_two)
        .execute(&pool)
        .await
        .unwrap();
    let suspended_scope = app
        .clone()
        .oneshot(request_with_cookies(
            Method::GET,
            &format!("/api/v1/servers/{scoped_server_two}"),
            &combined_cookies,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(suspended_scope.status(), StatusCode::NOT_FOUND);

    let revoked = app
        .clone()
        .oneshot(request_with_cookies(
            Method::DELETE,
            &format!("/api/v1/server/sessions/{session_one}"),
            &combined_cookies,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    assert!(
        !revoked
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .any(|value| value
                .to_str()
                .unwrap_or("")
                .contains("kurbis_server_session"))
    );
    let hidden_scopes = app
        .clone()
        .oneshot(request_with_cookies(
            Method::GET,
            "/api/v1/server/sessions",
            &combined_cookies,
            None,
        ))
        .await
        .unwrap();
    assert!(
        json_body(hidden_scopes)
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    sqlx::query("UPDATE users SET account_status = 'active' WHERE id = $1")
        .bind(scoped_owner_two)
        .execute(&pool)
        .await
        .unwrap();
    let still_authorized = app
        .clone()
        .oneshot(request_with_cookies(
            Method::GET,
            &format!("/api/v1/servers/{scoped_server_two}"),
            &combined_cookies,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(still_authorized.status(), StatusCode::OK);
    let retained_scopes = app
        .clone()
        .oneshot(request_with_cookies(
            Method::GET,
            "/api/v1/server/sessions",
            &combined_cookies,
            None,
        ))
        .await
        .unwrap();
    let retained_scopes = json_body(retained_scopes).await;
    assert_eq!(retained_scopes.as_array().unwrap().len(), 1);
    assert_eq!(retained_scopes[0]["id"], session_two.to_string());

    let revoked_last = app
        .clone()
        .oneshot(request_with_cookies(
            Method::DELETE,
            &format!("/api/v1/server/sessions/{session_two}"),
            &combined_cookies,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(revoked_last.status(), StatusCode::NO_CONTENT);
    assert!(
        revoked_last
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("kurbis_server_session=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0")
    );
    let no_scoped_servers = app
        .clone()
        .oneshot(request_with_cookies(
            Method::GET,
            "/api/v1/servers",
            &combined_cookies,
            None,
        ))
        .await
        .unwrap();
    let no_scoped_servers = json_body(no_scoped_servers).await;
    assert_eq!(no_scoped_servers.as_array().unwrap().len(), 1);
    assert_eq!(no_scoped_servers[0]["id"], account_server.to_string());

    let revoked_server = app
        .clone()
        .oneshot(request_with_cookies(
            Method::GET,
            &format!("/api/v1/servers/{scoped_server_one}"),
            &combined_cookies,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(revoked_server.status(), StatusCode::NOT_FOUND);
    let revoked_server_two = app
        .clone()
        .oneshot(request_with_cookies(
            Method::GET,
            &format!("/api/v1/servers/{scoped_server_two}"),
            &combined_cookies,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(revoked_server_two.status(), StatusCode::NOT_FOUND);
    let deleted_server = app
        .clone()
        .oneshot(request_with_cookies(
            Method::GET,
            &format!("/api/v1/servers/{scoped_server_three}"),
            &combined_cookies,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(deleted_server.status(), StatusCode::NOT_FOUND);
    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(
            [
                account_owner_id,
                scoped_owner_one,
                scoped_owner_two,
                scoped_owner_three,
            ]
            .as_slice(),
        )
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
async fn current_placement_capability_and_redeemed_session_share_server_scope() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_id, owner_token) = user_session(&pool, "active").await;
    let (other_owner_id, _) = user_session(&pool, "active").await;
    let app = router(state(pool.clone()));

    let server_id = create_test_server(&pool, owner_id, "scope").await;
    let other_server_id = create_test_server(&pool, other_owner_id, "other").await;

    let mut worker_id = Uuid::new_v4();
    let original_worker_id = worker_id;
    let worker = healthy_worker_template(
        worker_id,
        format!("scoped-api-worker-{worker_id}"),
        "127.0.0.1".to_owned(),
        8 * 1024 * 1024 * 1024,
        40 * 1024 * 1024 * 1024,
        4000,
        24000,
        24100,
    );
    heartbeat_worker(&pool, &worker).await.unwrap();
    let worker_api_token = register_worker_api_token(&pool, worker_id).await;
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

    let shared_token_response = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            "/internal/worker/server-capabilities",
            "api-v1-test-token",
            Some(json!({"server_id":server_id,"generation":1})),
        ))
        .await
        .unwrap();
    assert_eq!(shared_token_response.status(), StatusCode::UNAUTHORIZED);
    let spoofed_worker_response = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            "/internal/worker/server-capabilities",
            &worker_api_token,
            Some(json!({
                "server_id": server_id,
                "worker_identity": Uuid::new_v4(),
                "generation": 1
            })),
        ))
        .await
        .unwrap();
    assert!(spoofed_worker_response.status().is_client_error());

    let capability_response = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            "/internal/worker/server-capabilities",
            &worker_api_token,
            Some(json!({
                "server_id": server_id,
                "generation": 1
            })),
        ))
        .await
        .unwrap();
    assert_eq!(capability_response.status(), StatusCode::OK);
    assert_eq!(
        capability_response
            .headers()
            .get(header::CACHE_CONTROL)
            .unwrap(),
        "no-store"
    );
    let capability = json_body(capability_response).await["capability"]
        .as_str()
        .unwrap()
        .to_owned();

    let scoped_paths = [
        format!("/api/v1/servers/{server_id}"),
        format!("/api/v1/servers/{server_id}/players"),
        format!("/api/v1/servers/{server_id}/logs"),
        format!("/api/v1/servers/{server_id}/activity"),
        format!("/api/v1/servers/{server_id}/snapshots"),
        format!("/api/v1/servers/{server_id}/whitelist"),
    ];
    for path in scoped_paths {
        let response = app
            .clone()
            .oneshot(bearer_request(Method::GET, &path, &capability, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }
    let events = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}/events"),
            &capability,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(events.status(), StatusCode::OK);
    let first_event = tokio::time::timeout(
        Duration::from_secs(2),
        events.into_body().into_data_stream().next(),
    )
    .await
    .expect("server event stream should deliver the durable catch-up event")
    .expect("server event stream should remain open")
    .unwrap();
    assert!(String::from_utf8_lossy(&first_event).contains("event: server"));

    let patched = app
        .clone()
        .oneshot(bearer_request(
            Method::PATCH,
            &format!("/api/v1/servers/{server_id}"),
            &capability,
            Some(json!({"settings":{"motd":"Capability updated"}})),
        ))
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);

    for command in [
        json!({"command":"reset", "mode":"world"}),
        json!({"command":"delete"}),
    ] {
        let destructive = app
            .clone()
            .oneshot(bearer_request(
                Method::POST,
                &format!("/api/v1/servers/{server_id}/commands"),
                &capability,
                Some(command),
            ))
            .await
            .unwrap();
        assert_eq!(destructive.status(), StatusCode::FORBIDDEN);
    }

    let action = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            &format!("/api/v1/servers/{server_id}/commands"),
            &capability,
            Some(json!({"command":"snapshot"})),
        ))
        .await
        .unwrap();
    assert_eq!(action.status(), StatusCode::CONFLICT);

    sqlx::query(
        "INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes,
                               checksum, reason, complete)
         VALUES($1, $2, 1, 'scoped-restore', 10, 'scoped-checksum', 'manual', true)",
    )
    .bind(Uuid::new_v4())
    .bind(server_id)
    .execute(&pool)
    .await
    .unwrap();
    let restored = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            &format!("/api/v1/servers/{server_id}/commands"),
            &capability,
            Some(json!({"command":"restore", "generation":1})),
        ))
        .await
        .unwrap();
    // The route accepts the capability, then the lifecycle rejects the restore
    // because a live fenced placement must stop before snapshot recovery.
    assert_eq!(restored.status(), StatusCode::CONFLICT);

    let account_route = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            "/api/v1/servers",
            &capability,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(account_route.status(), StatusCode::UNAUTHORIZED);
    let cross_server = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            &format!("/api/v1/servers/{other_server_id}/players"),
            &capability,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(cross_server.status(), StatusCode::NOT_FOUND);
    let cross_server_operators = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            &format!("/api/v1/servers/{other_server_id}/operators"),
            &capability,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(cross_server_operators.status(), StatusCode::NOT_FOUND);
    let capability_operators = app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}/operators"),
            &capability,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(capability_operators.status(), StatusCode::NOT_FOUND);
    // The handler does not extract a body: a forged identifier may be sent but
    // cannot override scope, which must remain derived from the bearer token.
    let minted = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            "/api/v1/server/login-links",
            &capability,
            Some(json!({"server_id": other_server_id})),
        ))
        .await
        .unwrap();
    assert_eq!(minted.status(), StatusCode::OK);
    assert_eq!(
        minted.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let login_url = json_body(minted).await["url"].as_str().unwrap().to_owned();
    let login_token = login_url
        .split_once("#token=")
        .expect("login link includes its one-time fragment")
        .1
        .to_owned();

    let expiring_link = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            "/api/v1/server/login-links",
            &capability,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(expiring_link.status(), StatusCode::OK);
    let expiring_token = json_body(expiring_link).await["url"]
        .as_str()
        .unwrap()
        .split_once("#token=")
        .unwrap()
        .1
        .to_owned();
    sqlx::query(
        "UPDATE server_login_tokens SET expires_at = now() - interval '1 second'
         WHERE token_hash = $1",
    )
    .bind(blake3::hash(expiring_token.as_bytes()).as_bytes().to_vec())
    .execute(&pool)
    .await
    .unwrap();
    let expired_redemption = Request::builder()
        .method(Method::POST)
        .uri("/auth/server/redeem")
        .header(header::ORIGIN, "https://panel.example.test")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"token":&expiring_token}).to_string()))
        .unwrap();
    assert_eq!(
        app.clone()
            .oneshot(expired_redemption)
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    for attempt in 3..=6 {
        let response = app
            .clone()
            .oneshot(bearer_request(
                Method::POST,
                "/api/v1/server/login-links",
                &capability,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if attempt == 6 {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::OK
            },
            "login link mint attempt {attempt}"
        );
    }

    let cross_origin = Request::builder()
        .method(Method::POST)
        .uri("/auth/server/redeem")
        .header(header::ORIGIN, "https://attacker.example")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"token":&login_token}).to_string()))
        .unwrap();
    let rejected = app.clone().oneshot(cross_origin).await.unwrap();
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);

    let redemption = Request::builder()
        .method(Method::POST)
        .uri("/auth/server/redeem")
        .header(header::ORIGIN, "https://panel.example.test")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"token":&login_token}).to_string()))
        .unwrap();
    let redeemed = app.clone().oneshot(redemption).await.unwrap();
    assert_eq!(redeemed.status(), StatusCode::OK);
    assert_eq!(
        redeemed.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let server_session = redeemed
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("kurbis_server_session=")
        .unwrap()
        .to_owned();
    assert_eq!(
        json_body(redeemed).await["server_id"],
        server_id.to_string()
    );

    let session_access = app
        .clone()
        .oneshot(server_session_request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}/players"),
            &server_session,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(session_access.status(), StatusCode::OK);
    let session_operator_access = app
        .clone()
        .oneshot(server_session_request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}/operators"),
            &server_session,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(session_operator_access.status(), StatusCode::OK);
    let session_cross_server = app
        .clone()
        .oneshot(server_session_request(
            Method::GET,
            &format!("/api/v1/servers/{other_server_id}"),
            &server_session,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(session_cross_server.status(), StatusCode::NOT_FOUND);
    let session_account_route = app
        .clone()
        .oneshot(server_session_request(
            Method::GET,
            "/api/v1/servers",
            &server_session,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(session_account_route.status(), StatusCode::OK);
    let scoped_servers = json_body(session_account_route).await;
    assert_eq!(scoped_servers.as_array().unwrap().len(), 1);
    assert_eq!(scoped_servers[0]["id"], server_id.to_string());

    sqlx::query(
        "DELETE FROM server_sessions ss USING server_session_grants g
         WHERE ss.grant_id = g.id AND g.token_hash = $1 AND ss.server_id = $2",
    )
    .bind(blake3::hash(server_session.as_bytes()).as_bytes().to_vec())
    .bind(server_id)
    .execute(&pool)
    .await
    .unwrap();
    let expired_session = app
        .clone()
        .oneshot(server_session_request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}/players"),
            &server_session,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(expired_session.status(), StatusCode::UNAUTHORIZED);

    let replay = Request::builder()
        .method(Method::POST)
        .uri("/auth/server/redeem")
        .header(header::ORIGIN, "https://panel.example.test")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"token":&login_token}).to_string()))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(replay).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );

    let relocated_worker_id = Uuid::new_v4();
    let relocated_worker = healthy_worker_template(
        relocated_worker_id,
        format!("scoped-api-relocated-worker-{relocated_worker_id}"),
        "127.0.0.1".to_owned(),
        8 * 1024 * 1024 * 1024,
        40 * 1024 * 1024 * 1024,
        4000,
        40000,
        40100,
    );
    heartbeat_worker(&pool, &relocated_worker).await.unwrap();
    let relocated_worker_api_token = register_worker_api_token(&pool, relocated_worker_id).await;
    sqlx::query("UPDATE placements SET worker_id = $2 WHERE server_id = $1")
        .bind(server_id)
        .bind(relocated_worker_id)
        .execute(&pool)
        .await
        .unwrap();

    let stale_link_after_relocation = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            "/api/v1/server/login-links",
            &capability,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        stale_link_after_relocation.status(),
        StatusCode::UNAUTHORIZED
    );

    let relocated_response = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            "/internal/worker/server-capabilities",
            &relocated_worker_api_token,
            Some(json!({
                "server_id": server_id,
                "generation": 1,
                "current_capability": capability
            })),
        ))
        .await
        .unwrap();
    assert_eq!(relocated_response.status(), StatusCode::OK);
    let relocated_capability = json_body(relocated_response).await["capability"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(relocated_capability, capability);
    worker_id = relocated_worker_id;

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
    let rotated_response = router(state(pool.clone()))
        .oneshot(bearer_request(
            Method::POST,
            "/internal/worker/server-capabilities",
            &relocated_worker_api_token,
            Some(json!({
                "server_id": server_id,
                "generation": 2,
                "current_capability": relocated_capability
            })),
        ))
        .await
        .unwrap();
    assert_eq!(rotated_response.status(), StatusCode::OK);
    let rotated_capability = json_body(rotated_response).await["capability"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(rotated_capability, capability);

    let stale_capability = router(state(pool.clone()))
        .oneshot(bearer_request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}"),
            &capability,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(stale_capability.status(), StatusCode::UNAUTHORIZED);
    let current_capability = router(state(pool.clone()))
        .oneshot(bearer_request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}"),
            &rotated_capability,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(current_capability.status(), StatusCode::OK);

    sqlx::query("UPDATE placements SET active = false WHERE server_id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();
    let inactive_capability = router(state(pool.clone()))
        .oneshot(bearer_request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}"),
            &rotated_capability,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(inactive_capability.status(), StatusCode::UNAUTHORIZED);
    let owner_access = router(state(pool.clone()))
        .oneshot(request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}"),
            Some(&owner_token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(owner_access.status(), StatusCode::OK);

    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(vec![owner_id, other_owner_id])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workers WHERE id = ANY($1)")
        .bind(vec![original_worker_id, worker_id])
        .execute(&pool)
        .await
        .unwrap();
}

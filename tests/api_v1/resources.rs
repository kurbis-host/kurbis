use super::support::*;

#[tokio::test]
async fn versioned_api_covers_panel_resources_and_lifecycle_boundaries() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_id, token) = user_session(&pool, "active").await;
    let (_, other_token) = user_session(&pool, "active").await;
    let worker_id = Uuid::new_v4();
    let worker = healthy_worker_template(
        worker_id,
        format!("api-v1-worker-{worker_id}"),
        "127.0.0.1".to_owned(),
        8 * 1024 * 1024 * 1024,
        40 * 1024 * 1024 * 1024,
        4000,
        34000,
        34100,
    );
    heartbeat_worker(&pool, &worker).await.unwrap();
    let app = router(state(pool.clone()));

    let anonymous = app
        .clone()
        .oneshot(request(Method::GET, "/api/v1/session", None, None))
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

    let session = app
        .clone()
        .oneshot(request(Method::GET, "/api/v1/session", Some(&token), None))
        .await
        .unwrap();
    assert_eq!(session.status(), StatusCode::OK);
    let session = json_body(session).await;
    assert_eq!(session["user"]["account_status"], "active");
    assert_eq!(
        session["limits"]["memory_bytes"],
        2_i64 * 1024 * 1024 * 1024
    );
    assert_eq!(
        session["limits"]["storage_bytes"],
        5_i64 * 1024 * 1024 * 1024
    );
    assert_eq!(session["limits"]["idle_seconds"], 300);
    assert_eq!(session["root_domain"], "mc.example.test");

    let empty = app
        .clone()
        .oneshot(request(Method::GET, "/api/v1/servers", Some(&token), None))
        .await
        .unwrap();
    assert_eq!(json_body(empty).await, json!([]));

    let subdomain = format!("api-{}", &owner_id.simple().to_string()[..12]);
    let created = app
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/servers",
            Some(&token),
            Some(json!({
                "subdomain": subdomain,
                "settings": {
                    "motd": "API integration",
                    "difficulty": "Normal",
                    "gamemode": "Survival",
                    "view_distance": 12,
                    "simulation_distance": 8,
                    "hardcore": false,
                    "whitelist": true
                }
            })),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    let server_id = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();
    assert_eq!(created["observed_state"], "sleeping");
    assert_eq!(created["is_default"], true);
    assert_eq!(
        created["java_address"],
        format!("{subdomain}.mc.example.test")
    );
    assert_eq!(created["storage_limit_bytes"], 5_i64 * 1024 * 1024 * 1024);

    let owner_profile_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO identities(id, user_id, provider, subject, display_name, minecraft_player_id)
         VALUES($1, $2, 'minecraft_java', $3, 'WorldOwner', $4)",
    )
    .bind(Uuid::new_v4())
    .bind(owner_id)
    .bind(owner_profile_id.to_string())
    .bind(owner_profile_id)
    .execute(&pool)
    .await
    .unwrap();

    let isolated = app
        .clone()
        .oneshot(request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}"),
            Some(&other_token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(isolated.status(), StatusCode::NOT_FOUND);

    let patched = app
        .clone()
        .oneshot(request(
            Method::PATCH,
            &format!("/api/v1/servers/{server_id}"),
            Some(&token),
            Some(json!({"settings":{"motd":"Changed through v1"}})),
        ))
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    assert_eq!(
        json_body(patched).await["settings"]["motd"],
        "Changed through v1"
    );

    sqlx::query(
        "UPDATE servers SET observed_state = 'online', desired_state = 'running' WHERE id = $1",
    )
    .bind(server_id)
    .execute(&pool)
    .await
    .unwrap();
    let settings_uri = format!("/api/v1/servers/{server_id}");
    let online_settings = app
        .clone()
        .oneshot(request(
            Method::PATCH,
            &settings_uri,
            Some(&token),
            Some(json!({"settings":{"difficulty":"Hard"}})),
        ))
        .await
        .unwrap();
    assert_eq!(online_settings.status(), StatusCode::OK);
    assert_eq!(
        json_body(online_settings).await["settings"]["difficulty"],
        "Hard"
    );

    let online_subdomain = app
        .clone()
        .oneshot(request(
            Method::PATCH,
            &settings_uri,
            Some(&token),
            Some(json!({"subdomain":"new-address"})),
        ))
        .await
        .unwrap();
    assert_eq!(online_subdomain.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(online_subdomain).await["code"],
        "server_not_sleeping"
    );
    sqlx::query(
        "UPDATE servers SET observed_state = 'sleeping', desired_state = 'stopped' WHERE id = $1",
    )
    .bind(server_id)
    .execute(&pool)
    .await
    .unwrap();
    let sleeping_settings = app
        .clone()
        .oneshot(request(
            Method::PATCH,
            &settings_uri,
            Some(&token),
            Some(json!({"settings":{"difficulty":"Hard"}})),
        ))
        .await
        .unwrap();
    assert_eq!(sleeping_settings.status(), StatusCode::OK);
    assert_eq!(
        json_body(sleeping_settings).await["settings"]["difficulty"],
        "Hard"
    );
    let invalid_settings = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::PATCH)
                .uri(&settings_uri)
                .header(header::COOKIE, format!("kurbis_session={token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"settings":{"difficulty":"Impossible"}}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid_settings.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json_body(invalid_settings).await["code"], "invalid_request");

    let malformed_json = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::PATCH)
                .uri(&settings_uri)
                .header(header::COOKIE, format!("kurbis_session={token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"settings":{"difficulty":"Normal"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(malformed_json.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_body(malformed_json).await["code"], "invalid_json");

    let missing_content_type = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::PATCH)
                .uri(&settings_uri)
                .header(header::COOKIE, format!("kurbis_session={token}"))
                .body(Body::from(
                    r#"{"settings":{"motd":"missing content type"}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        missing_content_type.status(),
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    assert_eq!(
        json_body(missing_content_type).await["code"],
        "json_content_type_required"
    );

    let invalid_snapshot = app
        .clone()
        .oneshot(request(
            Method::POST,
            &format!("/api/v1/servers/{server_id}/commands"),
            Some(&token),
            Some(json!({"command":"snapshot"})),
        ))
        .await
        .unwrap();
    assert_eq!(invalid_snapshot.status(), StatusCode::CONFLICT);

    let mut log_ids = Vec::new();
    for line in ["first", "second", "third"] {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO server_logs(server_id, line) VALUES($1,$2) RETURNING id",
        )
        .bind(server_id)
        .bind(line)
        .fetch_one(&pool)
        .await
        .unwrap();
        log_ids.push(id);
    }
    let logs = app
        .clone()
        .oneshot(request(
            Method::GET,
            &format!(
                "/api/v1/servers/{server_id}/logs?after_id={}&limit=1",
                log_ids[0]
            ),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    let logs = json_body(logs).await;
    assert_eq!(logs["items"].as_array().unwrap().len(), 1);
    assert_eq!(logs["items"][0]["line"], "second");
    assert_eq!(logs["next_after_id"], log_ids[1]);

    let player_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO server_players(server_id, player_id, name, edition)
         VALUES($1,$2,'Alex','bedrock')",
    )
    .bind(server_id)
    .bind(player_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO server_known_players(server_id, player_id, name, edition)
         VALUES($1, $2, 'Alex', 'bedrock')",
    )
    .bind(server_id)
    .bind(player_id)
    .execute(&pool)
    .await
    .unwrap();
    let players = app
        .clone()
        .oneshot(request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}/players"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    let players = json_body(players).await;
    assert_eq!(players[0]["player_id"], player_id.to_string());
    assert_eq!(players[0]["edition"], "bedrock");

    let whitelist = app
        .clone()
        .oneshot(request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}/whitelist"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    let whitelist = json_body(whitelist).await;
    assert_eq!(whitelist["enabled"], true);
    assert_eq!(whitelist["revision"], 1);
    assert_eq!(whitelist["applied_revision"], 0);
    assert_eq!(whitelist["synchronized"], false);

    let added = app
        .clone()
        .oneshot(request(
            Method::POST,
            &format!("/api/v1/servers/{server_id}/whitelist"),
            Some(&token),
            Some(json!({"name":"Alex"})),
        ))
        .await
        .unwrap();
    assert_eq!(added.status(), StatusCode::CREATED);
    let added = json_body(added).await;
    assert_eq!(added["revision"], 2);
    assert_eq!(added["applied_revision"], 0);
    assert_eq!(added["synchronized"], false);
    assert_eq!(added["entries"][0]["player_id"], player_id.to_string());

    let operators_uri = format!("/api/v1/servers/{server_id}/operators");
    let initial_operators = app
        .clone()
        .oneshot(request(Method::GET, &operators_uri, Some(&token), None))
        .await
        .unwrap();
    let initial_operators = json_body(initial_operators).await;
    assert_eq!(initial_operators["revision"], 2);
    assert_eq!(initial_operators["applied_revision"], 0);
    assert_eq!(initial_operators["synchronized"], false);
    assert_eq!(
        initial_operators["entries"][0]["player_id"],
        owner_profile_id.to_string()
    );
    assert_eq!(initial_operators["entries"][0]["name"], "WorldOwner");
    assert_eq!(initial_operators["entries"][0]["is_owner"], true);

    let owner_mutation = app
        .clone()
        .oneshot(request(
            Method::POST,
            &operators_uri,
            Some(&token),
            Some(json!({"name":"WorldOwner"})),
        ))
        .await
        .unwrap();
    assert_eq!(owner_mutation.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(owner_mutation).await["code"],
        "owner_operator_always_enabled"
    );
    let owner_roster_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operator_entries WHERE server_id = $1 AND player_id = $2",
    )
    .bind(server_id)
    .bind(owner_profile_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        owner_roster_rows, 0,
        "locked owner is represented only by identity"
    );

    let added_operator = app
        .clone()
        .oneshot(request(
            Method::POST,
            &operators_uri,
            Some(&token),
            Some(json!({"name":"Alex"})),
        ))
        .await
        .unwrap();
    assert_eq!(added_operator.status(), StatusCode::CREATED);
    let added_operator = json_body(added_operator).await;
    assert_eq!(added_operator["revision"], 3);
    assert_eq!(added_operator["applied_revision"], 0);
    assert_eq!(added_operator["synchronized"], false);
    assert_eq!(
        added_operator["entries"][0]["player_id"],
        player_id.to_string()
    );

    sqlx::query("DELETE FROM server_players WHERE server_id = $1 AND player_id = $2")
        .bind(server_id)
        .bind(player_id)
        .execute(&pool)
        .await
        .unwrap();
    let readded_offline_bedrock = app
        .clone()
        .oneshot(request(
            Method::POST,
            &operators_uri,
            Some(&token),
            Some(json!({"name":"Alex"})),
        ))
        .await
        .unwrap();
    assert_eq!(readded_offline_bedrock.status(), StatusCode::CREATED);
    assert_eq!(
        json_body(readded_offline_bedrock).await["entries"][0]["player_id"],
        player_id.to_string()
    );

    let removed_operator = app
        .clone()
        .oneshot(request(
            Method::DELETE,
            &format!("{operators_uri}?player_id={player_id}"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(removed_operator.status(), StatusCode::OK);
    let removed_operator = json_body(removed_operator).await;
    assert_eq!(removed_operator["revision"], 4);
    assert_eq!(removed_operator["entries"].as_array().unwrap().len(), 1);
    assert_eq!(removed_operator["entries"][0]["is_owner"], true);

    let offline_bedrock_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO server_known_players(server_id, player_id, name, edition)
         VALUES($1, $2, 'OfflineBedrock', 'bedrock')",
    )
    .bind(server_id)
    .bind(offline_bedrock_id)
    .execute(&pool)
    .await
    .unwrap();
    let known_bedrock = app
        .clone()
        .oneshot(request(
            Method::POST,
            &operators_uri,
            Some(&token),
            Some(json!({"name":"OfflineBedrock"})),
        ))
        .await
        .unwrap();
    assert_eq!(known_bedrock.status(), StatusCode::CREATED);
    let known_bedrock = json_body(known_bedrock).await;
    assert!(
        known_bedrock["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| {
                entry["player_id"] == offline_bedrock_id.to_string()
                    && entry["name"] == "OfflineBedrock"
            })
    );

    let ambiguous_ids = [Uuid::new_v4(), Uuid::new_v4()];
    for id in ambiguous_ids {
        sqlx::query(
            "INSERT INTO server_players(server_id, player_id, name, edition)
             VALUES($1, $2, 'Twin', 'bedrock')",
        )
        .bind(server_id)
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    }
    let ambiguous = app
        .clone()
        .oneshot(request(
            Method::POST,
            &operators_uri,
            Some(&token),
            Some(json!({"name":"Twin"})),
        ))
        .await
        .unwrap();
    assert_eq!(ambiguous.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json_body(ambiguous).await["code"], "ambiguous_player_name");
    let unsupported_bedrock = app
        .clone()
        .oneshot(request(
            Method::POST,
            &operators_uri,
            Some(&token),
            Some(json!({"name":"Gamertag With Spaces"})),
        ))
        .await
        .unwrap();
    assert_eq!(
        unsupported_bedrock.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        json_body(unsupported_bedrock).await["code"],
        "bedrock_player_lookup_unavailable"
    );

    let activity = app
        .clone()
        .oneshot(request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}/activity?limit=8"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    let activity = json_body(activity).await;
    assert!(!activity.as_array().unwrap().is_empty());

    sqlx::query(
        "INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes, checksum, reason, complete)
         VALUES($1,$2,1,'complete-one',10,'one','manual',true),
               ($3,$2,2,'incomplete-two',20,'two','manual',false)",
    )
    .bind(Uuid::new_v4())
    .bind(server_id)
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .unwrap();
    let snapshots = app
        .clone()
        .oneshot(request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}/snapshots"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    let snapshots = json_body(snapshots).await;
    assert_eq!(snapshots.as_array().unwrap().len(), 1);
    assert_eq!(snapshots[0]["generation"], 1);

    let restore = app
        .clone()
        .oneshot(request(
            Method::POST,
            &format!("/api/v1/servers/{server_id}/commands"),
            Some(&token),
            Some(json!({"command":"restore", "generation":1})),
        ))
        .await
        .unwrap();
    assert_eq!(restore.status(), StatusCode::ACCEPTED);
    assert_eq!(json_body(restore).await["observed_state"], "restoring");

    let delete_whitelist = app
        .clone()
        .oneshot(request(
            Method::DELETE,
            &format!("/api/v1/servers/{server_id}/whitelist?player_id={player_id}"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(delete_whitelist.status(), StatusCode::OK);
    assert!(
        json_body(delete_whitelist).await["entries"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    sqlx::query(
        "DELETE FROM users WHERE id IN (
             SELECT user_id FROM identities WHERE provider = 'google' AND subject LIKE 'api-v1-test-%'
         )",
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

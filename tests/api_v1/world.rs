use super::support::*;

async fn reset(
    app: &axum::Router,
    server_id: Uuid,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(request(
            Method::POST,
            &format!("/api/v1/servers/{server_id}/commands"),
            Some(token),
            Some(body),
        ))
        .await
        .unwrap();
    let status = response.status();
    (status, json_body(response).await)
}

async fn queued_world(pool: &PgPool, server_id: Uuid) -> Value {
    sqlx::query_scalar(
        "SELECT payload->'world' FROM operations
         WHERE server_id = $1 AND kind = 'reset' AND status = 'pending'",
    )
    .bind(server_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn owners_pick_the_world_a_reset_generates() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_id, token) = user_session(&pool, "active").await;
    let server_id = create_test_server(&pool, owner_id, "world-reset").await;
    sqlx::query("UPDATE servers SET observed_state = 'sleeping' WHERE id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();
    let worker = healthy_worker_template(
        Uuid::new_v4(),
        format!("world-reset-{server_id}"),
        "127.0.0.1".into(),
        8 * 1024 * 1024 * 1024,
        40 * 1024 * 1024 * 1024,
        4000,
        34000,
        34100,
    );
    heartbeat_worker(&pool, &worker).await.unwrap();
    let app = router(state(pool.clone()));

    let server = json_body(
        app.clone()
            .oneshot(request(
                Method::GET,
                &format!("/api/v1/servers/{server_id}"),
                Some(&token),
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        server["world"],
        json!({"world_type": "normal", "structures": true, "seed": null})
    );

    for (invalid, message) in [
        (
            json!({"world_type": "flat", "flat": {"layers": [], "biome": "minecraft:plains", "features": false, "lakes": false, "structure_overrides": []}}),
            "layers",
        ),
        (
            json!({"world_type": "flat", "flat": {"layers": [{"block": "minecraft:nonsense", "height": 1}], "biome": "minecraft:plains", "features": false, "lakes": false, "structure_overrides": []}}),
            "unknown block",
        ),
        (
            json!({"world_type": "single_biome_surface", "biome": "minecraft:nonsense"}),
            "unknown biome",
        ),
        (
            json!({"world_type": "normal", "biome": "minecraft:desert"}),
            "single biome",
        ),
        (
            json!({"world_type": "normal", "seed": "x".repeat(33)}),
            "32 characters",
        ),
    ] {
        let (status, body) = reset(
            &app,
            server_id,
            &token,
            json!({"command": "reset", "mode": "world", "world": invalid}),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{invalid}");
        assert_eq!(body["code"], "invalid_world");
        assert!(
            body["message"].as_str().unwrap().contains(message),
            "{body}"
        );
    }
    let (status, _) = reset(&app, server_id, &token, json!({"command": "reset", "mode": "world", "world": {"world_type": "debug_all_block_states"}})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let untouched: String =
        sqlx::query_scalar("SELECT observed_state::text FROM servers WHERE id = $1")
            .bind(server_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(untouched, "sleeping");

    let (status, body) = reset(
        &app,
        server_id,
        &token,
        json!({"command": "reset", "mode": "world", "world": {
            "world_type": "single_biome_surface",
            "biome": "minecraft:cherry_grove",
            "structures": false,
            "seed": "kurbis",
        }}),
    )
    .await;
    assert_eq!(
        (status, &body["observed_state"]),
        (StatusCode::ACCEPTED, &json!("resetting"))
    );
    assert_eq!(
        queued_world(&pool, server_id).await,
        json!({"world_type": "single_biome_surface", "biome": "minecraft:cherry_grove", "structures": false, "seed": -1_120_102_460_i64})
    );
    sqlx::query("UPDATE operations SET status = 'cancelled' WHERE server_id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE servers SET observed_state = 'sleeping', desired_state = 'stopped' WHERE id = $1",
    )
    .bind(server_id)
    .execute(&pool)
    .await
    .unwrap();

    // Without a world the reset is a default world with a random seed, as before.
    let (status, _) = reset(
        &app,
        server_id,
        &token,
        json!({"command": "reset", "mode": "full"}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let world = queued_world(&pool, server_id).await;
    assert_eq!(world["world_type"], "normal");
    assert_eq!(world["structures"], true);
    assert!(world["seed"].is_i64());
    assert!(world.get("flat").is_none() && world.get("biome").is_none());

    sqlx::query("DELETE FROM workers WHERE id = $1")
        .bind(worker.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(owner_id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn the_world_catalog_is_public_and_lists_vanillas_choices() {
    let app = router(state(test_pool().await));
    let response = app
        .oneshot(request(Method::GET, "/api/v1/world-catalog", None, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "public, max-age=86400"
    );
    let catalog = json_body(response).await;
    let ids = |key: &str| -> Vec<String> {
        catalog[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(
        ids("world_types"),
        [
            "normal",
            "flat",
            "large_biomes",
            "amplified",
            "single_biome_surface"
        ]
    );
    assert_eq!(ids("flat_presets").len(), 9);
    assert_eq!(catalog["flat_presets"][8]["name"], "The Void");
    assert!(ids("biomes").contains(&"minecraft:cherry_grove".to_owned()));
    assert!(ids("blocks").contains(&"minecraft:air".to_owned()));
    assert!(ids("blocks").contains(&"minecraft:water".to_owned()));
    assert!(ids("structures").contains(&"minecraft:villages".to_owned()));
}

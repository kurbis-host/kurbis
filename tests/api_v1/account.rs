use super::support::*;

#[tokio::test]
async fn account_can_switch_default_between_merged_worlds_without_cross_owner_access() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_id, token) = user_session(&pool, "active").await;
    let (other_owner_id, _) = user_session(&pool, "active").await;
    let suffix = Uuid::new_v4().simple().to_string();
    let browser_world = Uuid::new_v4();
    let merged_world = Uuid::new_v4();
    let other_world = Uuid::new_v4();
    for (server_id, owner, label) in [
        (browser_world, owner_id, "browser"),
        (merged_world, owner_id, "merged"),
        (other_world, other_owner_id, "other"),
    ] {
        sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1,$2,$3)")
            .bind(server_id)
            .bind(owner)
            .bind(format!("world-{label}-{}", &suffix[..8]))
            .execute(&pool)
            .await
            .unwrap();
    }
    let app = router(state(pool.clone()));

    let account = app
        .clone()
        .oneshot(request(Method::GET, "/api/v1/account", Some(&token), None))
        .await
        .unwrap();
    assert_eq!(account.status(), StatusCode::OK);
    let account = json_body(account).await;
    assert!(
        account["identities"][0]["id"].is_string(),
        "account identities must include their IDs"
    );
    let worlds = account["worlds"].as_array().unwrap();
    assert_eq!(worlds.len(), 2);
    assert_eq!(
        worlds
            .iter()
            .filter(|world| world["is_default"] == true)
            .count(),
        1
    );
    assert!(
        worlds
            .iter()
            .any(|world| world["id"] == browser_world.to_string())
    );
    assert!(
        worlds
            .iter()
            .any(|world| world["id"] == merged_world.to_string())
    );
    assert!(
        worlds.iter().any(|world| {
            world["is_default"] == true && world["id"] == browser_world.to_string()
        })
    );

    let switched = app
        .clone()
        .oneshot(request(
            Method::PUT,
            &format!("/api/v1/account/worlds/{merged_world}/default"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(switched.status(), StatusCode::NO_CONTENT);

    let default_servers = app
        .clone()
        .oneshot(request(Method::GET, "/api/v1/servers", Some(&token), None))
        .await
        .unwrap();
    let default_servers = json_body(default_servers).await;
    assert!(default_servers.as_array().unwrap().iter().any(|server| {
        server["id"] == merged_world.to_string() && server["is_default"] == true
    }));

    let foreign_selection = app
        .clone()
        .oneshot(request(
            Method::PUT,
            &format!("/api/v1/account/worlds/{other_world}/default"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(foreign_selection.status(), StatusCode::NOT_FOUND);
    let default_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM servers WHERE owner_id=$1 AND is_default")
            .bind(owner_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(default_count, 1);

    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind([owner_id, other_owner_id].as_slice())
        .execute(&pool)
        .await
        .unwrap();
}

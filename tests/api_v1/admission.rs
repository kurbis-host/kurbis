use super::support::*;

#[tokio::test]
async fn authenticated_admission_serializes_creation_redirects_and_keeps_existing_host_access() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let identity_id = Uuid::new_v4();
    let (owner_id, _) = user_session(&pool, "active").await;
    let java_uuid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO identities(id, user_id, provider, subject, display_name, minecraft_player_id)
         VALUES($1, $2, 'minecraft_java', $3, 'Admission Java', $4)",
    )
    .bind(identity_id)
    .bind(owner_id)
    .bind(java_uuid.to_string())
    .bind(java_uuid)
    .execute(&pool)
    .await
    .unwrap();

    let app = router(state(pool.clone()));
    let hosts = (0..8)
        .map(|_| format!("race-{}", &Uuid::new_v4().simple().to_string()[..12]))
        .collect::<Vec<_>>();
    let identity = json!({"kind":"java", "uuid":java_uuid});
    let responses = join_all(hosts.iter().map(|subdomain| {
        let app = app.clone();
        let identity = identity.clone();
        let hostname = format!("{subdomain}.mc.example.test");
        async move {
            app.oneshot(internal_admission_request(
                &hostname,
                Some(identity),
                Some(java_uuid),
            ))
            .await
            .unwrap()
        }
    }))
    .await;

    let mut admitted_server = None;
    let mut created_count = 0;
    let mut redirect_count = 0;
    for response in responses {
        assert_eq!(response.status(), StatusCode::OK);
        let result = json_body(response).await;
        let server_id = Uuid::parse_str(result["target"]["server_id"].as_str().unwrap()).unwrap();
        if let Some(expected) = admitted_server {
            assert_eq!(server_id, expected);
        } else {
            admitted_server = Some(server_id);
        }
        if result["created"] == true {
            created_count += 1;
        }
        if let Some(message) = result["private_message"].as_str() {
            redirect_count += 1;
            let address = result["target"]["subdomain"].as_str().unwrap();
            assert_eq!(
                message,
                format!(
                    "You already have a server at {address}.mc.example.test; connecting you there."
                )
            );
        }
    }
    assert_eq!(created_count, 1);
    assert_eq!(redirect_count, hosts.len() - 1);
    let server_id = admitted_server.unwrap();
    let (server_count, default_count): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE is_default)
         FROM servers WHERE owner_id = $1",
    )
    .bind(owner_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((server_count, default_count), (1, 1));

    let hostname: String = sqlx::query_scalar("SELECT subdomain FROM servers WHERE id = $1")
        .bind(server_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let existing_guest = app
        .clone()
        .oneshot(internal_admission_request(
            &format!("{hostname}.mc.example.test"),
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(existing_guest.status(), StatusCode::OK);
    let existing_guest = json_body(existing_guest).await;
    assert_eq!(existing_guest["target"]["server_id"], server_id.to_string());
    assert_eq!(existing_guest["created"], false);

    let guest_unknown = app
        .clone()
        .oneshot(internal_admission_request(
            &format!(
                "guest-{}.mc.example.test",
                &Uuid::new_v4().simple().to_string()[..12]
            ),
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(guest_unknown.status(), StatusCode::NOT_FOUND);

    let forged_profile = app
        .clone()
        .oneshot(internal_admission_request(
            &format!(
                "forged-{}.mc.example.test",
                &Uuid::new_v4().simple().to_string()[..12]
            ),
            Some(identity),
            Some(Uuid::new_v4()),
        ))
        .await
        .unwrap();
    assert_eq!(forged_profile.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let (bedrock_owner, _) = user_session(&pool, "active").await;
    let xuid = format!("{}", Uuid::new_v4().as_u64_pair().0);
    let bedrock_profile = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO identities(id, user_id, provider, subject, display_name, minecraft_player_id)
         VALUES($1, $2, 'minecraft_bedrock', $3, 'Admission Bedrock', $4)",
    )
    .bind(Uuid::new_v4())
    .bind(bedrock_owner)
    .bind(&xuid)
    .bind(bedrock_profile)
    .execute(&pool)
    .await
    .unwrap();
    let bedrock = app
        .clone()
        .oneshot(internal_admission_request(
            &format!(
                "bedrock-{}.mc.example.test",
                &Uuid::new_v4().simple().to_string()[..12]
            ),
            Some(json!({"kind":"bedrock", "xuid":xuid})),
            Some(bedrock_profile),
        ))
        .await
        .unwrap();
    assert_eq!(bedrock.status(), StatusCode::OK);

    let (mixed_owner, mixed_token) = user_session(&pool, "active").await;
    let mixed_java_uuid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO identities(id, user_id, provider, subject, display_name, minecraft_player_id)
         VALUES($1, $2, 'minecraft_java', $3, 'Mixed Race Java', $4)",
    )
    .bind(Uuid::new_v4())
    .bind(mixed_owner)
    .bind(mixed_java_uuid.to_string())
    .bind(mixed_java_uuid)
    .execute(&pool)
    .await
    .unwrap();
    let mixed_identity = json!({"kind":"java", "uuid":mixed_java_uuid});
    let requests = (0..8).map(|index| {
        let app = app.clone();
        let token = mixed_token.clone();
        let identity = mixed_identity.clone();
        async move {
            let subdomain = format!(
                "mixed-{}-{}",
                index,
                &Uuid::new_v4().simple().to_string()[..8]
            );
            if index % 2 == 0 {
                app.oneshot(request(
                    Method::POST,
                    "/api/v1/servers",
                    Some(&token),
                    Some(json!({"subdomain":subdomain})),
                ))
                .await
                .unwrap()
            } else {
                app.oneshot(internal_admission_request(
                    &format!("{subdomain}.mc.example.test"),
                    Some(identity),
                    Some(mixed_java_uuid),
                ))
                .await
                .unwrap()
            }
        }
    });
    for response in join_all(requests).await {
        assert!(
            matches!(
                response.status(),
                StatusCode::CREATED | StatusCode::CONFLICT | StatusCode::OK
            ),
            "mixed web/admission request returned {}",
            response.status()
        );
    }
    let (mixed_server_count, mixed_default_count): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE is_default)
         FROM servers WHERE owner_id = $1",
    )
    .bind(mixed_owner)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((mixed_server_count, mixed_default_count), (1, 1));

    let (suspended_owner, _) = user_session(&pool, "suspended").await;
    let suspended_uuid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO identities(id, user_id, provider, subject, display_name, minecraft_player_id)
         VALUES($1, $2, 'minecraft_java', $3, 'Suspended Java', $4)",
    )
    .bind(Uuid::new_v4())
    .bind(suspended_owner)
    .bind(suspended_uuid.to_string())
    .bind(suspended_uuid)
    .execute(&pool)
    .await
    .unwrap();
    let suspended_admission = app
        .clone()
        .oneshot(internal_admission_request(
            &format!(
                "suspended-{}.mc.example.test",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            Some(json!({"kind":"java", "uuid":suspended_uuid})),
            Some(suspended_uuid),
        ))
        .await
        .unwrap();
    assert_eq!(suspended_admission.status(), StatusCode::FORBIDDEN);
    let suspended_server_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM servers WHERE owner_id = $1")
            .bind(suspended_owner)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(suspended_server_count, 0);

    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind([owner_id, bedrock_owner, mixed_owner, suspended_owner].as_slice())
        .execute(&pool)
        .await
        .unwrap();
}

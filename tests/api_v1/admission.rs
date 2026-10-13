use super::support::*;

#[tokio::test]
async fn first_verified_minecraft_join_registers_identity_and_creates_one_server() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let app = router(state(pool.clone()));
    let java_uuid = Uuid::new_v4();
    let identity = json!({"kind":"java", "uuid":java_uuid});
    let hosts = (0..6)
        .map(|_| format!("first-{}", &Uuid::new_v4().simple().to_string()[..10]))
        .collect::<Vec<_>>();

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

    let mut server_id = None;
    let mut created_count = 0;
    let mut redirect_count = 0;
    for response in responses {
        assert_eq!(response.status(), StatusCode::OK);
        let admission = json_body(response).await;
        let current_server =
            Uuid::parse_str(admission["target"]["server_id"].as_str().unwrap()).unwrap();
        if let Some(expected) = server_id {
            assert_eq!(current_server, expected);
        } else {
            server_id = Some(current_server);
        }
        if admission["created"] == true {
            created_count += 1;
        }
        if admission["private_message"].is_string() {
            redirect_count += 1;
        }
    }
    assert_eq!(created_count, 1);
    assert_eq!(redirect_count, hosts.len() - 1);

    let identity_record: (Uuid, String, Uuid, String) = sqlx::query_as(
        "SELECT i.user_id, i.display_name, i.minecraft_player_id, u.display_name
         FROM identities i JOIN users u ON u.id = i.user_id
         WHERE i.provider = 'minecraft_java' AND i.subject = $1",
    )
    .bind(java_uuid.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    let owner_id = identity_record.0;
    assert_eq!(identity_record.1, "Admission Player");
    assert_eq!(identity_record.2, java_uuid);
    assert_eq!(identity_record.3, "Admission Player");

    let server_owner: (Uuid, bool, i64) = sqlx::query_as(
        "SELECT owner_id, is_default,
                (SELECT count(*) FROM servers WHERE owner_id = $1)
         FROM servers WHERE id = $2",
    )
    .bind(identity_record.0)
    .bind(server_id.unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(server_owner, (identity_record.0, true, 1));

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(owner_id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn first_verified_bedrock_join_registers_its_xuid_and_creates_one_server() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let app = router(state(pool.clone()));
    let xuid = Uuid::new_v4().as_u64_pair().0.to_string();
    let minecraft_player_id = Uuid::new_v4();
    let response = app
        .oneshot(internal_admission_request(
            &format!("bedrock-first-{}.mc.example.test", &xuid[xuid.len() - 8..]),
            Some(json!({"kind":"bedrock", "xuid":xuid})),
            Some(minecraft_player_id),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let admission = json_body(response).await;
    assert_eq!(admission["created"], true);
    let server_id = Uuid::parse_str(admission["target"]["server_id"].as_str().unwrap()).unwrap();
    let identity_record: (Uuid, String, Uuid) = sqlx::query_as(
        "SELECT user_id, subject, minecraft_player_id FROM identities
         WHERE provider = 'minecraft_bedrock' AND subject = $1",
    )
    .bind(&xuid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(identity_record.1, xuid);
    assert_eq!(identity_record.2, minecraft_player_id);
    let server_owner: Uuid = sqlx::query_scalar("SELECT owner_id FROM servers WHERE id = $1")
        .bind(server_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(server_owner, identity_record.0);

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(identity_record.0)
        .execute(&pool)
        .await
        .unwrap();
}

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

    let users_before_unverified_join: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
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
    let users_after_unverified_join: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(users_after_unverified_join, users_before_unverified_join);

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

#[tokio::test]
async fn owner_of_a_failed_server_gets_a_panel_login_link_at_admission() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_id, _) = user_session(&pool, "active").await;
    let java_uuid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO identities(id, user_id, provider, subject, display_name, minecraft_player_id)
         VALUES($1, $2, 'minecraft_java', $3, 'Failed Owner', $4)",
    )
    .bind(Uuid::new_v4())
    .bind(owner_id)
    .bind(java_uuid.to_string())
    .bind(java_uuid)
    .execute(&pool)
    .await
    .unwrap();
    let server_id = Uuid::new_v4();
    let subdomain = format!("failed-{}", &server_id.simple().to_string()[..12]);
    sqlx::query(
        "INSERT INTO servers(id, owner_id, subdomain, observed_state)
         VALUES($1, $2, $3, 'failed')",
    )
    .bind(server_id)
    .bind(owner_id)
    .bind(&subdomain)
    .execute(&pool)
    .await
    .unwrap();
    let app = router(state(pool.clone()));
    let hostname = format!("{subdomain}.mc.example.test");
    let admit = |uuid: Uuid| {
        let app = app.clone();
        let hostname = hostname.clone();
        async move {
            let response = app
                .oneshot(internal_admission_request(
                    &hostname,
                    Some(json!({"kind":"java", "uuid":uuid})),
                    Some(uuid),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            json_body(response).await
        }
    };
    let login_tokens = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM server_login_tokens WHERE server_id = $1",
        )
        .bind(server_id)
        .fetch_one(&pool)
        .await
        .unwrap()
    };

    // The owner cannot reach the panel through a server that will not start,
    // so the lobby hands them the login link instead.
    let owner = admit(java_uuid).await;
    assert_eq!(owner["owner"], true);
    let message = owner["private_message"].as_str().unwrap();
    assert!(message.contains("/auth/server#token="), "{message}");
    assert_eq!(login_tokens().await, 1);

    // Anyone else joining the same address learns nothing and gets no link.
    let guest = admit(Uuid::new_v4()).await;
    assert_eq!(guest["owner"], false);
    assert!(guest["private_message"].is_null());
    assert_eq!(login_tokens().await, 1);

    // A healthy server needs no link: the owner uses /kurbis once it runs.
    sqlx::query("UPDATE servers SET observed_state = 'sleeping' WHERE id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();
    let owner = admit(java_uuid).await;
    assert_eq!(owner["owner"], true);
    assert!(owner["private_message"].is_null());
    assert_eq!(login_tokens().await, 1);
}

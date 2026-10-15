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
async fn linked_admission_serializes_creation_redirects_and_keeps_existing_host_access() {
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
        .bind([owner_id, bedrock_owner, suspended_owner].as_slice())
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
    let link = owner["private_link"].as_str().unwrap();
    assert!(link.contains("/auth/server#token="), "{link}");
    assert!(owner["private_message"].is_null());
    assert_eq!(login_tokens().await, 1);

    // Anyone else joining the same address learns nothing and gets no link.
    let guest = admit(Uuid::new_v4()).await;
    assert_eq!(guest["owner"], false);
    assert!(guest["private_link"].is_null());
    assert_eq!(login_tokens().await, 1);

    // A healthy server needs no link: the owner uses /kurbis once it runs.
    sqlx::query("UPDATE servers SET observed_state = 'sleeping' WHERE id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();
    let owner = admit(java_uuid).await;
    assert_eq!(owner["owner"], true);
    assert!(owner["private_link"].is_null());
    assert_eq!(login_tokens().await, 1);
}

fn redeem_request(token: &str, session: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri("/auth/minecraft/redeem")
        .header(header::ORIGIN, "https://panel.example.test")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(session) = session {
        builder = builder.header(header::COOKIE, format!("kurbis_session={session}"));
    }
    builder
        .body(Body::from(json!({ "token": token }).to_string()))
        .unwrap()
}

fn link_token(admission: &Value) -> String {
    let link = admission["account_link"].as_str().unwrap();
    let (base, token) = link.split_once("#token=").unwrap();
    assert_eq!(base, "https://panel.example.test/auth/minecraft");
    token.to_owned()
}

#[tokio::test]
async fn joining_after_a_web_sign_in_claims_the_name_and_links_both_accounts() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let app = router(state(pool.clone()));
    // A Google sign-in creates an account and nothing else.
    let (web_owner, web_token) = user_session(&pool, "active").await;
    let subdomain = format!("claim-{}", &Uuid::new_v4().simple().to_string()[..12]);
    let hostname = format!("{subdomain}.mc.example.test");
    let java_uuid = Uuid::new_v4();
    let join = || {
        let app = app.clone();
        let hostname = hostname.clone();
        async move {
            let response = app
                .oneshot(internal_admission_request(
                    &hostname,
                    Some(json!({"kind":"java", "uuid":java_uuid})),
                    Some(java_uuid),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            json_body(response).await
        }
    };

    // The first join creates the server under the player's own account and
    // hands them the one-use link that joins it to the web account.
    let first = join().await;
    assert_eq!(first["created"], true);
    assert_eq!(first["owner"], true);
    assert!(first["private_link"].is_null());
    let server_id = Uuid::parse_str(first["target"]["server_id"].as_str().unwrap()).unwrap();
    let player_account: Uuid = sqlx::query_scalar("SELECT owner_id FROM servers WHERE id = $1")
        .bind(server_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_ne!(player_account, web_owner);

    // Rejoining replaces the link instead of adding another live one.
    let stale = link_token(&first);
    let second = join().await;
    assert_eq!(second["created"], false);
    let token = link_token(&second);
    assert_ne!(token, stale);
    let live_links: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM minecraft_login_tokens WHERE provider = 'minecraft_java' AND subject = $1",
    )
    .bind(java_uuid.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(live_links, 1);
    let replaced = app
        .clone()
        .oneshot(redeem_request(&stale, Some(&web_token)))
        .await
        .unwrap();
    assert_eq!(replaced.status(), StatusCode::UNAUTHORIZED);

    // Opening the link where the web account is signed in makes them one.
    let redeemed = app
        .clone()
        .oneshot(redeem_request(&token, Some(&web_token)))
        .await
        .unwrap();
    assert_eq!(redeemed.status(), StatusCode::NO_CONTENT);
    let owner: Uuid = sqlx::query_scalar("SELECT owner_id FROM servers WHERE id = $1")
        .bind(server_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(owner, web_owner);
    let player_identity_owner: Uuid = sqlx::query_scalar(
        "SELECT user_id FROM identities WHERE provider = 'minecraft_java' AND subject = $1",
    )
    .bind(java_uuid.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(player_identity_owner, web_owner);
    let player_account_left: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
            .bind(player_account)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!player_account_left);

    // Linked, the owner signs in on the web and gets no more account links.
    let linked = join().await;
    assert_eq!(linked["owner"], true);
    assert!(linked["account_link"].is_null());

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(web_owner)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn the_account_link_signs_a_minecraft_only_owner_in_and_guests_get_none() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let app = router(state(pool.clone()));
    let subdomain = format!("solo-{}", &Uuid::new_v4().simple().to_string()[..12]);
    let hostname = format!("{subdomain}.mc.example.test");
    let join = |uuid: Uuid| {
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
    let owner_uuid = Uuid::new_v4();
    let owner = join(owner_uuid).await;
    assert_eq!(owner["created"], true);
    let guest_uuid = Uuid::new_v4();
    let guest = join(guest_uuid).await;
    assert_eq!(guest["owner"], false);
    assert!(guest["account_link"].is_null());

    let redeemed = app
        .clone()
        .oneshot(redeem_request(&link_token(&owner), None))
        .await
        .unwrap();
    assert_eq!(redeemed.status(), StatusCode::NO_CONTENT);
    let cookie = redeemed
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| value.strip_prefix("kurbis_session="))
        .and_then(|value| value.split(';').next())
        .unwrap()
        .to_owned();
    let servers = app
        .clone()
        .oneshot(request(Method::GET, "/api/v1/servers", Some(&cookie), None))
        .await
        .unwrap();
    assert_eq!(servers.status(), StatusCode::OK);
    let servers = json_body(servers).await;
    assert_eq!(servers.as_array().unwrap().len(), 1);
    assert_eq!(servers[0]["subdomain"], subdomain);

    sqlx::query(
        "DELETE FROM users WHERE id IN (SELECT user_id FROM identities
         WHERE provider = 'minecraft_java' AND subject = ANY($1))",
    )
    .bind([owner_uuid.to_string(), guest_uuid.to_string()].as_slice())
    .execute(&pool)
    .await
    .unwrap();
}

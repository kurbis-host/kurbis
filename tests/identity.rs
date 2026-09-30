mod common;

use common::{create_user, test_pool};
use kurbis_hoster::control_plane::identity::{attach_minecraft_identity, link_oauth_identity};
use sqlx::PgPool;
use uuid::Uuid;

async fn cleanup(pool: &PgPool, users: &[Uuid]) {
    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(users)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn oauth_identity_creation_links_verified_email_and_discards_unverified_email() {
    let pool = test_pool().await;
    let suffix = Uuid::new_v4();
    let email = format!("identity-{suffix}@example.test");

    let google_user = link_oauth_identity(
        &pool,
        "google",
        &format!("google-{suffix}"),
        Some(&format!("  {}  ", email.to_uppercase())),
        true,
        "Google Player",
        None,
    )
    .await
    .unwrap();
    let discord_user = link_oauth_identity(
        &pool,
        "discord",
        &format!("discord-{suffix}"),
        Some(&email),
        true,
        "Discord Player",
        None,
    )
    .await
    .unwrap();
    let unverified_user = link_oauth_identity(
        &pool,
        "google",
        &format!("unverified-{suffix}"),
        Some("unverified@example.test"),
        false,
        "Unverified Player",
        None,
    )
    .await
    .unwrap();

    assert_eq!(google_user, discord_user);
    assert_ne!(google_user, unverified_user);

    let account_email: String = sqlx::query_scalar(
        "SELECT email FROM identities WHERE user_id = $1 AND email_verified LIMIT 1",
    )
    .bind(google_user)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(account_email, email);

    let identities: Vec<(String, String, Option<String>, bool)> = sqlx::query_as(
        "SELECT provider, display_name, email, email_verified FROM identities
             WHERE user_id = $1 ORDER BY provider",
    )
    .bind(google_user)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        identities,
        vec![
            (
                "discord".to_owned(),
                "Discord Player".to_owned(),
                Some(email.clone()),
                true,
            ),
            (
                "google".to_owned(),
                "Google Player".to_owned(),
                Some(email.clone()),
                true,
            ),
        ]
    );

    let unverified: (Option<String>, bool) =
        sqlx::query_as("SELECT email, email_verified FROM identities WHERE user_id = $1")
            .bind(unverified_user)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(unverified, (None, false));
    let unverified_account_email: Option<String> = sqlx::query_scalar(
        "SELECT email FROM identities WHERE user_id = $1 AND email_verified LIMIT 1",
    )
    .bind(unverified_user)
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(unverified_account_email, None);

    cleanup(&pool, &[google_user, unverified_user]).await;
}

#[tokio::test]
async fn linking_a_verified_game_identity_merges_accounts_without_losing_worlds_or_sessions() {
    let pool = test_pool().await;
    let suffix = Uuid::new_v4();
    let browser_user = create_user(
        &pool,
        "google",
        &format!("merge-browser-{suffix}"),
        "Browser Owner",
        "active",
        None,
    )
    .await;
    let game_user = create_user(
        &pool,
        "discord",
        &format!("merge-game-{suffix}"),
        "Game Owner",
        "suspended",
        None,
    )
    .await;
    let first_server = Uuid::new_v4();
    let second_server = Uuid::new_v4();
    let third_server = Uuid::new_v4();
    for (server_id, owner, label) in [
        (first_server, browser_user, "browser"),
        (second_server, game_user, "game"),
        (third_server, game_user, "game-extra"),
    ] {
        sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)")
            .bind(server_id)
            .bind(owner)
            .bind(format!(
                "merge-{label}-{}",
                &suffix.simple().to_string()[..8]
            ))
            .execute(&pool)
            .await
            .unwrap();
    }
    for server_id in [first_server, second_server, third_server] {
        sqlx::query("INSERT INTO server_logs(server_id, line) VALUES($1, 'preserved history')")
            .bind(server_id)
            .execute(&pool)
            .await
            .unwrap();
    }
    let java_uuid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO identities(id, user_id, provider, subject, display_name)
         VALUES($1, $2, 'minecraft_java', $3, 'Java Player')",
    )
    .bind(Uuid::new_v4())
    .bind(game_user)
    .bind(java_uuid.to_string())
    .execute(&pool)
    .await
    .unwrap();
    for owner in [browser_user, game_user] {
        let token = format!("merge-session-{owner}");
        sqlx::query("INSERT INTO sessions(id, token_hash, user_id) VALUES($1, $2, $3)")
            .bind(Uuid::new_v4())
            .bind(blake3::hash(token.as_bytes()).as_bytes().to_vec())
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
    }

    let merged_user = attach_minecraft_identity(
        &pool,
        "minecraft_java",
        &java_uuid.to_string(),
        "Java Player",
        Some(browser_user),
    )
    .await
    .unwrap();

    assert_eq!(merged_user, browser_user);
    let worlds: Vec<(Uuid, bool)> =
        sqlx::query_as("SELECT id, is_default FROM servers WHERE owner_id = $1 ORDER BY id")
            .bind(browser_user)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(worlds.len(), 3);
    assert_eq!(
        worlds.iter().filter(|(_, is_default)| *is_default).count(),
        1
    );
    assert!(
        worlds
            .iter()
            .any(|(id, is_default)| *id == first_server && *is_default)
    );
    assert!(
        worlds
            .iter()
            .any(|(id, is_default)| *id == second_server && !*is_default)
    );
    assert!(
        worlds
            .iter()
            .any(|(id, is_default)| *id == third_server && !*is_default)
    );

    let source_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
            .bind(game_user)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!source_exists);
    let session_count: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
        .bind(browser_user)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(session_count, 2);
    let linked_identity_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM identities WHERE user_id = $1 AND provider IN ('google', 'discord', 'minecraft_java')",
    )
    .bind(browser_user)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(linked_identity_count, 3);
    let suspended: bool =
        sqlx::query_scalar("SELECT account_status = 'suspended' FROM users WHERE id = $1")
            .bind(browser_user)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(suspended);
    let preserved_history: i64 =
        sqlx::query_scalar("SELECT count(*) FROM server_logs WHERE server_id = ANY($1)")
            .bind([first_server, second_server, third_server].as_slice())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(preserved_history, 3);

    cleanup(&pool, &[browser_user]).await;
}

#[tokio::test]
async fn concurrent_identity_merges_preserve_all_sources_and_the_browser_default() {
    let pool = test_pool().await;
    let suffix = Uuid::new_v4();
    let browser_user = create_user(
        &pool,
        "google",
        &format!("concurrent-browser-{suffix}"),
        "Browser Owner",
        "active",
        None,
    )
    .await;
    let java_subject = Uuid::new_v4().to_string();
    let java_user =
        attach_minecraft_identity(&pool, "minecraft_java", &java_subject, "Java Player", None)
            .await
            .unwrap();
    let bedrock_subject = Uuid::new_v4().as_u128().to_string();
    let bedrock_user = attach_minecraft_identity(
        &pool,
        "minecraft_bedrock",
        &bedrock_subject,
        "Bedrock Player",
        None,
    )
    .await
    .unwrap();
    let default_server = Uuid::new_v4();
    let java_server = Uuid::new_v4();
    let bedrock_server = Uuid::new_v4();
    for (server_id, owner, label) in [
        (default_server, browser_user, "default"),
        (java_server, java_user, "java"),
        (bedrock_server, bedrock_user, "bedrock"),
    ] {
        sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)")
            .bind(server_id)
            .bind(owner)
            .bind(format!(
                "concurrent-{label}-{}",
                &suffix.simple().to_string()[..8]
            ))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO server_logs(server_id, line) VALUES($1, $2)")
            .bind(server_id)
            .bind(format!("history-{label}"))
            .execute(&pool)
            .await
            .unwrap();
        let token = format!("concurrent-session-{owner}");
        sqlx::query("INSERT INTO sessions(id, token_hash, user_id) VALUES($1, $2, $3)")
            .bind(Uuid::new_v4())
            .bind(blake3::hash(token.as_bytes()).as_bytes().to_vec())
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE users SET account_status = 'suspended' WHERE id = $1")
        .bind(bedrock_user)
        .execute(&pool)
        .await
        .unwrap();

    let java_link = attach_minecraft_identity(
        &pool,
        "minecraft_java",
        &java_subject,
        "Java Player",
        Some(browser_user),
    );
    let bedrock_link = attach_minecraft_identity(
        &pool,
        "minecraft_bedrock",
        &bedrock_subject,
        "Bedrock Player",
        Some(browser_user),
    );
    let (java_result, bedrock_result) = tokio::join!(java_link, bedrock_link);
    assert_eq!(java_result.unwrap(), browser_user);
    assert_eq!(bedrock_result.unwrap(), browser_user);

    let worlds: Vec<(Uuid, bool)> = sqlx::query_as(
        "SELECT id, is_default FROM servers WHERE owner_id = $1 ORDER BY created_at, id",
    )
    .bind(browser_user)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(worlds.len(), 3);
    assert_eq!(
        worlds.iter().filter(|(_, is_default)| *is_default).count(),
        1
    );
    assert!(
        worlds
            .iter()
            .any(|(id, is_default)| *id == default_server && *is_default)
    );

    let session_count: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id=$1")
        .bind(browser_user)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(session_count, 3);
    let log_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM server_logs WHERE server_id=ANY($1)")
            .bind([default_server, java_server, bedrock_server].as_slice())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(log_count, 3);
    let suspended: bool =
        sqlx::query_scalar("SELECT account_status='suspended' FROM users WHERE id=$1")
            .bind(browser_user)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(suspended);
    let identity_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM identities WHERE user_id=$1")
            .bind(browser_user)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(identity_count, 3);

    cleanup(&pool, &[browser_user, java_user, bedrock_user]).await;
}

#[tokio::test]
async fn verified_email_reauthentication_merges_unverified_legacy_accounts() {
    let pool = test_pool().await;
    let suffix = Uuid::new_v4();
    let email = format!("legacy-{suffix}@example.test");
    let discord_user = create_user(
        &pool,
        "discord",
        &format!("legacy-discord-{suffix}"),
        "Legacy Discord",
        "active",
        None,
    )
    .await;
    let google_user = create_user(
        &pool,
        "google",
        &format!("legacy-google-{suffix}"),
        "Legacy Google",
        "suspended",
        None,
    )
    .await;
    for (provider, owner) in [("discord", discord_user), ("google", google_user)] {
        sqlx::query(
            "UPDATE identities SET email=$3, email_normalized=$4, email_verified=false
             WHERE provider=$1 AND user_id=$2",
        )
        .bind(provider)
        .bind(owner)
        .bind(format!("  {}  ", email.to_uppercase()))
        .bind(email.to_lowercase())
        .execute(&pool)
        .await
        .unwrap();
        let token = format!("legacy-session-{owner}");
        sqlx::query("INSERT INTO sessions(id, token_hash, user_id) VALUES($1,$2,$3)")
            .bind(Uuid::new_v4())
            .bind(blake3::hash(token.as_bytes()).as_bytes().to_vec())
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
    }

    let _ = link_oauth_identity(
        &pool,
        "google",
        &format!("legacy-google-{suffix}"),
        Some(&email),
        true,
        "Verified Google",
        None,
    )
    .await
    .unwrap();
    let survivor = link_oauth_identity(
        &pool,
        "discord",
        &format!("legacy-discord-{suffix}"),
        Some(&email),
        true,
        "Verified Discord",
        None,
    )
    .await
    .unwrap();
    assert_eq!(survivor, discord_user);
    let merged_status: String = sqlx::query_scalar("SELECT account_status FROM users WHERE id=$1")
        .bind(survivor)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(merged_status, "suspended");
    let preserved_sessions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id=$1")
            .bind(survivor)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(preserved_sessions, 2);
    let preserved_identities: i64 =
        sqlx::query_scalar("SELECT count(*) FROM identities WHERE user_id=$1")
            .bind(survivor)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(preserved_identities, 2);

    cleanup(&pool, &[discord_user, google_user]).await;
}

#[tokio::test]
async fn oldest_world_becomes_default_when_the_browser_account_has_none() {
    let pool = test_pool().await;
    let suffix = Uuid::new_v4();
    let browser_user = create_user(
        &pool,
        "google",
        &format!("empty-browser-{suffix}"),
        "Browser Owner",
        "active",
        None,
    )
    .await;
    let identity_subject = Uuid::new_v4().to_string();
    let source_user = attach_minecraft_identity(
        &pool,
        "minecraft_java",
        &identity_subject,
        "Java Player",
        None,
    )
    .await
    .unwrap();
    let oldest = Uuid::new_v4();
    let newer = Uuid::new_v4();
    for (server_id, name, offset) in [(newer, "newer", 1), (oldest, "oldest", 0)] {
        sqlx::query(
            "INSERT INTO servers(id, owner_id, subdomain, created_at)
             VALUES($1,$2,$3, now() + make_interval(secs => $4))",
        )
        .bind(server_id)
        .bind(source_user)
        .bind(format!(
            "oldest-{name}-{}",
            &suffix.simple().to_string()[..8]
        ))
        .bind(offset)
        .execute(&pool)
        .await
        .unwrap();
    }
    attach_minecraft_identity(
        &pool,
        "minecraft_java",
        &identity_subject,
        "Java Player",
        Some(browser_user),
    )
    .await
    .unwrap();

    let defaults: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM servers WHERE owner_id=$1 AND is_default")
            .bind(browser_user)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(defaults, vec![oldest]);

    cleanup(&pool, &[browser_user, source_user]).await;
}

mod common;

use common::{create_user, test_pool};
use kurbis_hoster::control_plane::identity::{attach_minecraft_identity, link_oauth_identity};
use sqlx::PgPool;
use uuid::Uuid;

async fn world_history_counts(pool: &PgPool, server_ids: &[Uuid]) -> Vec<(String, i64)> {
    let mut counts = Vec::new();
    for table in [
        "snapshots",
        "operations",
        "server_logs",
        "server_events",
        "server_players",
        "whitelist_entries",
    ] {
        let query = format!("SELECT count(*) FROM {table} WHERE server_id = ANY($1)");
        let count: i64 = sqlx::query_scalar(&query)
            .bind(server_ids)
            .fetch_one(pool)
            .await
            .unwrap();
        counts.push((table.to_owned(), count));
    }
    counts
}

async fn add_world(pool: &PgPool, owner: Uuid, name: &str) -> Uuid {
    let server_id = Uuid::new_v4();
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)")
        .bind(server_id)
        .bind(owner)
        .bind(format!(
            "{name}-{}",
            &Uuid::new_v4().simple().to_string()[..8]
        ))
        .execute(pool)
        .await
        .unwrap();

    sqlx::query(
        "INSERT INTO snapshots(id, server_id, generation, manifest_key, size_bytes,
                               checksum, reason, complete)
         VALUES($1, $2, 1, $3, 17, 'checksum', 'manual', true)",
    )
    .bind(Uuid::new_v4())
    .bind(server_id)
    .bind(format!("{name}/snapshot-1"))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO operations(id, server_id, kind, status, generation)
         VALUES($1, $2, 'snapshot', 'succeeded', 1)",
    )
    .bind(Uuid::new_v4())
    .bind(server_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO server_logs(server_id, line) VALUES($1, $2)")
        .bind(server_id)
        .bind(format!("{name} log entry"))
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO server_events(server_id, event_type) VALUES($1, 'snapshot')")
        .bind(server_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO server_players(server_id, player_id, name, edition)
         VALUES($1, $2, $3, 'java')",
    )
    .bind(server_id)
    .bind(Uuid::new_v4())
    .bind(format!("{name}Player"))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO whitelist_entries(server_id, player_id, name) VALUES($1, $2, $3)")
        .bind(server_id)
        .bind(Uuid::new_v4())
        .bind(format!("{name}Allowed"))
        .execute(pool)
        .await
        .unwrap();

    server_id
}

#[tokio::test]
async fn account_merge_preserves_suspension_default_world_and_world_history() {
    let pool = test_pool().await;
    let suffix = Uuid::new_v4();
    let browser_user = create_user(
        &pool,
        "google",
        &format!("preserve-browser-{suffix}"),
        "Browser Owner",
        "active",
        None,
    )
    .await;
    let game_user = create_user(
        &pool,
        "discord",
        &format!("preserve-game-{suffix}"),
        "Game Owner",
        "suspended",
        None,
    )
    .await;
    let browser_world = add_world(&pool, browser_user, "preserve-browser").await;
    let game_world = add_world(&pool, game_user, "preserve-game").await;
    let java_uuid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO identities(id, user_id, provider, subject, display_name, minecraft_player_id)
         VALUES($1, $2, 'minecraft_java', $3, 'Verified Java Player', $4)",
    )
    .bind(Uuid::new_v4())
    .bind(game_user)
    .bind(java_uuid.to_string())
    .bind(java_uuid)
    .execute(&pool)
    .await
    .unwrap();

    let server_ids = [browser_world, game_world];
    let history_before = world_history_counts(&pool, &server_ids).await;
    assert!(history_before.iter().all(|(_, count)| *count > 0));

    let merged_user = attach_minecraft_identity(
        &pool,
        "minecraft_java",
        &java_uuid.to_string(),
        java_uuid,
        "Verified Java Player",
        Some(browser_user),
    )
    .await
    .unwrap();

    assert_eq!(merged_user, browser_user);
    let (status,): (String,) = sqlx::query_as("SELECT account_status FROM users WHERE id = $1")
        .bind(browser_user)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "suspended");

    let worlds: Vec<(Uuid, bool)> =
        sqlx::query_as("SELECT id, is_default FROM servers WHERE owner_id = $1 ORDER BY id")
            .bind(browser_user)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(worlds.len(), 2);
    assert_eq!(
        worlds
            .iter()
            .filter(|(id, is_default)| *id == browser_world && *is_default)
            .count(),
        1,
        "the browser account's selected world remains the default"
    );
    assert!(worlds.iter().any(|(id, _)| *id == game_world));

    let history_after = world_history_counts(&pool, &server_ids).await;
    assert_eq!(
        history_after, history_before,
        "per-world backup and operational history must survive account merging"
    );

    let source_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
            .bind(game_user)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!source_exists);
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(browser_user)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn concurrent_verified_email_links_converge_on_one_account() {
    let pool = test_pool().await;
    let suffix = Uuid::new_v4();
    let email = format!("concurrent-{suffix}@example.test");
    let google_email = email.to_uppercase();
    let discord_email = format!(" {email} ");
    let google_subject = format!("concurrent-google-{suffix}");
    let discord_subject = format!("concurrent-discord-{suffix}");

    let (google, discord) = tokio::join!(
        link_oauth_identity(
            &pool,
            "google",
            &google_subject,
            Some(&google_email),
            true,
            "Google Player",
            None,
        ),
        link_oauth_identity(
            &pool,
            "discord",
            &discord_subject,
            Some(&discord_email),
            true,
            "Discord Player",
            None,
        ),
    );
    let google_user = google.unwrap();
    let discord_user = discord.unwrap();
    assert_eq!(google_user, discord_user);

    let account_count: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT user_id) FROM identities
         WHERE email_normalized = $1 AND email_verified",
    )
    .bind(&email)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(account_count, 1);
    let identity_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM identities WHERE user_id = $1 AND email_verified")
            .bind(google_user)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(identity_count, 2);

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(google_user)
        .execute(&pool)
        .await
        .unwrap();
}

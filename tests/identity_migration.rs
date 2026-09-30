use std::borrow::Cow;

use anyhow::Result;
use sqlx::{PgPool, migrate::Migrator, postgres::PgPoolOptions};
use uuid::Uuid;

type LegacyIdentity = (Uuid, String, String, Option<String>, Option<String>, bool);

#[tokio::test]
async fn identity_migration_preserves_legacy_accounts_without_trusting_unverified_emails() {
    let database_url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL must point to a disposable Postgres test database");
    let admin = kurbis_hoster::db::connect(&database_url)
        .await
        .expect("connect to disposable Postgres database");
    let schema = format!("identity_migration_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("create isolated schema");

    let search_path = schema.clone();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .after_connect(move |connection, _| {
            let command = format!("SET search_path TO {search_path}");
            Box::pin(async move {
                sqlx::query(&command).execute(connection).await?;
                Ok(())
            })
        })
        .connect(&database_url)
        .await
        .expect("connect to isolated schema");

    let result = verify_migration(&pool).await;
    pool.close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop isolated schema");
    admin.close().await;
    result.expect("legacy identity migration checks");
}

async fn verify_migration(pool: &PgPool) -> Result<()> {
    let all_migrations = sqlx::migrate!("./migrations");
    let legacy_migrations = Migrator {
        migrations: Cow::Owned(
            all_migrations
                .iter()
                .filter(|migration| migration.version < 10)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    };
    legacy_migrations.run(pool).await?;

    let first_user = Uuid::new_v4();
    let second_user = Uuid::new_v4();
    for (id, provider, subject, email, subdomain) in [
        (
            first_user,
            "google",
            "legacy-google-subject",
            "  Shared@Example.Test  ",
            "legacy-google-world",
        ),
        (
            second_user,
            "discord",
            "legacy-discord-subject",
            "shared@example.test",
            "legacy-discord-world",
        ),
    ] {
        sqlx::query(
            "INSERT INTO users(id, oauth_provider, oauth_subject, email, display_name)
             VALUES($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(provider)
        .bind(subject)
        .bind(email)
        .bind(format!("Legacy {provider}"))
        .execute(pool)
        .await?;
        sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)")
            .bind(Uuid::new_v4())
            .bind(id)
            .bind(subdomain)
            .execute(pool)
            .await?;
    }

    let live_token_hash = [1_u8; 32];
    let expired_token_hash = [2_u8; 32];
    sqlx::query(
        "INSERT INTO sessions(token_hash, user_id, expires_at)
         VALUES($1, $2, now() + interval '1 day'),
               ($3, $2, now() - interval '1 second')",
    )
    .bind(live_token_hash.as_slice())
    .bind(first_user)
    .bind(expired_token_hash.as_slice())
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO oauth_states(state_hash, provider, expires_at)
         VALUES($1, 'google', now() + interval '1 minute')",
    )
    .bind([3_u8; 32].as_slice())
    .execute(pool)
    .await?;

    all_migrations.run(pool).await?;

    let identities: Vec<LegacyIdentity> = sqlx::query_as(
        "SELECT user_id, provider, subject, email, email_normalized, email_verified
             FROM identities ORDER BY provider",
    )
    .fetch_all(pool)
    .await?;
    assert_eq!(
        identities,
        vec![
            (
                second_user,
                "discord".to_owned(),
                "legacy-discord-subject".to_owned(),
                Some("shared@example.test".to_owned()),
                Some("shared@example.test".to_owned()),
                false,
            ),
            (
                first_user,
                "google".to_owned(),
                "legacy-google-subject".to_owned(),
                Some("  Shared@Example.Test  ".to_owned()),
                Some("shared@example.test".to_owned()),
                false,
            ),
        ]
    );

    let session: (Uuid, Vec<u8>, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("SELECT id, token_hash, last_seen_at FROM sessions WHERE user_id = $1")
            .bind(first_user)
            .fetch_one(pool)
            .await?;
    assert_eq!(session.1, live_token_hash);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sessions")
            .fetch_one(pool)
            .await?,
        1,
        "expired legacy sessions must not remain valid after migration"
    );
    let migrated_state: (String, Option<Uuid>) =
        sqlx::query_as("SELECT intent, user_id FROM oauth_states")
            .fetch_one(pool)
            .await?;
    assert_eq!(migrated_state, ("login".to_owned(), None));

    let default_count: i64 = sqlx::query_scalar("SELECT count(*) FROM servers WHERE is_default")
        .fetch_one(pool)
        .await?;
    assert_eq!(default_count, 2);
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1, $2, $3)")
        .bind(Uuid::new_v4())
        .bind(first_user)
        .bind("legacy-merged-world")
        .execute(pool)
        .await?;
    let first_user_defaults: i64 =
        sqlx::query_scalar("SELECT count(*) FROM servers WHERE owner_id = $1 AND is_default")
            .bind(first_user)
            .fetch_one(pool)
            .await?;
    assert_eq!(first_user_defaults, 1);

    let old_columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns
         WHERE table_schema = current_schema() AND table_name = 'users'
           AND column_name IN ('oauth_provider', 'oauth_subject', 'email')",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(old_columns, 0, "legacy provider columns must be removed");
    Ok(())
}

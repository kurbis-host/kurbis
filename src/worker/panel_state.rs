use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    pumpkin_control::{PlayerEdition, WaitingPlayer, WhitelistEntry},
    worker::runtime::ServerRuntime,
};

pub(super) async fn sync_players(
    pool: &PgPool,
    server_id: Uuid,
    players: &[WaitingPlayer],
) -> Result<()> {
    let mut tx = pool.begin().await?;
    let ids = players.iter().map(|player| player.id).collect::<Vec<_>>();
    sqlx::query(
        "DELETE FROM server_players
         WHERE server_id = $1 AND NOT (player_id = ANY($2::uuid[]))",
    )
    .bind(server_id)
    .bind(&ids)
    .execute(&mut *tx)
    .await?;
    for player in players {
        let edition = match player.edition {
            PlayerEdition::Java => "java",
            PlayerEdition::Bedrock => "bedrock",
        };
        sqlx::query(
            "INSERT INTO server_players(server_id, player_id, name, edition)
             VALUES($1,$2,$3,$4)
             ON CONFLICT(server_id, player_id) DO UPDATE
               SET name = EXCLUDED.name,
                   edition = EXCLUDED.edition,
                   last_seen_at = now()",
        )
        .bind(server_id)
        .bind(player.id)
        .bind(&player.name)
        .bind(edition)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub(super) async fn clear_players(pool: &PgPool, server_id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM server_players WHERE server_id = $1")
        .bind(server_id)
        .execute(pool)
        .await?;
    Ok(())
}

async fn desired_whitelist(
    pool: &PgPool,
    server_id: Uuid,
) -> Result<(bool, i64, i64, Vec<WhitelistEntry>)> {
    let (enabled, revision, applied_revision): (bool, i64, i64) = sqlx::query_as(
        "SELECT COALESCE((s.settings->>'whitelist')::boolean, false),
                COALESCE(w.revision, 0),
                COALESCE(w.applied_revision, -1)
         FROM servers s
         LEFT JOIN server_whitelist_state w ON w.server_id = s.id
         WHERE s.id = $1",
    )
    .bind(server_id)
    .fetch_one(pool)
    .await?;
    let entries: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT player_id, name FROM whitelist_entries
         WHERE server_id = $1 ORDER BY lower(name), player_id",
    )
    .bind(server_id)
    .fetch_all(pool)
    .await?;
    let mut by_id = entries.into_iter().collect::<BTreeMap<_, _>>();
    for owner in owner_profiles(pool, server_id).await? {
        by_id.insert(owner.id, owner.name);
    }
    Ok((
        enabled,
        revision,
        applied_revision,
        by_id
            .into_iter()
            .map(|(id, name)| WhitelistEntry { id, name })
            .collect(),
    ))
}

async fn owner_profiles(pool: &PgPool, server_id: Uuid) -> Result<Vec<WhitelistEntry>> {
    let owners: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT DISTINCT ON (i.minecraft_player_id) i.minecraft_player_id, i.display_name
         FROM servers s JOIN identities i ON i.user_id = s.owner_id
         WHERE s.id = $1 AND i.provider IN ('minecraft_java', 'minecraft_bedrock')
           AND i.minecraft_player_id IS NOT NULL
         ORDER BY i.minecraft_player_id, i.created_at, i.id",
    )
    .bind(server_id)
    .fetch_all(pool)
    .await?;
    Ok(owners
        .into_iter()
        .map(|(id, name)| WhitelistEntry { id, name })
        .collect())
}

pub(super) async fn write_operator_file(
    pool: &PgPool,
    server_id: Uuid,
    server_dir: &Path,
) -> Result<()> {
    let owners = owner_profiles(pool, server_id).await?;
    let path = server_dir.join("ops.json");
    let mut operators: Vec<serde_json::Value> = match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).context("invalid Pumpkin ops.json")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    upsert_owner_operators(&mut operators, &owners);
    let temporary = server_dir.join("ops.json.kurbis-tmp");
    tokio::fs::write(&temporary, serde_json::to_vec_pretty(&operators)?).await?;
    tokio::fs::rename(temporary, path).await?;
    Ok(())
}

fn upsert_owner_operators(operators: &mut Vec<serde_json::Value>, owners: &[WhitelistEntry]) {
    for owner in owners {
        let existing = operators.iter_mut().find(|entry| {
            entry
                .get("uuid")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.parse::<Uuid>().ok())
                == Some(owner.id)
        });
        let value = serde_json::json!({
            "uuid": owner.id,
            "name": owner.name,
            "level": 4,
            "bypasses_player_limit": true,
        });
        if let Some(existing) = existing {
            *existing = value;
        } else {
            operators.push(value);
        }
    }
}

pub(super) async fn sync_operators(
    pool: &PgPool,
    runtime: &dyn ServerRuntime,
    server_id: Uuid,
    server_dir: &Path,
    force: bool,
) -> Result<()> {
    let (revision, applied_revision): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(state.revision, 1), COALESCE(state.applied_revision, 0)
         FROM servers s LEFT JOIN server_operator_state state ON state.server_id = s.id
         WHERE s.id = $1",
    )
    .bind(server_id)
    .fetch_one(pool)
    .await?;
    if !force && applied_revision >= revision {
        return Ok(());
    }
    let owners = owner_profiles(pool, server_id).await?;
    runtime.replace_operators(server_dir, &owners).await?;
    let result = sqlx::query(
        "INSERT INTO server_operator_state(server_id, applied_revision)
         VALUES($1,$2)
         ON CONFLICT(server_id) DO UPDATE
           SET applied_revision = EXCLUDED.applied_revision
         WHERE server_operator_state.revision = EXCLUDED.applied_revision",
    )
    .bind(server_id)
    .bind(revision)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        tracing::debug!(%server_id, revision, "owner operators changed during synchronization; retrying next worker loop");
    }
    Ok(())
}

#[cfg(test)]
mod owner_operator_tests {
    use std::collections::HashSet;

    use sqlx::PgPool;

    use super::*;

    async fn test_pool() -> PgPool {
        let url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL must point to a disposable Postgres test database");
        let pool = crate::db::connect(&url)
            .await
            .expect("connect to test Postgres");
        crate::db::migrate(&pool)
            .await
            .expect("apply test database migrations");
        pool
    }

    #[tokio::test]
    async fn every_linked_minecraft_identity_is_a_level_four_operator_on_each_owned_server() {
        let pool = test_pool().await;
        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users(id, display_name) VALUES($1, 'Operator fixture')")
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();

        let server_ids = [Uuid::new_v4(), Uuid::new_v4()];
        for (index, server_id) in server_ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO servers(id, owner_id, subdomain, settings)
                 VALUES($1, $2, $3, jsonb_build_object('whitelist', true))",
            )
            .bind(server_id)
            .bind(user_id)
            .bind(format!(
                "operator-{index}-{}",
                &Uuid::new_v4().simple().to_string()[..8]
            ))
            .execute(&pool)
            .await
            .unwrap();
        }

        let linked = [
            ("minecraft_java", Uuid::new_v4(), "Java Owner"),
            ("minecraft_bedrock", Uuid::new_v4(), "Bedrock Owner"),
        ];
        for (provider, player_id, name) in linked {
            let subject = if provider == "minecraft_java" {
                player_id.to_string()
            } else {
                format!("2533274790395904-{}", player_id.simple())
            };
            sqlx::query(
                "INSERT INTO identities(id, user_id, provider, subject, display_name, minecraft_player_id)
                 VALUES($1, $2, $3, $4, $5, $6)",
            )
            .bind(Uuid::new_v4())
            .bind(user_id)
            .bind(provider)
            .bind(subject)
            .bind(name)
            .bind(player_id)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO identities(id, user_id, provider, subject, display_name)
             VALUES($1, $2, 'google', $3, 'Browser Only')",
        )
        .bind(Uuid::new_v4())
        .bind(user_id)
        .bind(format!("operator-google-{}", Uuid::new_v4()))
        .execute(&pool)
        .await
        .unwrap();

        let owners = owner_profiles(&pool, server_ids[0]).await.unwrap();
        let owner_ids = owners.iter().map(|owner| owner.id).collect::<HashSet<_>>();
        assert_eq!(owner_ids, linked.map(|(_, id, _)| id).into());

        let staging = tempfile::tempdir().unwrap();
        for (index, server_id) in server_ids.iter().enumerate() {
            let operator_revision: i64 = sqlx::query_scalar(
                "SELECT revision FROM server_operator_state WHERE server_id = $1",
            )
            .bind(server_id)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(
                operator_revision, 2,
                "each identity link schedules reconciliation"
            );

            let server_dir = staging.path().join(format!("server-{index}"));
            tokio::fs::create_dir_all(&server_dir).await.unwrap();
            tokio::fs::write(
                server_dir.join("ops.json"),
                br#"[{"uuid":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","name":"Manual Operator","level":2,"bypasses_player_limit":false}]"#,
            )
                .await
                .unwrap();
            write_whitelist_file(&pool, *server_id, &server_dir)
                .await
                .unwrap();
            let whitelist: Vec<serde_json::Value> = serde_json::from_slice(
                &tokio::fs::read(server_dir.join("whitelist.json"))
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(whitelist.len(), 2);
            assert!(owners.iter().all(|owner| {
                whitelist
                    .iter()
                    .any(|entry| entry["uuid"] == owner.id.to_string())
            }));

            write_operator_file(&pool, *server_id, &server_dir)
                .await
                .unwrap();
            let operators: Vec<serde_json::Value> = serde_json::from_slice(
                &tokio::fs::read(server_dir.join("ops.json")).await.unwrap(),
            )
            .unwrap();
            assert_eq!(operators.len(), 3);
            assert!(operators.iter().any(|operator| {
                operator["name"] == "Manual Operator"
                    && operator["level"] == 2
                    && operator["bypasses_player_limit"] == false
            }));
            for owner in &owners {
                let operator = operators
                    .iter()
                    .find(|operator| operator["uuid"] == owner.id.to_string())
                    .expect("linked Minecraft identity is present in ops.json");
                assert_eq!(operator["name"], owner.name);
                assert_eq!(operator["level"], 4);
                assert_eq!(operator["bypasses_player_limit"], true);
                assert!(operator.get("bypassesPlayerLimit").is_none());
            }
        }

        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
    }
}

pub(super) async fn write_whitelist_file(
    pool: &PgPool,
    server_id: Uuid,
    server_dir: &Path,
) -> Result<()> {
    let (_, _, _, entries) = desired_whitelist(pool, server_id).await?;
    let temporary = server_dir.join("whitelist.json.kurbis-tmp");
    tokio::fs::write(&temporary, serde_json::to_vec_pretty(&entries)?).await?;
    tokio::fs::rename(temporary, server_dir.join("whitelist.json")).await?;
    Ok(())
}

pub(super) async fn sync_whitelist(
    pool: &PgPool,
    runtime: &dyn ServerRuntime,
    server_id: Uuid,
    server_dir: &Path,
    force: bool,
) -> Result<()> {
    let (enabled, revision, applied_revision, entries) = desired_whitelist(pool, server_id).await?;
    if !force && applied_revision >= revision {
        return Ok(());
    }
    runtime
        .replace_whitelist(server_dir, enabled, &entries)
        .await?;
    let result = sqlx::query(
        "INSERT INTO server_whitelist_state(server_id, applied_revision)
         VALUES($1,$2)
         ON CONFLICT(server_id) DO UPDATE
           SET applied_revision = EXCLUDED.applied_revision
         WHERE server_whitelist_state.revision = EXCLUDED.applied_revision",
    )
    .bind(server_id)
    .bind(revision)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        tracing::debug!(%server_id, revision, "whitelist changed during synchronization; retrying next worker loop");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        sync::Mutex as StdMutex,
    };

    use anyhow::{Result, bail};
    use async_trait::async_trait;
    use serde_json::Value;
    use tokio::sync::Mutex;
    use uuid::Uuid;

    use super::{clear_players, sync_players, sync_whitelist, write_whitelist_file};
    use crate::{
        db,
        model::PlacementRecord,
        pumpkin_control::{PlayerEdition, PumpkinControl, WaitingPlayer, WhitelistEntry},
        worker::runtime::ServerRuntime,
    };

    static DB_TEST_LOCK: Mutex<()> = Mutex::const_new(());

    type WhitelistCall = (bool, Vec<(Uuid, String)>);

    struct RecordingRuntime {
        control: PumpkinControl,
        calls: StdMutex<Vec<WhitelistCall>>,
        fail: StdMutex<bool>,
    }

    impl RecordingRuntime {
        fn new() -> Self {
            Self {
                control: PumpkinControl::new("panel-state-test-token".repeat(2)),
                calls: StdMutex::new(Vec::new()),
                fail: StdMutex::new(false),
            }
        }
    }

    #[async_trait]
    impl ServerRuntime for RecordingRuntime {
        fn control(&self) -> &PumpkinControl {
            &self.control
        }

        fn socket_path(&self, _server_dir: &Path) -> Result<PathBuf> {
            Ok(PathBuf::from("unused-panel-state-test.sock"))
        }

        async fn start(
            &self,
            _server_id: Uuid,
            _placement: &PlacementRecord,
            _server_dir: &Path,
            _settings: &Value,
        ) -> Result<()> {
            Ok(())
        }

        async fn server_generation(&self, _server_id: Uuid) -> Result<Option<i64>> {
            Ok(None)
        }

        async fn logs_since(&self, _server_id: Uuid, _since: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn abort_start(&self, _server_id: Uuid) -> Result<()> {
            Ok(())
        }

        async fn replace_whitelist(
            &self,
            _server_dir: &Path,
            enabled: bool,
            entries: &[WhitelistEntry],
        ) -> Result<()> {
            if *self.fail.lock().unwrap() {
                bail!("runtime rejected whitelist update");
            }
            self.calls.lock().unwrap().push((
                enabled,
                entries
                    .iter()
                    .map(|entry| (entry.id, entry.name.clone()))
                    .collect(),
            ));
            Ok(())
        }
    }

    #[tokio::test]
    async fn presence_and_whitelist_state_cross_the_runtime_boundary() {
        let _serial = DB_TEST_LOCK.lock().await;
        let url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL must point to a disposable Postgres test database");
        let pool = db::connect(&url).await.unwrap();
        db::migrate(&pool).await.unwrap();
        let owner = Uuid::new_v4();
        let server = Uuid::new_v4();
        sqlx::query("INSERT INTO users(id, display_name) VALUES($1,'Panel State Test')")
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO identities(id, user_id, provider, subject, display_name)
             VALUES($1,$2,'google',$3,'Panel State Test')",
        )
        .bind(Uuid::new_v4())
        .bind(owner)
        .bind(owner.to_string())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO servers(id, owner_id, subdomain, settings)
             VALUES($1,$2,$3,jsonb_build_object('whitelist',true))",
        )
        .bind(server)
        .bind(owner)
        .bind(format!(
            "panel-state-{}",
            &server.simple().to_string()[..10]
        ))
        .execute(&pool)
        .await
        .unwrap();

        let java = WaitingPlayer {
            connection_id: Uuid::new_v4(),
            id: Uuid::new_v4(),
            name: "Alex".to_owned(),
            edition: PlayerEdition::Java,
            server_address: "unused".to_owned(),
            ready_for_transfer: true,
            verified_identity: None,
        };
        let bedrock = WaitingPlayer {
            connection_id: Uuid::new_v4(),
            id: Uuid::new_v4(),
            name: "Steve".to_owned(),
            edition: PlayerEdition::Bedrock,
            server_address: "unused".to_owned(),
            ready_for_transfer: true,
            verified_identity: None,
        };
        sync_players(&pool, server, &[java.clone(), bedrock.clone()])
            .await
            .unwrap();
        sync_players(&pool, server, std::slice::from_ref(&bedrock))
            .await
            .unwrap();
        let players: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT player_id, edition FROM server_players WHERE server_id=$1 ORDER BY player_id",
        )
        .bind(server)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(players, vec![(bedrock.id, "bedrock".to_owned())]);
        clear_players(&pool, server).await.unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM server_players WHERE server_id=$1")
                .bind(server)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 0);

        sqlx::query(
            "INSERT INTO whitelist_entries(server_id, player_id, name) VALUES($1,$2,'Alex')",
        )
        .bind(server)
        .bind(java.id)
        .execute(&pool)
        .await
        .unwrap();
        let revision: (i64, i64) = sqlx::query_as(
            "SELECT revision, applied_revision FROM server_whitelist_state WHERE server_id=$1",
        )
        .bind(server)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(revision, (1, 0));

        let runtime = RecordingRuntime::new();
        let root = tempfile::tempdir().unwrap();
        write_whitelist_file(&pool, server, root.path())
            .await
            .unwrap();
        let disk: Vec<Value> = serde_json::from_slice(
            &tokio::fs::read(root.path().join("whitelist.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(disk[0]["uuid"], java.id.to_string());
        assert_eq!(disk[0]["name"], "Alex");

        sync_whitelist(&pool, &runtime, server, root.path(), true)
            .await
            .unwrap();
        assert_eq!(
            runtime.calls.lock().unwrap().as_slice(),
            &[(true, vec![(java.id, "Alex".to_owned())])]
        );
        let revision: (i64, i64) = sqlx::query_as(
            "SELECT revision, applied_revision FROM server_whitelist_state WHERE server_id=$1",
        )
        .bind(server)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(revision, (1, 1));

        sqlx::query("UPDATE whitelist_entries SET name='Alex Updated' WHERE server_id=$1")
            .bind(server)
            .execute(&pool)
            .await
            .unwrap();
        *runtime.fail.lock().unwrap() = true;
        assert!(
            sync_whitelist(&pool, &runtime, server, root.path(), false)
                .await
                .is_err()
        );
        let revision: (i64, i64) = sqlx::query_as(
            "SELECT revision, applied_revision FROM server_whitelist_state WHERE server_id=$1",
        )
        .bind(server)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(revision, (2, 1));
        *runtime.fail.lock().unwrap() = false;
        sync_whitelist(&pool, &runtime, server, root.path(), false)
            .await
            .unwrap();
        let revision: (i64, i64) = sqlx::query_as(
            "SELECT revision, applied_revision FROM server_whitelist_state WHERE server_id=$1",
        )
        .bind(server)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(revision, (2, 2));

        sqlx::query("DELETE FROM users WHERE id=$1")
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
    }
}

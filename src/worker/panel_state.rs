use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    contracts::worker::WorkerServerContext,
    pumpkin_control::WhitelistEntry,
    worker::{control_plane::WorkerControl, runtime::ServerRuntime},
};

pub(super) async fn sync_players(
    control: &WorkerControl,
    server_id: Uuid,
    generation: i64,
    players: &[crate::pumpkin_control::WaitingPlayer],
) -> Result<()> {
    control
        .replace_players(server_id, generation, players)
        .await
}

pub(super) async fn clear_players(
    control: &WorkerControl,
    server_id: Uuid,
    generation: i64,
) -> Result<()> {
    control.clear_players(server_id, generation).await
}

pub(super) async fn write_whitelist_file(
    context: &WorkerServerContext,
    server_dir: &Path,
) -> Result<()> {
    let mut by_id = context
        .whitelist_entries
        .iter()
        .map(|entry| (entry.id, entry.name.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    for owner in &context.owners {
        by_id.insert(owner.id, owner.name.clone());
    }
    let entries = by_id
        .into_iter()
        .map(|(id, name)| WhitelistEntry { id, name })
        .collect::<Vec<_>>();
    write_json_atomically(&server_dir.join("data/whitelist.json"), &entries).await?;
    Ok(())
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ManagedOperatorState {
    player_ids: Vec<Uuid>,
    #[serde(default)]
    unrelated_ops: Vec<serde_json::Value>,
}

pub(super) async fn write_operator_file(
    context: &WorkerServerContext,
    server_dir: &Path,
) -> Result<()> {
    let state_path = managed_operator_state_path(server_dir);
    let previous_state = read_managed_operator_state(&state_path).await?;
    let managed = managed_operators(context);
    let current_ids = managed
        .iter()
        .map(|operator| operator.id)
        .collect::<HashSet<_>>();
    let ops_path = server_dir.join("data/ops.json");
    let mut operators = read_operator_file(&ops_path)
        .await?
        .unwrap_or_else(|| previous_state.unrelated_ops.clone());
    let previous_ids = previous_state
        .player_ids
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let unrelated_ops = operators
        .iter()
        .filter(|entry| {
            !operator_id(entry)
                .is_some_and(|id| previous_ids.contains(&id) || current_ids.contains(&id))
        })
        .cloned()
        .collect();
    reconcile_managed_operators(&mut operators, &previous_state.player_ids, &managed);

    write_json_atomically(&ops_path, &operators).await?;
    let next_state = ManagedOperatorState {
        player_ids: managed.iter().map(|operator| operator.id).collect(),
        unrelated_ops,
    };
    write_json_atomically(&state_path, &next_state).await?;
    Ok(())
}

async fn read_operator_file(path: &Path) -> Result<Option<Vec<serde_json::Value>>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn read_managed_operator_state(path: &Path) -> Result<ManagedOperatorState> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(ManagedOperatorState::default())
        }
        Err(error) => Err(error.into()),
    }
}

fn managed_operator_state_path(server_dir: &Path) -> PathBuf {
    server_dir.join("plugins/data/kurbis-control/operator-config.json")
}

async fn write_json_atomically<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .expect("managed JSON path always has a parent directory");
    tokio::fs::create_dir_all(parent).await?;
    let temporary = path.with_extension(format!(
        "{}.kurbis-tmp",
        path.extension()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("json")
    ));
    tokio::fs::write(&temporary, serde_json::to_vec_pretty(value)?).await?;
    tokio::fs::rename(temporary, path).await?;
    Ok(())
}

fn managed_operators(context: &WorkerServerContext) -> Vec<WhitelistEntry> {
    let by_id = context
        .operator_entries
        .iter()
        .chain(context.owners.iter())
        .map(|entry| (entry.id, entry.name.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    by_id
        .into_iter()
        .map(|(id, name)| WhitelistEntry { id, name })
        .collect()
}

fn reconcile_managed_operators(
    operators: &mut Vec<serde_json::Value>,
    previous_ids: &[Uuid],
    managed: &[WhitelistEntry],
) {
    let previous_ids = previous_ids.iter().copied().collect::<HashSet<_>>();
    operators.retain(|entry| !operator_id(entry).is_some_and(|id| previous_ids.contains(&id)));
    for operator in managed {
        let value = serde_json::json!({
            "uuid": operator.id,
            "name": operator.name,
            "level": 4,
            "bypasses_player_limit": true,
        });
        if let Some(existing) = operators
            .iter_mut()
            .find(|entry| operator_id(entry) == Some(operator.id))
        {
            *existing = value;
        } else {
            operators.push(value);
        }
    }
}

fn operator_id(entry: &serde_json::Value) -> Option<Uuid> {
    entry
        .get("uuid")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| value.parse().ok())
}

pub(super) async fn sync_whitelist(
    control: &WorkerControl,
    runtime: &dyn ServerRuntime,
    context: &WorkerServerContext,
    server_dir: &Path,
    force: bool,
) -> Result<()> {
    if !force && context.whitelist_applied_revision >= context.whitelist_revision {
        return Ok(());
    }
    let mut by_id = context
        .whitelist_entries
        .iter()
        .map(|entry| (entry.id, entry.name.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    for owner in &context.owners {
        by_id.insert(owner.id, owner.name.clone());
    }
    let entries = by_id
        .into_iter()
        .map(|(id, name)| WhitelistEntry { id, name })
        .collect::<Vec<_>>();
    runtime
        .replace_whitelist(server_dir, context.whitelist_enabled, &entries)
        .await?;
    control
        .mark_whitelist_applied(
            context.placement.server_id,
            context.placement.generation,
            context.whitelist_revision,
        )
        .await?;
    Ok(())
}

pub(super) async fn sync_operators(
    control: &WorkerControl,
    runtime: &dyn ServerRuntime,
    context: &WorkerServerContext,
    server_dir: &Path,
    force: bool,
) -> Result<()> {
    if !force && context.operator_applied_revision >= context.operator_revision {
        return Ok(());
    }
    runtime
        .replace_operators(server_dir, &managed_operators(context))
        .await?;
    control
        .mark_operators_applied(
            context.placement.server_id,
            context.placement.generation,
            context.operator_revision,
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PlacementRecord;
    use chrono::Utc;
    use tempfile::tempdir;

    fn context(
        owners: Vec<WhitelistEntry>,
        operator_entries: Vec<WhitelistEntry>,
    ) -> WorkerServerContext {
        WorkerServerContext {
            placement: PlacementRecord {
                server_id: Uuid::new_v4(),
                worker_id: Uuid::new_v4(),
                generation: 1,
                lease_token: Uuid::new_v4(),
                java_port: 25565,
                bedrock_port: 19132,
                signal_port: 19133,
                active: false,
                updated_at: Utc::now(),
            },
            desired_state: crate::model::DesiredState::Stopped,
            observed_state: crate::model::ObservedState::Sleeping,
            settings: serde_json::json!({}),
            current_snapshot_generation: None,
            whitelist_enabled: false,
            whitelist_revision: 0,
            whitelist_applied_revision: 0,
            whitelist_entries: Vec::new(),
            owners,
            operator_entries,
            operator_revision: 1,
            operator_applied_revision: 0,
        }
    }

    #[test]
    fn managed_roster_deduplicates_explicit_entries_and_owner_identity() {
        let owner_id = Uuid::new_v4();
        let owner = WhitelistEntry {
            id: owner_id,
            name: "Owner".to_owned(),
        };
        let managed = WhitelistEntry {
            id: owner_id,
            name: "Renamed Owner".to_owned(),
        };
        let additional = WhitelistEntry {
            id: Uuid::new_v4(),
            name: "Operator".to_owned(),
        };
        let context = context(vec![owner], vec![managed, additional.clone()]);
        let roster = managed_operators(&context);
        assert_eq!(roster.len(), 2);
        let managed_owner = roster.iter().find(|entry| entry.id == owner_id).unwrap();
        let additional_operator = roster
            .iter()
            .find(|entry| entry.id == additional.id)
            .unwrap();
        assert_eq!(managed_owner.name, "Owner");
        assert_eq!(additional_operator.name, "Operator");
    }

    #[tokio::test]
    async fn operator_file_uses_pumpkins_data_directory_and_keeps_unrelated_ops() {
        let directory = tempdir().unwrap();
        let data_directory = directory.path().join("data");
        tokio::fs::create_dir_all(&data_directory).await.unwrap();
        let manual_id = Uuid::new_v4();
        tokio::fs::write(
            data_directory.join("ops.json"),
            serde_json::to_vec(&vec![serde_json::json!({
                "uuid": manual_id,
                "name": "ManualOp",
                "level": 2,
                "bypasses_player_limit": false,
            })])
            .unwrap(),
        )
        .await
        .unwrap();
        let owner = WhitelistEntry {
            id: Uuid::new_v4(),
            name: "Owner".to_owned(),
        };
        let operator = WhitelistEntry {
            id: Uuid::new_v4(),
            name: "PanelOp".to_owned(),
        };
        let managed_context = context(vec![owner.clone()], vec![operator.clone()]);
        write_operator_file(&managed_context, directory.path())
            .await
            .unwrap();

        let operators: Vec<serde_json::Value> = serde_json::from_slice(
            &tokio::fs::read(data_directory.join("ops.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(operators.len(), 3);
        let manual = operators
            .iter()
            .find(|entry| operator_id(entry) == Some(manual_id))
            .unwrap();
        assert_eq!(manual["level"], 2);
        for managed in [owner.id, operator.id] {
            let entry = operators
                .iter()
                .find(|entry| operator_id(entry) == Some(managed))
                .unwrap();
            assert_eq!(entry["level"], 4);
            assert_eq!(entry["bypasses_player_limit"], true);
        }
        let state: ManagedOperatorState = serde_json::from_slice(
            &tokio::fs::read(managed_operator_state_path(directory.path()))
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(state.player_ids.contains(&owner.id));
        assert!(state.player_ids.contains(&operator.id));
        assert_eq!(state.unrelated_ops.len(), 1);
        assert_eq!(state.unrelated_ops[0]["uuid"], manual_id.to_string());

        let reduced_context = context(vec![owner.clone()], Vec::new());
        write_operator_file(&reduced_context, directory.path())
            .await
            .unwrap();
        let operators: Vec<serde_json::Value> = serde_json::from_slice(
            &tokio::fs::read(data_directory.join("ops.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(operators.len(), 2);
        assert!(
            operators
                .iter()
                .any(|entry| operator_id(entry) == Some(owner.id))
        );
        assert!(
            operators
                .iter()
                .any(|entry| operator_id(entry) == Some(manual_id))
        );
        assert!(
            !operators
                .iter()
                .any(|entry| operator_id(entry) == Some(operator.id))
        );
    }

    #[test]
    fn reconciliation_revokes_only_previous_managed_ops_and_preserves_manual_ops() {
        let removed_id = Uuid::new_v4();
        let manual_id = Uuid::new_v4();
        let retained_id = Uuid::new_v4();
        let mut operators = vec![
            serde_json::json!({"uuid":removed_id, "name":"Removed", "level":4}),
            serde_json::json!({"uuid":manual_id, "name":"Manual", "level":2}),
            serde_json::json!({"uuid":retained_id, "name":"Old", "level":1}),
        ];
        let desired = vec![WhitelistEntry {
            id: retained_id,
            name: "Managed".to_owned(),
        }];
        reconcile_managed_operators(&mut operators, &[removed_id, retained_id], &desired);

        assert_eq!(operators.len(), 2);
        let manual = operators
            .iter()
            .find(|entry| operator_id(entry) == Some(manual_id))
            .unwrap();
        let managed = operators
            .iter()
            .find(|entry| operator_id(entry) == Some(retained_id))
            .unwrap();
        assert_eq!(manual["name"], "Manual");
        assert_eq!(manual["level"], 2);
        assert_eq!(managed["name"], "Managed");
        assert_eq!(managed["level"], 4);
        assert_eq!(managed["bypasses_player_limit"], true);
        assert!(
            !operators
                .iter()
                .any(|entry| operator_id(entry) == Some(removed_id))
        );
    }
}

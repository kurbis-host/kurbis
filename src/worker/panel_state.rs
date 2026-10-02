use std::path::Path;

use anyhow::Result;
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
    tokio::fs::write(
        server_dir.join("whitelist.json"),
        serde_json::to_vec_pretty(&entries)?,
    )
    .await?;
    Ok(())
}

pub(super) async fn write_operator_file(
    context: &WorkerServerContext,
    server_dir: &Path,
) -> Result<()> {
    let path = server_dir.join("ops.json");
    let mut operators: Vec<serde_json::Value> = match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    upsert_owner_operators(&mut operators, &context.owners);
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
        .replace_operators(server_dir, &context.owners)
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

    #[test]
    fn owner_operator_upsert_preserves_unrelated_operators_and_replaces_owner_levels() {
        let owner = WhitelistEntry {
            id: Uuid::new_v4(),
            name: "Owner".to_owned(),
        };
        let unrelated_id = Uuid::new_v4();
        let mut operators = vec![
            serde_json::json!({"uuid": owner.id, "name":"Old", "level":1}),
            serde_json::json!({"uuid": unrelated_id, "name":"Other", "level":2}),
        ];
        upsert_owner_operators(&mut operators, std::slice::from_ref(&owner));
        assert_eq!(operators.len(), 2);
        assert_eq!(operators[0]["name"], "Owner");
        assert_eq!(operators[0]["level"], 4);
        assert_eq!(operators[1]["uuid"], unrelated_id.to_string());
    }
}

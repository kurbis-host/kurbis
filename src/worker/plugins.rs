//! Install a server's plugins into its runtime.
//!
//! The control plane's plugin list is authoritative. The worker writes each
//! enabled plugin into `plugins/installed/`, named by its id and binary, and
//! removes every other file there. Pumpkin loads only the top-level plugin
//! directory at startup, which holds the managed Kurbis plugin alone; the
//! runtime loads the installed directory when the worker asks it to, so the
//! managed plugin has always claimed its name first.

use std::{collections::BTreeMap, path::Path};

use anyhow::Result;
use uuid::Uuid;

use crate::{
    contracts::{
        plugins::{WorkerPluginReport, WorkerPluginsApplied, WorkerServerPlugin},
        worker::{WorkerAssignment, WorkerServerContext},
    },
    worker::{
        control_plane::WorkerControl,
        managed_files::{
            atomic_replace_at, ensure_dir_at, file_names_at, open_dir, remove_file_at,
        },
        runtime::ServerRuntime,
    },
};

/// Make `plugins/installed/` hold exactly the server's enabled plugins.
/// Returns why a plugin could not be written, by its id; the others are in
/// place.
pub(super) async fn write_plugin_files(
    control: &WorkerControl,
    context: &WorkerServerContext,
    server_dir: &Path,
) -> Result<BTreeMap<Uuid, String>> {
    let plugins = ensure_dir_at(
        &ensure_dir_at(&open_dir(server_dir)?, "plugins")?,
        "installed",
    )?;
    let present = file_names_at(&plugins)?;
    let wanted = context
        .plugins
        .iter()
        .map(|plugin| (plugin.file_name(), plugin))
        .collect::<BTreeMap<_, _>>();
    for name in &present {
        if !wanted.contains_key(name) {
            remove_file_at(&plugins, name)?;
        }
    }
    let assignment = WorkerAssignment::Placement {
        generation: context.placement.generation,
    };
    let mut failed = BTreeMap::new();
    for (name, plugin) in wanted {
        if present.contains(&name) {
            continue;
        }
        let written = async {
            let bytes = control
                .plugin_binary(context.placement.server_id, assignment, plugin)
                .await?;
            atomic_replace_at(&plugins, &name, &bytes, 0o644)
        }
        .await;
        if let Err(error) = written {
            tracing::warn!(
                server_id = %context.placement.server_id,
                plugin_id = %plugin.id,
                ?error,
                "failed to install plugin"
            );
            failed.insert(
                plugin.id,
                format!("Kurbis could not install the plugin: {error:#}"),
            );
        }
    }
    Ok(failed)
}

/// Install the server's plugins into its running runtime and report what the
/// runtime loaded, when the plugin list changed since the runtime last loaded
/// it or `force` asks for it.
pub(super) async fn sync_plugins(
    control: &WorkerControl,
    runtime: &dyn ServerRuntime,
    context: &WorkerServerContext,
    server_dir: &Path,
    force: bool,
) -> Result<()> {
    if !force && context.plugin_applied_revision >= context.plugin_revision {
        return Ok(());
    }
    let failed = write_plugin_files(control, context, server_dir).await?;
    let loaded = runtime.sync_plugins(server_dir).await?;
    control
        .report_plugins_applied(
            context.placement.server_id,
            &WorkerPluginsApplied {
                generation: context.placement.generation,
                revision: context.plugin_revision,
                plugins: reports(&context.plugins, failed, &loaded),
            },
        )
        .await
}

fn reports(
    plugins: &[WorkerServerPlugin],
    mut failed: BTreeMap<Uuid, String>,
    loaded: &[crate::pumpkin_control::InstalledPluginFile],
) -> Vec<WorkerPluginReport> {
    plugins
        .iter()
        .map(|plugin| {
            let name = plugin.file_name();
            let file = loaded.iter().find(|file| file.file == name);
            WorkerPluginReport {
                id: plugin.id,
                sha256: plugin.sha256.clone(),
                name: file.and_then(|file| file.name.clone()),
                version: file.and_then(|file| file.version.clone()),
                error: failed
                    .remove(&plugin.id)
                    .or_else(|| file.and_then(|file| file.error.clone()))
                    .or_else(|| {
                        file.is_none()
                            .then(|| "The server did not find the plugin file.".to_owned())
                    }),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pumpkin_control::InstalledPluginFile;

    fn plugin(sha: char) -> WorkerServerPlugin {
        WorkerServerPlugin {
            id: Uuid::new_v4(),
            sha256: sha.to_string().repeat(64),
            size_bytes: 8,
        }
    }

    #[test]
    fn reports_pair_each_plugin_with_what_the_runtime_did_with_its_file() {
        let (loaded, broken, missing, unfetched) =
            (plugin('a'), plugin('b'), plugin('c'), plugin('d'));
        let files = [
            InstalledPluginFile {
                file: loaded.file_name(),
                name: Some("tps-bar".into()),
                version: Some("0.1".into()),
                error: None,
            },
            InstalledPluginFile {
                file: broken.file_name(),
                name: None,
                version: None,
                error: Some("missing export".into()),
            },
        ];
        let failed = BTreeMap::from([(unfetched.id, "download failed".to_owned())]);
        let plugins = [loaded.clone(), broken, missing, unfetched];

        let reports = reports(&plugins, failed, &files);

        assert_eq!(reports[0].name.as_deref(), Some("tps-bar"));
        assert_eq!(reports[0].error, None);
        assert_eq!(reports[1].error.as_deref(), Some("missing export"));
        assert!(
            reports[2]
                .error
                .as_deref()
                .unwrap()
                .contains("did not find")
        );
        assert_eq!(reports[3].error.as_deref(), Some("download failed"));
        assert!(
            reports
                .iter()
                .zip(&plugins)
                .all(|(report, plugin)| report.sha256 == plugin.sha256)
        );
    }
}

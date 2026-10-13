use anyhow::{Context, Result, ensure};
use futures::StreamExt;
use uuid::Uuid;

use crate::contracts::{
    plugins::{
        MAX_PLUGIN_BYTES, PLUGIN_SHA256_HEADER, WorkerPluginsApplied, WorkerServerPlugin,
        sha256_hex,
    },
    worker::WorkerAssignment,
};

use super::super::WorkerControl;

impl WorkerControl {
    /// Download an enabled plugin's binary and check that it is the one the
    /// server's plugin list names.
    pub async fn plugin_binary(
        &self,
        server_id: Uuid,
        assignment: WorkerAssignment,
        plugin: &WorkerServerPlugin,
    ) -> Result<Vec<u8>> {
        let response = self
            .get_with_assignment(
                &format!(
                    "/internal/worker/control/servers/{server_id}/plugins/{}/binary",
                    plugin.id
                ),
                assignment,
            )
            .await?;
        ensure!(
            response
                .headers()
                .get(PLUGIN_SHA256_HEADER)
                .is_some_and(|value| value.as_bytes() == plugin.sha256.as_bytes()),
            "the control plane served another binary for plugin {}",
            plugin.id
        );
        let mut bytes = Vec::with_capacity(
            usize::try_from(plugin.size_bytes)
                .unwrap_or(0)
                .min(MAX_PLUGIN_BYTES),
        );
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("download plugin binary")?;
            ensure!(
                bytes.len() + chunk.len() <= MAX_PLUGIN_BYTES,
                "plugin {} is larger than the plugin size limit",
                plugin.id
            );
            bytes.extend_from_slice(&chunk);
        }
        ensure!(
            sha256_hex(&bytes) == plugin.sha256,
            "plugin {} does not match its checksum",
            plugin.id
        );
        Ok(bytes)
    }

    pub async fn report_plugins_applied(
        &self,
        server_id: Uuid,
        applied: &WorkerPluginsApplied,
    ) -> Result<()> {
        self.post_no_content(
            &format!("/internal/worker/control/servers/{server_id}/plugins-applied"),
            applied,
        )
        .await
    }
}

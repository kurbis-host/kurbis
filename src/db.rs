use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
const MIGRATION_MANIFEST: &str = include_str!("../migrations/MANIFEST.json");

pub async fn connect(database_url: &str) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .acquire_timeout(Duration::from_secs(10))
        .connect(database_url)
        .await?;
    Ok(pool)
}

fn verify_migration_manifest() -> Result<()> {
    let value: Value =
        serde_json::from_str(MIGRATION_MANIFEST).context("parse embedded migration manifest")?;
    let object = value
        .as_object()
        .context("embedded migration manifest is not an object")?;
    if object.get("schema").and_then(Value::as_str) != Some("kurbis.migration-manifest.v1") {
        bail!("embedded migration manifest schema is invalid");
    }
    let recorded = object
        .get("manifest_sha256")
        .and_then(Value::as_str)
        .context("embedded migration manifest hash is missing")?;
    let mut payload = BTreeMap::new();
    for key in ["migrations", "schema"] {
        payload.insert(
            key,
            object
                .get(key)
                .context("embedded migration manifest is incomplete")?,
        );
    }
    let canonical = format!("{}\n", serde_json::to_string(&payload)?);
    let calculated = format!("{:x}", Sha256::digest(canonical.as_bytes()));
    if calculated != recorded {
        bail!("embedded migration manifest hash does not match its contents");
    }
    let records = object
        .get("migrations")
        .and_then(Value::as_array)
        .context("embedded migration records are missing")?;
    if records.len() != MIGRATOR.iter().count() {
        bail!("embedded migration manifest does not match the compiled migration set");
    }
    for (record, migration) in records.iter().zip(MIGRATOR.iter()) {
        if record.get("version").and_then(Value::as_i64) != Some(migration.version)
            || record.get("sha384").and_then(Value::as_str)
                != Some(
                    &migration
                        .checksum
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>(),
                )
        {
            bail!("embedded migration manifest checksum differs from compiled migration");
        }
    }
    Ok(())
}

pub async fn migrate(pool: &PgPool) -> Result<()> {
    verify_migration_manifest()?;
    MIGRATOR.run(pool).await?;
    Ok(())
}

//! Per-server AI settings and how they resolve against gateway defaults.

use sqlx::PgExecutor;
use uuid::Uuid;

use crate::contracts::ai::{AiDefaults, AiEffort, AiSettings};

use super::{AiGateway, catalog::Model};

/// Settings of a server that never changed them.
pub fn default_settings() -> AiSettings {
    AiSettings {
        enabled: true,
        model: None,
        effort: None,
        monthly_budget_micros: None,
    }
}

#[derive(sqlx::FromRow)]
struct SettingsRow {
    enabled: bool,
    model: Option<String>,
    effort: Option<String>,
    monthly_budget_micros: Option<i64>,
}

pub async fn load(
    executor: impl PgExecutor<'_>,
    server_id: Uuid,
) -> Result<AiSettings, sqlx::Error> {
    let row: Option<SettingsRow> = sqlx::query_as(
        "SELECT enabled, model, effort, monthly_budget_micros
         FROM ai_server_settings WHERE server_id = $1",
    )
    .bind(server_id)
    .fetch_optional(executor)
    .await?;
    Ok(row.map_or_else(default_settings, |row| AiSettings {
        enabled: row.enabled,
        model: row.model,
        effort: row.effort.as_deref().and_then(AiEffort::parse),
        monthly_budget_micros: row.monthly_budget_micros,
    }))
}

pub async fn store(
    executor: impl PgExecutor<'_>,
    server_id: Uuid,
    settings: &AiSettings,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO ai_server_settings(server_id, enabled, model, effort, monthly_budget_micros)
         VALUES($1, $2, $3, $4, $5)
         ON CONFLICT(server_id) DO UPDATE SET
           enabled = EXCLUDED.enabled,
           model = EXCLUDED.model,
           effort = EXCLUDED.effort,
           monthly_budget_micros = EXCLUDED.monthly_budget_micros,
           updated_at = now()",
    )
    .bind(server_id)
    .bind(settings.enabled)
    .bind(settings.model.as_deref())
    .bind(settings.effort.map(AiEffort::as_str))
    .bind(settings.monthly_budget_micros)
    .execute(executor)
    .await?;
    Ok(())
}

/// Settings with every default filled in.
pub struct Effective<'a> {
    pub enabled: bool,
    pub model: &'a Model,
    pub effort: AiEffort,
    pub server_budget_micros: i64,
    pub account_budget_micros: i64,
}

impl AiGateway {
    pub fn defaults(&self) -> AiDefaults {
        let config = self.config();
        AiDefaults {
            model: self.catalog().default_model().id.clone(),
            effort: config.default_effort,
            server_monthly_budget_micros: config.server_monthly_budget_micros,
            account_monthly_budget_micros: config.account_monthly_budget_micros,
        }
    }

    pub fn effective(&self, settings: &AiSettings) -> Effective<'_> {
        let config = self.config();
        let catalog = self.catalog();
        Effective {
            enabled: settings.enabled,
            model: settings
                .model
                .as_deref()
                .and_then(|model| catalog.offered(model))
                .unwrap_or(catalog.default_model()),
            effort: settings.effort.unwrap_or(config.default_effort),
            // A server cap above the account cap could never be reached.
            server_budget_micros: settings
                .monthly_budget_micros
                .unwrap_or(config.server_monthly_budget_micros)
                .min(config.account_monthly_budget_micros),
            account_budget_micros: config.account_monthly_budget_micros,
        }
    }
}

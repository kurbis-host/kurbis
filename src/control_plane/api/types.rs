use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::model::{
    DEFAULT_SERVER_MOTD, DesiredState, ObservedState, OperationKind,
    OperationRecord as DbOperation, OperationStatus, SnapshotReason, SnapshotRecord as DbSnapshot,
};

use super::ApiError;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
pub enum Difficulty {
    Peaceful,
    Easy,
    Normal,
    Hard,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
pub enum GameMode {
    Survival,
    Creative,
    Adventure,
    Spectator,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ServerSettings {
    pub motd: String,
    pub difficulty: Difficulty,
    pub gamemode: GameMode,
    pub view_distance: u8,
    pub simulation_distance: u8,
    pub hardcore: bool,
    pub whitelist: bool,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            motd: DEFAULT_SERVER_MOTD.to_owned(),
            difficulty: Difficulty::Normal,
            gamemode: GameMode::Survival,
            view_distance: 12,
            simulation_distance: 8,
            hardcore: false,
            whitelist: false,
        }
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct ServerSettingsPatch {
    motd: Option<String>,
    difficulty: Option<Difficulty>,
    gamemode: Option<GameMode>,
    view_distance: Option<u8>,
    simulation_distance: Option<u8>,
    hardcore: Option<bool>,
    whitelist: Option<bool>,
}

impl ServerSettings {
    pub(super) fn validate(&self) -> Result<(), ApiError> {
        if self.motd.len() > 120 {
            return Err(ApiError::with_code(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_settings",
                "motd is too long",
            ));
        }
        if !(2..=32).contains(&self.view_distance) || !(2..=32).contains(&self.simulation_distance)
        {
            return Err(ApiError::with_code(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_settings",
                "distance must be between 2 and 32",
            ));
        }
        Ok(())
    }

    pub(super) fn merge(&mut self, patch: ServerSettingsPatch) {
        if let Some(value) = patch.motd {
            self.motd = value;
        }
        if let Some(value) = patch.difficulty {
            self.difficulty = value;
        }
        if let Some(value) = patch.gamemode {
            self.gamemode = value;
        }
        if let Some(value) = patch.view_distance {
            self.view_distance = value;
        }
        if let Some(value) = patch.simulation_distance {
            self.simulation_distance = value;
        }
        if let Some(value) = patch.hardcore {
            self.hardcore = value;
        }
        if let Some(value) = patch.whitelist {
            self.whitelist = value;
        }
    }

    pub(super) fn from_stored(value: Value) -> Result<Self, ApiError> {
        let patch: ServerSettingsPatch = serde_json::from_value(value).map_err(|error| {
            tracing::error!(?error, "stored server settings are invalid");
            ApiError::with_code(
                StatusCode::INTERNAL_SERVER_ERROR,
                "invalid_server_settings",
                "stored server settings are invalid",
            )
        })?;
        let mut settings = Self::default();
        settings.merge(patch);
        settings.validate()?;
        Ok(settings)
    }

    pub(super) fn into_stored(self) -> Value {
        serde_json::to_value(self).expect("server settings serialize")
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LifecycleStatus {
    pub desired_state: DesiredState,
    pub observed_state: ObservedState,
    pub stage: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Problem {
    pub code: &'static str,
    pub message: String,
    pub detail: Option<String>,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Operation {
    pub id: Uuid,
    pub kind: OperationKind,
    pub status: OperationStatus,
    pub generation: i64,
    pub attempt: i32,
    pub stage: Option<String>,
    pub message: Option<String>,
    pub error: Option<Problem>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<DbOperation> for Operation {
    fn from(operation: DbOperation) -> Self {
        Self {
            id: operation.id,
            kind: operation.kind,
            status: operation.status,
            generation: operation.generation,
            attempt: operation.attempt,
            stage: operation
                .payload
                .get("stage")
                .and_then(Value::as_str)
                .map(str::to_owned),
            message: operation
                .payload
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned),
            error: operation.last_error.map(|detail| Problem {
                code: "operation_failed",
                message: "The server operation failed".to_owned(),
                detail: Some(detail),
                retryable: true,
            }),
            created_at: operation.created_at,
            updated_at: operation.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Snapshot {
    pub generation: i64,
    pub size_bytes: i64,
    pub reason: SnapshotReason,
    pub created_at: DateTime<Utc>,
}

impl From<DbSnapshot> for Snapshot {
    fn from(snapshot: DbSnapshot) -> Self {
        Self {
            generation: snapshot.generation,
            size_bytes: snapshot.size_bytes,
            reason: snapshot.reason,
            created_at: snapshot.created_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StorageUsage {
    #[serde(rename = "storage_bytes")]
    pub used_bytes: i64,
    #[serde(rename = "storage_limit_bytes")]
    pub limit_bytes: i64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Server {
    pub id: Uuid,
    pub subdomain: String,
    pub is_default: bool,
    pub java_address: String,
    pub bedrock_address: String,
    pub settings: ServerSettings,
    #[serde(flatten)]
    pub lifecycle: LifecycleStatus,
    pub current_operation: Option<Operation>,
    pub latest_snapshot: Option<Snapshot>,
    pub player_count: i64,
    #[serde(flatten)]
    pub storage: StorageUsage,
    pub memory_limit_bytes: i64,
    pub generation: i64,
    pub current_snapshot_generation: Option<i64>,
    pub error: Option<Problem>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

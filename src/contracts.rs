//! Shared JSON contracts used by the control plane, gateway, and worker clients.

pub mod console {
    use chrono::{DateTime, Utc};
    use garde::Validate;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    use crate::model::ConsoleCommandStatus;

    /// A single server-console command submitted by an authorized browser actor.
    #[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Validate)]
    #[serde(deny_unknown_fields)]
    pub struct ConsoleCommandRequest {
        #[garde(skip)]
        pub request_id: Uuid,
        #[garde(length(min = 1, max = 512))]
        pub command: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct ConsoleCommandResponse {
        pub id: Uuid,
        pub server_id: Uuid,
        pub request_id: Uuid,
        pub generation: i64,
        pub attempt: i32,
        pub command: String,
        pub status: ConsoleCommandStatus,
        pub output: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub error: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub claimed_at: Option<DateTime<Utc>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub completed_at: Option<DateTime<Utc>>,
        pub created_at: DateTime<Utc>,
        pub updated_at: DateTime<Utc>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct ConsoleCommandCompletion {
        pub generation: i64,
        pub attempt: i32,
        pub succeeded: bool,
        #[serde(default)]
        pub output: Vec<String>,
        #[serde(default)]
        pub error: Option<String>,
    }
}

pub mod lifecycle {
    use garde::Validate;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    use crate::model::{ObservedState, SnapshotReason};

    /// Canonical lifecycle command wire format at
    /// `POST /api/v1/servers/{server_id}/commands`.
    #[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Validate)]
    #[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
    #[garde(allow_unvalidated)]
    pub enum ServerCommandRequest {
        Start {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[garde(skip)]
            reason: Option<String>,
        },
        Stop {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[garde(skip)]
            generation: Option<i64>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[garde(skip)]
            reason: Option<String>,
        },
        Restart,
        Snapshot {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[garde(skip)]
            generation: Option<i64>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[garde(skip)]
            reason: Option<SnapshotReason>,
        },
        Reset {
            mode: ResetMode,
        },
        Delete,
        Restore {
            generation: i64,
        },
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema, Validate)]
    #[serde(rename_all = "snake_case")]
    pub enum ResetMode {
        World,
        Full,
    }

    #[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
    pub struct CommandResponse {
        pub observed_state: ObservedState,
    }
}

pub mod worker {
    use chrono::{DateTime, Utc};
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use serde_json::Value;
    use uuid::Uuid;

    use super::lifecycle::ResetMode;
    use crate::model::{DesiredState, ObservedState, PlacementRecord, SnapshotReason};
    use crate::pumpkin_control::WhitelistEntry;

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct WorkerExportTask {
        pub id: Uuid,
        pub server_id: Uuid,
        pub source_generation: i64,
        pub attempt: i32,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct WorkerConsoleCommand {
        pub id: Uuid,
        pub server_id: Uuid,
        pub generation: i64,
        pub attempt: i32,
        pub command: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct WorkerSnapshotReservation {
        pub id: Uuid,
        pub server_id: Uuid,
        pub generation: i64,
        pub reason: SnapshotReason,
        pub created_at: DateTime<Utc>,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct WorkerRecoveryReport {
        pub operations: u64,
        pub exports: u64,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct WorkerServerState {
        pub placement: PlacementRecord,
        pub desired_state: DesiredState,
        pub observed_state: ObservedState,
        pub failed_start: bool,
        pub failed_snapshot: bool,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct WorkerServerContext {
        pub placement: PlacementRecord,
        pub desired_state: DesiredState,
        pub observed_state: ObservedState,
        pub settings: Value,
        pub current_snapshot_generation: Option<i64>,
        pub whitelist_enabled: bool,
        pub whitelist_revision: i64,
        pub whitelist_applied_revision: i64,
        pub whitelist_entries: Vec<WhitelistEntry>,
        pub owners: Vec<WhitelistEntry>,
        pub operator_entries: Vec<WhitelistEntry>,
        pub operator_revision: i64,
        pub operator_applied_revision: i64,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    pub enum WorkerAssignment {
        Placement {
            generation: i64,
        },
        Operation {
            operation_id: Uuid,
            generation: i64,
            attempt: i32,
        },
        Export {
            export_id: Uuid,
            generation: i64,
            attempt: i32,
        },
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct WorkerSnapshotCacheHint {
        pub snapshot_generation: i64,
        pub assignment: WorkerAssignment,
    }

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    pub enum OperationStage {
        #[default]
        Requested,
        Deferred,
        Starting,
        Saved,
        Stopped,
        Snapshotted,
        Restarted,
        Restored,
        BackedUp,
        Cleared,
        Initialized,
    }

    #[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
    #[serde(default, deny_unknown_fields)]
    pub struct OperationPayload {
        pub stage: OperationStage,
        pub reason: Option<String>,
        pub mode: Option<ResetMode>,
        pub was_running: Option<bool>,
        pub target_generation: Option<i64>,
        pub restore_after_stop_generation: Option<i64>,
        pub snapshot_generation: Option<i64>,
        pub snapshot_size: Option<i64>,
        pub pre_reset_generation: Option<i64>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct OperationProgress {
        pub stage: OperationStage,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub snapshot_generation: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub snapshot_size: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub pre_reset_generation: Option<i64>,
    }

    impl OperationProgress {
        pub fn stage(stage: OperationStage) -> Self {
            Self {
                stage,
                snapshot_generation: None,
                snapshot_size: None,
                pre_reset_generation: None,
            }
        }

        pub fn snapshot(stage: OperationStage, generation: i64, size: i64) -> Self {
            Self {
                stage,
                snapshot_generation: Some(generation),
                snapshot_size: Some(size),
                pre_reset_generation: None,
            }
        }

        pub fn backed_up(generation: i64) -> Self {
            Self {
                stage: OperationStage::BackedUp,
                snapshot_generation: None,
                snapshot_size: None,
                pre_reset_generation: Some(generation),
            }
        }

        pub fn valid_for(&self, kind: crate::model::OperationKind) -> bool {
            let stage_allowed = match kind {
                crate::model::OperationKind::Start => self.stage == OperationStage::Starting,
                crate::model::OperationKind::Stop | crate::model::OperationKind::Restart => {
                    matches!(
                        self.stage,
                        OperationStage::Saved
                            | OperationStage::Stopped
                            | OperationStage::Snapshotted
                    )
                }
                crate::model::OperationKind::Snapshot => matches!(
                    self.stage,
                    OperationStage::Saved
                        | OperationStage::Stopped
                        | OperationStage::Snapshotted
                        | OperationStage::Restarted
                ),
                crate::model::OperationKind::Restore => matches!(
                    self.stage,
                    OperationStage::Restored | OperationStage::Snapshotted
                ),
                crate::model::OperationKind::Reset => matches!(
                    self.stage,
                    OperationStage::Saved
                        | OperationStage::Stopped
                        | OperationStage::BackedUp
                        | OperationStage::Cleared
                        | OperationStage::Initialized
                        | OperationStage::Snapshotted
                ),
                crate::model::OperationKind::Delete => false,
            };
            let snapshot_values_match_stage = match self.stage {
                OperationStage::Snapshotted => {
                    self.snapshot_generation.is_some() && self.snapshot_size.is_some()
                }
                _ => self.snapshot_generation.is_none() && self.snapshot_size.is_none(),
            };
            let reset_value_matches_stage = match self.stage {
                OperationStage::BackedUp => self.pre_reset_generation.is_some(),
                _ => self.pre_reset_generation.is_none(),
            };
            if !stage_allowed
                || !snapshot_values_match_stage
                || !reset_value_matches_stage
                || self.snapshot_generation.is_some_and(|value| value < 1)
                || self
                    .snapshot_size
                    .is_some_and(|value| !(0..=crate::model::SERVER_STORAGE_BYTES).contains(&value))
                || self.pre_reset_generation.is_some_and(|value| value < 1)
            {
                return false;
            }
            true
        }
    }

    impl OperationPayload {
        pub fn valid_for(&self, kind: crate::model::OperationKind) -> bool {
            let stage_allowed = match kind {
                crate::model::OperationKind::Start => {
                    matches!(
                        self.stage,
                        OperationStage::Requested | OperationStage::Starting
                    )
                }
                crate::model::OperationKind::Stop | crate::model::OperationKind::Restart => {
                    matches!(
                        self.stage,
                        OperationStage::Requested
                            | OperationStage::Saved
                            | OperationStage::Stopped
                            | OperationStage::Snapshotted
                    )
                }
                crate::model::OperationKind::Snapshot => matches!(
                    self.stage,
                    OperationStage::Requested
                        | OperationStage::Deferred
                        | OperationStage::Saved
                        | OperationStage::Stopped
                        | OperationStage::Snapshotted
                        | OperationStage::Restarted
                ),
                crate::model::OperationKind::Restore => matches!(
                    self.stage,
                    OperationStage::Requested
                        | OperationStage::Restored
                        | OperationStage::Snapshotted
                ),
                crate::model::OperationKind::Reset => matches!(
                    self.stage,
                    OperationStage::Requested
                        | OperationStage::Saved
                        | OperationStage::Stopped
                        | OperationStage::BackedUp
                        | OperationStage::Cleared
                        | OperationStage::Initialized
                        | OperationStage::Snapshotted
                ),
                crate::model::OperationKind::Delete => self.stage == OperationStage::Requested,
            };
            if !stage_allowed
                || self.target_generation.is_some_and(|value| value < 1)
                || self
                    .restore_after_stop_generation
                    .is_some_and(|value| value < 1)
                || self.snapshot_generation.is_some_and(|value| value < 1)
                || self
                    .snapshot_size
                    .is_some_and(|value| !(0..=crate::model::SERVER_STORAGE_BYTES).contains(&value))
                || self.pre_reset_generation.is_some_and(|value| value < 1)
                || self.snapshot_generation.is_some() != self.snapshot_size.is_some()
            {
                return false;
            }

            let restore_values_match_kind = match kind {
                crate::model::OperationKind::Restore => {
                    self.target_generation.is_some() && self.restore_after_stop_generation.is_none()
                }
                crate::model::OperationKind::Stop => self.target_generation.is_none(),
                _ => {
                    self.target_generation.is_none() && self.restore_after_stop_generation.is_none()
                }
            };
            let reset_values_match_kind = match kind {
                crate::model::OperationKind::Reset => {
                    self.mode.is_some() && self.was_running.is_some()
                }
                _ => {
                    self.mode.is_none()
                        && self.was_running.is_none()
                        && self.pre_reset_generation.is_none()
                }
            };
            let snapshot_values_match_stage = match self.stage {
                OperationStage::Snapshotted => {
                    self.snapshot_generation.is_some() && self.snapshot_size.is_some()
                }
                OperationStage::Restarted if kind == crate::model::OperationKind::Snapshot => {
                    self.snapshot_generation.is_some() && self.snapshot_size.is_some()
                }
                _ => self.snapshot_generation.is_none() && self.snapshot_size.is_none(),
            };
            let reset_backup_matches_stage = if kind == crate::model::OperationKind::Reset {
                matches!(
                    self.stage,
                    OperationStage::BackedUp
                        | OperationStage::Cleared
                        | OperationStage::Initialized
                        | OperationStage::Snapshotted
                ) == self.pre_reset_generation.is_some()
            } else {
                true
            };
            restore_values_match_kind
                && reset_values_match_kind
                && snapshot_values_match_stage
                && reset_backup_matches_stage
        }

        /// Record the snapshot worker's failed upload after it has resumed the runtime.
        pub fn defer_snapshot_retry(&mut self) -> bool {
            if !self.valid_for(crate::model::OperationKind::Snapshot) {
                return false;
            }
            match self.stage {
                OperationStage::Requested | OperationStage::Saved | OperationStage::Stopped => {
                    self.stage = OperationStage::Deferred;
                    true
                }
                OperationStage::Deferred => true,
                _ => false,
            }
        }

        /// Reopen a deferred snapshot operation before the worker saves and stops again.
        pub fn reopen_deferred_snapshot(&mut self) -> bool {
            if !self.valid_for(crate::model::OperationKind::Snapshot) {
                return false;
            }
            match self.stage {
                OperationStage::Deferred => {
                    self.stage = OperationStage::Requested;
                    self.snapshot_generation = None;
                    self.snapshot_size = None;
                    true
                }
                OperationStage::Requested => true,
                _ => false,
            }
        }

        /// Apply one validated worker progress patch to the durable typed payload.
        pub fn merge_progress(
            &mut self,
            kind: crate::model::OperationKind,
            progress: OperationProgress,
        ) -> bool {
            if !self.valid_for(kind)
                || !progress.valid_for(kind)
                || !allows_progress_transition(kind, self.stage, progress.stage)
                || progress.snapshot_generation.is_some_and(|generation| {
                    self.snapshot_generation
                        .is_some_and(|current| current != generation)
                })
                || progress
                    .snapshot_size
                    .is_some_and(|size| self.snapshot_size.is_some_and(|current| current != size))
                || progress.pre_reset_generation.is_some_and(|generation| {
                    self.pre_reset_generation
                        .is_some_and(|current| current != generation)
                })
            {
                return false;
            }
            let mut next = self.clone();
            next.stage = progress.stage;
            if progress.snapshot_generation.is_some() {
                next.snapshot_generation = progress.snapshot_generation;
            }
            if progress.snapshot_size.is_some() {
                next.snapshot_size = progress.snapshot_size;
            }
            if progress.pre_reset_generation.is_some() {
                next.pre_reset_generation = progress.pre_reset_generation;
            }
            if !next.valid_for(kind) {
                return false;
            }
            *self = next;
            true
        }
    }

    fn allows_progress_transition(
        kind: crate::model::OperationKind,
        current: OperationStage,
        next: OperationStage,
    ) -> bool {
        if current == next {
            return true;
        }
        matches!(
            (kind, current, next),
            (
                crate::model::OperationKind::Start,
                OperationStage::Requested,
                OperationStage::Starting,
            ) | (
                crate::model::OperationKind::Stop | crate::model::OperationKind::Restart,
                OperationStage::Requested,
                OperationStage::Saved,
            ) | (
                crate::model::OperationKind::Snapshot,
                OperationStage::Requested,
                OperationStage::Saved,
            ) | (
                crate::model::OperationKind::Reset,
                OperationStage::Requested,
                OperationStage::Saved,
            ) | (
                crate::model::OperationKind::Stop
                    | crate::model::OperationKind::Restart
                    | crate::model::OperationKind::Snapshot
                    | crate::model::OperationKind::Reset,
                OperationStage::Requested | OperationStage::Saved,
                OperationStage::Stopped,
            ) | (
                crate::model::OperationKind::Stop | crate::model::OperationKind::Restart,
                OperationStage::Stopped,
                OperationStage::Snapshotted,
            ) | (
                crate::model::OperationKind::Snapshot,
                OperationStage::Requested | OperationStage::Stopped,
                OperationStage::Snapshotted,
            ) | (
                crate::model::OperationKind::Snapshot,
                OperationStage::Snapshotted,
                OperationStage::Restarted,
            ) | (
                crate::model::OperationKind::Restore,
                OperationStage::Requested,
                OperationStage::Restored,
            ) | (
                crate::model::OperationKind::Restore,
                OperationStage::Restored,
                OperationStage::Snapshotted,
            ) | (
                crate::model::OperationKind::Reset,
                OperationStage::Requested | OperationStage::Saved | OperationStage::Stopped,
                OperationStage::BackedUp,
            ) | (
                crate::model::OperationKind::Reset,
                OperationStage::BackedUp,
                OperationStage::Cleared,
            ) | (
                crate::model::OperationKind::Reset,
                OperationStage::Cleared,
                OperationStage::Initialized,
            ) | (
                crate::model::OperationKind::Reset,
                OperationStage::Initialized,
                OperationStage::Snapshotted,
            )
        )
    }

    #[cfg(test)]
    mod operation_payload_tests {
        use super::*;

        #[test]
        fn typed_operation_progress_round_trips_and_resumes_from_the_durable_stage() {
            let mut payload: OperationPayload = serde_json::from_value(serde_json::json!({
                "reason": "manual",
                "stage": "stopped"
            }))
            .unwrap();
            let progress: OperationProgress = serde_json::from_value(serde_json::json!({
                "stage": "snapshotted",
                "snapshot_generation": 7,
                "snapshot_size": 1234
            }))
            .unwrap();
            assert!(payload.merge_progress(crate::model::OperationKind::Snapshot, progress));
            let saved = serde_json::to_value(&payload).unwrap();
            let resumed: OperationPayload = serde_json::from_value(saved).unwrap();
            assert_eq!(resumed.stage, OperationStage::Snapshotted);
            assert_eq!(resumed.snapshot_generation, Some(7));
            assert_eq!(resumed.snapshot_size, Some(1234));
            assert_eq!(resumed.reason.as_deref(), Some("manual"));
        }

        #[test]
        fn progress_is_monotonic_and_snapshot_retry_uses_explicit_transitions() {
            let mut payload: OperationPayload = serde_json::from_value(serde_json::json!({
                "reason": "periodic",
                "stage": "requested"
            }))
            .unwrap();
            let kind = crate::model::OperationKind::Snapshot;
            assert!(payload.merge_progress(kind, OperationProgress::stage(OperationStage::Saved)));
            assert!(
                payload.merge_progress(kind, OperationProgress::stage(OperationStage::Stopped))
            );
            assert!(!payload.merge_progress(kind, OperationProgress::stage(OperationStage::Saved)));
            assert!(payload.defer_snapshot_retry());
            assert_eq!(payload.stage, OperationStage::Deferred);
            assert!(payload.reopen_deferred_snapshot());
            assert_eq!(payload.stage, OperationStage::Requested);
            assert_eq!(payload.reason.as_deref(), Some("periodic"));
            assert!(payload.reopen_deferred_snapshot());
        }

        #[test]
        fn valid_progress_covers_snapshot_restart_and_rejects_changed_replay_metadata() {
            let mut payload: OperationPayload = serde_json::from_value(serde_json::json!({
                "reason": "periodic",
                "stage": "requested"
            }))
            .unwrap();
            let kind = crate::model::OperationKind::Snapshot;
            for stage in [OperationStage::Saved, OperationStage::Stopped] {
                assert!(payload.merge_progress(kind, OperationProgress::stage(stage)));
            }
            let snapshot = OperationProgress::snapshot(OperationStage::Snapshotted, 7, 1234);
            assert!(snapshot.valid_for(kind));
            assert!(payload.merge_progress(kind, snapshot));
            assert!(
                payload.merge_progress(kind, OperationProgress::stage(OperationStage::Restarted))
            );
            assert!(!payload.merge_progress(
                kind,
                OperationProgress::snapshot(OperationStage::Restarted, 8, 1234)
            ));
            assert!(payload.valid_for(kind));
        }

        #[test]
        fn operation_payloads_reject_unknown_or_incomplete_persisted_shapes() {
            assert!(
                serde_json::from_value::<OperationPayload>(serde_json::json!({
                    "stage": "requested",
                    "unfenced_legacy_field": true
                }))
                .is_err()
            );
            assert!(
                !OperationPayload {
                    stage: OperationStage::Snapshotted,
                    ..OperationPayload::default()
                }
                .valid_for(crate::model::OperationKind::Snapshot)
            );
            assert!(
                !OperationProgress::stage(OperationStage::Snapshotted)
                    .valid_for(crate::model::OperationKind::Snapshot)
            );
            assert!(
                !OperationProgress::stage(OperationStage::Starting)
                    .valid_for(crate::model::OperationKind::Snapshot)
            );
        }
    }
}

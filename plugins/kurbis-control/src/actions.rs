//! The single action registry used by the command and inventory interfaces.
//!
//! Keeping the wire name, confirmation copy, and menu presentation together
//! prevents either interface from quietly growing a lifecycle action that the
//! other one cannot execute.

pub(crate) const RESTORE_PROMPT_HANDLER: &str = "restore_prompt";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LifecycleAction {
    Start,
    Stop,
    Restart,
    Snapshot,
    Restore,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ActionSpec {
    pub action: LifecycleAction,
    pub command: &'static str,
    pub argument: ActionArgument,
    pub confirmation_handler: &'static str,
    pub confirmation: &'static str,
    pub material: &'static str,
    pub label: &'static str,
    pub control_slot: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActionArgument {
    None,
    SnapshotGeneration,
}

pub(crate) const ACTIONS: &[ActionSpec] = &[
    ActionSpec {
        action: LifecycleAction::Start,
        command: "start",
        argument: ActionArgument::None,
        confirmation_handler: "start_confirm",
        confirmation: "Start this server and consume available capacity?",
        material: "lime_concrete",
        label: "Start",
        control_slot: Some(20),
    },
    ActionSpec {
        action: LifecycleAction::Stop,
        command: "stop",
        argument: ActionArgument::None,
        confirmation_handler: "stop_confirm",
        confirmation: "Stop this server and disconnect its players?",
        material: "red_concrete",
        label: "Stop",
        control_slot: Some(22),
    },
    ActionSpec {
        action: LifecycleAction::Restart,
        command: "restart",
        argument: ActionArgument::None,
        confirmation_handler: "restart_confirm",
        confirmation: "Restart this server and disconnect its players?",
        material: "orange_concrete",
        label: "Restart",
        control_slot: Some(24),
    },
    ActionSpec {
        action: LifecycleAction::Snapshot,
        command: "snapshot",
        argument: ActionArgument::None,
        confirmation_handler: "snapshot_confirm",
        confirmation: "Create a snapshot of this server now?",
        material: "chest",
        label: "Create snapshot",
        control_slot: Some(31),
    },
    ActionSpec {
        action: LifecycleAction::Restore,
        command: "restore",
        argument: ActionArgument::SnapshotGeneration,
        confirmation_handler: "restore_confirm",
        confirmation: "Replace the current world with the selected snapshot?",
        material: "barrier",
        label: "Restore snapshot",
        control_slot: None,
    },
];

pub(crate) fn spec(action: LifecycleAction) -> &'static ActionSpec {
    ACTIONS
        .iter()
        .find(|candidate| candidate.action == action)
        .expect("lifecycle action registry is complete")
}

pub(crate) fn by_command(command: &str) -> Option<LifecycleAction> {
    ACTIONS
        .iter()
        .find(|candidate| candidate.command == command)
        .map(|candidate| candidate.action)
}

pub(crate) fn by_confirmation_handler(handler: &str) -> Option<LifecycleAction> {
    ACTIONS
        .iter()
        .find(|candidate| candidate.confirmation_handler == handler)
        .map(|candidate| candidate.action)
}

pub(crate) fn prompted_actions() -> impl Iterator<Item = &'static ActionSpec> {
    ACTIONS
        .iter()
        .filter(|candidate| candidate.control_slot.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_keys_and_control_slots_are_unique() {
        for (index, action) in ACTIONS.iter().enumerate() {
            assert!(
                ACTIONS[index + 1..]
                    .iter()
                    .all(|other| other.command != action.command)
            );
            assert!(
                ACTIONS[index + 1..]
                    .iter()
                    .all(|other| other.confirmation_handler != action.confirmation_handler)
            );
            if let Some(slot) = action.control_slot {
                assert!(
                    ACTIONS[index + 1..]
                        .iter()
                        .all(|other| other.control_slot != Some(slot))
                );
            }
            assert_eq!(by_command(action.command), Some(action.action));
        }
        assert_eq!(
            prompted_actions()
                .map(|action| action.command)
                .collect::<Vec<_>>(),
            ["start", "stop", "restart", "snapshot"],
        );
        assert_eq!(
            spec(LifecycleAction::Restore).argument,
            ActionArgument::SnapshotGeneration
        );
        assert!(prompted_actions().all(|action| {
            action.argument == ActionArgument::None && action.control_slot.is_some()
        }));
    }
}

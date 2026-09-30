//! Lifecycle helpers for plugins running on the pinned official Pumpkin release.
use pumpkin_plugin_api::{Context, permission::Permission};

/// Register a permission once across plugin reloads.
///
/// The stock release retains the permission registry when unloading plugins.
/// Only the exact duplicate-node error is accepted; namespace and other errors
/// still fail initialization. Existing defaults remain in force until restart.
pub fn register_permission(context: &Context, permission: &Permission) -> Result<(), String> {
    match context.register_permission(permission) {
        Err(error) if error == format!("Permission {} is already registered", permission.node) => {
            Ok(())
        }
        result => result,
    }
}

use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};

use pumpkin_plugin_api::{
    Context, Server,
    events::{EventData, EventHandler, PlayerJoinEvent},
    scheduler::SchedulerExt,
    text::TextComponent,
};
use serde_json::Value;

use crate::api;

const FIRST_REFRESH_TICKS: u64 = 1;
const REFRESH_TICKS: u64 = 40;
const TAB_HEADER: &str = "§6§lKurbis Server";
const EMPTY_TEXT: &str = "";

static TASK_STARTED: OnceLock<()> = OnceLock::new();
static REFRESH_RUNNING: AtomicBool = AtomicBool::new(false);
static PUBLISHED: OnceLock<Mutex<Option<TabSnapshot>>> = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq)]
struct TabSnapshot {
    header: String,
    footer: String,
}

fn published() -> &'static Mutex<Option<TabSnapshot>> {
    PUBLISHED.get_or_init(|| Mutex::new(None))
}

/// Starts the single bounded stats poller for this plugin instance.
pub(crate) fn start(context: &Context) {
    if TASK_STARTED.set(()).is_err() {
        return;
    }

    context.schedule_repeating_task(FIRST_REFRESH_TICKS, REFRESH_TICKS, refresh_task);
}

fn refresh_task(server: Server) {
    if REFRESH_RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    refresh_now_inner(&server);
    REFRESH_RUNNING.store(false, Ordering::Release);
}

/// Refreshes the tab list immediately after an operator changes the setting.
///
/// The call is guarded so a command or menu click cannot overlap the scheduled poll.
pub(crate) fn refresh_now(server: &Server) {
    if REFRESH_RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    refresh_now_inner(server);
    REFRESH_RUNNING.store(false, Ordering::Release);
}

fn refresh_now_inner(server: &Server) {
    let snapshot = match tab_stats_enabled() {
        Ok(true) => Some(render_snapshot(server)),
        Ok(false) | Err(()) => None,
    };
    publish(server, snapshot);
}

fn tab_stats_enabled() -> Result<bool, ()> {
    let value = api::get(&api::server_path("").map_err(|_| ())?).map_err(|_| ())?;
    Ok(value
        .get("settings")
        .and_then(|settings| settings.get("tab_stats"))
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

fn render_snapshot(server: &Server) -> TabSnapshot {
    // Host-only values come from the server-scoped API. Live TPS and player
    // count come directly from Pumpkin; unavailable resource data stays
    // unavailable instead of being replaced with host-wide guesses.
    let runtime = api::server_path("/runtime-stats")
        .ok()
        .and_then(|path| api::get(&path).ok())
        .and_then(runtime_payload);
    let cpu = runtime
        .as_ref()
        .and_then(|value| percentage(value, "cpu_percent"))
        .unwrap_or_else(|| unavailable());
    let memory = runtime
        .as_ref()
        .and_then(|value| bytes_pair(value, "memory_used_bytes", "memory_limit_bytes"))
        .unwrap_or_else(|| unavailable());
    let disk = runtime
        .as_ref()
        .and_then(|value| bytes_pair(value, "disk_used_bytes", "disk_limit_bytes"))
        .unwrap_or_else(|| unavailable());
    let tps = finite_nonnegative(server.get_tps())
        .or_else(|| runtime.as_ref().and_then(|value| number(value, "tps")))
        .and_then(finite_nonnegative)
        .map_or_else(unavailable, |value| format!("{value:.1}"));
    let players = server.get_player_count();

    TabSnapshot {
        header: TAB_HEADER.to_owned(),
        footer: format!(
            "§7CPU §f{cpu} §8| §7RAM §f{memory} §8| §7Disk §f{disk}\n§7TPS §f{tps} §8| §7Players §f{players}"
        ),
    }
}

fn runtime_payload(value: Value) -> Option<Value> {
    if value.is_null() {
        return None;
    }
    // Accept the current raw RuntimeStats response and the older host-control
    // envelope without allowing arbitrary nested values to panic the renderer.
    value
        .get("stats")
        .filter(|stats| stats.is_object())
        .cloned()
        .or_else(|| value.is_object().then_some(value))
}

fn publish(server: &Server, next: Option<TabSnapshot>) {
    let changed = {
        let mut current = published()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *current == next {
            false
        } else {
            *current = next.clone();
            true
        }
    };
    if !changed {
        return;
    }

    match next {
        Some(snapshot) => server.broadcast_tab_list_header_footer(
            TextComponent::from_legacy_string(&snapshot.header),
            TextComponent::from_legacy_string(&snapshot.footer),
        ),
        None => clear(server),
    }
}

fn clear(server: &Server) {
    server.broadcast_tab_list_header_footer(
        TextComponent::text(EMPTY_TEXT),
        TextComponent::text(EMPTY_TEXT),
    );
}

fn unavailable() -> String {
    "—".to_owned()
}

fn percentage(value: &Value, key: &str) -> Option<String> {
    finite_nonnegative(number(value, key)?).map(|value| format!("{:.0}%", value.clamp(0.0, 100.0)))
}

fn bytes_pair(value: &Value, used_key: &str, limit_key: &str) -> Option<String> {
    bytes_pair_from_options(
        value.get(used_key).and_then(as_u64),
        value.get(limit_key).and_then(as_u64),
    )
}

fn bytes_pair_from_options(used: Option<u64>, limit: Option<u64>) -> Option<String> {
    let used = used?;
    let used_display = format_gib(used);
    match limit {
        Some(limit) if limit >= used => Some(format!("{used_display}/{} GB", format_gib(limit))),
        Some(_) => None,
        None => Some(format!("{used_display} GB")),
    }
}

fn format_gib(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1024.0 / 1024.0 / 1024.0)
}

fn as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(value) => value.as_u64(),
        Value::String(value) => value.trim().parse().ok(),
        _ => None,
    }
}

fn number(value: &Value, key: &str) -> Option<f64> {
    let value = value.get(key)?;
    let number = match value {
        Value::Number(value) => value.as_f64()?,
        Value::String(value) => value.trim().parse().ok()?,
        _ => return None,
    };
    number.is_finite().then_some(number)
}

fn finite_nonnegative(value: f64) -> Option<f64> {
    (value.is_finite() && value >= 0.0).then_some(value)
}

pub(crate) struct Join;

impl EventHandler<PlayerJoinEvent> for Join {
    fn handle(
        &self,
        _server: Server,
        event: EventData<PlayerJoinEvent>,
    ) -> EventData<PlayerJoinEvent> {
        let snapshot = published()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(snapshot) = snapshot {
            event.player.set_tab_list_header_footer(
                TextComponent::from_legacy_string(&snapshot.header),
                TextComponent::from_legacy_string(&snapshot.footer),
            );
        }
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_missing_and_malformed_resource_values_as_unavailable() {
        let value = serde_json::json!({
            "cpu_percent": "not-a-number",
            "memory_used_bytes": 1024,
            "disk_used_bytes": -1,
            "tps": null,
        });
        assert_eq!(percentage(&value, "cpu_percent"), None);
        assert_eq!(
            bytes_pair(&value, "memory_used_bytes", "memory_limit_bytes"),
            Some("0.0 GB".into())
        );
        assert_eq!(
            bytes_pair(&value, "disk_used_bytes", "disk_limit_bytes"),
            None
        );
        assert_eq!(number(&value, "tps"), None);
    }

    #[test]
    fn clamps_cpu_percent_without_accepting_negative_or_non_finite_values() {
        assert_eq!(
            percentage(&serde_json::json!({"cpu": 32.4}), "cpu"),
            Some("32%".into())
        );
        assert_eq!(
            percentage(&serde_json::json!({"cpu": 120}), "cpu"),
            Some("100%".into())
        );
        assert_eq!(percentage(&serde_json::json!({"cpu": -1}), "cpu"), None);
    }

    #[test]
    fn formats_resource_pairs_in_gib_and_rejects_impossible_limits() {
        let value = serde_json::json!({
            "disk_used_bytes": 1_073_741_824_u64,
            "disk_limit_bytes": 5_368_709_120_u64,
        });
        assert_eq!(
            bytes_pair(&value, "disk_used_bytes", "disk_limit_bytes"),
            Some("1.0/5.0 GB".into())
        );
        assert_eq!(bytes_pair_from_options(Some(8), Some(4)), None);
    }

    #[test]
    fn accepts_raw_and_enveloped_runtime_payloads_but_not_null() {
        let raw = serde_json::json!({"cpu_percent": 10});
        let envelope = serde_json::json!({"stats": {"cpu_percent": 20}});
        assert_eq!(
            runtime_payload(raw)
                .as_ref()
                .and_then(|v| number(v, "cpu_percent")),
            Some(10.0)
        );
        assert_eq!(
            runtime_payload(envelope)
                .as_ref()
                .and_then(|v| number(v, "cpu_percent")),
            Some(20.0)
        );
        assert!(runtime_payload(Value::Null).is_none());
    }

    #[test]
    fn snapshots_are_cleared_when_disabled_state_is_published() {
        let mut current = Some(TabSnapshot {
            header: TAB_HEADER.into(),
            footer: "stats".into(),
        });
        assert!(current.is_some());
        current = None;
        assert!(current.is_none());
    }
}

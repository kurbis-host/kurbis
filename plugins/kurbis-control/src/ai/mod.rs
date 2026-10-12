//! `/ai`: operators ask the Kurbis AI gateway to act on this server.
//!
//! A request becomes a job on the control plane. While jobs are active the
//! pump syncs with the gateway: it runs the tool calls the model asks for,
//! returns their results and shows progress lines and answers to the operator
//! who asked. `/ai` alone opens the assistant's page of the server menu.

pub(crate) mod records;
mod tools;

use std::{
    collections::HashMap,
    sync::{
        Mutex, MutexGuard, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use pumpkin_plugin_api::{
    Context, Player, Server,
    command::{
        ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType,
    },
    commands::CommandHandler,
    common::NamedColor,
    logging::{LogLevel, log},
    scheduler::SchedulerExt,
    text::TextComponent,
    uuid as wit_uuid,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    api,
    command::arg,
    menu::{self, View},
};
use records::Overview;

/// Ticks between syncs while a request is active (20 ticks per second).
const SYNC_TICKS: u64 = 10;
/// Exchanges in one run: results go back right after their calls ran.
const MAX_SYNCS_PER_RUN: usize = 3;
/// The gateway returns at most this many chat lines per sync.
const EVENT_PAGE: usize = 32;
/// The gateway accepts at most this many tool results per sync.
const MAX_SYNC_RESULTS: usize = 8;
/// Serialized results per sync; the rest of the client's request cap is left
/// for the cursor and the JSON framing. One result always fits: tools cap
/// their output at `tools::MAX_OUTPUT_BYTES`, which JSON escaping at most
/// doubles.
const MAX_SYNC_RESULT_BYTES: usize = api::MAX_REQUEST_BYTES - 1024;
/// The gateway answers a call itself once its result is this late and ignores
/// the plugin's afterwards, so older results are dropped instead of resent.
const RESULT_LIFETIME: Duration = Duration::from_secs(30);
const MAX_BACKOFF_RUNS: u32 = 64;
/// The gateway's prompt limit, checked here for a friendlier message.
const MAX_PROMPT_CHARS: usize = 2000;

static PUMP_RUNNING: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct State {
    /// Read position in this server's chat lines; `None` before the first sync.
    cursor: Option<i64>,
    /// The active request of each operator, as last reported by the gateway.
    active: HashMap<Uuid, Uuid>,
    /// Set when something changed that the next sync must pick up.
    dirty: bool,
    /// Tool results the gateway has not acknowledged yet, oldest first.
    results: Vec<ToolResult>,
    /// Runs left to skip after failed syncs, and how many failed in a row.
    backoff: u32,
    failures: u32,
}

fn lock_state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug, Clone, Serialize)]
struct ToolResult {
    job_id: Uuid,
    call_id: String,
    output: String,
    is_error: bool,
    #[serde(skip)]
    finished: Instant,
}

#[derive(Debug, Deserialize)]
struct SyncResponse {
    calls: Vec<ToolCall>,
    events: Vec<ChatLine>,
    cursor: i64,
    active_jobs: Vec<ActiveJob>,
}

#[derive(Debug, Deserialize)]
struct ToolCall {
    job_id: Uuid,
    call_id: String,
    operator_id: Uuid,
    name: String,
    input: Value,
}

#[derive(Debug, Deserialize)]
struct ChatLine {
    operator_id: Uuid,
    kind: LineKind,
    message: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LineKind {
    Progress,
    Answer,
    Error,
}

#[derive(Debug, Deserialize)]
struct ActiveJob {
    job_id: Uuid,
    operator_id: Uuid,
}

pub(crate) fn command() -> Command {
    Command::new(
        &["ai".into()],
        "Ask the Kurbis AI assistant to act on this server.",
    )
    .execute(Handler::Menu)
    .then(CommandNode::literal("ask").then(prompt(Handler::Ask)))
    .then(CommandNode::literal("new").then(prompt(Handler::New)))
    .then(CommandNode::literal("cancel").execute(Handler::Cancel))
    .then(CommandNode::literal("status").execute(Handler::Status))
}

fn prompt(handler: Handler) -> CommandNode {
    CommandNode::argument("prompt", &ArgumentType::String(StringType::Greedy)).execute(handler)
}

/// Start the pump; its first run reads where this server's chat lines end
/// and picks up requests that were active before a restart.
pub(crate) fn start(context: &Context) {
    lock_state().dirty = true;
    context.schedule_repeating_task(1, SYNC_TICKS, |server| {
        if PUMP_RUNNING.swap(true, Ordering::AcqRel) {
            return;
        }
        pump(&server);
        PUMP_RUNNING.store(false, Ordering::Release);
    });
}

fn pump(server: &Server) {
    {
        let mut state = lock_state();
        if state.backoff > 0 {
            state.backoff -= 1;
            return;
        }
        if !state.dirty && state.active.is_empty() && state.results.is_empty() {
            return;
        }
    }
    for _ in 0..MAX_SYNCS_PER_RUN {
        match sync(server) {
            Ok(true) => {}
            Ok(false) => break,
            Err(error) => {
                let mut state = lock_state();
                state.failures = state.failures.saturating_add(1);
                state.backoff = 2u32.saturating_pow(state.failures).min(MAX_BACKOFF_RUNS);
                log(LogLevel::Warn, &format!("Kurbis AI sync failed: {error:#}"));
                break;
            }
        }
    }
}

/// One exchange with the gateway. Returns whether another should follow at
/// once, because results are waiting or more chat lines are queued.
fn sync(server: &Server) -> Result<bool> {
    let (results, cursor) = {
        let mut state = lock_state();
        state.dirty = false;
        (next_batch(&mut state.results), state.cursor)
    };
    let response = api::post(
        &api::server_path("/ai/sync")?,
        &json!({"results": results, "after_event_id": cursor}),
    )
    .and_then(|value| {
        serde_json::from_value::<SyncResponse>(value).context("invalid AI sync response")
    });
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            // Returning a result twice is harmless; losing it is not.
            let mut state = lock_state();
            state.dirty = true;
            state.results.splice(0..0, results);
            return Err(error);
        }
    };
    for line in &response.events {
        if let Some(player) = online_player(server, line.operator_id) {
            show(&player, line.kind, &line.message);
        }
    }
    {
        let mut state = lock_state();
        state.cursor = Some(response.cursor);
        state.failures = 0;
        state.active = response
            .active_jobs
            .iter()
            .map(|job| (job.operator_id, job.job_id))
            .collect();
    }
    let more_lines = response.events.len() >= EVENT_PAGE;
    let results = response
        .calls
        .into_iter()
        .map(|call| {
            let (output, is_error) = tools::run(server, call.operator_id, &call.name, &call.input);
            ToolResult {
                job_id: call.job_id,
                call_id: call.call_id,
                output,
                is_error,
                finished: Instant::now(),
            }
        })
        .collect::<Vec<_>>();
    let mut state = lock_state();
    state.results.extend(results);
    Ok(more_lines || !state.results.is_empty())
}

/// Take the oldest results that fit one sync request, after dropping those
/// the gateway no longer waits for.
fn next_batch(results: &mut Vec<ToolResult>) -> Vec<ToolResult> {
    results.retain(|result| result.finished.elapsed() < RESULT_LIFETIME);
    let mut bytes = 0;
    let mut count = 0;
    for result in results.iter().take(MAX_SYNC_RESULTS) {
        let size = serde_json::to_vec(result).map_or(usize::MAX, |json| json.len());
        if count > 0 && bytes + size > MAX_SYNC_RESULT_BYTES {
            break;
        }
        bytes += size;
        count += 1;
    }
    results.drain(..count).collect()
}

#[derive(Debug, Clone, Copy)]
enum Handler {
    Menu,
    Ask,
    New,
    Cancel,
    Status,
}

impl CommandHandler for Handler {
    fn handle(
        &self,
        sender: CommandSender,
        _server: Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let run = || -> Result<()> {
            let player = sender
                .as_player()
                .context("Only players can use /ai: answers go to the operator who asked.")?;
            // Any command ends a pending chat prompt from the menu.
            menu::forget_text_input(&player);
            match self {
                Self::Menu => menu::open(&player, View::Ai)?,
                Self::Ask => ask(&player, &arg(&args, "prompt")?, false)?,
                Self::New => ask(&player, &arg(&args, "prompt")?, true)?,
                Self::Cancel => cancel(&player)?,
                Self::Status => status(&player)?,
            }
            Ok(())
        };
        run().map(|()| 1).map_err(|error| {
            CommandError::CommandFailed(TextComponent::text(&format!("AI: {error:#}")))
        })
    }
}

pub(crate) fn ask(player: &Player, prompt: &str, new_conversation: bool) -> Result<()> {
    let prompt = prompt.trim();
    anyhow::ensure!(!prompt.is_empty(), "Ask a question after the command.");
    anyhow::ensure!(
        prompt.chars().count() <= MAX_PROMPT_CHARS,
        "Keep the question under {MAX_PROMPT_CHARS} characters."
    );
    let operator_id = player_id(player)?;
    let job_id = Uuid::parse_str(&wit_uuid::to_string(wit_uuid::generate()))?;
    api::post(
        &api::server_path("/ai/jobs")?,
        &json!({
            "job_id": job_id,
            "operator_id": operator_id,
            "operator_name": player.get_name(),
            "prompt": prompt,
            "new_conversation": new_conversation,
        }),
    )?;
    {
        let mut state = lock_state();
        state.active.insert(operator_id, job_id);
        state.dirty = true;
    }
    show(player, LineKind::Progress, "Thinking…");
    Ok(())
}

fn cancel(player: &Player) -> Result<()> {
    let operator_id = player_id(player)?;
    let job_id = lock_state()
        .active
        .get(&operator_id)
        .copied()
        .context("You have no AI request running.")?;
    cancel_job(job_id)
}

/// Cancel any request on this server. The operator who asked is told with
/// the next sync.
pub(crate) fn cancel_job(job_id: Uuid) -> Result<()> {
    api::post_empty(&api::server_path(&format!("/ai/jobs/{job_id}/cancel"))?)?;
    lock_state().dirty = true;
    Ok(())
}

fn overview() -> Result<Overview> {
    serde_json::from_value(api::get(&api::server_path("/ai")?)?).context("invalid AI overview")
}

fn status(player: &Player) -> Result<()> {
    for line in status_lines(&overview()?, player_id(player)?) {
        player.send_system_message(TextComponent::text(&line), false);
    }
    Ok(())
}

fn status_lines(overview: &Overview, operator_id: Uuid) -> Vec<String> {
    if let Some(reason) = overview.unavailable_reason() {
        return vec![reason.to_owned()];
    }
    let current = overview
        .jobs
        .iter()
        .find(|job| job.operator_id == operator_id && job.state.is_active());
    vec![
        format!("AI assistant: {}", overview.model_name()),
        format!(
            "This month: {} for this server, {} for the owner's account.",
            overview.budget(&overview.usage.server),
            overview.budget(&overview.usage.account)
        ),
        match current {
            Some(job) => format!("Your request \"{}\" is in progress.", job.prompt),
            None => "You have no request running.".into(),
        },
    ]
}

pub(crate) fn show(player: &Player, kind: LineKind, message: &str) {
    let (color, italic) = match kind {
        LineKind::Progress => (NamedColor::Gray, true),
        LineKind::Answer => (NamedColor::White, false),
        LineKind::Error => (NamedColor::Red, false),
    };
    let line = TextComponent::text("[AI] ")
        .color_named(NamedColor::Aqua)
        .add_child(
            TextComponent::text(message)
                .color_named(color)
                .italic(italic),
        );
    player.send_system_message(line, false);
}

fn player_id(player: &Player) -> Result<Uuid> {
    Uuid::parse_str(&wit_uuid::to_string(player.get_id())).context("player has an invalid UUID")
}

fn online_player(server: &Server, id: Uuid) -> Option<Player> {
    wit_uuid::parse(&id.to_string()).and_then(|id| server.get_player_by_uuid(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_names_the_model_budgets_and_the_running_request() {
        let operator = Uuid::from_u128(1);
        let mut overview = records::tests::overview();
        assert_eq!(
            status_lines(&overview, operator),
            vec![
                "AI assistant: Claude Opus 5.5",
                "This month: $1.25 of $5.00 for this server, $2.00 of $20.00 for the owner's account.",
                "Your request \"make it day\" is in progress.",
            ]
        );
        assert_eq!(
            status_lines(&overview, Uuid::from_u128(2))[2],
            "You have no request running."
        );
        overview.available = false;
        assert_eq!(
            status_lines(&overview, operator),
            vec!["The AI assistant is not set up on this Kurbis installation."]
        );
    }

    fn result(output: &str, age: Duration) -> ToolResult {
        ToolResult {
            job_id: Uuid::nil(),
            call_id: format!("toolu_{}", output.len()),
            output: output.to_owned(),
            is_error: false,
            finished: Instant::now() - age,
        }
    }

    #[test]
    fn batches_respect_the_gateway_limits_and_drop_stale_results() {
        let mut results = (0..10)
            .map(|_| result("ok", Duration::ZERO))
            .collect::<Vec<_>>();
        assert_eq!(next_batch(&mut results).len(), MAX_SYNC_RESULTS);
        assert_eq!(results.len(), 2);

        // Worst-case outputs double when escaped; one still fits alone.
        let escaped = "\\".repeat(tools::MAX_OUTPUT_BYTES);
        let mut results = vec![
            result(&escaped, Duration::ZERO),
            result(&escaped, Duration::ZERO),
            result("ok", Duration::ZERO),
        ];
        let batch = next_batch(&mut results);
        assert_eq!(batch.len(), 1);
        assert!(serde_json::to_vec(&batch).unwrap().len() <= MAX_SYNC_RESULT_BYTES);
        assert_eq!(next_batch(&mut results).len(), 2);

        let mut results = vec![
            result("late", RESULT_LIFETIME),
            result("fresh", Duration::ZERO),
        ];
        let batch = next_batch(&mut results);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].output, "fresh");
        assert!(results.is_empty());
    }

    #[test]
    fn sync_responses_parse_the_gateway_contract() {
        let response: SyncResponse = serde_json::from_value(json!({
            "calls": [{
                "job_id": "00000000-0000-0000-0000-000000000003",
                "call_id": "toolu_1",
                "operator_id": "00000000-0000-0000-0000-000000000001",
                "name": "list_players",
                "input": {},
            }],
            "events": [{
                "id": 7,
                "job_id": "00000000-0000-0000-0000-000000000003",
                "operator_id": "00000000-0000-0000-0000-000000000001",
                "kind": "progress",
                "message": "Checking who is online.",
            }],
            "cursor": 7,
            "active_jobs": [{
                "job_id": "00000000-0000-0000-0000-000000000003",
                "operator_id": "00000000-0000-0000-0000-000000000001",
                "state": "awaiting_tools",
            }],
        }))
        .unwrap();
        assert_eq!(response.calls[0].name, "list_players");
        assert!(matches!(response.events[0].kind, LineKind::Progress));
        assert_eq!(response.cursor, 7);
        assert_eq!(response.active_jobs.len(), 1);
    }
}

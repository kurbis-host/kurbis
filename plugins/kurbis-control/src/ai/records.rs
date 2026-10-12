//! The gateway's overview and requests as the plugin reads them, and how they
//! read in game. Field names follow the control plane's AI contract; fields
//! the plugin does not show are left out.

use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub(crate) struct Overview {
    /// Whether the control plane has a model provider configured.
    pub available: bool,
    pub settings: Settings,
    pub defaults: Option<Defaults>,
    pub models: Vec<Model>,
    pub usage: Usage,
    /// The latest requests on this server, newest first.
    pub jobs: Vec<Job>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Settings {
    pub enabled: bool,
    pub model: Option<String>,
    pub effort: Option<Effort>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Defaults {
    pub model: String,
    pub effort: Effort,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Effort {
    Low,
    Medium,
    High,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Model {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Usage {
    pub period_start: String,
    pub server: Budget,
    pub account: Budget,
    pub model_calls: i64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Budget {
    pub spent_micros: i64,
    /// Worst-case cost of model calls still in flight.
    pub reserved_micros: i64,
    pub budget_micros: i64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Job {
    pub id: Uuid,
    pub operator_id: Uuid,
    pub operator_name: String,
    pub prompt: String,
    pub model: String,
    pub state: JobState,
    pub answer: Option<String>,
    pub error: Option<String>,
    pub model_calls: i32,
    pub tool_calls: i32,
    pub cost_micros: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum JobState {
    Queued,
    Running,
    AwaitingTools,
    Succeeded,
    Failed,
    Cancelled,
}

/// One request with every tool call it made.
#[derive(Debug, Deserialize)]
pub(crate) struct JobDetail {
    pub job: Job,
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ToolCall {
    pub name: String,
    pub input: Value,
    pub state: ToolCallState,
    pub output: Option<String>,
    pub is_error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolCallState {
    Pending,
    Delivered,
    Completed,
}

impl Overview {
    /// The model new requests use: the server's choice while the gateway
    /// still offers it, otherwise the gateway default.
    pub fn model_name(&self) -> &str {
        let chosen = self
            .settings
            .model
            .as_deref()
            .filter(|id| self.models.iter().any(|model| model.id == *id));
        let default = self
            .defaults
            .as_ref()
            .map(|defaults| defaults.model.as_str());
        self.named(chosen.or(default).unwrap_or_default())
    }

    /// The display name of a model, or its id when the gateway no longer
    /// offers it.
    pub fn named<'a>(&'a self, id: &'a str) -> &'a str {
        self.models
            .iter()
            .find(|model| model.id == id)
            .map_or(id, |model| model.name.as_str())
    }

    pub fn effort(&self) -> Option<Effort> {
        self.settings
            .effort
            .or(self.defaults.as_ref().map(|defaults| defaults.effort))
    }

    /// Whether operators can start requests right now.
    pub fn accepts_requests(&self) -> bool {
        self.available && self.settings.enabled
    }

    /// Why requests cannot start, when they cannot.
    pub fn unavailable_reason(&self) -> Option<&'static str> {
        if !self.available {
            Some("The AI assistant is not set up on this Kurbis installation.")
        } else if !self.settings.enabled {
            Some("The AI assistant is turned off for this server.")
        } else {
            None
        }
    }

    /// A budget as "$1.25 of $5.00", counting what running requests hold.
    /// Without a provider there are no caps, only past spending.
    pub fn budget(&self, budget: &Budget) -> String {
        let used = usd(budget.spent_micros + budget.reserved_micros);
        if self.available {
            format!("{used} of {}", usd(budget.budget_micros))
        } else {
            format!("{used} spent")
        }
    }
}

impl Effort {
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }
}

impl JobState {
    /// Active requests can still change and be cancelled.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::AwaitingTools)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Thinking",
            Self::AwaitingTools => "Acting in game",
            Self::Succeeded => "Answered",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }
}

impl Job {
    /// The answer, or why there is none, once the request has finished.
    pub fn outcome(&self) -> Option<Outcome<'_>> {
        match (&self.answer, &self.error) {
            (Some(answer), _) => Some(Outcome::Answer(answer)),
            (None, Some(error)) => Some(Outcome::Error(error)),
            (None, None) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome<'a> {
    Answer(&'a str),
    Error(&'a str),
}

impl ToolCall {
    pub fn status(&self) -> &'static str {
        match (self.state, self.is_error) {
            (ToolCallState::Pending, _) => "Waiting for the server",
            (ToolCallState::Delivered, _) => "Running in game",
            (ToolCallState::Completed, true) => "Error",
            (ToolCallState::Completed, false) => "Done",
        }
    }

    /// The input as compact JSON; tools without arguments show nothing.
    pub fn input_text(&self) -> Option<String> {
        match &self.input {
            Value::Null => None,
            Value::Object(fields) if fields.is_empty() => None,
            input => Some(input.to_string()),
        }
    }
}

/// Micro-USD as dollars and cents, rounded up so spending never reads low.
pub(crate) fn usd(micros: i64) -> String {
    let cents = (micros.max(0) as u64).div_ceil(10_000);
    format!("${}.{:02}", cents / 100, cents % 100)
}

/// An RFC 3339 timestamp to the minute, as "2026-10-12 01:54 UTC". The
/// gateway always answers in UTC.
pub(crate) fn utc_minute(timestamp: &str) -> String {
    match (timestamp.get(..10), timestamp.get(11..16)) {
        (Some(day), Some(time)) => format!("{day} {time} UTC"),
        _ => timestamp.to_owned(),
    }
}

/// The first day of the month after `period_start`, when budgets reset.
pub(crate) fn next_period(period_start: &str) -> Option<String> {
    let year: u32 = period_start.get(..4)?.parse().ok()?;
    let month: u32 = period_start.get(5..7)?.parse().ok()?;
    let (year, month) = if month >= 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    Some(format!("{year:04}-{month:02}-01"))
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::json;

    use super::*;

    pub(crate) fn overview() -> Overview {
        serde_json::from_value(json!({
            "available": true,
            "settings": {"enabled": true, "model": null, "effort": null, "monthly_budget_micros": null},
            "defaults": {
                "model": "claude-opus-5-5",
                "effort": "medium",
                "server_monthly_budget_micros": 5_000_000,
                "account_monthly_budget_micros": 20_000_000,
            },
            "models": [
                {"id": "claude-opus-5-5", "name": "Claude Opus 5.5", "input_micros_per_mtok": 5_000_000, "output_micros_per_mtok": 25_000_000},
                {"id": "claude-sonnet-5-5", "name": "Claude Sonnet 5.5", "input_micros_per_mtok": 3_000_000, "output_micros_per_mtok": 15_000_000},
            ],
            "usage": {
                "period_start": "2026-12-01T00:00:00Z",
                "server": {"spent_micros": 1_000_000, "reserved_micros": 250_000, "budget_micros": 5_000_000},
                "account": {"spent_micros": 2_000_000, "reserved_micros": 0, "budget_micros": 20_000_000},
                "model_calls": 12,
                "input_tokens": 1000,
                "output_tokens": 200,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
            },
            "jobs": [job("awaiting_tools", "make it day", None, None)],
        }))
        .unwrap()
    }

    pub(crate) fn job(
        state: &str,
        prompt: &str,
        answer: Option<&str>,
        error: Option<&str>,
    ) -> Value {
        let mut job = json!({
            "id": "00000000-0000-0000-0000-000000000003",
            "conversation_id": "00000000-0000-0000-0000-000000000004",
            "operator_id": "00000000-0000-0000-0000-000000000001",
            "operator_name": "Steve",
            "prompt": prompt,
            "model": "claude-opus-5-5",
            "state": state,
            "model_calls": 2,
            "tool_calls": 1,
            "cost_micros": 41_000,
            "created_at": "2026-10-12T01:54:33.123456Z",
            "updated_at": "2026-10-12T01:54:40Z",
        });
        if let Some(answer) = answer {
            job["answer"] = json!(answer);
        }
        if let Some(error) = error {
            job["error"] = json!(error);
        }
        job
    }

    #[test]
    fn dollars_round_up_to_the_cent() {
        assert_eq!(usd(0), "$0.00");
        assert_eq!(usd(1), "$0.01");
        assert_eq!(usd(5_000_000), "$5.00");
        assert_eq!(usd(1_234_567), "$1.24");
        assert_eq!(usd(-5), "$0.00");
    }

    #[test]
    fn the_overview_names_the_model_effort_and_budgets() {
        let mut overview = overview();
        assert_eq!(overview.model_name(), "Claude Opus 5.5");
        assert_eq!(overview.effort().map(Effort::label), Some("Medium"));
        assert_eq!(overview.budget(&overview.usage.server), "$1.25 of $5.00");
        assert!(overview.accepts_requests());

        overview.settings.model = Some("claude-sonnet-5-5".into());
        overview.settings.effort = Some(Effort::High);
        assert_eq!(overview.model_name(), "Claude Sonnet 5.5");
        assert_eq!(overview.effort().map(Effort::label), Some("High"));
        // A model the gateway stopped offering falls back to the default.
        overview.settings.model = Some("claude-retired".into());
        assert_eq!(overview.model_name(), "Claude Opus 5.5");
        assert_eq!(overview.named("claude-retired"), "claude-retired");

        overview.settings.enabled = false;
        assert!(!overview.accepts_requests());
        assert_eq!(
            overview.unavailable_reason(),
            Some("The AI assistant is turned off for this server.")
        );
        overview.available = false;
        assert_eq!(overview.budget(&overview.usage.server), "$1.25 spent");
    }

    #[test]
    fn requests_report_their_state_and_outcome() {
        let answered: Job = serde_json::from_value(job(
            "succeeded",
            "who is online?",
            Some("Steve and Alex."),
            None,
        ))
        .unwrap();
        assert_eq!(answered.state.label(), "Answered");
        assert!(!answered.state.is_active());
        assert_eq!(answered.outcome(), Some(Outcome::Answer("Steve and Alex.")));

        let cancelled: Job = serde_json::from_value(job(
            "cancelled",
            "build a castle",
            None,
            Some("Request cancelled."),
        ))
        .unwrap();
        assert_eq!(
            cancelled.outcome(),
            Some(Outcome::Error("Request cancelled."))
        );

        let running: Job =
            serde_json::from_value(job("awaiting_tools", "make it day", None, None)).unwrap();
        assert!(running.state.is_active());
        assert_eq!(running.state.label(), "Acting in game");
        assert_eq!(running.outcome(), None);
    }

    #[test]
    fn a_request_detail_parses_the_gateway_contract() {
        let mut detail = json!({
            "job": job("succeeded", "list players", Some("Two are online."), None),
            "tool_calls": [
                {
                    "call_id": "toolu_1",
                    "name": "run_command",
                    "input": {"command": "list"},
                    "state": "completed",
                    "output": "There are 2 players online.",
                    "is_error": false,
                    "created_at": "2026-10-12T01:54:34Z",
                    "completed_at": "2026-10-12T01:54:35Z",
                },
                {
                    "call_id": "toolu_2",
                    "name": "list_players",
                    "input": {},
                    "state": "delivered",
                    "is_error": false,
                    "created_at": "2026-10-12T01:54:36Z",
                },
            ],
        });
        let parsed: JobDetail = serde_json::from_value(detail.clone()).unwrap();
        assert_eq!(parsed.job.tool_calls, 1);
        assert_eq!(parsed.tool_calls.len(), 2);
        assert_eq!(parsed.tool_calls[0].status(), "Done");
        assert_eq!(
            parsed.tool_calls[0].input_text().as_deref(),
            Some(r#"{"command":"list"}"#)
        );
        assert_eq!(parsed.tool_calls[1].status(), "Running in game");
        assert_eq!(parsed.tool_calls[1].input_text(), None);

        detail["tool_calls"][0]["is_error"] = json!(true);
        let parsed: JobDetail = serde_json::from_value(detail).unwrap();
        assert_eq!(parsed.tool_calls[0].status(), "Error");
    }

    #[test]
    fn timestamps_read_to_the_minute_and_budgets_reset_monthly() {
        assert_eq!(
            utc_minute("2026-10-12T01:54:33.123456Z"),
            "2026-10-12 01:54 UTC"
        );
        assert_eq!(utc_minute("soon"), "soon");
        assert_eq!(
            next_period("2026-10-01T00:00:00Z").as_deref(),
            Some("2026-11-01")
        );
        assert_eq!(
            next_period("2026-12-01T00:00:00Z").as_deref(),
            Some("2027-01-01")
        );
        assert_eq!(next_period("garbage"), None);
    }
}

//! AI gateway contracts shared by the browser panel and the in-game plugin.
//!
//! Money is expressed in micro-USD (`1_000_000` = one US dollar) so budgets and
//! prices never pass through floating point.

use chrono::{DateTime, Utc};
use garde::Validate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Longest prompt accepted from an operator, in characters.
pub const MAX_PROMPT_CHARS: usize = 2000;
/// Largest tool result the plugin may return, in bytes.
pub const MAX_TOOL_OUTPUT_BYTES: usize = 16 * 1024;
/// Tool results accepted in one sync request.
pub const MAX_SYNC_RESULTS: usize = 8;

macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value),+
                }
            }

            pub fn parse(value: &str) -> Option<Self> {
                match value {
                    $($value => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

string_enum!(
    /// How much the model thinks before answering.
    AiEffort {
        Low => "low",
        Medium => "medium",
        High => "high",
    }
);

string_enum!(
    /// Lifecycle of one operator request.
    AiJobState {
        Queued => "queued",
        Running => "running",
        AwaitingTools => "awaiting_tools",
        Succeeded => "succeeded",
        Failed => "failed",
        Cancelled => "cancelled",
    }
);

impl AiJobState {
    pub const ACTIVE: &'static [Self] = &[Self::Queued, Self::Running, Self::AwaitingTools];

    pub fn is_active(self) -> bool {
        Self::ACTIVE.contains(&self)
    }
}

string_enum!(
    /// Execution state of one tool call requested by the model.
    AiToolCallState {
        Pending => "pending",
        Delivered => "delivered",
        Completed => "completed",
    }
);

string_enum!(
    /// Chat line delivered to the requesting operator.
    AiJobEventKind {
        Progress => "progress",
        Answer => "answer",
        Error => "error",
    }
);

/// Per-server AI settings. `None` means "use the gateway default".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct AiSettings {
    #[garde(skip)]
    pub enabled: bool,
    #[garde(length(min = 1, max = 64))]
    pub model: Option<String>,
    #[garde(skip)]
    pub effort: Option<AiEffort>,
    /// Monthly spending cap for this server in micro-USD, at most the
    /// account's monthly budget.
    #[garde(range(min = 0))]
    pub monthly_budget_micros: Option<i64>,
}

/// Gateway-wide defaults that apply when a server keeps a setting unset.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiDefaults {
    pub model: String,
    pub effort: AiEffort,
    pub server_monthly_budget_micros: i64,
    pub account_monthly_budget_micros: i64,
}

/// A model the server owner can choose, with its standard prices.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiModel {
    pub id: String,
    pub name: String,
    /// Micro-USD per million uncached input tokens.
    pub input_micros_per_mtok: i64,
    /// Micro-USD per million output tokens.
    pub output_micros_per_mtok: i64,
}

/// Spending against one monthly cap.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiBudget {
    pub spent_micros: i64,
    /// Worst-case cost of model calls that are still in flight.
    pub reserved_micros: i64,
    pub budget_micros: i64,
}

/// This calendar month's (UTC) usage of one server and its owner's account.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiUsage {
    pub period_start: DateTime<Utc>,
    pub server: AiBudget,
    pub account: AiBudget,
    pub model_calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub cache_read_input_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiJob {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub operator_id: Uuid,
    pub operator_name: String,
    pub prompt: String,
    pub model: String,
    pub state: AiJobState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub model_calls: i32,
    pub tool_calls: i32,
    pub cost_micros: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
}

/// One tool call as recorded for the owner's audit trail.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiToolCall {
    pub call_id: String,
    pub name: String,
    pub input: serde_json::Value,
    pub state: AiToolCallState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    pub is_error: bool,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

/// One request with every tool call it made.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiJobDetail {
    // Nested, not flattened: the request's `tool_calls` count would collide
    // with this list.
    pub job: AiJob,
    pub tool_calls: Vec<AiToolCall>,
}

/// Everything the panel shows about the AI assistant of one server.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiOverview {
    /// Whether the control plane has a model provider configured.
    pub available: bool,
    pub settings: AiSettings,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defaults: Option<AiDefaults>,
    pub models: Vec<AiModel>,
    pub usage: AiUsage,
    pub jobs: Vec<AiJob>,
}

/// Start a request on behalf of an in-game operator.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct AiJobRequest {
    /// Client-chosen id; retrying with the same id returns the same job.
    #[garde(skip)]
    pub job_id: Uuid,
    #[garde(skip)]
    pub operator_id: Uuid,
    #[garde(length(chars, min = 1, max = 64))]
    pub operator_name: String,
    #[garde(length(chars, min = 1, max = MAX_PROMPT_CHARS))]
    pub prompt: String,
    /// Start a fresh conversation instead of continuing the operator's last one.
    #[serde(default)]
    #[garde(skip)]
    pub new_conversation: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct AiToolResult {
    #[garde(skip)]
    pub job_id: Uuid,
    #[garde(length(min = 1, max = 128))]
    pub call_id: String,
    #[garde(length(bytes, max = MAX_TOOL_OUTPUT_BYTES))]
    pub output: String,
    #[serde(default)]
    #[garde(skip)]
    pub is_error: bool,
}

/// One plugin poll: return finished tool results, receive new calls and chat
/// lines.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct AiSyncRequest {
    #[serde(default)]
    #[garde(length(max = MAX_SYNC_RESULTS), dive)]
    pub results: Vec<AiToolResult>,
    /// Last event id the plugin has seen. Omit it on the first sync: the
    /// plugin then receives only the lines of requests still running.
    #[garde(range(min = 0))]
    pub after_event_id: Option<i64>,
}

/// A tool call the plugin must execute as the requesting operator.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiToolCallRequest {
    pub job_id: Uuid,
    pub call_id: String,
    pub operator_id: Uuid,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiJobEvent {
    pub id: i64,
    pub job_id: Uuid,
    pub operator_id: Uuid,
    pub kind: AiJobEventKind,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiActiveJob {
    pub job_id: Uuid,
    pub operator_id: Uuid,
    pub state: AiJobState,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiSyncResponse {
    pub calls: Vec<AiToolCallRequest>,
    pub events: Vec<AiJobEvent>,
    /// Pass back as `after_event_id` on the next sync.
    pub cursor: i64,
    pub active_jobs: Vec<AiActiveJob>,
}

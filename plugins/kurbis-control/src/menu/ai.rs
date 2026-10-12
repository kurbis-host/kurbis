//! The AI assistant's pages: recent requests with their answers, each
//! request's tool calls, this month's budgets and the settings.

use std::collections::HashMap;

use anyhow::{Context as _, Result};
use pumpkin_plugin_api::{Inventory, Player, forms::SimpleFormBuilder, text::TextComponent};
use serde::Deserialize;
use serde_json::Value;

use super::{
    Action, LORE_LINES, LORE_WIDTH, PAGE_SIZE, TextInputTarget, View, button, clip, close_screen,
    content_slot, current_page, page_controls, page_entries, wrap,
};
use crate::ai::{
    self, LineKind,
    records::{
        Effort, Job, JobDetail, JobState, Outcome, Overview, ToolCall, ToolCallState, next_period,
        usd, utc_minute,
    },
};

/// Characters of a prompt shown as an item's name.
const TITLE_CHARS: usize = 32;

pub(super) fn render_overview(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
) -> Result<()> {
    let overview = Overview::deserialize(data).context("invalid AI overview")?;
    button(
        inv,
        actions,
        4,
        "enchanted_book",
        "Kurbis • AI assistant",
        &header_lore(&overview),
        None,
    );
    if overview.accepts_requests() {
        button(
            inv,
            actions,
            2,
            "writable_book",
            "Ask",
            "Ask a question or give an instruction.\nFollow-ups within an hour continue it.",
            Some(Action::Prompt(TextInputTarget::AiPrompt {
                new_conversation: false,
            })),
        );
        button(
            inv,
            actions,
            6,
            "book",
            "New conversation",
            "Ask without the earlier conversation.",
            Some(Action::Prompt(TextInputTarget::AiPrompt {
                new_conversation: true,
            })),
        );
    }
    // The gateway lists the latest 20 requests, which fit on one page.
    for (index, job) in page_entries(&overview.jobs, 0).enumerate() {
        button(
            inv,
            actions,
            content_slot(index),
            state_material(job.state),
            &clip(&job.prompt, TITLE_CHARS),
            &job_lore(&overview, job),
            Some(Action::Open(View::AiRequest {
                job: job.id,
                page: 0,
            })),
        );
    }
    if overview.jobs.is_empty() {
        button(
            inv,
            actions,
            22,
            "paper",
            "No requests yet",
            "Requests of every operator show here.",
            None,
        );
    }
    button(
        inv,
        actions,
        47,
        "gold_ingot",
        "This month",
        &usage_lore(&overview),
        None,
    );
    if overview.available {
        button(
            inv,
            actions,
            51,
            "comparator",
            "Settings",
            &settings_lore(&overview),
            Some(Action::Login),
        );
    }
    Ok(())
}

pub(super) fn render_request(
    inv: &Inventory,
    actions: &mut HashMap<i16, Action>,
    data: &Value,
    requested_page: usize,
    view: &View,
) -> Result<()> {
    let detail = JobDetail::deserialize(data).context("invalid AI request")?;
    let job = &detail.job;
    button(
        inv,
        actions,
        4,
        state_material(job.state),
        &clip(&job.prompt, TITLE_CHARS),
        &request_lore(job),
        None,
    );
    if let Some(outcome) = job.outcome() {
        let (label, text, failed) = match outcome {
            Outcome::Answer(text) => ("Read the answer", text, false),
            Outcome::Error(text) => ("Read the error", text, true),
        };
        let mut lore = wrap(text, LORE_WIDTH, LORE_LINES - 1);
        lore.push("Click to read all of it.".into());
        button(
            inv,
            actions,
            2,
            "written_book",
            label,
            &lore.join("\n"),
            Some(Action::ShowAiOutcome {
                heading: format!("{} asked: {}", job.operator_name, job.prompt),
                outcome: text.to_owned(),
                failed,
            }),
        );
    }
    if job.state.is_active() {
        button(
            inv,
            actions,
            6,
            "barrier",
            "Cancel request",
            "Stop the assistant. What it already did\nin game stays done.",
            Some(Action::CancelAiJob {
                job: job.id,
                page: requested_page,
            }),
        );
    }
    let page = current_page(detail.tool_calls.len(), requested_page);
    for (index, call) in page_entries(&detail.tool_calls, page).enumerate() {
        button(
            inv,
            actions,
            content_slot(index),
            call_material(call),
            &format!("{}. {}", page * PAGE_SIZE + index + 1, call.name),
            &call_lore(call),
            None,
        );
    }
    if detail.tool_calls.is_empty() {
        button(
            inv,
            actions,
            22,
            "paper",
            "No tool calls",
            if job.state.is_active() {
                "The assistant has not acted in game yet."
            } else {
                "The assistant answered without acting\nin game."
            },
            None,
        );
    }
    page_controls(inv, actions, view, page, detail.tool_calls.len());
    Ok(())
}

/// Show a finished request's whole answer or error: in chat on Java, with
/// the menu closed so chat shows, and in a form on Bedrock.
pub(super) fn show_outcome(player: &Player, heading: &str, outcome: &str, failed: bool) {
    if let Some(bedrock) = player.as_bedrock() {
        bedrock.open_form(
            SimpleFormBuilder::new(
                TextComponent::text("AI assistant"),
                TextComponent::text(&format!("{heading}\n\n{outcome}")),
            )
            .button(TextComponent::text("Close"), None)
            .build(),
        );
        return;
    }
    close_screen(player);
    ai::show(player, LineKind::Progress, heading);
    ai::show(
        player,
        if failed {
            LineKind::Error
        } else {
            LineKind::Answer
        },
        outcome,
    );
}

fn header_lore(overview: &Overview) -> String {
    if let Some(reason) = overview.unavailable_reason() {
        let mut lines = wrap(reason, LORE_WIDTH, 2);
        if overview.available {
            lines.push("Turn it on in the web panel (Settings).".into());
        }
        return lines.join("\n");
    }
    [
        format!(
            "{} • {} effort",
            overview.model_name(),
            overview.effort().map_or("default", Effort::label)
        ),
        "Ask: /ai ask <question>".into(),
        "Start over: /ai new <question>".into(),
        "Stop your request: /ai cancel".into(),
        "Summary in chat: /ai status".into(),
    ]
    .join("\n")
}

fn job_lore(overview: &Overview, job: &Job) -> String {
    let mut lines = vec![
        format!("{} • {}", job.state.label(), job.operator_name),
        utc_minute(&job.created_at),
        format!(
            "{} • {} • {}",
            usd(job.cost_micros),
            quantity(job.tool_calls.into(), "tool call"),
            overview.named(&job.model)
        ),
    ];
    if let Some(Outcome::Answer(text) | Outcome::Error(text)) = job.outcome() {
        lines.extend(wrap(text, LORE_WIDTH, 2));
    }
    lines.push("Click for the details".into());
    lines.join("\n")
}

fn request_lore(job: &Job) -> String {
    let mut lines = vec![
        format!("{} • {}", job.state.label(), job.operator_name),
        utc_minute(&job.created_at),
        format!(
            "{} • {} • {}",
            usd(job.cost_micros),
            quantity(job.model_calls.into(), "model call"),
            quantity(job.tool_calls.into(), "tool call")
        ),
    ];
    let room = LORE_LINES - lines.len();
    lines.extend(wrap(&job.prompt, LORE_WIDTH, room));
    lines.join("\n")
}

fn usage_lore(overview: &Overview) -> String {
    let usage = &overview.usage;
    let mut lines = vec![
        format!("This server: {}", overview.budget(&usage.server)),
        format!("Owner's account: {}", overview.budget(&usage.account)),
        "The account budget covers all their servers.".into(),
        format!(
            "{} on this server",
            quantity(usage.model_calls, "model call")
        ),
    ];
    if let Some(reset) = next_period(&usage.period_start) {
        lines.push(format!("Resets on {reset} (UTC)"));
    }
    lines.join("\n")
}

fn settings_lore(overview: &Overview) -> String {
    [
        format!(
            "Assistant: {}",
            if overview.settings.enabled {
                "on"
            } else {
                "off"
            }
        ),
        format!("Model: {}", overview.model_name()),
        format!(
            "Effort: {}",
            overview.effort().map_or("default", Effort::label)
        ),
        format!(
            "Server budget: {} a month",
            usd(overview.usage.server.budget_micros)
        ),
        "Change these in the web panel.".into(),
        "Click for a one-use login link.".into(),
    ]
    .join("\n")
}

fn call_lore(call: &ToolCall) -> String {
    let mut lines = vec![call.status().to_owned()];
    if let Some(input) = call.input_text() {
        lines.extend(wrap(&format!("Input: {input}"), LORE_WIDTH, 2));
    }
    if let Some(output) = &call.output {
        let label = if call.is_error { "Error" } else { "Result" };
        let room = LORE_LINES - lines.len();
        lines.extend(wrap(&format!("{label}: {output}"), LORE_WIDTH, room));
    }
    lines.join("\n")
}

fn state_material(state: JobState) -> &'static str {
    match state {
        JobState::Queued | JobState::Running | JobState::AwaitingTools => "yellow_concrete",
        JobState::Succeeded => "lime_concrete",
        JobState::Failed => "red_concrete",
        JobState::Cancelled => "light_gray_concrete",
    }
}

fn call_material(call: &ToolCall) -> &'static str {
    match (call.state, call.is_error) {
        (ToolCallState::Pending | ToolCallState::Delivered, _) => "yellow_concrete",
        (ToolCallState::Completed, true) => "red_concrete",
        (ToolCallState::Completed, false) => "lime_concrete",
    }
}

fn quantity(count: i64, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ai::records::tests::{job, overview};

    fn lines(lore: &str) -> Vec<&str> {
        let lines = lore.lines().collect::<Vec<_>>();
        assert!(lines.len() <= LORE_LINES, "{lore}");
        for line in &lines {
            assert!(line.chars().count() <= LORE_WIDTH, "{line}");
        }
        lines
    }

    #[test]
    fn the_overview_header_says_how_to_use_the_assistant_or_why_it_is_off() {
        let mut overview = overview();
        assert_eq!(
            lines(&header_lore(&overview)),
            vec![
                "Claude Opus 5.5 • Medium effort",
                "Ask: /ai ask <question>",
                "Start over: /ai new <question>",
                "Stop your request: /ai cancel",
                "Summary in chat: /ai status",
            ]
        );
        overview.settings.enabled = false;
        assert_eq!(
            lines(&header_lore(&overview)),
            vec![
                "The AI assistant is turned off for this",
                "server.",
                "Turn it on in the web panel (Settings).",
            ]
        );
        overview.available = false;
        assert_eq!(
            lines(&header_lore(&overview)),
            vec![
                "The AI assistant is not set up on this Kurbis",
                "installation.",
            ]
        );
    }

    #[test]
    fn requests_show_state_asker_cost_and_a_preview_of_the_outcome() {
        let overview = overview();
        let answered: Job = serde_json::from_value(job(
            "succeeded",
            "who is online?",
            Some("Steve and Alex are online. Both are building the castle walls near spawn right now."),
            None,
        ))
        .unwrap();
        assert_eq!(
            lines(&job_lore(&overview, &answered)),
            vec![
                "Answered • Steve",
                "2026-10-12 01:54 UTC",
                "$0.05 • 1 tool call • Claude Opus 5.5",
                "Steve and Alex are online. Both are building",
                "the castle walls near spawn right now.",
                "Click for the details",
            ]
        );
        assert_eq!(
            lines(&request_lore(&answered)),
            vec![
                "Answered • Steve",
                "2026-10-12 01:54 UTC",
                "$0.05 • 2 model calls • 1 tool call",
                "who is online?",
            ]
        );
        assert_eq!(state_material(answered.state), "lime_concrete");
    }

    #[test]
    fn usage_and_settings_fit_their_items() {
        let overview = overview();
        assert_eq!(
            lines(&usage_lore(&overview)),
            vec![
                "This server: $1.25 of $5.00",
                "Owner's account: $2.00 of $20.00",
                "The account budget covers all their servers.",
                "12 model calls on this server",
                "Resets on 2027-01-01 (UTC)",
            ]
        );
        assert_eq!(
            lines(&settings_lore(&overview)),
            vec![
                "Assistant: on",
                "Model: Claude Opus 5.5",
                "Effort: Medium",
                "Server budget: $5.00 a month",
                "Change these in the web panel.",
                "Click for a one-use login link.",
            ]
        );
    }

    #[test]
    fn tool_calls_show_status_input_and_result() {
        let call = |value| serde_json::from_value::<ToolCall>(value).unwrap();
        let done = call(json!({
            "name": "run_command",
            "input": {"command": "time set day"},
            "state": "completed",
            "output": "Set the time to 1000",
            "is_error": false,
        }));
        assert_eq!(
            lines(&call_lore(&done)),
            vec![
                "Done",
                r#"Input: {"command":"time set day"}"#,
                "Result: Set the time to 1000",
            ]
        );
        assert_eq!(call_material(&done), "lime_concrete");

        let waiting = call(json!({
            "name": "list_players",
            "input": {},
            "state": "pending",
            "is_error": false,
        }));
        assert_eq!(lines(&call_lore(&waiting)), vec!["Waiting for the server"]);
        assert_eq!(call_material(&waiting), "yellow_concrete");

        let failed = call(json!({
            "name": "run_command",
            "input": {"command": "x".repeat(200)},
            "state": "completed",
            "output": "y".repeat(400),
            "is_error": true,
        }));
        let lore = call_lore(&failed);
        let failed_lines = lines(&lore);
        assert_eq!(failed_lines.len(), LORE_LINES);
        assert_eq!(failed_lines[0], "Error");
        assert!(failed_lines[3].starts_with("Error: y"));
        assert!(failed_lines[5].ends_with('…'));
        assert_eq!(call_material(&failed), "red_concrete");
    }

    #[test]
    fn counts_read_naturally() {
        assert_eq!(quantity(0, "tool call"), "0 tool calls");
        assert_eq!(quantity(1, "tool call"), "1 tool call");
        assert_eq!(quantity(64, "tool call"), "64 tool calls");
    }
}

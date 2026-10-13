//! `control-plane check-ai`: one tool round trip with every offered model,
//! run where the control plane runs. It sends the assistant's real system
//! prompt and tools, so a deployment can confirm that its provider settings,
//! network path and models work before operators use `/ai`.

use anyhow::{Context, bail, ensure};
use serde_json::{Value, value::RawValue};
use uuid::Uuid;

use crate::config::AiConfig;

use super::{
    catalog::{Catalog, Model, Tokens},
    prompt,
    provider::{Block, ModelClient, ModelRequest, Stop, Turn},
};

const QUESTION: &str = "Which players are online right now?";
const PLAYERS: &str = r#"[{"name":"Steve","world":"world","game_mode":"survival"},{"name":"Alex","world":"world","game_mode":"creative"}]"#;
const MAX_TOKENS: u32 = 4096;
/// Tool rounds a check allows before it gives up on an answer.
const MAX_ROUNDS: usize = 4;

/// Check every offered model and print one line per model; fails when any
/// model fails.
pub async fn check_provider(config: &AiConfig) -> anyhow::Result<()> {
    let catalog = Catalog::new(config.provider, &config.models);
    let client = ModelClient::new(config)?;
    let mut failed = Vec::new();
    for model in catalog.iter() {
        match round_trip(&client, &catalog, model, config).await {
            Ok(report) => println!("ok    {}: {report}", model.id),
            Err(error) => {
                println!("FAIL  {}: {error:#}", model.id);
                failed.push(model.id.as_str());
            }
        }
    }
    ensure!(
        failed.is_empty(),
        "{} of {} models failed: {}",
        failed.len(),
        catalog.iter().count(),
        failed.join(", ")
    );
    Ok(())
}

fn raw(value: &Value) -> anyhow::Result<Box<RawValue>> {
    Ok(RawValue::from_string(value.to_string())?)
}

async fn round_trip(
    client: &ModelClient,
    catalog: &Catalog,
    model: &Model,
    config: &AiConfig,
) -> anyhow::Result<String> {
    let system = raw(&Value::from(prompt::SYSTEM_PROMPT))?;
    let tools = raw(&prompt::tools())?;
    let mut transcript = vec![(
        "user",
        raw(&prompt::user_turn("Kurbis", Uuid::nil(), QUESTION))?,
    )];
    let mut tokens = Tokens::default();
    let mut cost = 0;
    let mut called = Vec::new();
    for round in 1..=MAX_ROUNDS {
        let response = client
            .complete(&ModelRequest {
                model,
                max_tokens: MAX_TOKENS,
                effort: config.default_effort,
                system: &system,
                tools: &tools,
                messages: transcript
                    .iter()
                    .map(|(role, content)| Turn { role, content })
                    .collect(),
            })
            .await
            .with_context(|| format!("call {round}"))?;
        for (attempt, used) in &response.attempts {
            tokens.input += used.input + used.cache_creation + used.cache_read;
            tokens.output += used.output;
            cost += catalog.cost_micros(attempt, *used);
        }
        let blocks = response
            .blocks()
            .with_context(|| format!("content of call {round}"))?;
        let mut texts = Vec::new();
        let mut results = Vec::new();
        for block in blocks {
            match block {
                Block::Text { text } if !text.trim().is_empty() => {
                    texts.push(text.trim().to_owned());
                }
                Block::ToolUse { id, name, .. } => {
                    let output = if name == "list_players" {
                        PLAYERS
                    } else {
                        "This check has no data for that tool."
                    };
                    results.push(prompt::tool_result(&id, output, false));
                    called.push(name);
                }
                _ => {}
            }
        }
        match response.stop {
            Stop::ToolUse if !results.is_empty() => {
                transcript.push(("assistant", response.content));
                transcript.push(("user", raw(&Value::Array(results))?));
            }
            Stop::EndTurn if called.is_empty() => {
                bail!("the model answered without calling a tool; the assistant needs tool calls")
            }
            Stop::EndTurn if !texts.is_empty() => {
                return Ok(format!(
                    "called {}, then answered {:?} ({} prompt and {} output tokens, ${}.{:06})",
                    called.join(", "),
                    texts.join(" "),
                    tokens.input,
                    tokens.output,
                    cost / 1_000_000,
                    cost % 1_000_000
                ));
            }
            stop => bail!("call {round} ended without an answer (stop: {stop:?})"),
        }
    }
    bail!(
        "the model was still calling tools after {MAX_ROUNDS} rounds: {}",
        called.join(", ")
    )
}

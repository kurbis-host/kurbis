//! Anthropic's Messages API. The transcript already has its format: stored
//! turns are replayed byte for byte and the assistant's content is stored
//! exactly as returned, so thinking blocks keep their signatures valid across
//! turns.

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::{contracts::ai::AiEffort, control_plane::ai::catalog::Tokens};

use super::{ModelRequest, ModelResponse, Stop, Turn, WireFormat};

const API_VERSION: &str = "2023-06-01";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const PROGRESS_UPDATES_BETA: &str = "thinking-display-updates-2026-08-18";
const THINKING_BINDING_BETA: &str = "thinking-binding-controls-2026-08-01";

pub(super) struct Messages;

#[derive(Serialize)]
struct Body<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a RawValue,
    tools: &'a RawValue,
    messages: &'a [Turn<'a>],
    thinking: Thinking,
    output_config: OutputConfig,
    cache_control: CacheControl,
    #[serde(skip_serializing_if = "Option::is_none")]
    fallbacks: Option<&'static str>,
}

#[derive(Serialize)]
struct Thinking {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    display: Option<&'static str>,
    block_binding: BlockBinding,
}

/// A replayed thinking block that no longer matches its conversation is
/// dropped instead of failing the whole request.
#[derive(Serialize)]
struct BlockBinding {
    prefix_mismatch_behavior: &'static str,
}

#[derive(Serialize)]
struct OutputConfig {
    effort: AiEffort,
}

#[derive(Serialize)]
struct CacheControl {
    #[serde(rename = "type")]
    kind: &'static str,
}

fn fallbacks(request: &ModelRequest<'_>) -> bool {
    !request.model.fallback_models.is_empty()
}

impl WireFormat for Messages {
    const PATH: &'static str = "/messages";
    const AUTH: (HeaderName, &'static str) = (HeaderName::from_static("x-api-key"), "");

    fn headers(request: &ModelRequest<'_>) -> HeaderMap {
        let mut betas = vec![THINKING_BINDING_BETA];
        if request.model.progress_updates {
            betas.push(PROGRESS_UPDATES_BETA);
        }
        if fallbacks(request) {
            betas.push(FALLBACK_BETA);
        }
        let mut headers = HeaderMap::new();
        headers.insert("anthropic-version", HeaderValue::from_static(API_VERSION));
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_str(&betas.join(",")).expect("beta names are header values"),
        );
        headers
    }

    fn body(request: &ModelRequest<'_>) -> serde_json::Result<Vec<u8>> {
        serde_json::to_vec(&Body {
            model: &request.model.id,
            max_tokens: request.max_tokens,
            system: request.system,
            tools: request.tools,
            messages: &request.messages,
            thinking: Thinking {
                kind: "adaptive",
                display: request.model.progress_updates.then_some("updates"),
                block_binding: BlockBinding {
                    prefix_mismatch_behavior: "drop_block",
                },
            },
            output_config: OutputConfig {
                effort: request.effort,
            },
            cache_control: CacheControl { kind: "ephemeral" },
            fallbacks: fallbacks(request).then_some("default"),
        })
    }

    fn response(_request: &ModelRequest<'_>, body: &[u8]) -> Result<ModelResponse, String> {
        let message: Message = serde_json::from_slice(body).map_err(|error| error.to_string())?;
        let attempts = message.attempts();
        Ok(ModelResponse {
            stop: match message.stop_reason.as_deref() {
                Some("tool_use") => Stop::ToolUse,
                Some("end_turn" | "stop_sequence") => Stop::EndTurn,
                Some("max_tokens" | "model_context_window_exceeded") => Stop::MaxTokens,
                Some("refusal") => Stop::Refusal,
                other => Stop::Other(other.map(str::to_owned)),
            },
            content: message.content,
            attempts,
        })
    }
}

#[derive(Debug, Deserialize)]
struct Message {
    model: String,
    content: Box<RawValue>,
    #[serde(default)]
    stop_reason: Option<String>,
    usage: Usage,
}

#[derive(Debug, Default, Deserialize)]
struct Usage {
    #[serde(flatten)]
    tokens: UsageTokens,
    /// Per-attempt usage when the request ran more than one model attempt
    /// (server-side fallbacks); the top-level counts then cover only the
    /// attempt that produced the message.
    #[serde(default)]
    iterations: Option<Vec<Iteration>>,
}

#[derive(Debug, Default, Deserialize)]
struct UsageTokens {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct Iteration {
    #[serde(default)]
    model: Option<String>,
    #[serde(flatten)]
    tokens: UsageTokens,
}

impl From<&UsageTokens> for Tokens {
    fn from(usage: &UsageTokens) -> Self {
        Self {
            input: usage.input_tokens.unwrap_or(0),
            output: usage.output_tokens.unwrap_or(0),
            cache_creation: usage.cache_creation_input_tokens.unwrap_or(0),
            cache_read: usage.cache_read_input_tokens.unwrap_or(0),
        }
    }
}

impl Message {
    /// Billable attempts of this message as `(model, tokens)` pairs.
    fn attempts(&self) -> Vec<(String, Tokens)> {
        match self.usage.iterations.as_deref() {
            Some(iterations) if !iterations.is_empty() => iterations
                .iter()
                .map(|iteration| {
                    (
                        iteration
                            .model
                            .clone()
                            .unwrap_or_else(|| self.model.clone()),
                        Tokens::from(&iteration.tokens),
                    )
                })
                .collect(),
            _ => vec![(self.model.clone(), Tokens::from(&self.usage.tokens))],
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{config::AiProvider, control_plane::ai::provider::Block};

    use super::super::tests::{model, raw};
    use super::*;

    fn read(body: &str) -> ModelResponse {
        let opus = model(AiProvider::Anthropic, "claude-opus-5-5");
        let system = raw(r#""""#);
        let tools = raw("[]");
        let request = ModelRequest {
            model: &opus,
            max_tokens: 16,
            effort: AiEffort::Low,
            system: &system,
            tools: &tools,
            messages: Vec::new(),
        };
        Messages::response(&request, body.as_bytes()).unwrap()
    }

    #[test]
    fn request_body_replays_the_transcript_verbatim() {
        let opus = model(AiProvider::Anthropic, "claude-opus-5-5");
        let system = raw(r#""Be brief.""#);
        let tools = raw("[]");
        let first = raw(r#"[{"type":"text","text":"hi"}]"#);
        let second = raw(r#"[{"signature":"x","thinking":"","type":"thinking"}]"#);
        let request = ModelRequest {
            model: &opus,
            max_tokens: 8192,
            effort: AiEffort::Medium,
            system: &system,
            tools: &tools,
            messages: vec![
                Turn {
                    role: "user",
                    content: &first,
                },
                Turn {
                    role: "assistant",
                    content: &second,
                },
            ],
        };
        let body = String::from_utf8(Messages::body(&request).unwrap()).unwrap();
        assert!(body.contains(
            r#""messages":[{"role":"user","content":[{"type":"text","text":"hi"}]},{"role":"assistant","content":[{"signature":"x","thinking":"","type":"thinking"}]}]"#
        ));
        assert!(body.contains(r#""thinking":{"type":"adaptive","display":"updates","block_binding":{"prefix_mismatch_behavior":"drop_block"}}"#));
        assert!(body.contains(r#""output_config":{"effort":"medium"}"#));
        assert!(body.contains(r#""fallbacks":"default""#));
        assert_eq!(
            Messages::headers(&request)["anthropic-beta"],
            "thinking-binding-controls-2026-08-01,thinking-display-updates-2026-08-18,server-side-fallback-2026-07-01"
        );

        let haiku = model(AiProvider::Anthropic, "claude-haiku-5-5");
        let plain = ModelRequest {
            model: &haiku,
            ..request
        };
        let body = String::from_utf8(Messages::body(&plain).unwrap()).unwrap();
        assert!(!body.contains("fallbacks"));
        assert!(!body.contains("display"));
        assert_eq!(
            Messages::headers(&plain)["anthropic-beta"],
            "thinking-binding-controls-2026-08-01"
        );
    }

    #[test]
    fn usage_is_read_per_attempt_when_a_fallback_ran() {
        let response = read(
            r#"{"model":"claude-opus-4-8","content":[{"type":"fallback","from":{"model":"claude-opus-5-5"},"to":{"model":"claude-opus-4-8"}},{"type":"text","text":"ok"}],
                "stop_reason":"end_turn",
                "usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":null,
                  "iterations":[{"type":"message","model":"claude-opus-5-5","input_tokens":10,"output_tokens":0},
                                {"type":"fallback_message","model":"claude-opus-4-8","input_tokens":10,"output_tokens":5,"cache_read_input_tokens":3}]}}"#,
        );
        assert_eq!(response.stop, Stop::EndTurn);
        assert_eq!(response.attempts.len(), 2);
        assert_eq!(response.attempts[0].0, "claude-opus-5-5");
        assert_eq!(response.attempts[1].1.cache_read, 3);
        let blocks = response.blocks().unwrap();
        assert!(matches!(blocks[0], Block::Other));
        assert!(matches!(&blocks[1], Block::Text { text } if text == "ok"));
    }

    #[test]
    fn usage_without_iterations_is_one_attempt_of_the_serving_model() {
        let response = read(
            r#"{"model":"claude-opus-5-5","content":[],"stop_reason":"max_tokens",
                "usage":{"input_tokens":7,"output_tokens":2,"cache_read_input_tokens":40}}"#,
        );
        assert_eq!(response.stop, Stop::MaxTokens);
        assert_eq!(
            response.attempts,
            vec![(
                "claude-opus-5-5".to_owned(),
                Tokens {
                    input: 7,
                    output: 2,
                    cache_creation: 0,
                    cache_read: 40
                }
            )]
        );
        let refused =
            read(r#"{"model":"claude-opus-5-5","content":[],"stop_reason":"refusal","usage":{}}"#);
        assert_eq!(refused.stop, Stop::Refusal);
        let unexplained = read(r#"{"model":"claude-opus-5-5","content":[],"usage":{}}"#);
        assert_eq!(unexplained.stop, Stop::Other(None));
    }
}

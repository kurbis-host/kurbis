//! Minimal Claude Messages API client.
//!
//! The transcript travels as raw JSON: stored turns are replayed byte for
//! byte and the assistant's content is stored exactly as returned, so thinking
//! blocks keep their signatures valid across turns.

use std::time::Duration;

use reqwest::{StatusCode, header};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::{config::AiConfig, contracts::ai::AiEffort};

use super::catalog::Tokens;

const API_VERSION: &str = "2023-06-01";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const PROGRESS_UPDATES_BETA: &str = "thinking-display-updates-2026-08-18";
const THINKING_BINDING_BETA: &str = "thinking-binding-controls-2026-08-01";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(240);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_ATTEMPTS: u32 = 3;
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
const MAX_ERROR_BODY_CHARS: usize = 2000;

#[derive(Clone)]
pub struct AnthropicClient {
    http: reqwest::Client,
    endpoint: String,
    api_key: crate::config::SecretString,
    retry_base_delay: Duration,
}

/// One stored transcript turn.
#[derive(Serialize)]
pub struct Turn<'a> {
    pub role: &'a str,
    pub content: &'a RawValue,
}

pub struct MessageRequest<'a> {
    pub model: &'a str,
    pub max_tokens: u32,
    pub effort: AiEffort,
    pub system: &'a RawValue,
    pub tools: &'a RawValue,
    pub messages: Vec<Turn<'a>>,
    pub fallbacks: bool,
    pub progress_updates: bool,
}

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

impl MessageRequest<'_> {
    fn body(&self) -> Body<'_> {
        Body {
            model: self.model,
            max_tokens: self.max_tokens,
            system: self.system,
            tools: self.tools,
            messages: &self.messages,
            thinking: Thinking {
                kind: "adaptive",
                display: self.progress_updates.then_some("updates"),
                block_binding: BlockBinding {
                    prefix_mismatch_behavior: "drop_block",
                },
            },
            output_config: OutputConfig {
                effort: self.effort,
            },
            cache_control: CacheControl { kind: "ephemeral" },
            fallbacks: self.fallbacks.then_some("default"),
        }
    }

    fn betas(&self) -> String {
        let mut betas = vec![THINKING_BINDING_BETA];
        if self.progress_updates {
            betas.push(PROGRESS_UPDATES_BETA);
        }
        if self.fallbacks {
            betas.push(FALLBACK_BETA);
        }
        betas.join(",")
    }
}

#[derive(Debug, Deserialize)]
pub struct MessageResponse {
    pub model: String,
    pub content: Box<RawValue>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    pub usage: Usage,
}

#[derive(Debug, Default, Deserialize)]
pub struct Usage {
    #[serde(flatten)]
    pub tokens: UsageTokens,
    /// Per-attempt usage when the request ran more than one model attempt
    /// (server-side fallbacks); the top-level counts then cover only the
    /// attempt that produced the message.
    #[serde(default)]
    pub iterations: Option<Vec<Iteration>>,
}

#[derive(Debug, Default, Deserialize)]
pub struct UsageTokens {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct Iteration {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(flatten)]
    pub tokens: UsageTokens,
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

impl MessageResponse {
    /// Billable attempts of this response as `(model, tokens)` pairs.
    pub fn attempts(&self) -> Vec<(String, Tokens)> {
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

    pub fn blocks(&self) -> Result<Vec<Block>, serde_json::Error> {
        serde_json::from_str(self.content.get())
    }
}

/// Content blocks the gateway acts on; every other block type is only stored.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("model provider returned HTTP {status}: {body}")]
    Status { status: StatusCode, body: String },
    #[error("model provider request failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("model provider returned an unreadable response: {0}")]
    Decode(#[from] serde_json::Error),
}

impl ProviderError {
    /// Whether the provider was overloaded or rate limited rather than the
    /// request being wrong.
    pub fn is_capacity(&self) -> bool {
        match self {
            Self::Status { status, .. } => retryable(*status),
            Self::Transport(error) => error.is_connect() || error.is_timeout(),
            Self::Decode(_) => false,
        }
    }
}

fn retryable(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() || status.as_u16() == 529
}

impl AnthropicClient {
    pub fn new(config: &AiConfig) -> anyhow::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .connect_timeout(CONNECT_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            endpoint: format!("{}/v1/messages", config.base_url),
            api_key: config.api_key.clone(),
            retry_base_delay: Duration::from_secs(2),
        })
    }

    #[cfg(test)]
    fn with_retry_base_delay(mut self, delay: Duration) -> Self {
        self.retry_base_delay = delay;
        self
    }

    pub async fn create_message(
        &self,
        request: &MessageRequest<'_>,
    ) -> Result<MessageResponse, ProviderError> {
        let body = serde_json::to_vec(&request.body())?;
        let betas = request.betas();
        let mut attempt = 0;
        loop {
            attempt += 1;
            let result = self
                .http
                .post(&self.endpoint)
                .header("x-api-key", self.api_key.expose())
                .header("anthropic-version", API_VERSION)
                .header("anthropic-beta", &betas)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body.clone())
                .send()
                .await;
            let delay = match result {
                Ok(response) if response.status().is_success() => {
                    let bytes = response.bytes().await?;
                    return Ok(serde_json::from_slice(&bytes)?);
                }
                Ok(response) => {
                    let status = response.status();
                    let retry_after = retry_after(response.headers());
                    if !retryable(status) || attempt >= MAX_ATTEMPTS {
                        let body = response.text().await.unwrap_or_default();
                        return Err(ProviderError::Status {
                            status,
                            body: body.chars().take(MAX_ERROR_BODY_CHARS).collect(),
                        });
                    }
                    retry_after
                }
                // A request that never connected cannot have been billed; a
                // timed-out one may have been, so it is not repeated.
                Err(error) if error.is_connect() && attempt < MAX_ATTEMPTS => None,
                Err(error) => return Err(error.into()),
            };
            let backoff = self.retry_base_delay * 2u32.pow(attempt - 1);
            tokio::time::sleep(delay.unwrap_or(backoff).min(MAX_RETRY_DELAY)).await;
        }
    }
}

fn retry_after(headers: &header::HeaderMap) -> Option<Duration> {
    headers
        .get(header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(value: &str) -> Box<RawValue> {
        RawValue::from_string(value.to_owned()).unwrap()
    }

    #[test]
    fn request_body_replays_the_transcript_verbatim() {
        let system = raw(r#""Be brief.""#);
        let tools = raw("[]");
        let first = raw(r#"[{"type":"text","text":"hi"}]"#);
        let second = raw(r#"[{"signature":"x","thinking":"","type":"thinking"}]"#);
        let request = MessageRequest {
            model: "claude-opus-5-5",
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
            fallbacks: true,
            progress_updates: true,
        };
        let body = serde_json::to_string(&request.body()).unwrap();
        assert!(body.contains(
            r#""messages":[{"role":"user","content":[{"type":"text","text":"hi"}]},{"role":"assistant","content":[{"signature":"x","thinking":"","type":"thinking"}]}]"#
        ));
        assert!(body.contains(r#""thinking":{"type":"adaptive","display":"updates","block_binding":{"prefix_mismatch_behavior":"drop_block"}}"#));
        assert!(body.contains(r#""output_config":{"effort":"medium"}"#));
        assert!(body.contains(r#""fallbacks":"default""#));
        assert_eq!(
            request.betas(),
            "thinking-binding-controls-2026-08-01,thinking-display-updates-2026-08-18,server-side-fallback-2026-07-01"
        );

        let plain = MessageRequest {
            fallbacks: false,
            progress_updates: false,
            ..request
        };
        let body = serde_json::to_string(&plain.body()).unwrap();
        assert!(!body.contains("fallbacks"));
        assert!(!body.contains("display"));
        assert_eq!(plain.betas(), "thinking-binding-controls-2026-08-01");
    }

    #[test]
    fn usage_is_read_per_attempt_when_a_fallback_ran() {
        let response: MessageResponse = serde_json::from_str(
            r#"{"model":"claude-opus-4-8","content":[{"type":"fallback","from":{"model":"claude-opus-5-5"},"to":{"model":"claude-opus-4-8"}},{"type":"text","text":"ok"}],
                "stop_reason":"end_turn",
                "usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":null,
                  "iterations":[{"type":"message","model":"claude-opus-5-5","input_tokens":10,"output_tokens":0},
                                {"type":"fallback_message","model":"claude-opus-4-8","input_tokens":10,"output_tokens":5,"cache_read_input_tokens":3}]}}"#,
        )
        .unwrap();
        let attempts = response.attempts();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].0, "claude-opus-5-5");
        assert_eq!(attempts[1].1.cache_read, 3);
        let blocks = response.blocks().unwrap();
        assert!(matches!(blocks[0], Block::Other));
        assert!(matches!(&blocks[1], Block::Text { text } if text == "ok"));
    }

    /// Serve `statuses` in order from a local endpoint, then report how many
    /// requests arrived.
    async fn provider(
        statuses: Vec<u16>,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        let app = axum::Router::new().route(
            "/v1/messages",
            axum::routing::post(move || {
                let index = counter.fetch_add(1, Ordering::SeqCst);
                let status = StatusCode::from_u16(statuses[index.min(statuses.len() - 1)]).unwrap();
                async move {
                    let body = if status.is_success() {
                        r#"{"model":"claude-opus-5-5","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#
                    } else {
                        r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#
                    };
                    (status, body)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), requests)
    }

    fn client(base_url: String) -> AnthropicClient {
        AnthropicClient::new(&AiConfig {
            api_key: crate::config::SecretString::new("test-key"),
            base_url,
            default_model: "claude-opus-5-5".to_owned(),
            default_effort: AiEffort::Medium,
            server_monthly_budget_micros: 0,
            account_monthly_budget_micros: 0,
            max_concurrent_calls: 1,
        })
        .unwrap()
        .with_retry_base_delay(Duration::from_millis(1))
    }

    async fn send(client: &AnthropicClient) -> Result<MessageResponse, ProviderError> {
        let system = raw(r#""""#);
        let tools = raw("[]");
        let content = raw(r#"[{"type":"text","text":"hi"}]"#);
        client
            .create_message(&MessageRequest {
                model: "claude-opus-5-5",
                max_tokens: 16,
                effort: AiEffort::Low,
                system: &system,
                tools: &tools,
                messages: vec![Turn {
                    role: "user",
                    content: &content,
                }],
                fallbacks: false,
                progress_updates: false,
            })
            .await
    }

    #[tokio::test]
    async fn capacity_errors_are_retried_and_request_errors_are_not() {
        use std::sync::atomic::Ordering;

        let (url, requests) = provider(vec![529, 500, 200]).await;
        let response = send(&client(url)).await.unwrap();
        assert_eq!(response.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(requests.load(Ordering::SeqCst), 3);

        let (url, requests) = provider(vec![529]).await;
        let error = send(&client(url)).await.unwrap_err();
        assert!(error.is_capacity());
        assert_eq!(requests.load(Ordering::SeqCst), MAX_ATTEMPTS as usize);

        let (url, requests) = provider(vec![400]).await;
        let error = send(&client(url)).await.unwrap_err();
        assert!(!error.is_capacity());
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn usage_without_iterations_is_one_attempt_of_the_serving_model() {
        let response: MessageResponse = serde_json::from_str(
            r#"{"model":"claude-opus-5-5","content":[],"stop_reason":"end_turn",
                "usage":{"input_tokens":7,"output_tokens":2,"cache_read_input_tokens":40}}"#,
        )
        .unwrap();
        assert_eq!(
            response.attempts(),
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
    }
}

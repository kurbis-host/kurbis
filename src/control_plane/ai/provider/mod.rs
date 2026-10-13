//! Model providers behind one interface.
//!
//! Transcripts are stored in the Messages API's content-block format. The
//! Anthropic adapter replays them byte for byte, which keeps thinking block
//! signatures valid across turns; the OpenAI-compatible adapter translates
//! them to chat messages on every call and its responses back into blocks.

mod anthropic;
mod openai;

use std::time::Duration;

use anyhow::Context;
use reqwest::{
    StatusCode,
    header::{self, HeaderMap, HeaderName, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::{
    config::{AiConfig, AiProvider, SecretString},
    contracts::ai::AiEffort,
};

use super::catalog::{Model, Tokens};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(240);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_ATTEMPTS: u32 = 3;
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
const MAX_ERROR_BODY_CHARS: usize = 2000;

/// One stored transcript turn.
#[derive(Serialize)]
pub struct Turn<'a> {
    pub role: &'a str,
    pub content: &'a RawValue,
}

pub struct ModelRequest<'a> {
    pub model: &'a Model,
    pub max_tokens: u32,
    pub effort: AiEffort,
    pub system: &'a RawValue,
    pub tools: &'a RawValue,
    pub messages: Vec<Turn<'a>>,
}

/// Why a response ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// The model waits for the results of its tool calls.
    ToolUse,
    EndTurn,
    /// The output allowance or the context window ran out.
    MaxTokens,
    Refusal,
    Other(Option<String>),
}

#[derive(Debug)]
pub struct ModelResponse {
    /// The assistant turn in the transcript format.
    pub content: Box<RawValue>,
    pub stop: Stop,
    /// Billable attempts as `(model, tokens)` pairs.
    pub attempts: Vec<(String, Tokens)>,
}

impl ModelResponse {
    pub fn blocks(&self) -> Result<Vec<Block>, serde_json::Error> {
        serde_json::from_str(self.content.get())
    }
}

/// Transcript content blocks the gateway reads; every other block type is
/// only stored.
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
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: serde_json::Value,
        #[serde(default)]
        is_error: bool,
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
    #[error("model provider exchange is invalid: {0}")]
    Invalid(String),
}

impl ProviderError {
    /// Whether the provider was overloaded or rate limited rather than the
    /// request being wrong.
    pub fn is_capacity(&self) -> bool {
        match self {
            Self::Status { status, .. } => retryable(*status),
            Self::Transport(error) => error.is_connect() || error.is_timeout(),
            Self::Invalid(_) => false,
        }
    }
}

fn retryable(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() || status.as_u16() == 529
}

/// One provider API's wire format.
trait WireFormat {
    /// Appended to the configured base URL.
    const PATH: &'static str;
    /// The header carrying the API key, and the text before the key.
    const AUTH: (HeaderName, &'static str);

    /// Headers besides authentication and the content type.
    fn headers(request: &ModelRequest<'_>) -> HeaderMap;
    fn body(request: &ModelRequest<'_>) -> serde_json::Result<Vec<u8>>;
    /// Read a successful response; `Err` says what is wrong with it.
    fn response(request: &ModelRequest<'_>, body: &[u8]) -> Result<ModelResponse, String>;
}

#[derive(Clone)]
pub struct ModelClient {
    http: reqwest::Client,
    provider: AiProvider,
    base_url: String,
    api_key: Option<SecretString>,
    retry_base_delay: Duration,
}

impl ModelClient {
    pub fn new(config: &AiConfig) -> anyhow::Result<Self> {
        if let Some(key) = &config.api_key {
            HeaderValue::from_str(key.expose())
                .context("AI_API_KEY must be a valid HTTP header value")?;
        }
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .connect_timeout(CONNECT_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            provider: config.provider,
            base_url: config.base_url.clone(),
            api_key: config.api_key.clone(),
            retry_base_delay: Duration::from_secs(2),
        })
    }

    #[cfg(test)]
    fn with_retry_base_delay(mut self, delay: Duration) -> Self {
        self.retry_base_delay = delay;
        self
    }

    pub async fn complete(
        &self,
        request: &ModelRequest<'_>,
    ) -> Result<ModelResponse, ProviderError> {
        match self.provider {
            AiProvider::Anthropic => self.send::<anthropic::Messages>(request).await,
            AiProvider::OpenAi => self.send::<openai::ChatCompletions>(request).await,
        }
    }

    async fn send<W: WireFormat>(
        &self,
        request: &ModelRequest<'_>,
    ) -> Result<ModelResponse, ProviderError> {
        let body = W::body(request).map_err(|error| {
            ProviderError::Invalid(format!("the transcript is unreadable: {error}"))
        })?;
        let mut headers = W::headers(request);
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        if let Some(key) = &self.api_key {
            let (name, prefix) = W::AUTH;
            // `new` checked that the key is a valid header value.
            if let Ok(mut value) = HeaderValue::from_str(&format!("{prefix}{}", key.expose())) {
                value.set_sensitive(true);
                headers.insert(name, value);
            }
        }
        let response = self
            .post(&format!("{}{}", self.base_url, W::PATH), headers, body)
            .await?;
        W::response(request, &response).map_err(|reason| {
            ProviderError::Invalid(format!(
                "{reason}; response: {}",
                excerpt(&String::from_utf8_lossy(&response))
            ))
        })
    }

    async fn post(
        &self,
        url: &str,
        headers: HeaderMap,
        body: Vec<u8>,
    ) -> Result<bytes::Bytes, ProviderError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let result = self
                .http
                .post(url)
                .headers(headers.clone())
                .body(body.clone())
                .send()
                .await;
            let delay = match result {
                Ok(response) if response.status().is_success() => {
                    return Ok(response.bytes().await?);
                }
                Ok(response) => {
                    let status = response.status();
                    let retry_after = retry_after(response.headers());
                    if !retryable(status) || attempt >= MAX_ATTEMPTS {
                        let body = response.text().await.unwrap_or_default();
                        return Err(ProviderError::Status {
                            status,
                            body: excerpt(&body),
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

fn excerpt(text: &str) -> String {
    text.chars().take(MAX_ERROR_BODY_CHARS).collect()
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
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
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use crate::{config::AiModelEntry, control_plane::ai::catalog::Catalog};

    use super::*;

    pub(super) fn raw(value: &str) -> Box<RawValue> {
        RawValue::from_string(value.to_owned()).unwrap()
    }

    pub(super) fn model(provider: AiProvider, id: &str) -> Model {
        Catalog::new(
            provider,
            &[AiModelEntry {
                id: id.to_owned(),
                prices: None,
            }],
        )
        .default_model()
        .clone()
    }

    /// Serve `statuses` in order from a local endpoint that expects the
    /// bearer key, then report how many requests arrived.
    async fn provider(statuses: Vec<u16>) -> (String, Arc<AtomicUsize>) {
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |headers: HeaderMap| {
                let index = counter.fetch_add(1, Ordering::SeqCst);
                let status = StatusCode::from_u16(statuses[index.min(statuses.len() - 1)]).unwrap();
                async move {
                    assert_eq!(headers[header::AUTHORIZATION], "Bearer test-key");
                    let body = if status.is_success() {
                        r#"{"model":"llama3.1:8b","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#
                    } else {
                        r#"{"error":{"message":"busy"}}"#
                    };
                    (status, body)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}/v1"), requests)
    }

    fn client(base_url: String) -> ModelClient {
        ModelClient::new(&AiConfig {
            provider: AiProvider::OpenAi,
            base_url,
            api_key: Some(SecretString::new("test-key")),
            models: Vec::new(),
            default_effort: AiEffort::Medium,
            server_monthly_budget_micros: 0,
            account_monthly_budget_micros: 0,
            max_concurrent_calls: 1,
        })
        .unwrap()
        .with_retry_base_delay(Duration::from_millis(1))
    }

    async fn send(client: &ModelClient) -> Result<ModelResponse, ProviderError> {
        let model = model(AiProvider::OpenAi, "llama3.1:8b");
        let system = raw(r#""""#);
        let tools = raw("[]");
        let content = raw(r#"[{"type":"text","text":"hi"}]"#);
        client
            .complete(&ModelRequest {
                model: &model,
                max_tokens: 16,
                effort: AiEffort::Low,
                system: &system,
                tools: &tools,
                messages: vec![Turn {
                    role: "user",
                    content: &content,
                }],
            })
            .await
    }

    #[tokio::test]
    async fn capacity_errors_are_retried_and_request_errors_are_not() {
        let (url, requests) = provider(vec![529, 500, 200]).await;
        let response = send(&client(url)).await.unwrap();
        assert_eq!(response.stop, Stop::EndTurn);
        assert_eq!(requests.load(Ordering::SeqCst), 3);

        let (url, requests) = provider(vec![429]).await;
        let error = send(&client(url)).await.unwrap_err();
        assert!(error.is_capacity());
        assert_eq!(requests.load(Ordering::SeqCst), MAX_ATTEMPTS as usize);

        let (url, requests) = provider(vec![400]).await;
        let error = send(&client(url)).await.unwrap_err();
        assert!(!error.is_capacity());
        assert!(error.to_string().contains("busy"), "{error}");
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn keys_that_cannot_travel_in_a_header_are_refused() {
        let mut config = AiConfig {
            provider: AiProvider::Anthropic,
            base_url: "https://api.anthropic.com/v1".to_owned(),
            api_key: Some(SecretString::new("line\nbreak")),
            models: Vec::new(),
            default_effort: AiEffort::Medium,
            server_monthly_budget_micros: 0,
            account_monthly_budget_micros: 0,
            max_concurrent_calls: 1,
        };
        assert!(ModelClient::new(&config).is_err());
        config.api_key = None;
        assert!(ModelClient::new(&config).is_ok());
    }
}

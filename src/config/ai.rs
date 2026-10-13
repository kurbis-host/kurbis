//! AI gateway configuration. The gateway is enabled only when `AI_PROVIDER`
//! names the API it calls models through; every other setting either has a
//! production default or is required by that provider.

use std::env;

use anyhow::{Context, Result, ensure};

use crate::contracts::ai::AiEffort;

use super::SecretString;

/// Longest model id a server's settings can store.
const MAX_MODEL_ID_BYTES: usize = 64;

/// The API the gateway calls models through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiProvider {
    /// Anthropic's Messages API, with prompt caching, adaptive thinking that
    /// reports progress, and server-side fallbacks for Claude models.
    Anthropic,
    /// Any OpenAI-compatible Chat Completions API, such as Ollama, vLLM,
    /// LiteLLM or Open WebUI.
    OpenAi,
}

impl AiProvider {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "anthropic" => Some(Self::Anthropic),
            "openai" => Some(Self::OpenAi),
            _ => None,
        }
    }

    fn default_base_url(self) -> Option<&'static str> {
        match self {
            Self::Anthropic => Some("https://api.anthropic.com/v1"),
            Self::OpenAi => None,
        }
    }

    fn default_models(self) -> Option<&'static str> {
        match self {
            Self::Anthropic => Some("claude-opus-5-5,claude-sonnet-5-5,claude-haiku-5-5"),
            Self::OpenAi => None,
        }
    }
}

/// A model server owners can choose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiModelEntry {
    pub id: String,
    /// Input and output prices in micro-USD per million tokens, when the
    /// deployment sets them.
    pub prices: Option<(i64, i64)>,
}

#[derive(Clone, Debug)]
pub struct AiConfig {
    pub provider: AiProvider,
    /// API root that the provider's endpoint path is appended to.
    pub base_url: String,
    pub api_key: Option<SecretString>,
    /// Offered models; the first is the default.
    pub models: Vec<AiModelEntry>,
    pub default_effort: AiEffort,
    pub server_monthly_budget_micros: i64,
    pub account_monthly_budget_micros: i64,
    /// Model calls the gateway runs at the same time across all servers.
    pub max_concurrent_calls: usize,
}

impl AiConfig {
    pub fn from_env() -> Result<Option<Self>> {
        Self::from_vars(|name| match env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(env::VarError::NotPresent) => Ok(None),
            Err(error) => Err(error).with_context(|| format!("{name} must be valid UTF-8")),
        })
    }

    fn from_vars(var: impl Fn(&str) -> Result<Option<String>>) -> Result<Option<Self>> {
        // Unset and blank both select the default, so a deployment can pass
        // every variable through and leave the ones it does not set empty.
        let setting = |name: &str| -> Result<Option<String>> {
            Ok(var(name)?
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()))
        };
        let Some(provider) = setting("AI_PROVIDER")? else {
            return Ok(None);
        };
        let provider =
            AiProvider::parse(&provider).context("AI_PROVIDER must be anthropic or openai")?;
        let api_key = setting("AI_API_KEY")?;
        ensure!(
            api_key.is_some() || provider != AiProvider::Anthropic,
            "AI_API_KEY is required for the anthropic provider"
        );
        ensure!(
            api_key
                .as_deref()
                .is_none_or(|key| key.bytes().all(|byte| byte.is_ascii_graphic())),
            "AI_API_KEY must be printable ASCII without spaces"
        );
        let base_url = setting("AI_BASE_URL")?
            .or_else(|| provider.default_base_url().map(str::to_owned))
            .context("AI_BASE_URL is required for the openai provider")?
            .trim_end_matches('/')
            .to_owned();
        ensure!(
            base_url.starts_with("https://") || base_url.starts_with("http://"),
            "AI_BASE_URL must be an HTTP(S) URL"
        );
        let models = setting("AI_MODELS")?
            .or_else(|| provider.default_models().map(str::to_owned))
            .context("AI_MODELS is required for the openai provider")?;
        let default_effort = match setting("AI_DEFAULT_EFFORT")? {
            Some(effort) => AiEffort::parse(&effort)
                .context("AI_DEFAULT_EFFORT must be low, medium, or high")?,
            None => AiEffort::Medium,
        };
        let max_concurrent_calls = setting("AI_MAX_CONCURRENT_CALLS")?
            .map_or(Some(16), |value| value.parse::<usize>().ok())
            .filter(|value| (1..=256).contains(value))
            .context("AI_MAX_CONCURRENT_CALLS must be between 1 and 256")?;
        let budget = |name: &str, default: &str| -> Result<i64> {
            parse_usd_micros(setting(name)?.as_deref().unwrap_or(default)).with_context(|| {
                format!("{name} must be a non-negative US dollar amount with at most six decimals")
            })
        };
        Ok(Some(Self {
            provider,
            base_url,
            api_key: api_key.map(SecretString::new),
            models: parse_models(&models)?,
            default_effort,
            server_monthly_budget_micros: budget("AI_SERVER_MONTHLY_BUDGET_USD", "5")?,
            account_monthly_budget_micros: budget("AI_ACCOUNT_MONTHLY_BUDGET_USD", "20")?,
            max_concurrent_calls,
        }))
    }
}

/// Parse `AI_MODELS`: comma-separated model ids, each optionally followed by
/// `=INPUT/OUTPUT` prices in US dollars per million tokens.
fn parse_models(value: &str) -> Result<Vec<AiModelEntry>> {
    let mut models: Vec<AiModelEntry> = Vec::new();
    for entry in value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let (id, prices) = match entry.split_once('=') {
            Some((id, prices)) => {
                let price =
                    |price: Option<&str>| price.and_then(|price| parse_usd_micros(price.trim()));
                let (input, output) = prices.split_once('/').unzip();
                let prices = price(input).zip(price(output)).with_context(|| {
                    format!("AI_MODELS entry {entry} must price as id=INPUT/OUTPUT in US dollars per million tokens")
                })?;
                (id.trim(), Some(prices))
            }
            None => (entry, None),
        };
        ensure!(
            !id.is_empty()
                && id.len() <= MAX_MODEL_ID_BYTES
                && !id.chars().any(char::is_whitespace),
            "AI_MODELS entry {entry} must name a model id of at most {MAX_MODEL_ID_BYTES} bytes without spaces"
        );
        ensure!(
            models.iter().all(|model| model.id != id),
            "AI_MODELS lists {id} more than once"
        );
        models.push(AiModelEntry {
            id: id.to_owned(),
            prices,
        });
    }
    ensure!(!models.is_empty(), "AI_MODELS must list at least one model");
    Ok(models)
}

/// Parse a decimal US dollar amount ("5", "0.25") into micro-USD exactly.
pub fn parse_usd_micros(value: &str) -> Option<i64> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || fraction.len() > 6 || !digits(fraction) {
        return None;
    }
    let fraction: i64 = format!("{fraction:0<6}").parse().ok()?;
    whole
        .parse::<i64>()
        .ok()?
        .checked_mul(1_000_000)?
        .checked_add(fraction)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn config(vars: &[(&str, &str)]) -> Result<Option<AiConfig>> {
        let vars = vars
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect::<HashMap<_, _>>();
        AiConfig::from_vars(|name| Ok(vars.get(name).cloned()))
    }

    fn model(id: &str, prices: Option<(i64, i64)>) -> AiModelEntry {
        AiModelEntry {
            id: id.to_owned(),
            prices,
        }
    }

    #[test]
    fn the_assistant_is_off_until_a_provider_is_named() {
        assert!(config(&[]).unwrap().is_none());
        assert!(
            config(&[("AI_PROVIDER", " "), ("AI_API_KEY", "key")])
                .unwrap()
                .is_none()
        );
        assert!(config(&[("AI_PROVIDER", "claude")]).is_err());
    }

    #[test]
    fn anthropic_defaults_to_its_api_and_current_claude_models() {
        assert!(config(&[("AI_PROVIDER", "anthropic")]).is_err());
        let config = config(&[("AI_PROVIDER", "anthropic"), ("AI_API_KEY", "sk-ant-test")])
            .unwrap()
            .unwrap();
        assert_eq!(config.base_url, "https://api.anthropic.com/v1");
        assert_eq!(config.api_key.unwrap().expose(), "sk-ant-test");
        assert_eq!(
            config.models,
            [
                model("claude-opus-5-5", None),
                model("claude-sonnet-5-5", None),
                model("claude-haiku-5-5", None),
            ]
        );
        assert_eq!(config.default_effort, AiEffort::Medium);
        assert_eq!(config.server_monthly_budget_micros, 5_000_000);
        assert_eq!(config.account_monthly_budget_micros, 20_000_000);
        assert_eq!(config.max_concurrent_calls, 16);
    }

    #[test]
    fn openai_needs_an_endpoint_and_models_but_no_key() {
        assert!(config(&[("AI_PROVIDER", "openai"), ("AI_MODELS", "llama3.1:8b")]).is_err());
        assert!(
            config(&[
                ("AI_PROVIDER", "openai"),
                ("AI_BASE_URL", "http://ollama:11434/v1")
            ])
            .is_err()
        );
        let config = config(&[
            ("AI_PROVIDER", "openai"),
            ("AI_BASE_URL", "https://ollama.example.com/v1/"),
            (
                "AI_MODELS",
                " claude-opus-5-5 , gpt-oss-120b=0.15/0.6,, qwen/qwen3:32b=0/0 ",
            ),
            ("AI_DEFAULT_EFFORT", "high"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(config.provider, AiProvider::OpenAi);
        assert_eq!(config.base_url, "https://ollama.example.com/v1");
        assert!(config.api_key.is_none());
        assert_eq!(
            config.models,
            [
                model("claude-opus-5-5", None),
                model("gpt-oss-120b", Some((150_000, 600_000))),
                model("qwen/qwen3:32b", Some((0, 0))),
            ]
        );
        assert_eq!(config.default_effort, AiEffort::High);
    }

    #[test]
    fn invalid_model_lists_and_keys_are_refused() {
        let long_id = "m".repeat(65);
        for models in [
            ",",
            "gpt-oss-120b=1",
            "gpt-oss-120b=1/x",
            "=1/1",
            "two words",
            "gpt-oss-120b,gpt-oss-120b",
            long_id.as_str(),
        ] {
            let result = config(&[
                ("AI_PROVIDER", "openai"),
                ("AI_BASE_URL", "http://ollama:11434/v1"),
                ("AI_MODELS", models),
            ]);
            assert!(result.is_err(), "{models}");
        }
        assert!(
            config(&[
                ("AI_PROVIDER", "openai"),
                ("AI_BASE_URL", "ollama:11434"),
                ("AI_MODELS", "llama3.1:8b"),
            ])
            .is_err()
        );
        assert!(config(&[("AI_PROVIDER", "anthropic"), ("AI_API_KEY", "two words")]).is_err());
    }

    #[test]
    fn dollar_amounts_parse_to_exact_micros() {
        assert_eq!(parse_usd_micros("5"), Some(5_000_000));
        assert_eq!(parse_usd_micros("0.25"), Some(250_000));
        assert_eq!(parse_usd_micros("12.000001"), Some(12_000_001));
        assert_eq!(parse_usd_micros("7."), Some(7_000_000));
        assert_eq!(parse_usd_micros("0"), Some(0));
        for invalid in [
            "",
            ".5",
            "-1",
            "1.2345678",
            "1e3",
            "one",
            "1.5.0",
            "9223372036855",
        ] {
            assert_eq!(parse_usd_micros(invalid), None, "{invalid}");
        }
    }
}

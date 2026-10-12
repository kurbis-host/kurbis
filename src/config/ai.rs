//! AI gateway configuration. The gateway is enabled only when a model provider
//! API key is configured; every other setting has a production default.

use std::env;

use anyhow::{Context, Result, bail, ensure};

use crate::contracts::ai::AiEffort;

use super::SecretString;

pub const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
pub const DEFAULT_AI_MODEL: &str = "claude-opus-5-5";

#[derive(Clone, Debug)]
pub struct AiConfig {
    pub api_key: SecretString,
    pub base_url: String,
    pub default_model: String,
    pub default_effort: AiEffort,
    pub server_monthly_budget_micros: i64,
    pub account_monthly_budget_micros: i64,
    /// Model calls the gateway runs at the same time across all servers.
    pub max_concurrent_calls: usize,
}

impl AiConfig {
    pub fn from_env() -> Result<Option<Self>> {
        let api_key = match env::var("ANTHROPIC_API_KEY") {
            Ok(key) if !key.trim().is_empty() => SecretString::new(key.trim()),
            Ok(_) | Err(env::VarError::NotPresent) => return Ok(None),
            Err(error) => return Err(error).context("ANTHROPIC_API_KEY must be valid UTF-8"),
        };
        let base_url = setting("ANTHROPIC_BASE_URL", DEFAULT_ANTHROPIC_BASE_URL)
            .trim_end_matches('/')
            .to_owned();
        ensure!(
            base_url.starts_with("https://") || base_url.starts_with("http://"),
            "ANTHROPIC_BASE_URL must be an HTTP(S) URL"
        );
        let Some(default_effort) =
            AiEffort::parse(&setting("AI_DEFAULT_EFFORT", AiEffort::Medium.as_str()))
        else {
            bail!("AI_DEFAULT_EFFORT must be low, medium, or high");
        };
        let max_concurrent_calls = setting("AI_MAX_CONCURRENT_CALLS", "16")
            .parse::<usize>()
            .ok()
            .filter(|value| (1..=256).contains(value))
            .context("AI_MAX_CONCURRENT_CALLS must be between 1 and 256")?;
        Ok(Some(Self {
            api_key,
            base_url,
            default_model: setting("AI_DEFAULT_MODEL", DEFAULT_AI_MODEL),
            default_effort,
            server_monthly_budget_micros: usd_micros_env("AI_SERVER_MONTHLY_BUDGET_USD", "5")?,
            account_monthly_budget_micros: usd_micros_env("AI_ACCOUNT_MONTHLY_BUDGET_USD", "20")?,
            max_concurrent_calls,
        }))
    }
}

/// An optional setting. Unset and blank both select the default, so a
/// deployment can pass every variable through and leave the ones it does not
/// set empty.
fn setting(name: &str, default: &str) -> String {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => value.trim().to_owned(),
        _ => default.to_owned(),
    }
}

fn usd_micros_env(name: &str, default: &str) -> Result<i64> {
    parse_usd_micros(&setting(name, default)).with_context(|| {
        format!("{name} must be a non-negative US dollar amount with at most six decimals")
    })
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
    use super::parse_usd_micros;

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

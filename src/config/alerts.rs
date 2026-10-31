//! Operator alerts. They are sent only when `ALERT_NTFY_URL` names the ntfy
//! topic to publish to.

use std::env;

use anyhow::{Context, Result, ensure};

use super::SecretString;

#[derive(Clone, Debug)]
pub struct AlertConfig {
    /// The full topic URL, such as `https://ntfy.sh/kurbis-ops`.
    pub ntfy_url: String,
    /// Sent as a bearer token when the topic needs one.
    pub ntfy_token: Option<SecretString>,
}

impl AlertConfig {
    pub fn from_env() -> Result<Option<Self>> {
        Self::from_vars(|name| match env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(env::VarError::NotPresent) => Ok(None),
            Err(error) => Err(error).with_context(|| format!("{name} must be valid UTF-8")),
        })
    }

    fn from_vars(var: impl Fn(&str) -> Result<Option<String>>) -> Result<Option<Self>> {
        // Blank is unset, so a deployment can pass every variable through.
        let setting = |name: &str| -> Result<Option<String>> {
            Ok(var(name)?
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()))
        };
        let Some(ntfy_url) = setting("ALERT_NTFY_URL")? else {
            return Ok(None);
        };
        ensure!(
            ntfy_url.starts_with("https://") || ntfy_url.starts_with("http://"),
            "ALERT_NTFY_URL must be an HTTP(S) URL"
        );
        let ntfy_token = setting("ALERT_NTFY_TOKEN")?;
        ensure!(
            ntfy_token
                .as_deref()
                .is_none_or(|token| token.bytes().all(|byte| byte.is_ascii_graphic())),
            "ALERT_NTFY_TOKEN must be printable ASCII without spaces"
        );
        Ok(Some(Self {
            ntfy_url,
            ntfy_token: ntfy_token.map(SecretString::new),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(vars: &[(&str, &str)]) -> Result<Option<AlertConfig>> {
        AlertConfig::from_vars(|name| {
            Ok(vars
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned()))
        })
    }

    #[test]
    fn alerts_are_off_unless_a_topic_is_set() {
        assert!(parse(&[]).unwrap().is_none());
        assert!(parse(&[("ALERT_NTFY_URL", "  ")]).unwrap().is_none());
    }

    #[test]
    fn a_topic_and_token_are_read_and_bad_values_rejected() {
        let config = parse(&[
            ("ALERT_NTFY_URL", " https://ntfy.sh/kurbis-ops "),
            ("ALERT_NTFY_TOKEN", "tk_abc"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(config.ntfy_url, "https://ntfy.sh/kurbis-ops");
        assert_eq!(config.ntfy_token.unwrap().expose(), "tk_abc");
        assert!(parse(&[("ALERT_NTFY_URL", "ntfy.sh/kurbis-ops")]).is_err());
        assert!(
            parse(&[
                ("ALERT_NTFY_URL", "https://ntfy.sh/kurbis-ops"),
                ("ALERT_NTFY_TOKEN", "two words"),
            ])
            .is_err()
        );
    }
}

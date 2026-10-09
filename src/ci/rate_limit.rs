//! Shared and durable pacing for GitHub source and Check Run requests.
//!
//! GitHub's core and secondary limits are shared by every endpoint using one
//! token.  Keep one guard for the process and persist its lease in Postgres so
//! a control-plane restart cannot immediately recreate the request herd.
use std::{collections::HashMap, sync::Arc, time::Duration};

use chrono::{DateTime, TimeDelta, Utc};
use reqwest::{StatusCode, header::HeaderMap};
use sqlx::PgPool;
use tokio::{sync::Mutex, time::sleep};

use super::source_control::PollError;

#[derive(Debug, Clone)]
pub struct GitHubRateLimitConfig {
    pub minimum_interval: Duration,
    pub max_backoff: Duration,
    pub jitter: f64,
}

impl Default for GitHubRateLimitConfig {
    fn default() -> Self {
        Self {
            minimum_interval: Duration::from_secs(2),
            max_backoff: Duration::from_secs(900),
            jitter: 0.2,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct GitHubRateLimitDiagnostics {
    pub remaining: Option<u64>,
    pub reset_at: Option<DateTime<Utc>>,
    pub paused_until: Option<DateTime<Utc>>,
    pub next_allowed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
struct State {
    remaining: Option<u64>,
    reset_at: Option<DateTime<Utc>>,
    paused_until: Option<DateTime<Utc>>,
    next_allowed_at: Option<DateTime<Utc>>,
    failures: u32,
}

#[derive(Clone)]
pub struct GitHubRateLimitGuard {
    config: GitHubRateLimitConfig,
    state: Arc<Mutex<HashMap<String, State>>>,
    pool: Option<PgPool>,
}

impl GitHubRateLimitGuard {
    pub fn new(config: GitHubRateLimitConfig) -> Self {
        Self {
            config: GitHubRateLimitConfig {
                jitter: config.jitter.clamp(0.0, 1.0),
                ..config
            },
            state: Arc::new(Mutex::new(HashMap::new())),
            pool: None,
        }
    }

    pub fn with_store(mut self, pool: PgPool) -> Self {
        self.pool = Some(pool);
        self
    }

    pub fn config(&self) -> &GitHubRateLimitConfig {
        &self.config
    }

    pub async fn diagnostics(&self, scope: &str) -> GitHubRateLimitDiagnostics {
        self.refresh_from_store(scope).await;
        let state = self
            .state
            .lock()
            .await
            .get(scope)
            .cloned()
            .unwrap_or_default();
        GitHubRateLimitDiagnostics {
            remaining: state.remaining,
            reset_at: state.reset_at,
            paused_until: state.paused_until,
            next_allowed_at: state.next_allowed_at,
        }
    }

    /// Pause an optional feature without marking the shared core quota
    /// exhausted. This is used for a stable Checks permission failure.
    pub async fn pause_for(&self, scope: &str, duration: Duration) {
        let until = Utc::now() + TimeDelta::from_std(duration).unwrap_or(TimeDelta::MAX);
        let snapshot = {
            let mut states = self.state.lock().await;
            let state = states.entry(scope.to_owned()).or_default();
            state.paused_until = Some(state.paused_until.map_or(until, |old| old.max(until)));
            state.next_allowed_at = state.paused_until;
            state.clone()
        };
        self.save(scope, &snapshot).await;
    }

    /// Reserve one request slot. Normal pacing waits in-place; only a provider
    /// imposed pause is returned as `RateLimited` so callers can back off their
    /// poll loop without dropping a successful build admission.
    pub async fn before_request(&self, scope: &str) -> Result<(), PollError> {
        if let Some(result) = self.reserve_from_store(scope).await {
            let delay = result?;
            if !delay.is_zero() {
                sleep(delay).await;
            }
            return Ok(());
        }

        let (delay, paused) = {
            let mut states = self.state.lock().await;
            let state = states.entry(scope.to_owned()).or_default();
            let now = Utc::now();
            let paused_until = state
                .paused_until
                .or(state.reset_at.filter(|_| state.remaining == Some(0)));
            if let Some(until) = paused_until.filter(|value| *value > now) {
                (Duration::ZERO, Some(to_duration(until - now)))
            } else {
                if state.paused_until.is_some_and(|until| until <= now) {
                    state.paused_until = None;
                }
                let start_at = state.next_allowed_at.unwrap_or(now).max(now);
                let delay = to_duration(start_at - now);
                state.next_allowed_at = Some(
                    start_at
                        + TimeDelta::from_std(self.config.minimum_interval)
                            .unwrap_or(TimeDelta::MAX),
                );
                (delay, None)
            }
        };
        if let Some(retry_after) = paused {
            return Err(PollError::RateLimited {
                retry_after: Some(retry_after),
            });
        }
        if !delay.is_zero() {
            sleep(delay).await;
        }
        Ok(())
    }

    /// Observe provider headers/body after every request. A successful 200
    /// with `X-RateLimit-Remaining: 0` remains successful, but pauses the next
    /// request until reset. A 403 is only classified as a rate limit when its
    /// response body says so; permission-denied Check Run responses stay plain
    /// HTTP errors and are handled as an optional status side effect.
    pub async fn observe_response(
        &self,
        scope: &str,
        status: StatusCode,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Option<Duration> {
        self.refresh_from_store(scope).await;
        let now = Utc::now();
        let remaining = header_u64(headers, "x-ratelimit-remaining");
        let reset_at = header_u64(headers, "x-ratelimit-reset")
            .and_then(|value| DateTime::from_timestamp(value as i64, 0));
        let retry_header = retry_after(headers, now);
        let body_rate = std::str::from_utf8(&body[..body.len().min(4096)])
            .ok()
            .map(str::to_ascii_lowercase)
            .is_some_and(|body| body.contains("rate limit") || body.contains("secondary rate"));
        let provider_limited = status == StatusCode::TOO_MANY_REQUESTS
            || retry_header.is_some()
            || (status == StatusCode::FORBIDDEN && body_rate);
        let exhausted = remaining == Some(0);
        let (result, snapshot) = {
            let mut states = self.state.lock().await;
            let state = states.entry(scope.to_owned()).or_default();
            state.remaining = remaining.or(state.remaining);
            state.reset_at = reset_at.or(state.reset_at);
            if provider_limited {
                state.failures = state.failures.saturating_add(1);
                let exponential =
                    self.backoff(state.failures)
                        .max(if status == StatusCode::FORBIDDEN {
                            Duration::from_secs(60)
                        } else {
                            Duration::ZERO
                        });
                let reset_delay = state
                    .reset_at
                    .map(|reset| to_duration((reset - now).max(TimeDelta::zero())));
                let delay = retry_header
                    .or(reset_delay)
                    .unwrap_or(exponential)
                    .max(exponential);
                let until = now + TimeDelta::from_std(delay).unwrap_or(TimeDelta::MAX);
                state.paused_until = Some(state.paused_until.map_or(until, |old| old.max(until)));
                state.next_allowed_at = state.paused_until;
                (
                    Some(to_duration(state.paused_until.unwrap() - now)),
                    state.clone(),
                )
            } else {
                state.failures = 0;
                if exhausted
                    && let Some(reset) = state.reset_at
                    && reset > now
                {
                    state.paused_until =
                        Some(state.paused_until.map_or(reset, |old| old.max(reset)));
                    state.next_allowed_at = state.paused_until;
                } else if state.paused_until.is_some_and(|until| until <= now) {
                    state.paused_until = None;
                }
                (None, state.clone())
            }
        };
        self.save(scope, &snapshot).await;
        result
    }

    fn backoff(&self, failures: u32) -> Duration {
        let base = self.config.minimum_interval.max(Duration::from_secs(1));
        let raw = base
            .checked_mul(1u32 << failures.saturating_sub(1).min(16))
            .unwrap_or(self.config.max_backoff)
            .min(self.config.max_backoff);
        if self.config.jitter == 0.0 {
            return raw;
        }
        let multiplier = (1.0 + (rand::random::<f64>() * 2.0 - 1.0) * self.config.jitter).max(0.0);
        raw.mul_f64(multiplier).min(self.config.max_backoff)
    }

    /// Atomically reserve a persisted request slot. The INSERT is deliberately
    /// separate from UPDATE so the first request starts immediately while two
    /// control-plane processes still serialize their subsequent reservations on
    /// the row lock taken by PostgreSQL's UPDATE.
    async fn reserve_from_store(&self, scope: &str) -> Option<Result<Duration, PollError>> {
        let pool = self.pool.as_ref()?;
        let interval_ms = self.config.minimum_interval.as_millis() as f64;
        if let Err(error) = sqlx::query(
            "INSERT INTO ci_github_rate_limits (scope, next_allowed_at)
             VALUES ($1, now()) ON CONFLICT (scope) DO NOTHING",
        )
        .bind(scope)
        .execute(pool)
        .await
        {
            tracing::warn!(scope = %sanitize_scope(scope), ?error, "failed to create GitHub rate-limit state; using local guard");
            return None;
        }
        let row = match sqlx::query_as::<_, PersistedState>(
            "UPDATE ci_github_rate_limits
             SET next_allowed_at = CASE
                 WHEN paused_until IS NOT NULL AND paused_until > now() THEN next_allowed_at
                 ELSE GREATEST(COALESCE(next_allowed_at, now()), now())
                      + ($2::double precision * interval '1 millisecond')
                 END,
                 updated_at = now()
             WHERE scope = $1
             RETURNING remaining, reset_at, paused_until, next_allowed_at,
                       consecutive_failures",
        )
        .bind(scope)
        .bind(interval_ms)
        .fetch_one(pool)
        .await
        {
            Ok(row) => row,
            Err(error) => {
                tracing::warn!(scope = %sanitize_scope(scope), ?error, "failed to reserve GitHub rate-limit state; using local guard");
                return None;
            }
        };
        let state = State::from(row);
        self.state
            .lock()
            .await
            .insert(scope.to_owned(), state.clone());
        let now = Utc::now();
        if let Some(until) = state
            .paused_until
            .or(state.reset_at.filter(|_| state.remaining == Some(0)))
            .filter(|value| *value > now)
        {
            return Some(Err(PollError::RateLimited {
                retry_after: Some(to_duration(until - now)),
            }));
        }
        let start_at = state.next_allowed_at.unwrap_or(now)
            - TimeDelta::from_std(self.config.minimum_interval).unwrap_or(TimeDelta::MAX);
        Some(Ok(to_duration((start_at - now).max(TimeDelta::zero()))))
    }

    async fn refresh_from_store(&self, scope: &str) {
        let Some(pool) = &self.pool else {
            return;
        };
        let result = sqlx::query_as::<_, PersistedState>(
            "SELECT remaining, reset_at, paused_until, next_allowed_at,
                    consecutive_failures
             FROM ci_github_rate_limits WHERE scope = $1",
        )
        .bind(scope)
        .fetch_optional(pool)
        .await;
        if let Ok(Some(row)) = result {
            self.state
                .lock()
                .await
                .insert(scope.to_owned(), State::from(row));
        }
    }

    async fn save(&self, scope: &str, state: &State) {
        self.state
            .lock()
            .await
            .insert(scope.to_owned(), state.clone());
        let Some(pool) = &self.pool else {
            return;
        };
        if let Err(error) = sqlx::query(
            "INSERT INTO ci_github_rate_limits
                 (scope, remaining, reset_at, paused_until, next_allowed_at,
                  consecutive_failures, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, now())
             ON CONFLICT(scope) DO UPDATE SET
                 remaining = COALESCE(EXCLUDED.remaining,
                                      ci_github_rate_limits.remaining),
                 reset_at = COALESCE(EXCLUDED.reset_at,
                                     ci_github_rate_limits.reset_at),
                 paused_until = CASE
                     WHEN EXCLUDED.paused_until IS NULL
                          AND ci_github_rate_limits.paused_until <= now() THEN NULL
                     WHEN EXCLUDED.paused_until IS NULL THEN ci_github_rate_limits.paused_until
                     WHEN ci_github_rate_limits.paused_until IS NULL
                          OR EXCLUDED.paused_until > ci_github_rate_limits.paused_until
                       THEN EXCLUDED.paused_until
                     ELSE ci_github_rate_limits.paused_until
                 END,
                 next_allowed_at = CASE
                     WHEN EXCLUDED.next_allowed_at IS NULL
                       THEN ci_github_rate_limits.next_allowed_at
                     WHEN ci_github_rate_limits.next_allowed_at IS NULL
                       THEN EXCLUDED.next_allowed_at
                     ELSE GREATEST(ci_github_rate_limits.next_allowed_at,
                                   EXCLUDED.next_allowed_at)
                 END,
                 consecutive_failures = EXCLUDED.consecutive_failures,
                 updated_at = now()",
        )
        .bind(scope)
        .bind(
            state
                .remaining
                .map(|value| i64::try_from(value).unwrap_or(i64::MAX)),
        )
        .bind(state.reset_at)
        .bind(state.paused_until)
        .bind(state.next_allowed_at)
        .bind(i32::try_from(state.failures).unwrap_or(i32::MAX))
        .execute(pool)
        .await
        {
            tracing::warn!(scope = %sanitize_scope(scope), ?error, "failed to persist GitHub rate-limit state");
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct PersistedState {
    remaining: Option<i64>,
    reset_at: Option<DateTime<Utc>>,
    paused_until: Option<DateTime<Utc>>,
    next_allowed_at: Option<DateTime<Utc>>,
    consecutive_failures: i32,
}

impl From<PersistedState> for State {
    fn from(value: PersistedState) -> Self {
        Self {
            remaining: value.remaining.and_then(|v| u64::try_from(v).ok()),
            reset_at: value.reset_at,
            paused_until: value.paused_until,
            next_allowed_at: value.next_allowed_at,
            failures: u32::try_from(value.consecutive_failures).unwrap_or(0),
        }
    }
}

fn header_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.parse().ok()
}

fn retry_after(headers: &HeaderMap, now: DateTime<Utc>) -> Option<Duration> {
    let value = headers.get("retry-after")?.to_str().ok()?;
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|timestamp| to_duration((timestamp.with_timezone(&Utc) - now).max(TimeDelta::zero())))
}

fn to_duration(value: TimeDelta) -> Duration {
    value.to_std().unwrap_or_default()
}

fn sanitize_scope(scope: &str) -> String {
    scope
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderValue, RETRY_AFTER};

    fn guard() -> GitHubRateLimitGuard {
        GitHubRateLimitGuard::new(GitHubRateLimitConfig {
            minimum_interval: Duration::from_millis(10),
            max_backoff: Duration::from_secs(20),
            jitter: 0.0,
        })
    }

    #[tokio::test]
    async fn json_403_and_reset_pause() {
        let guard = guard();
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
        headers.insert(
            "x-ratelimit-reset",
            HeaderValue::from_str(&(Utc::now().timestamp() + 30).to_string()).unwrap(),
        );
        let delay = guard
            .observe_response(
                "repo/Pumpkin:pulls",
                StatusCode::FORBIDDEN,
                &headers,
                br#"{"message":"API rate limit exceeded"}"#,
            )
            .await
            .unwrap();
        assert!(delay >= Duration::from_secs(1));
        assert!(matches!(
            guard.before_request("repo/Pumpkin:pulls").await,
            Err(PollError::RateLimited { .. })
        ));
    }

    #[tokio::test]
    async fn retry_after_is_honoured() {
        let guard = guard();
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("7"));
        let delay = guard
            .observe_response("check", StatusCode::TOO_MANY_REQUESTS, &headers, b"{}")
            .await
            .unwrap();
        assert!(delay >= Duration::from_secs(6));
    }

    #[test]
    fn retry_after_supports_http_date() {
        let mut headers = HeaderMap::new();
        let future = Utc::now() + TimeDelta::seconds(20);
        headers.insert(
            RETRY_AFTER,
            HeaderValue::from_str(&future.to_rfc2822()).unwrap(),
        );
        assert!(retry_after(&headers, Utc::now()).unwrap() >= Duration::from_secs(19));
    }

    #[tokio::test]
    async fn feature_pause_does_not_pause_shared_core_scope() {
        let guard = guard();
        guard
            .pause_for("github:checks:permission", Duration::from_secs(60))
            .await;
        assert!(matches!(
            guard.before_request("github:checks:permission").await,
            Err(PollError::RateLimited { .. })
        ));
        assert!(guard.before_request("github:rest:core").await.is_ok());
    }

    #[tokio::test]
    async fn ordinary_forbidden_is_not_paused() {
        let guard = guard();
        assert!(
            guard
                .observe_response(
                    "check",
                    StatusCode::FORBIDDEN,
                    &HeaderMap::new(),
                    br#"{"message":"not allowed"}"#,
                )
                .await
                .is_none()
        );
        assert!(guard.before_request("check").await.is_ok());
    }

    #[tokio::test]
    async fn exhausted_success_pauses_without_turning_response_into_failure() {
        let guard = guard();
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
        headers.insert(
            "x-ratelimit-reset",
            HeaderValue::from_str(&(Utc::now().timestamp() + 30).to_string()).unwrap(),
        );
        assert!(
            guard
                .observe_response("github:core", StatusCode::OK, &headers, b"[]")
                .await
                .is_none()
        );
        assert!(matches!(
            guard.before_request("github:core").await,
            Err(PollError::RateLimited { .. })
        ));
    }
}

//! AI gateway for the in-game assistant.
//!
//! The kurbis-control plugin turns an operator's `/ai` command into a job. The
//! runner claims queued jobs and calls Claude with the call's worst-case cost
//! reserved against the server and account budgets. Tool calls the model asks
//! for wait in the database until the plugin's sync picks them up, runs them
//! in game and returns the results; the job then continues with its next model
//! call. Every model attempt is priced into the `ai_usage` ledger, which the
//! monthly budgets are sums over. Chat lines for the operator travel back
//! through the same sync.

mod anthropic;
mod budget;
pub mod catalog;
mod jobs;
mod prompt;
mod runner;
mod settings;
mod sync;
mod transitions;

use std::sync::Arc;

use anyhow::Context;
use sqlx::PgPool;
use tokio::sync::{Mutex, Notify};

use crate::config::AiConfig;

pub use jobs::{cancel_job, job_detail, overview};
pub use sync::sync;

/// Handle to the gateway; cheap to clone.
#[derive(Clone)]
pub struct AiGateway {
    inner: Arc<Inner>,
}

struct Inner {
    pool: PgPool,
    config: AiConfig,
    default_model: &'static catalog::ModelSpec,
    client: anthropic::AnthropicClient,
    /// Signalled when a job becomes runnable, so the runner does not wait for
    /// its next tick.
    wake: Notify,
    maintenance: Mutex<runner::Maintenance>,
}

impl AiGateway {
    pub fn new(pool: PgPool, config: AiConfig) -> anyhow::Result<Self> {
        let default_model = catalog::selectable(&config.default_model).with_context(|| {
            format!(
                "AI_DEFAULT_MODEL {} is not one of the offered models",
                config.default_model
            )
        })?;
        let client = anthropic::AnthropicClient::new(&config)?;
        Ok(Self {
            inner: Arc::new(Inner {
                pool,
                config,
                default_model,
                client,
                wake: Notify::new(),
                maintenance: Mutex::default(),
            }),
        })
    }

    fn pool(&self) -> &PgPool {
        &self.inner.pool
    }

    fn config(&self) -> &AiConfig {
        &self.inner.config
    }

    fn default_model(&self) -> &'static catalog::ModelSpec {
        self.inner.default_model
    }

    fn client(&self) -> &anthropic::AnthropicClient {
        &self.inner.client
    }

    /// Let the runner pick up newly runnable jobs immediately.
    pub fn wake(&self) {
        self.inner.wake.notify_one();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("The AI assistant is turned off for this server.")]
    Disabled,
    #[error("That model is not offered.")]
    UnknownModel,
    #[error("A server's AI budget cannot be higher than the account's AI budget.")]
    BudgetAboveAccount,
    #[error("Ask a question after the command.")]
    EmptyPrompt,
    #[error("This request id already belongs to a different request.")]
    JobIdReused,
    #[error("That AI request does not exist.")]
    JobNotFound,
    #[error("This server's AI budget for this month is used up.")]
    ServerBudgetExhausted,
    #[error("The owner's AI budget for this month is used up.")]
    AccountBudgetExhausted,
    #[error("Your previous request is still running; wait for it or use /ai cancel.")]
    JobActive,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

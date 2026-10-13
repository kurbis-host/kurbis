//! Monthly budgets over the usage ledger.
//!
//! Budgets cover the current calendar month in UTC. Spending is the ledger
//! sum; in-flight model calls additionally hold a reservation of their worst
//! case cost until they settle, so concurrent calls cannot overrun a budget.

use chrono::{DateTime, Utc};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use crate::contracts::ai::{AiBudget, AiUsage};

use super::{
    catalog::{Catalog, Tokens},
    transitions::Tx,
};

const PERIOD_START: &str = "date_trunc('month', now(), 'UTC')";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Spending {
    pub spent: i64,
    pub reserved: i64,
}

impl Spending {
    pub fn remaining(&self, budget: i64) -> i64 {
        budget
            .saturating_sub(self.spent)
            .saturating_sub(self.reserved)
    }

    fn budget(self, budget_micros: i64) -> AiBudget {
        AiBudget {
            spent_micros: self.spent,
            reserved_micros: self.reserved,
            budget_micros,
        }
    }
}

pub async fn server_spending(
    executor: impl PgExecutor<'_>,
    server_id: Uuid,
) -> Result<Spending, sqlx::Error> {
    let (spent, reserved): (i64, i64) = sqlx::query_as(&format!(
        "SELECT
           (SELECT COALESCE(SUM(cost_micros), 0)::bigint FROM ai_usage
            WHERE server_id = $1 AND created_at >= {PERIOD_START}),
           (SELECT COALESCE(SUM(reserved_micros), 0)::bigint FROM ai_jobs
            WHERE server_id = $1 AND reserved_micros > 0 AND reserved_until > now())"
    ))
    .bind(server_id)
    .fetch_one(executor)
    .await?;
    Ok(Spending { spent, reserved })
}

pub async fn account_spending(
    executor: impl PgExecutor<'_>,
    owner_id: Uuid,
) -> Result<Spending, sqlx::Error> {
    let (spent, reserved): (i64, i64) = sqlx::query_as(&format!(
        "SELECT
           (SELECT COALESCE(SUM(cost_micros), 0)::bigint FROM ai_usage
            WHERE owner_id = $1 AND created_at >= {PERIOD_START}),
           (SELECT COALESCE(SUM(j.reserved_micros), 0)::bigint
            FROM ai_jobs j JOIN servers s ON s.id = j.server_id
            WHERE s.owner_id = $1 AND j.reserved_micros > 0 AND j.reserved_until > now())"
    ))
    .bind(owner_id)
    .fetch_one(executor)
    .await?;
    Ok(Spending { spent, reserved })
}

/// Serialize budget decisions of one account until the transaction ends.
pub async fn lock_account(tx: &mut Tx<'_>, owner_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("kurbis-ai-budget:{owner_id}"))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Which cap stops a request first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exhausted {
    Server,
    Account,
}

pub struct Budgets {
    pub server: Spending,
    pub account: Spending,
    pub server_budget: i64,
    pub account_budget: i64,
}

impl Budgets {
    pub async fn load(
        tx: &mut Tx<'_>,
        server_id: Uuid,
        owner_id: Uuid,
        server_budget: i64,
        account_budget: i64,
    ) -> Result<Self, sqlx::Error> {
        Ok(Self {
            server: server_spending(&mut **tx, server_id).await?,
            account: account_spending(&mut **tx, owner_id).await?,
            server_budget,
            account_budget,
        })
    }

    /// Money still available to new model calls, and the cap that limits it.
    pub fn available(&self) -> (i64, Exhausted) {
        let server = self.server.remaining(self.server_budget);
        let account = self.account.remaining(self.account_budget);
        if account < server {
            (account, Exhausted::Account)
        } else {
            (server, Exhausted::Server)
        }
    }
}

/// Price every attempt of a model response into the ledger; returns the
/// total cost in micro-USD.
pub async fn record_usage(
    tx: &mut Tx<'_>,
    catalog: &Catalog,
    owner_id: Uuid,
    server_id: Uuid,
    job_id: Uuid,
    attempts: &[(String, Tokens)],
) -> Result<i64, sqlx::Error> {
    let mut total = 0i64;
    for (model, tokens) in attempts {
        let cost = catalog.cost_micros(model, *tokens);
        total = total.saturating_add(cost);
        sqlx::query(
            "INSERT INTO ai_usage(owner_id, server_id, job_id, model, input_tokens,
                                  output_tokens, cache_creation_input_tokens,
                                  cache_read_input_tokens, cost_micros)
             VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(owner_id)
        .bind(server_id)
        .bind(job_id)
        .bind(model.chars().take(128).collect::<String>())
        .bind(clamp_tokens(tokens.input))
        .bind(clamp_tokens(tokens.output))
        .bind(clamp_tokens(tokens.cache_creation))
        .bind(clamp_tokens(tokens.cache_read))
        .bind(cost)
        .execute(&mut **tx)
        .await?;
    }
    Ok(total)
}

fn clamp_tokens(tokens: u64) -> i64 {
    i64::try_from(tokens).unwrap_or(i64::MAX)
}

#[derive(sqlx::FromRow)]
struct LedgerTotals {
    period_start: DateTime<Utc>,
    model_calls: i64,
    input_tokens: i64,
    output_tokens: i64,
    cache_creation_input_tokens: i64,
    cache_read_input_tokens: i64,
}

/// This month's usage of a server and its owner's account.
pub async fn usage(
    pool: &PgPool,
    server_id: Uuid,
    owner_id: Uuid,
    server_budget: i64,
    account_budget: i64,
) -> Result<AiUsage, sqlx::Error> {
    let totals: LedgerTotals = sqlx::query_as(&format!(
        "SELECT {PERIOD_START} AS period_start,
                COUNT(*)::bigint AS model_calls,
                COALESCE(SUM(input_tokens), 0)::bigint AS input_tokens,
                COALESCE(SUM(output_tokens), 0)::bigint AS output_tokens,
                COALESCE(SUM(cache_creation_input_tokens), 0)::bigint
                    AS cache_creation_input_tokens,
                COALESCE(SUM(cache_read_input_tokens), 0)::bigint AS cache_read_input_tokens
         FROM ai_usage
         WHERE server_id = $1 AND created_at >= {PERIOD_START}"
    ))
    .bind(server_id)
    .fetch_one(pool)
    .await?;
    Ok(AiUsage {
        period_start: totals.period_start,
        server: server_spending(pool, server_id)
            .await?
            .budget(server_budget),
        account: account_spending(pool, owner_id)
            .await?
            .budget(account_budget),
        model_calls: totals.model_calls,
        input_tokens: totals.input_tokens,
        output_tokens: totals.output_tokens,
        cache_creation_input_tokens: totals.cache_creation_input_tokens,
        cache_read_input_tokens: totals.cache_read_input_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tighter_cap_limits_new_calls() {
        let budgets = Budgets {
            server: Spending {
                spent: 4_000_000,
                reserved: 500_000,
            },
            account: Spending {
                spent: 19_900_000,
                reserved: 0,
            },
            server_budget: 5_000_000,
            account_budget: 20_000_000,
        };
        assert_eq!(budgets.available(), (100_000, Exhausted::Account));
        let budgets = Budgets {
            account: Spending::default(),
            ..budgets
        };
        assert_eq!(budgets.available(), (500_000, Exhausted::Server));
        assert_eq!(
            Spending {
                spent: 9,
                reserved: 9
            }
            .remaining(10),
            -8
        );
    }
}

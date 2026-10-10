//! Reclaim expired CI coordination state without touching account servers.
//!
//! This command is intentionally conservative: it never deletes previews,
//! worlds, artifacts, builds, or production deployment rows. By default it
//! only reports candidates. Applying a report requires an exact confirmation
//! string and a new backup file containing the candidate identities.

use std::{fs::OpenOptions, io::Write, path::PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use clap::Parser;
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use uuid::Uuid;

const CONFIRMATION: &str = "RECLAIM EXPIRED CI COORDINATION LEASES";
const DEFAULT_LIMIT: i64 = 128;
const MAX_LIMIT: i64 = 1024;

#[derive(Debug, Parser)]
#[command(
    name = "ci-cleanup",
    about = "Report or reclaim expired CI coordination leases"
)]
struct Arguments {
    /// PostgreSQL URL. Defaults to DATABASE_URL.
    #[arg(long, env = "DATABASE_URL")]
    database_url: String,

    /// Apply the exact candidates in the report instead of only printing them.
    #[arg(long)]
    apply: bool,

    /// Must equal the printed confirmation phrase when --apply is used.
    #[arg(long)]
    confirm: Option<String>,

    /// New file that receives the candidate snapshot before any mutation.
    /// Existing files are rejected so every apply has an immutable audit input.
    #[arg(long, value_name = "PATH")]
    backup_file: Option<PathBuf>,

    /// Operator-supplied identifier for a verified PostgreSQL backup/snapshot.
    /// The command records it but cannot validate the external backup system.
    #[arg(long, value_name = "ID_OR_PATH")]
    database_backup_reference: Option<String>,

    /// Also delete one stale build-admission lease after its external builder
    /// resources were removed by the operator. The token comes from a dry run.
    #[arg(long, value_name = "UUID")]
    admission_lease_token: Option<Uuid>,

    /// Required with --admission-lease-token to make the builder scope explicit.
    #[arg(long)]
    builder_identity: Option<String>,

    /// Required with --admission-lease-token. The command cannot inspect or
    /// remove Docker/Bubblewrap resources itself.
    #[arg(long)]
    admission_resources_cleaned: bool,

    /// Maximum candidates per category. Lower values keep transactions bounded.
    #[arg(long, default_value_t = DEFAULT_LIMIT, value_parser = clap::value_parser!(i64).range(1..=MAX_LIMIT))]
    limit: i64,
}

#[derive(Debug, Serialize)]
struct Candidate {
    category: &'static str,
    id: String,
    details: Value,
}

#[derive(Debug, Serialize)]
struct CandidateReport {
    generated_at: DateTime<Utc>,
    limit: i64,
    database_backup_reference: Option<String>,
    candidates: Vec<Candidate>,
    truncated: bool,
}

#[derive(Debug, Serialize)]
struct ApplyResult {
    dry_run: bool,
    report: CandidateReport,
    reclaimed: ReclaimedCounts,
}

#[derive(Debug, Default, Serialize)]
struct ReclaimedCounts {
    build_leases: u64,
    check_run_claims: u64,
    webhook_claims: u64,
    poll_leases: u64,
    runtime_cleanup_claims: u64,
    admission_leases: u64,
}

fn validate_arguments(arguments: &Arguments, report: &CandidateReport) -> Result<()> {
    if !arguments.apply {
        if arguments.confirm.is_some()
            || arguments.backup_file.is_some()
            || arguments.database_backup_reference.is_some()
            || arguments.admission_lease_token.is_some()
            || arguments.admission_resources_cleaned
        {
            bail!("--confirm, --backup-file, and admission cleanup flags require --apply");
        }
        return Ok(());
    }
    if arguments.confirm.as_deref() != Some(CONFIRMATION) {
        bail!("--apply requires --confirm '{CONFIRMATION}'");
    }
    if arguments.backup_file.is_none() {
        bail!("--apply requires --backup-file pointing to a new audit file");
    }
    if arguments
        .database_backup_reference
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
    {
        bail!("--apply requires --database-backup-reference for a verified PostgreSQL backup");
    }
    if report.truncated {
        bail!("candidate report is truncated; rerun with a larger --limit before applying");
    }
    match arguments.admission_lease_token {
        Some(_) if arguments.builder_identity.is_none() => {
            bail!("--admission-lease-token requires --builder-identity");
        }
        Some(_) if !arguments.admission_resources_cleaned => {
            bail!(
                "--admission-lease-token requires --admission-resources-cleaned after external cleanup"
            );
        }
        None if arguments.builder_identity.is_some() || arguments.admission_resources_cleaned => {
            bail!("admission scope flags require --admission-lease-token");
        }
        None => {}
        Some(_) => {}
    }
    Ok(())
}

async fn collect_report(pool: &PgPool, limit: i64) -> Result<CandidateReport> {
    let query_limit = limit + 1;
    let mut candidates = Vec::new();
    let mut truncated = false;

    let rows = sqlx::query(
        "SELECT id, repository, commit_sha, stage, lease_token, lease_expires_at
           FROM ci_builds
          WHERE lease_token IS NOT NULL AND lease_expires_at <= now()
            AND stage IN ('queued', 'fetching', 'patching', 'compiling', 'packaging')
          ORDER BY lease_expires_at, id LIMIT $1",
    )
    .bind(query_limit)
    .fetch_all(pool)
    .await?;
    truncated |= rows.len() as i64 > limit;
    for row in rows.into_iter().take(limit as usize) {
        let id: Uuid = row.try_get("id")?;
        candidates.push(Candidate {
            category: "ci_build_lease",
            id: id.to_string(),
            details: json!({
                "repository": row.try_get::<String, _>("repository")?,
                "commit_sha": row.try_get::<String, _>("commit_sha")?,
                "stage": row.try_get::<String, _>("stage")?,
                "lease_token": row.try_get::<Uuid, _>("lease_token")?.to_string(),
                "lease_expires_at": row.try_get::<DateTime<Utc>, _>("lease_expires_at")?,
            }),
        });
    }

    let rows = sqlx::query(
        "SELECT id, build_id, claim_token, claim_expires_at
           FROM ci_check_runs
          WHERE external_id IS NULL AND claim_token IS NOT NULL
            AND claim_expires_at <= now()
          ORDER BY claim_expires_at, id LIMIT $1",
    )
    .bind(query_limit)
    .fetch_all(pool)
    .await?;
    truncated |= rows.len() as i64 > limit;
    for row in rows.into_iter().take(limit as usize) {
        let id: Uuid = row.try_get("id")?;
        candidates.push(Candidate {
            category: "ci_check_run_claim",
            id: id.to_string(),
            details: json!({
                "build_id": row.try_get::<Uuid, _>("build_id")?.to_string(),
                "claim_token": row.try_get::<Uuid, _>("claim_token")?.to_string(),
                "claim_expires_at": row.try_get::<DateTime<Utc>, _>("claim_expires_at")?,
            }),
        });
    }

    let rows = sqlx::query(
        "SELECT delivery_id, repository_id, attempts, claim_token, claim_expires_at
           FROM ci_github_webhook_deliveries
          WHERE processed_at IS NULL AND claim_token IS NOT NULL
            AND claim_expires_at <= now()
          ORDER BY claim_expires_at, delivery_id LIMIT $1",
    )
    .bind(query_limit)
    .fetch_all(pool)
    .await?;
    truncated |= rows.len() as i64 > limit;
    for row in rows.into_iter().take(limit as usize) {
        let id: Uuid = row.try_get("delivery_id")?;
        candidates.push(Candidate {
            category: "ci_webhook_claim",
            id: id.to_string(),
            details: json!({
                "repository_id": row.try_get::<Uuid, _>("repository_id")?.to_string(),
                "attempts": row.try_get::<i32, _>("attempts")?,
                "claim_token": row.try_get::<Uuid, _>("claim_token")?.to_string(),
                "claim_expires_at": row.try_get::<DateTime<Utc>, _>("claim_expires_at")?,
            }),
        });
    }

    let rows = sqlx::query(
        "SELECT id, repository, poll_lease_token, poll_lease_expires_at
           FROM ci_watched_repositories
          WHERE poll_lease_token IS NOT NULL AND poll_lease_expires_at <= now()
          ORDER BY poll_lease_expires_at, id LIMIT $1",
    )
    .bind(query_limit)
    .fetch_all(pool)
    .await?;
    truncated |= rows.len() as i64 > limit;
    for row in rows.into_iter().take(limit as usize) {
        let id: Uuid = row.try_get("id")?;
        candidates.push(Candidate {
            category: "ci_poll_lease",
            id: id.to_string(),
            details: json!({
                "repository": row.try_get::<String, _>("repository")?,
                "poll_lease_token": row.try_get::<Uuid, _>("poll_lease_token")?.to_string(),
                "poll_lease_expires_at": row.try_get::<DateTime<Utc>, _>("poll_lease_expires_at")?,
            }),
        });
    }

    let rows = sqlx::query(
        "SELECT runtime.preview_id, runtime.worker_id, runtime.generation,
                runtime.lease_token, runtime.stale_cleanup_claim_token,
                runtime.stale_cleanup_claimed_at
           FROM ci_runtime_placements runtime
           JOIN ci_preview_aliases alias ON alias.id = runtime.preview_id
           JOIN workers worker ON worker.id = runtime.worker_id
          WHERE runtime.state = 'deleting' AND alias.state = 'expired'
            AND worker.heartbeat_at <= now() - interval '30 seconds'
            AND runtime.stale_cleanup_claim_token IS NOT NULL
            AND runtime.stale_cleanup_claimed_at <= now() - interval '10 minutes'
          ORDER BY runtime.stale_cleanup_claimed_at, runtime.preview_id LIMIT $1",
    )
    .bind(query_limit)
    .fetch_all(pool)
    .await?;
    truncated |= rows.len() as i64 > limit;
    for row in rows.into_iter().take(limit as usize) {
        let id: Uuid = row.try_get("preview_id")?;
        candidates.push(Candidate {
            category: "ci_runtime_cleanup_claim",
            id: id.to_string(),
            details: json!({
                "worker_id": row.try_get::<Uuid, _>("worker_id")?.to_string(),
                "generation": row.try_get::<i64, _>("generation")?,
                "lease_token": row.try_get::<Uuid, _>("lease_token")?.to_string(),
                "stale_cleanup_claim_token": row.try_get::<Uuid, _>("stale_cleanup_claim_token")?.to_string(),
                "stale_cleanup_claimed_at": row.try_get::<DateTime<Utc>, _>("stale_cleanup_claimed_at")?,
            }),
        });
    }

    let rows = sqlx::query(
        "SELECT builder_identity, slot, lease_token, owner_pid,
                requested_memory_bytes, heartbeat_at
           FROM ci_build_admission_leases
          WHERE heartbeat_at <= now() - interval '45 seconds'
          ORDER BY heartbeat_at, builder_identity, slot LIMIT $1",
    )
    .bind(query_limit)
    .fetch_all(pool)
    .await?;
    truncated |= rows.len() as i64 > limit;
    for row in rows.into_iter().take(limit as usize) {
        let token: Uuid = row.try_get("lease_token")?;
        candidates.push(Candidate {
            category: "ci_build_admission_lease",
            id: token.to_string(),
            details: json!({
                "builder_identity": row.try_get::<String, _>("builder_identity")?,
                "slot": row.try_get::<i16, _>("slot")?,
                "owner_pid": row.try_get::<i32, _>("owner_pid")?,
                "requested_memory_bytes": row.try_get::<i64, _>("requested_memory_bytes")?,
                "heartbeat_at": row.try_get::<DateTime<Utc>, _>("heartbeat_at")?,
                "requires_external_resource_cleanup": true,
            }),
        });
    }

    Ok(CandidateReport {
        generated_at: Utc::now(),
        limit,
        database_backup_reference: None,
        candidates,
        truncated,
    })
}

fn write_backup(path: &PathBuf, report: &CandidateReport) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(report)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create cleanup backup {}", path.display()))?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

async fn apply_cleanup(
    pool: &PgPool,
    report: &CandidateReport,
    admission_lease_token: Option<Uuid>,
    builder_identity: Option<&str>,
) -> Result<ReclaimedCounts> {
    let mut tx = pool.begin().await?;
    let mut reclaimed = ReclaimedCounts::default();
    let ids = |category: &str| {
        report
            .candidates
            .iter()
            .filter(|candidate| candidate.category == category)
            .filter_map(|candidate| candidate.id.parse::<Uuid>().ok())
            .collect::<Vec<_>>()
    };

    let build_ids = ids("ci_build_lease");
    if !build_ids.is_empty() {
        reclaimed.build_leases = sqlx::query(
            "UPDATE ci_builds
                SET lease_token = NULL, lease_expires_at = NULL, updated_at = now()
              WHERE id = ANY($1::uuid[])
                AND lease_expires_at <= now()",
        )
        .bind(&build_ids)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    let check_run_ids = ids("ci_check_run_claim");
    if !check_run_ids.is_empty() {
        reclaimed.check_run_claims = sqlx::query(
            "UPDATE ci_check_runs
                SET claim_token = NULL, claim_expires_at = NULL, updated_at = now()
              WHERE id = ANY($1::uuid[])
                AND external_id IS NULL AND claim_expires_at <= now()",
        )
        .bind(&check_run_ids)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    let webhook_ids = ids("ci_webhook_claim");
    if !webhook_ids.is_empty() {
        reclaimed.webhook_claims = sqlx::query(
            "UPDATE ci_github_webhook_deliveries
                SET claim_token = NULL, claim_expires_at = NULL,
                    next_attempt_at = LEAST(next_attempt_at, now())
              WHERE delivery_id = ANY($1::uuid[])
                AND processed_at IS NULL AND claim_expires_at <= now()",
        )
        .bind(&webhook_ids)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    let repository_ids = ids("ci_poll_lease");
    if !repository_ids.is_empty() {
        reclaimed.poll_leases = sqlx::query(
            "UPDATE ci_watched_repositories
                SET poll_lease_token = NULL, poll_lease_expires_at = NULL, updated_at = now()
              WHERE id = ANY($1::uuid[])
                AND poll_lease_expires_at <= now()",
        )
        .bind(&repository_ids)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    let preview_ids = ids("ci_runtime_cleanup_claim");
    if !preview_ids.is_empty() {
        reclaimed.runtime_cleanup_claims = sqlx::query(
            "UPDATE ci_runtime_placements runtime
                SET stale_cleanup_claimed_at = NULL,
                    stale_cleanup_claim_token = NULL, updated_at = now()
              FROM ci_preview_aliases alias, workers worker
              WHERE runtime.preview_id = ANY($1::uuid[])
                AND alias.id = runtime.preview_id AND worker.id = runtime.worker_id
                AND runtime.state = 'deleting' AND alias.state = 'expired'
                AND worker.heartbeat_at <= now() - interval '30 seconds'
                AND runtime.stale_cleanup_claimed_at <= now() - interval '10 minutes'",
        )
        .bind(&preview_ids)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    if let Some(token) = admission_lease_token {
        let identity = builder_identity.context("builder identity missing")?;
        reclaimed.admission_leases = sqlx::query(
            "DELETE FROM ci_build_admission_leases
              WHERE builder_identity = $1 AND lease_token = $2
                AND heartbeat_at <= now() - interval '45 seconds'",
        )
        .bind(identity)
        .bind(token)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    tx.commit().await?;
    Ok(reclaimed)
}

#[tokio::main]
async fn main() -> Result<()> {
    kurbis_hoster::init_tracing("kurbis_hoster=warn");
    let arguments = Arguments::parse();
    let pool = kurbis_hoster::db::connect(&arguments.database_url).await?;
    let mut report = collect_report(&pool, arguments.limit).await?;
    report.database_backup_reference = arguments.database_backup_reference.clone();
    validate_arguments(&arguments, &report)?;

    if arguments.apply {
        let backup = arguments
            .backup_file
            .as_ref()
            .expect("validated --backup-file");
        write_backup(backup, &report)?;
        let reclaimed = apply_cleanup(
            &pool,
            &report,
            arguments.admission_lease_token,
            arguments.builder_identity.as_deref(),
        )
        .await?;
        println!(
            "{}",
            serde_json::to_string_pretty(&ApplyResult {
                dry_run: false,
                report,
                reclaimed,
            })?
        );
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&ApplyResult {
                dry_run: true,
                report,
                reclaimed: ReclaimedCounts::default(),
            })?
        );
    }
    pool.close().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments() -> Arguments {
        Arguments {
            database_url: "postgres://example".into(),
            apply: false,
            confirm: None,
            backup_file: None,
            admission_lease_token: None,
            builder_identity: None,
            admission_resources_cleaned: false,
            limit: DEFAULT_LIMIT,
            database_backup_reference: None,
        }
    }

    fn report() -> CandidateReport {
        CandidateReport {
            generated_at: Utc::now(),
            limit: DEFAULT_LIMIT,
            database_backup_reference: None,
            candidates: Vec::new(),
            truncated: false,
        }
    }

    #[test]
    fn dry_run_rejects_mutation_flags() {
        let mut args = arguments();
        args.confirm = Some(CONFIRMATION.into());
        assert!(validate_arguments(&args, &report()).is_err());
    }

    #[test]
    fn apply_requires_exact_confirmation_and_new_backup() {
        let mut args = arguments();
        args.apply = true;
        assert!(validate_arguments(&args, &report()).is_err());
        args.confirm = Some(CONFIRMATION.into());
        assert!(validate_arguments(&args, &report()).is_err());
        args.backup_file = Some("cleanup.json".into());
        args.database_backup_reference = Some("backup-2026-10-11".into());
        assert!(validate_arguments(&args, &report()).is_ok());
    }

    #[test]
    fn truncated_reports_cannot_be_applied() {
        let mut args = arguments();
        args.apply = true;
        args.confirm = Some(CONFIRMATION.into());
        args.backup_file = Some("cleanup.json".into());
        args.database_backup_reference = Some("backup-2026-10-11".into());
        let mut report = report();
        report.truncated = true;
        assert!(validate_arguments(&args, &report).is_err());
    }

    #[test]
    fn admission_deletion_requires_external_cleanup_proof() {
        let mut args = arguments();
        args.apply = true;
        args.confirm = Some(CONFIRMATION.into());
        args.backup_file = Some("cleanup.json".into());
        args.database_backup_reference = Some("backup-2026-10-11".into());
        args.admission_lease_token = Some(Uuid::new_v4());
        assert!(validate_arguments(&args, &report()).is_err());
        args.builder_identity = Some("docker:test".into());
        assert!(validate_arguments(&args, &report()).is_err());
        args.admission_resources_cleaned = true;
        assert!(validate_arguments(&args, &report()).is_ok());
    }
}

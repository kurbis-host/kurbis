use sqlx::{PgPool, Postgres, Transaction};
use thiserror::Error;
use uuid::Uuid;

use crate::pumpkin_control::VerifiedMinecraftIdentity;

const IDENTITY_WRITE_LOCK: i64 = 0x4b55_5242_4953;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinecraftIdentityKey {
    pub provider: &'static str,
    pub subject: String,
}

pub fn minecraft_identity_key(
    identity: &VerifiedMinecraftIdentity,
    minecraft_player_id: Uuid,
) -> Result<MinecraftIdentityKey, &'static str> {
    if minecraft_player_id.is_nil() {
        return Err("verified Minecraft profile is invalid");
    }
    let (provider, subject) = match identity {
        VerifiedMinecraftIdentity::Java { uuid } if *uuid == minecraft_player_id => {
            ("minecraft_java", uuid.to_string())
        }
        VerifiedMinecraftIdentity::Java { .. } => {
            return Err("verified Java profile does not match its identity");
        }
        VerifiedMinecraftIdentity::Bedrock { xuid } if valid_xuid(xuid) => {
            ("minecraft_bedrock", xuid.clone())
        }
        VerifiedMinecraftIdentity::Bedrock { .. } => {
            return Err("verified Minecraft identity is invalid");
        }
    };
    Ok(MinecraftIdentityKey { provider, subject })
}

/// Whether an in-game name may be stored as an identity display name.
pub fn valid_minecraft_display_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty() && name.len() <= 64 && !name.chars().any(char::is_control)
}

pub async fn resolve_minecraft_owner(
    tx: &mut Transaction<'_, Postgres>,
    key: &MinecraftIdentityKey,
    minecraft_player_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT user_id FROM identities
         WHERE provider = $1 AND subject = $2 AND minecraft_player_id = $3",
    )
    .bind(key.provider)
    .bind(&key.subject)
    .bind(minecraft_player_id)
    .fetch_optional(&mut **tx)
    .await
}

fn valid_xuid(xuid: &str) -> bool {
    !xuid.is_empty()
        && xuid.len() <= 20
        && xuid.bytes().all(|byte| byte.is_ascii_digit())
        && xuid.bytes().any(|byte| byte != b'0')
}

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("identity is already linked to another account")]
    AlreadyLinked,
    #[error("account no longer exists")]
    AccountMissing,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

pub async fn link_oauth_identity(
    pool: &PgPool,
    provider: &str,
    subject: &str,
    email: Option<&str>,
    email_verified: bool,
    display_name: &str,
    link_to: Option<Uuid>,
) -> Result<Uuid, IdentityError> {
    let mut tx = pool.begin().await?;
    lock_identity_writes(&mut tx).await?;

    // An unverified provider address is neither an account email nor identity
    // metadata: persist it only after the provider has verified ownership.
    let email = email
        .filter(|_| email_verified)
        .map(normalize_email)
        .filter(|email| !email.is_empty());
    let normalized_email = email.clone();
    if let Some(email) = normalized_email.as_deref() {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(email)
            .execute(&mut *tx)
            .await?;
    }

    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT user_id FROM identities WHERE provider = $1 AND subject = $2 FOR UPDATE",
    )
    .bind(provider)
    .bind(subject)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((owner_id,)) = existing {
        if link_to.is_some_and(|account_id| account_id != owner_id) {
            return Err(IdentityError::AlreadyLinked);
        }
        update_oauth_identity(
            &mut tx,
            provider,
            subject,
            email.as_deref(),
            normalized_email.as_deref(),
            email_verified && normalized_email.is_some(),
            display_name,
        )
        .await?;
        if link_to.is_none()
            && email_verified
            && let Some(email) = normalized_email.as_deref()
        {
            merge_verified_email_accounts(&mut tx, owner_id, email).await?;
        }
        tx.commit().await?;
        return Ok(owner_id);
    }

    let matching_accounts = if link_to.is_none() && email_verified {
        if let Some(email) = normalized_email.as_deref() {
            verified_email_accounts(&mut tx, email).await?
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    let user_id = if let Some(account_id) = link_to {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
            .bind(account_id)
            .fetch_one(&mut *tx)
            .await?;
        if !exists {
            return Err(IdentityError::AccountMissing);
        }
        account_id
    } else if let Some(account_id) = matching_accounts.first() {
        *account_id
    } else {
        Uuid::new_v4()
    };

    let user_exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
    if !user_exists {
        sqlx::query("INSERT INTO users(id, display_name) VALUES($1, $2)")
            .bind(user_id)
            .bind(display_name)
            .execute(&mut *tx)
            .await?;
    }

    if link_to.is_none() && email_verified {
        for source_id in matching_accounts
            .iter()
            .copied()
            .filter(|id| *id != user_id)
        {
            merge_accounts(&mut tx, user_id, source_id).await?;
        }
    }

    sqlx::query(
        "INSERT INTO identities(id, user_id, provider, subject, display_name, email,
                                email_normalized, email_verified)
         VALUES($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(provider)
    .bind(subject)
    .bind(display_name)
    .bind(email)
    .bind(normalized_email.as_deref())
    .bind(email_verified && normalized_email.is_some())
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(user_id)
}

async fn verified_email_accounts(
    tx: &mut Transaction<'_, Postgres>,
    email: &str,
) -> Result<Vec<Uuid>, IdentityError> {
    Ok(sqlx::query_scalar(
        "SELECT user_id FROM identities
         WHERE email_normalized = $1 AND email_verified
         GROUP BY user_id
         ORDER BY min(created_at), user_id",
    )
    .bind(email)
    .fetch_all(&mut **tx)
    .await?)
}

async fn merge_verified_email_accounts(
    tx: &mut Transaction<'_, Postgres>,
    survivor_id: Uuid,
    email: &str,
) -> Result<(), IdentityError> {
    for source_id in verified_email_accounts(tx, email)
        .await?
        .into_iter()
        .filter(|id| *id != survivor_id)
    {
        merge_accounts(tx, survivor_id, source_id).await?;
    }
    Ok(())
}

pub async fn attach_minecraft_identity(
    pool: &PgPool,
    provider: &str,
    subject: &str,
    minecraft_player_id: Uuid,
    display_name: &str,
    browser_account: Option<Uuid>,
) -> Result<Uuid, IdentityError> {
    let mut tx = pool.begin().await?;
    lock_identity_writes(&mut tx).await?;

    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT user_id FROM identities WHERE provider = $1 AND subject = $2 FOR UPDATE",
    )
    .bind(provider)
    .bind(subject)
    .fetch_optional(&mut *tx)
    .await?;

    let account_id = match (browser_account, existing) {
        (Some(browser_id), Some((source_id,))) if browser_id != source_id => {
            merge_accounts(&mut tx, browser_id, source_id).await?;
            browser_id
        }
        (Some(browser_id), _) => {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
                    .bind(browser_id)
                    .fetch_one(&mut *tx)
                    .await?;
            if !exists {
                return Err(IdentityError::AccountMissing);
            }
            browser_id
        }
        (None, Some((owner_id,))) => owner_id,
        (None, None) => {
            let account_id = Uuid::new_v4();
            sqlx::query("INSERT INTO users(id, display_name) VALUES($1, $2)")
                .bind(account_id)
                .bind(display_name)
                .execute(&mut *tx)
                .await?;
            account_id
        }
    };

    if existing.is_none() {
        sqlx::query(
            "INSERT INTO identities(id, user_id, provider, subject, minecraft_player_id, display_name)
             VALUES($1, $2, $3, $4, $5, $6)",
        )
        .bind(Uuid::new_v4())
        .bind(account_id)
        .bind(provider)
        .bind(subject)
        .bind(minecraft_player_id)
        .bind(display_name)
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query(
            "UPDATE identities SET display_name = $3, minecraft_player_id = $4, updated_at = now()
             WHERE provider = $1 AND subject = $2 AND user_id = $5",
        )
        .bind(provider)
        .bind(subject)
        .bind(display_name)
        .bind(minecraft_player_id)
        .bind(account_id)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(account_id)
}

async fn merge_accounts(
    tx: &mut Transaction<'_, Postgres>,
    browser_id: Uuid,
    source_id: Uuid,
) -> Result<(), IdentityError> {
    let account_ids = [browser_id, source_id];
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, account_status FROM users WHERE id = ANY($1) ORDER BY id FOR UPDATE",
    )
    .bind(account_ids.as_slice())
    .fetch_all(&mut **tx)
    .await?;
    if rows.len() != 2 {
        return Err(IdentityError::AccountMissing);
    }
    let suspended = rows.iter().any(|(_, status)| status == "suspended");

    let browser_default: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM servers WHERE owner_id = $1 AND is_default LIMIT 1")
            .bind(browser_id)
            .fetch_optional(&mut **tx)
            .await?;
    let selected_default =
        match browser_default {
            Some(id) => Some(id),
            None => sqlx::query_scalar(
                "SELECT id FROM servers WHERE owner_id = ANY($1) ORDER BY created_at, id LIMIT 1",
            )
            .bind(account_ids.as_slice())
            .fetch_optional(&mut **tx)
            .await?,
        };

    sqlx::query("UPDATE servers SET is_default = false WHERE owner_id = ANY($1) AND is_default")
        .bind(account_ids.as_slice())
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE servers SET owner_id = $1 WHERE owner_id = $2")
        .bind(browser_id)
        .bind(source_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE servers SET is_default = false WHERE owner_id = $1 AND is_default")
        .bind(browser_id)
        .execute(&mut **tx)
        .await?;
    if let Some(server_id) = selected_default {
        sqlx::query("UPDATE servers SET is_default = true WHERE id = $1")
            .bind(server_id)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query("UPDATE identities SET user_id = $1, updated_at = now() WHERE user_id = $2")
        .bind(browser_id)
        .bind(source_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE sessions SET user_id = $1 WHERE user_id = $2")
        .bind(browser_id)
        .bind(source_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "UPDATE users target
         SET account_status = CASE WHEN $3 THEN 'suspended' ELSE target.account_status END,
             updated_at = now()
         FROM users source WHERE target.id = $1 AND source.id = $2",
    )
    .bind(browser_id)
    .bind(source_id)
    .bind(suspended)
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(source_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn update_oauth_identity(
    tx: &mut Transaction<'_, Postgres>,
    provider: &str,
    subject: &str,
    email: Option<&str>,
    normalized_email: Option<&str>,
    email_verified: bool,
    display_name: &str,
) -> Result<(), IdentityError> {
    sqlx::query(
        "UPDATE identities SET display_name = $3, email = $4, email_normalized = $5,
                                email_verified = $6, updated_at = now()
         WHERE provider = $1 AND subject = $2",
    )
    .bind(provider)
    .bind(subject)
    .bind(display_name)
    .bind(email)
    .bind(normalized_email)
    .bind(email_verified)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn lock_identity_writes(tx: &mut Transaction<'_, Postgres>) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(IDENTITY_WRITE_LOCK)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::valid_minecraft_display_name;

    #[test]
    fn minecraft_display_names_are_bounded_and_safe() {
        assert!(valid_minecraft_display_name("Steve"));
        assert!(valid_minecraft_display_name(&"a".repeat(64)));
        assert!(!valid_minecraft_display_name(""));
        assert!(!valid_minecraft_display_name(" \t "));
        assert!(!valid_minecraft_display_name(&"a".repeat(65)));
        assert!(!valid_minecraft_display_name("player\nname"));
    }
}

//! Lobby admission: the only way a server name is claimed.
//!
//! A verified Minecraft player who joins a free address registers (or resumes)
//! their account and creates its server under that name. A browser sign-in
//! never claims a name; the owner links their Google or Discord account to the
//! player through the one-use account link admission hands the lobby.

use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use uuid::Uuid;

use crate::{
    control_plane::{
        auth::create_minecraft_login_link,
        identity::{self, MinecraftIdentityKey},
    },
    model::{ObservedState, RouteTarget},
    pumpkin_control::VerifiedMinecraftIdentity,
    routing::{AdmissionRequest, AdmissionResponse, RouteRepository},
};

use super::internal::require_internal;
use super::types::ServerSettings;
use super::{ApiError, AppState};

/// A joining player whose online Minecraft identity the lobby verified.
struct VerifiedPlayer {
    key: MinecraftIdentityKey,
    profile_id: Uuid,
    display_name: String,
}

/// Where admission sends the player, before any owner link is minted.
struct Admitted {
    target: RouteTarget,
    private_message: Option<String>,
    created: bool,
    owner: bool,
}

impl Admitted {
    fn guest(target: RouteTarget) -> Self {
        Self {
            target,
            private_message: None,
            created: false,
            owner: false,
        }
    }
}

pub(super) async fn admit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<AdmissionRequest>,
) -> Result<Json<AdmissionResponse>, ApiError> {
    require_internal(&state, &headers)?;
    let routes = RouteRepository::new(state.pool.clone(), state.config.root_domain.clone());
    let subdomain = routes
        .subdomain_for_hostname(&request.hostname)
        .ok_or_else(|| {
            ApiError::with_code(
                StatusCode::NOT_FOUND,
                "invalid_hostname",
                "unknown server address",
            )
        })?;
    let player = match request.identity.as_ref() {
        Some(identity) => Some(register_player(&state, identity, &request).await?),
        None => None,
    };

    let admitted = match routes.resolve_subdomain(&subdomain).await? {
        Some(target) => {
            let owner = match player.as_ref() {
                Some(player) => owns_server(&state, player, target.server_id).await?,
                None => false,
            };
            Admitted {
                owner,
                ..Admitted::guest(target)
            }
        }
        None => {
            let player = player.as_ref().ok_or_else(|| {
                ApiError::with_code(
                    StatusCode::NOT_FOUND,
                    "unknown_hostname",
                    "unknown server address",
                )
            })?;
            claim_address(&state, &routes, &subdomain, player).await?
        }
    };

    let (account_link, private_link) = match player.as_ref().filter(|_| admitted.owner) {
        Some(player) => owner_links(&state, player, &admitted.target).await?,
        None => (None, None),
    };
    Ok(Json(AdmissionResponse {
        target: admitted.target,
        private_message: admitted.private_message,
        account_link,
        private_link,
        created: admitted.created,
        owner: admitted.owner,
    }))
}

/// Online authenticated Minecraft identity is the registration credential:
/// a first join creates an account and later joins resume it.
async fn register_player(
    state: &AppState,
    identity: &VerifiedMinecraftIdentity,
    request: &AdmissionRequest,
) -> Result<VerifiedPlayer, ApiError> {
    let invalid = |message: &'static str| {
        ApiError::with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_identity",
            message,
        )
    };
    let profile_id = request
        .minecraft_player_id
        .ok_or_else(|| invalid("verified Minecraft profile is missing"))?;
    let key = identity::minecraft_identity_key(identity, profile_id).map_err(invalid)?;
    let display_name = request
        .display_name
        .as_deref()
        .filter(|name| identity::valid_minecraft_display_name(name))
        .ok_or_else(|| invalid("verified Minecraft profile name is invalid"))?
        .trim()
        .to_owned();
    identity::attach_minecraft_identity(
        &state.pool,
        key.provider,
        &key.subject,
        profile_id,
        &display_name,
        None,
    )
    .await
    .map_err(|error| match error {
        identity::IdentityError::Database(error) => error.into(),
        identity::IdentityError::AccountMissing => identity_changed(),
    })?;
    Ok(VerifiedPlayer {
        key,
        profile_id,
        display_name,
    })
}

async fn owns_server(
    state: &AppState,
    player: &VerifiedPlayer,
    server_id: Uuid,
) -> Result<bool, ApiError> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM identities i
         JOIN servers s ON s.owner_id = i.user_id
         WHERE i.provider = $1 AND i.subject = $2 AND i.minecraft_player_id = $3
           AND s.id = $4)",
    )
    .bind(player.key.provider)
    .bind(&player.key.subject)
    .bind(player.profile_id)
    .bind(server_id)
    .fetch_one(&state.pool)
    .await?)
}

/// Create the player's server under the free address they joined, or send
/// them to the server their account already has.
async fn claim_address(
    state: &AppState,
    routes: &RouteRepository,
    subdomain: &str,
    player: &VerifiedPlayer,
) -> Result<Admitted, ApiError> {
    let mut tx = state.pool.begin().await?;
    let Some(owner_id) =
        identity::resolve_minecraft_owner(&mut tx, &player.key, player.profile_id).await?
    else {
        tx.rollback().await?;
        return Err(identity_changed());
    };
    let account_status: Option<String> =
        sqlx::query_scalar("SELECT account_status FROM users WHERE id = $1 FOR UPDATE")
            .bind(owner_id)
            .fetch_optional(&mut *tx)
            .await?;
    if account_status.as_deref() != Some("active") {
        tx.rollback().await?;
        return Err(ApiError::with_code(
            StatusCode::FORBIDDEN,
            "account_suspended",
            "account is suspended",
        ));
    }
    let identity_still_linked: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM identities WHERE provider = $1 AND subject = $2 AND minecraft_player_id = $3 AND user_id = $4)",
    )
    .bind(player.key.provider)
    .bind(&player.key.subject)
    .bind(player.profile_id)
    .bind(owner_id)
    .fetch_one(&mut *tx)
    .await?;
    if !identity_still_linked {
        tx.rollback().await?;
        return Err(identity_changed());
    }

    let existing_address: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM servers WHERE subdomain = $1 AND desired_state <> 'deleted')",
    )
    .bind(subdomain)
    .fetch_one(&mut *tx)
    .await?;
    if existing_address {
        tx.commit().await?;
        let target = resolve(routes, subdomain, "server route is temporarily unavailable").await?;
        return Ok(Admitted::guest(target));
    }

    let default_subdomain: Option<String> = sqlx::query_scalar(
        "SELECT subdomain FROM servers WHERE owner_id = $1 AND desired_state <> 'deleted'
         ORDER BY is_default DESC, created_at, id LIMIT 1",
    )
    .bind(owner_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(default_subdomain) = default_subdomain {
        tx.commit().await?;
        let target = resolve(
            routes,
            &default_subdomain,
            "default server route is temporarily unavailable",
        )
        .await?;
        let address = format!("{default_subdomain}.{}", state.config.root_domain);
        return Ok(Admitted {
            target,
            private_message: Some(format!(
                "You already have a server at {address}; connecting you there."
            )),
            created: false,
            owner: true,
        });
    }
    let account_has_server: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM servers WHERE owner_id = $1)")
            .bind(owner_id)
            .fetch_one(&mut *tx)
            .await?;
    if account_has_server {
        return Err(ApiError::with_code(
            StatusCode::CONFLICT,
            "server_deletion_in_progress",
            "your previous server is still being removed",
        ));
    }

    let insert = super::server::insert_server_record(
        &mut tx,
        owner_id,
        subdomain,
        ServerSettings::default().into_stored(),
    )
    .await;
    if let Err(error) = insert {
        tx.rollback().await?;
        // Another player claimed the address first: they own it now.
        if let Some(target) = routes.resolve_subdomain(subdomain).await? {
            return Ok(Admitted::guest(target));
        }
        return Err(super::mutation_error(
            error,
            "server address or account already exists",
        ));
    }
    tx.commit().await?;

    let target = resolve(routes, subdomain, "server route is unavailable").await?;
    Ok(Admitted {
        target,
        private_message: None,
        created: true,
        owner: true,
    })
}

/// The one-use links an owner gets in the lobby. An owner who has not yet
/// linked Google or Discord gets the account link, which also opens their
/// panel. Everyone else can sign in on the web already, so only an owner of a
/// server that cannot start (and so cannot run `/kurbis login`) gets a link:
/// the panel link for that server.
async fn owner_links(
    state: &AppState,
    player: &VerifiedPlayer,
    target: &RouteTarget,
) -> Result<(Option<String>, Option<String>), ApiError> {
    let has_web_sign_in: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM identities web
         JOIN identities player ON player.user_id = web.user_id
         WHERE player.provider = $1 AND player.subject = $2
           AND web.provider NOT IN ('minecraft_java', 'minecraft_bedrock'))",
    )
    .bind(player.key.provider)
    .bind(&player.key.subject)
    .fetch_one(&state.pool)
    .await?;
    if !has_web_sign_in {
        let link = create_minecraft_login_link(
            &state.pool,
            &state.config.public_base_url,
            &player.key,
            player.profile_id,
            &player.display_name,
        )
        .await?;
        return Ok((Some(link), None));
    }
    if target.state != ObservedState::Failed {
        return Ok((None, None));
    }
    let mut tx = state.pool.begin().await?;
    let link = super::server_access::create_server_login_link(
        &mut tx,
        &state.config.public_base_url,
        target.server_id,
    )
    .await?;
    tx.commit().await?;
    Ok((None, link))
}

async fn resolve(
    routes: &RouteRepository,
    subdomain: &str,
    unavailable: &'static str,
) -> Result<RouteTarget, ApiError> {
    routes.resolve_subdomain(subdomain).await?.ok_or_else(|| {
        ApiError::with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            "route_unavailable",
            unavailable,
        )
    })
}

fn identity_changed() -> ApiError {
    ApiError::with_code(
        StatusCode::CONFLICT,
        "identity_changed",
        "Minecraft account linking changed during admission",
    )
}

//! Resolve player names supplied by panel mutations into the UUID Pumpkin stores.
//!
//! Java profile names are globally resolvable through Mojang's public profile
//! endpoint. Bedrock does not expose an equivalent public name-to-XUID lookup;
//! the only safe server-side resolution uses a player profile already known to
//! this server or a linked, server-visible Minecraft identity.

use std::sync::Arc;

use axum::http::StatusCode;
use serde::Deserialize;
use uuid::Uuid;

use super::{ApiError, AppState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedPlayer {
    pub(super) player_id: Uuid,
    pub(super) name: String,
}

#[derive(Debug, sqlx::FromRow)]
struct OnlinePlayerRow {
    player_id: Uuid,
    name: String,
    edition: String,
}

#[derive(Debug, Deserialize)]
struct MojangProfile {
    id: String,
    name: String,
}

fn ambiguous_player_name() -> ApiError {
    ApiError::with_code(
        StatusCode::UNPROCESSABLE_ENTITY,
        "ambiguous_player_name",
        "more than one player on this server matches that name; use the unique in-game name before changing access",
    )
}

pub(super) fn validate_player_name(name: &str) -> Result<&str, ApiError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(ApiError::with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_player_name",
            "player name must contain 1 to 64 printable characters",
        ));
    }
    Ok(name)
}

/// Resolve a mutation target by name, preferring the authoritative live
/// player table so Bedrock players can be managed while online.
pub(super) async fn resolve_player_name(
    state: &Arc<AppState>,
    server_id: Uuid,
    requested_name: &str,
) -> Result<ResolvedPlayer, ApiError> {
    let name = validate_player_name(requested_name)?;
    let known_players = sqlx::query_as::<_, OnlinePlayerRow>(
        "SELECT candidates.player_id, min(candidates.name) AS name,
                CASE WHEN bool_or(candidates.edition = 'bedrock') THEN 'bedrock' ELSE 'java' END AS edition
         FROM (
             SELECT player_id, name, edition FROM server_players
             WHERE server_id = $1 AND lower(name) = lower($2)
             UNION ALL
             SELECT player_id, name, edition FROM server_known_players
             WHERE server_id = $1 AND lower(name) = lower($2)
             UNION ALL
             SELECT i.minecraft_player_id AS player_id, i.display_name AS name,
                    CASE i.provider WHEN 'minecraft_bedrock' THEN 'bedrock' ELSE 'java' END AS edition
             FROM identities i JOIN servers s ON s.id = $1 AND s.owner_id = i.user_id
             WHERE i.provider IN ('minecraft_java', 'minecraft_bedrock')
               AND i.minecraft_player_id IS NOT NULL AND lower(i.display_name) = lower($2)
         ) candidates
         GROUP BY candidates.player_id
         ORDER BY candidates.player_id
         LIMIT 2",
    )
    .bind(server_id)
    .bind(name)
    .fetch_all(&state.pool)
    .await?;
    if known_players.len() > 1 {
        return Err(ambiguous_player_name());
    }
    if let Some(player) = known_players.into_iter().next() {
        if !matches!(player.edition.as_str(), "java" | "bedrock") {
            return Err(ApiError::with_code(
                StatusCode::INTERNAL_SERVER_ERROR,
                "invalid_player_edition",
                "database contains an unknown player edition",
            ));
        }
        return Ok(ResolvedPlayer {
            player_id: player.player_id,
            name: player.name,
        });
    }

    if !is_java_profile_name(name) {
        return Err(ApiError::with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bedrock_player_lookup_unavailable",
            "Bedrock player names cannot be resolved globally; the player must be online on this server, previously seen here, or linked as this server's owner",
        ));
    }
    resolve_java_profile(&state.http, MOJANG_PROFILE_BASE_URL, name).await
}

pub(super) const MOJANG_PROFILE_BASE_URL: &str = "https://api.mojang.com/users/profiles/minecraft";
const MAX_MOJANG_PROFILE_BYTES: usize = 4 * 1024;

pub(super) async fn resolve_java_profile(
    http: &reqwest::Client,
    profile_base_url: &str,
    name: &str,
) -> Result<ResolvedPlayer, ApiError> {
    let profile_base_url = profile_base_url.trim_end_matches('/');
    let url = format!("{profile_base_url}/{name}");
    let response = http.get(&url).send().await.map_err(|error| {
        tracing::warn!(?error, player_name = %name, "Mojang profile lookup failed");
        ApiError::with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            "minecraft_profile_lookup_unavailable",
            "Minecraft profile lookup is temporarily unavailable; try again later",
        )
    })?;
    if response.url().as_str() != url {
        tracing::warn!(player_name = %name, "Mojang profile lookup followed an unexpected redirect");
        return Err(invalid_mojang_response());
    }
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(ApiError::with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            "minecraft_player_not_found",
            "no Java Minecraft profile exists for that player name",
        ));
    }
    if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        tracing::warn!(player_name = %name, "Mojang profile lookup was rate limited");
        return Err(ApiError::with_code(
            StatusCode::TOO_MANY_REQUESTS,
            "minecraft_profile_lookup_rate_limited",
            "Minecraft profile lookup is temporarily rate limited; try again later",
        ));
    }
    if !response.status().is_success() {
        tracing::warn!(status = %response.status(), player_name = %name, "Mojang profile lookup returned an error");
        return Err(ApiError::with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            "minecraft_profile_lookup_unavailable",
            "Minecraft profile lookup is temporarily unavailable; try again later",
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_MOJANG_PROFILE_BYTES as u64)
    {
        return Err(invalid_mojang_response());
    }
    let mut response = response;
    let mut body = Vec::with_capacity(
        MAX_MOJANG_PROFILE_BYTES.min(response.content_length().unwrap_or(0) as usize),
    );
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        tracing::warn!(?error, player_name = %name, "Mojang profile response could not be read");
        invalid_mojang_response()
    })? {
        if body.len().saturating_add(chunk.len()) > MAX_MOJANG_PROFILE_BYTES {
            return Err(invalid_mojang_response());
        }
        body.extend_from_slice(&chunk);
    }
    let profile = serde_json::from_slice::<MojangProfile>(&body).map_err(|error| {
        tracing::warn!(?error, player_name = %name, "Mojang profile response was invalid");
        invalid_mojang_response()
    })?;
    let player_id = parse_mojang_uuid(&profile.id).filter(|id| !id.is_nil()).ok_or_else(|| {
        tracing::warn!(player_name = %name, profile_id = %profile.id, "Mojang profile UUID was invalid");
        invalid_mojang_response()
    })?;
    if !is_java_profile_name(&profile.name) || !profile.name.eq_ignore_ascii_case(name) {
        return Err(invalid_mojang_response());
    }
    Ok(ResolvedPlayer {
        player_id,
        name: profile.name,
    })
}

fn invalid_mojang_response() -> ApiError {
    ApiError::with_code(
        StatusCode::SERVICE_UNAVAILABLE,
        "minecraft_profile_lookup_unavailable",
        "Minecraft profile lookup returned an invalid response",
    )
}

pub(super) fn is_java_profile_name(name: &str) -> bool {
    let length = name.chars().count();
    (3..=16).contains(&length)
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn parse_mojang_uuid(value: &str) -> Option<Uuid> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let dashed = format!(
        "{}-{}-{}-{}-{}",
        &value[0..8],
        &value[8..12],
        &value[12..16],
        &value[16..20],
        &value[20..32]
    );
    dashed.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::http::StatusCode;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::sleep,
    };

    use super::{
        MOJANG_PROFILE_BASE_URL, is_java_profile_name, parse_mojang_uuid, resolve_java_profile,
    };

    async fn mock_mojang(status: u16, body: String, delay: Duration) -> String {
        mock_mojang_with_location(status, body, delay, None).await
    }

    async fn mock_mojang_with_location(
        status: u16,
        body: String,
        delay: Duration,
        location: Option<String>,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            let _ = stream.read(&mut request).await;
            sleep(delay).await;
            let reason = match status {
                200 => "OK",
                404 => "Not Found",
                429 => "Too Many Requests",
                _ => "Service Unavailable",
            };
            let location_header = location
                .as_deref()
                .map(|location| format!("Location: {location}\r\n"))
                .unwrap_or_default();
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n{location_header}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
        format!("http://{address}/users/profiles/minecraft")
    }

    #[tokio::test]
    async fn resolves_java_names_using_the_canonical_profile_and_uuid() {
        let id = "8667ba71b85a4004af54457a9734eed7";
        let base = mock_mojang(
            200,
            format!(r#"{{"id":"{id}","name":"Notch"}}"#),
            Duration::ZERO,
        )
        .await;
        let resolved = resolve_java_profile(&reqwest::Client::new(), &base, "notch")
            .await
            .unwrap();
        assert_eq!(
            resolved.player_id.to_string(),
            "8667ba71-b85a-4004-af54-457a9734eed7"
        );
        assert_eq!(resolved.name, "Notch");
        assert_eq!(
            MOJANG_PROFILE_BASE_URL,
            "https://api.mojang.com/users/profiles/minecraft"
        );
    }

    #[tokio::test]
    async fn missing_java_profile_is_a_user_input_error() {
        let base = mock_mojang(404, "{}".to_owned(), Duration::ZERO).await;
        let error = resolve_java_profile(&reqwest::Client::new(), &base, "Notch")
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(error.code, "minecraft_player_not_found");
    }

    #[tokio::test]
    async fn mojang_rate_limit_is_distinguished_from_service_failure() {
        let base = mock_mojang(429, "{}".to_owned(), Duration::ZERO).await;
        let error = resolve_java_profile(&reqwest::Client::new(), &base, "Notch")
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(error.code, "minecraft_profile_lookup_rate_limited");
    }

    #[tokio::test]
    async fn redirect_responses_are_not_followed_to_arbitrary_hosts() {
        let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_address = target.local_addr().unwrap();
        let (contacted_tx, mut contacted_rx) = tokio::sync::mpsc::unbounded_channel();
        let target_task = tokio::spawn(async move {
            if let Ok((mut stream, _)) = target.accept().await {
                let _ = contacted_tx.send(());
                let mut request = [0; 1024];
                let _ = stream.read(&mut request).await;
                let body = r#"{"id":"8667ba71b85a4004af54457a9734eed7","name":"Notch"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        let base = mock_mojang_with_location(
            302,
            String::new(),
            Duration::ZERO,
            Some(format!("http://{target_address}/internal")),
        )
        .await;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let error = resolve_java_profile(&client, &base, "Notch")
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.code, "minecraft_profile_lookup_unavailable");
        assert!(
            tokio::time::timeout(Duration::from_millis(25), contacted_rx.recv())
                .await
                .is_err()
        );
        target_task.abort();
    }

    #[tokio::test]
    async fn upstream_errors_and_invalid_profile_payloads_fail_closed() {
        let upstream = mock_mojang(503, "{}".to_owned(), Duration::ZERO).await;
        let error = resolve_java_profile(&reqwest::Client::new(), &upstream, "Notch")
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.code, "minecraft_profile_lookup_unavailable");

        for payload in [
            "not json",
            r#"{"id":"bad-uuid","name":"Notch"}"#,
            r#"{"id":"8667ba71b85a4004af54457a9734eed7","name":"Other"}"#,
        ] {
            let base = mock_mojang(200, payload.to_owned(), Duration::ZERO).await;
            let error = resolve_java_profile(&reqwest::Client::new(), &base, "Notch")
                .await
                .unwrap_err();
            assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(error.code, "minecraft_profile_lookup_unavailable");
        }

        let oversized = format!(
            r#"{{"id":"{}","name":"Notch","padding":"{}"}}"#,
            "8667ba71b85a4004af54457a9734eed7",
            "x".repeat(5000)
        );
        let base = mock_mojang(200, oversized, Duration::ZERO).await;
        let error = resolve_java_profile(&reqwest::Client::new(), &base, "Notch")
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.code, "minecraft_profile_lookup_unavailable");
    }

    #[tokio::test]
    async fn lookup_timeout_is_a_retryable_upstream_failure() {
        let base = mock_mojang(
            200,
            r#"{"id":"8667ba71b85a4004af54457a9734eed7","name":"Notch"}"#.to_owned(),
            Duration::from_millis(150),
        )
        .await;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(20))
            .build()
            .unwrap();
        let error = resolve_java_profile(&client, &base, "Notch")
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.code, "minecraft_profile_lookup_unavailable");
    }

    #[test]
    fn java_profile_name_validation_matches_mojang_constraints() {
        assert!(is_java_profile_name("Notch"));
        assert!(is_java_profile_name("player_123"));
        assert!(!is_java_profile_name("ab"));
        assert!(!is_java_profile_name("name with spaces"));
        assert!(!is_java_profile_name("a.b"));
        assert!(!is_java_profile_name("this_name_is_longer_than_16"));
    }

    #[test]
    fn mojang_profile_ids_are_normalized_to_uuid() {
        assert_eq!(
            parse_mojang_uuid("8667ba71b85a4004af54457a9734eed7")
                .expect("valid Mojang UUID")
                .to_string(),
            "8667ba71-b85a-4004-af54-457a9734eed7"
        );
        assert!(parse_mojang_uuid("not-a-profile").is_none());
        assert!(
            parse_mojang_uuid("00000000000000000000000000000000").is_some_and(|id| id.is_nil())
        );
    }
}

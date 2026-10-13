//! Mojang and Microsoft account lookups on behalf of sandboxed Pumpkin servers.
//!
//! A hosted server has no route to the internet, but an online-mode server
//! must ask Mojang whether a joining Java player owns their account and fetch
//! the keys that sign Bedrock logins. The worker answers exactly those
//! lookups here; a server reaches them through its own API proxy and can ask
//! for nothing else.

use axum::{
    body::Body,
    http::{StatusCode, header},
    response::Response,
};
use reqwest::{Client, Url};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::runtime_api_proxy::{MINECRAFT_SERVICES_PREFIX, SERVICE_BASE_URL};

const SESSION_SERVER: &str = "https://sessionserver.mojang.com";
const PROFILE_SERVER: &str = "https://api.mojang.com";
const SERVICES_SERVER: &str = "https://api.minecraftservices.com";
const BEDROCK_DISCOVERY: &str =
    "https://client.discovery.minecraft-services.net/api/v1.0/discovery/MinecraftPE/builds/1.0.0.0";
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

/// Answer one lookup. `path` has already matched the proxy's allowlist.
pub(crate) async fn handle(client: &Client, path: &str, query: Option<&str>) -> Response {
    match lookup(client, path, query).await {
        Ok(response) => response,
        Err(status) => empty(status),
    }
}

async fn lookup(client: &Client, path: &str, query: Option<&str>) -> Result<Response, StatusCode> {
    let route = path
        .strip_prefix(MINECRAFT_SERVICES_PREFIX)
        .ok_or(StatusCode::NOT_FOUND)?;
    let segments = route.split('/').collect::<Vec<_>>();
    match segments.as_slice() {
        ["session", "hasJoined"] => {
            let username = query_value(query, "username").filter(|name| valid_username(name));
            let server_hash = query_value(query, "serverId").filter(|hash| valid_server_hash(hash));
            let (Some(username), Some(server_hash)) = (username, server_hash) else {
                return Err(StatusCode::BAD_REQUEST);
            };
            let mut url = parse(&format!("{SESSION_SERVER}/session/minecraft/hasJoined"))?;
            url.query_pairs_mut()
                .append_pair("username", &username)
                .append_pair("serverId", &server_hash);
            forward(client, url).await
        }
        ["services", "publickeys"] => {
            forward(client, parse(&format!("{SERVICES_SERVER}/publickeys"))?).await
        }
        ["profile", "name", username] if valid_username(username) => {
            let url = format!("{PROFILE_SERVER}/users/profiles/minecraft/{username}");
            forward(client, parse(&url)?).await
        }
        ["profile", "uuid", id] => {
            let id = Uuid::parse_str(id).map_err(|_| StatusCode::BAD_REQUEST)?;
            let url = format!(
                "{SESSION_SERVER}/session/minecraft/profile/{}?unsigned=false",
                id.simple()
            );
            forward(client, parse(&url)?).await
        }
        // Pumpkin finds the Bedrock signing keys in three hops, each naming
        // the next by absolute URL. The first two answers are rewritten so
        // every hop comes back here.
        ["bedrock", "discovery"] => Ok(json_response(&json!({
            "result": {"serviceEnvironments": {"auth": {"prod": {
                "serviceUri": bedrock_base(),
            }}}}
        }))),
        ["bedrock", ".well-known", "openid-configuration"] => {
            let mut configuration = bedrock_openid_configuration(client).await?;
            configuration["jwks_uri"] = Value::String(format!("{}/keys", bedrock_base()));
            Ok(json_response(&configuration))
        }
        ["bedrock", "keys"] => {
            let configuration = bedrock_openid_configuration(client).await?;
            let keys = configuration
                .get("jwks_uri")
                .and_then(Value::as_str)
                .ok_or(StatusCode::BAD_GATEWAY)?;
            forward(client, https(keys)?).await
        }
        _ => Err(StatusCode::NOT_FOUND),
    }
}

fn bedrock_base() -> String {
    format!("{SERVICE_BASE_URL}{MINECRAFT_SERVICES_PREFIX}bedrock")
}

async fn bedrock_openid_configuration(client: &Client) -> Result<Value, StatusCode> {
    let discovery = fetch_json(client, parse(BEDROCK_DISCOVERY)?).await?;
    let service = discovery
        .pointer("/result/serviceEnvironments/auth/prod/serviceUri")
        .and_then(Value::as_str)
        .ok_or(StatusCode::BAD_GATEWAY)?;
    let url = format!(
        "{}/.well-known/openid-configuration",
        service.trim_end_matches('/')
    );
    fetch_json(client, https(&url)?).await
}

fn parse(url: &str) -> Result<Url, StatusCode> {
    Url::parse(url).map_err(|_| StatusCode::BAD_GATEWAY)
}

/// URLs named by an upstream answer are followed only over HTTPS.
fn https(url: &str) -> Result<Url, StatusCode> {
    parse(url).and_then(|url| {
        if url.scheme() == "https" {
            Ok(url)
        } else {
            Err(StatusCode::BAD_GATEWAY)
        }
    })
}

async fn fetch(client: &Client, url: Url) -> Result<(StatusCode, Vec<u8>), StatusCode> {
    let response = client.get(url).send().await.map_err(|error| {
        if error.is_timeout() {
            StatusCode::GATEWAY_TIMEOUT
        } else {
            StatusCode::BAD_GATEWAY
        }
    })?;
    let status =
        StatusCode::from_u16(response.status().as_u16()).map_err(|_| StatusCode::BAD_GATEWAY)?;
    let body = response
        .bytes()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(StatusCode::BAD_GATEWAY);
    }
    Ok((status, body.to_vec()))
}

async fn fetch_json(client: &Client, url: Url) -> Result<Value, StatusCode> {
    let (status, body) = fetch(client, url).await?;
    if status != StatusCode::OK {
        return Err(StatusCode::BAD_GATEWAY);
    }
    serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_GATEWAY)
}

/// Pass the upstream status through: Mojang answers 204 for a player who has
/// not joined, and Pumpkin tells that apart from an unreachable service.
async fn forward(client: &Client, url: Url) -> Result<Response, StatusCode> {
    let (status, body) = fetch(client, url).await?;
    let mut response = empty(status);
    if !body.is_empty() {
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        *response.body_mut() = Body::from(body);
    }
    Ok(response)
}

fn json_response(value: &Value) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(value.to_string()))
        .expect("static JSON response is valid")
}

fn empty(status: StatusCode) -> Response {
    Response::builder()
        .status(status)
        .body(Body::empty())
        .expect("static status response is valid")
}

fn query_value(query: Option<&str>, name: &str) -> Option<String> {
    let mut url = Url::parse("http://query.invalid/").ok()?;
    url.set_query(Some(query?));
    url.query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

fn valid_username(name: &str) -> bool {
    (1..=16).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Minecraft's server hash is a signed hexadecimal SHA-1 digest.
fn valid_server_hash(hash: &str) -> bool {
    let digits = hash.strip_prefix('-').unwrap_or(hash);
    (1..=40).contains(&digits.len()) && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_account_shaped_values_reach_mojang() {
        assert!(valid_username("flam3rboy"));
        assert!(!valid_username(""));
        assert!(!valid_username("name&ip=1.2.3.4"));
        assert!(!valid_username("seventeen_letters"));
        assert!(valid_server_hash(
            "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1"
        ));
        assert!(valid_server_hash(
            "4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48"
        ));
        assert!(!valid_server_hash("-"));
        assert!(!valid_server_hash("abc/../profile"));
    }

    #[test]
    fn query_values_are_decoded_by_name() {
        let query = Some("username=Alex&serverId=-1f");
        assert_eq!(query_value(query, "username").as_deref(), Some("Alex"));
        assert_eq!(query_value(query, "serverId").as_deref(), Some("-1f"));
        assert_eq!(query_value(query, "ip"), None);
        assert_eq!(query_value(None, "username"), None);
    }

    #[tokio::test]
    async fn unknown_lookups_are_not_forwarded() {
        let client = Client::new();
        for path in [
            "/minecraft/session/join",
            "/minecraft/bedrock",
            "/api/v1/server",
        ] {
            assert_eq!(
                handle(&client, path, None).await.status(),
                StatusCode::NOT_FOUND
            );
        }
        assert_eq!(
            handle(
                &client,
                "/minecraft/session/hasJoined",
                Some("username=a b")
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn bedrock_discovery_points_back_at_the_proxy() {
        let response = handle(&Client::new(), "/minecraft/bedrock/discovery", None).await;
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            value["result"]["serviceEnvironments"]["auth"]["prod"]["serviceUri"],
            "http://kurbis-runtime-api-proxy:8080/minecraft/bedrock"
        );
    }
}

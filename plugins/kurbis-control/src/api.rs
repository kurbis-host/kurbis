use std::{net::IpAddr, str::FromStr};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use wasi::{
    http::{
        outgoing_handler,
        types::{
            Fields, IncomingBody, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
        },
    },
    io::{
        poll::Pollable,
        streams::{InputStream, StreamError},
    },
};

pub(crate) const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const CONNECT_TIMEOUT_NANOS: u64 = 3_000_000_000;
const FIRST_BYTE_TIMEOUT_NANOS: u64 = 8_000_000_000;
const BETWEEN_BYTES_TIMEOUT_NANOS: u64 = 3_000_000_000;

pub(crate) struct Endpoint {
    scheme: Scheme,
    authority: String,
    prefix: String,
}

impl Endpoint {
    pub(crate) fn parse(value: &str, allow_insecure_local_http: bool) -> Result<Self> {
        let (scheme, remainder) = value
            .split_once("://")
            .context("API base URL must include a scheme")?;
        let scheme = match scheme {
            "https" => Scheme::Https,
            "http" => Scheme::Http,
            _ => bail!("API base URL must use HTTP or HTTPS"),
        };
        let (authority, prefix) = match remainder.split_once('/') {
            Some((authority, path)) => (authority, format!("/{path}")),
            None => (remainder, String::new()),
        };
        if authority.is_empty()
            || authority.contains('@')
            || authority.contains('?')
            || authority.contains('#')
            || authority
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control() || byte == b'\\')
            || prefix.contains('?')
            || prefix.contains('#')
            || !prefix.is_empty() && !prefix.starts_with('/')
        {
            bail!("API base URL is invalid");
        }
        if matches!(&scheme, Scheme::Http)
            && !(allow_insecure_local_http && is_local_http_authority(authority))
        {
            bail!("unencrypted API URLs are only allowed for local development");
        }
        Ok(Self {
            scheme,
            authority: authority.to_owned(),
            prefix: prefix.trim_end_matches('/').to_owned(),
        })
    }
}

fn is_local_http_authority(authority: &str) -> bool {
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split_once(']').map_or(authority, |(host, _)| host)
    } else {
        authority
            .rsplit_once(':')
            .filter(|(_, port)| port.bytes().all(|byte| byte.is_ascii_digit()))
            .map_or(authority, |(host, _)| host)
    };
    (host.eq_ignore_ascii_case("kurbis-runtime-api-proxy")
        && authority
            .rsplit_once(':')
            .is_some_and(|(_, port)| port == "8080"))
        || host.eq_ignore_ascii_case("localhost")
        || IpAddr::from_str(host).is_ok_and(|address| {
            address.is_loopback()
                || matches!(address, IpAddr::V4(value) if value.is_private())
                || matches!(address, IpAddr::V6(value) if value.is_unique_local())
        })
}

pub(crate) fn valid_token(token: &str) -> bool {
    token.len() == 43
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

pub(crate) fn get(path: &str) -> Result<Value> {
    request(Method::Get, path, None)
}

pub(crate) fn post(path: &str, value: &Value) -> Result<Value> {
    request(Method::Post, path, Some(value))
}

pub(crate) fn post_empty(path: &str) -> Result<Value> {
    request(Method::Post, path, None)
}

pub(crate) fn patch(path: &str, value: &Value) -> Result<Value> {
    request(Method::Patch, path, Some(value))
}

pub(crate) fn delete(path: &str) -> Result<Value> {
    request(Method::Delete, path, None)
}

pub(crate) fn server_path(suffix: &str) -> Result<String> {
    let config = crate::config()?;
    if config.ci_preview {
        anyhow::ensure!(
            suffix.is_empty()
                || suffix.starts_with('/') && !suffix.starts_with("//") && !suffix.contains('#'),
            "invalid CI preview API suffix"
        );
        return Ok(format!(
            "/internal/ci/previews/{}{}",
            config.server_id, suffix
        ));
    }
    server_path_for(config.server_id, suffix)
}

fn server_path_for(server_id: uuid::Uuid, suffix: &str) -> Result<String> {
    anyhow::ensure!(
        suffix.is_empty()
            || suffix.starts_with('/') && !suffix.starts_with("//") && !suffix.contains('#'),
        "invalid server API suffix"
    );
    Ok(format!("/api/v1/servers/{}{}", server_id, suffix))
}

pub(crate) fn update_setting(key: &str, value: Value) -> Result<()> {
    validate_setting_value(key, &value)?;
    let mut settings = serde_json::Map::new();
    settings.insert(key.to_owned(), value);
    patch(
        &server_path("")?,
        &serde_json::json!({"settings": settings}),
    )?;
    Ok(())
}

pub(crate) fn adjust_setting(key: &str, direction: i8) -> Result<()> {
    anyhow::ensure!(direction != 0, "setting adjustment cannot be zero");
    let server = get(&server_path("")?)?;
    let settings = server
        .get("settings")
        .context("server response omitted settings")?;
    let value = match key {
        "difficulty" => cycle_string(
            settings.get(key).and_then(Value::as_str),
            &["Peaceful", "Easy", "Normal", "Hard"],
            direction,
        )?,
        "gamemode" => cycle_string(
            settings.get(key).and_then(Value::as_str),
            &["Survival", "Creative", "Adventure", "Spectator"],
            direction,
        )?,
        "view_distance" | "simulation_distance" => {
            let current = settings
                .get(key)
                .and_then(Value::as_u64)
                .context("distance setting was not numeric")? as i16;
            Value::Number(current.saturating_add(direction as i16).clamp(2, 32).into())
        }
        "hardcore" | "whitelist" | "whitelist_by_members" | "tab_stats" => {
            Value::Bool(!settings.get(key).and_then(Value::as_bool).unwrap_or(false))
        }
        _ => anyhow::bail!("invalid settings key"),
    };
    update_setting(key, value)
}

fn cycle_string(current: Option<&str>, options: &[&str], direction: i8) -> Result<Value> {
    let current = current.context("setting was not a string")?;
    let index = options
        .iter()
        .position(|option| *option == current)
        .context("setting contains an unsupported value")?;
    let next = (index as i8 + direction).rem_euclid(options.len() as i8) as usize;
    Ok(Value::String(options[next].to_owned()))
}

fn validate_setting_value(key: &str, value: &Value) -> Result<()> {
    match key {
        "motd" if value.as_str().is_some_and(|text| text.len() <= 120) => Ok(()),
        "difficulty"
            if value
                .as_str()
                .is_some_and(|value| matches!(value, "Peaceful" | "Easy" | "Normal" | "Hard")) =>
        {
            Ok(())
        }
        "gamemode"
            if value.as_str().is_some_and(|value| {
                matches!(value, "Survival" | "Creative" | "Adventure" | "Spectator")
            }) =>
        {
            Ok(())
        }
        "view_distance" | "simulation_distance"
            if value
                .as_u64()
                .is_some_and(|value| (2..=32).contains(&value)) =>
        {
            Ok(())
        }
        "hardcore" | "whitelist" | "whitelist_by_members" | "tab_stats"
            if value.as_bool().is_some() =>
        {
            Ok(())
        }
        _ => anyhow::bail!("invalid settings key or value"),
    }
}

pub(crate) fn create_login_link() -> Result<String> {
    anyhow::ensure!(
        !crate::config()?.ci_preview,
        "server login links are unavailable for CI previews"
    );
    let response = post_empty("/api/v1/server/login-links")?;
    response
        .get("url")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .context("login link response omitted its URL")
}

/// Add a player to the whitelist. With `added_by`, the player in game who
/// adds them, the API checks that player may and records them; `player_id`
/// is then the UUID Pumpkin reported for someone it turned away. Without it
/// (the server console) the name is resolved like a panel addition.
pub(crate) fn add_whitelist_entry(
    name: &str,
    player_id: Option<uuid::Uuid>,
    added_by: Option<(uuid::Uuid, &str)>,
) -> Result<()> {
    let Some((adder_id, adder_name)) = added_by else {
        post(
            &server_path("/whitelist")?,
            &serde_json::json!({"name": name}),
        )?;
        return Ok(());
    };
    let mut body = serde_json::json!({
        "name": name,
        "added_by": {"player_id": adder_id, "name": adder_name},
    });
    if let Some(player_id) = player_id {
        body["player_id"] = Value::String(player_id.to_string());
    }
    post(&server_path("/whitelist/in-game")?, &body)?;
    Ok(())
}

/// Tell the panel the whitelist turned this player away.
pub(crate) fn report_join_request(player_id: uuid::Uuid, name: &str) -> Result<()> {
    post(
        &server_path("/whitelist/requests")?,
        &serde_json::json!({"player_id": player_id, "name": name}),
    )?;
    Ok(())
}

pub(crate) fn remove_whitelist_entry(player_id: &str) -> Result<()> {
    uuid::Uuid::parse_str(player_id).context("whitelist entry has an invalid player id")?;
    delete(&format!(
        "{}?player_id={player_id}",
        server_path("/whitelist")?
    ))?;
    Ok(())
}

pub(crate) fn server_command(
    action: crate::actions::LifecycleAction,
    generation: Option<i64>,
) -> Result<Value> {
    post(
        &server_path("/commands")?,
        &server_command_body(action, generation)?,
    )
}

fn server_command_body(
    action: crate::actions::LifecycleAction,
    generation: Option<i64>,
) -> Result<Value> {
    use crate::actions::ActionArgument;

    let spec = crate::actions::spec(action);
    match (spec.argument, generation) {
        (ActionArgument::None, None) => Ok(serde_json::json!({ "command": spec.command })),
        (ActionArgument::SnapshotGeneration, Some(generation)) => {
            anyhow::ensure!(generation > 0, "snapshot generation must be positive");
            Ok(serde_json::json!({
                "command": spec.command,
                "generation": generation,
            }))
        }
        (ActionArgument::None, Some(_)) => {
            anyhow::bail!(
                "{} command does not accept a snapshot generation",
                spec.command
            )
        }
        (ActionArgument::SnapshotGeneration, None) => {
            anyhow::bail!("{} command requires a snapshot generation", spec.command)
        }
    }
}

fn request(method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
    anyhow::ensure!(
        path.starts_with('/')
            && !path.starts_with("//")
            && !path
                .bytes()
                .any(|byte| matches!(byte, b'\\' | b'\r' | b'\n' | b'#')),
        "invalid API path"
    );
    let config = crate::config()?;
    let endpoint = Endpoint::parse(&config.api_base_url, config.allow_insecure_local_http)?;
    let headers = Fields::new();
    headers
        .set(
            "authorization",
            &[format!("Bearer {}", config.capability).into_bytes()],
        )
        .map_err(|_| anyhow::anyhow!("invalid authorization header"))?;
    headers
        .set("accept", &[b"application/json".to_vec()])
        .map_err(|_| anyhow::anyhow!("invalid accept header"))?;
    let body_bytes = body.map(serde_json::to_vec).transpose()?;
    anyhow::ensure!(
        body_bytes
            .as_ref()
            .is_none_or(|body| body.len() <= MAX_REQUEST_BYTES),
        "API request exceeded the size limit"
    );
    if body_bytes.is_some() {
        headers
            .set("content-type", &[b"application/json".to_vec()])
            .map_err(|_| anyhow::anyhow!("invalid content type header"))?;
    }

    let request = OutgoingRequest::new(headers);
    request
        .set_method(&method)
        .map_err(|_| anyhow::anyhow!("invalid HTTP method"))?;
    request
        .set_scheme(Some(&endpoint.scheme))
        .map_err(|_| anyhow::anyhow!("invalid API URL scheme"))?;
    request
        .set_authority(Some(&endpoint.authority))
        .map_err(|_| anyhow::anyhow!("invalid API host"))?;
    let path = format!("{}{}", endpoint.prefix, path);
    request
        .set_path_with_query(Some(&path))
        .map_err(|_| anyhow::anyhow!("invalid API path"))?;

    if let Some(bytes) = body_bytes {
        let body = request
            .body()
            .map_err(|_| anyhow::anyhow!("could not create API request body"))?;
        let stream = body
            .write()
            .map_err(|_| anyhow::anyhow!("could not write API request body"))?;
        stream
            .blocking_write_and_flush(&bytes)
            .context("send API request body")?;
        drop(stream);
        OutgoingBody::finish(body, None)
            .map_err(|error| anyhow::anyhow!("could not finish API request: {error}"))?;
    }

    let options = RequestOptions::new();
    options
        .set_connect_timeout(Some(CONNECT_TIMEOUT_NANOS))
        .map_err(|_| anyhow::anyhow!("Pumpkin does not support HTTP connect timeouts"))?;
    options
        .set_first_byte_timeout(Some(FIRST_BYTE_TIMEOUT_NANOS))
        .map_err(|_| anyhow::anyhow!("Pumpkin does not support HTTP response timeouts"))?;
    options
        .set_between_bytes_timeout(Some(BETWEEN_BYTES_TIMEOUT_NANOS))
        .map_err(|_| anyhow::anyhow!("Pumpkin does not support HTTP body timeouts"))?;

    let future = outgoing_handler::handle(request, Some(options))
        .map_err(|error| anyhow::anyhow!("API request was rejected: {error}"))?;
    let pollable = future.subscribe();
    Pollable::block(&pollable);
    drop(pollable);
    let response = future
        .get()
        .context("API response was not ready")?
        .map_err(|_| anyhow::anyhow!("API response future failed"))?
        .map_err(|error| anyhow::anyhow!("API request failed: {error}"))?;
    let status = response.status();
    let incoming = response
        .consume()
        .map_err(|_| anyhow::anyhow!("could not read API response"))?;
    let stream = incoming
        .stream()
        .map_err(|_| anyhow::anyhow!("could not read API response body"))?;
    let bytes = read_bounded(stream, incoming)?;
    if !(200..300).contains(&status) {
        let message = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| {
                value
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| format!("Control plane returned HTTP {status}"));
        bail!("{message}");
    }
    if status == 204 || bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).context("control plane returned invalid JSON")
}

fn read_bounded(stream: InputStream, incoming: IncomingBody) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    loop {
        // WASI input-stream.blocking-read signals EOF with `StreamError::Closed`.
        // Treating that as an ordinary failure skips `incoming-body.finish`;
        // dropping the parent body while its stream is still open traps in
        // Pumpkin's Wasmtime host.
        let chunk = match stream.blocking_read(8192) {
            Ok(chunk) => chunk,
            Err(StreamError::Closed) => break,
            Err(error) => {
                drop(stream);
                drop(incoming);
                return Err(anyhow::anyhow!("read API response body: {error}"));
            }
        };
        if chunk.is_empty() {
            break;
        }
        if output.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            drop(stream);
            drop(incoming);
            bail!("control plane response exceeded the size limit");
        }
        output.extend_from_slice(&chunk);
    }
    drop(stream);
    let trailers = IncomingBody::finish(incoming);
    let pollable = trailers.subscribe();
    Pollable::block(&pollable);
    drop(pollable);
    let response_trailers = trailers
        .get()
        .context("API response body trailers were not ready")?
        .map_err(|_| anyhow::anyhow!("API response body trailers were already consumed"))?
        .map_err(|error| anyhow::anyhow!("receive API response body: {error:?}"))?;
    drop(response_trailers);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_endpoint_requires_tls_except_for_loopback_development() {
        let endpoint = Endpoint::parse("https://control.example.test/prefix/", false).unwrap();
        assert_eq!(endpoint.authority, "control.example.test");
        assert_eq!(endpoint.prefix, "/prefix");
        assert!(Endpoint::parse("http://localhost:8080/api", true).is_ok());
        assert!(Endpoint::parse("http://172.18.0.1:18080/api", false).is_err());
        assert!(Endpoint::parse("http://172.18.0.1:18080/api", true).is_ok());
        assert!(Endpoint::parse("http://control.example.test", true).is_err());
        assert!(Endpoint::parse("http://203.0.113.10", true).is_err());
        assert!(Endpoint::parse("https://user@control.example.test", false).is_err());
        assert!(Endpoint::parse("https://control.example.test?token=bad", false).is_err());
    }

    #[test]
    fn server_paths_are_derived_from_config_and_reject_origin_like_suffixes() {
        let server_id = uuid::Uuid::parse_str("1f2ebf5d-4219-45c4-ae0f-3a61535bb01a").unwrap();
        assert_eq!(
            server_path_for(server_id, "/players").unwrap(),
            "/api/v1/servers/1f2ebf5d-4219-45c4-ae0f-3a61535bb01a/players"
        );
        assert!(server_path_for(server_id, "//other-server").is_err());
        assert!(server_path_for(server_id, "/players#fragment").is_err());
    }

    #[test]
    fn bearer_capability_has_a_fixed_url_safe_shape() {
        assert!(valid_token(&"a".repeat(43)));
        assert!(!valid_token(&"a".repeat(42)));
        assert!(!valid_token(&format!("{}.", "a".repeat(42))));
    }

    #[test]
    fn insecure_http_is_limited_to_the_fixed_runtime_proxy_endpoint() {
        assert!(Endpoint::parse("http://kurbis-runtime-api-proxy:8080", true).is_ok());
        assert!(Endpoint::parse("http://kurbis-runtime-api-proxy:8081", true).is_err());
        assert!(Endpoint::parse("http://other-proxy:8080", true).is_err());
        assert!(Endpoint::parse("http://kurbis-runtime-api-proxy:8080", false).is_err());
    }

    #[test]
    fn setting_values_are_validated_before_being_sent_to_the_control_plane() {
        for (key, value) in [
            ("motd", serde_json::json!("Welcome")),
            ("difficulty", serde_json::json!("Hard")),
            ("gamemode", serde_json::json!("Creative")),
            ("view_distance", serde_json::json!(32)),
            ("simulation_distance", serde_json::json!(2)),
            ("hardcore", serde_json::json!(true)),
            ("whitelist", serde_json::json!(false)),
            ("tab_stats", serde_json::json!(true)),
        ] {
            assert!(validate_setting_value(key, &value).is_ok(), "{key}={value}");
        }
        assert!(validate_setting_value("view_distance", &serde_json::json!(1)).is_err());
        assert!(validate_setting_value("difficulty", &serde_json::json!("Impossible")).is_err());
        assert!(validate_setting_value("unknown", &serde_json::json!(true)).is_err());
        assert!(validate_setting_value("motd", &serde_json::json!("x".repeat(121))).is_err());
    }

    #[test]
    fn action_registry_builds_the_canonical_server_command_bodies() {
        for action in [
            crate::actions::LifecycleAction::Start,
            crate::actions::LifecycleAction::Stop,
            crate::actions::LifecycleAction::Restart,
            crate::actions::LifecycleAction::Snapshot,
        ] {
            let spec = crate::actions::spec(action);
            assert_eq!(
                server_command_body(action, None).unwrap(),
                serde_json::json!({"command": spec.command}),
            );
        }
        assert_eq!(
            server_command_body(crate::actions::LifecycleAction::Restore, Some(17)).unwrap(),
            serde_json::json!({"command":"restore","generation":17}),
        );
        assert!(server_command_body(crate::actions::LifecycleAction::Restore, None).is_err());
        assert!(server_command_body(crate::actions::LifecycleAction::Restore, Some(0)).is_err());
        assert!(server_command_body(crate::actions::LifecycleAction::Start, Some(17)).is_err());
    }
}

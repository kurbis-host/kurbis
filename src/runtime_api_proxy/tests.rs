use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::net::TcpListener;

fn id() -> Uuid {
    Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap()
}

#[test]
fn normal_scope_allows_only_server_capability_routes() {
    let expected = id();
    for (method, path) in [
        (Method::GET, format!("/api/v1/servers/{expected}")),
        (Method::PATCH, format!("/api/v1/servers/{expected}")),
        (Method::GET, format!("/api/v1/servers/{expected}/players")),
        (Method::GET, format!("/api/v1/servers/{expected}/logs")),
        (Method::GET, format!("/api/v1/servers/{expected}/activity")),
        (Method::GET, format!("/api/v1/servers/{expected}/snapshots")),
        (Method::GET, format!("/api/v1/servers/{expected}/whitelist")),
        (
            Method::POST,
            format!("/api/v1/servers/{expected}/whitelist"),
        ),
        (
            Method::DELETE,
            format!("/api/v1/servers/{expected}/whitelist"),
        ),
        (Method::POST, format!("/api/v1/servers/{expected}/commands")),
        (Method::POST, "/api/v1/server/login-links".to_owned()),
    ] {
        assert!(
            route_allowed(Scope::Server, &method, &path, expected),
            "{method} {path}"
        );
    }
}

#[test]
fn normal_scope_denies_sibling_account_and_worker_routes() {
    let expected = id();
    for (method, path) in [
        (
            Method::GET,
            format!("/api/v1/servers/{}/players", Uuid::new_v4()),
        ),
        (Method::GET, "/api/v1/account".to_owned()),
        (Method::GET, format!("/api/v1/servers/{expected}/exports")),
        (
            Method::POST,
            "/internal/worker/control/heartbeat".to_owned(),
        ),
        (Method::GET, format!("/internal/ci/previews/{expected}")),
        (Method::GET, format!("/api/v1/servers/{expected}/operators")),
    ] {
        assert!(
            !route_allowed(Scope::Server, &method, &path, expected),
            "{method} {path}"
        );
    }
}

#[test]
fn ci_scope_is_preview_only_and_cannot_reach_account_routes() {
    let expected = id();
    assert!(route_allowed(
        Scope::Ci,
        &Method::GET,
        &format!("/internal/ci/previews/{expected}"),
        expected
    ));
    assert!(route_allowed(
        Scope::Ci,
        &Method::PATCH,
        &format!("/internal/ci/previews/{expected}"),
        expected
    ));
    assert!(route_allowed(
        Scope::Ci,
        &Method::GET,
        &format!("/internal/ci/previews/{expected}/players"),
        expected
    ));
    assert!(route_allowed(
        Scope::Ci,
        &Method::POST,
        &format!("/internal/ci/previews/{expected}/commands"),
        expected
    ));
    assert!(!route_allowed(
        Scope::Ci,
        &Method::GET,
        "/api/v1/account",
        expected
    ));
    assert!(!route_allowed(
        Scope::Ci,
        &Method::GET,
        &format!("/api/v1/servers/{expected}"),
        expected
    ));
    assert!(!route_allowed(
        Scope::Ci,
        &Method::GET,
        &format!("/internal/ci/previews/{}/players", Uuid::new_v4()),
        expected
    ));
}

#[tokio::test]
async fn capability_proxy_forwards_allowed_request_and_blocks_scope_escape() {
    async fn upstream(State(count): State<Arc<AtomicUsize>>, request: Request<Body>) -> Response {
        count.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.method(), Method::GET);
        assert_eq!(
            request.headers().get(header::AUTHORIZATION).unwrap(),
            "Bearer AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        );
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"players":[]}"#))
            .unwrap()
    }

    let forwarded = Arc::new(AtomicUsize::new(0));
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_address = upstream_listener.local_addr().unwrap();
    let count = forwarded.clone();
    tokio::spawn(async move {
        axum::serve(
            upstream_listener,
            Router::new().fallback(upstream).with_state(count),
        )
        .await
        .unwrap();
    });

    let expected_id = id();
    let token = "A".repeat(43);
    let config = ProxyConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        upstream: Url::parse(&format!("http://{upstream_address}")).unwrap(),
        scope: Scope::Server,
        expected_id,
        expected_capability_hash: *blake3::hash(token.as_bytes()).as_bytes(),
    };
    let proxy_listener = TcpListener::bind(config.listen).await.unwrap();
    let proxy_address = proxy_listener.local_addr().unwrap();
    let proxy_task = tokio::spawn(async move {
        axum::serve(
            proxy_listener,
            proxy_router(ProxyState {
                config,
                client: Client::builder().redirect(Policy::none()).build().unwrap(),
            }),
        )
        .await
        .unwrap();
    });
    let client = reqwest::Client::new();
    let own_url = format!("http://{proxy_address}/api/v1/servers/{expected_id}/players");
    let own = client
        .get(&own_url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(own.status(), StatusCode::OK);
    assert_eq!(
        own.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({"players": []}),
    );
    assert_eq!(forwarded.load(Ordering::SeqCst), 1);

    for path in [
        "/api/v1/account".to_owned(),
        format!("/api/v1/servers/{}/players", Uuid::new_v4()),
        "/internal/worker/control/heartbeat".to_owned(),
    ] {
        let response = client
            .get(format!("http://{proxy_address}{path}"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    let wrong_capability = client
        .get(&own_url)
        .bearer_auth("B".repeat(43))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_capability.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        forwarded.load(Ordering::SeqCst),
        1,
        "denied requests never reach the control plane"
    );
    proxy_task.abort();
}

#[test]
fn proxy_egress_requires_ipv4_bridge_and_remote_https() {
    let internal = serde_json::json!({
        "Driver": "bridge",
        "Internal": true,
        "EnableIPv6": false,
    });
    assert_eq!(validate_egress_network_metadata(&internal).unwrap(), true);
    let mut external = internal;
    external["Internal"] = serde_json::Value::Bool(false);
    assert_eq!(validate_egress_network_metadata(&external).unwrap(), false);
    assert!(validate_upstream_url("https://control.example", false).is_ok());
    assert!(validate_upstream_url("http://control.example", true).is_err());
    for invalid in [
        serde_json::json!({"Driver":"bridge", "Internal":true, "EnableIPv6":true}),
        serde_json::json!({"Driver":"overlay", "Internal":true, "EnableIPv6":false}),
    ] {
        assert!(validate_egress_network_metadata(&invalid).is_err());
    }
}

#[test]
fn configured_upstream_health_probe_is_same_origin_and_path_pinned() {
    let upstream = validate_upstream_url("https://control.example/base", false);
    assert!(upstream.is_err(), "base paths are not accepted as origins");
    let mut upstream = validate_upstream_url("https://control.example", false).unwrap();
    upstream.set_path("/healthz");
    assert_eq!(upstream.as_str(), "https://control.example/healthz");
}

#[test]
fn origin_and_upstream_validation_are_strict() {
    let base = Url::parse("https://control.example").unwrap();
    assert_eq!(
        upstream_url(&base, "/api/v1/account", Some("x=1"))
            .unwrap()
            .as_str(),
        "https://control.example/api/v1/account?x=1"
    );
    assert!(validate_upstream_url("https://control.example", false).is_ok());
    assert!(validate_upstream_url("http://localhost:8080", false).is_err());
    assert!(validate_upstream_url("http://172.18.0.1:8080", true).is_ok());
    assert!(validate_upstream_url("http://control-plane:8080", true).is_ok());
    assert!(validate_upstream_url("http://control.example", true).is_err());
    assert!(validate_upstream_url("https://control.example/prefix", false).is_err());
    assert!(validate_upstream_url("https://user@control.example", false).is_err());
    assert!(hex_decode_32(&"a".repeat(64)).is_ok());
    assert!(hex_decode_32("not-a-hash").is_err());
}

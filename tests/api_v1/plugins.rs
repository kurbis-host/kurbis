use std::sync::Mutex as StdMutex;

use axum::{Router, extract::Path as RoutePath, routing::get};
use kurbis_hoster::control_plane::plugins::PluginMarket;

use super::support::*;

/// A WebAssembly component header followed by `marker`, so each test binary
/// has its own hash.
fn component(marker: &str) -> Vec<u8> {
    let mut bytes = vec![0x00, b'a', b's', b'm', 0x0d, 0x00, 0x01, 0x00];
    bytes.extend_from_slice(marker.as_bytes());
    bytes
}

/// Stand-in for market.pumpkinmc.org serving a scripted catalog.
#[derive(Clone, Default)]
struct FakeMarket {
    listings: Arc<StdMutex<Vec<Value>>>,
    downloads: Arc<StdMutex<Vec<i64>>>,
}

impl FakeMarket {
    async fn start(listings: Vec<Value>) -> (Self, String) {
        let fake = Self {
            listings: Arc::new(StdMutex::new(listings)),
            ..Self::default()
        };
        let (catalog, one, download) = (fake.clone(), fake.clone(), fake.clone());
        let app = Router::new()
            .route(
                "/api/plugins",
                get(move || {
                    let fake = catalog.clone();
                    async move { axum::Json(Value::Array(fake.listings.lock().unwrap().clone())) }
                }),
            )
            .route(
                "/api/plugins/{id}",
                get(move |RoutePath(id): RoutePath<i64>| {
                    let fake = one.clone();
                    async move {
                        fake.listing(id)
                            .map(axum::Json)
                            .ok_or(StatusCode::NOT_FOUND)
                    }
                }),
            )
            .route(
                "/api/plugins/{id}/download",
                get(move |RoutePath(id): RoutePath<i64>| {
                    let fake = download.clone();
                    async move {
                        let listing = fake.listing(id).ok_or(StatusCode::NOT_FOUND)?;
                        fake.downloads.lock().unwrap().push(id);
                        Ok::<_, StatusCode>(component(&format!(
                            "{id}-{}",
                            listing["version"].as_str().unwrap()
                        )))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (fake, format!("http://{address}"))
    }

    fn listing(&self, id: i64) -> Option<Value> {
        self.listings
            .lock()
            .unwrap()
            .iter()
            .find(|listing| listing["id"] == id)
            .cloned()
    }

    fn set_version(&self, id: i64, version: &str) {
        for listing in self.listings.lock().unwrap().iter_mut() {
            if listing["id"] == id {
                listing["version"] = json!(version);
            }
        }
    }

    fn downloads(&self) -> Vec<i64> {
        self.downloads.lock().unwrap().clone()
    }
}

fn listing(id: i64, name: &str, downloads: i64, price_cents: i64) -> Value {
    json!({
        "id": id,
        "name": name,
        "dev_name": "PING",
        "category": "Admin Tools",
        "version": "0.1",
        "downloads": downloads,
        "price_cents": price_cents,
        "type": if price_cents == 0 { "free" } else { "paid" },
        "status": "published",
        "is_preorder": false,
        "preview_path": "https://assets.pumpkinmc.org/icons/icon.jpg",
        "translated_descriptions": {"en-US": format!("{name} for your server.")}
    })
}

struct Fixture {
    app: Router,
    pool: PgPool,
    market: FakeMarket,
    owner_id: Uuid,
    owner_token: String,
    server_id: Uuid,
    worker_id: Uuid,
    worker_token: String,
    capability: String,
}

impl Fixture {
    async fn new() -> Self {
        let pool = test_pool().await;
        let (owner_id, owner_token) = user_session(&pool, "active").await;
        let server_id = create_test_server(&pool, owner_id, "plugins").await;
        let worker_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO workers(id, name, address, signal_address, total_memory_bytes,
                                total_disk_bytes, total_cpu_millis, port_min, port_max)
             VALUES($1, $2, '127.0.0.1', '127.0.0.1', 8589934592, 42949672960, 4000, 24000, 24100)",
        )
        .bind(worker_id)
        .bind(format!("plugins-{worker_id}"))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE servers SET generation = 1 WHERE id = $1")
            .bind(server_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO placements(server_id, worker_id, generation, lease_token,
                                    java_port, bedrock_port, signal_port, active)
             VALUES($1, $2, 1, $3, 24000, 24001, 24002, true)",
        )
        .bind(server_id)
        .bind(worker_id)
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();
        let capability = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
        sqlx::query(
            "INSERT INTO server_instance_capabilities(server_id, worker_id,
                                                       placement_generation, token_hash)
             VALUES($1, $2, 1, $3)",
        )
        .bind(server_id)
        .bind(worker_id)
        .bind(blake3::hash(capability.as_bytes()).as_bytes().to_vec())
        .execute(&pool)
        .await
        .unwrap();
        let worker_token = register_worker_api_token(&pool, worker_id).await;

        let (market, market_url) = FakeMarket::start(vec![
            listing(55, "TPSBar", 200, 0),
            listing(56, "Homes", 900, 0),
            listing(57, "Premium", 5000, 499),
        ])
        .await;
        let mut state = Arc::into_inner(state(pool.clone())).unwrap();
        state.plugin_market = PluginMarket::new(&market_url).unwrap();
        Self {
            app: router(Arc::new(state)),
            pool,
            market,
            owner_id,
            owner_token,
            server_id,
            worker_id,
            worker_token,
            capability,
        }
    }

    async fn cleanup(self) {
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(self.owner_id)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM workers WHERE id = $1")
            .bind(self.worker_id)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    async fn call(&self, request: Request<Body>) -> (StatusCode, Value) {
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, body)
    }

    fn uri(&self, path: &str) -> String {
        format!("/api/v1/servers/{}/plugins{path}", self.server_id)
    }

    async fn owner(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        self.call(request(
            method,
            &self.uri(path),
            Some(&self.owner_token),
            body,
        ))
        .await
    }

    /// A request from the server's in-game plugin.
    async fn plugin(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        self.call(bearer_request(
            method,
            &self.uri(path),
            &self.capability,
            body,
        ))
        .await
    }

    async fn upload(
        &self,
        file_name: &str,
        content_type: &str,
        bytes: Vec<u8>,
    ) -> (StatusCode, Value) {
        self.call(
            Request::builder()
                .method(Method::POST)
                .uri(format!("{}?file_name={file_name}", self.uri("/uploads")))
                .header(
                    header::COOKIE,
                    format!("kurbis_session={}", self.owner_token),
                )
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(bytes))
                .unwrap(),
        )
        .await
    }

    async fn worker(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let uri = format!("/internal/worker/control/servers/{}{path}", self.server_id);
        self.call(bearer_request(method, &uri, &self.worker_token, body))
            .await
    }

    async fn revision(&self) -> (i64, i64) {
        sqlx::query_as(
            "SELECT revision, applied_revision FROM server_plugin_state WHERE server_id = $1",
        )
        .bind(self.server_id)
        .fetch_optional(&self.pool)
        .await
        .unwrap()
        .unwrap_or((0, 0))
    }
}

#[tokio::test]
async fn market_lists_only_free_plugins_and_installs_updates_and_removes_them() {
    let _serial = DB_TEST_LOCK.lock().await;
    let fixture = Fixture::new().await;

    let (status, market) = fixture.owner(Method::GET, "/market", None).await;
    assert_eq!(status, StatusCode::OK, "{market}");
    let names: Vec<_> = market["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|listing| listing["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["Homes", "TPSBar"],
        "free listings, most downloaded first"
    );

    let (status, found) = fixture.owner(Method::GET, "/market?query=tps", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(found["plugins"].as_array().unwrap().len(), 1);

    let (status, error) = fixture
        .plugin(
            Method::POST,
            "/market",
            Some(json!({"market_plugin_id": 57})),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "paid plugins are not installable"
    );
    assert_eq!(error["code"], "market_plugin_not_installable");

    // The in-game plugin installs through its server capability.
    let (status, installed) = fixture
        .plugin(
            Method::POST,
            "/market",
            Some(json!({"market_plugin_id": 55})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{installed}");
    assert_eq!(installed["name"], "TPSBar");
    assert_eq!(installed["version"], "0.1");
    assert_eq!(installed["source"], "market");
    assert_eq!(installed["state"], "pending");
    assert_eq!(fixture.revision().await, (1, 0));

    // Installing the same version again is a no-op that downloads nothing.
    let (status, again) = fixture
        .owner(
            Method::POST,
            "/market",
            Some(json!({"market_plugin_id": 55})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["id"], installed["id"]);
    assert_eq!(fixture.market.downloads(), [55]);
    assert_eq!(fixture.revision().await, (1, 0));

    let (_, market) = fixture.owner(Method::GET, "/market?query=tps", None).await;
    assert_eq!(market["plugins"][0]["installed_plugin_id"], installed["id"]);
    assert_eq!(market["plugins"][0]["installed_version"], "0.1");

    fixture.market.set_version(55, "0.2");
    let (status, updated) = fixture
        .owner(
            Method::POST,
            "/market",
            Some(json!({"market_plugin_id": 55})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["id"], installed["id"], "an update keeps the plugin");
    assert_eq!(updated["version"], "0.2");
    assert_ne!(updated["sha256"], installed["sha256"]);
    assert_eq!(fixture.revision().await, (2, 0));

    let plugin_path = format!("/{}", installed["id"].as_str().unwrap());
    let (status, disabled) = fixture
        .plugin(Method::PATCH, &plugin_path, Some(json!({"enabled": false})))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(disabled["state"], "disabled");
    assert_eq!(fixture.revision().await, (3, 0));

    let (status, list) = fixture.owner(Method::GET, "", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["plugins"].as_array().unwrap().len(), 1);
    assert_eq!(list["revision"], 3);
    assert_eq!(list["synchronized"], false);
    assert_eq!(list["max_plugins"], 16);

    let (status, _) = fixture.plugin(Method::DELETE, &plugin_path, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, error) = fixture.owner(Method::DELETE, &plugin_path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["code"], "plugin_not_found");
    assert_eq!(fixture.revision().await, (4, 0));
    fixture.cleanup().await;
}

#[tokio::test]
async fn owners_upload_components_from_the_browser_only() {
    let _serial = DB_TEST_LOCK.lock().await;
    let fixture = Fixture::new().await;
    let marker = Uuid::new_v4().to_string();

    let (status, error) = fixture
        .upload("mine.wasm", "application/octet-stream", component(&marker))
        .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(error["code"], "wasm_content_type_required");

    let module = [0x00, b'a', b's', b'm', 0x01, 0x00, 0x00, 0x00].to_vec();
    let (status, error) = fixture
        .upload("mine.wasm", "application/wasm", module)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["code"], "invalid_plugin");
    assert!(error["message"].as_str().unwrap().contains("wasm32-wasip2"));

    let (status, uploaded) = fixture
        .upload("My%20Plugin.wasm", "application/wasm", component(&marker))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{uploaded}");
    assert_eq!(uploaded["name"], "My Plugin");
    assert_eq!(uploaded["source"], "upload");
    assert_eq!(uploaded["market_plugin_id"], Value::Null);

    let (status, error) = fixture
        .upload("copy.wasm", "application/wasm", component(&marker))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["code"], "plugin_already_installed");

    // The in-game capability cannot upload code: browser-only routes do not
    // know the server for it.
    let (status, error) = fixture
        .call(
            Request::builder()
                .method(Method::POST)
                .uri(format!("{}?file_name=x.wasm", fixture.uri("/uploads")))
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", fixture.capability),
                )
                .header(header::CONTENT_TYPE, "application/wasm")
                .body(Body::from(component("capability")))
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["code"], "server_not_found");
    let (_, list) = fixture.owner(Method::GET, "", None).await;
    assert_eq!(list["plugins"].as_array().unwrap().len(), 1);
    fixture.cleanup().await;
}

#[tokio::test]
async fn workers_download_enabled_binaries_and_report_what_loaded() {
    let _serial = DB_TEST_LOCK.lock().await;
    let fixture = Fixture::new().await;
    let marker = Uuid::new_v4().to_string();
    let (_, uploaded) = fixture
        .upload("loaded.wasm", "application/wasm", component(&marker))
        .await;
    let plugin_id = uploaded["id"].as_str().unwrap().to_owned();
    let sha256 = uploaded["sha256"].as_str().unwrap().to_owned();

    let (status, context) = fixture
        .worker(
            Method::GET,
            "/context?assignment=placement&generation=1",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{context}");
    assert_eq!(context["plugin_revision"], 1);
    assert_eq!(context["plugin_applied_revision"], 0);
    assert_eq!(context["plugins"][0]["id"], plugin_id);
    assert_eq!(context["plugins"][0]["sha256"], sha256);

    let binary_path = format!("/plugins/{plugin_id}/binary?assignment=placement&generation=1");
    let response = fixture
        .app
        .clone()
        .oneshot(bearer_request(
            Method::GET,
            &format!(
                "/internal/worker/control/servers/{}{binary_path}",
                fixture.server_id
            ),
            &fixture.worker_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-kurbis-plugin-sha256"],
        sha256.as_str()
    );
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert_eq!(bytes.as_ref(), component(&marker).as_slice());

    let (status, _) = fixture
        .worker(
            Method::GET,
            &format!("/plugins/{plugin_id}/binary?assignment=placement&generation=2"),
            None,
        )
        .await;
    assert!(status.is_client_error(), "a stale placement gets nothing");

    let (status, _) = fixture
        .worker(
            Method::POST,
            "/plugins-applied",
            Some(json!({
                "generation": 1,
                "revision": 1,
                "plugins": [{
                    "id": plugin_id,
                    "sha256": sha256,
                    "name": "loaded",
                    "version": "1.0.0",
                    "error": "missing export",
                }],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(fixture.revision().await, (1, 1));
    let (_, list) = fixture.owner(Method::GET, "", None).await;
    assert_eq!(list["synchronized"], true);
    assert_eq!(list["plugins"][0]["state"], "failed");
    assert_eq!(list["plugins"][0]["error"], "missing export");
    assert_eq!(list["plugins"][0]["runtime_version"], "1.0.0");

    // A disabled plugin is no longer served to the worker.
    fixture
        .owner(
            Method::PATCH,
            &format!("/{plugin_id}"),
            Some(json!({"enabled": false})),
        )
        .await;
    let (status, _) = fixture.worker(Method::GET, &binary_path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, context) = fixture
        .worker(
            Method::GET,
            "/context?assignment=placement&generation=1",
            None,
        )
        .await;
    assert_eq!(context["plugins"], json!([]));
    assert_eq!(context["plugin_revision"], 2);
    fixture.cleanup().await;
}

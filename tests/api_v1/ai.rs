use std::{collections::VecDeque, sync::Mutex as StdMutex};

use axum::{Router, extract::State, http::HeaderMap, routing::post};
use kurbis_hoster::{
    config::{AiConfig, SecretString},
    contracts::ai::AiEffort,
    control_plane::ai::{
        AiGateway,
        catalog::{self, Tokens},
    },
};

use super::support::*;

const OPUS: &str = "claude-opus-5-5";

/// Scripted stand-in for the Claude Messages API that records every request.
#[derive(Clone, Default)]
struct FakeClaude {
    responses: Arc<StdMutex<VecDeque<Value>>>,
    requests: Arc<StdMutex<Vec<(HeaderMap, Value)>>>,
}

impl FakeClaude {
    async fn start() -> (Self, String) {
        let fake = Self::default();
        let app = Router::new()
            .route("/v1/messages", post(Self::messages))
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (fake, format!("http://{address}"))
    }

    async fn messages(
        State(fake): State<Self>,
        headers: HeaderMap,
        body: Bytes,
    ) -> (StatusCode, String) {
        fake.requests
            .lock()
            .unwrap()
            .push((headers, serde_json::from_slice(&body).unwrap()));
        match fake.responses.lock().unwrap().pop_front() {
            Some(response) => (StatusCode::OK, response.to_string()),
            None => (
                StatusCode::BAD_REQUEST,
                json!({"type":"error","error":{"type":"invalid_request_error","message":"unexpected request"}})
                    .to_string(),
            ),
        }
    }

    fn respond(&self, response: Value) {
        self.responses.lock().unwrap().push_back(response);
    }

    fn requests(&self) -> Vec<(HeaderMap, Value)> {
        self.requests.lock().unwrap().clone()
    }
}

fn usage(input: u64, output: u64, cache_creation: u64, cache_read: u64) -> Value {
    json!({
        "input_tokens": input,
        "output_tokens": output,
        "cache_creation_input_tokens": cache_creation,
        "cache_read_input_tokens": cache_read,
    })
}

fn message(content: Value, stop_reason: &str, usage: Value) -> Value {
    json!({
        "id": format!("msg_{}", Uuid::new_v4().simple()),
        "type": "message",
        "role": "assistant",
        "model": OPUS,
        "content": content,
        "stop_reason": stop_reason,
        "usage": usage,
    })
}

fn answer(text: &str, usage: Value) -> Value {
    message(
        json!([
            {"type": "thinking", "thinking": "", "signature": "sig-answer"},
            {"type": "text", "text": text},
        ]),
        "end_turn",
        usage,
    )
}

fn ai_config(base_url: &str) -> AiConfig {
    AiConfig {
        api_key: SecretString::new("test-anthropic-key"),
        base_url: base_url.to_owned(),
        default_model: OPUS.to_owned(),
        default_effort: AiEffort::Medium,
        server_monthly_budget_micros: 5_000_000,
        account_monthly_budget_micros: 20_000_000,
        max_concurrent_calls: 4,
    }
}

struct AiFixture {
    pool: PgPool,
    app: axum::Router,
    gateway: AiGateway,
    fake: FakeClaude,
    base_url: String,
    owner_id: Uuid,
    owner_token: String,
    server_id: Uuid,
    worker_id: Uuid,
    capability: String,
    operator_id: Uuid,
}

impl AiFixture {
    async fn new() -> Self {
        let pool = test_pool().await;
        // Only these tests create AI jobs. Jobs left behind by an aborted run
        // would otherwise be claimed by this run's runner.
        sqlx::query("DELETE FROM ai_jobs")
            .execute(&pool)
            .await
            .unwrap();
        let (owner_id, owner_token) = user_session(&pool, "active").await;
        let server_id = create_test_server(&pool, owner_id, "ai").await;
        let worker_id = Uuid::new_v4();
        let capability = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
        sqlx::query(
            "INSERT INTO workers(id, name, address, signal_address, total_memory_bytes,
                                total_disk_bytes, total_cpu_millis, port_min, port_max)
             VALUES($1, $2, '127.0.0.1', '127.0.0.1', 8589934592, 42949672960, 4000, 24000, 24100)",
        )
        .bind(worker_id)
        .bind(format!("ai-{worker_id}"))
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
        let operator_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO operator_entries(server_id, player_id, name) VALUES($1, $2, 'Steve')",
        )
        .bind(server_id)
        .bind(operator_id)
        .execute(&pool)
        .await
        .unwrap();

        let (fake, base_url) = FakeClaude::start().await;
        let gateway = AiGateway::new(pool.clone(), ai_config(&base_url)).unwrap();
        let mut state = Arc::into_inner(state(pool.clone())).unwrap();
        state.ai = Some(gateway.clone());
        Self {
            app: router(Arc::new(state)),
            pool,
            gateway,
            fake,
            base_url,
            owner_id,
            owner_token,
            server_id,
            worker_id,
            capability,
            operator_id,
        }
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

    /// A request from the server's plugin, authenticated by its capability.
    async fn plugin(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let uri = format!("/api/v1/servers/{}/ai{path}", self.server_id);
        self.call(bearer_request(method, &uri, &self.capability, body))
            .await
    }

    /// A request from the owner's browser session.
    async fn owner(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let uri = format!("/api/v1/servers/{}/ai{path}", self.server_id);
        self.call(request(method, &uri, Some(&self.owner_token), body))
            .await
    }

    async fn ask(&self, job_id: Uuid, prompt: &str) -> (StatusCode, Value) {
        self.plugin(
            Method::POST,
            "/jobs",
            Some(json!({
                "job_id": job_id,
                "operator_id": self.operator_id,
                "operator_name": "Steve",
                "prompt": prompt,
            })),
        )
        .await
    }

    async fn sync(&self, results: Value, after_event_id: Option<i64>) -> Value {
        let (status, body) = self
            .plugin(
                Method::POST,
                "/sync",
                Some(json!({"results": results, "after_event_id": after_event_id})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    async fn run(&self) {
        self.gateway.run_jobs().await.unwrap();
    }

    async fn job_state(&self, job_id: Uuid) -> String {
        let (status, body) = self
            .owner(Method::GET, &format!("/jobs/{job_id}"), None)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["state"].as_str().unwrap().to_owned()
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
}

#[tokio::test]
async fn ai_job_runs_a_tool_round_through_the_plugin_and_meters_every_call() {
    let _serial = DB_TEST_LOCK.lock().await;
    let fixture = AiFixture::new().await;
    let cursor = fixture.sync(json!([]), None).await["cursor"]
        .as_i64()
        .unwrap();

    let first = message(
        json!([
            {"type": "thinking", "thinking": "Checking who is online.", "signature": "sig-1"},
            {"type": "tool_use", "id": "toolu_players", "name": "list_players", "input": {}},
            {"type": "tool_use", "id": "toolu_op", "name": "run_server_command", "input": {"command": "op Steve"}},
        ]),
        "tool_use",
        usage(12, 80, 3000, 0),
    );
    fixture.fake.respond(first.clone());
    fixture.fake.respond(answer(
        "Steve and Alex are online.",
        usage(40, 25, 120, 3000),
    ));

    let baseline: i64 =
        sqlx::query_scalar("SELECT COALESCE(max(id), 0) FROM server_events WHERE server_id = $1")
            .bind(fixture.server_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    let job_id = Uuid::new_v4();
    let (status, job) = fixture.ask(job_id, "  who is online?  ").await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    // The panel's event stream carries the whole overview on every change.
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/servers/{}/events", fixture.server_id))
                .header(
                    header::COOKIE,
                    format!("kurbis_session={}", fixture.owner_token),
                )
                .header("last-event-id", baseline.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let frame = tokio::time::timeout(
        Duration::from_secs(2),
        response.into_body().into_data_stream().next(),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    let frame = String::from_utf8(frame.to_vec()).unwrap();
    assert!(frame.contains("\"type\":\"ai\""), "{frame}");
    assert!(frame.contains("\"prompt\":\"who is online?\""), "{frame}");
    assert_eq!(job["state"], "queued");
    assert_eq!(job["prompt"], "who is online?");
    assert_eq!(job["model"], OPUS);
    let (status, retried) = fixture.ask(job_id, "who is online?").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a retried request returns the same job"
    );
    assert_eq!(retried["id"], job["id"]);

    fixture.run().await;
    assert_eq!(fixture.job_state(job_id).await, "awaiting_tools");

    // The refused command never reaches the plugin; it is answered at once.
    let delivery = fixture.sync(json!([]), Some(cursor)).await;
    let calls = delivery["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1, "{delivery}");
    assert_eq!(calls[0]["call_id"], "toolu_players");
    assert_eq!(calls[0]["name"], "list_players");
    assert_eq!(calls[0]["operator_id"], json!(fixture.operator_id));
    let events = delivery["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["kind"], "progress");
    assert_eq!(events[0]["message"], "Checking who is online.");
    assert_eq!(delivery["active_jobs"][0]["state"], "awaiting_tools");
    let cursor = delivery["cursor"].as_i64().unwrap();

    // A restarted plugin replays the lines of requests still running, while
    // calls are handed out only once.
    let restarted = fixture.sync(json!([]), None).await;
    assert_eq!(restarted["events"], delivery["events"]);
    assert_eq!(restarted["cursor"], cursor);
    assert!(restarted["calls"].as_array().unwrap().is_empty());

    let returned = fixture
        .sync(
            json!([{"job_id": job_id, "call_id": "toolu_players", "output": "Steve, Alex"}]),
            Some(cursor),
        )
        .await;
    assert!(returned["calls"].as_array().unwrap().is_empty());
    assert_eq!(returned["active_jobs"][0]["state"], "queued");

    fixture.run().await;
    let finished = fixture.sync(json!([]), Some(cursor)).await;
    assert_eq!(finished["events"][0]["kind"], "answer");
    assert_eq!(
        finished["events"][0]["message"],
        "Steve and Alex are online."
    );
    assert!(finished["active_jobs"].as_array().unwrap().is_empty());
    let restarted = fixture.sync(json!([]), None).await;
    assert!(restarted["events"].as_array().unwrap().is_empty());
    assert_eq!(restarted["cursor"], finished["cursor"]);

    // The second call replays the transcript verbatim: the assistant turn is
    // byte-identical to the first response's content, followed by both tool
    // results in call order.
    let requests = fixture.fake.requests();
    assert_eq!(requests.len(), 2);
    let (headers, second) = &requests[1];
    assert_eq!(headers["x-api-key"], "test-anthropic-key");
    assert_eq!(headers["anthropic-version"], "2023-06-01");
    assert_eq!(
        headers["anthropic-beta"],
        "thinking-binding-controls-2026-08-01,thinking-display-updates-2026-08-18,server-side-fallback-2026-07-01"
    );
    assert_eq!(second["model"], OPUS);
    assert_eq!(second["thinking"]["type"], "adaptive");
    assert_eq!(second["output_config"]["effort"], "medium");
    assert_eq!(second["fallbacks"], "default");
    assert_eq!(second["system"], requests[0].1["system"]);
    assert_eq!(second["tools"], requests[0].1["tools"]);
    let messages = second["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["role"], "user");
    assert!(
        messages[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .ends_with("asks:\nwho is online?")
    );
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"], first["content"]);
    assert_eq!(messages[2]["content"][0]["tool_use_id"], "toolu_players");
    assert_eq!(messages[2]["content"][0]["content"], "Steve, Alex");
    assert_eq!(messages[2]["content"][1]["tool_use_id"], "toolu_op");
    assert_eq!(messages[2]["content"][1]["is_error"], true);

    let first_cost = catalog::cost_micros(
        OPUS,
        Tokens {
            input: 12,
            output: 80,
            cache_creation: 3000,
            cache_read: 0,
        },
    );
    let second_cost = catalog::cost_micros(
        OPUS,
        Tokens {
            input: 40,
            output: 25,
            cache_creation: 120,
            cache_read: 3000,
        },
    );
    let ledger: Vec<(Uuid, Option<Uuid>, String, i64, i64)> = sqlx::query_as(
        "SELECT owner_id, job_id, model, cache_read_input_tokens, cost_micros
         FROM ai_usage WHERE server_id = $1 ORDER BY id",
    )
    .bind(fixture.server_id)
    .fetch_all(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(
        ledger,
        vec![
            (
                fixture.owner_id,
                Some(job_id),
                OPUS.to_owned(),
                0,
                first_cost
            ),
            (
                fixture.owner_id,
                Some(job_id),
                OPUS.to_owned(),
                3000,
                second_cost
            ),
        ]
    );

    let (status, detail) = fixture
        .owner(Method::GET, &format!("/jobs/{job_id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["state"], "succeeded");
    assert_eq!(detail["answer"], "Steve and Alex are online.");
    assert_eq!(detail["model_calls"], 2);
    assert_eq!(detail["tool_calls"].as_array().unwrap().len(), 2);
    assert_eq!(detail["tool_calls"][1]["state"], "completed");
    assert_eq!(detail["tool_calls"][1]["is_error"], true);
    assert_eq!(detail["cost_micros"], first_cost + second_cost);

    let (status, overview) = fixture.owner(Method::GET, "", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(overview["available"], true);
    assert_eq!(
        overview["usage"]["server"]["spent_micros"],
        first_cost + second_cost
    );
    assert_eq!(overview["usage"]["server"]["reserved_micros"], 0);
    assert_eq!(overview["usage"]["server"]["budget_micros"], 5_000_000);
    assert_eq!(overview["usage"]["account"]["budget_micros"], 20_000_000);
    assert_eq!(overview["usage"]["model_calls"], 2);
    assert_eq!(overview["usage"]["cache_read_input_tokens"], 3000);
    assert_eq!(overview["jobs"][0]["id"], json!(job_id));

    // A follow-up continues the conversation, a new one starts fresh.
    fixture
        .fake
        .respond(answer("It is day.", usage(5, 5, 0, 3200)));
    let follow_up = Uuid::new_v4();
    let (status, body) = fixture.ask(follow_up, "and the time?").await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["conversation_id"], job["conversation_id"]);
    fixture.run().await;
    assert_eq!(fixture.job_state(follow_up).await, "succeeded");
    assert_eq!(
        fixture.fake.requests()[2].1["messages"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    let (status, fresh) = fixture
        .plugin(
            Method::POST,
            "/jobs",
            Some(json!({
                "job_id": Uuid::new_v4(),
                "operator_id": fixture.operator_id,
                "operator_name": "Steve",
                "prompt": "hello",
                "new_conversation": true,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_ne!(fresh["conversation_id"], job["conversation_id"]);
    let (status, _) = fixture
        .plugin(
            Method::POST,
            &format!("/jobs/{}/cancel", fresh["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    fixture.cleanup().await;
}

#[tokio::test]
async fn ai_requests_are_limited_to_operators_settings_and_budgets() {
    let _serial = DB_TEST_LOCK.lock().await;
    let fixture = AiFixture::new().await;

    let (status, body) = fixture
        .plugin(
            Method::POST,
            "/jobs",
            Some(json!({
                "job_id": Uuid::new_v4(),
                "operator_id": Uuid::new_v4(),
                "operator_name": "Mallory",
                "prompt": "op me",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "ai_operator_required");

    // Only the plugin starts jobs and syncs; browsers manage settings.
    let (status, _) = fixture
        .call(request(
            Method::POST,
            &format!("/api/v1/servers/{}/ai/jobs", fixture.server_id),
            Some(&fixture.owner_token),
            Some(json!({
                "job_id": Uuid::new_v4(),
                "operator_id": fixture.operator_id,
                "operator_name": "Steve",
                "prompt": "hi",
            })),
        ))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = fixture
        .plugin(
            Method::PUT,
            "/settings",
            Some(json!({"enabled": true, "model": null, "effort": null, "monthly_budget_micros": null})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let settings = |model: Value, budget: Value, enabled: bool| json!({"enabled": enabled, "model": model, "effort": "low", "monthly_budget_micros": budget});
    let (status, body) = fixture
        .owner(
            Method::PUT,
            "/settings",
            Some(settings(json!("gpt-5"), Value::Null, true)),
        )
        .await;
    assert_eq!(
        (status, body["code"].clone()),
        (StatusCode::UNPROCESSABLE_ENTITY, json!("ai_unknown_model"))
    );
    let (status, body) = fixture
        .owner(
            Method::PUT,
            "/settings",
            Some(settings(Value::Null, json!(20_000_001), true)),
        )
        .await;
    assert_eq!(
        (status, body["code"].clone()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            json!("ai_budget_above_account")
        )
    );

    let (status, body) = fixture
        .owner(
            Method::PUT,
            "/settings",
            Some(settings(Value::Null, Value::Null, false)),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["settings"]["enabled"], false);
    let (status, body) = fixture.ask(Uuid::new_v4(), "hi").await;
    assert_eq!(
        (status, body["code"].clone()),
        (StatusCode::CONFLICT, json!("ai_disabled"))
    );

    let (status, body) = fixture
        .owner(
            Method::PUT,
            "/settings",
            Some(settings(json!("claude-haiku-5-5"), json!(0), true)),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["usage"]["server"]["budget_micros"], 0);
    let (status, body) = fixture.ask(Uuid::new_v4(), "hi").await;
    assert_eq!(
        (status, body["code"].clone()),
        (
            StatusCode::TOO_MANY_REQUESTS,
            json!("ai_server_budget_exhausted")
        )
    );

    // A budget too small for one call's worst case lets the job start but
    // stops it before the model is called.
    fixture
        .owner(
            Method::PUT,
            "/settings",
            Some(settings(json!("claude-opus-5-5"), json!(1_000), true)),
        )
        .await;
    let starved = Uuid::new_v4();
    let (status, body) = fixture.ask(starved, "hi").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = fixture.ask(Uuid::new_v4(), "again").await;
    assert_eq!(
        (status, body["code"].clone()),
        (StatusCode::CONFLICT, json!("ai_job_active"))
    );
    let (status, body) = fixture.ask(starved, "a different prompt").await;
    assert_eq!(
        (status, body["code"].clone()),
        (StatusCode::CONFLICT, json!("ai_job_id_reused"))
    );
    fixture.run().await;
    let (_, detail) = fixture
        .owner(Method::GET, &format!("/jobs/{starved}"), None)
        .await;
    assert_eq!(detail["state"], "failed");
    assert!(
        detail["error"].as_str().unwrap().contains("budget"),
        "{detail}"
    );
    assert!(fixture.fake.requests().is_empty());

    // Spending from other servers of the same owner counts against the
    // account budget.
    sqlx::query(
        "INSERT INTO ai_usage(owner_id, server_id, job_id, model, input_tokens, output_tokens,
                              cache_creation_input_tokens, cache_read_input_tokens, cost_micros)
         VALUES($1, NULL, NULL, $2, 0, 0, 0, 0, 20000000)",
    )
    .bind(fixture.owner_id)
    .bind(OPUS)
    .execute(&fixture.pool)
    .await
    .unwrap();
    fixture
        .owner(
            Method::PUT,
            "/settings",
            Some(settings(Value::Null, Value::Null, true)),
        )
        .await;
    let (status, body) = fixture.ask(Uuid::new_v4(), "hi").await;
    assert_eq!(
        (status, body["code"].clone()),
        (
            StatusCode::TOO_MANY_REQUESTS,
            json!("ai_account_budget_exhausted")
        )
    );

    fixture.cleanup().await;
}

#[tokio::test]
async fn cancelled_and_undelivered_tool_rounds_end_cleanly() {
    let _serial = DB_TEST_LOCK.lock().await;
    let fixture = AiFixture::new().await;
    let cursor = fixture.sync(json!([]), None).await["cursor"]
        .as_i64()
        .unwrap();
    let tool_round = || {
        message(
            json!([{"type": "tool_use", "id": format!("toolu_{}", Uuid::new_v4().simple()), "name": "list_worlds", "input": {}}]),
            "tool_use",
            usage(10, 10, 0, 0),
        )
    };

    // Cancelling while the plugin runs a tool closes the round; the late
    // result is ignored.
    fixture.fake.respond(tool_round());
    let cancelled = Uuid::new_v4();
    fixture.ask(cancelled, "list worlds").await;
    fixture.run().await;
    let delivery = fixture.sync(json!([]), Some(cursor)).await;
    let call_id = delivery["calls"][0]["call_id"].clone();
    let (status, body) = fixture
        .plugin(Method::POST, &format!("/jobs/{cancelled}/cancel"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "cancelled");
    let late = fixture
        .sync(
            json!([{"job_id": cancelled, "call_id": call_id, "output": "overworld"}]),
            Some(delivery["cursor"].as_i64().unwrap()),
        )
        .await;
    assert_eq!(late["events"][0]["kind"], "error");
    assert_eq!(late["events"][0]["message"], "Request cancelled.");
    assert_eq!(fixture.job_state(cancelled).await, "cancelled");
    let (status, _) = fixture
        .plugin(Method::POST, &format!("/jobs/{cancelled}/cancel"), None)
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "cancelling a finished job is a no-op"
    );
    let (status, body) = fixture
        .plugin(
            Method::POST,
            &format!("/jobs/{}/cancel", Uuid::new_v4()),
            None,
        )
        .await;
    assert_eq!(
        (status, body["code"].clone()),
        (StatusCode::NOT_FOUND, json!("ai_job_not_found"))
    );

    // A fresh gateway sweeps on its first cycle; backdating a job's calls
    // makes them overdue.
    let sweep_overdue_calls = |job_id: Uuid| {
        let pool = fixture.pool.clone();
        let config = ai_config(&fixture.base_url);
        async move {
            sqlx::query(
                "UPDATE ai_tool_calls SET created_at = now() - interval '1 minute'
                 WHERE job_id = $1",
            )
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
            AiGateway::new(pool, config)
                .unwrap()
                .run_jobs()
                .await
                .unwrap();
        }
    };
    let last_tool_result = |request: usize| {
        let requests = fixture.fake.requests();
        let messages = requests[request].1["messages"].as_array().unwrap();
        messages.last().unwrap()["content"][0]["content"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    // A cancelled conversation is closed, so the next request starts fresh.
    // A call the plugin never picked up is answered with an error saying it
    // did not run, and the model continues.
    fixture.fake.respond(tool_round());
    fixture
        .fake
        .respond(answer("I could not read the worlds.", usage(10, 10, 0, 0)));
    let stalled = Uuid::new_v4();
    let (_, job) = fixture.ask(stalled, "list worlds").await;
    let cancelled_conversation: Uuid =
        sqlx::query_scalar("SELECT conversation_id FROM ai_jobs WHERE id = $1")
            .bind(cancelled)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_ne!(job["conversation_id"], json!(cancelled_conversation));
    fixture.run().await;
    sweep_overdue_calls(stalled).await;
    let (_, detail) = fixture
        .owner(Method::GET, &format!("/jobs/{stalled}"), None)
        .await;
    assert_eq!(detail["state"], "succeeded", "{detail}");
    assert_eq!(detail["tool_calls"][0]["is_error"], true);
    assert!(last_tool_result(2).contains("so it did not run"));

    // A call the plugin took but never answered may have run.
    fixture.fake.respond(tool_round());
    fixture
        .fake
        .respond(answer("The worlds did not answer.", usage(10, 10, 0, 0)));
    let unanswered = Uuid::new_v4();
    fixture.ask(unanswered, "list worlds again").await;
    fixture.run().await;
    let delivery = fixture.sync(json!([]), Some(cursor)).await;
    assert_eq!(delivery["calls"][0]["job_id"], json!(unanswered));
    sweep_overdue_calls(unanswered).await;
    assert_eq!(fixture.job_state(unanswered).await, "succeeded");
    assert!(last_tool_result(4).contains("may or may not have run"));

    fixture.cleanup().await;
}

#[tokio::test]
async fn ai_routes_report_an_unconfigured_gateway() {
    let _serial = DB_TEST_LOCK.lock().await;
    let fixture = AiFixture::new().await;
    let app = router(state(fixture.pool.clone()));
    let uri = format!("/api/v1/servers/{}/ai", fixture.server_id);
    let response = app
        .clone()
        .oneshot(request(Method::GET, &uri, Some(&fixture.owner_token), None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let overview = json_body(response).await;
    assert_eq!(overview["available"], false);
    assert!(overview["models"].as_array().unwrap().is_empty());
    assert!(overview.get("defaults").is_none_or(Value::is_null));

    let response = app
        .clone()
        .oneshot(bearer_request(
            Method::POST,
            &format!("{uri}/jobs"),
            &fixture.capability,
            Some(json!({
                "job_id": Uuid::new_v4(),
                "operator_id": fixture.operator_id,
                "operator_name": "Steve",
                "prompt": "hi",
            })),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json_body(response).await["code"], "ai_unavailable");

    fixture.cleanup().await;
}

use super::support::*;

#[tokio::test]
async fn sse_replays_durable_events_after_last_event_id() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_id, token) = user_session(&pool, "active").await;
    let server_id = Uuid::new_v4();
    sqlx::query("INSERT INTO servers(id, owner_id, subdomain) VALUES($1,$2,$3)")
        .bind(server_id)
        .bind(owner_id)
        .bind(format!("sse-{}", &server_id.simple().to_string()[..12]))
        .execute(&pool)
        .await
        .unwrap();
    let baseline: i64 =
        sqlx::query_scalar("SELECT max(id) FROM server_events WHERE server_id = $1")
            .bind(server_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let event_id: i64 = sqlx::query_scalar(
        "INSERT INTO server_events(server_id, event_type) VALUES($1,'players') RETURNING id",
    )
    .bind(server_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    let app = router(state(pool.clone()));
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/api/v1/servers/{server_id}/events"))
                .header(header::COOKIE, format!("kurbis_session={token}"))
                .header("last-event-id", baseline.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/event-stream"
    );
    let mut stream = response.into_body().into_data_stream();
    let frame = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("SSE catch-up frame should arrive")
        .expect("SSE stream should remain open")
        .expect("SSE frame should be readable");
    let text = String::from_utf8(frame.to_vec()).unwrap();
    assert!(text.contains(&format!("id:{event_id}")) || text.contains(&format!("id: {event_id}")));
    assert!(text.contains("event:players") || text.contains("event: players"));
    assert!(text.contains("\"type\":\"players\""));

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(owner_id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn sse_streams_server_changes_committed_after_the_connection_opens() {
    let _serial = DB_TEST_LOCK.lock().await;
    let pool = test_pool().await;
    let (owner_id, token) = user_session(&pool, "active").await;
    let server_id = create_test_server(&pool, owner_id, "sse-live").await;
    let cursor: i64 = sqlx::query_scalar("SELECT max(id) FROM server_events WHERE server_id = $1")
        .bind(server_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let app = router(state(pool.clone()));
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/api/v1/servers/{server_id}/events"))
                .header(header::COOKIE, format!("kurbis_session={token}"))
                .header("last-event-id", cursor.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    sqlx::query("UPDATE servers SET last_error = 'live SSE update' WHERE id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();

    let frame = tokio::time::timeout(
        Duration::from_secs(2),
        response.into_body().into_data_stream().next(),
    )
    .await
    .expect("live server change should wake the stream")
    .expect("SSE stream should remain open")
    .expect("SSE frame should be readable");
    let frame = String::from_utf8(frame.to_vec()).unwrap();
    assert!(frame.contains("event: server") || frame.contains("event:server"));
    assert!(frame.contains("live SSE update"));
}

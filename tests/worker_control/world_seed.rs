use super::*;

async fn world(pool: &PgPool, server_id: Uuid) -> Value {
    sqlx::query_scalar("SELECT world FROM servers WHERE id = $1")
        .bind(server_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn report(
    service: &Router,
    server_id: Uuid,
    token: &str,
    generation: i64,
    seed: i64,
) -> StatusCode {
    send_json(
        service.clone(),
        Method::POST,
        &format!("/internal/worker/control/servers/{server_id}/world-seed"),
        Some(token),
        json!({"generation": generation, "seed": seed}),
    )
    .await
    .0
}

/// A world made before seeds were recorded gets its seed from the worker
/// that runs it, once: a recorded seed is never replaced, and only the
/// server's current placement may report one.
#[tokio::test]
async fn workers_record_the_seed_of_a_world_made_before_seeds_were_recorded() {
    let pool = test_pool().await;
    let worker = create_worker(&pool, "world-seed").await;
    let foreign = create_worker(&pool, "world-seed-foreign").await;
    let server_id = create_server(&pool, "world-seed").await;
    set_server_state(&pool, server_id, 2, "running", "online").await;
    add_placement(&pool, server_id, worker.id, 2, true).await;
    let service = app(pool.clone(), Arc::new(InMemory::new()));

    assert_eq!(world(&pool, server_id).await["seed"], Value::Null);
    assert_ne!(
        report(&service, server_id, &foreign.token, 2, 1).await,
        StatusCode::NO_CONTENT
    );
    assert_ne!(
        report(&service, server_id, &worker.token, 1, 1).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(world(&pool, server_id).await["seed"], Value::Null);

    assert_eq!(
        report(&service, server_id, &worker.token, 2, -1_120_102_460).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        world(&pool, server_id).await,
        json!({"world_type": "normal", "structures": true, "seed": -1_120_102_460_i64})
    );
    assert_eq!(
        report(&service, server_id, &worker.token, 2, 99).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(world(&pool, server_id).await["seed"], -1_120_102_460_i64);

    sqlx::query("DELETE FROM placements WHERE server_id = $1")
        .bind(server_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workers WHERE id = ANY($1::uuid[])")
        .bind(vec![worker.id, foreign.id])
        .execute(&pool)
        .await
        .unwrap();
}

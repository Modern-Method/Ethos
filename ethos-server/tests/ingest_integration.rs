use ethos_core::ipc::EthosRequest;
use sqlx::PgPool;
use serde_json::json;
use ethos_server::router;

#[tokio::test]
async fn test_ingest_session_events() {
    let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
    let pool = PgPool::connect(database_url).await.expect("Failed to connect to Postgres");

    // Clean up before test
    sqlx::query!("DELETE FROM session_events WHERE session_id = 'test-session'")
        .execute(&pool)
        .await
        .unwrap();

    let payload = json!({
        "content": "hello world",
        "source": "user",
        "metadata": {
            "session_id": "test-session",
            "agent_id": "forge-test"
        }
    });

    let request = EthosRequest::Ingest { payload };
    let response = router::handle_request(request, &pool).await;

    assert_eq!(response.status, "ok");

    // Verify DB write
    let row = sqlx::query!(
        "SELECT content, role, session_id, agent_id FROM session_events WHERE session_id = 'test-session'"
    )
    .fetch_one(&pool)
    .await
    .expect("Row not found in session_events");

    assert_eq!(row.content, "hello world");
    assert_eq!(row.role, "user");
    assert_eq!(row.session_id, "test-session");
    assert_eq!(row.agent_id, "forge-test");
}

#[tokio::test]
async fn test_ingest_memory_vectors() {
    let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
    let pool = PgPool::connect(database_url).await.expect("Failed to connect to Postgres");

    // Clean up
    sqlx::query!("DELETE FROM memory_vectors WHERE source = 'test-source'")
        .execute(&pool)
        .await
        .unwrap();

    let payload = json!({
        "content": "memory context",
        "source": "user",
        "metadata": {
            "session_id": "test-session-2",
            "author": "test-source"
        }
    });

    let request = EthosRequest::Ingest { payload };
    let _ = router::handle_request(request, &pool).await;

    // Verify DB write
    // Use query_as! or a manual query to avoid vector mapping issues for now
    let row: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT content, source FROM memory_vectors WHERE content = 'memory context'"
    )
    .fetch_one(&pool)
    .await
    .expect("Row not found in memory_vectors");

    assert_eq!(row.0.as_deref(), Some("memory context"));
    assert_eq!(row.1.as_deref(), Some("test-source"));
}

#[tokio::test]
async fn test_assistant_role_mapping() {
    let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
    let pool = PgPool::connect(database_url).await.expect("Failed to connect to Postgres");

    let payload = json!({
        "content": "bot response",
        "source": "assistant",
        "metadata": {
            "session_id": "test-role-map"
        }
    });

    let request = EthosRequest::Ingest { payload };
    let _ = router::handle_request(request, &pool).await;

    let row = sqlx::query!(
        "SELECT role FROM session_events WHERE session_id = 'test-role-map'"
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(row.role, "assistant");
}

#[tokio::test]
async fn test_malformed_payload() {
    let database_url = "postgresql://ethos:ethos_dev@localhost:5432/ethos";
    let pool = PgPool::connect(database_url).await.expect("Failed to connect to Postgres");

    let payload = json!({});

    let request = EthosRequest::Ingest { payload };
    let response = router::handle_request(request, &pool).await;

    assert_eq!(response.status, "error");
}

use sqlx::PgPool;
use serde_json::Value;
use uuid::Uuid;
use crate::subsystems::embedder;

pub async fn ingest_payload(payload: Value, pool: &PgPool) -> anyhow::Result<()> {
    ingest_payload_with_embedding(payload, pool, None).await?;
    Ok(())
}

pub async fn ingest_payload_with_embedding(
    payload: Value,
    pool: &PgPool,
    config: Option<&ethos_core::EthosConfig>,
) -> anyhow::Result<Uuid> {
    // Extract data from payload
    let content = payload["content"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing 'content'"))?;

    let source = payload["source"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing 'source'"))?;

    let metadata = payload["metadata"].as_object();
    
    let session_id = metadata
        .and_then(|m| m.get("session_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    let agent_id = metadata
        .and_then(|m| m.get("agent_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("ethos");

    let author = metadata
        .and_then(|m| m.get("author"))
        .and_then(|v| v.as_str())
        .unwrap_or(source);

    // Mapping source to role
    let role = match source {
        "user" => "user",
        "assistant" => "assistant",
        "system" => "system",
        "tool" => "tool",
        _ => "user",
    };

    // Atomic transaction
    let mut tx = pool.begin().await?;

    // 1. Insert into session_events
    sqlx::query!(
        r#"
        INSERT INTO session_events (session_id, agent_id, role, content, metadata)
        VALUES ($1, $2, $3, $4, $5)
        "#,
        session_id,
        agent_id,
        role,
        content,
        serde_json::to_value(metadata).unwrap_or(serde_json::json!({}))
    )
    .execute(&mut *tx)
    .await?;

    // 2. Insert into memory_vectors and return the ID
    let row = sqlx::query!(
        r#"
        INSERT INTO memory_vectors (content, source, metadata)
        VALUES ($1, $2, $3)
        RETURNING id
        "#,
        content,
        author,
        serde_json::to_value(metadata).unwrap_or(serde_json::json!({}))
    )
    .fetch_one(&mut *tx)
    .await?;

    let memory_id = row.id;

    tx.commit().await?;

    tracing::info!("Successfully ingested payload into DB, memory_id: {}", memory_id);

    // 3. Spawn embedding task in background (non-blocking)
    if let Some(cfg) = config {
        let embedder_config = embedder::EmbedderConfig::from(cfg);
        embedder::spawn_embed_task(memory_id, pool.clone(), embedder_config);
    }

    Ok(memory_id)
}

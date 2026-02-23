use crate::subsystems::{consolidate, embedder, ingest, retrieve};
use ethos_core::ipc::{EthosRequest, EthosResponse};
use sqlx::PgPool;

pub async fn handle_request(request: EthosRequest, pool: &PgPool) -> EthosResponse {
    handle_request_with_config(request, pool, None).await
}

/// Handle request with optional config for embedding
pub async fn handle_request_with_config(
    request: EthosRequest,
    pool: &PgPool,
    config: Option<ethos_core::EthosConfig>,
) -> EthosResponse {
    match request {
        EthosRequest::Ping => EthosResponse::pong(),
        EthosRequest::Health => {
            let pg_ver = match ethos_core::db::health_check(pool).await {
                Ok(v) => v,
                Err(e) => return EthosResponse::err(format!("DB Health Check failed: {}", e)),
            };
            let vec_ver = match ethos_core::db::check_pgvector(pool).await {
                Ok(v) => v,
                Err(e) => return EthosResponse::err(format!("pgvector Check failed: {}", e)),
            };
            EthosResponse::ok(serde_json::json!({
                "postgresql": pg_ver,
                "pgvector": vec_ver,
                "status": "healthy"
            }))
        }
        EthosRequest::Ingest { payload } => {
            match ingest::ingest_payload_with_embedding(payload, pool, config.as_ref()).await {
                Ok(id) => EthosResponse::ok(serde_json::json!({
                    "queued": true,
                    "id": id
                })),
                Err(e) => EthosResponse::err(e.to_string()),
            }
        }
        EthosRequest::Search { query, limit, use_spreading } => {
            match handle_search_request(query, limit, use_spreading, pool, config.as_ref()).await {
                Ok(data) => EthosResponse::ok(data),
                Err(e) => EthosResponse::err(e.to_string()),
            }
        }
        EthosRequest::Consolidate { session, reason } => {
            // Get config for consolidation
            let (consolidation_config, conflict_config, decay_config) = match config {
                Some(c) => (
                    c.consolidation.clone(),
                    c.conflict_resolution.clone(),
                    c.decay.clone(),
                ),
                None => {
                    return EthosResponse::err("No config available for consolidation");
                }
            };
            match consolidate::trigger_consolidation(
                pool.clone(),
                consolidation_config,
                conflict_config,
                decay_config,
                session,
                reason,
            )
            .await
            {
                Ok(report) => EthosResponse::ok(serde_json::json!({
                    "triggered": true,
                    "episodes_scanned": report.episodes_scanned,
                    "episodes_promoted": report.episodes_promoted,
                    "facts_created": report.facts_created,
                    "facts_updated": report.facts_updated,
                    "facts_superseded": report.facts_superseded,
                    "facts_flagged": report.facts_flagged,
                })),
                Err(e) => EthosResponse::err(e.to_string()),
            }
        }
        EthosRequest::Embed { id } => {
            // Manual embed trigger
            match handle_embed_request(id, pool, config.as_ref()).await {
                Ok(_) => EthosResponse::ok(serde_json::json!({"embedded": true, "id": id})),
                Err(e) => EthosResponse::err(e.to_string()),
            }
        }
        _ => EthosResponse::ok(serde_json::json!({"stub": true})),
    }
}

/// Handle manual Embed request
async fn handle_embed_request(
    id: uuid::Uuid,
    pool: &PgPool,
    config: Option<&ethos_core::EthosConfig>,
) -> anyhow::Result<()> {
    let config = match config {
        Some(c) => c,
        None => {
            return Err(anyhow::anyhow!("No config available for embedding"));
        }
    };

    let embedder_config = embedder::EmbedderConfig::from(config);
    let client = embedder::create_client(&embedder_config)?;
    
    embedder::embed_by_id(id, pool, &client).await?;
    
    Ok(())
}

/// Handle Search request with semantic retrieval
async fn handle_search_request(
    query: String,
    limit: Option<u32>,
    use_spreading: bool,
    pool: &PgPool,
    config: Option<&ethos_core::EthosConfig>,
) -> anyhow::Result<serde_json::Value> {
    let config = match config {
        Some(c) => c,
        None => {
            return Ok(serde_json::json!({
                "status": "error",
                "error": "No config available for embedding"
            }));
        }
    };

    let embedder_config = embedder::EmbedderConfig::from(config);
    let client = embedder::create_client(&embedder_config)?;
    
    let result = retrieve::search_memory(
        query,
        limit,
        use_spreading,
        pool,
        &client,
        &config.retrieval,
    ).await?;
    Ok(result)
}

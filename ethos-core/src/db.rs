use crate::config::DatabaseConfig;
use sqlx::{postgres::PgPoolOptions, PgPool};

pub async fn create_pool(config: &DatabaseConfig) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(config.max_connections)
        .connect(&config.url)
        .await
}

pub async fn health_check(pool: &PgPool) -> Result<String, sqlx::Error> {
    let row: (String,) = sqlx::query_as("SELECT version()").fetch_one(pool).await?;
    Ok(row.0)
}

pub async fn check_pgvector(pool: &PgPool) -> Result<String, sqlx::Error> {
    let row: (String,) =
        sqlx::query_as("SELECT extversion FROM pg_extension WHERE extname = 'vector'")
            .fetch_one(pool)
            .await?;
    Ok(row.0)
}

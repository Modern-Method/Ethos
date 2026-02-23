use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct EpisodicTrace {
    pub id: Uuid,
    pub session_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub summary: String,
    pub content: serde_json::Value,
    pub salience: serde_json::Value,
}

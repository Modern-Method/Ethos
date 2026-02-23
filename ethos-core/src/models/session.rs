use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Session {
    pub id: Uuid,
    pub session_key: String,
    pub agent_id: String,
    pub channel: Option<String>,
    pub started_at: DateTime<Utc>,
    pub last_active_at: DateTime<Utc>,
    pub message_count: i32,
    pub metadata: serde_json::Value,
}

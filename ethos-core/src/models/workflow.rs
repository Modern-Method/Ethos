use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct WorkflowMemory {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub name: String,
    pub description: Option<String>,
    pub content: serde_json::Value,
    pub metadata: serde_json::Value,
}

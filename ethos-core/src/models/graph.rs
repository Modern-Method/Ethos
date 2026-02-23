use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct MemoryGraphLink {
    pub id: Uuid,
    pub source_id: Uuid,
    pub target_id: Uuid,
    pub link_type: String,
    pub weight: f32,
    pub metadata: serde_json::Value,
}

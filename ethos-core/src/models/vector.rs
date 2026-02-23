use pgvector::Vector;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct MemoryVector {
    pub id: Uuid,
    pub source_type: String,
    pub source_id: Uuid,
    pub embedding: Vector,
    pub model_name: String,
}

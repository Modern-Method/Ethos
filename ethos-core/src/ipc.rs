use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum EthosRequest {
    Ping,
    Health,
    Ingest {
        payload: serde_json::Value,
    },
    Search {
        query: String,
        limit: Option<u32>,
        #[serde(default)]
        use_spreading: bool,
    },
    Get {
        id: uuid::Uuid,
    },
    Consolidate {
        session: Option<String>,
        reason: Option<String>,
    },
    Embed {
        id: uuid::Uuid,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EthosResponse {
    pub status: String,
    pub data: Option<serde_json::Value>,
    pub error: Option<String>,
    pub version: String,
}

impl EthosResponse {
    pub fn ok(data: serde_json::Value) -> Self {
        Self {
            status: "ok".to_string(),
            data: Some(data),
            error: None,
            version: "0.1.0".to_string(),
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            status: "error".to_string(),
            data: None,
            error: Some(msg.into()),
            version: "0.1.0".to_string(),
        }
    }

    pub fn pong() -> Self {
        Self::ok(serde_json::json!({"pong": true}))
    }
}

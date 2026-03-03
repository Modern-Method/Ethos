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
        #[serde(default, alias = "resourceId")]
        resource_id: Option<String>,
        #[serde(default, alias = "threadId")]
        thread_id: Option<String>,
        #[serde(default, alias = "agentId")]
        agent_id: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::EthosRequest;

    #[test]
    fn test_search_request_deserializes_scope_filters_in_snake_and_camel_case() {
        let snake_case = serde_json::json!({
            "action": "search",
            "query": "find this",
            "resource_id": "resource-a",
            "thread_id": "thread-a",
            "agent_id": "agent-a"
        });

        let camel_case = serde_json::json!({
            "action": "search",
            "query": "find this",
            "resourceId": "resource-b",
            "threadId": "thread-b",
            "agentId": "agent-b"
        });

        let req_snake: EthosRequest =
            serde_json::from_value(snake_case).expect("snake_case request should deserialize");
        let req_camel: EthosRequest =
            serde_json::from_value(camel_case).expect("camelCase request should deserialize");

        match req_snake {
            EthosRequest::Search {
                resource_id,
                thread_id,
                agent_id,
                ..
            } => {
                assert_eq!(resource_id.as_deref(), Some("resource-a"));
                assert_eq!(thread_id.as_deref(), Some("thread-a"));
                assert_eq!(agent_id.as_deref(), Some("agent-a"));
            }
            other => panic!("unexpected request variant: {other:?}"),
        }

        match req_camel {
            EthosRequest::Search {
                resource_id,
                thread_id,
                agent_id,
                ..
            } => {
                assert_eq!(resource_id.as_deref(), Some("resource-b"));
                assert_eq!(thread_id.as_deref(), Some("thread-b"));
                assert_eq!(agent_id.as_deref(), Some("agent-b"));
            }
            other => panic!("unexpected request variant: {other:?}"),
        }
    }
}

//! Embeddings module for Ethos — Gemini API integration
//!
//! This module provides async embedding generation via the Gemini Embeddings API.
//! It handles retries with exponential backoff and returns 768-dimensional vectors
//! for storage in pgvector.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;
use tokio_retry::strategy::{jitter, ExponentialBackoff};
use tokio_retry::Retry;

/// Default Gemini embedding dimensions
pub const GEMINI_DIMENSIONS: usize = 768;

/// Task type for embedding API
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskType {
    #[default]
    RetrievalDocument,
    RetrievalQuery,
}

/// Gemini API request structure
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiRequest {
    model: String,
    content: GeminiContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_type: Option<TaskType>,
    /// Force output to specific dimensions (Gemini supports truncation)
    #[serde(skip_serializing_if = "Option::is_none")]
    output_dimensionality: Option<usize>,
}

#[derive(Debug, Serialize)]
struct GeminiContent {
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Serialize)]
struct GeminiPart {
    text: String,
}

/// Gemini API response structure
#[derive(Debug, Deserialize)]
struct GeminiResponse {
    embedding: GeminiEmbedding,
}

#[derive(Debug, Deserialize)]
struct GeminiEmbedding {
    values: Vec<f32>,
}

/// Gemini API error response
#[derive(Debug, Deserialize)]
struct GeminiErrorResponse {
    error: Option<GeminiErrorDetail>,
}

#[derive(Debug, Deserialize)]
struct GeminiErrorDetail {
    code: u16,
    message: String,
}

/// Embedding client configuration
#[derive(Debug, Clone)]
pub struct EmbeddingConfig {
    /// API key for Gemini
    pub api_key: String,
    /// Model name (e.g., "gemini-embedding-001")
    pub model: String,
    /// Embedding dimensions (768 for Gemini)
    pub dimensions: usize,
    /// Maximum retry attempts
    pub max_retries: usize,
    /// Initial retry delay in milliseconds
    pub retry_delay_ms: u64,
}

impl EmbeddingConfig {
    /// Create a new embedding config from environment and parameters
    pub fn new(api_key: Option<String>, model: String, dimensions: usize) -> Self {
        let api_key = api_key
            .or_else(|| std::env::var("GOOGLE_API_KEY").ok())
            .unwrap_or_default();
        
        Self {
            api_key,
            model,
            dimensions,
            max_retries: 3,
            retry_delay_ms: 1000,
        }
    }
}

/// Embedding generation errors
#[derive(Error, Debug)]
pub enum EmbeddingError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    
    #[error("API error ({code}): {message}")]
    Api { code: u16, message: String },
    
    #[error("Invalid response: expected {expected} dimensions, got {actual}")]
    InvalidDimensions { expected: usize, actual: usize },
    
    #[error("Missing embedding in response")]
    MissingEmbedding,
    
    #[error("Missing API key")]
    MissingApiKey,
    
    #[error("All {attempts} retry attempts failed")]
    RetryExhausted { attempts: usize },
}

/// Gemini embedding client
#[derive(Debug, Clone)]
pub struct GeminiEmbeddingClient {
    client: Client,
    config: EmbeddingConfig,
    base_url: String,
}

impl GeminiEmbeddingClient {
    /// Create a new Gemini embedding client
    pub fn new(config: EmbeddingConfig) -> Result<Self, EmbeddingError> {
        if config.api_key.is_empty() {
            return Err(EmbeddingError::MissingApiKey);
        }

        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;

        Ok(Self {
            client,
            config,
            base_url: "https://generativelanguage.googleapis.com/v1beta".to_string(),
        })
    }

    /// Create a client with a custom base URL (for testing / integration)
    pub fn with_base_url(config: EmbeddingConfig, base_url: String) -> Result<Self, EmbeddingError> {
        if config.api_key.is_empty() {
            return Err(EmbeddingError::MissingApiKey);
        }

        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;

        Ok(Self {
            client,
            config,
            base_url,
        })
    }

    /// Generate an embedding for the given text
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.embed_with_task(text, TaskType::RetrievalDocument).await
    }

    /// Generate an embedding with a specific task type
    pub async fn embed_with_task(
        &self,
        text: &str,
        task_type: TaskType,
    ) -> Result<Vec<f32>, EmbeddingError> {
        let retry_strategy = ExponentialBackoff::from_millis(self.config.retry_delay_ms)
            .max_delay(Duration::from_secs(10))
            .map(jitter)
            .take(self.config.max_retries);

        let result = Retry::spawn(retry_strategy, || {
            self.embed_once(text, task_type)
        })
        .await;

        match result {
            Ok(vec) => Ok(vec),
            Err(e) => {
                tracing::error!(
                    attempts = self.config.max_retries,
                    error = %e,
                    "All embedding retry attempts failed"
                );
                Err(EmbeddingError::RetryExhausted {
                    attempts: self.config.max_retries,
                })
            }
        }
    }

    /// Single embedding attempt (used internally by retry logic)
    async fn embed_once(
        &self,
        text: &str,
        task_type: TaskType,
    ) -> Result<Vec<f32>, EmbeddingError> {
        let url = format!(
            "{}/models/{}:embedContent?key={}",
            self.base_url, self.config.model, self.config.api_key
        );

        let request = GeminiRequest {
            model: format!("models/{}", self.config.model),
            content: GeminiContent {
                parts: vec![GeminiPart { text: text.to_string() }],
            },
            task_type: Some(task_type),
            output_dimensionality: Some(self.config.dimensions),
        };

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            let error_detail = serde_json::from_str::<GeminiErrorResponse>(&error_body)
                .ok()
                .and_then(|e| e.error);
            
            let (code, message) = error_detail
                .map(|e| (e.code, e.message))
                .unwrap_or((status.as_u16(), error_body));

            tracing::error!(
                code = code,
                message = %message,
                "Gemini API error"
            );
            
            return Err(EmbeddingError::Api { code, message });
        }

        let gemini_response: GeminiResponse = response.json().await?;

        let embedding = gemini_response.embedding;
        let values = embedding.values;

        if values.len() != self.config.dimensions {
            return Err(EmbeddingError::InvalidDimensions {
                expected: self.config.dimensions,
                actual: values.len(),
            });
        }

        Ok(values)
    }

    /// Get the configured dimensions
    pub fn dimensions(&self) -> usize {
        self.config.dimensions
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, body_json, header};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_config(api_key: &str) -> EmbeddingConfig {
        EmbeddingConfig {
            api_key: api_key.to_string(),
            model: "gemini-embedding-001".to_string(),
            dimensions: GEMINI_DIMENSIONS,
            max_retries: 3,
            retry_delay_ms: 100, // Faster for tests
        }
    }

    fn mock_embedding_response() -> serde_json::Value {
        // Generate 768 floats for the embedding
        let values: Vec<f32> = (0..768).map(|i| (i as f32) / 768.0).collect();
        serde_json::json!({
            "embedding": {
                "values": values
            }
        })
    }

    #[tokio::test]
    async fn test_embed_content_calls_api_and_returns_768_dim_vector() {
        // Arrange
        let mock_server = MockServer::start().await;
        let config = test_config("test-api-key");
        let client = GeminiEmbeddingClient::with_base_url(
            config,
            mock_server.uri(),
        ).expect("Failed to create client");

        Mock::given(method("POST"))
            .and(path("/models/gemini-embedding-001:embedContent"))
            .and(header("content-type", "application/json"))
            .and(body_json(serde_json::json!({
                "model": "models/gemini-embedding-001",
                "content": { "parts": [{ "text": "hello world" }] },
                "taskType": "RETRIEVAL_DOCUMENT",
                "outputDimensionality": 768
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(mock_embedding_response())
            )
            .mount(&mock_server)
            .await;

        // Act
        let result = client.embed("hello world").await;

        // Assert
        assert!(result.is_ok(), "Expected Ok, got Err: {:?}", result.err());
        let embedding = result.unwrap();
        assert_eq!(embedding.len(), 768, "Expected 768 dimensions");
    }

    #[tokio::test]
    async fn test_embed_returns_error_on_api_500() {
        let mock_server = MockServer::start().await;
        let config = test_config("test-api-key");
        let client = GeminiEmbeddingClient::with_base_url(
            config,
            mock_server.uri(),
        ).expect("Failed to create client");

        // All retries fail
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(500)
                    .set_body_json(serde_json::json!({
                        "error": { "code": 500, "message": "Internal server error" }
                    }))
            )
            .mount(&mock_server)
            .await;

        let result = client.embed("hello world").await;

        assert!(result.is_err(), "Expected error on 500 response");
        match result {
            Err(EmbeddingError::RetryExhausted { attempts }) => {
                assert_eq!(attempts, 3, "Expected 3 retry attempts");
            }
            _ => panic!("Expected RetryExhausted error"),
        }
    }

    #[tokio::test]
    async fn test_embed_retries_on_429_then_succeeds() {
        let mock_server = MockServer::start().await;
        let config = test_config("test-api-key");
        let client = GeminiEmbeddingClient::with_base_url(
            config,
            mock_server.uri(),
        ).expect("Failed to create client");

        // First call returns 429 (rate limit)
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(429)
                    .set_body_json(serde_json::json!({
                        "error": { "code": 429, "message": "Rate limit exceeded" }
                    }))
            )
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        // Second call succeeds
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(mock_embedding_response())
            )
            .mount(&mock_server)
            .await;

        let result = client.embed("hello world").await;

        assert!(result.is_ok(), "Expected success after retry");
        let embedding = result.unwrap();
        assert_eq!(embedding.len(), 768);
    }

    #[tokio::test]
    async fn test_embed_fails_with_missing_api_key() {
        let config = test_config("");
        let result = GeminiEmbeddingClient::new(config);
        
        assert!(result.is_err(), "Expected error with missing API key");
        match result {
            Err(EmbeddingError::MissingApiKey) => {}
            _ => panic!("Expected MissingApiKey error"),
        }
    }

    #[tokio::test]
    async fn test_embed_returns_error_on_wrong_dimensions() {
        let mock_server = MockServer::start().await;
        let config = test_config("test-api-key");
        let client = GeminiEmbeddingClient::with_base_url(
            config,
            mock_server.uri(),
        ).expect("Failed to create client");

        // Return wrong number of dimensions
        let wrong_response = serde_json::json!({
            "embedding": {
                "values": [0.1, 0.2, 0.3]  // Only 3 dimensions
            }
        });

        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(wrong_response)
            )
            .mount(&mock_server)
            .await;

        let result = client.embed("hello world").await;

        assert!(result.is_err(), "Expected error on wrong dimensions");
        match result {
            Err(EmbeddingError::InvalidDimensions { expected, actual }) => {
                assert_eq!(expected, 768);
                assert_eq!(actual, 3);
            }
            Err(EmbeddingError::RetryExhausted { .. }) => {
                // Also acceptable - retries exhausted due to repeated dimension errors
            }
            _ => panic!("Expected InvalidDimensions or RetryExhausted error"),
        }
    }
}

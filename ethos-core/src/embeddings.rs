//! Embeddings module for Ethos — multi-backend embedding support
//!
//! Provides an `EmbeddingBackend` trait with implementations for:
//! - **Gemini** — cloud embeddings via the Gemini API (768-dim)
//! - **ONNX** — local embeddings via `all-MiniLM-L6-v2` (384-dim)
//! - **Gemini-fallback-ONNX** — Gemini with graceful degradation to `Ok(None)`

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;
use tokio_retry::strategy::{jitter, ExponentialBackoff};
use tokio_retry::Retry;

/// Default Gemini embedding dimensions
pub const GEMINI_DIMENSIONS: usize = 768;

/// Default ONNX (all-MiniLM-L6-v2) embedding dimensions
pub const ONNX_DIMENSIONS: usize = 384;

// ============================================================================
// EmbeddingBackend trait
// ============================================================================

/// Abstraction over embedding providers.
#[async_trait]
pub trait EmbeddingBackend: Send + Sync {
    /// Embed a single text. Returns `None` if embedding is unavailable
    /// (used in fallback mode to signal graceful degradation).
    async fn embed(&self, text: &str) -> Result<Option<Vec<f32>>, EmbeddingError>;

    /// Embed a search query. Backends that support task-type hints (e.g. Gemini)
    /// can override this to use `RETRIEVAL_QUERY` instead of `RETRIEVAL_DOCUMENT`.
    /// Defaults to calling `embed()`.
    async fn embed_query(&self, text: &str) -> Result<Option<Vec<f32>>, EmbeddingError> {
        self.embed(text).await
    }

    /// Returns the embedding dimension (e.g., 768 or 384).
    fn dimensions(&self) -> usize;

    /// Backend name for logging.
    fn name(&self) -> &str;
}

// ============================================================================
// Error types
// ============================================================================

/// Task type for embedding API
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskType {
    #[default]
    RetrievalDocument,
    RetrievalQuery,
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

    #[error("ONNX model not found at {path} — run scripts/download-onnx-model.sh to fetch it")]
    ModelNotFound { path: String },

    #[error("ONNX inference error: {0}")]
    OnnxInference(String),

    #[error("Tokenizer error: {0}")]
    Tokenizer(String),
}

// ============================================================================
// Config types
// ============================================================================

/// Gemini embedding client configuration
#[derive(Debug, Clone)]
pub struct EmbeddingConfig {
    pub api_key: String,
    pub model: String,
    pub dimensions: usize,
    pub max_retries: usize,
    pub retry_delay_ms: u64,
}

impl EmbeddingConfig {
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

/// ONNX backend configuration
#[derive(Debug, Clone)]
pub struct OnnxConfig {
    pub model_path: PathBuf,
    pub tokenizer_path: PathBuf,
    pub dimensions: usize,
}

/// Configuration union for the backend factory.
pub enum BackendConfig {
    Gemini(EmbeddingConfig),
    Onnx(OnnxConfig),
    GeminiFallbackOnnx(EmbeddingConfig),
}

/// Create the appropriate backend from configuration.
pub fn create_backend(config: BackendConfig) -> Result<Box<dyn EmbeddingBackend>, EmbeddingError> {
    match config {
        BackendConfig::Gemini(c) => Ok(Box::new(GeminiEmbeddingClient::new(c)?)),
        BackendConfig::Onnx(c) => {
            Ok(Box::new(crate::onnx_embedder::OnnxEmbeddingClient::new(c)?))
        }
        BackendConfig::GeminiFallbackOnnx(c) => Ok(Box::new(FallbackEmbeddingClient::new(c)?)),
    }
}

// ============================================================================
// Gemini API structs (private)
// ============================================================================

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiRequest {
    model: String,
    content: GeminiContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_type: Option<TaskType>,
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

#[derive(Debug, Deserialize)]
struct GeminiResponse {
    embedding: GeminiEmbedding,
}

#[derive(Debug, Deserialize)]
struct GeminiEmbedding {
    values: Vec<f32>,
}

#[derive(Debug, Deserialize)]
struct GeminiErrorResponse {
    error: Option<GeminiErrorDetail>,
}

#[derive(Debug, Deserialize)]
struct GeminiErrorDetail {
    code: u16,
    message: String,
}

// ============================================================================
// GeminiEmbeddingClient
// ============================================================================

/// Gemini embedding client — calls the Gemini Embeddings API.
#[derive(Debug, Clone)]
pub struct GeminiEmbeddingClient {
    client: Client,
    config: EmbeddingConfig,
    base_url: String,
}

impl GeminiEmbeddingClient {
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
    pub fn with_base_url(
        config: EmbeddingConfig,
        base_url: String,
    ) -> Result<Self, EmbeddingError> {
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

    /// Generate an embedding for the given text (direct call, returns raw Vec)
    pub async fn embed_raw(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
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

        let result = Retry::spawn(retry_strategy, || self.embed_once(text, task_type)).await;

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
                parts: vec![GeminiPart {
                    text: text.to_string(),
                }],
            },
            task_type: Some(task_type),
            output_dimensionality: Some(self.config.dimensions),
        };

        let response = self.client.post(&url).json(&request).send().await?;

        let status = response.status();

        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            let error_detail = serde_json::from_str::<GeminiErrorResponse>(&error_body)
                .ok()
                .and_then(|e| e.error);

            let (code, message) = error_detail
                .map(|e| (e.code, e.message))
                .unwrap_or((status.as_u16(), error_body));

            tracing::error!(code = code, message = %message, "Gemini API error");

            return Err(EmbeddingError::Api { code, message });
        }

        let gemini_response: GeminiResponse = response.json().await?;

        let values = gemini_response.embedding.values;

        if values.len() != self.config.dimensions {
            return Err(EmbeddingError::InvalidDimensions {
                expected: self.config.dimensions,
                actual: values.len(),
            });
        }

        Ok(values)
    }
}

#[async_trait]
impl EmbeddingBackend for GeminiEmbeddingClient {
    async fn embed(&self, text: &str) -> Result<Option<Vec<f32>>, EmbeddingError> {
        self.embed_raw(text).await.map(Some)
    }

    async fn embed_query(&self, text: &str) -> Result<Option<Vec<f32>>, EmbeddingError> {
        self.embed_with_task(text, TaskType::RetrievalQuery)
            .await
            .map(Some)
    }

    fn dimensions(&self) -> usize {
        self.config.dimensions
    }

    fn name(&self) -> &str {
        "gemini"
    }
}

// ============================================================================
// FallbackEmbeddingClient
// ============================================================================

/// Wraps `GeminiEmbeddingClient`. On any error, logs a warning and returns
/// `Ok(None)` so the memory is stored without an embedding vector.
pub struct FallbackEmbeddingClient {
    inner: GeminiEmbeddingClient,
}

impl FallbackEmbeddingClient {
    pub fn new(config: EmbeddingConfig) -> Result<Self, EmbeddingError> {
        Ok(Self {
            inner: GeminiEmbeddingClient::new(config)?,
        })
    }

    #[cfg(test)]
    pub fn with_base_url(config: EmbeddingConfig, base_url: String) -> Result<Self, EmbeddingError> {
        Ok(Self {
            inner: GeminiEmbeddingClient::with_base_url(config, base_url)?,
        })
    }
}

#[async_trait]
impl EmbeddingBackend for FallbackEmbeddingClient {
    async fn embed(&self, text: &str) -> Result<Option<Vec<f32>>, EmbeddingError> {
        match self.inner.embed_raw(text).await {
            Ok(v) => Ok(Some(v)),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Gemini embedding failed — storing memory without embedding (keyword search only)"
                );
                Ok(None)
            }
        }
    }

    async fn embed_query(&self, text: &str) -> Result<Option<Vec<f32>>, EmbeddingError> {
        match self.inner.embed_with_task(text, TaskType::RetrievalQuery).await {
            Ok(v) => Ok(Some(v)),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Gemini query embedding failed — storing memory without embedding (keyword search only)"
                );
                Ok(None)
            }
        }
    }

    fn dimensions(&self) -> usize {
        GEMINI_DIMENSIONS
    }

    fn name(&self) -> &str {
        "gemini-fallback-onnx"
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_config(api_key: &str) -> EmbeddingConfig {
        EmbeddingConfig {
            api_key: api_key.to_string(),
            model: "gemini-embedding-001".to_string(),
            dimensions: GEMINI_DIMENSIONS,
            max_retries: 3,
            retry_delay_ms: 100,
        }
    }

    fn mock_embedding_response() -> serde_json::Value {
        let values: Vec<f32> = (0..768).map(|i| (i as f32) / 768.0).collect();
        serde_json::json!({
            "embedding": {
                "values": values
            }
        })
    }

    #[tokio::test]
    async fn test_embed_content_calls_api_and_returns_768_dim_vector() {
        let mock_server = MockServer::start().await;
        let config = test_config("test-api-key");
        let client =
            GeminiEmbeddingClient::with_base_url(config, mock_server.uri())
                .expect("Failed to create client");

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
                ResponseTemplate::new(200).set_body_json(mock_embedding_response()),
            )
            .mount(&mock_server)
            .await;

        let result = client.embed_raw("hello world").await;

        assert!(result.is_ok(), "Expected Ok, got Err: {:?}", result.err());
        let embedding = result.unwrap();
        assert_eq!(embedding.len(), 768, "Expected 768 dimensions");
    }

    #[tokio::test]
    async fn test_embed_returns_error_on_api_500() {
        let mock_server = MockServer::start().await;
        let config = test_config("test-api-key");
        let client =
            GeminiEmbeddingClient::with_base_url(config, mock_server.uri())
                .expect("Failed to create client");

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
                "error": { "code": 500, "message": "Internal server error" }
            })))
            .mount(&mock_server)
            .await;

        let result = client.embed_raw("hello world").await;

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
        let client =
            GeminiEmbeddingClient::with_base_url(config, mock_server.uri())
                .expect("Failed to create client");

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
                "error": { "code": 429, "message": "Rate limit exceeded" }
            })))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(mock_embedding_response()),
            )
            .mount(&mock_server)
            .await;

        let result = client.embed_raw("hello world").await;

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
        let client =
            GeminiEmbeddingClient::with_base_url(config, mock_server.uri())
                .expect("Failed to create client");

        let wrong_response = serde_json::json!({
            "embedding": {
                "values": [0.1, 0.2, 0.3]
            }
        });

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(wrong_response))
            .mount(&mock_server)
            .await;

        let result = client.embed_raw("hello world").await;

        assert!(result.is_err(), "Expected error on wrong dimensions");
        match result {
            Err(EmbeddingError::InvalidDimensions { expected, actual }) => {
                assert_eq!(expected, 768);
                assert_eq!(actual, 3);
            }
            Err(EmbeddingError::RetryExhausted { .. }) => {
                // Also acceptable
            }
            _ => panic!("Expected InvalidDimensions or RetryExhausted error"),
        }
    }

    // --- EmbeddingBackend trait tests ---

    #[tokio::test]
    async fn test_gemini_backend_trait_returns_some() {
        let mock_server = MockServer::start().await;
        let config = test_config("test-api-key");
        let backend: Box<dyn EmbeddingBackend> = Box::new(
            GeminiEmbeddingClient::with_base_url(config, mock_server.uri()).unwrap(),
        );

        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(mock_embedding_response()),
            )
            .mount(&mock_server)
            .await;

        let result = backend.embed("hello").await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 768);
        assert_eq!(backend.dimensions(), 768);
        assert_eq!(backend.name(), "gemini");
    }

    #[tokio::test]
    async fn test_fallback_returns_none_on_gemini_error() {
        let mock_server = MockServer::start().await;
        let config = EmbeddingConfig {
            api_key: "test-key".to_string(),
            model: "gemini-embedding-001".to_string(),
            dimensions: GEMINI_DIMENSIONS,
            max_retries: 1,
            retry_delay_ms: 10,
        };
        let fallback = FallbackEmbeddingClient::with_base_url(config, mock_server.uri()).unwrap();

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
                "error": { "code": 500, "message": "boom" }
            })))
            .mount(&mock_server)
            .await;

        let result = fallback.embed("hello").await;
        assert!(result.is_ok(), "Fallback should not propagate errors");
        assert!(result.unwrap().is_none(), "Fallback should return None on error");
        assert_eq!(fallback.name(), "gemini-fallback-onnx");
    }

    #[tokio::test]
    async fn test_fallback_returns_some_on_success() {
        let mock_server = MockServer::start().await;
        let config = test_config("test-api-key");
        let fallback = FallbackEmbeddingClient::with_base_url(config, mock_server.uri()).unwrap();

        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(mock_embedding_response()),
            )
            .mount(&mock_server)
            .await;

        let result = fallback.embed("hello").await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 768);
    }
}

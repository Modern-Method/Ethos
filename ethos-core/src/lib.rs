pub mod config;
pub mod db;
pub mod embeddings;
pub mod error;
pub mod graph;
pub mod ipc;
pub mod models;
pub mod onnx_embedder;

pub use config::EthosConfig;
pub use embeddings::{
    BackendConfig, EmbeddingBackend, EmbeddingConfig, EmbeddingError, FallbackEmbeddingClient,
    GeminiEmbeddingClient, OnnxConfig, GEMINI_DIMENSIONS, ONNX_DIMENSIONS,
    create_backend,
};
pub use error::EthosError;
pub use graph::{ActivationNode, SpreadResult};
pub use onnx_embedder::OnnxEmbeddingClient;

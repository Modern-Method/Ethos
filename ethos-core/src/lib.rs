pub mod config;
pub mod db;
pub mod embeddings;
pub mod error;
pub mod graph;
pub mod ipc;
pub mod models;

pub use config::EthosConfig;
pub use embeddings::{EmbeddingConfig, EmbeddingError, GeminiEmbeddingClient, GEMINI_DIMENSIONS};
pub use error::EthosError;
pub use graph::{ActivationNode, SpreadResult};

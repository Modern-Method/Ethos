use config::{Config, ConfigError, File};
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct EthosConfig {
    pub service: ServiceConfig,
    pub database: DatabaseConfig,
    pub embedding: EmbeddingConfig,
    pub consolidation: ConsolidationConfig,
    pub retrieval: RetrievalConfig,
    pub decay: DecayConfig,
    pub conflict_resolution: ConflictResolutionConfig,
    #[serde(default)]
    pub http: HttpConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServiceConfig {
    pub socket_path: String,
    pub log_level: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct EmbeddingConfig {
    pub backend: String,
    pub gemini_model: String,
    pub gemini_dimensions: u32,
    pub onnx_model: String,
    pub onnx_dimensions: u32,
    pub batch_size: u32,
    pub batch_timeout_seconds: u64,
    pub queue_capacity: u32,
    pub rate_limit_rpm: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ConsolidationConfig {
    pub interval_minutes: u64,
    pub idle_threshold_seconds: u64,
    pub cpu_threshold_percent: u8,
    pub importance_threshold: f32,
    pub repetition_threshold: u32,
    pub retrieval_threshold: u32,
}

impl Default for ConsolidationConfig {
    fn default() -> Self {
        Self {
            interval_minutes: 15,
            idle_threshold_seconds: 60,
            cpu_threshold_percent: 80,
            importance_threshold: 0.8,
            repetition_threshold: 3,
            retrieval_threshold: 5,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct RetrievalConfig {
    pub decay_factor: f32,
    pub spreading_strength: f32,
    pub iterations: u32,
    pub anchor_top_k_episodes: u32,
    pub anchor_top_k_facts: u32,
    pub weight_similarity: f32,
    pub weight_activation: f32,
    pub weight_structural: f32,
    pub confidence_gate: f32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DecayConfig {
    pub base_tau_days: f64,
    pub ltp_multiplier: f64,
    pub frequency_weight: f64,
    pub emotional_weight: f64,
    pub prune_threshold: f64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ConflictResolutionConfig {
    pub auto_supersede_confidence_delta: f64,
    pub review_inbox: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct HttpConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            host: "127.0.0.1".to_string(),
            port: 8766,
        }
    }
}

impl EthosConfig {
    pub fn load(path: &str) -> Result<Self, ConfigError> {
        let s = Config::builder()
            .add_source(File::with_name(path))
            .build()?;
        s.try_deserialize()
    }
}

//! ONNX embedding backend — local inference via `all-MiniLM-L6-v2`
//!
//! Uses the `ort` crate for ONNX Runtime and `tokenizers` for BPE tokenization.
//! Produces 384-dimensional embeddings entirely offline.

use async_trait::async_trait;
use ort::session::Session;
use ort::value::Tensor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::embeddings::{EmbeddingBackend, EmbeddingError, OnnxConfig};

/// Local ONNX embedding client using `all-MiniLM-L6-v2`.
pub struct OnnxEmbeddingClient {
    session: Arc<Mutex<Session>>,
    tokenizer: Arc<tokenizers::Tokenizer>,
    dimensions: usize,
}

impl std::fmt::Debug for OnnxEmbeddingClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnnxEmbeddingClient")
            .field("dimensions", &self.dimensions)
            .finish_non_exhaustive()
    }
}

impl OnnxEmbeddingClient {
    /// Create a new ONNX embedding client.
    ///
    /// Loads the ONNX model and tokenizer from the paths specified in `config`.
    /// Returns `EmbeddingError::ModelNotFound` if either file is missing.
    pub fn new(config: OnnxConfig) -> Result<Self, EmbeddingError> {
        if !config.model_path.exists() {
            return Err(EmbeddingError::ModelNotFound {
                path: config.model_path.display().to_string(),
            });
        }
        if !config.tokenizer_path.exists() {
            return Err(EmbeddingError::ModelNotFound {
                path: config.tokenizer_path.display().to_string(),
            });
        }

        let session = Session::builder()
            .and_then(|b| b.with_intra_threads(1))
            .and_then(|b| b.commit_from_file(&config.model_path))
            .map_err(|e| EmbeddingError::OnnxInference(e.to_string()))?;

        let tokenizer = tokenizers::Tokenizer::from_file(&config.tokenizer_path)
            .map_err(|e| EmbeddingError::Tokenizer(e.to_string()))?;

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            tokenizer: Arc::new(tokenizer),
            dimensions: config.dimensions,
        })
    }
}

#[async_trait]
impl EmbeddingBackend for OnnxEmbeddingClient {
    async fn embed(&self, text: &str) -> Result<Option<Vec<f32>>, EmbeddingError> {
        // ONNX inference is CPU-bound — run on the blocking thread pool.
        let session = Arc::clone(&self.session);
        let tokenizer = Arc::clone(&self.tokenizer);
        let dimensions = self.dimensions;
        let text = text.to_string();

        let result = tokio::task::spawn_blocking(move || {
            let mut session_guard = session
                .lock()
                .map_err(|e| EmbeddingError::OnnxInference(format!("session lock poisoned: {e}")))?;
            embed_sync(&mut session_guard, &tokenizer, &text, dimensions)
        })
        .await
        .map_err(|e| EmbeddingError::OnnxInference(format!("spawn_blocking join error: {e}")))?;

        result.map(Some)
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn name(&self) -> &str {
        "onnx"
    }
}

/// Run ONNX inference synchronously.
fn embed_sync(
    session: &mut Session,
    tokenizer: &tokenizers::Tokenizer,
    text: &str,
    expected_dims: usize,
) -> Result<Vec<f32>, EmbeddingError> {
    // 1. Tokenize
    let encoding = tokenizer
        .encode(text, true)
        .map_err(|e| EmbeddingError::Tokenizer(e.to_string()))?;

    let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
    let attention_mask: Vec<i64> = encoding
        .get_attention_mask()
        .iter()
        .map(|&m| m as i64)
        .collect();
    let token_type_ids: Vec<i64> = encoding
        .get_type_ids()
        .iter()
        .map(|&t| t as i64)
        .collect();

    let seq_len = input_ids.len();
    let shape = vec![1i64, seq_len as i64];

    // 2. Build input tensors via Tensor::from_array (batch_size=1)
    let input_ids_tensor = Tensor::from_array((shape.clone(), input_ids))
        .map_err(|e| EmbeddingError::OnnxInference(e.to_string()))?;
    let attention_mask_tensor = Tensor::from_array((shape.clone(), attention_mask.clone()))
        .map_err(|e| EmbeddingError::OnnxInference(e.to_string()))?;
    let token_type_ids_tensor = Tensor::from_array((shape, token_type_ids))
        .map_err(|e| EmbeddingError::OnnxInference(e.to_string()))?;

    let inputs = ort::inputs! {
        "input_ids" => input_ids_tensor,
        "attention_mask" => attention_mask_tensor,
        "token_type_ids" => token_type_ids_tensor,
    };

    // 3. Run session
    let outputs = session
        .run(inputs)
        .map_err(|e| EmbeddingError::OnnxInference(e.to_string()))?;

    // 4. Extract last hidden state
    // try_extract_tensor returns (&Shape, &[f32])
    // Shape derefs to [i64] for dimension access
    let (out_shape, data) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| EmbeddingError::OnnxInference(e.to_string()))?;

    // Expected shape: [1, seq_len, hidden_dim]
    if out_shape.len() != 3 {
        return Err(EmbeddingError::OnnxInference(format!(
            "Expected 3D output, got {}D",
            out_shape.len()
        )));
    }
    let out_seq_len = out_shape[1] as usize;
    let hidden_dim = out_shape[2] as usize;

    // 5. Mean-pool over sequence length, masked by attention_mask
    let mut pooled = vec![0.0f32; hidden_dim];
    let mask_sum: f32 = attention_mask.iter().map(|&m| m as f32).sum();

    for tok_idx in 0..out_seq_len {
        let mask_val = if tok_idx < attention_mask.len() {
            attention_mask[tok_idx] as f32
        } else {
            0.0
        };
        if mask_val > 0.0 {
            let offset = tok_idx * hidden_dim; // flat index into [1, seq_len, hidden_dim]
            for dim in 0..hidden_dim {
                pooled[dim] += data[offset + dim] * mask_val;
            }
        }
    }
    if mask_sum > 0.0 {
        for v in &mut pooled {
            *v /= mask_sum;
        }
    }

    // 6. L2 normalize
    let norm: f32 = pooled.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in &mut pooled {
            *v /= norm;
        }
    }

    if pooled.len() != expected_dims {
        return Err(EmbeddingError::InvalidDimensions {
            expected: expected_dims,
            actual: pooled.len(),
        });
    }

    Ok(pooled)
}

/// Resolve the default model directory.
pub fn default_model_dir() -> PathBuf {
    let data_home = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".local/share")
        });
    data_home.join("ethos/models")
}

/// Resolve paths for the ONNX model and tokenizer.
///
/// If `onnx_model_path` from config is empty, uses the default location.
pub fn resolve_onnx_paths(onnx_model_path: &str) -> (PathBuf, PathBuf) {
    if onnx_model_path.is_empty() {
        let dir = default_model_dir();
        (
            dir.join("all-MiniLM-L6-v2.onnx"),
            dir.join("all-MiniLM-L6-v2-tokenizer.json"),
        )
    } else {
        let model = PathBuf::from(onnx_model_path);
        let stem = model
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let tokenizer = model.with_file_name(format!("{stem}-tokenizer.json"));
        (model, tokenizer)
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::{OnnxConfig, ONNX_DIMENSIONS};

    #[test]
    fn test_model_not_found_returns_error() {
        let config = OnnxConfig {
            model_path: PathBuf::from("/nonexistent/model.onnx"),
            tokenizer_path: PathBuf::from("/nonexistent/tokenizer.json"),
            dimensions: ONNX_DIMENSIONS,
        };

        let result = OnnxEmbeddingClient::new(config);
        assert!(result.is_err());
        match result.unwrap_err() {
            EmbeddingError::ModelNotFound { path } => {
                assert!(path.contains("nonexistent"), "path was: {path}");
            }
            other => panic!("Expected ModelNotFound, got: {other:?}"),
        }
    }

    #[test]
    fn test_default_model_dir_contains_ethos() {
        let dir = default_model_dir();
        assert!(
            dir.to_string_lossy().contains("ethos/models"),
            "Expected ethos/models in path, got: {}",
            dir.display()
        );
    }

    #[test]
    fn test_resolve_onnx_paths_default() {
        let (model, tokenizer) = resolve_onnx_paths("");
        assert!(model.to_string_lossy().ends_with("all-MiniLM-L6-v2.onnx"));
        assert!(tokenizer
            .to_string_lossy()
            .ends_with("all-MiniLM-L6-v2-tokenizer.json"));
    }

    #[test]
    fn test_resolve_onnx_paths_custom() {
        let (model, tokenizer) = resolve_onnx_paths("/opt/models/custom.onnx");
        assert_eq!(model, PathBuf::from("/opt/models/custom.onnx"));
        assert_eq!(
            tokenizer,
            PathBuf::from("/opt/models/custom-tokenizer.json")
        );
    }
}

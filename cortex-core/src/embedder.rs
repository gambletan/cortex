//! Local embedding engine using fastembed (all-MiniLM-L6-v2, 384 dimensions).
//! Gated behind the `embeddings` cargo feature. On first use the model (~30MB) is
//! downloaded from the Hugging Face CDN and cached locally; it then runs fully offline
//! and transmits none of the user's data. Set `CORTEX_NO_EMBEDDINGS=1` for a zero-network
//! path (see `Cortex::auto_embed`).

#[cfg(feature = "embeddings")]
mod inner {
    use crate::CortexError;
    use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
    use std::sync::Arc;
    use parking_lot::Mutex;

    /// Local embedding engine — generates 384-dim vectors from text.
    pub struct Embedder {
        model: Arc<Mutex<TextEmbedding>>,
        dimensions: usize,
    }

    impl Embedder {
        /// Create a new embedder using gte-small (384 dimensions).
        /// Downloads the model on first use (~30MB, cached locally).
        pub fn new() -> Result<Self, CortexError> {
            Self::with_model(EmbeddingModel::AllMiniLML6V2)
        }

        /// Create an embedder with a specific model.
        pub fn with_model(model: EmbeddingModel) -> Result<Self, CortexError> {
            let dimensions = match model {
                EmbeddingModel::AllMiniLML6V2 => 384,
                EmbeddingModel::AllMiniLML6V2Q => 384,
                _ => 384, // default assumption
            };

            let options = InitOptions::new(model).with_show_download_progress(true);
            let text_embedding = TextEmbedding::try_new(options)
                .map_err(|e| CortexError::Storage(format!("Failed to initialize embedding model: {}", e)))?;

            tracing::info!("Embedding model loaded ({} dimensions)", dimensions);

            Ok(Self {
                model: Arc::new(Mutex::new(text_embedding)),
                dimensions,
            })
        }

        /// Generate an embedding for a single text.
        pub fn embed(&self, text: &str) -> Result<Vec<f32>, CortexError> {
            let model = self.model.lock();
            let embeddings = model
                .embed(vec![text], None)
                .map_err(|e| CortexError::Storage(format!("Embedding failed: {}", e)))?;

            embeddings
                .into_iter()
                .next()
                .ok_or_else(|| CortexError::Storage("No embedding returned".to_string()))
        }

        /// Generate embeddings for multiple texts (batch).
        pub fn embed_batch(&self, texts: Vec<&str>) -> Result<Vec<Vec<f32>>, CortexError> {
            if texts.is_empty() {
                return Ok(Vec::new());
            }
            let model = self.model.lock();
            model
                .embed(texts, None)
                .map_err(|e| CortexError::Storage(format!("Batch embedding failed: {}", e)))
        }

        /// Get the embedding dimensionality.
        pub fn dimensions(&self) -> usize {
            self.dimensions
        }
    }
}

#[cfg(feature = "embeddings")]
pub use inner::Embedder;

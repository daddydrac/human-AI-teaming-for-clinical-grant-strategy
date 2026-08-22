use anyhow::{bail, Context, Result};
use parking_lot::Mutex;
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct EmbeddingClient {
    client: Client,
    url: String,
    model: String,
    batch_size: usize,
    health_cache: Arc<Mutex<Option<(Instant, usize)>>>,
    health_ttl: Duration,
    document_prefix: String,
    query_prefix: String,
}

#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingItem>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingItem {
    index: usize,
    embedding: Vec<f32>,
}

impl EmbeddingClient {
    pub fn from_env() -> Result<Self> {
        let timeout = std::env::var("EMBEDDING_HTTP_TIMEOUT_SECONDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120u64);
        let batch_size = std::env::var("EMBEDDING_BATCH_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(64usize)
            .clamp(1, 512);
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(timeout))
                .build()?,
            url: std::env::var("EMBEDDING_URL")
                .unwrap_or_else(|_| "http://host.docker.internal:8000/v1/embeddings".to_string()),
            model: std::env::var("EMBEDDING_MODEL")
                .unwrap_or_else(|_| "grant-embedding".to_string()),
            batch_size,
            health_cache: Arc::new(Mutex::new(None)),
            health_ttl: Duration::from_secs(
                std::env::var("EMBEDDING_HEALTH_TTL_SECONDS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(600u64),
            ),
            document_prefix: std::env::var("EMBEDDING_DOCUMENT_PREFIX").unwrap_or_default(),
            query_prefix: std::env::var("EMBEDDING_QUERY_PREFIX").unwrap_or_default(),
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub async fn health(&self) -> Result<serde_json::Value> {
        if let Some((checked, dimensions)) = *self.health_cache.lock() {
            if checked.elapsed() < self.health_ttl {
                return Ok(
                    serde_json::json!({"ok":true,"model":self.model,"dimensions":dimensions,"cached":true}),
                );
            }
        }
        let v = self.embed_query("health check").await?;
        let dimensions = v.len();
        *self.health_cache.lock() = Some((Instant::now(), dimensions));
        Ok(serde_json::json!({"ok":true,"model":self.model,"dimensions":dimensions,"cached":false}))
    }

    pub async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let mut all = Vec::with_capacity(inputs.len());
        let mut expected_dim: Option<usize> = None;
        for chunk in inputs.chunks(self.batch_size) {
            let r = self
                .client
                .post(&self.url)
                .json(&json!({"model": self.model, "input": chunk}))
                .send()
                .await?
                .error_for_status()
                .context("embedding endpoint returned error")?;
            let mut body: EmbeddingResponse = r.json().await?;
            body.data.sort_by_key(|x| x.index);
            if body.data.len() != chunk.len() {
                bail!(
                    "embedding endpoint returned {} vectors for {} inputs",
                    body.data.len(),
                    chunk.len()
                );
            }
            for item in body.data {
                if item.embedding.is_empty() {
                    bail!("embedding endpoint returned empty vector");
                }
                match expected_dim {
                    None => expected_dim = Some(item.embedding.len()),
                    Some(d) if d != item.embedding.len() => bail!(
                        "embedding dimension changed within request: {d} -> {}",
                        item.embedding.len()
                    ),
                    _ => {}
                }
                all.push(item.embedding);
            }
        }
        Ok(all)
    }

    pub async fn embed_documents(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
        if self.document_prefix.is_empty() {
            return self.embed(inputs).await;
        }
        let prefixed: Vec<String> = inputs
            .iter()
            .map(|x| format!("{}{}", self.document_prefix, x))
            .collect();
        self.embed(&prefixed).await
    }

    pub async fn embed_query(&self, input: &str) -> Result<Vec<f32>> {
        let text = if self.query_prefix.is_empty() {
            input.to_string()
        } else {
            format!("{}{}", self.query_prefix, input)
        };
        self.embed_one(&text).await
    }

    pub async fn embed_one(&self, input: &str) -> Result<Vec<f32>> {
        let mut v = self.embed(&[input.to_string()]).await?;
        v.pop().context("embedding endpoint returned no vector")
    }
}

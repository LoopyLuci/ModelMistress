//! Embedders: how the selector (and memory retrieval) turns text into vectors.
//!
//! * [`HubEmbedder`] asks an embedding model the hub serves (llama-server's `/v1/embeddings`, the same path as
//!   `embeddings.create`). Its id is the configured `km.embed_model`; a KM's centroid is only comparable when the KM's
//!   `routing.embed_model` is that same id.
//! * [`HashEmbedder`] needs nothing: it hashes words and character trigrams into a fixed number of buckets. Its id is
//!   `hash-<dim>`. It is deterministic, so tests and machines without an embedding model get a working (lexical-ish)
//!   vector term. It is not a semantic model and says so by its name.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::Res;
use crate::engine::{Engine, Fail};

pub trait Embedder: Send + Sync {
    /// The id a KM's `routing.embed_model` must equal for vectors to be comparable.
    fn model_id(&self) -> String;
    fn embed<'a>(&'a self, texts: &'a [String]) -> BoxFuture<'a, Res<Vec<Vec<f32>>>>;
}

pub const HASH_DIM: usize = 384;

pub struct HashEmbedder {
    pub dim: usize,
}

impl Default for HashEmbedder {
    fn default() -> Self {
        Self { dim: HASH_DIM }
    }
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

fn bucket(feature: &str, dim: usize) -> (usize, f32) {
    let h = Sha256::digest(feature.as_bytes());
    let n = u64::from_le_bytes(h[..8].try_into().unwrap_or([0; 8]));
    ((n % dim as u64) as usize, if h[8] & 1 == 0 { 1.0 } else { -1.0 })
}

impl HashEmbedder {
    pub fn embed_one(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0f32; self.dim];
        for w in words(text) {
            let (i, s) = bucket(&format!("w:{w}"), self.dim);
            v[i] += 2.0 * s;
            let padded: Vec<char> = format!("^{w}$").chars().collect();
            for tri in padded.windows(3) {
                let (i, s) = bucket(&format!("t:{}", tri.iter().collect::<String>()), self.dim);
                v[i] += 0.5 * s;
            }
        }
        normalize(&mut v);
        v
    }
}

impl Embedder for HashEmbedder {
    fn model_id(&self) -> String {
        format!("hash-{}", self.dim)
    }
    fn embed<'a>(&'a self, texts: &'a [String]) -> BoxFuture<'a, Res<Vec<Vec<f32>>>> {
        Box::pin(async move { Ok(texts.iter().map(|t| self.embed_one(t)).collect()) })
    }
}

pub fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// f16 little-endian bytes to f32.
#[allow(clippy::chunks_exact_to_as_chunks)]
pub fn f16_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|b| half::f16::from_le_bytes([b[0], b[1]]).to_f32())
        .collect()
}

pub fn f32_to_f16(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| half::f16::from_f32(*x).to_le_bytes()).collect()
}

/// The hub's embedding model: `POST /v1/embeddings` on the model named in `km.embed_model`.
pub struct HubEmbedder {
    pub engine: Arc<Engine>,
    pub model: String,
}

impl Embedder for HubEmbedder {
    fn model_id(&self) -> String {
        self.model.clone()
    }
    fn embed<'a>(&'a self, texts: &'a [String]) -> BoxFuture<'a, Res<Vec<Vec<f32>>>> {
        Box::pin(async move {
            let body = json!({"model": self.model, "input": texts});
            let (model, resp, _lease) = self.engine.forward("/v1/embeddings", body).await?;
            let status = resp.status();
            let v: Value = resp.json().await.unwrap_or(Value::Null);
            if !status.is_success() {
                return Err(Fail::new(
                    502,
                    "backend_error",
                    format!(
                        "{model}: {}",
                        v["error"]["message"].as_str().unwrap_or("embedding failed")
                    ),
                ));
            }
            let rows: Vec<Vec<f32>> = v["data"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|d| {
                            d["embedding"]
                                .as_array()
                                .map(|e| e.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect())
                                .unwrap_or_default()
                        })
                        .collect()
                })
                .unwrap_or_default();
            if rows.len() != texts.len() || rows.iter().any(|r: &Vec<f32>| r.is_empty()) {
                return Err(Fail::new(
                    502,
                    "backend_error",
                    format!("{model} returned no usable embeddings"),
                ));
            }
            Ok(rows)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_embedder_is_deterministic_and_unit_length() {
        let e = HashEmbedder::default();
        assert_eq!(e.model_id(), "hash-384");
        let a = e.embed_one("Async Rust with Tokio");
        assert_eq!(a, e.embed_one("async rust with tokio"));
        assert!((a.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-4);
        assert_eq!(a.len(), 384);
    }

    #[test]
    fn similar_text_is_closer_than_unrelated_text() {
        let e = HashEmbedder::default();
        let q = e.embed_one("how do I cancel a tokio task");
        let near = e.embed_one("tokio task cancellation and select");
        let far = e.embed_one("baking sourdough bread with rye flour");
        assert!(cosine(&q, &near) > cosine(&q, &far) + 0.1);
    }

    #[test]
    fn f16_round_trips_closely() {
        let v = vec![0.5f32, -0.25, 1.0, 0.0];
        let back = f16_to_f32(&f32_to_f16(&v));
        assert_eq!(v, back);
        assert_eq!(
            cosine(&[1.0, 0.0], &[1.0, 0.0, 0.0]),
            0.0,
            "mismatched length is no similarity"
        );
    }

    #[tokio::test]
    async fn the_trait_object_works() {
        let e: Arc<dyn Embedder> = Arc::new(HashEmbedder::default());
        let r = e.embed(&["a".to_string(), "b".to_string()]).await.unwrap();
        assert_eq!(r.len(), 2);
    }
}

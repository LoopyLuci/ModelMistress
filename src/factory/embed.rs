//! The factory's embedder: a small embedding GGUF served by the hub (`mm-embed-1`, for example Qwen3-Embedding-0.6B
//! in Q8_0), asked in small batches with long texts cut, and the vectors helpers the steps share. The hashing
//! embedder (`km::embed::HashEmbedder`) stays the offline default and what the tests use.

use std::sync::Arc;

use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{json, Value};

use super::model::Device;
use super::store::Res;
use crate::engine::{Engine, Fail, LoadOpts, Target};
use crate::km::embed::{normalize, Embedder};

/// Texts longer than this many characters are cut before embedding (embedding models have a short context).
pub const MAX_EMBED_CHARS: usize = 3000;
const BATCH: usize = 8;

/// `POST /v1/embeddings` on a hub model, in batches. `name` is the id the vectors are filed under
/// (`routing.embed_model`); `model` is the hub model that makes them.
pub struct HubEmbedding {
    pub engine: Arc<Engine>,
    pub model: String,
    pub device: Device,
    loaded: Mutex<bool>,
}

impl HubEmbedding {
    pub fn new(engine: Arc<Engine>, model: &str, device: Device) -> Self {
        Self {
            engine,
            model: model.into(),
            device,
            loaded: Mutex::new(false),
        }
    }

    async fn place(&self) -> Res<()> {
        if *self.loaded.lock() {
            return Ok(());
        }
        if let (Ok(Target::Llama(entry)), Some(layers)) = (self.engine.resolve(&self.model), self.device.gpu_layers()) {
            if !self.engine.is_loaded(&entry.id) {
                let opts = LoadOpts {
                    gpu_layers: Some(layers),
                    embeddings: true,
                    ..Default::default()
                };
                self.engine.ensure_loaded(&entry, &opts, true).await?;
            }
        }
        *self.loaded.lock() = true;
        Ok(())
    }

    async fn batch(&self, texts: &[String]) -> Res<Vec<Vec<f32>>> {
        let input: Vec<String> = texts
            .iter()
            .map(|t| {
                let t = if t.trim().is_empty() { "-" } else { t.as_str() };
                t.chars().take(MAX_EMBED_CHARS).collect()
            })
            .collect();
        let (model, resp, _lease) = self
            .engine
            .forward("/v1/embeddings", json!({"model": self.model, "input": input}))
            .await?;
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
        let mut rows: Vec<(u64, Vec<f32>)> = v["data"]
            .as_array()
            .map(|a| {
                a.iter()
                    .enumerate()
                    .map(|(i, d)| {
                        (
                            d["index"].as_u64().unwrap_or(i as u64),
                            d["embedding"]
                                .as_array()
                                .map(|e| e.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect())
                                .unwrap_or_default(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        rows.sort_by_key(|r| r.0);
        let mut out: Vec<Vec<f32>> = rows.into_iter().map(|r| r.1).collect();
        if out.len() != texts.len() || out.iter().any(|r| r.is_empty()) {
            return Err(Fail::new(
                502,
                "backend_error",
                format!("{model} returned no usable embeddings for {} texts", texts.len()),
            ));
        }
        out.iter_mut().for_each(|r| normalize(r));
        Ok(out)
    }
}

impl Embedder for HubEmbedding {
    fn model_id(&self) -> String {
        self.model.clone()
    }

    fn embed<'a>(&'a self, texts: &'a [String]) -> BoxFuture<'a, Res<Vec<Vec<f32>>>> {
        Box::pin(async move {
            self.place().await?;
            let mut out = Vec::with_capacity(texts.len());
            for part in texts.chunks(BATCH) {
                out.extend(self.batch(part).await?);
            }
            Ok(out)
        })
    }
}

/// The unit-length mean of some vectors (a KM's centroid); None for no vectors.
pub fn centroid(vs: &[&[f32]]) -> Option<Vec<f32>> {
    let dim = vs.first()?.len();
    let mut m = vec![0f32; dim];
    let mut n = 0;
    for v in vs.iter().filter(|v| v.len() == dim) {
        m.iter_mut().zip(v.iter()).for_each(|(a, b)| *a += b);
        n += 1;
    }
    if n == 0 {
        return None;
    }
    normalize(&mut m);
    Some(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_centroid_is_the_unit_mean() {
        let a = [1.0f32, 0.0];
        let b = [0.0f32, 1.0];
        let c = centroid(&[&a, &b]).unwrap();
        assert!((c[0] - c[1]).abs() < 1e-6 && (c.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-5);
        assert!(centroid(&[]).is_none());
        // vectors of another size are left out
        let odd = [1.0f32, 2.0, 3.0];
        assert_eq!(centroid(&[&a, &odd]).unwrap(), vec![1.0, 0.0]);
    }
}

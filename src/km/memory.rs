//! Memory KMs (km-spec.md §3): text chunks with embeddings, queried by cosine similarity.
//!
//! Layout (named in `[memory]`): `chunks` is JSONL, one `{"id", "text", ...}` per line; `embeddings` is f16 little
//! endian, row-major, one row of `dim` values per chunk in the same order. The index is a flat scan: exact, and fast
//! enough for the chunk counts a KM holds; a KM that asks for `index = "hnsw"` is served by the flat scan too (no ANN
//! library is built in) and the answer says so. Hits carry the chunk id and score so answers can cite them.

use std::path::Path;

use serde_json::{json, Value};

use super::embed::normalize;
use super::manifest::Manifest;
use super::Res;
use crate::engine::Fail;

#[derive(Debug, Clone)]
pub struct Chunk {
    pub id: String,
    pub text: String,
    pub source: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub chunk_id: String,
    pub score: f32,
    /// Position in the KM's chunk file.
    pub ord: usize,
    pub text: String,
    pub source: Option<String>,
}

impl Hit {
    pub fn to_json(&self, km: &str) -> Value {
        json!({"km": km, "chunk": self.chunk_id, "score": self.score, "ord": self.ord, "text": self.text,
               "source": self.source, "citation": format!("{km}#{}", self.chunk_id)})
    }
}

pub struct MemIndex {
    pub dim: usize,
    pub embed_model: String,
    pub index: String,
    pub chunks: Vec<Chunk>,
    emb: Vec<u8>,
    inv_norm: Vec<f32>,
    /// Bytes held: what the payload files take.
    pub bytes: u64,
}

impl MemIndex {
    pub fn load(dir: &Path, m: &Manifest) -> Res<MemIndex> {
        let mem = m
            .memory
            .as_ref()
            .ok_or_else(|| Fail::invalid(format!("{} is not a memory KM", m.key())))?;
        let chunks_text = std::fs::read_to_string(dir.join(&mem.chunks))
            .map_err(|e| Fail::invalid(format!("{}: {e}", mem.chunks)))?;
        let emb =
            std::fs::read(dir.join(&mem.embeddings)).map_err(|e| Fail::invalid(format!("{}: {e}", mem.embeddings)))?;
        let mut chunks = vec![];
        let mut seen = std::collections::HashSet::new();
        for (i, line) in chunks_text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            let v: Value =
                serde_json::from_str(line).map_err(|e| Fail::invalid(format!("{} line {}: {e}", mem.chunks, i + 1)))?;
            let id = v["id"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| {
                Fail::invalid(format!("{} line {}: every chunk needs a string id", mem.chunks, i + 1))
            })?;
            let text = v["text"].as_str().ok_or_else(|| {
                Fail::invalid(format!(
                    "{} line {}: every chunk needs a string text",
                    mem.chunks,
                    i + 1
                ))
            })?;
            if !seen.insert(id.to_string()) {
                return Err(Fail::invalid(format!("{}: chunk id {id:?} appears twice", mem.chunks)));
            }
            chunks.push(Chunk {
                id: id.to_string(),
                text: text.to_string(),
                source: v["source"].as_str().map(str::to_string),
            });
        }
        let row = mem.dim * 2;
        if emb.len() != chunks.len() * row {
            return Err(Fail::invalid(format!(
                "{} holds {} bytes; {} chunks of {} dimensions need {}",
                mem.embeddings,
                emb.len(),
                chunks.len(),
                mem.dim,
                chunks.len() * row
            )));
        }
        let inv_norm = (0..chunks.len())
            .map(|i| {
                let n = row_f32(&emb, i, mem.dim).iter().map(|x| x * x).sum::<f32>().sqrt();
                if n > 0.0 {
                    1.0 / n
                } else {
                    0.0
                }
            })
            .collect();
        Ok(MemIndex {
            dim: mem.dim,
            embed_model: m.routing.embed_model.clone().unwrap_or_default(),
            index: mem.index.clone(),
            bytes: chunks_text.len() as u64 + emb.len() as u64,
            chunks,
            emb,
            inv_norm,
        })
    }

    /// The best `k` chunks by cosine similarity to `q`, with at least `min_score`, best first.
    pub fn query(&self, q: &[f32], k: usize, min_score: f32) -> Res<Vec<Hit>> {
        if q.len() != self.dim {
            return Err(Fail::invalid(format!(
                "the query has {} dimensions; this KM's embeddings have {}",
                q.len(),
                self.dim
            )));
        }
        let mut qn = q.to_vec();
        normalize(&mut qn);
        let mut scored: Vec<(usize, f32)> = (0..self.chunks.len())
            .map(|i| {
                let dot: f32 = row_f32(&self.emb, i, self.dim)
                    .iter()
                    .zip(&qn)
                    .map(|(a, b)| a * b)
                    .sum();
                (i, dot * self.inv_norm[i])
            })
            .filter(|(_, s)| *s >= min_score)
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        scored.truncate(k);
        Ok(scored
            .into_iter()
            .map(|(i, score)| Hit {
                chunk_id: self.chunks[i].id.clone(),
                score,
                ord: i,
                text: self.chunks[i].text.clone(),
                source: self.chunks[i].source.clone(),
            })
            .collect())
    }
}

fn row_f32(emb: &[u8], i: usize, dim: usize) -> Vec<f32> {
    super::embed::f16_to_f32(&emb[i * dim * 2..(i + 1) * dim * 2])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::km::embed::HashEmbedder;
    use crate::km::sample;

    fn kms() -> (crate::testing::TestDir, std::path::PathBuf) {
        let home = crate::testing::temp_home();
        let dir = sample::memory(
            &home,
            "docs.rust",
            "1.0.0",
            &[
                (
                    "c1",
                    "tokio tasks can be cancelled by dropping the JoinHandle or calling abort",
                ),
                (
                    "c2",
                    "the borrow checker enforces aliasing xor mutability at compile time",
                ),
                ("c3", "select! waits on several futures and cancels the losers"),
            ],
        );
        (home, dir)
    }

    #[test]
    fn retrieval_returns_chunk_ids_with_scores_best_first() {
        let (home, dir) = kms();
        let m = Manifest::from_file(&dir.join("km.toml")).unwrap();
        let idx = MemIndex::load(&dir, &m).unwrap();
        assert_eq!(idx.chunks.len(), 3);
        assert_eq!(idx.embed_model, "hash-384");
        let q = HashEmbedder::default().embed_one("how do I cancel a tokio task");
        let hits = idx.query(&q, 2, -1.0).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].chunk_id, "c1");
        assert!(hits[0].score >= hits[1].score);
        assert!(hits[0].score > 0.1 && hits[0].score <= 1.0001);
        assert_eq!(hits[0].to_json("docs.rust@1.0.0")["citation"], "docs.rust@1.0.0#c1");
        // min_score filters
        let none = idx.query(&q, 5, 0.99).unwrap();
        assert!(none.is_empty());
        // a chunk's own embedding finds itself with score ~1
        let own = HashEmbedder::default().embed_one(&idx.chunks[1].text);
        let top = idx.query(&own, 1, 0.0).unwrap();
        assert_eq!(top[0].chunk_id, "c2");
        assert!((top[0].score - 1.0).abs() < 0.01);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_wrong_dimension_query_is_refused() {
        let (home, dir) = kms();
        let m = Manifest::from_file(&dir.join("km.toml")).unwrap();
        let idx = MemIndex::load(&dir, &m).unwrap();
        let e = idx.query(&[1.0, 2.0], 1, 0.0).unwrap_err();
        assert!(e.message.contains("2 dimensions"), "{}", e.message);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn damaged_memory_files_are_refused() {
        let (home, dir) = kms();
        let m = Manifest::from_file(&dir.join("km.toml")).unwrap();
        // embeddings of the wrong length
        let emb = dir.join("payload/emb.f16");
        let good = std::fs::read(&emb).unwrap();
        std::fs::write(&emb, &good[..good.len() - 2]).unwrap();
        let e = MemIndex::load(&dir, &m).err().unwrap();
        assert!(e.message.contains("need"), "{}", e.message);
        std::fs::write(&emb, good).unwrap();
        // a duplicated chunk id
        let ch = dir.join("payload/chunks.jsonl");
        std::fs::write(
            &ch,
            "{\"id\":\"a\",\"text\":\"x\"}\n{\"id\":\"a\",\"text\":\"y\"}\n{\"id\":\"b\",\"text\":\"z\"}\n",
        )
        .unwrap();
        assert!(MemIndex::load(&dir, &m).err().unwrap().message.contains("twice"));
        std::fs::write(&ch, "{\"text\":\"x\"}\n").unwrap();
        assert!(MemIndex::load(&dir, &m).err().unwrap().message.contains("string id"));
        let _ = std::fs::remove_dir_all(&home);
    }
}

//! Step 2, gather: the task's sources become chunks with their origin, and every chunk gets an embedding.
//! Gathering is re-run from scratch if it was interrupted (it is cheap); embedding resumes where it stopped.

use serde_json::{json, Value};

use super::gather::{gather, Limits};
use super::run::{Halt, Run, StepRes};
use super::StepEnd;

const EMBED_BATCH: usize = 16;

pub async fn run(run: &Run) -> StepRes<StepEnd> {
    let s = run.f.settings();
    let mut chunks = run.f.db.chunks(&run.id);
    if chunks.is_empty() {
        let limits = Limits {
            chunk_chars: s.chunk_chars,
            max_file_bytes: s.max_file_bytes,
            max_files: s.max_files,
            max_total_bytes: s.max_total_bytes,
            max_url_bytes: s.max_url_bytes,
        };
        run.progress(0, 1, "reading the sources");
        let cancelled = || run.check().is_err();
        let g = match gather(
            &run.hub.engine,
            &run.km,
            &run.spec.sources,
            &limits,
            run.spec.license.as_deref(),
            &cancelled,
        )
        .await
        {
            Ok(g) => g,
            Err(f) if f.code == "cancelled" => return Err(Halt::Cancelled),
            Err(f) => return Err(Halt::Fail(f)),
        };
        if g.chunks.is_empty() {
            return Err(Halt::Fail(crate::engine::Fail::invalid(
                "the sources gave no text (see the log for what was skipped)",
            )));
        }
        for n in &g.notes {
            run.log("warn", n);
        }
        run.log(
            "info",
            &format!("{} chunks from {} files, {} bytes", g.chunks.len(), g.files, g.bytes),
        );
        run.f.db.replace_chunks(&run.id, &g.chunks)?;
        chunks = run.f.db.chunks(&run.id);
    }
    // embeddings, a batch at a time, kept as they come
    let missing: Vec<(String, String)> = chunks
        .iter()
        .filter(|c| c.emb.is_none())
        .map(|c| (c.id.clone(), c.text.clone()))
        .collect();
    let total = chunks.len() as u64;
    let mut done = total - missing.len() as u64;
    for batch in missing.chunks(EMBED_BATCH) {
        run.check()?;
        let texts: Vec<String> = batch.iter().map(|b| b.1.clone()).collect();
        let embs = run.embed(&texts).await?;
        let rows: Vec<(String, Vec<f32>)> = batch.iter().map(|b| b.0.clone()).zip(embs).collect();
        run.f.db.set_chunk_embs(&run.id, &rows)?;
        done += batch.len() as u64;
        run.progress(done, total, &format!("embedded {done} of {total} chunks"));
    }
    let chunks = run.f.db.chunks(&run.id);
    let mut files: Vec<String> = chunks
        .iter()
        .map(|c| {
            c.origin["path"]
                .as_str()
                .or(c.origin["url"].as_str())
                .or(c.origin["km"].as_str())
                .or(c.origin["name"].as_str())
                .or(c.origin["trace"].as_str())
                .unwrap_or("")
                .to_string()
        })
        .collect();
    files.sort();
    files.dedup();
    let tools: Vec<Value> = run.tools().iter().map(|t| json!(t.name)).collect();
    Ok(StepEnd::Done(
        json!({"chunks": chunks.len(), "origins": files.len(), "tools": tools,
                            "embed_model": run.embedder().model_id()}),
    ))
}

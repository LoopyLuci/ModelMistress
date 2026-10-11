//! Step 7, evaluate: the base model answers held-out questions with and without the task's KMs (notes retrieved from
//! the memory KM, the skill's instructions, similar training examples, the tools' descriptions), and the reviewer
//! model grades both against the record's reference answer with the fixed rubric in `prompts/judge.txt`. The uplift
//! (with minus without, on a 0 to 1 scale) is written into each KM's `[quality]` by a new patch version, because a
//! published version never changes.

use std::collections::BTreeMap;

use serde_json::json;

use super::build::{read_files, FileSpec};
use super::run::{Role, Run, StepRes};
use super::store::{ChunkRow, RecordRow};
use super::util::{fence, prompt, truncate};
use super::verify::user_text;
use super::StepEnd;
use crate::engine::Fail;
use crate::km::embed::cosine;
use crate::km::sha256_hex;

/// Notes the KMs would put in the context for this question.
fn notes_for(
    q_emb: &[f32],
    docs: &[ChunkRow],
    train: &[RecordRow],
    skill: Option<&str>,
    tools: &[String],
    k: usize,
) -> String {
    let mut parts: Vec<String> = vec![];
    if let Some(s) = skill {
        parts.push(format!("Skill instructions:\n{}", truncate(s, 1200)));
    }
    let mut ranked: Vec<(f32, &ChunkRow)> = docs
        .iter()
        .filter_map(|c| c.emb.as_ref().map(|e| (cosine(e, q_emb), c)))
        .collect();
    ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    for (_, c) in ranked.into_iter().take(k) {
        parts.push(format!("Note:\n{}", truncate(&c.text, 1200)));
    }
    let mut ex: Vec<(f32, &RecordRow)> = train
        .iter()
        .filter_map(|r| r.emb.as_ref().map(|e| (cosine(e, q_emb), r)))
        .collect();
    ex.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    for (_, r) in ex.into_iter().take(2) {
        parts.push(format!(
            "Example:\nQ: {}\nA: {}",
            truncate(&user_text(&r.json), 400),
            truncate(r.json["output"].as_str().unwrap_or(""), 600)
        ));
    }
    for t in tools.iter().take(3) {
        parts.push(format!("Tool:\n{}", truncate(t, 500)));
    }
    parts.join("\n\n")
}

async fn judge(run: &Run, question: &str, reference: &str, candidate: &str) -> StepRes<(f64, String)> {
    let (rf, cf, qf) = (fence("REFERENCE", reference), fence("ANSWER", candidate), fence("QUESTION", question));
    let (system, user) = prompt(run.home(), "judge", &[("question", &qf), ("reference", &rf), ("candidate", &cf)]);
    let r = run
        .ask_json(Role::Reviewer, "eval:judge", &system, &user, 300, &|v| {
            v["score"].as_f64().map(|_| ()).ok_or_else(|| "score must be a number from 0 to 10".to_string())
        })
        .await?;
    Ok(match r {
        Ok((v, _)) => (v["score"].as_f64().unwrap_or(0.0).clamp(0.0, 10.0), v["reason"].as_str().unwrap_or("").to_string()),
        Err(e) => (0.0, format!("the judge gave no valid score ({e})")),
    })
}

pub async fn run(run: &Run) -> StepRes<StepEnd> {
    let rows = run.f.db.records(&run.id, None);
    let held: Vec<&RecordRow> = rows.iter().filter(|r| r.split == "heldout" && r.status != "rejected").collect();
    let built: Vec<_> = run
        .f
        .db
        .artifacts(&run.id)
        .into_iter()
        .filter(|a| a.stage == "built" && a.imported)
        .collect();
    if built.is_empty() {
        return Ok(StepEnd::Skipped("nothing was built".into()));
    }
    if held.is_empty() {
        return Ok(StepEnd::Skipped(
            "no held-out records (a task needs at least 3 usable records to hold one out)".into(),
        ));
    }
    let docs: Vec<ChunkRow> = if built.iter().any(|a| a.kind == "memory") {
        run.f.db.chunks(&run.id).into_iter().filter(|c| c.role != "tool").collect()
    } else {
        vec![]
    };
    let train: Vec<RecordRow> = if built.iter().any(|a| a.kind == "data") {
        rows.iter().filter(|r| r.split != "heldout" && r.status != "rejected").cloned().collect()
    } else {
        vec![]
    };
    let skill_text = if built.iter().any(|a| a.kind == "skill") {
        run.f.db.chunks(&run.id).into_iter().find(|c| c.role == "skill").map(|c| c.text)
    } else {
        None
    };
    let tool_texts: Vec<String> = if built.iter().any(|a| a.kind == "tool") {
        run.f.db.chunks(&run.id).into_iter().filter(|c| c.role == "tool").map(|c| c.text).collect()
    } else {
        vec![]
    };
    let k = run.f.settings().eval_notes;
    let done: std::collections::HashSet<String> = run.f.db.evals(&run.id).into_iter().map(|e| e.0).collect();
    let total = held.len() as u64;
    let mut n = done.len() as u64;
    for r in &held {
        if done.contains(&r.id) {
            continue;
        }
        run.check()?;
        let q = user_text(&r.json);
        let reference = r.json["output"].as_str().unwrap_or("").to_string();
        let q_emb = run.embed(std::slice::from_ref(&q)).await?.remove(0);
        let (system, user) = prompt(run.home(), "answer", &[("question", &q)]);
        let without = run.ask(Role::Base, "eval:without", &system, &user, 500).await?;
        let notes = notes_for(&q_emb, &docs, &train.iter().filter(|t| t.id != r.id).cloned().collect::<Vec<_>>(), skill_text.as_deref(), &tool_texts, k);
        let nf = fence("NOTES FROM KNOWLEDGE MODULES", &notes);
        let (system, user) = prompt(run.home(), "answer_with_km", &[("notes", &nf), ("question", &q)]);
        let with = run.ask(Role::Base, "eval:with", &system, &user, 500).await?;
        let (s0, why0) = judge(run, &q, &reference, &without.text).await?;
        let (s1, why1) = judge(run, &q, &reference, &with.text).await?;
        run.f.db.put_eval(
            &run.id,
            &r.id,
            &json!({"question": q, "without": {"answer": truncate(&without.text, 1500), "score": s0, "reason": why0, "trace": without.trace},
                    "with": {"answer": truncate(&with.text, 1500), "score": s1, "reason": why1, "trace": with.trace},
                    "notes_chars": notes.len()}),
        )?;
        n += 1;
        run.progress(n, total, &format!("evaluated {n} of {total} held-out records"));
    }
    let evals = run.f.db.evals(&run.id);
    let cnt = evals.len().max(1) as f64;
    let mean = |side: &str| evals.iter().map(|e| e.1[side]["score"].as_f64().unwrap_or(0.0)).sum::<f64>() / cnt / 10.0;
    let (with_m, without_m) = (mean("with"), mean("without"));
    let uplift = ((with_m - without_m) * 1000.0).round() / 1000.0;
    let scores: BTreeMap<String, f64> = BTreeMap::from([
        ("uplift".to_string(), uplift),
        ("score_with_km".to_string(), (with_m * 1000.0).round() / 1000.0),
        ("score_without_km".to_string(), (without_m * 1000.0).round() / 1000.0),
        ("n_eval".to_string(), evals.len() as f64),
    ]);
    // the held-out set travels with the data KM
    let eval_jsonl: String = held
        .iter()
        .map(|r| {
            format!(
                "{}\n",
                json!({"id": r.id, "input": r.json["input"], "output": r.json["output"]})
            )
        })
        .collect();
    let existing = run.f.db.artifacts(&run.id);
    let mut versions = vec![];
    for a in built {
        if existing.iter().any(|x| x.id == a.id && x.stage == "evaluated" && x.imported) {
            continue;
        }
        if a.kind == "adapter" {
            continue; // its uplift needs the adapter attached; the Forge evaluates it
        }
        let mut d: super::build::Draft = serde_json::from_value(a.draft.clone())
            .map_err(|e| super::run::Halt::Fail(Fail::new(500, "internal", format!("{}: {e}", a.id))))?;
        let old_dir = run.dir().join("build").join(d.key());
        let mut files = read_files(&d, &old_dir)?;
        let v = semver::Version::parse(&d.version).unwrap_or(semver::Version::new(0, 1, 0));
        d.parent = Some(d.key());
        d.version = format!("{}.{}.{}", v.major, v.minor, v.patch + 1);
        d.scores = scores.clone();
        if d.kind == "data" {
            d.eval_set = Some("evals/heldout.jsonl".into());
            d.eval_hash = Some(format!("sha256:{}", sha256_hex(eval_jsonl.as_bytes())));
            d.files.push(FileSpec {
                path: "evals/heldout.jsonl".into(),
                role: "eval".into(),
                format: "jsonl".into(),
            });
            files.insert("evals/heldout.jsonl".into(), eval_jsonl.clone().into_bytes());
        }
        d.created = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let r = super::build::publish_version(run, d, files, "evaluated").await?;
        versions.push(r);
    }
    Ok(StepEnd::Done(json!({"evaluated": evals.len(), "uplift": uplift, "score_with_km": with_m,
                            "score_without_km": without_m, "versions": versions})))
}

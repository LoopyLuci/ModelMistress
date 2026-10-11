//! Step 4, verify. A reviewer model checks each record against the chunks it cites (supported, partial, unsupported,
//! with a reason); executable checks run where a program can decide; then records are deduplicated (normalised text
//! hash, then embedding similarity), scored, checked against evaluation sets so test data never reaches training
//! data, and split into train and held-out. Doubtful records go to the review queue.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use super::checks::CheckInput;
use super::run::{Role, Run, StepRes};
use super::store::{ChunkRow, RecordRow};
use super::util::{fence, prompt, text_hash, truncate};
use super::StepEnd;
use crate::km::embed::cosine;

/// The user's side of a record.
pub fn user_text(rec: &Value) -> String {
    rec["input"]
        .as_array()
        .and_then(|a| a.iter().rev().find(|m| m["role"] == "user"))
        .and_then(|m| m["content"].as_str())
        .unwrap_or("")
        .to_string()
}

/// Evaluation material records must not duplicate.
#[derive(Default, Debug)]
pub struct EvalSet {
    pub ids: HashSet<String>,
    pub hashes: HashSet<String>,
    pub texts: Vec<String>,
}

/// Reads the task's `eval_sets`: `{path}` (JSONL; `id` and the first of `input`, `question`, `prompt`, `text`,
/// `query` per line), `{ids}`, `{hashes}`.
pub fn load_eval_sets(sets: &[Value]) -> Result<EvalSet, String> {
    let mut out = EvalSet::default();
    for (i, s) in sets.iter().enumerate() {
        for id in s["ids"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            out.ids.insert(id.to_string());
        }
        for h in s["hashes"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            out.hashes.insert(h.to_string());
        }
        if let Some(p) = s["path"].as_str() {
            let text = std::fs::read_to_string(p).map_err(|e| format!("eval_sets[{i}] {p}: {e}"))?;
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                let v: Value = serde_json::from_str(line).map_err(|e| format!("eval_sets[{i}] {p}: {e}"))?;
                if let Some(id) = v["id"].as_str() {
                    out.ids.insert(id.to_string());
                }
                let q = ["input", "question", "prompt", "text", "query"]
                    .iter()
                    .find_map(|k| match &v[*k] {
                        Value::String(s) => Some(s.clone()),
                        Value::Array(a) => a
                            .iter()
                            .rev()
                            .find(|m| m["role"] == "user")
                            .and_then(|m| m["content"].as_str())
                            .map(str::to_string),
                        _ => None,
                    });
                if let Some(q) = q.filter(|q| !q.trim().is_empty()) {
                    out.hashes.insert(text_hash(&q));
                    out.texts.push(q);
                }
            }
        }
    }
    Ok(out)
}

fn verdict_value(v: &str) -> f64 {
    match v {
        "supported" => 1.0,
        "partial" => 0.5,
        _ => 0.0,
    }
}

/// 0..1: how good a verified record is.
pub fn quality(rec: &Value) -> f64 {
    let v = verdict_value(rec["verify"]["verdict"].as_str().unwrap_or("unsupported"));
    let rev = rec["verify"]["score"].as_f64().unwrap_or(0.5).clamp(0.0, 1.0);
    let len = rec["output"].as_str().unwrap_or("").chars().count();
    let heur = if (20..=4000).contains(&len) { 1.0 } else { 0.3 };
    ((0.5 * v + 0.4 * rev + 0.1 * heur) * 1000.0).round() / 1000.0
}

async fn review_one(run: &Run, row: &RecordRow, chunks: &HashMap<String, ChunkRow>) -> StepRes<RecordRow> {
    let mut row = row.clone();
    let rec = row.json.clone();
    let cited: Vec<&ChunkRow> = rec["source"]["chunks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|c| chunks.get(c))
        .collect();
    // executable checks first: they decide without asking a model
    let tools = run.tools();
    let mut outcomes = vec![];
    let mut failed: Option<String> = None;
    for c in run.f.checks() {
        let o = c.run(&CheckInput {
            record: &rec,
            tools: &tools,
        });
        if o.ran {
            if !o.ok && failed.is_none() {
                failed = Some(format!("check {}: {}", c.name(), o.note));
            }
            outcomes.push(json!({"name": c.name(), "ok": o.ok, "note": o.note}));
        }
    }
    let (model, _) = run.model_of(Role::Reviewer);
    let mut verify = json!({"reviewer": model, "checks": outcomes});
    if let Some(why) = failed {
        verify["verdict"] = json!("unsupported");
        verify["reason"] = json!(why);
        verify["score"] = json!(0.0);
        row.status = "rejected".into();
        row.reason = Some(why);
    } else {
        let blocks = cited
            .iter()
            .map(|c| format!("[{}]\n{}", c.id, truncate(&c.text, 1500)))
            .collect::<Vec<_>>()
            .join("\n\n");
        let answer = rec["output"].as_str().unwrap_or("");
        let calls = rec["tool_calls"].as_array().map(|c| {
            format!(
                "\n(The assistant also called: {})",
                serde_json::to_string(c).unwrap_or_default()
            )
        });
        let out_text = format!("{answer}{}", calls.unwrap_or_default());
        let user = user_text(&rec);
        let fenced = fence("CITED CHUNKS", &blocks);
        let (system, prompt_text) = prompt(
            run.home(),
            "verify",
            &[("chunks", &fenced), ("input", &user), ("output", &out_text)],
        );
        let res = run
            .ask_json(
                Role::Reviewer,
                "verify",
                &system,
                &prompt_text,
                400,
                &|v| match v["verdict"].as_str() {
                    Some("supported" | "partial" | "unsupported") => Ok(()),
                    _ => Err("verdict must be supported, partial or unsupported".to_string()),
                },
            )
            .await?;
        match res {
            Ok((v, reply)) => {
                let verdict = v["verdict"].as_str().unwrap_or("partial").to_string();
                verify["verdict"] = json!(verdict);
                verify["reason"] = json!(v["reason"].as_str().unwrap_or(""));
                verify["score"] = json!(v["score"].as_f64().unwrap_or(0.5).clamp(0.0, 1.0));
                verify["trace"] = json!(reply.trace);
                if verdict == "unsupported" {
                    row.status = "rejected".into();
                    row.reason = Some(format!("unsupported: {}", v["reason"].as_str().unwrap_or("")));
                }
            }
            Err(e) => {
                verify["verdict"] = json!("partial");
                verify["reason"] = json!(format!("the reviewer gave no valid verdict ({e})"));
                verify["score"] = json!(0.3);
                verify["unreviewed"] = json!(true);
            }
        }
    }
    row.json["verify"] = verify;
    row.verified = true;
    Ok(row)
}

pub async fn run(run: &Run) -> StepRes<StepEnd> {
    let s = run.f.settings();
    let chunks: HashMap<String, ChunkRow> = run
        .f
        .db
        .chunks(&run.id)
        .into_iter()
        .map(|c| (c.id.clone(), c))
        .collect();

    // A: one review per record, each kept as it is made
    let todo: Vec<RecordRow> = run
        .f
        .db
        .records(&run.id, Some("pending"))
        .into_iter()
        .filter(|r| !r.verified)
        .collect();
    let total = run.f.db.record_count(&run.id) as u64;
    let mut done = total - todo.len() as u64;
    for r in todo {
        run.check()?;
        let updated = review_one(run, &r, &chunks).await?;
        run.f.db.put_record(&run.id, &updated)?;
        done += 1;
        run.progress(done, total, &format!("reviewed {done} of {total}"));
    }

    // B: embeddings, leak check, duplicates, quality, split and the review queue (pure bookkeeping)
    let rows: Vec<RecordRow> = run
        .f
        .db
        .records(&run.id, None)
        .into_iter()
        .filter(|r| r.status != "rejected" && r.verified && r.json["verify"]["final"] != true)
        .collect();
    let mut counts = json!({"reviewed": total, "supported": 0, "partial": 0, "unsupported": 0, "check_failed": 0,
                            "duplicates": 0, "leaks": 0, "accepted": 0, "queued": 0, "heldout": 0});
    let bump = |c: &mut Value, k: &str| c[k] = json!(c[k].as_i64().unwrap_or(0) + 1);
    for r in run.f.db.records(&run.id, None) {
        match r.json["verify"]["verdict"].as_str() {
            Some("supported") => bump(&mut counts, "supported"),
            Some("partial") => bump(&mut counts, "partial"),
            Some("unsupported") => {
                if r.reason.as_deref().is_some_and(|x| x.starts_with("check ")) {
                    bump(&mut counts, "check_failed");
                } else {
                    bump(&mut counts, "unsupported");
                }
            }
            _ => {}
        }
    }
    if !rows.is_empty() {
        let mut rows = rows;
        // embeddings of the user's side
        let need: Vec<usize> = (0..rows.len()).filter(|i| rows[*i].emb.is_none()).collect();
        for batch in need.chunks(16) {
            let texts: Vec<String> = batch.iter().map(|i| user_text(&rows[*i].json)).collect();
            let embs = run.embed(&texts).await?;
            for (i, e) in batch.iter().zip(embs) {
                rows[*i].emb = Some(e);
            }
        }
        let eval =
            load_eval_sets(&run.spec.eval_sets).map_err(|e| super::run::Halt::Fail(crate::engine::Fail::invalid(e)))?;
        let eval_embs: Vec<Vec<f32>> = if eval.texts.is_empty() {
            vec![]
        } else {
            run.embed(&eval.texts[..eval.texts.len().min(2000)]).await?
        };
        // what is already kept (records finalised in an earlier run)
        let mut kept: Vec<(String, String, Vec<f32>)> = run
            .f
            .db
            .records(&run.id, None)
            .into_iter()
            .filter(|r| r.status != "rejected" && r.json["verify"]["final"] == true)
            .map(|r| (r.id.clone(), r.hash.clone(), r.emb.clone().unwrap_or_default()))
            .collect();
        for r in rows.iter_mut() {
            r.score = quality(&r.json);
            r.json["labels"]["quality"] = json!(r.score);
            r.hash = text_hash(&user_text(&r.json));
        }
        let mut order: Vec<usize> = (0..rows.len()).collect();
        order.sort_by(|a, b| {
            rows[*b]
                .score
                .partial_cmp(&rows[*a].score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(rows[*a].ord.cmp(&rows[*b].ord))
        });
        let mut changed: Vec<usize> = vec![];
        for i in order {
            let user = user_text(&rows[i].json);
            let out_hash = text_hash(rows[i].json["output"].as_str().unwrap_or(""));
            let emb = rows[i].emb.clone().unwrap_or_default();
            // test data must not leak into training data
            let leak = if eval.ids.contains(&rows[i].id)
                || eval.hashes.contains(&rows[i].hash)
                || eval.hashes.contains(&out_hash)
            {
                Some("matches an evaluation set entry exactly".to_string())
            } else {
                eval_embs
                    .iter()
                    .map(|e| cosine(e, &emb))
                    .fold(0.0f32, f32::max)
                    .ge(&(s.leak_threshold as f32))
                    .then(|| "is nearly identical to an evaluation question".to_string())
            };
            if let Some(why) = leak {
                reject(&mut rows[i], format!("leak: {why}"), "leak");
                bump(&mut counts, "leaks");
                changed.push(i);
                continue;
            }
            let dup = kept.iter().find(|(_, h, e)| {
                *h == rows[i].hash || (!e.is_empty() && cosine(e, &emb) >= s.dedupe_threshold as f32)
            });
            if let Some((other, _, _)) = dup {
                let why = format!("duplicate of {other}");
                reject(&mut rows[i], why, "duplicate");
                bump(&mut counts, "duplicates");
                changed.push(i);
                continue;
            }
            kept.push((rows[i].id.clone(), rows[i].hash.clone(), emb));
            let _ = user;
            let verdict = rows[i].json["verify"]["verdict"]
                .as_str()
                .unwrap_or("partial")
                .to_string();
            let clean = verdict == "supported" && rows[i].score >= s.review_threshold;
            let review = run.spec.tagging.review.as_str();
            let (status, queue) = match (review, clean) {
                ("all", _) => ("pending", Some("review of every record was asked for".to_string())),
                ("auto", true) | ("none", true) => ("approved", None),
                ("auto", false) => (
                    "pending",
                    Some(format!(
                        "{verdict} support, quality {:.2} (below {:.2} or not fully supported)",
                        rows[i].score, s.review_threshold
                    )),
                ),
                _ => ("pending", None),
            };
            rows[i].status = status.into();
            rows[i].json["status"] = json!(status);
            rows[i].json["verify"]["final"] = json!(true);
            bump(&mut counts, "accepted");
            if let Some(why) = queue {
                let data = json!({"verdict": verdict, "score": rows[i].score, "input": user_text(&rows[i].json),
                                  "output": rows[i].json["output"], "reason": rows[i].json["verify"]["reason"]});
                run.f.db.add_review(&run.id, "record", &rows[i].id, &why, &data)?;
                bump(&mut counts, "queued");
            }
            changed.push(i);
        }
        let out: Vec<RecordRow> = changed.iter().map(|i| rows[*i].clone()).collect();
        run.f.db.put_records(&run.id, &out)?;
    }
    // split: a tenth of the accepted records (at least one, from three on) is held out for the evaluation
    let mut all = run.f.db.records(&run.id, None);
    all.retain(|r| r.status != "rejected");
    if all.len() >= 3 && !all.iter().any(|r| r.split == "heldout") {
        let mut by_hash: Vec<(String, usize)> = all
            .iter()
            .enumerate()
            .map(|(i, r)| (crate::km::sha256_hex(r.id.as_bytes()), i))
            .collect();
        by_hash.sort();
        let n = (all.len() / 10).max(1);
        let picked: Vec<RecordRow> = by_hash
            .iter()
            .take(n)
            .map(|(_, i)| {
                let mut r = all[*i].clone();
                r.split = "heldout".into();
                r.json["split"] = json!("heldout");
                r
            })
            .collect();
        run.f.db.put_records(&run.id, &picked)?;
    }
    counts["heldout"] = json!(run
        .f
        .db
        .records(&run.id, None)
        .iter()
        .filter(|r| r.split == "heldout" && r.status != "rejected")
        .count());
    counts["rejected_total"] = json!(run.f.db.records(&run.id, Some("rejected")).len());
    Ok(StepEnd::Done(counts))
}

fn reject(r: &mut RecordRow, why: String, kind: &str) {
    r.status = "rejected".into();
    r.reason = Some(why.clone());
    r.json["status"] = json!("rejected");
    r.json["verify"]["final"] = json!(true);
    r.json["verify"]["rejected_by"] = json!(kind);
    r.json["verify"]["reason"] = json!(why);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_weighs_support_the_reviewer_and_length() {
        let good =
            json!({"output": "A complete answer of decent length.", "verify": {"verdict": "supported", "score": 0.9}});
        let part =
            json!({"output": "A complete answer of decent length.", "verify": {"verdict": "partial", "score": 0.9}});
        let short = json!({"output": "no", "verify": {"verdict": "supported", "score": 0.9}});
        assert!(quality(&good) > quality(&part) && quality(&good) > quality(&short));
        assert!(quality(&good) > 0.9 && quality(&json!({"verify": {"verdict": "unsupported", "score": 0.0}})) < 0.2);
    }

    #[test]
    fn eval_sets_come_from_files_ids_and_hashes() {
        let d = crate::testing::temp_home();
        let p = d.join("eval.jsonl");
        std::fs::write(
            &p,
            "{\"id\":\"e1\",\"question\":\"What is a Future?\"}\n{\"input\":[{\"role\":\"user\",\"content\":\"How do I join tasks?\"}]}\n",
        )
        .unwrap();
        let e = load_eval_sets(&[json!({"path": p}), json!({"ids": ["x"]}), json!({"hashes": ["abc"]})]).unwrap();
        assert!(e.ids.contains("e1") && e.ids.contains("x") && e.hashes.contains("abc"));
        assert!(
            e.hashes.contains(&text_hash("what is a future")) && e.hashes.contains(&text_hash("How do I join tasks"))
        );
        assert_eq!(e.texts.len(), 2);
        assert!(load_eval_sets(&[json!({"path": d.join("none.jsonl")})]).is_err());
        let _ = std::fs::remove_dir_all(d);
    }
}

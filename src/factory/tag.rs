//! Step 5, tag. Three sources, each with a confidence: a rule/keyword classifier driven by the vocabulary (and by what
//! the owner's earlier review decisions taught it), the tags of the nearest existing KMs by embedding, and the author
//! model's suggestions constrained to the vocabulary. They are combined; a tag the combination is sure of is
//! attached, an uncertain one goes to the review queue, and a value the vocabulary does not have is proposed (and
//! added only if the factory's policy says so).

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::run::{Role, Run, StepRes};
use super::store::RecordRow;
use super::util::{fence, prompt, words};
use super::verify::user_text;
use super::StepEnd;
use crate::km::embed::{cosine, f16_to_f32};
use crate::km::vocab::Vocabulary;

pub const WEIGHTS: [(&str, f64); 3] = [("classifier", 0.9), ("nearest", 0.7), ("author_model", 0.8)];

type Cands = Vec<(String, f64, &'static str)>;

fn has_word(text: &str, w: &str) -> bool {
    let mut from = 0;
    while let Some(i) = text[from..].find(w) {
        let a = from + i;
        let b = a + w.len();
        let before = text[..a].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        let after = text[b..].chars().next().is_none_or(|c| !c.is_alphanumeric());
        if before && after {
            return true;
        }
        from = b;
        if from >= text.len() {
            break;
        }
    }
    false
}

const TASK_WORDS: [(&str, &[&str]); 8] = [
    ("codegen", &["write a function", "implement", "write code", "fn ", "def ", "```", "snippet", "example code"]),
    ("debug", &["error", "bug", "fix", "panic", "exception", "fails", "failing", "not working"]),
    ("explain", &["explain", "what is", "what does", "why ", "how does", "meaning of", "difference between"]),
    ("summarise", &["summarise", "summarize", "summary", "tl;dr"]),
    ("translate", &["translate", "translation"]),
    ("plan", &["plan", "roadmap", "outline", "steps to", "strategy"]),
    ("extract", &["extract", "list all", "pull out"]),
    ("chat", &["hello", "thanks", "help me"]),
];
const LANGS: [&str; 14] = [
    "rust", "python", "javascript", "typescript", "go", "java", "kotlin", "swift", "sql", "bash", "c++", "c#", "ruby",
    "haskell",
];
const SECURITY: [&str; 6] = ["password", "secret key", "exploit", "vulnerability", "api key", "credential"];

fn looks_like_pii(text: &str) -> bool {
    let is_email = |w: &str| {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric());
        match w.split_once('@') {
            Some((l, d)) => {
                !l.is_empty()
                    && l.chars().all(|c| c.is_ascii_alphanumeric() || "._%+-".contains(c))
                    && d.contains('.')
                    && d.chars().all(|c| c.is_ascii_alphanumeric() || ".-".contains(c))
                    && d.rsplit('.').next().is_some_and(|t| t.len() >= 2 && t.chars().all(|c| c.is_ascii_alphabetic()))
            }
            None => false,
        }
    };
    let mut run = 0;
    let mut longest = 0;
    for c in text.chars() {
        if c.is_ascii_digit() {
            run += 1;
            longest = longest.max(run);
        } else if c != ' ' && c != '-' {
            run = 0;
        }
    }
    text.split_whitespace().any(is_email) || longest >= 9
}

/// Rule-based tags with confidence. `plan_tags` are hints from the plan; `feedback` is what review decisions taught.
pub fn rule_tags(
    rec: &Value,
    vocab: &Vocabulary,
    plan_tags: &[String],
    licenses: &[String],
    feedback: &[(String, f64)],
) -> Cands {
    let text = format!("{} {}", user_text(rec), rec["output"].as_str().unwrap_or("")).to_lowercase();
    let mut out: Cands = vec![
        ("modality:text".into(), 0.95, "classifier"),
        ("quality:synthetic".into(), 1.0, "classifier"),
    ];
    let tool_calls = rec["tool_calls"].as_array().cloned().unwrap_or_default();
    for (task, ws) in TASK_WORDS {
        let hits = ws.iter().filter(|w| text.contains(*w)).count();
        if hits > 0 {
            out.push((format!("task:{task}"), (0.45 + 0.15 * hits as f64).min(0.9), "classifier"));
        }
    }
    if !tool_calls.is_empty() {
        out.push(("task:tool-use".into(), 0.95, "classifier"));
        for c in &tool_calls {
            if let Some(n) = c["name"].as_str() {
                out.push((format!("tool:{}", super::util::slug(n).replace('.', "-")), 0.95, "classifier"));
            }
        }
    }
    for l in LANGS {
        if has_word(&text, l) {
            out.push((format!("lang:{}", l.replace("c++", "cpp").replace("c#", "csharp")), 0.7, "classifier"));
        }
    }
    if has_word(&text, "the") && has_word(&text, "and") {
        out.push(("lang:en".into(), 0.6, "classifier"));
    }
    for t in plan_tags {
        if let Some((_, v)) = t.split_once(':') {
            let seen = has_word(&text, &v.to_lowercase());
            out.push((t.clone(), if seen { 0.75 } else { 0.3 }, "classifier"));
        }
    }
    // other closed namespaces of the vocabulary: a value whose words all appear
    for (ns, n) in &vocab.namespaces {
        if matches!(ns.as_str(), "task" | "lang" | "lib" | "modality" | "skill" | "tool" | "source" | "license" | "quality" | "risk")
            || n.open
        {
            continue;
        }
        for v in &n.values {
            if v.split('-').all(|p| has_word(&text, p)) {
                out.push((format!("{ns}:{v}"), 0.6, "classifier"));
            }
        }
    }
    let risk = if looks_like_pii(&text) {
        ("risk:pii", 0.7)
    } else if SECURITY.iter().any(|w| text.contains(w)) {
        ("risk:security", 0.6)
    } else {
        ("risk:none", 0.7)
    };
    out.push((risk.0.into(), risk.1, "classifier"));
    for l in licenses {
        out.push((format!("license:{}", l.to_lowercase().replace(' ', "-")), 0.9, "classifier"));
    }
    // what the owner's decisions taught: more or less sure of the tags they approved or refused for these words
    for (tag, n) in feedback {
        let delta = (0.05 * n).clamp(-0.4, 0.3);
        match out.iter_mut().find(|c| &c.0 == tag) {
            Some(c) => c.1 = (c.1 + delta).clamp(0.0, 1.0),
            None if delta > 0.0 => out.push((tag.clone(), 0.3 + delta, "classifier")),
            None => {}
        }
    }
    out
}

/// Combines candidates: each source counts with its weight, several agreeing sources add up.
pub fn combine(cands: &Cands) -> BTreeMap<String, (f64, &'static str)> {
    let mut by: BTreeMap<String, Vec<(f64, &'static str)>> = BTreeMap::new();
    for (t, c, origin) in cands {
        let w = WEIGHTS.iter().find(|(o, _)| o == origin).map_or(0.8, |x| x.1);
        by.entry(t.clone()).or_default().push((c.clamp(0.0, 1.0) * w, origin));
    }
    by.into_iter()
        .map(|(t, v)| {
            let miss: f64 = v.iter().map(|x| 1.0 - x.0).product();
            let best = v.iter().max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            (t, (((1.0 - miss) * 1000.0).round() / 1000.0, best.map_or("classifier", |b| b.1)))
        })
        .collect()
}

fn vocab_text(v: &Vocabulary) -> String {
    v.namespaces
        .iter()
        .map(|(ns, n)| {
            if n.open {
                format!("{ns}: any value ({})", n.description)
            } else {
                format!("{ns}: {}", n.values.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn valid_tag_shape(t: &str) -> bool {
    t.split_once(':').is_some_and(|(n, v)| {
        let ok = |s: &str| !s.is_empty() && s.len() <= 64 && s.chars().all(|c| !c.is_whitespace() && !c.is_control() && c != ':');
        ok(n) && ok(v)
    })
}

pub async fn run(run: &Run) -> StepRes<StepEnd> {
    let s = run.f.settings();
    let mut vocab = run.km.vocabulary()?;
    let mut vocab_changed = false;
    let plan = run.plan()?;
    let plan_tags: Vec<String> = plan["taxonomy"]["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    let embedder = run.embedder();
    let model_id = embedder.model_id();
    // existing KMs, by their centroids (only those made with this embedder are comparable)
    let kms: Vec<(String, Vec<f32>, Vec<String>)> = run
        .km
        .store
        .all(true)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.embed_model.as_deref() == Some(model_id.as_str()))
        .filter_map(|r| r.centroid.as_ref().map(|c| (r.key.clone(), f16_to_f32(c), r.tags.clone())))
        .collect();
    let chunks: std::collections::HashMap<String, super::store::ChunkRow> =
        run.f.db.chunks(&run.id).into_iter().map(|c| (c.id.clone(), c)).collect();

    let todo: Vec<RecordRow> = run
        .f
        .db
        .records(&run.id, None)
        .into_iter()
        .filter(|r| r.status != "rejected" && !r.tagged)
        .collect();
    let total = todo.len() as u64;
    let (mut attached, mut queued, mut proposed, mut done) = (0u64, 0u64, 0u64, 0u64);
    for mut row in todo {
        run.check()?;
        let rec = row.json.clone();
        let licenses: Vec<String> = {
            let mut l: Vec<String> = rec["source"]["origins"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|o| o["license"].as_str().map(str::to_string))
                .collect();
            l.sort();
            l.dedup();
            l
        };
        let _ = &chunks;
        let ws = words(&format!("{} {}", user_text(&rec), rec["output"].as_str().unwrap_or("")));
        let feedback = run.f.db.feedback(&ws);
        let mut cands = rule_tags(&rec, &vocab, &plan_tags, &licenses, &feedback);

        // the nearest existing KMs
        if let Some(emb) = &row.emb {
            let mut near: Vec<(f32, &Vec<String>)> = kms
                .iter()
                .map(|(_, c, tags)| (cosine(emb, c), tags))
                .filter(|(c, _)| *c >= 0.35)
                .collect();
            near.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            for (c, tags) in near.into_iter().take(3) {
                for t in tags.iter().filter(|t| vocab.problem(t).is_none()) {
                    cands.push((t.clone(), (c as f64 * 0.9).min(0.9), "nearest"));
                }
            }
        }

        // the author model, constrained to the vocabulary
        let user = user_text(&rec);
        let vt = fence("VOCABULARY", &vocab_text(&vocab));
        let (system, ptext) = prompt(
            run.home(),
            "tag",
            &[("vocabulary", &vt), ("input", &user), ("output", rec["output"].as_str().unwrap_or(""))],
        );
        let res = run
            .ask_json(Role::Author, "tag", &system, &ptext, 400, &|v| {
                v["tags"].as_array().map(|_| ()).ok_or_else(|| "tags must be a list".to_string())
            })
            .await?;
        let mut proposals: Vec<(String, String)> = vec![];
        if let Ok((v, _)) = res {
            for t in v["tags"].as_array().into_iter().flatten() {
                let (tag, conf) = match t {
                    Value::String(x) => (x.clone(), 0.6),
                    _ => (
                        t["tag"].as_str().unwrap_or("").to_string(),
                        t["confidence"].as_f64().unwrap_or(0.6),
                    ),
                };
                let tag = tag.trim().to_lowercase();
                if !valid_tag_shape(&tag) {
                    continue;
                }
                if vocab.problem(&tag).is_none() {
                    cands.push((tag, conf, "author_model"));
                } else {
                    proposals.push((tag, "suggested by the author model".into()));
                }
            }
            for p in v["propose"].as_array().into_iter().flatten() {
                let tag = p["tag"].as_str().unwrap_or("").trim().to_lowercase();
                if valid_tag_shape(&tag) && vocab.problem(&tag).is_some() {
                    proposals.push((tag, p["reason"].as_str().unwrap_or("proposed by the author model").to_string()));
                }
            }
        }
        // values the vocabulary does not have: proposed, or added when the policy says so
        for (tag, why) in proposals {
            if vocab.problem(&tag).is_none() {
                continue;
            }
            if s.auto_add_vocabulary {
                if let Some((ns, v)) = tag.split_once(':') {
                    if vocab.edit(&json!({"namespace": ns, "add": [v]})).is_ok() {
                        vocab_changed = true;
                        cands.push((tag.clone(), 0.65, "author_model"));
                        run.log("info", &format!("vocabulary: added {tag} (policy auto_add_vocabulary)"));
                    }
                }
            } else {
                run.f.db.add_review(
                    &run.id,
                    "vocab",
                    &tag,
                    &why,
                    &json!({"tag": tag, "example_record": row.id, "reason": why}),
                )?;
                proposed += 1;
            }
        }
        // anything the vocabulary does not know is dropped from the candidates (rule tags can name unlisted values)
        let mut unknown: Vec<String> = vec![];
        cands.retain(|(t, _, _)| {
            let ok = vocab.problem(t).is_none();
            if !ok {
                unknown.push(t.clone());
            }
            ok
        });
        // (rule and plan hints outside the vocabulary are dropped, not proposed: only the author model proposes)
        let _ = unknown;

        let merged = combine(&cands);
        let mut tags: Vec<String> = vec![];
        let mut info = serde_json::Map::new();
        for (tag, (conf, origin)) in &merged {
            if *conf >= s.tag_threshold {
                tags.push(tag.clone());
                info.insert(tag.clone(), json!({"origin": origin, "confidence": conf}));
                attached += 1;
            } else if *conf >= s.tag_floor && run.spec.tagging.review != "none" {
                run.f.db.add_review(
                    &run.id,
                    "tag",
                    &format!("{}|{tag}", row.id),
                    &format!("{tag} with confidence {conf:.2} ({origin}); attached at {:.2}", s.tag_threshold),
                    &json!({"record": row.id, "tag": tag, "confidence": conf, "origin": origin}),
                )?;
                queued += 1;
            }
        }
        row.json["tags"] = json!(tags);
        row.json["tag_info"] = Value::Object(info);
        row.tagged = true;
        run.f.db.put_record(&run.id, &row)?;
        done += 1;
        run.progress(done, total, &format!("tagged {done} of {total}"));
    }
    if vocab_changed {
        vocab.save(&run.km.store.root)?;
    }
    Ok(StepEnd::Done(json!({"records": done, "tags_attached": attached, "tags_queued": queued,
                            "values_proposed": proposed, "vocabulary_changed": vocab_changed,
                            "nearest_kms_compared": kms.len()})))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(input: &str, output: &str) -> Value {
        json!({"input": [{"role": "user", "content": input}], "output": output})
    }

    fn tags_of(c: &Cands) -> Vec<String> {
        combine(c).into_iter().filter(|(_, (conf, _))| *conf >= 0.6).map(|(t, _)| t).collect()
    }

    #[test]
    fn the_classifier_reads_the_vocabulary() {
        let v = Vocabulary::default();
        let c = rule_tags(
            &rec("Explain why this Rust code panics", "The function panics because of an unwrap on None. Fix: ..."),
            &v,
            &[],
            &["Apache-2.0".into()],
            &[],
        );
        let t = tags_of(&c);
        for want in ["modality:text", "quality:synthetic", "lang:rust", "license:apache-2.0", "risk:none"] {
            assert!(t.contains(&want.to_string()), "{want} in {t:?}");
        }
        assert!(combine(&c)["task:explain"].0 > 0.3 && combine(&c).contains_key("task:debug"));
    }

    #[test]
    fn tool_records_get_tool_tags_and_pii_is_flagged() {
        let mut r = rec("mail me at bob@example.org", "ok");
        r["tool_calls"] = json!([{"name": "send_mail", "arguments": {}}]);
        let t = tags_of(&rule_tags(&r, &Vocabulary::default(), &[], &[], &[]));
        assert!(t.contains(&"task:tool-use".to_string()) && t.contains(&"tool:send_mail".replace('_', "-")));
        assert!(t.contains(&"risk:pii".to_string()) || combine(&rule_tags(&r, &Vocabulary::default(), &[], &[], &[]))["risk:pii"].0 > 0.5);
    }

    #[test]
    fn agreeing_sources_add_up_and_one_weak_source_stays_weak() {
        let c: Cands = vec![
            ("a:x".into(), 0.5, "classifier"),
            ("a:x".into(), 0.5, "nearest"),
            ("b:y".into(), 0.5, "author_model"),
        ];
        let m = combine(&c);
        assert!(m["a:x"].0 > m["b:y"].0, "{m:?}");
        assert!(m["b:y"].0 < 0.6 && m["a:x"].0 > 0.6, "{m:?}");
    }

    #[test]
    fn owner_feedback_moves_confidence_both_ways() {
        let v = Vocabulary::default();
        let r = rec("Explain the translate function", "It translates text. Explain more.");
        let plain = combine(&rule_tags(&r, &v, &[], &[], &[]));
        let boosted = combine(&rule_tags(&r, &v, &[], &[], &[("task:translate".into(), 6.0)]));
        let refused = combine(&rule_tags(&r, &v, &[], &[], &[("task:translate".into(), -6.0)]));
        assert!(boosted["task:translate"].0 > plain["task:translate"].0);
        assert!(refused["task:translate"].0 < plain["task:translate"].0);
        let new = combine(&rule_tags(&r, &v, &[], &[], &[("task:plan".into(), 6.0)]));
        assert!(new.contains_key("task:plan"), "a tag the owner approved before is a candidate again");
    }

    #[test]
    fn words_need_word_boundaries() {
        assert!(has_word("a rust program", "rust") && !has_word("trusty", "rust") && has_word("go.", "go"));
    }
}

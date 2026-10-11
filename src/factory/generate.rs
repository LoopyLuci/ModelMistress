//! Step 3, generate: the author model writes records in the data-KM schema (`km-spec.md` section 8) from chunks picked
//! for each subtopic of the plan. Every record cites the chunks it came from; a record without a valid citation is
//! kept only as a rejected one, never trained on. Work is done one model call at a time and each call's records are
//! stored together with the call being marked done, so a crash loses at most the call in flight.

use serde_json::{json, Value};

use super::run::{Halt, Role, Run, StepRes};
use super::store::{ChunkRow, RecordRow};
use super::util::{fence, prompt, truncate};
use super::StepEnd;
use crate::engine::Fail;
use crate::km::embed::cosine;

const KINDS: [(&str, &str); 4] = [
    (
        "qa",
        "A user asks a question the chunks answer; the assistant answers it directly and completely.",
    ),
    (
        "instruction",
        "The user gives an instruction (explain, write, show, compare) and the assistant carries it out using the chunks.",
    ),
    (
        "steps",
        "The user asks how to do something; the assistant gives numbered steps drawn from the chunks.",
    ),
    (
        "counter",
        "The user states a common misconception, or asks for something the chunks do not cover. The assistant corrects the misconception, or says plainly that the material does not cover it, and gives what it does cover.",
    ),
];

/// What kind of record call number `i` asks for.
pub fn kind_for(i: usize, has_tools: bool) -> &'static str {
    if has_tools && i % 3 == 2 {
        "tool_use"
    } else {
        KINDS[i % KINDS.len()].0
    }
}

/// A record the model wrote, checked: `Ok` carries the cleaned fields, `Err` why it is not usable.
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub input: String,
    pub output: String,
    pub cites: Vec<String>,
    pub tool_calls: Vec<Value>,
}

fn clean_cite(c: &str) -> String {
    c.trim()
        .trim_matches(|ch| ch == '[' || ch == ']' || ch == '#' || ch == ' ')
        .to_string()
}

/// Checks one record from the model against the chunk ids that were shown to it.
pub fn check_record(r: &Value, offered: &[String], tool_use: bool) -> Result<Draft, String> {
    let text = |k: &str| r[k].as_str().map(|s| s.trim().to_string()).unwrap_or_default();
    let (input, output) = (text("input"), text("output"));
    if input.chars().count() < 6 {
        return Err("input is missing or too short".into());
    }
    if output.chars().count() < 10 {
        return Err("output is missing or too short".into());
    }
    let cites: Vec<String> = match &r["cites"] {
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_str)
            .map(clean_cite)
            .filter(|c| !c.is_empty())
            .collect(),
        Value::String(s) => s.split(',').map(clean_cite).filter(|c| !c.is_empty()).collect(),
        _ => vec![],
    };
    if cites.is_empty() {
        return Err("cites must list the chunk ids the record is based on".into());
    }
    if let Some(bad) = cites.iter().find(|c| !offered.contains(c)) {
        return Err(format!(
            "cites {bad:?}, which is not one of the chunks shown ({})",
            offered.join(", ")
        ));
    }
    let mut tool_calls = vec![];
    if tool_use {
        let calls = r["tool_calls"].as_array().filter(|c| !c.is_empty());
        let Some(calls) = calls else {
            return Err("a tool-use record needs tool_calls".into());
        };
        for c in calls {
            let name = c["name"].as_str().unwrap_or("");
            if name.is_empty() {
                return Err("every tool call needs a name".into());
            }
            let args = match &c["arguments"] {
                Value::Object(_) => c["arguments"].clone(),
                Value::Null => json!({}),
                // some models send the arguments as a JSON string
                Value::String(s) => serde_json::from_str::<Value>(s)
                    .ok()
                    .filter(Value::is_object)
                    .ok_or("tool call arguments must be an object")?,
                _ => return Err("tool call arguments must be an object".into()),
            };
            tool_calls.push(json!({"name": name, "arguments": args}));
        }
    }
    let mut cites = cites;
    cites.dedup();
    Ok(Draft {
        input,
        output,
        cites,
        tool_calls,
    })
}

fn chunk_block(c: &ChunkRow) -> String {
    let at = if let Some(p) = c.origin["path"].as_str() {
        format!("{p} lines {}-{}", c.origin["line_start"], c.origin["line_end"])
    } else if let Some(u) = c.origin["url"].as_str() {
        u.to_string()
    } else if let Some(n) = c.origin["name"].as_str() {
        format!("tool {n}")
    } else {
        c.origin["kind"].as_str().unwrap_or("source").to_string()
    };
    format!("[{}] ({at})\n{}", c.id, truncate(&c.text, 1500))
}

/// The record as it is stored and later written to the data KM.
#[allow(clippy::too_many_arguments)]
pub fn make_record(
    id: &str,
    d: &Draft,
    kind: &str,
    sub: &Value,
    model: &str,
    run_ids: (&str, &str),
    trace: Option<&str>,
    chunks: &[&ChunkRow],
) -> Value {
    let origins: Vec<Value> = d
        .cites
        .iter()
        .filter_map(|c| chunks.iter().find(|x| &x.id == c))
        .map(|c| {
            let mut o = c.origin.clone();
            if let Some(obj) = o.as_object_mut() {
                obj.remove("def");
            }
            o
        })
        .collect();
    let mut r = json!({
        "id": id,
        "task": if kind == "tool_use" { "tool_use" } else { "chat" },
        "input": [{"role": "user", "content": d.input}],
        "output": d.output,
        "labels": {"kind": kind, "subtopic": sub["name"]},
        "tags": [],
        "source": {"kind": "teacher", "model": model, "trace": trace, "factory_task": run_ids.0, "job": run_ids.1,
                   "chunks": d.cites, "origins": origins},
        "split": "train", "status": "pending", "rev": 1
    });
    if !d.tool_calls.is_empty() {
        r["tool_calls"] = json!(d.tool_calls);
    }
    r
}

pub fn row(ord: i64, id: &str, json: Value, status: &str, reason: Option<String>) -> RecordRow {
    RecordRow {
        id: id.to_string(),
        ord,
        json,
        status: status.to_string(),
        reason,
        verified: status == "rejected",
        tagged: false,
        hash: String::new(),
        score: 0.0,
        split: "train".into(),
        emb: None,
    }
}

pub async fn run(run: &Run) -> StepRes<StepEnd> {
    let plan = run.plan()?;
    let subs = plan["subtopics"].as_array().cloned().unwrap_or_default();
    if subs.is_empty() {
        return Err(Halt::Fail(Fail::invalid("the plan has no subtopics")));
    }
    let all = run.f.db.chunks(&run.id);
    let docs: Vec<&ChunkRow> = {
        let d: Vec<&ChunkRow> = all.iter().filter(|c| c.role != "tool").collect();
        if d.is_empty() {
            all.iter().collect()
        } else {
            d
        }
    };
    let tool_chunks: Vec<&ChunkRow> = all.iter().filter(|c| c.role == "tool").collect();
    let tools = run.tools();
    let target = run.spec.target_records() as i64;
    let per_call = run.spec.per_call as i64;
    let (model, _) = run.model_of(Role::Author);

    let texts: Vec<String> = subs
        .iter()
        .map(|s| {
            format!(
                "{} {} {}",
                s["name"].as_str().unwrap_or(""),
                s["description"].as_str().unwrap_or(""),
                s["questions"]
                    .as_array()
                    .map(|q| q.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))
                    .unwrap_or_default()
            )
        })
        .collect();
    let sub_embs = run.embed(&texts).await?;

    let max_calls = target * 3 / per_call.max(1) + 12;
    let mut streak = 0;
    let (mut rejected, mut calls_ok) = (0i64, 0i64);
    loop {
        run.check()?;
        let usable = run.f.db.usable_count(&run.id);
        run.progress(
            usable.max(0) as u64,
            target as u64,
            &format!("{usable} of {target} records"),
        );
        if usable >= target {
            break;
        }
        let (idx, item) = match run.f.db.next_work(&run.id, "generate") {
            Some(w) => w,
            None => {
                let (p, d, f) = run.f.db.work_counts(&run.id, "generate");
                let total = p + d + f;
                if total >= max_calls {
                    run.log(
                        "warn",
                        &format!("stopped after {total} model calls with {usable} of {target} records"),
                    );
                    break;
                }
                let i = total as usize;
                let item = json!({"subtopic": i % subs.len(), "round": i / subs.len(), "kind": kind_for(i, !tools.is_empty())});
                run.f.db.add_work(&run.id, "generate", total, &item)?;
                (total, item)
            }
        };
        let si = item["subtopic"].as_u64().unwrap_or(0) as usize % subs.len();
        let round = item["round"].as_u64().unwrap_or(0) as usize;
        let kind = item["kind"].as_str().unwrap_or("qa").to_string();
        let tool_use = kind == "tool_use";
        let sub = &subs[si];

        // chunks for this call: the most relevant to the subtopic, a different window each round
        let mut ranked: Vec<&ChunkRow> = docs.to_vec();
        ranked.sort_by(|a, b| {
            let sa = a.emb.as_ref().map_or(0.0, |e| cosine(e, &sub_embs[si]));
            let sb = b.emb.as_ref().map_or(0.0, |e| cosine(e, &sub_embs[si]));
            sb.partial_cmp(&sa)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.ord.cmp(&b.ord))
        });
        let want = if tool_use { 2 } else { 4 }.min(ranked.len());
        let start = (round * 2) % ranked.len().max(1);
        let mut picked: Vec<&ChunkRow> = (0..want).map(|k| ranked[(start + k) % ranked.len()]).collect();
        if tool_use {
            picked.splice(0..0, tool_chunks.iter().take(4).copied());
        }
        let offered: Vec<String> = picked.iter().map(|c| c.id.clone()).collect();
        let usable_now = run.f.db.usable_count(&run.id);
        let count = per_call.min(target - usable_now).max(1);
        let guide = KINDS.iter().find(|k| k.0 == kind).map_or("", |k| k.1);
        let blocks = picked.iter().map(|c| chunk_block(c)).collect::<Vec<_>>().join("\n\n");
        let tools_text = tools
            .iter()
            .map(|t| {
                format!(
                    "- {}: {}\n  schema: {}",
                    t.name,
                    truncate(&t.description, 200),
                    serde_json::to_string(&t.input_schema).unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let questions = sub["questions"]
            .as_array()
            .map(|q| q.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" | "))
            .unwrap_or_default();
        let avoid = {
            let mut qs: Vec<String> = run
                .f
                .db
                .records(&run.id, None)
                .iter()
                .filter(|r| r.json["labels"]["subtopic"] == sub["name"])
                .map(|r| format!("- {}", truncate(&super::verify::user_text(&r.json), 120)))
                .collect();
            let skip = qs.len().saturating_sub(15);
            qs.drain(..skip);
            if qs.is_empty() {
                "(none yet)".to_string()
            } else {
                qs.join(
                    "
",
                )
            }
        };
        let chunks_text = fence("SOURCE CHUNKS", &blocks);
        let tools_fenced = fence("TOOLS", &tools_text);
        let count_s = count.to_string();
        let vars: Vec<(&str, &str)> = vec![
            ("goal", &run.spec.goal),
            ("subtopic", sub["name"].as_str().unwrap_or("")),
            ("description", sub["description"].as_str().unwrap_or("")),
            ("questions", &questions),
            ("kind", &kind),
            ("kind_guide", guide),
            ("chunks", &chunks_text),
            ("tools", &tools_fenced),
            ("count", &count_s),
            ("avoid", &avoid),
        ];
        let (system, user) = prompt(run.home(), if tool_use { "generate_tool" } else { "generate" }, &vars);
        let offered_c = offered.clone();
        let validate = move |v: &Value| -> Result<(), String> {
            let recs = v["records"]
                .as_array()
                .filter(|a| !a.is_empty())
                .ok_or("records must be a non-empty list")?;
            let mut first_err = String::new();
            for r in recs {
                match check_record(r, &offered_c, tool_use) {
                    Ok(_) => return Ok(()),
                    Err(e) if first_err.is_empty() => first_err = e,
                    Err(_) => {}
                }
            }
            Err(format!("no usable record: {first_err}"))
        };
        let res = run
            .ask_json(
                Role::Author,
                &format!("generate:{kind}"),
                &system,
                &user,
                run.f.settings().gen_max_tokens,
                &validate,
            )
            .await?;
        match res {
            Err(why) => {
                run.f
                    .db
                    .finish_work(&run.id, "generate", idx, "failed", &json!({"error": why}))?;
                run.log("warn", &format!("call {idx} gave no usable records: {why}"));
                streak += 1;
                if streak >= 4 {
                    return Err(Halt::Fail(Fail::new(
                        422,
                        "invalid_output",
                        format!("the author model keeps giving unusable records (last: {why})"),
                    )));
                }
            }
            Ok((v, reply)) => {
                streak = 0;
                let mut rows = vec![];
                let mut next = run.f.db.record_count(&run.id) + 1;
                let mut good = 0;
                for r in v["records"].as_array().cloned().unwrap_or_default() {
                    let id = format!("r-{next:05}");
                    match check_record(&r, &offered, tool_use) {
                        Ok(d) if good < count => {
                            let rec = make_record(
                                &id,
                                &d,
                                &kind,
                                sub,
                                &model,
                                (&run.id, &run.job_id),
                                reply.trace.as_deref(),
                                &picked,
                            );
                            rows.push(row(next, &id, rec, "pending", None));
                            good += 1;
                            next += 1;
                        }
                        Ok(_) => {} // more than asked for
                        Err(why) => {
                            let inp = r["input"].as_str().unwrap_or("").trim();
                            let out = r["output"].as_str().unwrap_or("").trim();
                            if inp.is_empty() || out.is_empty() {
                                continue;
                            }
                            // kept only so the owner can see what was refused and why
                            let rec = json!({"id": id, "task": "chat", "input": [{"role": "user", "content": inp}],
                                             "output": out, "labels": {"kind": kind, "subtopic": sub["name"]},
                                             "tags": [], "source": {"kind": "teacher", "model": model,
                                             "trace": reply.trace, "factory_task": run.id}, "split": "train",
                                             "status": "rejected", "rev": 1});
                            rows.push(row(next, &id, rec, "rejected", Some(format!("citation: {why}"))));
                            rejected += 1;
                            next += 1;
                        }
                    }
                }
                run.f.db.add_generated(
                    &run.id,
                    "generate",
                    idx,
                    &rows,
                    &json!({"records": good, "refused": rows.len() as i64 - good, "kind": kind, "trace": reply.trace}),
                )?;
                calls_ok += 1;
            }
        }
    }
    let usable = run.f.db.usable_count(&run.id);
    if usable == 0 {
        return Err(Halt::Fail(Fail::new(
            422,
            "no_records",
            "no usable record was generated",
        )));
    }
    let (p, d, f) = run.f.db.work_counts(&run.id, "generate");
    Ok(StepEnd::Done(
        json!({"records": usable, "target": target, "refused_citation": rejected,
                            "calls": p + d + f, "calls_done": d, "calls_failed": f, "calls_this_run": calls_ok}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered() -> Vec<String> {
        vec!["c1".into(), "c2".into()]
    }

    #[test]
    fn a_record_must_cite_chunks_that_were_shown() {
        let ok = json!({"input": "How do I cancel?", "output": "Call abort on the handle.", "cites": ["c1", "[c2]"]});
        let d = check_record(&ok, &offered(), false).unwrap();
        assert_eq!(d.cites, ["c1", "c2"]);
        let none = json!({"input": "How do I cancel?", "output": "Call abort on the handle."});
        assert!(check_record(&none, &offered(), false).unwrap_err().contains("cites"));
        let invented = json!({"input": "How do I cancel?", "output": "Call abort on the handle.", "cites": ["c9"]});
        assert!(check_record(&invented, &offered(), false).unwrap_err().contains("c9"));
        let short = json!({"input": "?", "output": "Call abort on the handle.", "cites": ["c1"]});
        assert!(check_record(&short, &offered(), false).is_err());
    }

    #[test]
    fn tool_records_need_calls_with_object_arguments() {
        let r = json!({"input": "check the crate", "output": "It passed with no errors.", "cites": ["c1"],
                       "tool_calls": [{"name": "cargo_check", "arguments": "{\"path\": \".\"}"}]});
        let d = check_record(&r, &offered(), true).unwrap();
        assert_eq!(d.tool_calls[0]["arguments"]["path"], ".");
        let mut no_calls = r.clone();
        no_calls["tool_calls"] = json!([]);
        assert!(check_record(&no_calls, &offered(), true).is_err());
        assert!(check_record(&r, &offered(), false).unwrap().tool_calls.is_empty());
    }

    #[test]
    fn kinds_rotate_and_tool_use_needs_tools() {
        let ks: Vec<&str> = (0..6).map(|i| kind_for(i, false)).collect();
        assert_eq!(ks, ["qa", "instruction", "steps", "counter", "qa", "instruction"]);
        assert_eq!(kind_for(2, true), "tool_use");
        assert_eq!(kind_for(1, true), "instruction");
    }
}

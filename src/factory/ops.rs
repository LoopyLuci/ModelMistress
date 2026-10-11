//! The factory as hub operations (`factory.*`, so also MCP tools) and REST routes under `/v1/factory/...`.
//!
//! `factory.task.create` is `POST /v1/factory/task/create`; operations that only read also answer GET with the
//! arguments in the query string. None of these routes is in the peer allowlist: only the owner's token reaches them.

use std::collections::HashMap;

use axum::extract::{Query as AxQuery, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use super::spec::TaskSpec;
use super::{runtime, start, Factory, Res, STEPS};
use crate::engine::{Fail, Target};
use crate::hub::{ApiError, Hub};
use crate::km::vocab::Vocabulary;

fn op(
    id: &str,
    summary: &str,
    mutating: bool,
    destructive: bool,
    props: Value,
    required: &[&str],
    open: bool,
) -> Value {
    json!({"id": id, "group": "factory", "summary": summary, "mutating": mutating, "destructive": destructive,
           "input_schema": {"type": "object", "properties": props, "required": required, "additionalProperties": open}})
}

pub fn operations() -> Vec<Value> {
    let strings = json!({"type": "array", "items": {"type": "string"}});
    let id = json!({"type": "string", "description": "a factory task id (factory.task.list shows them)"});
    let create_props = json!({
        "goal": {"type": "string", "description": "what the Knowledge Module is for"},
        "kinds": {"type": "array", "items": {"type": "string", "enum": ["data", "memory", "skill", "tool", "adapter"]}},
        "domain": {"type": "string", "description": "taxonomy path, most general first: code/rust/async"},
        "sources": {"type": "array", "items": {"type": "object"}, "description": "{kind: dir|file|url|km|skill|tool|traces, ...}; see docs/native/km-factory-runtime.md"},
        "author": {"type": "string", "description": "the hub model that writes the records"},
        "reviewer": {"type": "string", "description": "the hub model that checks them (default: the author)"},
        "base": {"type": "string", "description": "the model the evaluation answers with, and the core an adapter is for"},
        "size": {"type": "object", "description": "{records, max_tokens}"},
        "tagging": {"type": "object", "description": "{vocabulary: default, review: auto|all|none}"},
        "approval": {"type": "string", "enum": ["gated", "propose", "auto", "off"]},
        "budget": {"type": "object", "description": "{wall_s, max_records, gpu_hours}"},
        "device": {"type": "object", "description": "{author, reviewer, base, embedder}: auto|gpu|cpu|layers N"},
        "plan_approval": {"type": "boolean", "description": "stop after planning until the plan is approved"},
        "eval_sets": {"type": "array", "items": {"type": "object"}, "description": "{path}|{ids}|{hashes} the records must not leak"},
        "license": {"type": "string"}, "think": {"type": "boolean"}, "per_call": {"type": "integer"}, "name": {"type": "string"},
        "start": {"type": "boolean", "description": "run the pipeline now (default true)"}
    });
    vec![
        op("factory.task.create", "Create a factory task (a description of the KMs to make) and start its pipeline: plan, gather, generate, verify, tag, build, evaluate, publish", true, false, create_props, &["goal", "author"], true),
        op("factory.task.list", "Factory tasks, newest first, with their state and step", false, false,
           json!({"state": {"type": "string"}, "limit": {"type": "integer", "minimum": 1}}), &[], false),
        op("factory.task.get", "One task: state, steps with their results, plan, usage, summary, recent log; optionally records and review items", false, false,
           json!({"id": id, "logs": {"type": "integer", "minimum": 0}, "since": {"type": "integer"},
                  "records": {"type": "integer", "minimum": 0, "description": "how many records to include (0 = none)"},
                  "status": {"type": "string", "description": "only records of this status"},
                  "review": {"type": "boolean", "description": "include the open review items"}}), &["id"], false),
        op("factory.task.plan.edit", "Replace or patch the task's plan (merge patch: null removes a key) and/or approve it; a task with plan_approval waits for this", true, false,
           json!({"id": id, "plan": {"type": "object"}, "changes": {"type": "object"}, "approve": {"type": "boolean"}, "by": {"type": "string"}}), &["id"], false),
        op("factory.task.resume", "Continue a task after an interruption, a cancel, a failure or an exhausted budget; changes raises budgets or changes models first; from re-opens a step", true, false,
           json!({"id": id, "from": {"type": "string", "enum": STEPS}, "changes": {"type": "object"}}), &["id"], false),
        op("factory.task.cancel", "Cancel a running task (it stops at the next safe point and can be resumed)", true, false, json!({"id": id}), &["id"], false),
        op("factory.review.list", "The review queue: records, tags, new vocabulary values and promotions waiting for a decision", false, false,
           json!({"task": id, "kind": {"type": "string", "enum": ["record", "tag", "vocab", "promotion"]},
                  "state": {"type": "string", "description": "open (default), approved, rejected, edited, or all"},
                  "limit": {"type": "integer", "minimum": 1}, "offset": {"type": "integer", "minimum": 0}}), &[], false),
        op("factory.review.decide", "Approve, reject or edit a review item; decisions are recorded and teach the tag classifier", true, false,
           json!({"item": {"type": ["integer", "string"]}, "decision": {"type": "string", "enum": ["approve", "reject", "edit"]},
                  "changes": {"type": "object", "description": "edit: {input, output, tags} of a record"},
                  "by": {"type": "string"}, "note": {"type": "string"},
                  "add_to_vocabulary": {"type": "boolean", "description": "approving a tag outside the vocabulary adds its value (default true)"}}), &["item", "decision"], false),
        op("factory.vocabulary.get", "The controlled tag vocabulary (same as km.vocabulary.get)", false, false, json!({}), &[], false),
        op("factory.vocabulary.edit", "Approve or remove tag values, open or close a namespace (same as km.vocabulary.edit)", true, false,
           json!({"namespace": {"type": "string"}, "add": strings, "remove": strings, "open": {"type": "boolean"},
                  "description": {"type": "string"}, "delete": {"type": "boolean"}}), &["namespace"], false),
        op("factory.settings", "The factory's settings (embedder, promotion policy, thresholds); give settings to change them", true, false,
           json!({"settings": {"type": "object"}}), &[], false),
        op("factory.embedder.setup", "Make a hub model the factory's embedder under the name mm-embed-1 (linked into <home>/models) and point the KM runtime at it", true, false,
           json!({"model": {"type": "string", "description": "a hub model id of an embedding GGUF, e.g. hf.co/Qwen/Qwen3-Embedding-0.6B-GGUF:Q8_0 once pulled"},
                  "name": {"type": "string"}, "configure": {"type": "boolean"}}), &["model"], false),
    ]
}

fn s<'a>(a: &'a Value, k: &str) -> Res<&'a str> {
    a.get(k)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Fail::invalid(format!("{k} is required")))
}

fn os<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str).filter(|v| !v.is_empty())
}

/// RFC 7386 merge patch.
pub fn merge_patch(target: &mut Value, patch: &Value) {
    match patch {
        Value::Object(p) => {
            if !target.is_object() {
                *target = json!({});
            }
            if let Some(t) = target.as_object_mut() {
                for (k, v) in p {
                    if v.is_null() {
                        t.remove(k);
                    } else {
                        merge_patch(t.entry(k.clone()).or_insert(Value::Null), v);
                    }
                }
            }
        }
        _ => *target = patch.clone(),
    }
}

fn model_known(hub: &Hub, m: &str) -> Res<()> {
    match hub.engine.resolve(m) {
        Ok(Target::Llama(_)) | Ok(Target::Ollama(_)) => Ok(()),
        Err(f) => Err(f),
    }
}

fn running_job(hub: &Hub, f: &Factory, id: &str) -> Option<String> {
    let t = f.db.task(id).ok()?;
    let j = t.job.as_deref().and_then(|j| hub.jobs.get(j))?;
    (j.status() == "running").then(|| j.id.clone())
}

/// Forgets what the steps from `step` on produced, so they run again (records and chunks the earlier steps made stay).
fn reopen(f: &Factory, id: &str, step: &str) -> Res<()> {
    let Some(i) = STEPS.iter().position(|x| *x == step) else {
        return Err(Fail::invalid(format!("from must be one of {}", STEPS.join(", "))));
    };
    f.db.reset_steps(id, &STEPS[i..])?;
    let at = |n: &str| STEPS.iter().position(|x| *x == n).unwrap_or(0);
    if i <= at("gather") {
        f.db.replace_chunks(id, &[])?;
    }
    if i <= at("generate") {
        // new calls are added up to the (possibly raised) target; none are discarded
    }
    if i <= at("tag") {
        let rows: Vec<_> =
            f.db.records(id, None)
                .into_iter()
                .filter(|r| r.status != "rejected")
                .map(|mut r| {
                    r.tagged = false;
                    r
                })
                .collect();
        f.db.put_records(id, &rows)?;
    }
    if i <= at("build") {
        f.db.clear_artifacts(id)?;
    }
    if i <= at("evaluate") {
        f.db.clear_evals(id);
        f.db.clear_artifacts_stage(id, "evaluated")?;
    }
    Ok(())
}

pub async fn call(hub: &Hub, id: &str, a: &Value) -> Res<Value> {
    let f = runtime(&hub.engine)?;
    Ok(match id {
        "factory.task.create" => {
            let mut body = a.clone();
            let go = body
                .as_object_mut()
                .and_then(|o| o.remove("start"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let spec = TaskSpec::from_json(&body)?;
            for m in [spec.author.clone(), spec.reviewer_model()] {
                model_known(hub, &m)?;
            }
            let tid = format!("ft-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]);
            let stored = serde_json::to_value(&spec).map_err(|e| Fail::new(500, "internal", e.to_string()))?;
            f.db.insert_task(&tid, &spec.name_slug(), &stored)?;
            let _ = std::fs::create_dir_all(f.dir.join("tasks").join(&tid));
            let mut out = json!({"task": tid, "state": "created", "name": spec.name_slug()});
            if go {
                let j = start(hub, &f, &tid)?;
                out["job"] = j["job"].clone();
                out["state"] = json!("running");
            }
            out
        }
        "factory.task.list" => {
            let rows = f.db.tasks(os(a, "state"), a["limit"].as_u64().unwrap_or(50) as usize)?;
            json!(rows
                .iter()
                .map(
                    |t| json!({"id": t.id, "name": t.name, "state": t.state, "status": t.state, "step": t.step,
                                "error": t.error, "goal": t.spec["goal"], "created": t.created, "updated": t.updated,
                                "created_ms": t.created, "updated_ms": t.updated, "job": t.job,
                                "records": f.db.usable_count(&t.id)})
                )
                .collect::<Vec<_>>())
        }
        "factory.task.get" => {
            let t = f.db.task(s(a, "id")?)?;
            let mut v = t.to_json();
            v["steps"] = json!(f.db.steps(&t.id).iter().map(|x| x.to_json()).collect::<Vec<_>>());
            match f.db.plan(&t.id) {
                Some((rev, plan, approved)) => {
                    v["plan"] = plan;
                    v["plan_rev"] = json!(rev);
                    v["plan_approved"] = json!(approved);
                }
                None => v["plan"] = Value::Null,
            }
            let mut per_km = serde_json::Map::new();
            let usable = f.db.usable_count(&t.id);
            for a in f.db.artifacts(&t.id).iter().filter(|a| a.kind == "data") {
                per_km.insert(a.id.clone(), json!(usable));
            }
            v["records"] = Value::Object(per_km);
            v["record_count"] = json!(usable);
            v["kms"] = json!(f
                .db
                .artifacts(&t.id)
                .iter()
                .filter(|a| a.imported)
                .map(|a| format!("{}@{}", a.id, a.version))
                .collect::<Vec<_>>());
            let logs = a["logs"].as_u64().unwrap_or(20) as usize;
            v["logs"] = json!(f.db.logs(&t.id, logs, a["since"].as_i64().unwrap_or(0)));
            v["running"] = json!(running_job(hub, &f, &t.id));
            v["review_open"] = json!(f.db.review_open_count(&t.id));
            v["artifacts"] = json!(f
                .db
                .artifacts(&t.id)
                .iter()
                .map(
                    |x| json!({"id": x.id, "version": x.version, "kind": x.kind, "stage": x.stage, "key": x.key,
                                "imported": x.imported})
                )
                .collect::<Vec<_>>());
            let n = a["records"].as_u64().unwrap_or(0) as usize;
            if n > 0 {
                v["record_items"] = json!(f
                    .db
                    .records(&t.id, os(a, "status"))
                    .iter()
                    .take(n)
                    .map(|r| {
                        let mut j = r.json.clone();
                        j["status"] = json!(r.status);
                        j["reason"] = json!(r.reason);
                        j["score"] = json!(r.score);
                        j
                    })
                    .collect::<Vec<_>>());
            }
            if a["review"].as_bool().unwrap_or(false) {
                v["review"] = json!(f
                    .db
                    .review_list(Some(&t.id), None, Some("open"), 200, 0)
                    .1
                    .iter()
                    .map(|r| r.to_json())
                    .collect::<Vec<_>>());
            }
            v
        }
        "factory.task.plan.edit" => {
            let tid = s(a, "id")?;
            let t = f.db.task(tid)?;
            if let Some(j) = running_job(hub, &f, tid) {
                return Err(Fail::new(
                    409,
                    "running",
                    format!("task {tid} is running as job {j}; cancel it first"),
                ));
            }
            let spec = TaskSpec::from_json(&t.spec)?;
            let cur = f.db.plan(tid);
            let mut plan = match (a.get("plan").filter(|p| p.is_object()), &cur) {
                (Some(p), _) => p.clone(),
                (None, Some((_, p, _))) => p.clone(),
                (None, None) => return Err(Fail::invalid("the task has no plan yet; give plan")),
            };
            if let Some(ch) = a.get("changes").filter(|c| c.is_object()) {
                merge_patch(&mut plan, ch);
            }
            let plan = super::plan::normalize_plan(&plan, &spec.domain).map_err(Fail::invalid)?;
            let approve = a["approve"].as_bool().unwrap_or(false) || !spec.plan_approval;
            let rev = f.db.put_plan(tid, &plan, approve, os(a, "by").unwrap_or("owner"))?;
            f.db.log(
                tid,
                "plan",
                "info",
                &format!(
                    "plan revision {rev} saved{}",
                    if approve { " and approved" } else { "" }
                ),
            );
            json!({"task": tid, "rev": rev, "approved": approve, "plan": plan,
                   "next": "factory.task.resume continues the pipeline (from generate re-opens generation on the new plan)"})
        }
        "factory.task.resume" => {
            let tid = s(a, "id")?;
            let t = f.db.task(tid)?;
            if let Some(j) = running_job(hub, &f, tid) {
                return Err(Fail::new(
                    409,
                    "running",
                    format!("task {tid} is already running as job {j}"),
                ));
            }
            if let Some(ch) = a.get("changes").filter(|c| c.is_object()) {
                let mut spec = t.spec.clone();
                const ALLOWED: [&str; 14] = [
                    "size",
                    "budget",
                    "device",
                    "approval",
                    "author",
                    "reviewer",
                    "base",
                    "tagging",
                    "plan_approval",
                    "eval_sets",
                    "think",
                    "per_call",
                    "license",
                    "kinds",
                ];
                for (k, v) in ch.as_object().into_iter().flatten() {
                    if !ALLOWED.contains(&k.as_str()) {
                        return Err(Fail::invalid(format!(
                            "{k} cannot be changed on a task (allowed: {})",
                            ALLOWED.join(", ")
                        )));
                    }
                    let mut one = json!({});
                    one[k] = v.clone();
                    merge_patch(&mut spec, &one);
                }
                let checked = TaskSpec::from_json(&spec)?;
                for m in [checked.author.clone(), checked.reviewer_model()] {
                    model_known(hub, &m)?;
                }
                f.db.set_spec(
                    tid,
                    &serde_json::to_value(&checked).map_err(|e| Fail::new(500, "internal", e.to_string()))?,
                )?;
            }
            match os(a, "from") {
                Some(step) => reopen(&f, tid, step)?,
                None if t.state == "done" => {
                    return Err(Fail::new(
                        409,
                        "done",
                        format!("task {tid} is done; give from to run a step again"),
                    ))
                }
                None => {}
            }
            let mut out = start(hub, &f, tid)?;
            out["resumed_from"] = json!(f
                .db
                .steps(tid)
                .iter()
                .find(|x| !matches!(x.status.as_str(), "done" | "skipped"))
                .map(|x| x.name.clone()));
            out
        }
        "factory.task.cancel" => {
            let tid = s(a, "id")?;
            let t = f.db.task(tid)?;
            match running_job(hub, &f, tid) {
                Some(j) => {
                    f.db.set_cancel(tid, true)?;
                    hub.jobs.cancel(&j);
                    json!({"task": tid, "job": j, "cancelling": true})
                }
                None if matches!(t.state.as_str(), "done" | "cancelled") => {
                    return Err(Fail::new(409, "not_running", format!("task {tid} is {}", t.state)))
                }
                None => {
                    f.db.set_state(tid, "cancelled", None, None)?;
                    json!({"task": tid, "cancelled": true})
                }
            }
        }
        "factory.review.list" => {
            let state = match os(a, "state") {
                Some("all") => None,
                Some(x) => Some(x),
                None => Some("open"),
            };
            let (total, rows) = f.db.review_list(
                os(a, "task"),
                os(a, "kind"),
                state,
                a["limit"].as_u64().unwrap_or(100) as usize,
                a["offset"].as_u64().unwrap_or(0) as usize,
            );
            json!({"total": total, "items": rows.iter().map(|r| r.to_json()).collect::<Vec<_>>()})
        }
        "factory.review.decide" => decide(hub, &f, a).await?,
        "factory.vocabulary.get" => crate::km::ops::call(hub, "km.vocabulary.get", a).await?,
        "factory.vocabulary.edit" => crate::km::ops::call(hub, "km.vocabulary.edit", a).await?,
        "factory.settings" => match a.get("settings").filter(|s| s.is_object()) {
            Some(p) => json!(f.set_settings(p)?),
            None => json!(f.settings()),
        },
        "factory.embedder.setup" => embedder_setup(hub, &f, a)?,
        _ => return Err(Fail::not_found(format!("no operation {id}"))),
    })
}

fn embedder_setup(hub: &Hub, f: &Factory, a: &Value) -> Res<Value> {
    let Target::Llama(entry) = hub.engine.resolve(s(a, "model")?)? else {
        return Err(Fail::invalid(
            "the embedder must be a GGUF model the hub serves, not an Ollama one",
        ));
    };
    let name = os(a, "name")
        .map(str::to_string)
        .unwrap_or_else(|| f.settings().embed_model);
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(Fail::invalid("name must be letters, digits, '-', '_' or '.'"));
    }
    let models = hub.engine.home.join("models");
    std::fs::create_dir_all(&models).map_err(|e| Fail::new(500, "io", format!("{}: {e}", models.display())))?;
    let dest = models.join(format!("{name}.gguf"));
    let mut how = "exists";
    if !dest.exists() {
        // a hard link costs no disk; a copy when the files are on different volumes
        how = if std::fs::hard_link(&entry.path, &dest).is_ok() {
            "linked"
        } else {
            std::fs::copy(&entry.path, &dest).map_err(|e| Fail::new(500, "io", format!("{}: {e}", dest.display())))?;
            "copied"
        };
    }
    hub.engine.refresh();
    if a["configure"].as_bool().unwrap_or(true) {
        let mut st = hub.engine.settings();
        st.km.embedder = "hub".into();
        st.km.embed_model = Some(name.clone());
        hub.engine.set_settings(st)?;
        f.set_settings(&json!({"embedder": "hub", "embed_model": name}))?;
    }
    Ok(json!({"name": name, "from": entry.id, "path": dest, "how": how,
              "note": "KMs made from now on carry routing.embed_model = this name; km.embedder is hub with the same model"}))
}

async fn decide(hub: &Hub, f: &Factory, a: &Value) -> Res<Value> {
    let item = a["item"]
        .as_i64()
        .or_else(|| a["item"].as_str().and_then(|s| s.parse().ok()))
        .ok_or_else(|| Fail::invalid("item (a review item number) is required"))?;
    // the dashboard sends the edit as `record` or `tag`
    let mut a = a.clone();
    if a.get("changes").is_none() {
        if let Some(c) = a.get("record").filter(|c| c.is_object()).cloned() {
            a["changes"] = c;
        }
    }
    let a = &a;
    let decision = s(a, "decision")?;
    if !matches!(decision, "approve" | "reject" | "edit") {
        return Err(Fail::invalid("decision must be approve, reject or edit"));
    }
    let by = os(a, "by").unwrap_or("owner");
    let row = f.db.review_item(item)?;
    if row.state != "open" {
        return Err(Fail::new(
            409,
            "decided",
            format!("review item {item} was already decided ({})", row.state),
        ));
    }
    let km = crate::km::runtime(&hub.engine)?;
    let mut effect = json!({});
    match row.kind.as_str() {
        "record" => {
            let mut r =
                f.db.record(&row.task, &row.reference)
                    .ok_or_else(|| Fail::not_found(format!("record {} is gone", row.reference)))?;
            match decision {
                "reject" => {
                    r.status = "rejected".into();
                    r.reason = Some(format!("rejected by {by}"));
                }
                _ => {
                    if decision == "edit" {
                        let ch = a
                            .get("changes")
                            .filter(|c| c.is_object())
                            .ok_or_else(|| Fail::invalid("edit needs changes"))?;
                        if let Some(i) = ch["input"].as_str() {
                            r.json["input"] = json!([{"role": "user", "content": i}]);
                            r.emb = None;
                        }
                        if let Some(o) = ch["output"].as_str() {
                            r.json["output"] = json!(o);
                        }
                        if let Some(t) = ch["tags"].as_array() {
                            r.json["tags"] = json!(t);
                            r.tagged = true;
                        }
                        r.hash = super::util::text_hash(&super::verify::user_text(&r.json));
                    }
                    r.status = "approved".into();
                    r.reason = None;
                }
            }
            r.json["status"] = json!(r.status);
            r.json["rev"] = json!(r.json["rev"].as_i64().unwrap_or(1) + 1);
            r.json["edited_by"] = json!(by);
            r.json["labels"]["reviewed"] = json!(decision);
            f.db.put_record(&row.task, &r)?;
            effect = json!({"record": r.id, "status": r.status});
        }
        "tag" => {
            let (rid, tag) = row.reference.split_once('|').unwrap_or((&row.reference, ""));
            let mut r =
                f.db.record(&row.task, rid)
                    .ok_or_else(|| Fail::not_found(format!("record {rid} is gone")))?;
            let words = super::util::words(&format!(
                "{} {}",
                super::verify::user_text(&r.json),
                r.json["output"].as_str().unwrap_or("")
            ));
            if decision == "reject" {
                f.db.add_feedback(tag, &words, -1.0);
                effect = json!({"record": rid, "tag": tag, "attached": false});
            } else {
                let mut vocab = km.vocabulary()?;
                if vocab.problem(tag).is_some() {
                    if a["add_to_vocabulary"].as_bool().unwrap_or(true) {
                        if let Some((ns, v)) = tag.split_once(':') {
                            vocab.edit(&json!({"namespace": ns, "add": [v]}))?;
                            vocab.save(&km.store.root)?;
                        }
                    } else {
                        return Err(Fail::invalid(format!(
                            "{tag} is not in the vocabulary; approve it with add_to_vocabulary"
                        )));
                    }
                }
                let mut tags: Vec<String> = r.json["tags"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
                if !tags.iter().any(|t| t == tag) {
                    tags.push(tag.to_string());
                }
                tags.sort();
                r.json["tags"] = json!(tags);
                r.json["tag_info"][tag] = json!({"origin": "owner", "confidence": 1.0});
                f.db.put_record(&row.task, &r)?;
                f.db.add_feedback(tag, &words, 1.0);
                effect = json!({"record": rid, "tag": tag, "attached": true});
            }
        }
        "vocab" => {
            if decision != "reject" {
                if let Some((ns, v)) = row.reference.split_once(':') {
                    let mut vocab: Vocabulary = km.vocabulary()?;
                    vocab.edit(&json!({"namespace": ns, "add": [v]}))?;
                    vocab.save(&km.store.root)?;
                    effect = json!({"added": row.reference});
                }
            }
        }
        "promotion" => {
            if decision != "reject" {
                let mut moved = vec![];
                for k in row.data["kms"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    if let Some((kid, ver)) = k.split_once('@') {
                        if !kid.starts_with("adapter.") {
                            km.store.channel_set(kid, "stable", ver, None)?;
                            moved.push(k.to_string());
                        }
                    }
                }
                effect = json!({"promoted": moved});
            }
        }
        other => return Err(Fail::invalid(format!("unknown review kind {other}"))),
    }
    let state = match decision {
        "approve" => "approved",
        "reject" => "rejected",
        _ => "edited",
    };
    f.db.review_decide(item, state, decision, by, os(a, "note"))?;
    f.db.log(
        &row.task,
        "review",
        "info",
        &format!("item {item} ({}) {decision} by {by}", row.kind),
    );
    Ok(json!({"item": item, "kind": row.kind, "state": state, "effect": effect,
              "open_left": f.db.review_open_count(&row.task),
              "note": if row.kind == "record" || row.kind == "tag" { "KMs already built keep their version; resume with from=build to make a new one" } else { "" }}))
}

// ---- REST --------------------------------------------------------------------------------------------------------

/// Query-string arguments to JSON: integers and booleans are typed.
pub fn query_args(q: HashMap<String, String>) -> Value {
    let mut o = serde_json::Map::new();
    for (k, v) in q {
        let val = if let Ok(n) = v.parse::<i64>() {
            json!(n)
        } else if matches!(v.as_str(), "true" | "false") {
            json!(v == "true")
        } else {
            json!(v)
        };
        o.insert(k, val);
    }
    Value::Object(o)
}

async fn run(hub: Hub, op: String, args: Value) -> Result<Json<Value>, ApiError> {
    if !args.is_object() {
        return Err(Fail::invalid("the body must be a JSON object").into());
    }
    let r = crate::hub::call(&hub, &op, &args).await?;
    Ok(Json(json!({"result": r})))
}

/// `factory.task.plan.edit` is `/v1/factory/task/plan/edit`.
pub fn route_of(op: &str) -> String {
    format!("/v1/{}", op.replace('.', "/"))
}

pub fn routes() -> Router<Hub> {
    let mut r = Router::new();
    for o in operations() {
        let id = o["id"].as_str().unwrap_or("").to_string();
        let path = route_of(&id);
        let (idp, idg) = (id.clone(), id.clone());
        let posting = post(move |State(hub): State<Hub>, body: Option<Json<Value>>| {
            let (op, args) = (idp.clone(), body.map(|Json(v)| v).unwrap_or(json!({})));
            async move { run(hub, op, args).await }
        });
        let method = if o["mutating"].as_bool().unwrap_or(true) {
            posting
        } else {
            posting.merge(get(
                move |State(hub): State<Hub>, AxQuery(q): AxQuery<HashMap<String, String>>| {
                    let op = idg.clone();
                    async move { run(hub, op, query_args(q)).await }
                },
            ))
        };
        r = r.route(&path, method);
    }
    r
}

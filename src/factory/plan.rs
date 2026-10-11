//! Step 1, plan: the author model outlines the KM (subtopics and their questions, tools, skills, where it sits in the
//! taxonomy). The outline is stored and can be edited (`factory.task.plan.edit`) before anything is generated; a task
//! may require the owner's approval of the plan first.

use serde_json::{json, Value};

use super::run::{Halt, Role, Run, StepRes};
use super::util::{fence, prompt, truncate};
use super::StepEnd;
use crate::engine::Fail;

/// Checks (and tidies) a plan: it needs subtopics, each with a name.
pub fn normalize_plan(v: &Value, spec_domain: &str) -> Result<Value, String> {
    let subs = v["subtopics"].as_array().filter(|a| !a.is_empty()).ok_or("subtopics must be a non-empty list")?;
    let mut out = vec![];
    for (i, s) in subs.iter().enumerate() {
        // a bare string is a subtopic name
        let (name, desc, qs) = match s {
            Value::String(n) => (n.clone(), String::new(), vec![]),
            Value::Object(_) => (
                s["name"].as_str().unwrap_or("").trim().to_string(),
                s["description"].as_str().unwrap_or("").trim().to_string(),
                s["questions"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|q| q.as_str().map(|q| q.trim().to_string()))
                            .filter(|q| !q.is_empty())
                            .collect()
                    })
                    .unwrap_or_default(),
            ),
            _ => return Err(format!("subtopics[{i}] must be an object with a name")),
        };
        if name.is_empty() {
            return Err(format!("subtopics[{i}] needs a name"));
        }
        out.push(json!({"name": name, "description": desc, "questions": qs}));
    }
    let strings = |k: &str| -> Vec<Value> {
        v[k].as_array()
            .map(|a| a.iter().filter(|x| x.is_string()).cloned().collect())
            .unwrap_or_default()
    };
    let domain = v["taxonomy"]["domain"]
        .as_str()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .unwrap_or(spec_domain)
        .trim_matches('/')
        .to_string();
    Ok(json!({
        "title": v["title"].as_str().unwrap_or("").trim(),
        "summary": v["summary"].as_str().unwrap_or("").trim(),
        "subtopics": out,
        "tools": strings("tools"),
        "skills": strings("skills"),
        "taxonomy": {"domain": domain, "tags": v["taxonomy"]["tags"].as_array().cloned().unwrap_or_default()},
        "keywords": strings("keywords"),
    }))
}

pub async fn run(run: &Run) -> StepRes<StepEnd> {
    if let Some((rev, plan, approved)) = run.f.db.plan(&run.id) {
        run.log("info", &format!("plan revision {rev} already stored; not drafting another"));
        return Ok(StepEnd::Done(json!({"rev": rev, "approved": approved,
                                        "subtopics": plan["subtopics"].as_array().map(|a| a.len())})));
    }
    let tools: Vec<String> = run
        .spec
        .sources
        .iter()
        .filter(|s| s["kind"] == "tool")
        .filter_map(|s| super::gather::tool_def(s).ok())
        .map(|t| format!("{}: {}", t.name, super::util::truncate(&t.description, 100)))
        .collect();
    let target = run.spec.target_records() as usize;
    let n_sub = (target / 8).clamp(2, 6);
    let preview = {
        let s = run.spec.sources.clone();
        tokio::task::spawn_blocking(move || super::gather::preview(&s))
            .await
            .unwrap_or_default()
    };
    let (system, user) = prompt(
        run.home(),
        "plan",
        &[
            ("goal", &run.spec.goal),
            ("domain", if run.spec.domain.is_empty() { "(choose one)" } else { &run.spec.domain }),
            ("kinds", &run.spec.kinds.join(", ")),
            ("tools", &if tools.is_empty() { "none".to_string() } else { tools.join("; ") }),
            ("sources", &fence("SOURCES", &truncate(&preview, 4000))),
            ("subtopics", &n_sub.to_string()),
            ("questions", "3"),
        ],
    );
    run.progress(0, 1, "drafting the outline");
    let domain = run.spec.domain.clone();
    let res = run
        .ask_json(Role::Author, "plan", &system, &user, 1500, &|v| normalize_plan(v, &domain).map(|_| ()))
        .await?;
    let (v, reply) = res.map_err(|e| {
        Halt::Fail(Fail::new(
            422,
            "invalid_output",
            format!("the author model gave no usable outline after retries: {e}"),
        ))
    })?;
    let plan = normalize_plan(&v, &domain).map_err(|e| Halt::Fail(Fail::invalid(e)))?;
    let approved = !run.spec.plan_approval;
    let rev = run.f.db.put_plan(&run.id, &plan, approved, "author model")?;
    run.log("info", &format!("plan drafted: {} subtopics", plan["subtopics"].as_array().map_or(0, |a| a.len())));
    Ok(StepEnd::Done(json!({"rev": rev, "approved": approved, "trace": reply.trace,
                            "subtopics": plan["subtopics"].as_array().map(|a| a.len())})))
}

//! Misty as hub operations (`misty.*`, so also MCP tools), REST routes under `/v1/misty/...`, a stream of a run's events,
//! and the OpenAI-compatible virtual models (`misty`, `misty-fast`, `misty-main`, `misty-deep`).
//!
//! Every operation `misty.a.b` is also `/v1/misty/a/b`: GET with the arguments in the query string (read-only
//! operations only) or POST with a JSON body. None of these routes is in the peer allowlist: only the owner's token
//! reaches them. A peer reaches Misty only through the OpenAI route, where she has no tools and no files.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Query as AxQuery, State};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use super::agent::{self, Approvals, Ctx, Req};
use super::config::Config;
use super::env::{self, Environment};
use super::pipeline;
use super::roster::Role;
use super::runs::Run;
use super::toolbox;
use super::{router, setup, Misty};
use crate::engine::Fail;
use crate::hosting::Permit;
use crate::hub::{ApiError, Hub};

type Res<T> = Result<T, Fail>;

fn op(id: &str, summary: &str, mutating: bool, destructive: bool, props: Value, required: &[&str]) -> Value {
    json!({"id": id, "group": "misty", "summary": summary, "mutating": mutating, "destructive": destructive,
           "input_schema": {"type": "object", "properties": props, "required": required, "additionalProperties": false}})
}

fn ask_props() -> Value {
    let t = |d: &str| json!({"type": "string", "description": d});
    json!({
        "input": t("what to ask or tell Misty"),
        "session": t("continue this session (misty.session.list shows them)"),
        "new_session": {"type": "boolean", "description": "start a session for this and return its id"},
        "role": {"type": "string", "enum": ["fast", "main", "deep"], "description": "force a role instead of letting the router choose (@fast, @main, @deep in the text do the same)"},
        "rag": {"type": "boolean", "description": "true: always search the index; false: never; unset: the router decides"},
        "collection": t("index collection (default: the configured one)"),
        "tools": {"type": "array", "items": {"type": "string"}, "description": "offer only these tools (misty.tools.list)"},
        "no_tools": {"type": "boolean"},
        "panel": {"type": "boolean", "description": "answer through a panel of two roles and an aggregator (needs panel = true in the config)"},
        "approvals": {"type": "string", "enum": ["ask", "deny"], "description": "a tool that needs approval: wait for misty.approve, or refuse (default: ask for run.start, deny for ask)"},
        "env": t("work in this environment (an id or name from misty.env.list): with a session it is remembered there, without one it applies to this request"),
        "cwd": t("work in this folder (a full path that exists); wins over the environment's folder"),
        "system": t("replace Misty's persona for this request"),
        "audio_path": t("a WAV file on the hub's machine, transcribed by Whistle first"),
        "audio_base64": t("WAV bytes, base64"),
        "image_paths": {"type": "array", "items": {"type": "string"}, "description": "image files on the hub's machine, described by Mirage first"},
        "images_base64": {"type": "array", "items": {"type": "string"}},
        "temperature": {"type": "number"},
        "max_tokens": {"type": "integer", "minimum": 1}
    })
}

pub fn operations() -> Vec<Value> {
    let t = |d: &str| json!({"type": "string", "description": d});
    let coll = t("index collection (default: the configured one)");
    vec![
        op("misty.status", "Is Misty ready: the roster (which model fills which role), the Cactus runtime, engines, memory, the index, policy, what to do next", false, false, json!({}), &[]),
        op("misty.doctor", "Check everything Misty relies on and make one real call of each kind; says how to fix what fails", true, false, json!({}), &[]),
        op("misty.setup.plan", "What can be installed (the Prism engine, the Cactus runtime and models, Bonsai models), sizes, and what is already here", false, false, json!({}), &[]),
        op("misty.setup.run", "Install components as jobs (checked by size and SHA-256); job.get shows progress", true, false,
           json!({"install": {"type": "array", "items": {"type": "string"}, "description": "default, or prism, cactus, needle3, whistle, mirage, bonsai-2b, bonsai-4b, bonsai-9b, bonsai-27b"},
                  "pack": t("Bonsai packing: PQ2_0 (default, fastest, needs the Prism engine), Q2_0, PTQ1_0")}), &["install"]),
        op("misty.ask", "Ask Misty and wait for the answer: routes, retrieves, uses tools under the policy, returns text, role and citations", true, false, ask_props(), &[]),
        op("misty.run.start", "Start a run and return at once; follow it with misty.run.get (or /v1/misty/run/stream); answer approvals with misty.approve", true, false, ask_props(), &[]),
        op("misty.run.get", "Events of a run after `from` (waits up to wait_ms for new ones), whether it is done, and the approvals it waits for", false, false,
           json!({"run": t("a run id"), "from": {"type": "integer", "minimum": 0}, "wait_ms": {"type": "integer", "minimum": 0, "maximum": 25000}}), &["run"]),
        op("misty.run.cancel", "Stop a run (a tool that waits for approval is refused)", true, false, json!({"run": t("a run id")}), &["run"]),
        op("misty.run.list", "Runs in progress and recent finished ones", false, false, json!({"limit": {"type": "integer", "minimum": 1}}), &[]),
        op("misty.approvals", "Tool calls that wait for the owner's answer", false, false, json!({}), &[]),
        op("misty.approve", "Answer a waiting tool call: allow it or refuse it", true, false, json!({"approval": t("an approval id from misty.approvals"), "allow": {"type": "boolean"}}), &["approval", "allow"]),
        op("misty.session.list", "Sessions, newest first", false, false, json!({"limit": {"type": "integer", "minimum": 1}}), &[]),
        op("misty.session.new", "Start a session, optionally in an environment and working folder", true, false,
           json!({"title": t("a name"), "env": t("an environment id or name"), "cwd": t("a working folder (full path)")}), &[]),
        op("misty.session.set", "Choose a session's environment and working folder; null clears one, a field left out stays as it is", true, false,
           json!({"session": t("a session id"), "env": {"type": ["string", "null"], "description": "an environment id or name, or null for none"},
                  "cwd": {"type": ["string", "null"], "description": "a full folder path, or null to use the environment's (or Misty's workspace)"}}), &["session"]),
        op("misty.env.list", "Environments: named working setups (folder, extra folders, variables, a policy for each class of tool)", false, false, json!({}), &[]),
        op("misty.env.save", "Create an environment, or change one by id (fields left out stay as they are; vars and roots replace the whole list)", true, false,
           json!({"id": t("an existing environment's id; leave out to create"), "name": t("a unique name"), "description": t("what it is for"),
                  "cwd": {"type": ["string", "null"], "description": "the working folder (full path, must exist)"},
                  "roots": {"type": "array", "items": {"type": "string"}, "description": "more folders the file tools may reach"},
                  "vars": {"type": "object", "description": "variables for shell_run, name to text"},
                  "policy": {"type": "object", "description": "per class (read, write, exec, net, hub): auto, ask or deny; a class left out keeps Misty's own policy"}}), &[]),
        op("misty.env.delete", "Delete an environment; its sessions keep their own folder and go back to Misty's policy", true, true, json!({"id": t("an environment id")}), &["id"]),
        op("misty.browse", "The folders inside a folder, for choosing a working folder; with no path, the usual starting places (home, drives)", false, false,
           json!({"path": t("a full folder path"), "hidden": {"type": "boolean", "description": "include folders whose name starts with a dot"}}), &[]),
        op("misty.session.get", "A session's messages", false, false, json!({"session": t("a session id"), "limit": {"type": "integer", "minimum": 1}}), &["session"]),
        op("misty.session.rename", "Rename a session", true, false, json!({"session": t("a session id"), "title": t("the new name")}), &["session", "title"]),
        op("misty.session.delete", "Delete a session and its messages", true, true, json!({"session": t("a session id")}), &["session"]),
        op("misty.route", "Show how a request would be routed (features, a score per role, the winner, why) without running it", false, false,
           json!({"input": t("the request"), "collection": coll}), &["input"]),
        op("misty.rag.ingest", "Add a file or a folder (or text with a uri) to the index; unchanged files are skipped", true, false,
           json!({"path": t("file or folder on the hub's machine"), "uri": t("name for text"), "text": t("text to index"), "collection": coll}), &[]),
        op("misty.rag.search", "Search the index (BM25, plus vectors when an embedding model is installed)", false, false,
           json!({"query": t("what to look for"), "collection": coll, "k": {"type": "integer", "minimum": 1, "maximum": 50}}), &["query"]),
        op("misty.rag.sources", "What is indexed, with chunk counts", false, false, json!({"collection": coll}), &[]),
        op("misty.rag.remove", "Remove one source, or a whole collection, from the index", true, true,
           json!({"source": t("a source uri"), "collection": coll, "all": {"type": "boolean", "description": "remove the whole collection"}}), &["collection"]),
        op("misty.tools.list", "The tools Misty can use, their class and the policy for that class", false, false, json!({}), &[]),
        op("misty.config.get", "Misty's settings (roles, policy, roots, limits)", false, false, json!({}), &[]),
        op("misty.config.set", "Change settings: a partial object merged into the current one and checked before it is saved", true, false,
           json!({"patch": {"type": "object", "description": "for example {\"policy\": {\"exec\": \"ask\"}, \"roles\": {\"main\": \"my-model\"}}"}}), &["patch"]),
        op("misty.pipeline.list", "Pipelines: the built-in ones and those in <home>/misty/pipelines", false, false, json!({}), &[]),
        op("misty.pipeline.show", "One pipeline's steps", false, false, json!({"name": t("a pipeline name")}), &["name"]),
        op("misty.pipeline.save", "Save a pipeline from TOML text (checked first)", true, false, json!({"toml": t("the pipeline file's text")}), &["toml"]),
        op("misty.pipeline.run", "Run a pipeline: as a job (job.get), or with wait: true until it ends", true, false,
           json!({"name": t("a pipeline name"), "inputs": {"type": "object"}, "wait": {"type": "boolean"}}), &["name"]),
        op("misty.bench", "Time one short generation on every ready role (the first run includes loading)", true, false,
           json!({"tokens": {"type": "integer", "minimum": 8, "maximum": 512}}), &[]),
    ]
}

// ---- arguments ---------------------------------------------------------------------------------------------------------

fn s<'a>(a: &'a Value, k: &str) -> Res<&'a str> {
    a.get(k)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Fail::invalid(format!("{k} is required")))
}

fn os<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str).filter(|v| !v.is_empty())
}

fn strs(a: &Value, k: &str) -> Vec<String> {
    match a.get(k) {
        Some(Value::Array(v)) => v.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
        Some(Value::String(x)) => x
            .split(',')
            .map(|y| y.trim().to_string())
            .filter(|y| !y.is_empty())
            .collect(),
        _ => vec![],
    }
}

fn io(e: impl std::fmt::Display) -> Fail {
    Fail::new(500, "misty_failed", e.to_string())
}

fn read_media(path: &str, what: &str) -> Res<Vec<u8>> {
    let p = std::path::Path::new(path);
    if toolbox::is_secret(p) {
        return Err(Fail::invalid(format!("{path}: not a {what} file Misty will read")));
    }
    let meta = std::fs::metadata(p).map_err(|e| Fail::invalid(format!("{path}: {e}")))?;
    if meta.len() > 64 << 20 {
        return Err(Fail::invalid(format!("{path} is over 64 MB")));
    }
    std::fs::read(p).map_err(|e| Fail::invalid(format!("{path}: {e}")))
}

/// A request from an operation's arguments. `ask` defaults the approvals to refuse (nobody is there to answer a
/// synchronous call), `run.start` to wait for the owner.
fn req_from(misty: &Misty, a: &Value, default_approvals: Approvals) -> Res<Req> {
    let mut r = Req {
        input: os(a, "input").unwrap_or("").to_string(),
        approvals: default_approvals,
        ..Default::default()
    };
    r.session = os(a, "session").map(str::to_string);
    let names_scope = a.get("env").is_some() || a.get("cwd").is_some();
    let new_session = a["new_session"].as_bool().unwrap_or(false) && r.session.is_none();
    // the environment and folder are checked before a session is made for them
    let mut chosen = None;
    if names_scope && (r.session.is_some() || new_session) {
        let cur = match &r.session {
            Some(sid) => misty.state.session_scope(sid).map_err(io)?,
            None => (None, None),
        };
        chosen = Some(scope_args(misty, a, cur)?);
    } else if names_scope {
        // no session to remember them: they apply to this request only
        r.scope = Some(misty.scope_named(os(a, "env"), os(a, "cwd")).map_err(Fail::invalid)?);
    }
    if new_session {
        let title: String = r.input.chars().take(60).collect();
        r.session = Some(
            misty
                .state
                .new_session(if title.is_empty() { "chat" } else { &title })
                .map_err(io)?,
        );
    }
    if let (Some(sid), Some((env_id, cwd))) = (&r.session, chosen) {
        if !misty
            .state
            .set_session_scope(sid, env_id.as_deref(), cwd.as_deref())
            .map_err(io)?
        {
            return Err(Fail::not_found(format!(
                "no session {sid} (misty.session.list shows them)"
            )));
        }
    }
    if let Some(role) = os(a, "role") {
        r.role = Some(
            Role::parse(role)
                .filter(|x| x.is_brain())
                .ok_or_else(|| Fail::invalid(format!("role {role:?}: use fast, main or deep")))?,
        );
    }
    r.rag = a.get("rag").and_then(Value::as_bool);
    r.collection = os(a, "collection").map(str::to_string);
    let tools = strs(a, "tools");
    for t in &tools {
        toolbox::find(t)
            .ok_or_else(|| Fail::invalid(format!("there is no tool named {t} (misty.tools.list shows them)")))?;
    }
    if !tools.is_empty() {
        r.tools = Some(tools);
    }
    r.no_tools = a["no_tools"].as_bool().unwrap_or(false);
    r.panel = a["panel"].as_bool().unwrap_or(false);
    r.system = os(a, "system").map(str::to_string);
    r.temperature = a["temperature"].as_f64();
    r.max_tokens = a["max_tokens"].as_u64().map(|n| n.min(u32::MAX as u64) as u32);
    match os(a, "approvals") {
        None => {}
        Some("ask") => r.approvals = Approvals::Ask,
        Some("deny") => r.approvals = Approvals::Deny,
        Some(o) => return Err(Fail::invalid(format!("approvals {o:?}: use ask or deny"))),
    }
    if let Some(p) = os(a, "audio_path") {
        r.audio_wav = Some(read_media(p, "audio")?);
    } else if let Some(b) = os(a, "audio_base64") {
        r.audio_wav = Some(crate::vision::decode_base64(b).map_err(|e| Fail::invalid(e.replace("image", "audio")))?);
    }
    for p in strs(a, "image_paths") {
        r.images.push(read_media(&p, "image")?);
    }
    for b in strs(a, "images_base64") {
        r.images.push(crate::vision::decode_base64(&b).map_err(Fail::invalid)?);
    }
    if r.input.trim().is_empty() && r.audio_wav.is_none() && r.images.is_empty() {
        return Err(Fail::invalid("input is required (or audio_path, or image_paths)"));
    }
    Ok(r)
}

/// A session's environment id and folder once `a` is applied to what it has now: a key left out stays, `null` or an
/// empty string clears it. The environment may be named by id or name; the folder must exist.
fn scope_args(
    misty: &Misty,
    a: &Value,
    cur: (Option<String>, Option<String>),
) -> Res<(Option<String>, Option<String>)> {
    let env_id = match a.get("env") {
        None => cur.0,
        Some(Value::Null) => None,
        Some(Value::String(k)) if k.trim().is_empty() => None,
        Some(Value::String(k)) => Some(
            misty
                .state
                .environment(k.trim())
                .map_err(io)?
                .ok_or_else(|| Fail::not_found(format!("no environment {k:?} (misty.env.list shows them)")))?
                .id,
        ),
        Some(_) => return Err(Fail::invalid("env: an environment id or name, or null")),
    };
    let cwd = match a.get("cwd") {
        None => cur.1,
        Some(Value::Null) => None,
        Some(Value::String(c)) if c.trim().is_empty() => None,
        Some(Value::String(c)) => Some(env::check_folder(c).map_err(|e| Fail::invalid(format!("cwd: {e}")))?),
        Some(_) => return Err(Fail::invalid("cwd: a full folder path, or null")),
    };
    Ok((env_id, cwd))
}

/// A session's environment, folder and what they add up to.
fn session_view(misty: &Misty, sid: &str) -> Value {
    let (env_id, cwd) = misty.state.session_scope(sid).unwrap_or_default();
    let env_name = env_id
        .as_deref()
        .and_then(|id| misty.state.environment(id).ok().flatten())
        .map(|e| e.name);
    json!({"session": sid, "env": env_id, "env_name": env_name, "cwd": cwd, "scope": misty.scope_for(Some(sid)).describe()})
}

/// An environment from the arguments of `misty.env.save`, laid over `e` (a new one, or the one being changed).
fn environment_from(a: &Value, mut e: Environment) -> Res<Environment> {
    if let Some(n) = a.get("name").and_then(Value::as_str) {
        e.name = n.to_string();
    }
    if let Some(d) = a.get("description").and_then(Value::as_str) {
        e.description = d.to_string();
    }
    match a.get("cwd") {
        None => {}
        Some(Value::Null) => e.cwd = None,
        Some(Value::String(c)) => e.cwd = Some(c.clone()),
        Some(_) => return Err(Fail::invalid("cwd: a full folder path, or null")),
    }
    match a.get("roots") {
        None => {}
        Some(Value::Array(v)) => {
            e.roots = v
                .iter()
                .map(|x| {
                    x.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| Fail::invalid("roots: a list of folder paths"))
                })
                .collect::<Res<_>>()?
        }
        Some(_) => return Err(Fail::invalid("roots: a list of folder paths")),
    }
    match a.get("vars") {
        None => {}
        Some(Value::Object(m)) => {
            e.vars = m
                .iter()
                .map(|(k, v)| match v {
                    Value::String(x) => Ok((k.clone(), x.clone())),
                    _ => Err(Fail::invalid(format!("variable {k}: the value must be text"))),
                })
                .collect::<Res<_>>()?
        }
        Some(_) => return Err(Fail::invalid("vars: an object, name to text")),
    }
    if let Some(p) = a.get("policy") {
        e.policy = serde_json::from_value(p.clone()).map_err(|x| {
            Fail::invalid(format!(
                "policy: {x} (the classes are read, write, exec, net, hub; each auto, ask or deny)"
            ))
        })?;
    }
    Ok(e)
}

fn outcome(o: &agent::Outcome) -> Value {
    json!({"run": o.run, "status": o.status, "text": o.text, "role": o.role, "citations": o.citations, "usage": o.usage,
           "session": o.session, "ms": o.ms, "error": o.error})
}

fn ctx_of(hub: &Hub, misty: &Arc<Misty>) -> Ctx {
    Ctx {
        hub: hub.clone(),
        misty: misty.clone(),
        llm: misty.llm(hub),
    }
}

fn merge(cur: &mut Value, patch: &Value) {
    match (cur, patch) {
        (Value::Object(c), Value::Object(p)) => {
            for (k, v) in p {
                // JSON Merge Patch: null takes the key away (a setting goes back to its default, a role override is
                // dropped). Without it a map such as `roles` could be added to and never emptied.
                if v.is_null() {
                    c.remove(k);
                    continue;
                }
                match c.get_mut(k) {
                    Some(x) if x.is_object() && v.is_object() => merge(x, v),
                    _ => {
                        c.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (c, p) => *c = p.clone(),
    }
}

// ---- dispatch ----------------------------------------------------------------------------------------------------------

pub async fn call(hub: &Hub, id: &str, a: &Value) -> Res<Value> {
    let misty = Misty::get(hub)?;
    misty.warm().await;
    let cfg = misty.cfg.read().clone();
    let collection = os(a, "collection").unwrap_or(&cfg.collection).to_string();
    Ok(match id {
        "misty.status" => misty.report(hub).await,
        "misty.doctor" => setup::doctor(hub, &misty).await,
        "misty.setup.plan" => setup::plan(hub, &misty),
        "misty.setup.run" => setup::run(hub, &misty, &strs(a, "install"), os(a, "pack")).await?,
        "misty.ask" => {
            let req = req_from(&misty, a, Approvals::Deny)?;
            let ctx = ctx_of(hub, &misty);
            let run = misty.runs.create();
            outcome(&agent::run(&ctx, req, run).await)
        }
        "misty.run.start" => {
            let req = req_from(&misty, a, Approvals::Ask)?;
            let session = req.session.clone();
            let ctx = ctx_of(hub, &misty);
            let run = misty.runs.create();
            let id = run.id.clone();
            tokio::spawn(async move {
                agent::run(&ctx, req, run).await;
            });
            json!({"run": id, "session": session})
        }
        "misty.run.get" => {
            run_get(
                &misty,
                s(a, "run")?,
                a["from"].as_u64().unwrap_or(0) as usize,
                a["wait_ms"].as_u64().unwrap_or(0).min(25_000),
            )
            .await?
        }
        "misty.run.cancel" => {
            let r = misty
                .runs
                .get(s(a, "run")?)
                .ok_or_else(|| Fail::not_found("no such run (or it ended long ago)"))?;
            let was = !r.is_done();
            r.cancel();
            json!({"run": r.id, "cancelled": was})
        }
        "misty.run.list" => {
            let limit = a["limit"].as_u64().unwrap_or(20) as usize;
            let active: Vec<Value> = misty.runs.active().iter().map(|r| json!({"id": r.id, "status": "running", "started": r.started_unix, "pending": r.pending().len()})).collect();
            json!({"active": active, "recent": misty.state.runs(limit).map_err(io)?})
        }
        "misty.approvals" => json!({"pending": misty.runs.pending()}),
        "misty.approve" => {
            let (approval, allow) = (
                s(a, "approval")?,
                a["allow"]
                    .as_bool()
                    .ok_or_else(|| Fail::invalid("allow is required: true or false"))?,
            );
            let done = misty.runs.active().iter().any(|r| r.answer(approval, allow));
            if !done {
                return Err(Fail::not_found(
                    "no approval is waiting under that id (it may have timed out)",
                ));
            }
            json!({"approval": approval, "allowed": allow})
        }
        "misty.session.list" => {
            json!({"sessions": misty.state.sessions(a["limit"].as_u64().unwrap_or(50) as usize).map_err(io)?})
        }
        "misty.session.new" => {
            let (env_id, cwd) = scope_args(&misty, a, (None, None))?;
            let sid = misty.state.new_session(os(a, "title").unwrap_or("chat")).map_err(io)?;
            if env_id.is_some() || cwd.is_some() {
                misty
                    .state
                    .set_session_scope(&sid, env_id.as_deref(), cwd.as_deref())
                    .map_err(io)?;
            }
            json!({"session": sid, "env": env_id, "cwd": cwd})
        }
        "misty.session.set" => {
            let sid = s(a, "session")?;
            if !misty.state.has_session(sid) {
                return Err(Fail::not_found(format!("no session {sid}")));
            }
            let cur = misty.state.session_scope(sid).map_err(io)?;
            let (env_id, cwd) = scope_args(&misty, a, cur)?;
            misty
                .state
                .set_session_scope(sid, env_id.as_deref(), cwd.as_deref())
                .map_err(io)?;
            session_view(&misty, sid)
        }
        "misty.env.list" => json!({"environments": misty.state.environments().map_err(io)?}),
        "misty.env.save" => {
            let base = match os(a, "id") {
                Some(id) => misty
                    .state
                    .environment(id)
                    .map_err(io)?
                    .filter(|e| e.id == id)
                    .ok_or_else(|| Fail::not_found(format!("no environment {id}")))?,
                None => Environment::default(),
            };
            let mut e = environment_from(a, base)?;
            e.check().map_err(Fail::invalid)?;
            misty.state.save_environment(&mut e).map_err(Fail::invalid)?;
            json!({"environment": e})
        }
        "misty.env.delete" => {
            let key = s(a, "id")?;
            let e = misty
                .state
                .environment(key)
                .map_err(io)?
                .ok_or_else(|| Fail::not_found(format!("no environment {key}")))?;
            json!({"deleted": misty.state.delete_environment(&e.id).map_err(io)?, "id": e.id, "name": e.name})
        }
        "misty.browse" => env::browse(os(a, "path"), a["hidden"].as_bool().unwrap_or(false)).map_err(Fail::invalid)?,
        "misty.session.get" => {
            let sid = s(a, "session")?;
            if !misty.state.has_session(sid) {
                return Err(Fail::not_found(format!("no session {sid}")));
            }
            let msgs: Vec<Value> = misty
                .state
                .history(sid, a["limit"].as_u64().unwrap_or(200) as usize)
                .map_err(io)?
                .into_iter()
                .map(|(r, c)| json!({"role": r, "content": c}))
                .collect();
            json!({"session": sid, "messages": msgs})
        }
        "misty.session.rename" => json!({"renamed": misty.state.rename(s(a, "session")?, s(a, "title")?).map_err(io)?}),
        "misty.session.delete" => json!({"deleted": misty.state.delete_session(s(a, "session")?).map_err(io)?}),
        "misty.route" => {
            let (fleet, res) = misty.fleet(hub).await;
            let text = s(a, "input")?;
            let mut f = router::Features::of(text);
            f.rag_hit = misty.rag.strength(&collection, text);
            let r = router::route(&f, &fleet, cfg.reflex);
            json!({"features": f, "route": r, "ready": fleet.ready, "warm": fleet.warm, "roster": res})
        }
        "misty.rag.ingest" => {
            if let Some(path) = os(a, "path") {
                let p = std::path::Path::new(path);
                // A missing path used to index zero files and say nothing: a typo looked like an empty folder.
                if !p.exists() {
                    return Err(Fail::not_found(format!(
                        "nothing at {path} (a relative path is read from where the command ran; use a full path)"
                    )));
                }
                misty.ingest_path(hub, &collection, p).await.map_err(io)?
            } else {
                misty
                    .ingest_text(hub, &collection, s(a, "uri")?, s(a, "text")?)
                    .await
                    .map_err(io)?
            }
        }
        "misty.rag.search" => {
            let hits = misty
                .search(
                    hub,
                    &collection,
                    s(a, "query")?,
                    a["k"].as_u64().unwrap_or(cfg.rag_top_k as u64) as usize,
                )
                .await
                .map_err(io)?;
            json!({"collection": collection, "hits": hits})
        }
        "misty.rag.sources" => json!({"sources": misty.rag.sources(os(a, "collection")).map_err(io)?,
                                      "collections": misty.rag.collections().map_err(io)?.into_iter().map(|(k, (s, c))| json!({"collection": k, "sources": s, "chunks": c})).collect::<Vec<_>>()}),
        "misty.rag.remove" => {
            if a["all"].as_bool().unwrap_or(false) {
                json!({"removed_sources": misty.rag.remove_collection(&collection).map_err(io)?})
            } else {
                json!({"removed": misty.rag.remove_source(&collection, s(a, "source")?).map_err(io)?})
            }
        }
        "misty.tools.list" => {
            json!({"tools": toolbox::specs().iter().map(|t| json!({"name": t.name, "class": t.class, "policy": t.class.policy(&cfg.policy),
                                      "description": t.description, "parameters": t.parameters})).collect::<Vec<_>>(), "policy": cfg.policy})
        }
        "misty.config.get" => json!({"config": cfg, "path": Config::path(&misty.dir)}),
        "misty.config.set" => {
            let patch = a
                .get("patch")
                .filter(|p| p.is_object())
                .ok_or_else(|| Fail::invalid("patch must be an object"))?;
            let mut cur = serde_json::to_value(&cfg).map_err(io)?;
            for k in patch
                .as_object()
                .map(|o| o.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
            {
                if cur.get(&k).is_none() {
                    return Err(Fail::invalid(format!("no setting {k} (misty.config.get shows them)")));
                }
            }
            merge(&mut cur, patch);
            let new: Config = serde_json::from_value(cur).map_err(|e| Fail::invalid(e.to_string()))?;
            new.check().map_err(Fail::invalid)?;
            new.save(&misty.dir).map_err(io)?;
            misty.cactus.set_threads(new.threads as i32);
            *misty.cfg.write() = new.clone();
            json!({"config": new})
        }
        "misty.pipeline.list" => json!({"pipelines": pipeline::list(&misty)}),
        "misty.pipeline.show" => json!({"pipeline": pipeline::load(&misty, s(a, "name")?).map_err(Fail::invalid)?}),
        "misty.pipeline.save" => {
            let p = pipeline::save(&misty, s(a, "toml")?).map_err(Fail::invalid)?;
            json!({"saved": p.name, "steps": p.steps.len()})
        }
        "misty.pipeline.run" => {
            let p = pipeline::load(&misty, s(a, "name")?).map_err(Fail::invalid)?;
            let inputs = a.get("inputs").cloned().unwrap_or(json!({}));
            let ctx = ctx_of(hub, &misty);
            if a["wait"].as_bool().unwrap_or(false) {
                pipeline::run_pipeline(&ctx, &p, &inputs, &|_, _, _| {})
                    .await
                    .map_err(|e| Fail::new(502, "pipeline_failed", e))?
            } else {
                let label = format!("pipeline {}", p.name);
                let job = hub.jobs.start_task("misty", label, move |job| async move {
                    let j = job.clone();
                    pipeline::run_pipeline(&ctx, &p, &inputs, &move |d, t, n| j.progress(d, t, n))
                        .await
                        .map_err(|e| Fail::new(502, "pipeline_failed", e))
                });
                json!({"job": job.id, "follow": "job.get {job}"})
            }
        }
        "misty.bench" => {
            setup::bench(
                hub,
                &misty,
                misty.llm(hub).as_ref(),
                a["tokens"].as_u64().unwrap_or(64) as u32,
            )
            .await
        }
        _ => return Err(Fail::not_found(format!("no operation {id}"))),
    })
}

async fn run_get(misty: &Misty, id: &str, from: usize, wait_ms: u64) -> Res<Value> {
    if let Some(r) = misty.runs.get(id) {
        let mut events = r.events();
        if events.len() <= from && !r.is_done() && wait_ms > 0 {
            if let Ok(Some(more)) = tokio::time::timeout(Duration::from_millis(wait_ms), r.next(from)).await {
                events = r.events();
                let _ = more;
            }
        }
        let total = events.len();
        let slice: Vec<Value> = events.into_iter().skip(from).collect();
        return Ok(
            json!({"run": id, "events": slice, "next": total, "done": r.is_done() && from + slice.len() >= total,
                         "running": !r.is_done(), "pending": r.pending()}),
        );
    }
    // a run from an earlier session of the hub: the stored record (without token events)
    let rec = misty
        .state
        .run(id)
        .map_err(io)?
        .ok_or_else(|| Fail::not_found(format!("no run {id}")))?;
    let events: Vec<Value> = rec["events"].as_array().cloned().unwrap_or_default();
    let total = events.len();
    Ok(
        json!({"run": id, "events": events.into_iter().skip(from).collect::<Vec<_>>(), "next": total, "done": true, "running": false,
              "pending": [], "stored": true, "text": rec["output"], "status": rec["status"]}),
    )
}

// ---- REST --------------------------------------------------------------------------------------------------------------

const NUMBERS: [&str; 6] = ["from", "wait_ms", "limit", "k", "tokens", "max_tokens"];
const BOOLS: [&str; 8] = [
    "all",
    "allow",
    "rag",
    "no_tools",
    "new_session",
    "wait",
    "panel",
    "history",
];

pub fn query_args(q: HashMap<String, String>) -> Value {
    let mut o = serde_json::Map::new();
    for (k, v) in q {
        let val = if NUMBERS.contains(&k.as_str()) && v.parse::<u64>().is_ok() {
            json!(v.parse::<u64>().unwrap_or(0))
        } else if BOOLS.contains(&k.as_str()) && matches!(v.as_str(), "true" | "false") {
            json!(v == "true")
        } else {
            json!(v)
        };
        o.insert(k, val);
    }
    Value::Object(o)
}

async fn run_op(hub: Hub, op: String, args: Value) -> Result<Json<Value>, ApiError> {
    if !args.is_object() {
        return Err(Fail::invalid("the body must be a JSON object").into());
    }
    let r = crate::hub::call(&hub, &op, &args).await?;
    Ok(Json(json!({"result": r})))
}

/// The run's events as server-sent events, from `from` on, until the run is over.
async fn stream(State(hub): State<Hub>, AxQuery(q): AxQuery<HashMap<String, String>>) -> Result<Response, ApiError> {
    let misty = Misty::get(&hub)?;
    let id = q
        .get("run")
        .filter(|r| !r.is_empty())
        .ok_or_else(|| Fail::invalid("run is required"))?;
    let run = misty
        .runs
        .get(id)
        .ok_or_else(|| Fail::not_found("no such live run (a finished run from before a restart: misty.run.get)"))?;
    let from = q.get("from").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
    let body = Body::from_stream(futures::stream::unfold(
        (run, from, false),
        |(run, from, over)| async move {
            if over {
                return None;
            }
            match run.next(from).await {
                Some(events) => {
                    let mut text = String::new();
                    for e in &events {
                        text.push_str(&format!(
                            "event: {}\ndata: {}\n\n",
                            e["type"].as_str().unwrap_or("message"),
                            e
                        ));
                    }
                    let n = from + events.len();
                    Some((Ok::<Bytes, Infallible>(Bytes::from(text)), (run, n, false)))
                }
                None => Some((Ok(Bytes::from("event: end\ndata: {}\n\n")), (run, from, true))),
            }
        },
    ));
    Ok(Response::builder()
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(body)
        .map_err(|e| Fail::new(500, "io", e.to_string()))?)
}

/// `misty.run.get` is `/v1/misty/run/get`.
pub fn route_of(op: &str) -> String {
    format!("/v1/{}", op.replace('.', "/"))
}

pub fn routes() -> Router<Hub> {
    let mut r = Router::new().route("/v1/misty/run/stream", get(stream));
    for o in operations() {
        let id = o["id"].as_str().unwrap_or("").to_string();
        let path = route_of(&id);
        let (idp, idg) = (id.clone(), id.clone());
        let posting = post(move |State(hub): State<Hub>, body: Option<Json<Value>>| {
            let (op, args) = (idp.clone(), body.map(|Json(v)| v).unwrap_or(json!({})));
            async move { run_op(hub, op, args).await }
        });
        let method = if o["mutating"].as_bool().unwrap_or(true) {
            posting
        } else {
            posting.merge(get(
                move |State(hub): State<Hub>, AxQuery(q): AxQuery<HashMap<String, String>>| {
                    let op = idg.clone();
                    async move { run_op(hub, op, query_args(q)).await }
                },
            ))
        };
        r = r.route(&path, method);
    }
    r
}

// ---- the OpenAI virtual models -----------------------------------------------------------------------------------------

pub const VIRTUAL: [&str; 4] = ["misty", "misty-fast", "misty-main", "misty-deep"];

/// Does this model name mean the agent? A model of the catalog with the same name wins (the native Misty core, say).
pub fn is_virtual(hub: &Hub, name: &str) -> bool {
    let n = name.trim().to_lowercase();
    VIRTUAL.contains(&n.as_str()) && !hub.engine.catalog().iter().any(|m| m.id.to_lowercase() == n)
}

/// A peer key reaches Misty only when its allow-list names her: a key with no list (every model) does not get her too.
pub fn peer_may_use(p: &Permit, name: &str) -> bool {
    !p.allow_list().is_empty() && p.allows(name, name)
}

/// `/v1/models` entries for the virtual models (those the catalog does not already have).
pub fn virtual_models(hub: &Hub) -> Vec<Value> {
    VIRTUAL
        .iter()
        .filter(|n| is_virtual(hub, n))
        .map(|n| {
            json!({"id": n, "object": "model", "created": 0, "owned_by": "modelmistress",
                   "meta": {"agent": true, "about": "Misty: routes between local models, retrieves from your index, uses tools under your policy"}})
        })
        .collect()
}

pub(super) fn content_text(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

struct Guard(Arc<Run>);

impl Drop for Guard {
    fn drop(&mut self) {
        // a client that went away takes its run with it
        if !self.0.is_done() {
            self.0.cancel();
        }
    }
}

fn chunk(id: &str, model: &str, delta: Value, finish: Value, extra: Option<(&str, Value)>) -> String {
    let mut v = json!({"id": id, "object": "chat.completion.chunk", "created": chrono::Utc::now().timestamp(), "model": model,
                       "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]});
    if let Some((k, x)) = extra {
        v[k] = x;
    }
    format!("data: {v}\n\n")
}

pub async fn openai_chat(hub: Hub, permit: Option<Arc<Permit>>, body: Value) -> Res<Response> {
    let misty = Misty::get(&hub)?;
    misty.warm().await;
    let model = body["model"].as_str().unwrap_or("misty").to_string();
    if body
        .get("tools")
        .map(|t| t.as_array().map(|a| !a.is_empty()).unwrap_or(!t.is_null()))
        .unwrap_or(false)
    {
        return Err(Fail::invalid("Misty runs her own tools: remove `tools` from the request (misty.tools.list shows the ones she has), or use a plain model for client-side function calling"));
    }
    let msgs = body["messages"]
        .as_array()
        .filter(|m| !m.is_empty())
        .ok_or_else(|| Fail::invalid("messages is required"))?;
    let (mut system, mut history, mut images, mut audio) = (vec![], vec![], vec![], None);
    for m in &msgs[..msgs.len() - 1] {
        match m["role"].as_str() {
            Some("system") | Some("developer") => system.push(content_text(&m["content"])),
            Some(r @ ("user" | "assistant")) => {
                history.push(json!({"role": r, "content": content_text(&m["content"])}))
            }
            _ => {}
        }
    }
    let last = &msgs[msgs.len() - 1];
    if last["role"] != "user" {
        return Err(Fail::invalid("the last message must be from the user"));
    }
    for p in last["content"].as_array().into_iter().flatten() {
        match p["type"].as_str() {
            Some("image_url") => {
                let url = p["image_url"]["url"].as_str().or(p["image_url"].as_str()).unwrap_or("");
                let b64 = url
                    .split_once("base64,")
                    .map(|x| x.1)
                    .ok_or_else(|| Fail::invalid("images must be data: URIs with base64"))?;
                images.push(crate::vision::decode_base64(b64).map_err(Fail::invalid)?);
            }
            Some("input_audio") => {
                audio = Some(
                    crate::vision::decode_base64(p["input_audio"]["data"].as_str().unwrap_or(""))
                        .map_err(|e| Fail::invalid(e.replace("image", "audio")))?,
                );
            }
            _ => {}
        }
    }
    let ext = body.get("misty").cloned().unwrap_or(json!({}));
    let peer = permit.is_some();
    let req = Req {
        input: content_text(&last["content"]),
        session: if peer {
            None
        } else {
            os(&ext, "session").map(str::to_string)
        },
        history,
        system: Some(system.join("\n")).filter(|s| !s.trim().is_empty()),
        collection: os(&ext, "collection").map(str::to_string),
        // a peer reaches chat and routing only: no files, no tools, no index
        rag: if peer {
            Some(false)
        } else {
            ext.get("rag").and_then(Value::as_bool)
        },
        no_tools: peer || ext["no_tools"].as_bool().unwrap_or(false),
        classes: if peer { Some(vec![]) } else { None },
        role: router::role_of_model(&model),
        approvals: Approvals::Deny,
        audio_wav: audio,
        images,
        panel: ext["panel"].as_bool().unwrap_or(false),
        temperature: body["temperature"].as_f64(),
        max_tokens: body["max_tokens"]
            .as_u64()
            .or(body["max_completion_tokens"].as_u64())
            .map(|n| n.min(u32::MAX as u64) as u32),
        ..Default::default()
    };
    if req.input.trim().is_empty() && req.audio_wav.is_none() && req.images.is_empty() {
        return Err(Fail::invalid("the last user message is empty"));
    }
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let ctx = ctx_of(&hub, &misty);
    let run = misty.runs.create();
    let finish = move |p: &Option<Arc<Permit>>, model: &str, o: &agent::Outcome| {
        if let Some(p) = p {
            p.finish(
                model,
                o.status == "done",
                o.usage["prompt_tokens"].as_u64().unwrap_or(0),
                o.usage["completion_tokens"].as_u64().unwrap_or(0),
            );
        }
    };
    let json_resp = |status: u16, v: Value| {
        Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .map_err(|e| Fail::new(500, "io", e.to_string()))
    };
    if !body["stream"].as_bool().unwrap_or(false) {
        let o = agent::run(&ctx, req, run).await;
        finish(&permit, &model, &o);
        if o.status != "done" {
            return Err(Fail::new(
                502,
                "misty_failed",
                o.error.unwrap_or_else(|| o.status.to_string()),
            ));
        }
        let (pt, ct) = (
            o.usage["prompt_tokens"].as_u64().unwrap_or(0),
            o.usage["completion_tokens"].as_u64().unwrap_or(0),
        );
        return json_resp(
            200,
            json!({"id": id, "object": "chat.completion", "created": chrono::Utc::now().timestamp(), "model": model,
                   "choices": [{"index": 0, "message": {"role": "assistant", "content": o.text}, "finish_reason": "stop"}],
                   "usage": {"prompt_tokens": pt, "completion_tokens": ct, "total_tokens": pt + ct},
                   "x_misty": {"role": o.role, "run": o.run, "citations": o.citations}}),
        );
    }
    let usage = body["stream_options"]["include_usage"].as_bool().unwrap_or(false);
    let guard = Guard(run.clone());
    let task_run = run.clone();
    let (m2, p2) = (model.clone(), permit.clone());
    tokio::spawn(async move {
        let o = agent::run(&ctx, req, task_run).await;
        finish(&p2, &m2, &o);
    });
    let (mid, mname) = (id.clone(), model.clone());
    let body = Body::from_stream(futures::stream::unfold(
        (run, 0usize, 0u8, guard),
        move |(run, from, phase, guard)| {
            let (id, model) = (mid.clone(), mname.clone());
            async move {
                match phase {
                    0 => Some((
                        Ok::<Bytes, Infallible>(Bytes::from(chunk(
                            &id,
                            &model,
                            json!({"role": "assistant", "content": ""}),
                            Value::Null,
                            None,
                        ))),
                        (run, from, 1, guard),
                    )),
                    1 => match run.next(from).await {
                        None => Some((Ok(Bytes::from("data: [DONE]\n\n")), (run, from, 2, guard))),
                        Some(events) => {
                            let mut out = String::new();
                            let mut ended = false;
                            for e in &events {
                                match e["type"].as_str() {
                                    Some("token") => out.push_str(&chunk(
                                        &id,
                                        &model,
                                        json!({"content": e["text"]}),
                                        Value::Null,
                                        None,
                                    )),
                                    Some("route") | Some("retrieve") | Some("tool") | Some("escalate")
                                    | Some("reflex") => out.push_str(&chunk(
                                        &id,
                                        &model,
                                        json!({}),
                                        Value::Null,
                                        Some(("x_misty", e.clone())),
                                    )),
                                    Some("final") => {
                                        ended = true;
                                        if e["status"] == "done" {
                                            out.push_str(&chunk(
                                                &id,
                                                &model,
                                                json!({}),
                                                json!("stop"),
                                                Some((
                                                    "x_misty",
                                                    json!({"role": e["role"], "citations": e["citations"]}),
                                                )),
                                            ));
                                            if usage {
                                                let (pt, ct) = (
                                                    e["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
                                                    e["usage"]["completion_tokens"].as_u64().unwrap_or(0),
                                                );
                                                out.push_str(&format!("data: {}\n\n", json!({"id": id, "object": "chat.completion.chunk", "model": model, "choices": [],
                                                                                          "usage": {"prompt_tokens": pt, "completion_tokens": ct, "total_tokens": pt + ct}})));
                                            }
                                        } else {
                                            out.push_str(&format!(
                                                "data: {}\n\n",
                                                json!({"error": {"message": e["error"], "type": "misty_failed"}})
                                            ));
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            if ended {
                                out.push_str("data: [DONE]\n\n");
                            }
                            Some((
                                Ok(Bytes::from(out)),
                                (run, from + events.len(), if ended { 2 } else { 1 }, guard),
                            ))
                        }
                    },
                    _ => None,
                }
            }
        },
    ));
    Response::builder()
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(body)
        .map_err(|e| Fail::new(500, "io", e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::misty::agent::tests::{rig, Rig, MODELS};

    fn rig_ops(cactus: u32) -> Rig {
        let r = rig(&MODELS, cactus);
        r.ctx.misty.set_llm(r.llm.clone());
        r
    }

    async fn c(r: &Rig, id: &str, a: Value) -> Res<Value> {
        call(&r.ctx.hub, id, &a).await
    }

    #[test]
    fn every_operation_is_a_misty_operation_with_a_schema() {
        let ops = operations();
        assert!(ops.len() >= 28);
        for o in &ops {
            let id = o["id"].as_str().unwrap();
            assert!(id.starts_with("misty."), "{id}");
            assert!(
                o["input_schema"]["type"] == "object" && o["summary"].is_string(),
                "{id}"
            );
        }
        let ids: Vec<&str> = ops.iter().map(|o| o["id"].as_str().unwrap()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "duplicate operation ids");
        assert_eq!(route_of("misty.run.get"), "/v1/misty/run/get");
    }

    #[tokio::test]
    async fn ask_answers_and_new_session_remembers_the_turn() {
        let r = rig_ops(0);
        r.llm.say("Hello there.");
        let v = c(&r, "misty.ask", json!({"input": "hello", "new_session": true}))
            .await
            .unwrap();
        assert_eq!(
            (v["status"].as_str(), v["text"].as_str(), v["role"].as_str()),
            (Some("done"), Some("Hello there."), Some("fast"))
        );
        let sid = v["session"].as_str().unwrap().to_string();
        let got = c(&r, "misty.session.get", json!({"session": sid})).await.unwrap();
        assert_eq!(got["messages"].as_array().unwrap().len(), 2);
        assert_eq!(
            c(&r, "misty.session.list", json!({})).await.unwrap()["sessions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            c(&r, "misty.session.rename", json!({"session": sid, "title": "x"}))
                .await
                .unwrap()["renamed"],
            true
        );
        assert_eq!(
            c(&r, "misty.session.delete", json!({"session": sid})).await.unwrap()["deleted"],
            true
        );
        assert_eq!(
            c(&r, "misty.session.get", json!({"session": sid}))
                .await
                .unwrap_err()
                .status,
            404
        );
        // bad arguments name the problem
        assert!(c(&r, "misty.ask", json!({}))
            .await
            .unwrap_err()
            .message
            .contains("input is required"));
        assert!(c(&r, "misty.ask", json!({"input": "x", "role": "huge"}))
            .await
            .unwrap_err()
            .message
            .contains("fast, main or deep"));
        assert!(c(&r, "misty.ask", json!({"input": "x", "tools": ["nope"]}))
            .await
            .unwrap_err()
            .message
            .contains("no tool named nope"));
    }

    fn folder(r: &Rig, name: &str) -> String {
        let p = r.dir.join(name);
        std::fs::create_dir_all(&p).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn environments_are_made_changed_listed_and_deleted_and_bad_ones_say_why() {
        let r = rig_ops(0);
        let app = folder(&r, "app");
        let made = c(
            &r,
            "misty.env.save",
            json!({"name": " App ", "cwd": app, "vars": {"MODE": "dev"}, "policy": {"exec": "ask"}}),
        )
        .await
        .unwrap();
        let e = &made["environment"];
        let id = e["id"].as_str().unwrap().to_string();
        assert_eq!(
            (e["name"].as_str(), e["cwd"].as_str(), e["policy"]["exec"].as_str()),
            (Some("App"), Some(app.as_str()), Some("ask"))
        );
        assert!(
            e["policy"].get("write").is_none(),
            "a class left out keeps Misty's own policy"
        );

        // change one field by id; the rest stays
        let data = folder(&r, "data");
        let changed = c(
            &r,
            "misty.env.save",
            json!({"id": id, "description": "the app", "roots": [data]}),
        )
        .await
        .unwrap();
        let e = &changed["environment"];
        assert_eq!(
            (
                e["description"].as_str(),
                e["vars"]["MODE"].as_str(),
                e["roots"][0].as_str()
            ),
            (Some("the app"), Some("dev"), Some(data.as_str()))
        );
        assert_eq!(
            c(&r, "misty.env.list", json!({})).await.unwrap()["environments"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        // vars replace as a whole, and cwd can be cleared
        let cleared = c(&r, "misty.env.save", json!({"id": id, "vars": {}, "cwd": null}))
            .await
            .unwrap();
        assert!(
            cleared["environment"]["cwd"].is_null() && cleared["environment"]["vars"].as_object().unwrap().is_empty()
        );

        // what cannot work is refused with the reason
        let bad = |args: Value| {
            let r = &r;
            async move { c(r, "misty.env.save", args).await.unwrap_err() }
        };
        let missing = r.dir.join("missing").to_string_lossy().into_owned();
        assert!(bad(json!({"name": "x", "cwd": missing}))
            .await
            .message
            .contains("no such folder"));
        assert!(
            bad(json!({"name": "app"})).await.message.contains("already exists"),
            "names are unique in any case"
        );
        assert!(bad(json!({"name": "x", "policy": {"exce": "ask"}}))
            .await
            .message
            .contains("exce"));
        assert!(bad(json!({"name": "x", "policy": {"exec": "always"}}))
            .await
            .message
            .contains("always"));
        assert!(bad(json!({"name": "x", "vars": {"N": 5}}))
            .await
            .message
            .contains("must be text"));
        assert!(bad(json!({"name": "x", "vars": {"1BAD": "v"}}))
            .await
            .message
            .contains("variable"));
        assert!(bad(json!({"description": "no name"}))
            .await
            .message
            .contains("needs a name"));
        assert_eq!(bad(json!({"id": "nope0000", "name": "x"})).await.status, 404);

        // delete by name or id
        let gone = c(&r, "misty.env.delete", json!({"id": "APP"})).await.unwrap();
        assert_eq!(
            (gone["deleted"].as_bool(), gone["id"].as_str()),
            (Some(true), Some(id.as_str()))
        );
        assert_eq!(
            c(&r, "misty.env.delete", json!({"id": "app"}))
                .await
                .unwrap_err()
                .status,
            404
        );
        assert!(c(&r, "misty.env.list", json!({})).await.unwrap()["environments"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn a_session_gets_an_environment_and_folder_when_made_and_can_change_them_later() {
        let r = rig_ops(0);
        let (app, other) = (folder(&r, "app"), folder(&r, "other"));
        let env = c(&r, "misty.env.save", json!({"name": "App", "cwd": app}))
            .await
            .unwrap()["environment"]
            .clone();
        let sid = c(&r, "misty.session.new", json!({"title": "work", "env": "app"}))
            .await
            .unwrap()["session"]
            .as_str()
            .unwrap()
            .to_string();
        let listed = |r: &Rig| {
            let r = r.ctx.hub.clone();
            async move { call(&r, "misty.session.list", &json!({})).await.unwrap()["sessions"][0].clone() }
        };
        let s = listed(&r).await;
        assert_eq!(
            (s["env"].as_str(), s["env_name"].as_str(), s["cwd"].is_null()),
            (env["id"].as_str(), Some("App"), true)
        );

        // a folder of its own wins over the environment's, and the answer says what that adds up to
        let set = c(&r, "misty.session.set", json!({"session": sid, "cwd": other}))
            .await
            .unwrap();
        assert_eq!(set["scope"]["cwd"].as_str(), Some(other.as_str()));
        assert_eq!(set["env_name"], "App", "a field left out stays");
        // null clears one: back to the environment's folder
        let set = c(&r, "misty.session.set", json!({"session": sid, "cwd": null}))
            .await
            .unwrap();
        assert_eq!(
            (set["cwd"].is_null(), set["scope"]["cwd"].as_str()),
            (true, Some(app.as_str()))
        );
        // and with neither, Misty's own workspace and settings
        let set = c(&r, "misty.session.set", json!({"session": sid, "env": null}))
            .await
            .unwrap();
        assert!(set["env"].is_null() && set["scope"]["cwd"].is_null());
        assert_eq!(set["scope"]["policy"]["exec"], "deny");

        // refused with the reason, and nothing changes
        let before = listed(&r).await;
        assert_eq!(
            c(&r, "misty.session.set", json!({"session": sid, "env": "nope"}))
                .await
                .unwrap_err()
                .status,
            404
        );
        let missing = r.dir.join("missing").to_string_lossy().into_owned();
        assert!(c(&r, "misty.session.set", json!({"session": sid, "cwd": missing}))
            .await
            .unwrap_err()
            .message
            .contains("no such folder"));
        assert!(c(&r, "misty.session.set", json!({"session": sid, "cwd": "relative"}))
            .await
            .unwrap_err()
            .message
            .contains("full path"));
        assert_eq!(
            c(&r, "misty.session.set", json!({"session": "nope"}))
                .await
                .unwrap_err()
                .status,
            404
        );
        assert_eq!(listed(&r).await, before);

        // deleting the environment leaves the session working
        c(
            &r,
            "misty.session.set",
            json!({"session": sid, "env": "App", "cwd": other}),
        )
        .await
        .unwrap();
        c(&r, "misty.env.delete", json!({"id": "App"})).await.unwrap();
        let s = listed(&r).await;
        assert_eq!((s["env"].is_null(), s["cwd"].as_str()), (true, Some(other.as_str())));
    }

    #[tokio::test]
    async fn a_run_remembers_the_environment_and_folder_it_started_with_and_a_bad_one_makes_no_session() {
        let r = rig_ops(0);
        let (app, other) = (folder(&r, "app"), folder(&r, "other"));
        c(&r, "misty.env.save", json!({"name": "App", "cwd": app}))
            .await
            .unwrap();
        r.llm.say("hello");
        let v = c(
            &r,
            "misty.ask",
            json!({"input": "hello", "new_session": true, "env": "App", "cwd": other}),
        )
        .await
        .unwrap();
        assert_eq!(v["status"], "done", "{v}");
        let sid = v["session"].as_str().unwrap().to_string();
        let s = c(&r, "misty.session.list", json!({})).await.unwrap()["sessions"][0].clone();
        assert_eq!(
            (s["env_name"].as_str(), s["cwd"].as_str()),
            (Some("App"), Some(other.as_str()))
        );

        // naming another folder for a session that exists moves it
        r.llm.say("again");
        c(&r, "misty.ask", json!({"input": "again", "session": sid, "cwd": app}))
            .await
            .unwrap();
        let s = c(&r, "misty.session.list", json!({})).await.unwrap()["sessions"][0].clone();
        assert_eq!(s["cwd"].as_str(), Some(app.as_str()));

        // an environment that is not there stops the request before a session is made for it
        let e = c(
            &r,
            "misty.ask",
            json!({"input": "x", "new_session": true, "env": "nope"}),
        )
        .await
        .unwrap_err();
        assert_eq!(e.status, 404);
        assert_eq!(
            c(&r, "misty.session.list", json!({})).await.unwrap()["sessions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        // a request with no session uses them for that request alone
        r.llm.say("one-off");
        let v = c(&r, "misty.ask", json!({"input": "hi", "env": "App"})).await.unwrap();
        assert_eq!(v["status"], "done");
        assert_eq!(
            c(&r, "misty.session.list", json!({})).await.unwrap()["sessions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(c(&r, "misty.ask", json!({"input": "hi", "env": "nope"}))
            .await
            .unwrap_err()
            .message
            .contains("no environment"));
    }

    #[tokio::test]
    async fn browse_lists_the_folders_in_a_folder_and_the_places_to_start() {
        let r = rig_ops(0);
        let app = folder(&r, "app");
        folder(&r, "app/src");
        folder(&r, "app/.git");
        let v = c(&r, "misty.browse", json!({"path": app})).await.unwrap();
        assert_eq!(
            v["dirs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| d["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["src"]
        );
        let v = c(&r, "misty.browse", json!({"path": app, "hidden": true}))
            .await
            .unwrap();
        assert_eq!(v["dirs"].as_array().unwrap().len(), 2);
        assert!(!c(&r, "misty.browse", json!({})).await.unwrap()["places"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(
            c(&r, "misty.browse", json!({"path": "relative"}))
                .await
                .unwrap_err()
                .status,
            400
        );
    }

    #[tokio::test]
    async fn a_started_run_is_followed_and_a_write_waits_for_approve() {
        let r = rig_ops(0);
        r.llm.call("fs_write", json!({"path": "a.txt", "content": "hi"}));
        r.llm.say("saved it");
        let v = c(&r, "misty.run.start", json!({"input": "write a file a.txt with hi"}))
            .await
            .unwrap();
        let run = v["run"].as_str().unwrap().to_string();
        let approval = loop {
            let p = c(&r, "misty.approvals", json!({})).await.unwrap();
            if let Some(a) = p["pending"].as_array().and_then(|a| a.first()) {
                break a["approval"].as_str().unwrap().to_string();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert!(
            c(&r, "misty.approve", json!({"approval": "nope", "allow": true}))
                .await
                .unwrap_err()
                .status
                == 404
        );
        c(&r, "misty.approve", json!({"approval": approval, "allow": true}))
            .await
            .unwrap();
        let (mut from, mut types) = (0, vec![]);
        loop {
            let g = c(&r, "misty.run.get", json!({"run": run, "from": from, "wait_ms": 5000}))
                .await
                .unwrap();
            for e in g["events"].as_array().unwrap() {
                types.push(e["type"].as_str().unwrap().to_string());
            }
            from = g["next"].as_u64().unwrap() as usize;
            if g["done"] == true {
                break;
            }
        }
        assert!(
            types.contains(&"approval".to_string()) && types.last().map(String::as_str) == Some("final"),
            "{types:?}"
        );
        assert_eq!(
            std::fs::read_to_string(r.dir.join("misty/workspace/a.txt")).unwrap(),
            "hi"
        );
        assert!(c(&r, "misty.run.list", json!({})).await.unwrap()["recent"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["id"] == run.as_str()));
    }

    #[tokio::test]
    async fn cancel_refuses_what_is_waiting_and_a_sync_ask_never_waits_for_approval() {
        let r = rig_ops(0);
        r.llm.call("fs_write", json!({"path": "b.txt", "content": "x"}));
        r.llm.say("could not write");
        let v = c(&r, "misty.ask", json!({"input": "write a file b.txt"}))
            .await
            .unwrap();
        assert_eq!(v["status"], "done");
        assert!(!r.dir.join("misty/workspace/b.txt").exists());
        assert_eq!(
            c(&r, "misty.run.cancel", json!({"run": "nope"}))
                .await
                .unwrap_err()
                .status,
            404
        );
    }

    #[tokio::test]
    async fn config_set_merges_checks_and_saves_and_tools_show_the_policy() {
        let r = rig_ops(0);
        let t = c(&r, "misty.tools.list", json!({})).await.unwrap();
        let shell = t["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["name"] == "shell_run")
            .unwrap()
            .clone();
        assert_eq!(
            (shell["class"].as_str(), shell["policy"].as_str()),
            (Some("exec"), Some("deny"))
        );
        let v = c(
            &r,
            "misty.config.set",
            json!({"patch": {"policy": {"exec": "ask"}, "max_steps": 4}}),
        )
        .await
        .unwrap();
        assert_eq!(v["config"]["policy"]["exec"], "ask");
        assert_eq!(
            v["config"]["policy"]["write"], "ask",
            "the rest of the policy is untouched"
        );
        assert_eq!(r.ctx.misty.cfg.read().max_steps, 4);
        assert!(Config::load(&r.ctx.misty.dir).unwrap().max_steps == 4, "saved to disk");
        assert!(c(&r, "misty.config.set", json!({"patch": {"nonsense": 1}}))
            .await
            .unwrap_err()
            .message
            .contains("no setting nonsense"));
        assert!(
            c(&r, "misty.config.set", json!({"patch": {"policy": {"exec": "maybe"}}}))
                .await
                .is_err()
        );
        assert!(
            c(&r, "misty.config.set", json!({"patch": {"max_steps": 0}}))
                .await
                .is_err(),
            "the checks run before saving"
        );
        assert_eq!(r.ctx.misty.cfg.read().max_steps, 4);
        // a role override can be added, one entry dropped with null, and everything dropped with null
        let v = c(
            &r,
            "misty.config.set",
            json!({"patch": {"roles": {"main": "a-model", "fast": "b-model"}}}),
        )
        .await
        .unwrap();
        assert_eq!(v["config"]["roles"], json!({"fast": "b-model", "main": "a-model"}));
        let v = c(&r, "misty.config.set", json!({"patch": {"roles": {"main": null}}}))
            .await
            .unwrap();
        assert_eq!(v["config"]["roles"], json!({"fast": "b-model"}));
        let v = c(
            &r,
            "misty.config.set",
            json!({"patch": {"roles": null, "max_steps": null}}),
        )
        .await
        .unwrap();
        assert_eq!(v["config"]["roles"], json!({}));
        assert_eq!(v["config"]["max_steps"], 8, "null brings a setting back to its default");
    }

    #[tokio::test]
    async fn the_index_is_filled_searched_listed_and_emptied() {
        let r = rig_ops(0);
        let f = r.dir.join("misty/workspace/doc.md");
        std::fs::write(&f, "# Tides\nThe tide is highest at the full moon.\n").unwrap();
        let v = c(&r, "misty.rag.ingest", json!({"path": f.to_string_lossy()}))
            .await
            .unwrap();
        assert_eq!(v["new"], 1);
        c(
            &r,
            "misty.rag.ingest",
            json!({"uri": "note", "text": "Bring the umbrella on Thursday."}),
        )
        .await
        .unwrap();
        let h = c(&r, "misty.rag.search", json!({"query": "when is the tide highest"}))
            .await
            .unwrap();
        assert!(h["hits"][0]["text"].as_str().unwrap().contains("full moon"));
        assert_eq!(
            c(&r, "misty.rag.sources", json!({})).await.unwrap()["collections"][0]["sources"],
            2
        );
        assert_eq!(
            c(
                &r,
                "misty.rag.remove",
                json!({"collection": "default", "source": "note"})
            )
            .await
            .unwrap()["removed"],
            true
        );
        assert_eq!(
            c(&r, "misty.rag.remove", json!({"collection": "default", "all": true}))
                .await
                .unwrap()["removed_sources"],
            1
        );
        // a path that is not there is an error, not an empty report
        let e = c(
            &r,
            "misty.rag.ingest",
            json!({"path": r.dir.join("nope").to_string_lossy()}),
        )
        .await
        .unwrap_err();
        assert_eq!(e.status, 404, "{e:?}");
        // keys never go into the index
        let secret = r.dir.join("misty/workspace/keys.json");
        std::fs::write(&secret, "{\"k\": \"sk-secret\"}").unwrap();
        let v = c(&r, "misty.rag.ingest", json!({"path": secret.to_string_lossy()}))
            .await
            .unwrap();
        assert_eq!((v["new"].as_u64(), v["skipped"].as_u64()), (Some(0), Some(1)));
    }

    #[tokio::test]
    async fn route_explains_and_status_lists_the_roster() {
        let r = rig_ops(0);
        let v = c(&r, "misty.route", json!({"input": "prove that the square root of 2 is irrational, step by step, and analyze the trade-offs of each method"})).await.unwrap();
        assert_eq!(v["route"]["role"], "deep");
        assert!(!v["route"]["reasons"].as_array().unwrap().is_empty());
        let st = c(&r, "misty.status", json!({})).await.unwrap();
        assert_eq!(st["ready"], true);
        assert!(st["roster"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["role"] == "main" && x["status"] == "ready"));
        assert_eq!(st["cactus"]["available"], true);
        assert!(st["next"].as_array().unwrap().is_empty(), "{}", st["next"]);
    }

    #[tokio::test]
    async fn pipelines_are_listed_saved_and_run_to_the_end() {
        let r = rig_ops(0);
        assert!(
            c(&r, "misty.pipeline.list", json!({})).await.unwrap()["pipelines"]
                .as_array()
                .unwrap()
                .len()
                >= 4
        );
        let t = "name = 'greet'\noutput = '{{steps.hi.text}}'\ninputs = ['who']\n[[steps]]\nid = 'hi'\nkind = 'ask'\nrole = 'fast'\nprompt = 'Say hello to {{input.who}}'\n";
        assert_eq!(
            c(&r, "misty.pipeline.save", json!({"toml": t})).await.unwrap()["saved"],
            "greet"
        );
        r.llm.say("Hello Ann!");
        let v = c(
            &r,
            "misty.pipeline.run",
            json!({"name": "greet", "inputs": {"who": "Ann"}, "wait": true}),
        )
        .await
        .unwrap();
        assert_eq!(v["text"], "Hello Ann!");
        assert!(serde_json::to_string(&r.llm.seen.lock()[0].1)
            .unwrap()
            .contains("Say hello to Ann"));
        // as a job
        r.llm.say("Hello Bo!");
        let j = c(
            &r,
            "misty.pipeline.run",
            json!({"name": "greet", "inputs": {"who": "Bo"}}),
        )
        .await
        .unwrap();
        let job = r.ctx.hub.jobs.get(j["job"].as_str().unwrap()).unwrap();
        for _ in 0..200 {
            if job.status() != "running" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(job.describe()["status"], "done");
        assert_eq!(job.describe()["result"]["text"], "Hello Bo!");
        assert!(c(
            &r,
            "misty.pipeline.run",
            json!({"name": "greet", "inputs": {}, "wait": true})
        )
        .await
        .unwrap_err()
        .message
        .contains("input who is missing"));
    }

    #[tokio::test]
    async fn the_openai_route_answers_in_the_standard_shape_and_a_peer_gets_no_tools() {
        let r = rig_ops(0);
        r.llm.say("Hi!");
        let body = json!({"model": "misty-main", "messages": [{"role": "system", "content": "Be nice."}, {"role": "user", "content": "hello"}]});
        let resp = openai_chat(r.ctx.hub.clone(), None, body.clone()).await.unwrap();
        let v: Value = serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap()).unwrap();
        assert_eq!(v["object"], "chat.completion");
        assert_eq!(v["choices"][0]["message"]["content"], "Hi!");
        assert_eq!(v["usage"]["total_tokens"], 15);
        assert_eq!(v["x_misty"]["role"], "main", "misty-main forces the main role");
        assert!(r.llm.seen.lock()[0].1["messages"][0]["content"]
            .as_str()
            .unwrap()
            .starts_with("Be nice."));
        // the client's own tools are refused with a reason
        let mut with_tools = body.clone();
        with_tools["tools"] = json!([{"type": "function", "function": {"name": "x"}}]);
        assert!(openai_chat(r.ctx.hub.clone(), None, with_tools)
            .await
            .err()
            .unwrap()
            .message
            .contains("her own tools"));
        // the last message must be the user's
        assert!(openai_chat(
            r.ctx.hub.clone(),
            None,
            json!({"model": "misty", "messages": [{"role": "assistant", "content": "x"}]})
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn the_openai_stream_is_sse_chunks_ending_in_done_and_a_failure_is_an_error_event() {
        let r = rig_ops(0);
        r.llm.say("Streamed answer.");
        let body = json!({"model": "misty", "stream": true, "stream_options": {"include_usage": true}, "messages": [{"role": "user", "content": "hello"}]});
        let resp = openai_chat(r.ctx.hub.clone(), None, body).await.unwrap();
        assert_eq!(resp.headers()["content-type"], "text/event-stream");
        let text = String::from_utf8(axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap().to_vec()).unwrap();
        let events: Vec<&str> = text.split("\n\n").filter(|e| !e.is_empty()).collect();
        assert_eq!(*events.last().unwrap(), "data: [DONE]");
        assert!(events[0].contains("\"role\":\"assistant\""));
        assert!(text.contains("Streamed answer."));
        assert!(text.contains("\"finish_reason\":\"stop\"") && text.contains("\"total_tokens\":15"));
        let r2 = rig_ops(0);
        let body = json!({"model": "misty", "stream": true, "messages": [{"role": "user", "content": "hello"}]});
        // no scripted reply: the model call fails
        let text = String::from_utf8(
            axum::body::to_bytes(
                openai_chat(r2.ctx.hub.clone(), None, body).await.unwrap().into_body(),
                1 << 20,
            )
            .await
            .unwrap()
            .to_vec(),
        )
        .unwrap();
        assert!(
            text.contains("\"error\"") && text.ends_with("data: [DONE]\n\n"),
            "{text}"
        );
    }

    #[test]
    fn virtual_names_are_recognised_unless_the_catalog_has_the_name() {
        let r = rig_ops(0);
        assert!(
            is_virtual(&r.ctx.hub, "misty")
                && is_virtual(&r.ctx.hub, "MISTY-DEEP")
                && !is_virtual(&r.ctx.hub, "misty-xl")
        );
        assert_eq!(virtual_models(&r.ctx.hub).len(), 4);
        let own = crate::testing::TestDir::new("misty-own");
        let hub = crate::testing::hub(&own, &["misty"]);
        assert!(!is_virtual(&hub, "misty"), "a model of the catalog called misty wins");
        assert_eq!(virtual_models(&hub).len(), 3);
    }

    #[test]
    fn query_arguments_are_typed() {
        let q = HashMap::from([
            ("from".to_string(), "5".to_string()),
            ("allow".to_string(), "true".to_string()),
            ("run".to_string(), "abc".to_string()),
        ]);
        let v = query_args(q);
        assert_eq!(
            (v["from"].as_u64(), v["allow"].as_bool(), v["run"].as_str()),
            (Some(5), Some(true), Some("abc"))
        );
    }
}

//! The trace API: `/v1/native/...` routes and the `trace.*` operations (so MCP gets them too). Owner only: none of
//! it is in the peer allowlist.
//!
//!   GET    /v1/native/traces               list (filters: model, status, kind, q, parent, since_ms, until_ms, order;
//!                                          job, task; paging: limit, offset)
//!   GET    /v1/native/traces/{id}          the whole trace, without its tokens
//!   DELETE /v1/native/traces/{id}
//!   GET    /v1/native/traces/{id}/tokens   the tokens (offset, limit)
//!   POST   /v1/native/traces/{id}/rerun    the same request again, with overrides (the ablation hook)
//!   GET    /v1/native/diff?a=&b=           compare two traces
//!   GET    /v1/native/stream               live events (server-sent events; ?trace= for one trace)
//!   GET    /v1/native/export               every matching trace and its tokens as JSON lines
//!   GET    /v1/native/settings, POST       tracing settings (enabled, top_k, retention)

use std::collections::HashMap;
use std::convert::Infallible;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;

use super::store::Filter;
use super::{capture, diff, tracer, TraceSettings};
use crate::engine::Fail;
use crate::hub::{ApiError, Hub};

fn op(id: &str, summary: &str, mutating: bool, destructive: bool, props: Value, required: &[&str]) -> Value {
    json!({"id": id, "group": "trace", "summary": summary, "mutating": mutating, "destructive": destructive,
           "input_schema": {"type": "object", "properties": props, "required": required, "additionalProperties": false}})
}

pub fn operations() -> Vec<Value> {
    let id = json!({"type": "string", "description": "a trace id from trace.list"});
    vec![
        op(
            "trace.list",
            "List traces (the model's recorded thoughts), newest first, with filters and paging",
            false,
            false,
            json!({
                "model": {"type": "string"}, "status": {"type": "string", "description": "running, ok, error, aborted"},
                "kind": {"type": "string", "description": "chat (later: km, train, ...)"},
                "q": {"type": "string", "description": "text in the prompt, answer or reasoning"},
                "parent": {"type": "string", "description": "only reruns of this trace"},
                "job": {"type": "string", "description": "only traces of this job (a pipeline step)"},
                "task": {"type": "string", "description": "only traces of this task (a KM factory task)"},
                "since_ms": {"type": "integer"}, "until_ms": {"type": "integer"},
                "order": {"type": "string", "enum": ["asc", "desc"]},
                "limit": {"type": "integer", "minimum": 1, "maximum": 500}, "offset": {"type": "integer", "minimum": 0}
            }),
            &[],
        ),
        op(
            "trace.get",
            "One trace in full: prompt, reasoning, answer, timings, resources, adapters, stages, events, specialist calls",
            false,
            false,
            json!({"id": id, "max_events": {"type": "integer", "minimum": 0}}),
            &["id"],
        ),
        op(
            "trace.tokens",
            "A trace's tokens: text, probability, entropy, top-k alternatives, reasoning or answer",
            false,
            false,
            json!({"id": id, "offset": {"type": "integer", "minimum": 0}, "limit": {"type": "integer", "minimum": 1, "maximum": 20000}}),
            &["id"],
        ),
        op(
            "trace.rerun",
            "Run a trace's request again as a new trace (ablation: change settings or remove adapters) and diff it with the original",
            true,
            false,
            json!({
                "id": id,
                "overrides": {"type": "object", "description": "request fields to replace, e.g. {\"temperature\": 0, \"seed\": 1}; null removes one"},
                "remove_adapters": {"description": "true = every adapter off; or a list of adapter indexes or file-name parts",
                                    "oneOf": [{"type": "boolean"}, {"type": "array", "items": {"type": ["integer", "string"]}}]},
                "adapter_scales": {"type": "object", "description": "adapter index -> scale"}
            }),
            &["id"],
        ),
        op(
            "trace.diff",
            "Compare two traces: answer and reasoning word by word, settings, adapters, and where the tokens first differ",
            false,
            false,
            json!({"a": id, "b": id}),
            &["a", "b"],
        ),
        op(
            "trace.settings",
            "Show or change tracing: enabled, top_k (alternatives per token), max_age_days, max_total_mb",
            true,
            false,
            json!({
                "enabled": {"type": "boolean"}, "top_k": {"type": "integer", "minimum": 1, "maximum": 20},
                "max_age_days": {"type": "integer", "minimum": 0}, "max_total_mb": {"type": "integer", "minimum": 0}
            }),
            &[],
        ),
        op(
            "trace.export",
            "Write the matching traces and their tokens as JSON lines into <home>/trace/exports; returns the path",
            false,
            false,
            json!({
                "model": {"type": "string"}, "status": {"type": "string"}, "kind": {"type": "string"},
                "q": {"type": "string"}, "since_ms": {"type": "integer"}, "until_ms": {"type": "integer"}
            }),
            &[],
        ),
        op(
            "trace.delete",
            "Delete a trace and its tokens",
            true,
            true,
            json!({"id": id}),
            &["id"],
        ),
    ]
}

fn text<'a>(a: &'a Value, k: &str) -> Result<&'a str, Fail> {
    a.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| Fail::invalid(format!("{k} is required")))
}

fn full(hub: &Hub, id: &str, max_events: usize) -> Result<Value, Fail> {
    let tr = tracer(&hub.engine.home);
    let store = tr.store().map_err(Fail::unavailable)?;
    store
        .get(id, max_events)
        .map_err(|e| Fail::new(500, "io", e))?
        .ok_or_else(|| Fail::not_found(format!("no trace {id} (trace.list shows them)")))
}

fn io(e: String) -> Fail {
    Fail::new(500, "io", e)
}

pub async fn call(hub: &Hub, id: &str, a: &Value) -> Result<Value, Fail> {
    let tr = tracer(&hub.engine.home);
    Ok(match id {
        "trace.list" => {
            let (total, items) = tr
                .store()
                .map_err(Fail::unavailable)?
                .list(&Filter::from_json(a))
                .map_err(io)?;
            json!({"total": total, "items": items})
        }
        "trace.get" => full(hub, text(a, "id")?, a["max_events"].as_u64().unwrap_or(5000) as usize)?,
        "trace.tokens" => {
            let tid = text(a, "id")?;
            let store = tr.store().map_err(Fail::unavailable)?;
            if !store.exists(tid) {
                return Err(Fail::not_found(format!("no trace {tid}")));
            }
            let all = store.tokens(tid);
            let off = (a["offset"].as_u64().unwrap_or(0) as usize).min(all.len());
            let lim = a["limit"].as_u64().unwrap_or(2000).clamp(1, 20_000) as usize;
            let toks: Vec<Value> = all[off..]
                .iter()
                .take(lim)
                .enumerate()
                .map(|(i, t)| t.to_json(off + i))
                .collect();
            json!({"id": tid, "total": all.len(), "offset": off, "tokens": toks})
        }
        "trace.rerun" => rerun(hub, a).await?,
        "trace.diff" => {
            let (x, y) = (text(a, "a")?, text(a, "b")?);
            let store = tr.store().map_err(Fail::unavailable)?;
            let (ta, tb) = (full(hub, x, 0)?, full(hub, y, 0)?);
            diff::diff(&ta, &store.tokens(x), &tb, &store.tokens(y))
        }
        "trace.settings" => {
            let mut cur = json!(tr.settings());
            let mut changed = false;
            if let Some(o) = a.as_object() {
                for (k, v) in o {
                    if cur.get(k).is_none() {
                        return Err(Fail::invalid(format!("no tracing setting {k}")));
                    }
                    cur[k] = v.clone();
                    changed = true;
                }
            }
            if changed {
                let new: TraceSettings = serde_json::from_value(cur).map_err(|e| Fail::invalid(e.to_string()))?;
                json!(tr.set_settings(new).map_err(io)?)
            } else {
                cur["stored_bytes"] = json!(tr.store().map(|s| s.total_bytes()).unwrap_or(0));
                cur
            }
        }
        "trace.export" => {
            let filter = Filter::from_json(a);
            let dir = hub.engine.home.join("trace").join("exports");
            let path = dir.join(format!("traces-{}.jsonl", super::store::now_ms()));
            let (tr2, p2) = (tr.clone(), path.clone());
            let n = tokio::task::spawn_blocking(move || -> Result<usize, String> {
                std::fs::create_dir_all(p2.parent().unwrap()).map_err(|e| e.to_string())?;
                let mut f = std::io::BufWriter::new(std::fs::File::create(&p2).map_err(|e| e.to_string())?);
                let n = tr2.store()?.export_jsonl(&filter, &mut f)?;
                std::io::Write::flush(&mut f).map_err(|e| e.to_string())?;
                Ok(n)
            })
            .await
            .map_err(|e| io(e.to_string()))?
            .map_err(io)?;
            json!({"path": path, "count": n})
        }
        "trace.delete" => {
            let tid = text(a, "id")?;
            let gone = tr.store().map_err(Fail::unavailable)?.delete(tid).map_err(io)?;
            if !gone {
                return Err(Fail::not_found(format!("no trace {tid}")));
            }
            json!({"deleted": tid})
        }
        _ => return Err(Fail::not_found(format!("no operation {id}"))),
    })
}

/// Which adapters (by index) a `remove_adapters` value names.
pub(super) fn named_adapters(sel: &Value, list: &[Value]) -> Result<Vec<usize>, Fail> {
    let all = || (0..list.len()).collect::<Vec<_>>();
    match sel {
        Value::Bool(true) => Ok(all()),
        Value::Bool(false) | Value::Null => Ok(vec![]),
        Value::Array(items) => {
            let mut out = vec![];
            for it in items {
                let found = match it {
                    Value::Number(n) => n.as_u64().map(|i| i as usize).filter(|i| *i < list.len()),
                    Value::String(s) => list
                        .iter()
                        .position(|a| a["name"].as_str().is_some_and(|n| n.contains(s.as_str()))),
                    _ => None,
                };
                out.push(found.ok_or_else(|| Fail::invalid(format!("no adapter {it} on that trace")))?);
            }
            Ok(out)
        }
        _ => Err(Fail::invalid(
            "remove_adapters is true or a list of adapter indexes or names",
        )),
    }
}

async fn rerun(hub: &Hub, a: &Value) -> Result<Value, Fail> {
    let tid = text(a, "id")?;
    let orig = full(hub, tid, 0)?;
    let mut body = orig["request"].clone();
    if !body.is_object() {
        return Err(Fail::invalid("that trace has no request to run again"));
    }
    if let Some(o) = a.get("overrides").filter(|o| !o.is_null()) {
        let o = o
            .as_object()
            .ok_or_else(|| Fail::invalid("overrides must be an object"))?;
        for (k, v) in o {
            if v.is_null() {
                body.as_object_mut().unwrap().remove(k);
            } else {
                body[k] = v.clone();
            }
        }
    }
    let list = orig["adapters"].as_array().cloned().unwrap_or_default();
    let mut lora: Vec<Value> = body["lora"].as_array().cloned().unwrap_or_default();
    let mut set = |i: usize, scale: f64| {
        lora.retain(|l| l["id"].as_u64() != Some(i as u64));
        lora.push(json!({"id": i, "scale": scale}));
    };
    let mut ablated = vec![];
    if let Some(sel) = a.get("remove_adapters").filter(|s| !s.is_null()) {
        let names = named_adapters(sel, &list)?;
        if names.is_empty() && sel == &json!(true) {
            return Err(Fail::invalid("that trace had no adapters attached"));
        }
        for i in names {
            set(i, 0.0);
            ablated.push(i);
        }
    }
    if let Some(sc) = a.get("adapter_scales").and_then(Value::as_object) {
        for (k, v) in sc {
            let i: usize = k
                .parse()
                .map_err(|_| Fail::invalid("adapter_scales keys are adapter indexes"))?;
            set(
                i,
                v.as_f64()
                    .ok_or_else(|| Fail::invalid("adapter_scales values are numbers"))?,
            );
        }
    }
    if !lora.is_empty() {
        body["lora"] = json!(lora);
    }
    let meta = json!({"source": "rerun", "rerun_of": tid, "overrides": a.get("overrides").cloned().unwrap_or(Value::Null),
                      "adapters_removed": ablated});
    let (new_id, resp) = capture::run_traced(
        hub,
        body,
        Some(tid),
        meta,
        &super::Scope::new(orig["job_id"].as_str(), orig["task_id"].as_str()),
    )
    .await?;
    let tr = tracer(&hub.engine.home);
    let store = tr.store().map_err(Fail::unavailable)?;
    let now = full(hub, &new_id, 0)?;
    let d = diff::diff(&orig, &store.tokens(tid), &now, &store.tokens(&new_id));
    Ok(
        json!({"trace": new_id, "rerun_of": tid, "answer": now["answer"], "reasoning": now["reasoning"],
              "response": resp, "diff": d}),
    )
}

// ---- routes ---------------------------------------------------------------------------------------------------

/// Query strings are all text: the paging and time fields are numbers.
fn args_of(q: HashMap<String, String>) -> Value {
    let mut o = serde_json::Map::new();
    for (k, v) in q {
        let numeric = matches!(k.as_str(), "limit" | "offset" | "since_ms" | "until_ms" | "max_events");
        let val = if numeric {
            v.parse::<i64>().map(Value::from).unwrap_or(Value::String(v))
        } else {
            Value::String(v)
        };
        o.insert(k, val);
    }
    Value::Object(o)
}

fn with_id(id: String, mut a: Value) -> Value {
    a["id"] = json!(id);
    a
}

async fn rest(hub: Hub, op: &str, a: Value) -> Result<Json<Value>, ApiError> {
    Ok(Json(call(&hub, op, &a).await?))
}

async fn list(State(hub): State<Hub>, Query(q): Query<HashMap<String, String>>) -> Result<Json<Value>, ApiError> {
    rest(hub, "trace.list", args_of(q)).await
}

async fn get_one(
    State(hub): State<Hub>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    rest(hub, "trace.get", with_id(id, args_of(q))).await
}

async fn delete_one(State(hub): State<Hub>, Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    rest(hub, "trace.delete", json!({"id": id})).await
}

async fn tokens(
    State(hub): State<Hub>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    rest(hub, "trace.tokens", with_id(id, args_of(q))).await
}

async fn rerun_route(
    State(hub): State<Hub>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, ApiError> {
    let a = body.map(|Json(v)| v).unwrap_or(json!({}));
    if !a.is_object() {
        return Err(Fail::invalid("the body must be a JSON object").into());
    }
    rest(hub, "trace.rerun", with_id(id, a)).await
}

async fn diff_route(State(hub): State<Hub>, Query(q): Query<HashMap<String, String>>) -> Result<Json<Value>, ApiError> {
    rest(hub, "trace.diff", args_of(q)).await
}

async fn settings_get(State(hub): State<Hub>) -> Result<Json<Value>, ApiError> {
    rest(hub, "trace.settings", json!({})).await
}

async fn settings_set(State(hub): State<Hub>, Json(a): Json<Value>) -> Result<Json<Value>, ApiError> {
    rest(hub, "trace.settings", a).await
}

async fn export(State(hub): State<Hub>, Query(q): Query<HashMap<String, String>>) -> Result<Response, ApiError> {
    let tr = tracer(&hub.engine.home);
    let filter = Filter::from_json(&args_of(q));
    let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let mut out = vec![];
        tr.store()?.export_jsonl(&filter, &mut out)?;
        Ok(out)
    })
    .await
    .map_err(|e| io(e.to_string()))?
    .map_err(io)?;
    Ok(([("content-type", "application/x-ndjson")], Body::from(bytes)).into_response())
}

async fn stream(State(hub): State<Hub>, Query(q): Query<HashMap<String, String>>) -> Result<Response, ApiError> {
    let tr = tracer(&hub.engine.home);
    // subscribe before looking at what is running, so nothing falls between the two
    let mut rx = tr.subscribe();
    let only = q.get("trace").cloned();
    let running = tr
        .store()
        .map_err(Fail::unavailable)?
        .list(&Filter {
            status: Some("running".into()),
            limit: 50,
            ..Default::default()
        })
        .map(|(_, items)| items)
        .unwrap_or_default();
    let events = async_stream::stream! {
        yield Ok::<_, Infallible>(Event::default().event("hello").data(json!({"type": "hello", "running": running}).to_string()));
        loop {
            match rx.recv().await {
                Ok(v) => {
                    if only.as_deref().is_some_and(|t| v["trace"].as_str() != Some(t)) {
                        continue;
                    }
                    let kind = v["type"].as_str().unwrap_or("event").to_string();
                    yield Ok(Event::default().event(kind).data(v.to_string()));
                }
                Err(RecvError::Lagged(n)) => {
                    yield Ok(Event::default().event("lagged").data(json!({"type": "lagged", "missed": n}).to_string()));
                }
                Err(RecvError::Closed) => break,
            }
        }
    };
    Ok(Sse::new(events).keep_alive(KeepAlive::default()).into_response())
}

/// The routes (owner only: the hub puts them behind its token check and out of the peer allowlist).
pub fn routes() -> Router<Hub> {
    Router::new()
        .route("/v1/native/traces", get(list))
        .route("/v1/native/traces/:id", get(get_one).delete(delete_one))
        .route("/v1/native/traces/:id/tokens", get(tokens))
        .route("/v1/native/traces/:id/rerun", axum::routing::post(rerun_route))
        .route("/v1/native/diff", get(diff_route))
        .route("/v1/native/stream", get(stream))
        .route("/v1/native/export", get(export))
        .route("/v1/native/settings", get(settings_get).post(settings_set))
}

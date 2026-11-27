//! The error codes of llama-server's failed answers on the routes that do not stream: the `embeddings.create` and
//! `rerank.run` operations, and the Ollama shim's chat, generate and embed routes. Each one reaches a stand-in that
//! answers with the error, so the status, code, Retry-After and fields a client sees are the ones the hub promises.

use std::sync::atomic::Ordering;

use axum::http::{HeaderMap, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::hub::{self, Hub};
use crate::testing::{self, failing_stand_in, full_stand_in, stand_in, StandIn, TOKEN};

const FULL: &str = "request (9000 tokens) exceeds the available context size (4096 tokens), try increasing it";

fn overflow() -> Value {
    json!({"error": {"code": 400, "type": "exceed_context_size_error", "message": FULL},
           "n_prompt_tokens": 9000, "n_ctx": 4096})
}

fn busy() -> Value {
    json!({"error": {"code": 503, "type": "unavailable_error", "message": "no slot available"}})
}

fn loading() -> Value {
    json!({"error": {"code": 503, "type": "unavailable_error", "message": "Loading model"}})
}

fn unmapped() -> Value {
    json!({"error": {"code": 400, "type": "invalid_request_error", "message": "bad input"}})
}

/// A hub that knows `tiny`, served by a llama-server stand-in that answers every request with `status` and `body`.
/// `embeddings` loads it for embedding (the embeddings routes refuse a chat model, and the chat routes an embedding one).
async fn answering(home: &std::path::Path, status: u16, body: Value, embeddings: bool) -> (Hub, StandIn) {
    let h = testing::hub(home, &["tiny"]);
    let s = failing_stand_in(status, body).await;
    h.engine.adopt_as("tiny", s.port, "k", embeddings, Some(s.on_stop()));
    (h, s)
}

async fn call(h: &Hub, method: &str, path: &str, body: Value) -> (StatusCode, HeaderMap, Value) {
    let r = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    let resp = hub::router(h.clone()).oneshot(r).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22).await.unwrap();
    (status, headers, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn the_embeddings_op_names_a_full_context_and_keeps_its_counts() {
    let home = testing::temp_home();
    let (h, _s) = answering(&home, 400, overflow(), true).await;
    let (st, _, v) = call(
        &h,
        "POST",
        "/v1/call/embeddings.create",
        json!({"model": "tiny", "input": "hi"}),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["error"]["code"], "context_exceeded");
    assert_eq!(v["error"]["n_prompt_tokens"], 9000);
    assert_eq!(v["error"]["n_ctx"], 4096);
}

#[tokio::test]
async fn the_op_says_a_loading_model_is_loading_and_when_to_retry() {
    // rerank.run takes the same path (forward_json); an embedding model is used here because the hub's stand-ins only
    // load for embedding or chat
    let home = testing::temp_home();
    let (h, _s) = answering(&home, 503, loading(), true).await;
    let (st, headers, v) = call(
        &h,
        "POST",
        "/v1/call/embeddings.create",
        json!({"model": "tiny", "input": "hi"}),
    )
    .await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(v["error"]["code"], "loading");
    assert_eq!(v["error"]["retry_after_s"], 2);
    assert_eq!(headers["retry-after"].to_str().unwrap(), "2");
}

#[tokio::test]
async fn an_unmapped_error_on_the_op_stays_a_502_backend_error() {
    let home = testing::temp_home();
    let (h, _s) = answering(&home, 400, unmapped(), true).await;
    let (st, headers, v) = call(
        &h,
        "POST",
        "/v1/call/embeddings.create",
        json!({"model": "tiny", "input": "hi"}),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "{v}");
    assert_eq!(v["error"]["code"], "backend_error");
    assert!(v["error"]["message"].as_str().unwrap().contains("bad input"));
    assert!(!headers.contains_key("retry-after"));
}

#[tokio::test]
async fn the_shim_names_a_full_context_on_chat() {
    let home = testing::temp_home();
    let (h, _s) = answering(&home, 400, overflow(), false).await;
    let chat = json!({"model": "tiny", "messages": [{"role": "user", "content": "hi"}], "stream": false});
    let (st, _, v) = call(&h, "POST", "/api/chat", chat).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["code"], "context_exceeded");
    assert_eq!(v["n_prompt_tokens"], 9000);
    assert!(v["error"]
        .as_str()
        .unwrap()
        .contains("exceeds the available context size"));
}

#[tokio::test]
async fn the_shim_names_a_full_context_on_embed() {
    let home = testing::temp_home();
    let (h, _s) = answering(&home, 400, overflow(), true).await;
    let (st, _, v) = call(&h, "POST", "/api/embed", json!({"model": "tiny", "input": "hi"})).await;
    assert_eq!(
        (st, v["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("context_exceeded")),
        "{v}"
    );
}

#[tokio::test]
async fn the_shim_says_busy_with_a_retry_after() {
    let home = testing::temp_home();
    let (h, _s) = answering(&home, 503, busy(), true).await;
    let (st, headers, v) = call(&h, "POST", "/api/embed", json!({"model": "tiny", "input": "hi"})).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(
        (v["code"].as_str(), v["retry_after_s"].as_u64()),
        (Some("busy"), Some(1))
    );
    assert_eq!(headers["retry-after"].to_str().unwrap(), "1");
}

#[tokio::test]
async fn the_shim_keeps_an_unmapped_upstream_status_with_backend_error() {
    let home = testing::temp_home();
    let (h, _s) = answering(&home, 400, unmapped(), false).await;
    let (st, _, v) = call(
        &h,
        "POST",
        "/api/generate",
        json!({"model": "tiny", "prompt": "hi", "stream": false}),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(
        (v["code"].as_str(), v["error"].as_str()),
        (Some("backend_error"), Some("bad input"))
    );
}

#[tokio::test]
async fn the_shim_errors_carry_the_hubs_code() {
    let home = testing::temp_home();
    let h = testing::hub(&home, &["tiny"]);
    let (st, _, v) = call(&h, "POST", "/api/show", json!({"model": "no-such-model"})).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{v}");
    assert_eq!(
        (v["code"].as_str(), v["error"].as_str().is_some()),
        (Some("not_found"), true)
    );
}

#[test]
fn an_answer_that_is_not_an_error_object_has_no_stable_code() {
    assert!(hub::upstream_failure(400, "not json").is_none());
    assert_eq!(
        hub::upstream_message(502, "  "),
        "llama-server answered 502 with no message"
    );
    assert_eq!(hub::upstream_message(500, "plain text"), "plain text");
}

/// A hub that knows `tiny`, served by `s`. With `fail_fast`, the setting is saved with `config.set`, as the owner would.
async fn serving(home: &std::path::Path, s: &StandIn, fail_fast: bool) -> Hub {
    let h = testing::hub(home, &["tiny"]);
    h.engine.adopt("tiny", s.port, "k", Some(s.on_stop()));
    if fail_fast {
        let set = json!({"settings": {"fail_fast_busy": true}});
        let (st, _, v) = call(&h, "POST", "/v1/call/config.set", set).await;
        assert_eq!(st, StatusCode::OK, "{v}");
    }
    h
}

fn chat() -> Value {
    json!({"model": "tiny", "messages": [{"role": "user", "content": "hi"}], "stream": false})
}

#[tokio::test]
async fn fail_fast_busy_answers_busy_at_once_when_every_slot_is_taken() {
    let home = testing::temp_home();
    let s = full_stand_in("pong").await;
    let h = serving(&home, &s, true).await;
    let (st, headers, v) = call(&h, "POST", "/v1/chat/completions", chat()).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(v["error"]["code"], "busy");
    assert_eq!(headers["retry-after"].to_str().unwrap(), "1");
    assert_eq!(
        s.seen.load(Ordering::SeqCst),
        0,
        "the request must not reach llama-server"
    );
}

#[tokio::test]
async fn fail_fast_busy_is_also_answered_by_the_ollama_shim() {
    let home = testing::temp_home();
    let s = full_stand_in("pong").await;
    let h = serving(&home, &s, true).await;
    let (st, headers, v) = call(&h, "POST", "/api/chat", chat()).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(
        (v["code"].as_str(), headers["retry-after"].to_str().unwrap()),
        (Some("busy"), "1")
    );
    assert_eq!(s.seen.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn without_the_setting_a_full_server_still_queues_the_request() {
    let home = testing::temp_home();
    let s = full_stand_in("pong").await;
    let h = serving(&home, &s, false).await;
    let (st, _, v) = call(&h, "POST", "/v1/chat/completions", chat()).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(s.seen.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn fail_fast_busy_sends_the_request_when_a_slot_is_free() {
    let home = testing::temp_home();
    let s = stand_in("pong", 0).await;
    let h = serving(&home, &s, true).await;
    let (st, _, v) = call(&h, "POST", "/v1/chat/completions", chat()).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(s.seen.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn fail_fast_busy_is_off_by_default_and_config_set_turns_it_on() {
    let home = testing::temp_home();
    let h = testing::hub(&home, &["tiny"]);
    let (_, _, before) = call(&h, "POST", "/v1/call/config.get", json!({})).await;
    assert_eq!(before["result"]["fail_fast_busy"], false, "{before}");
    let (st, _, v) = call(
        &h,
        "POST",
        "/v1/call/config.set",
        json!({"settings": {"fail_fast_busy": true}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (_, _, after) = call(&h, "POST", "/v1/call/config.get", json!({})).await;
    assert_eq!(after["result"]["fail_fast_busy"], true, "{after}");
}

// ---- the fabric (src/fabric.rs) ----------------------------------------------------------------------------

async fn call_with(h: &Hub, path: &str, body: Value, extra: &[(&str, &str)]) -> (StatusCode, HeaderMap, Value) {
    let mut r = Request::builder()
        .method("POST")
        .uri(path)
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json");
    for (k, v) in extra {
        r = r.header(*k, *v);
    }
    let resp = hub::router(h.clone()).oneshot(r.body(axum::body::Body::from(body.to_string())).unwrap()).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22).await.unwrap();
    (status, headers, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// A hub whose only server has no free slot, joined to a fabric whose endpoint is a stand-in tagged "fabric".
async fn joined(home: &std::path::Path, offload: &str) -> (Hub, StandIn, StandIn) {
    let busy = full_stand_in("local").await;
    let h = serving(home, &busy, true).await;
    let fabric = stand_in("fabric", 0).await;
    let (st, _, v) = call(
        &h,
        "POST",
        "/v1/call/fabric.set",
        json!({"endpoint": format!("http://127.0.0.1:{}", fabric.port), "offload": offload}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    (h, busy, fabric)
}

#[tokio::test]
async fn a_busy_hub_passes_the_request_to_the_fabric() {
    let home = testing::temp_home();
    let (h, busy, fabric) = joined(&home, "when_full").await;
    let (st, headers, v) = call(&h, "POST", "/v1/chat/completions", chat()).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(headers["x-mm-offloaded"].to_str().unwrap(), "1");
    assert_eq!(v["choices"][0]["message"]["content"], "fabric", "the answer is the fabric's: {v}");
    assert_eq!(busy.seen.load(Ordering::SeqCst), 0, "this hub's own server never saw it");
    assert_eq!(fabric.seen.load(Ordering::SeqCst), 1);
    // the hub counts what it passed on, by the node that served it (the stand-in does not name one)
    let (_, _, v) = call(&h, "POST", "/v1/call/fabric.status", json!({})).await;
    assert_eq!(v["result"]["offloaded_by_node"]["?"], 1, "{v}");
}

#[tokio::test]
async fn an_offloaded_request_is_charged_to_the_callers_key_with_the_tokens_the_fabric_answered() {
    let home = testing::temp_home();
    let (h, _busy, _fabric) = joined(&home, "when_full").await;
    let (st, _, v) = call(&h, "POST", "/v1/call/key.create", json!({"peer": "alice"})).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let key = v["result"]["key"].as_str().unwrap().to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", format!("Bearer {key}"))
        .header("content-type", "application/json")
        .body(axum::body::Body::from(chat().to_string()))
        .unwrap();
    let resp = hub::router(h.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["x-mm-offloaded"].to_str().unwrap(), "1");
    // the key is charged when the answer has been read to its end
    let _ = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let (_, _, u) = call(&h, "POST", "/v1/call/usage.get", json!({"key": "alice"})).await;
    let row = &u["result"][0];
    assert_eq!(
        (row["requests"].as_u64(), row["prompt_tokens"].as_u64(), row["completion_tokens"].as_u64()),
        (Some(1), Some(3), Some(5)),
        "{u}"
    );
}

#[tokio::test]
async fn a_request_that_came_through_the_fabric_is_not_sent_back_to_it() {
    let home = testing::temp_home();
    let (h, _busy, fabric) = joined(&home, "when_full").await;
    let (st, _, v) = call_with(&h, "/v1/chat/completions", chat(), &[("x-cf-hop", "1")]).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(v["error"]["code"], "busy", "the hub's own answer");
    assert_eq!(fabric.seen.load(Ordering::SeqCst), 0, "nothing was passed on");
}

#[tokio::test]
async fn offload_off_and_a_hub_that_has_not_joined_keep_the_busy_answer() {
    let home = testing::temp_home();
    let (h, _busy, fabric) = joined(&home, "off").await;
    let (st, _, v) = call(&h, "POST", "/v1/chat/completions", chat()).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(fabric.seen.load(Ordering::SeqCst), 0);

    let home2 = testing::temp_home();
    let busy = full_stand_in("local").await;
    let h2 = serving(&home2, &busy, true).await;
    let (st, _, v) = call(&h2, "POST", "/v1/chat/completions", chat()).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    let (_, _, v) = call(&h2, "POST", "/v1/call/fabric.status", json!({})).await;
    assert_eq!(v["result"]["joined"], false, "{v}");
}

#[tokio::test]
async fn an_unreachable_fabric_leaves_the_hubs_own_error() {
    let home = testing::temp_home();
    let busy = full_stand_in("local").await;
    let h = serving(&home, &busy, true).await;
    // nothing listens on port 9
    let (st, _, v) = call(&h, "POST", "/v1/call/fabric.set", json!({"endpoint": "http://127.0.0.1:9"})).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, _, v) = call(&h, "POST", "/v1/chat/completions", chat()).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(v["error"]["code"], "busy");
}

#[tokio::test]
async fn fabric_set_checks_its_input_and_leave_removes_the_settings() {
    let home = testing::temp_home();
    let h = testing::hub(&home, &["tiny"]);
    let (st, _, v) = call(&h, "POST", "/v1/call/fabric.set", json!({"endpoint": "ftp://x"})).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, _, v) = call(&h, "POST", "/v1/call/fabric.set", json!({"offload": "sometimes", "api": "http://127.0.0.1:7878"})).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, _, v) = call(&h, "POST", "/v1/call/fabric.set", json!({})).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "an empty set joins nothing: {v}");
    let (st, _, v) = call(&h, "POST", "/v1/call/fabric.set", json!({"api": "http://127.0.0.1:9/", "endpoint": "http://127.0.0.1:9"})).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["result"]["api"], "http://127.0.0.1:9", "a trailing slash is dropped");
    assert_eq!(v["result"]["offload"], "when_full", "the default");
    let (_, _, v) = call(&h, "POST", "/v1/call/fabric.status", json!({})).await;
    assert_eq!(v["result"]["joined"], true, "{v}");
    assert_eq!(v["result"]["endpoint_reachable"], false, "nothing answers there");
    let (st, _, v) = call(&h, "POST", "/v1/call/fabric.nodes", json!({})).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "the fabric does not answer: {v}");
    let (_, _, v) = call(&h, "POST", "/v1/call/fabric.set", json!({"leave": true})).await;
    assert_eq!(v["result"]["joined"], false);
    let (st, _, _) = call(&h, "POST", "/v1/call/fabric.nodes", json!({})).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "no fabric to ask");
}

#[tokio::test]
async fn machine_status_reports_the_room_a_load_has() {
    let home = testing::temp_home();
    let h = testing::hub(&home, &["tiny"]);
    let (st, _, v) = call(&h, "POST", "/v1/call/machine.status", json!({})).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let r = &v["result"];
    assert!(r["ram_total_mb"].as_f64().unwrap() > 0.0, "{v}");
    assert!(r["ram_available_mb"].as_f64().unwrap() > 0.0);
    assert!(r["vram_usable_mib"].is_u64(), "{v}");
    assert_eq!(r["loaded"], 0);
    assert!(r["gpus"].is_array());
    let (_, _, ops) = call(&h, "GET", "/v1/operations", json!({})).await;
    let ids: Vec<&str> = ops["operations"].as_array().map(|a| a.iter().filter_map(|o| o["id"].as_str()).collect()).unwrap_or_default();
    assert!(ids.contains(&"machine.status") && ids.contains(&"fabric.set"), "{ids:?}");
}

//! The error codes of llama-server's failed answers on the routes that do not stream: the `embeddings.create` and
//! `rerank.run` operations, and the Ollama shim's chat, generate and embed routes. Each one reaches a stand-in that
//! answers with the error, so the status, code, Retry-After and fields a client sees are the ones the hub promises.

use axum::http::{HeaderMap, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::hub::{self, Hub};
use crate::testing::{self, failing_stand_in, StandIn, TOKEN};

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

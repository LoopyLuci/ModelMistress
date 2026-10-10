//! Hosting for peers, end to end over HTTP: a hub in this process on a free port, stand-ins for llama-server behind
//! its models (no real model is needed), and real requests with real keys.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::hub::Hub;
use crate::testing::{self, stand_in, StandIn, TOKEN};

struct Rig {
    hub: Hub,
    url: String,
    http: reqwest::Client,
    home: std::path::PathBuf,
    _server: tokio::task::JoinHandle<()>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.hub.engine.unload_all();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

async fn rig(models: &[&str]) -> Rig {
    let home = testing::temp_home();
    let hub = testing::hub(&home, models);
    let (addr, server) = testing::serve(hub.clone()).await;
    Rig {
        hub,
        url: format!("http://{addr}"),
        http: reqwest::Client::new(),
        home,
        _server: server,
    }
}

impl Rig {
    /// A model that answers with `tag`, after `delay_ms`.
    async fn model(&self, id: &str, tag: &str, delay_ms: u64) -> (Arc<crate::engine::Instance>, StandIn) {
        let s = stand_in(tag, delay_ms).await;
        let inst = self.hub.engine.adopt(id, s.port, "k", Some(s.on_stop()));
        (inst, s)
    }

    /// Make a key through the owner's operation; returns its secret.
    async fn key(&self, args: Value) -> String {
        let r = self
            .http
            .post(format!("{}/v1/call/key.create", self.url))
            .bearer_auth(TOKEN)
            .json(&args)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let v: Value = r.json().await.unwrap();
        v["result"]["key"].as_str().unwrap().to_string()
    }

    fn chat(&self, key: &str, model: &str) -> reqwest::RequestBuilder {
        self.http
            .post(format!("{}/v1/chat/completions", self.url))
            .bearer_auth(key)
            .json(&json!({"model": model, "messages": [{"role": "user", "content": "hi"}]}))
    }

    async fn call(&self, op: &str, args: Value) -> (u16, Value) {
        let r = self
            .http
            .post(format!("{}/v1/call/{op}", self.url))
            .bearer_auth(TOKEN)
            .json(&args)
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }
}

async fn text_of(r: reqwest::Response) -> String {
    let v: Value = r.json().await.unwrap();
    v["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string()
}

#[tokio::test]
async fn a_peer_key_reaches_the_models_and_nothing_else() {
    let r = rig(&["alpha"]).await;
    let (_i, _s) = r.model("alpha", "A", 0).await;
    let key = r.key(json!({"peer": "alice"})).await;
    assert!(key.starts_with("mmk_"));

    assert_eq!(text_of(r.chat(&key, "alpha").send().await.unwrap()).await, "A");
    let models = r
        .http
        .get(format!("{}/v1/models", r.url))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap();
    assert_eq!(models.status(), 200);
    // not the control API, not stopping the hub
    for path in [
        "/v1/operations",
        "/v1/call/service.status",
        "/v1/call/key.list",
        "/v1/service/stop",
    ] {
        let resp = if path == "/v1/operations" {
            r.http.get(format!("{}{path}", r.url))
        } else {
            r.http.post(format!("{}{path}", r.url)).json(&json!({}))
        }
        .bearer_auth(&key)
        .send()
        .await
        .unwrap();
        assert_eq!(resp.status(), 403, "{path}");
    }
    // a made-up key, and no key at all
    assert_eq!(r.chat("mmk_made_up", "alpha").send().await.unwrap().status(), 401);
    let none = r
        .http
        .post(format!("{}/v1/chat/completions", r.url))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(none.status(), 401);
    // the owner is not limited by any of it
    assert_eq!(r.chat(TOKEN, "alpha").send().await.unwrap().status(), 200);
}

#[tokio::test]
async fn requests_per_minute_are_refused_with_429_and_retry_after() {
    let r = rig(&["alpha"]).await;
    let (_i, s) = r.model("alpha", "A", 0).await;
    let key = r.key(json!({"peer": "alice", "requests_per_min": 2})).await;
    for _ in 0..2 {
        assert_eq!(r.chat(&key, "alpha").send().await.unwrap().status(), 200);
    }
    let third = r.chat(&key, "alpha").send().await.unwrap();
    assert_eq!(third.status(), 429);
    let wait: u64 = third.headers()["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((1..=60).contains(&wait));
    let v: Value = third.json().await.unwrap();
    assert_eq!(v["error"]["code"], "rate_limit_exceeded");
    // the refused request never reached the model
    assert_eq!(s.seen.load(std::sync::atomic::Ordering::SeqCst), 2);
    // another peer is not affected
    let other = r.key(json!({"peer": "bob", "requests_per_min": 2})).await;
    assert_eq!(r.chat(&other, "alpha").send().await.unwrap().status(), 200);
    let (_, u) = r.call("usage.get", json!({"key": "alice"})).await;
    assert_eq!(
        (u["result"][0]["requests"].as_u64(), u["result"][0]["rejected"].as_u64()),
        (Some(2), Some(1))
    );
}

#[tokio::test]
async fn tokens_per_day_are_counted_from_the_answers() {
    let r = rig(&["alpha"]).await;
    let (_i, _s) = r.model("alpha", "A", 0).await;
    // each answer is 8 tokens (3 + 5)
    let key = r.key(json!({"peer": "alice", "tokens_per_day": 10})).await;
    assert_eq!(r.chat(&key, "alpha").send().await.unwrap().status(), 200); // 8
    assert_eq!(r.chat(&key, "alpha").send().await.unwrap().status(), 200); // 16: served, it started under the limit
    let third = r.chat(&key, "alpha").send().await.unwrap();
    assert_eq!(third.status(), 429);
    let wait: u64 = third.headers()["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((1..=86_400).contains(&wait));
    let (_, u) = r.call("usage.get", json!({})).await;
    let u = &u["result"][0];
    assert_eq!(
        (
            u["prompt_tokens"].as_u64(),
            u["completion_tokens"].as_u64(),
            u["tokens_today"].as_u64()
        ),
        (Some(6), Some(10), Some(16))
    );
    assert_eq!(u["by_model"]["alpha"]["requests"], 2);
}

#[tokio::test]
async fn concurrent_requests_are_limited_and_a_stream_counts_until_its_end() {
    let r = rig(&["alpha"]).await;
    let (_i, _s) = r.model("alpha", "A", 600).await;
    let key = r.key(json!({"peer": "alice", "concurrent": 1})).await;
    let slow = tokio::spawn(r.chat(&key, "alpha").send());
    tokio::time::sleep(Duration::from_millis(200)).await;
    let refused = r.chat(&key, "alpha").send().await.unwrap();
    assert_eq!(refused.status(), 429);
    assert!(refused.headers().contains_key("retry-after"));
    assert_eq!(slow.await.unwrap().unwrap().status(), 200);
    // free again
    assert_eq!(r.chat(&key, "alpha").send().await.unwrap().status(), 200);

    // a streamed answer holds its place until the last byte
    let streaming = r
        .http
        .post(format!("{}/v1/chat/completions", r.url))
        .bearer_auth(&key)
        .json(&json!({"model": "alpha", "stream": true, "messages": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(streaming.status(), 200);
    assert_eq!(r.chat(&key, "alpha").send().await.unwrap().status(), 429);
    let body = streaming.text().await.unwrap();
    assert!(body.contains("[DONE]"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(r.chat(&key, "alpha").send().await.unwrap().status(), 200);
    // the stream's final event carried the usage: 3 + 5 tokens, counted for the key
    let (_, u) = r.call("usage.get", json!({})).await;
    assert_eq!(u["result"][0]["in_flight"], 0);
    let total =
        u["result"][0]["prompt_tokens"].as_u64().unwrap() + u["result"][0]["completion_tokens"].as_u64().unwrap();
    assert_eq!(total, 8 * 4); // three plain answers and the stream
}

#[tokio::test]
async fn a_revoked_key_stops_at_once() {
    let r = rig(&["alpha"]).await;
    let (_i, _s) = r.model("alpha", "A", 0).await;
    let key = r.key(json!({"peer": "alice"})).await;
    assert_eq!(r.chat(&key, "alpha").send().await.unwrap().status(), 200);
    let (st, v) = r.call("key.list", json!({})).await;
    assert_eq!(st, 200);
    let id = v["result"][0]["id"].as_str().unwrap().to_string();
    assert!(!v.to_string().contains(&key), "the list never shows the secret");
    let (st, v) = r.call("key.revoke", json!({"key": id})).await;
    assert_eq!((st, v["result"]["revoked"].as_u64()), (200, Some(1)));
    let after = r.chat(&key, "alpha").send().await.unwrap();
    assert_eq!(after.status(), 401);
    assert_eq!(after.json::<Value>().await.unwrap()["error"]["code"], "key_revoked");
    // its usage stays readable, and an unknown key is a 404
    let (_, u) = r.call("usage.get", json!({"key": "alice"})).await;
    assert_eq!(u["result"][0]["requests"], 1);
    assert!(!u["result"][0]["revoked"].is_null());
    assert_eq!(r.call("key.revoke", json!({"key": "nobody"})).await.0, 404);
}

#[tokio::test]
async fn the_allow_list_limits_the_models_a_key_can_use_and_see() {
    let r = rig(&["alpha", "beta", "gamma"]).await;
    let (_a, _sa) = r.model("alpha", "A", 0).await;
    let (_b, _sb) = r.model("beta", "B", 0).await;
    let key = r.key(json!({"peer": "alice", "models": ["alpha", "gam*"]})).await;
    assert_eq!(text_of(r.chat(&key, "alpha").send().await.unwrap()).await, "A");
    let denied = r.chat(&key, "beta").send().await.unwrap();
    assert_eq!(denied.status(), 403);
    assert_eq!(
        denied.json::<Value>().await.unwrap()["error"]["code"],
        "model_not_allowed"
    );
    let ids = |v: Value| -> Vec<String> {
        let mut ids: Vec<String> = v["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    };
    let seen = r
        .http
        .get(format!("{}/v1/models", r.url))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap();
    assert_eq!(ids(seen.json().await.unwrap()), ["alpha", "gamma"]);
    let all = r
        .http
        .get(format!("{}/v1/models", r.url))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(ids(all.json().await.unwrap()), ["alpha", "beta", "gamma"]);
    // the owner may use any
    assert_eq!(text_of(r.chat(TOKEN, "beta").send().await.unwrap()).await, "B");
}

#[tokio::test]
async fn bad_key_requests_are_refused() {
    let r = rig(&[]).await;
    assert_eq!(r.call("key.create", json!({})).await.0, 400);
    assert_eq!(r.call("key.create", json!({"peer": "x", "concurrent": 0})).await.0, 400);
    assert_eq!(
        r.call("key.create", json!({"peer": "x", "models": "alpha"})).await.0,
        400
    );
}

/// The big one: replacing the model while peers are connected drops nothing.
#[tokio::test]
async fn a_hot_swap_finishes_requests_on_the_old_model_and_sends_new_ones_to_the_new() {
    let r = rig(&["alpha"]).await;
    let (old, old_server) = r.model("alpha", "OLD", 800).await;
    let key = r.key(json!({"peer": "alice"})).await;

    // a handful of requests in flight, one of them a stream
    let mut inflight = vec![];
    for _ in 0..6 {
        inflight.push(tokio::spawn(r.chat(&key, "alpha").send()));
    }
    let stream = tokio::spawn(
        r.http
            .post(format!("{}/v1/chat/completions", r.url))
            .bearer_auth(&key)
            .json(&json!({"model": "alpha", "stream": true, "messages": []}))
            .send(),
    );
    let started = Instant::now();
    while old.inflight() < 7 {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "requests never arrived: {}",
            old.inflight()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // replace the model
    let (new, _new_server) = r.model("alpha", "NEW", 10).await;
    assert!(
        !old.is_stopped() && old_server.running(),
        "the old model must keep serving what it has"
    );

    // new requests go to the new model, at once, while the old requests are still running
    assert_eq!(text_of(r.chat(&key, "alpha").send().await.unwrap()).await, "NEW");
    assert!(old.inflight() > 0);

    // every old request finishes on the old model
    for t in inflight {
        let resp = t.await.unwrap().unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(text_of(resp).await, "OLD");
    }
    let body = stream.await.unwrap().unwrap().text().await.unwrap();
    assert!(body.contains("OLD") && body.contains("[DONE]"), "{body}");

    // then the old one is stopped, and the new one is not
    let started = Instant::now();
    while !old.is_stopped() || old_server.running() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the old model was never stopped"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!new.is_stopped());
    assert_eq!(text_of(r.chat(&key, "alpha").send().await.unwrap()).await, "NEW");
    // nothing was counted as an error
    let (_, u) = r.call("usage.get", json!({})).await;
    assert_eq!(u["result"][0]["errors"], 0);
    assert_eq!(u["result"][0]["requests"], 9); // six, the stream, and two after the swap
}

#[tokio::test]
async fn unloading_a_model_does_not_cut_the_request_it_is_serving() {
    let r = rig(&["alpha"]).await;
    let (inst, server) = r.model("alpha", "A", 400).await;
    let slow = tokio::spawn(r.chat(TOKEN, "alpha").send());
    while inst.inflight() < 1 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(r.hub.engine.unload("alpha"));
    assert!(!r.hub.engine.is_loaded("alpha"));
    assert!(!inst.is_stopped() && server.running());
    assert_eq!(text_of(slow.await.unwrap().unwrap()).await, "A");
    let started = Instant::now();
    while !inst.is_stopped() {
        assert!(started.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // shutdown does not wait for anyone
    let (busy, _s) = r.model("alpha", "B", 5_000).await;
    let t = tokio::spawn(r.chat(TOKEN, "alpha").send());
    while busy.inflight() < 1 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(r.hub.engine.unload_all(), 1);
    assert!(busy.is_stopped());
    t.abort();
}

/// The typed client (what the desktop app and the CLI use) against the same hub.
#[tokio::test]
async fn the_client_drives_models_chat_and_keys() {
    use crate::client::Client;
    let r = rig(&["alpha", "beta"]).await;
    let (_i, _s) = r.model("alpha", "A", 0).await;
    let c = Client::new(&r.url, TOKEN);
    assert_eq!(c.health().await.unwrap()["ok"], true);
    assert_eq!(c.status().await.unwrap()["models_known"], 2);
    assert_eq!(c.list_models(true).await.unwrap().len(), 2);
    assert_eq!(c.loaded_models().await.unwrap().len(), 1);
    let hi = vec![json!({"role": "user", "content": "hi"})];
    assert_eq!(c.chat("alpha", hi.clone()).await.unwrap()["text"], "A");
    let mut pieces = 0;
    assert_eq!(
        c.chat_stream("alpha", hi.clone(), |_| pieces += 1).await.unwrap(),
        "AAA"
    );
    assert_eq!(pieces, 3);
    assert!(c.chat_stream("nope", hi, |_| {}).await.is_err());
    assert!(c.delete_model("nope").await.is_err());
    let made = c.create_key("alice", json!({"concurrent": 2})).await.unwrap();
    assert!(made["key"].as_str().unwrap().starts_with("mmk_"));
    assert_eq!(c.keys().await.unwrap().len(), 1);
    assert_eq!(c.usage(Some("alice")).await.unwrap().len(), 1);
    assert_eq!(c.revoke_key("alice").await.unwrap()["revoked"], 1);
    assert!(Client::new(&r.url, "wrong-token-0123456789").status().await.is_err());
    assert_eq!(c.unload_model("alpha").await.unwrap()["unloaded"], 1);
}

#[test]
fn the_owner_token_is_for_this_machine_only() {
    use crate::hub::owner_allowed;
    let a = |s: &str| Some(s.parse::<std::net::SocketAddr>().unwrap());
    assert!(owner_allowed(None));
    assert!(owner_allowed(a("127.0.0.1:5000")) && owner_allowed(a("[::1]:5000")));
    assert!(owner_allowed(a("[::ffff:127.0.0.1]:5000")));
    assert!(!owner_allowed(a("192.168.1.20:5000")) && !owner_allowed(a("[2001:db8::1]:5000")));
}

/// Through the router with an address attached, as the listener does: the owner's token from another machine is
/// refused, a peer key from there works, and the owner's token from this machine works.
#[tokio::test]
async fn over_the_network_only_peer_keys_work() {
    use axum::extract::ConnectInfo;
    use tower::ServiceExt;
    let r = rig(&["alpha"]).await;
    let (_i, _s) = r.model("alpha", "A", 0).await;
    let key = r.key(json!({"peer": "alice"})).await;
    let ask = |token: String, from: &str| {
        let mut req = axum::http::Request::post("/v1/chat/completions")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(r#"{"model":"alpha","messages":[]}"#))
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(from.parse::<std::net::SocketAddr>().unwrap()));
        let app = crate::hub::router(r.hub.clone());
        async move { app.oneshot(req).await.unwrap().status().as_u16() }
    };
    assert_eq!(ask(TOKEN.into(), "127.0.0.1:4000").await, 200);
    assert_eq!(ask(TOKEN.into(), "10.1.2.3:4000").await, 403);
    assert_eq!(ask(key, "10.1.2.3:4000").await, 200);
}

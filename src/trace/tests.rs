//! Tests of the trace store, the Tracer API, the capture of the chat path against a stand-in llama-server that
//! returns log probabilities, the API, the live stream, rerun and diff, and the dashboard's route.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::{json, Value};

use super::store::{decode, encode, header, Filter, Store, NO_ID};
use super::*;
use crate::hub::Hub;
use crate::testing::{self, TOKEN};

// ---- the store --------------------------------------------------------------------------------------------------

fn tok(text: &str, lp: f32, phase: u8, alts: &[(&str, f32)]) -> TokenRec {
    TokenRec {
        id: NO_ID,
        text: text.into(),
        logprob: lp,
        entropy: store::entropy(alts.iter().map(|a| a.1)),
        phase,
        alts: alts
            .iter()
            .map(|(t, l)| Alt {
                id: 7,
                text: (*t).into(),
                logprob: *l,
            })
            .collect(),
        routing: vec![],
    }
}

#[test]
fn segment_records_round_trip_and_a_torn_one_is_ignored() {
    let a = tok("héllo", -0.25, 0, &[("héllo", -0.25), ("hi", -1.5)]);
    let mut b = tok("<think>", -3.0, 1, &[]);
    b.routing = vec![1, 2, 3]; // the reserved routing column survives
    b.id = 42;
    let mut bytes = header();
    encode(&a, &mut bytes);
    encode(&b, &mut bytes);
    assert_eq!(decode(&bytes), vec![a.clone(), b.clone()]);
    let mut torn = bytes.clone();
    torn.truncate(torn.len() - 3);
    assert_eq!(decode(&torn), vec![a]);
    assert!(decode(b"nope").is_empty());
}

#[test]
fn entropy_is_zero_for_a_certain_token_and_ln2_for_a_coin() {
    assert!(store::entropy([0.0f32].into_iter()) < 1e-6);
    let h = store::entropy([0.5f32.ln(), 0.5f32.ln()].into_iter());
    assert!((h - std::f32::consts::LN_2).abs() < 1e-5);
}

fn open() -> (Store, crate::testing::TestDir) {
    let home = testing::temp_home();
    (Store::open(&home.join("trace")).unwrap(), home)
}

#[test]
fn traces_are_listed_filtered_and_paged() {
    let (s, home) = open();
    for (i, m) in ["a", "a", "b"].iter().enumerate() {
        let id = format!("t{i}");
        s.insert(
            &id,
            "chat",
            m,
            &json!({"n": i}),
            None,
            &json!({}),
            &Scope::new(None, (i < 2).then_some("task1")),
        )
        .unwrap();
        s.patch(
            &id,
            &json!({"prompt": format!("question {i} about cats_100%"), "answer": "fine"}),
        )
        .unwrap();
        s.finish(&id, if i == 2 { "error" } else { "ok" }, None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
    let f = |v: Value| Filter::from_json(&v);
    let (total, items) = s.list(&f(json!({}))).unwrap();
    assert_eq!((total, items[0]["id"].as_str()), (3, Some("t2")), "newest first");
    assert_eq!(s.list(&f(json!({"model": "a"}))).unwrap().0, 2);
    assert_eq!(s.list(&f(json!({"status": "error"}))).unwrap().0, 1);
    assert_eq!(s.list(&f(json!({"task": "task1"}))).unwrap().0, 2);
    assert_eq!(s.list(&f(json!({"q": "question 1"}))).unwrap().0, 1);
    assert_eq!(
        s.list(&f(json!({"q": "cats_100%"}))).unwrap().0,
        3,
        "% and _ are not wildcards"
    );
    assert_eq!(s.list(&f(json!({"q": "100_"}))).unwrap().0, 0);
    let (total, page) = s.list(&f(json!({"limit": 2, "offset": 2, "order": "asc"}))).unwrap();
    assert_eq!((total, page.len(), page[0]["id"].as_str()), (3, 1, Some("t2")));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn old_traces_and_then_the_oldest_go_by_age_and_size() {
    let (s, home) = open();
    let make = |id: &str, kb: usize| {
        s.insert(id, "chat", "m", &json!({}), None, &json!({}), &Scope::default())
            .unwrap();
        s.patch(id, &json!({"answer": "x".repeat(kb * 1024)})).unwrap();
        s.append_tokens(id, &[tok("a", -0.1, 0, &[("a", -0.1)])]).unwrap();
        s.finish(id, "ok", None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(3));
    };
    for (i, kb) in [600, 600, 600, 600].iter().enumerate() {
        make(&format!("t{i}"), *kb);
    }
    s.insert("live", "chat", "m", &json!({}), None, &json!({}), &Scope::default())
        .unwrap(); // still running: never deleted
    assert!(s.total_bytes() > 2_000_000);
    // 1 MB allowed: the oldest go until the rest fit
    let gone = s
        .retain(&TraceSettings {
            max_total_mb: 1,
            max_age_days: 0,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(gone, 3);
    let (total, left) = s.list(&Filter::from_json(&json!({}))).unwrap();
    assert_eq!(total, 2);
    assert!(left.iter().any(|t| t["id"] == "t3") && left.iter().any(|t| t["id"] == "live"));
    assert!(
        !s.dir.join("segments").join("t0.tok").exists(),
        "the segment file went with its trace"
    );
    assert!(s.dir.join("segments").join("t3.tok").exists());
    // age: one older than a day goes, a young one stays
    s.insert("old", "chat", "m", &json!({}), None, &json!({}), &Scope::default())
        .unwrap();
    s.finish("old", "ok", None).unwrap();
    s.exec("UPDATE traces SET started_ms = started_ms - 3 * 86400000 WHERE id = 'old'");
    let gone = s
        .retain(&TraceSettings {
            max_age_days: 1,
            max_total_mb: 0,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(gone, 1);
    assert!(!s.exists("old") && s.exists("t3"));
    // a segment file nobody owns is swept
    std::fs::write(s.dir.join("segments").join("ghost.tok"), b"x").unwrap();
    s.sweep_orphans();
    assert!(!s.dir.join("segments").join("ghost.tok").exists());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn export_writes_one_json_line_per_trace_with_its_tokens() {
    let (s, home) = open();
    for id in ["a", "b"] {
        s.insert(id, "chat", "m", &json!({}), None, &json!({}), &Scope::default())
            .unwrap();
        s.append_tokens(id, &[tok("hi", -0.5, 0, &[("hi", -0.5)])]).unwrap();
        s.add_event(id, 1, "selection", None, &json!({"kms": ["x"]})).unwrap();
        s.finish(id, "ok", None).unwrap();
    }
    let mut out = vec![];
    assert_eq!(s.export_jsonl(&Filter::from_json(&json!({})), &mut out).unwrap(), 2);
    let lines: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["tokens"][0]["text"], "hi");
    assert_eq!(lines[0]["events"][0]["kind"], "selection");
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn the_tracer_api_records_and_publishes() {
    let home = testing::temp_home();
    let t = tracer(&home);
    let mut rx = t.subscribe();
    let id = t
        .start_in(
            "km",
            "core",
            &json!({"q": 1}),
            None,
            &json!({"why": "test"}),
            &Scope::new(Some("job7"), Some("task3")),
        )
        .unwrap();
    let st = t.stage_start(&id, "selection").unwrap();
    t.event(
        &id,
        "selection",
        json!({"candidates": [{"km": "code.rust", "score": 0.9}]}),
    );
    t.stage_end(&st, json!({"picked": 1}));
    t.stage_span(&id, "load", 10, 20, json!({}));
    t.specialist_call(&id, "tts", json!({"text": "hi"}), json!({"bytes": 3}), 12, true);
    t.tokens(&id, &[tok("a", -0.1, 0, &[("a", -0.1)]), tok("b", -0.2, 0, &[])]);
    t.tokens(&id, &[tok("c", -0.3, 1, &[])]);
    t.update(&id, json!({"answer": "abc"}));
    t.finish(&id, "ok", None);
    let full = t.store().unwrap().get(&id, 100).unwrap().unwrap();
    assert_eq!(
        (
            full["kind"].as_str(),
            full["status"].as_str(),
            full["token_count"].as_i64()
        ),
        (Some("km"), Some("ok"), Some(3))
    );
    assert_eq!(full["stages"].as_array().unwrap().len(), 2);
    assert_eq!(full["events"][0]["data"]["candidates"][0]["km"], "code.rust");
    assert_eq!(full["specialist_calls"][0]["specialist"], "tts");
    assert_eq!(full["answer"], "abc");
    assert_eq!(
        (full["job_id"].as_str(), full["task_id"].as_str()),
        (Some("job7"), Some("task3"))
    );
    assert_eq!(t.store().unwrap().tokens(&id).len(), 3);
    let mut kinds = vec![];
    while let Ok(v) = rx.try_recv() {
        kinds.push(v["type"].as_str().unwrap().to_string());
    }
    assert_eq!(
        kinds,
        [
            "trace.start",
            "stage",
            "event",
            "stage",
            "stage",
            "specialist",
            "tokens",
            "tokens",
            "update",
            "trace.finish"
        ]
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn settings_persist_and_are_clamped() {
    let home = testing::temp_home();
    let t = tracer(&home);
    assert!(!t.enabled());
    let s = t
        .set_settings(TraceSettings {
            enabled: true,
            top_k: 99,
            ..Default::default()
        })
        .unwrap();
    assert!(t.enabled() && s.top_k == 20);
    let again: TraceSettings =
        serde_json::from_slice(&std::fs::read(home.join("trace/settings.json")).unwrap()).unwrap();
    assert_eq!(again, s);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn routing_blobs_round_trip_and_stay_inside_the_record() {
    let l = |layer: u32| store::RouteLayer {
        layer,
        experts: vec![3, 70_000 % 65_536, 9],
        weights: vec![0.5, 0.25, 0.125],
    };
    assert!(store::encode_routing(&[]).is_empty());
    let blob = store::encode_routing(&[l(0), l(7)]);
    let back = store::decode_routing(&blob).unwrap();
    assert_eq!(
        (back.len(), back[1].layer, back[1].experts[2], back[1].weights[0]),
        (2, 7, 9, 0.5)
    );
    assert!(
        store::decode_routing(&blob[..blob.len() - 1]).is_none(),
        "a torn blob is not routing"
    );
    assert!(store::decode_routing(&[1, 2, 3]).is_none());
    // far more layers than a record can hold: the blob still decodes, with fewer layers
    let many: Vec<_> = (0..5000).map(l).collect();
    let blob = store::encode_routing(&many);
    assert!(blob.len() <= u16::MAX as usize);
    assert!(store::decode_routing(&blob).unwrap().len() < 5000);
    // and it survives the segment file
    let mut t = tok("a", -0.1, 0, &[]);
    t.routing = store::encode_routing(&[l(1)]);
    let mut buf = header();
    encode(&t, &mut buf);
    assert_eq!(decode(&buf)[0].routing, t.routing);
}

// ---- a stand-in llama-server with log probabilities -------------------------------------------------------------

#[derive(Clone, Default)]
struct Fake {
    seen: Arc<Mutex<Vec<Value>>>,
}

/// (text, reasoning?) of what the stand-in "thinks" and says; the answer's last word depends on the request so that
/// a rerun with other settings differs: temperature 0 says 5, an adapter set to scale 0 says 42, else 4.
fn script(req: &Value) -> Vec<(String, bool)> {
    let last = if req["temperature"].as_f64() == Some(0.0) {
        " 5"
    } else if req["lora"]
        .as_array()
        .is_some_and(|l| l.iter().any(|x| x["scale"].as_f64() == Some(0.0)))
    {
        " 42"
    } else {
        " 4"
    };
    let mut v: Vec<(String, bool)> = vec![
        ("<think>".into(), true),
        ("Add".into(), true),
        (" it".into(), true),
        ("</think>".into(), true),
    ];
    for t in ["The", " answer", " is", last] {
        v.push((t.into(), false));
    }
    v
}

fn lp_entry(text: &str, k: usize) -> Value {
    let alts: Vec<Value> = [(text, -0.2f64), (" other", -1.8), (" else", -2.5), (" more", -4.0)]
        .iter()
        .take(k)
        .map(|(t, l)| json!({"token": t, "logprob": l, "bytes": t.bytes().collect::<Vec<_>>()}))
        .collect();
    json!({"token": text, "logprob": -0.2, "bytes": text.bytes().collect::<Vec<_>>(), "top_logprobs": alts})
}

/// What the patched llama-server reports for token `i` when the request has `mm_routing`: two MoE layers.
fn route_entry(i: usize) -> Value {
    json!([{"layer": 0, "experts": [i % 4, 4 + i % 2], "weights": [0.75, 0.25]},
           {"layer": 1, "experts": [1, 2], "weights": [0.5, 0.5]}])
}

async fn fake_chat(State(f): State<Fake>, Json(req): Json<Value>) -> Response {
    f.seen.lock().push(req.clone());
    let want_lp = req["logprobs"].as_bool().unwrap_or(false);
    let want_route = req["mm_routing"].as_bool().unwrap_or(false);
    let k = req["top_logprobs"].as_u64().unwrap_or(0) as usize;
    let toks = script(&req);
    let timings = json!({"prompt_n": 6, "prompt_ms": 12.5, "predicted_n": toks.len(), "predicted_ms": 40.0,
                         "predicted_per_second": 200.0});
    if req["stream"].as_bool().unwrap_or(false) {
        let events = async_stream::stream! {
            for (i, (t, r)) in toks.into_iter().enumerate() {
                let delta = if t.contains("think>") { json!({}) }
                    else if r { json!({"reasoning_content": t}) } else { json!({"content": t}) };
                let mut c = json!({"index": 0, "delta": delta, "finish_reason": null});
                if want_lp { c["logprobs"] = json!({"content": [lp_entry(&t, k)]}); }
                if want_route { c["mm_routing"] = route_entry(i); }
                yield Ok::<_, std::io::Error>(format!("data: {}\n\n", json!({"choices": [c]})));
            }
            let last = json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}], "timings": timings});
            yield Ok(format!("data: {last}\n\n"));
            yield Ok("data: [DONE]\n\n".to_string());
        };
        return ([("content-type", "text/event-stream")], Body::from_stream(events)).into_response();
    }
    let reasoning: String = toks
        .iter()
        .filter(|t| t.1 && !t.0.contains("think>"))
        .map(|t| t.0.as_str())
        .collect();
    let content: String = toks.iter().filter(|t| !t.1).map(|t| t.0.as_str()).collect();
    let mut choice = json!({"index": 0, "finish_reason": "stop",
                            "message": {"role": "assistant", "content": content, "reasoning_content": reasoning}});
    if want_lp {
        choice["logprobs"] = json!({"content": toks.iter().map(|t| lp_entry(&t.0, k)).collect::<Vec<_>>()});
    }
    if want_route {
        choice["mm_routing"] = json!((0..toks.len()).map(route_entry).collect::<Vec<_>>());
    }
    Json(json!({"model": req["model"], "choices": [choice], "timings": timings,
                "usage": {"prompt_tokens": 6, "completion_tokens": toks.len(), "total_tokens": 6 + toks.len()}}))
    .into_response()
}

async fn fake_template(Json(req): Json<Value>) -> Json<Value> {
    let text: String = req["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            format!(
                "<|im_start|>{}\n{}<|im_end|>\n",
                m["role"].as_str().unwrap(),
                m["content"].as_str().unwrap_or("")
            )
        })
        .collect();
    Json(json!({"prompt": format!("{text}<|im_start|>assistant\n")}))
}

struct Rig {
    hub: Hub,
    url: String,
    http: reqwest::Client,
    fake: Fake,
    home: crate::testing::TestDir,
    _server: tokio::task::JoinHandle<()>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.hub.engine.unload_all();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

async fn rig() -> Rig {
    let home = testing::temp_home();
    let hub = testing::hub(&home, &["qwen"]);
    let fake = Fake::default();
    let app = Router::new()
        .route("/v1/chat/completions", post(fake_chat))
        .route("/apply-template", post(fake_template))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let abort = task.abort_handle();
    hub.engine
        .adopt("qwen", port, "k", Some(Box::new(move || abort.abort())));
    let (addr, server) = testing::serve(hub.clone()).await;
    Rig {
        hub,
        url: format!("http://{addr}"),
        http: reqwest::Client::new(),
        fake,
        home,
        _server: server,
    }
}

impl Rig {
    fn chat(&self, extra: Value) -> reqwest::RequestBuilder {
        let mut body = json!({"model": "qwen", "messages": [{"role": "user", "content": "What is 2+2?"}]});
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        self.http
            .post(format!("{}/v1/chat/completions", self.url))
            .bearer_auth(TOKEN)
            .json(&body)
    }

    async fn get(&self, path: &str) -> (u16, Value) {
        let r = self
            .http
            .get(format!("{}{path}", self.url))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self
            .http
            .post(format!("{}{path}", self.url))
            .bearer_auth(TOKEN)
            .json(&body)
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }

    async fn traces(&self) -> Vec<Value> {
        self.get("/v1/native/traces").await.1["items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
}

// ---- capture ----------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_traced_answer_is_the_same_answer_and_the_trace_has_the_thoughts() {
    let r = rig().await;
    let plain: Value = r.chat(json!({})).send().await.unwrap().json().await.unwrap();
    assert!(r.traces().await.is_empty(), "untraced requests leave no trace");
    assert!(
        r.fake.seen.lock()[0].get("logprobs").is_none(),
        "and ask the model for no log probabilities"
    );

    let traced: Value = r
        .chat(json!({"mm_trace": true}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(traced["choices"][0]["message"], plain["choices"][0]["message"]);
    assert!(
        traced["choices"][0].get("logprobs").is_none(),
        "log probabilities the client did not ask for are taken out"
    );
    let sent = r.fake.seen.lock().last().cloned().unwrap();
    assert!(sent.get("mm_trace").is_none(), "mm_trace is the hub's, not the model's");
    assert_eq!(sent["top_logprobs"], 8);

    let items = r.traces().await;
    assert_eq!(items.len(), 1);
    let id = items[0]["id"].as_str().unwrap();
    let (code, t) = r.get(&format!("/v1/native/traces/{id}")).await;
    assert_eq!(code, 200);
    assert_eq!(
        (t["status"].as_str(), t["model"].as_str(), t["token_count"].as_i64()),
        (Some("ok"), Some("qwen"), Some(8))
    );
    assert_eq!(t["answer"], "The answer is 4");
    assert_eq!(t["reasoning"], "Add it");
    assert_eq!(t["prompt"], "What is 2+2?");
    assert!(t["rendered_prompt"]
        .as_str()
        .unwrap()
        .starts_with("<|im_start|>user\nWhat is 2+2?"));
    assert_eq!(t["timings"]["predicted_per_second"], 200.0);
    assert!(t["timings"]["wall_ms"].is_u64());
    assert!(t["resources"]["start"]["hub"]["rss_mb"].is_number());
    assert!(t["resources"]["end"]["system"]["total_mb"].as_f64().unwrap() > 0.0);
    let stages: Vec<&str> = t["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(stages, ["prepare", "prompt_eval", "generation"]);
    assert_eq!(t["request"]["messages"][0]["content"], "What is 2+2?");

    let (_, toks) = r.get(&format!("/v1/native/traces/{id}/tokens?offset=3&limit=3")).await;
    assert_eq!((toks["total"].as_i64(), toks["offset"].as_i64()), (Some(8), Some(3)));
    let list = toks["tokens"].as_array().unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(list[0]["text"], "</think>");
    assert_eq!(list[0]["phase"], "reasoning");
    assert_eq!(list[1]["phase"], "answer");
    assert_eq!(list[1]["alts"].as_array().unwrap().len(), 4);
    assert!(list[1]["entropy"].as_f64().unwrap() > 0.0);
    assert!((list[1]["p"].as_f64().unwrap() - (-0.2f64).exp()).abs() < 1e-4);
}

#[tokio::test]
async fn the_client_keeps_the_log_probabilities_it_asked_for() {
    let r = rig().await;
    let v: Value = r
        .chat(json!({"mm_trace": true, "logprobs": true, "top_logprobs": 2}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let first = &v["choices"][0]["logprobs"]["content"][0];
    assert_eq!(
        first["top_logprobs"].as_array().unwrap().len(),
        2,
        "no more alternatives than it asked for"
    );
    let id = r.traces().await[0]["id"].as_str().unwrap().to_string();
    let (_, toks) = r.get(&format!("/v1/native/traces/{id}/tokens")).await;
    assert_eq!(
        toks["tokens"][0]["alts"].as_array().unwrap().len(),
        4,
        "the trace has its own top_k"
    );
}

#[tokio::test]
async fn streaming_is_traced_and_the_stream_is_unchanged() {
    let r = rig().await;
    let plain = r
        .chat(json!({"stream": true}))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let traced = r
        .chat(json!({"stream": true, "mm_trace": true}))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(traced, plain, "same bytes with the log probabilities taken out again");
    let items = r.traces().await;
    assert_eq!(items.len(), 1);
    let id = items[0]["id"].as_str().unwrap();
    let (_, t) = r.get(&format!("/v1/native/traces/{id}")).await;
    assert_eq!((t["status"].as_str(), t["token_count"].as_i64()), (Some("ok"), Some(8)));
    assert_eq!(
        (t["answer"].as_str(), t["reasoning"].as_str()),
        (Some("The answer is 4"), Some("Add it"))
    );
    assert_eq!(t["timings"]["predicted_n"], 8);
    assert!(t["timings"]["first_token_ms"].is_u64());
    let stages: Vec<&str> = t["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(stages, ["prepare", "prompt_eval", "generation"]);
    let (_, toks) = r.get(&format!("/v1/native/traces/{id}/tokens")).await;
    let phases: Vec<&str> = toks["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["phase"].as_str().unwrap())
        .collect();
    assert_eq!(
        phases,
        [
            "reasoning",
            "reasoning",
            "reasoning",
            "reasoning",
            "answer",
            "answer",
            "answer",
            "answer"
        ]
    );
}

#[tokio::test]
async fn a_client_that_leaves_mid_stream_does_not_leave_a_running_trace() {
    let r = rig().await;
    let resp = r.chat(json!({"stream": true, "mm_trace": true})).send().await.unwrap();
    let mut s = resp.bytes_stream();
    let _ = s.next().await;
    drop(s);
    for _ in 0..50 {
        if r.traces().await.first().is_some_and(|t| t["status"] != "running") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let t = &r.traces().await[0];
    // the stand-in is fast: the whole stream may already have been read
    assert!(t["status"] == "aborted" || t["status"] == "ok", "{t}");
}

#[tokio::test]
async fn a_factory_task_names_its_calls_and_browses_them() {
    let r = rig().await;
    // naming a task is enough to be traced, by field or by header; the fields are not sent on to the model
    r.chat(json!({"mm_task": "task9", "mm_job": "gen-1"}))
        .send()
        .await
        .unwrap();
    r.http
        .post(format!("{}/v1/chat/completions", r.url))
        .bearer_auth(TOKEN)
        .header("x-mm-task", "task9")
        .json(&json!({"model": "qwen", "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    r.chat(json!({"mm_trace": true})).send().await.unwrap();
    r.chat(json!({"mm_task": "task9", "mm_trace": false}))
        .send()
        .await
        .unwrap();
    let sent = r.fake.seen.lock().first().cloned().unwrap();
    assert!(sent.get("mm_task").is_none() && sent.get("mm_job").is_none());
    let (_, l) = r.get("/v1/native/traces?task=task9").await;
    assert_eq!(l["total"], 2);
    let (_, l) = r.get("/v1/native/traces?job=gen-1").await;
    assert_eq!(
        (l["total"].as_i64(), l["items"][0]["task_id"].as_str()),
        (Some(1), Some("task9"))
    );
    assert_eq!(r.get("/v1/native/traces").await.1["total"], 3);
    // a rerun stays in its original's task
    let id = l["items"][0]["id"].as_str().unwrap();
    let (_, v) = r.post(&format!("/v1/native/traces/{id}/rerun"), json!({})).await;
    let (_, t) = r
        .get(&format!("/v1/native/traces/{}", v["trace"].as_str().unwrap()))
        .await;
    assert_eq!(
        (t["task_id"].as_str(), t["job_id"].as_str()),
        (Some("task9"), Some("gen-1"))
    );
}

#[tokio::test]
async fn the_setting_and_the_request_decide() {
    let r = rig().await;
    let (code, s) = r
        .post("/v1/native/settings", json!({"enabled": true, "top_k": 3}))
        .await;
    assert_eq!(
        (code, s["enabled"].as_bool(), s["top_k"].as_i64()),
        (200, Some(true), Some(3))
    );
    r.chat(json!({})).send().await.unwrap();
    assert_eq!(r.traces().await.len(), 1, "the setting traces every request");
    assert_eq!(r.fake.seen.lock()[0]["top_logprobs"], 3);
    r.chat(json!({"mm_trace": false})).send().await.unwrap();
    assert_eq!(r.traces().await.len(), 1, "and a request can opt out");
    r.post("/v1/native/settings", json!({"enabled": false})).await;
    r.http
        .post(format!("{}/v1/chat/completions", r.url))
        .bearer_auth(TOKEN)
        .header("x-mm-trace", "1")
        .json(&json!({"model": "qwen", "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.traces().await.len(), 2, "the header asks for one too");
    let (code, _) = r.post("/v1/native/settings", json!({"nonsense": 1})).await;
    assert_eq!(code, 400);
}

#[tokio::test]
async fn a_peer_cannot_use_the_trace_api_or_trace_itself() {
    let r = rig().await;
    let (_, k) = r.post("/v1/call/key.create", json!({"peer": "bob"})).await;
    let key = k["result"]["key"].as_str().unwrap().to_string();
    for path in [
        "/v1/native/traces",
        "/v1/native/stream",
        "/v1/native/settings",
        "/v1/native/export",
    ] {
        let resp = r
            .http
            .get(format!("{}{path}", r.url))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403, "{path}");
        let resp = r.http.get(format!("{}{path}", r.url)).send().await.unwrap();
        assert_eq!(resp.status(), 401, "{path} without any token");
    }
    let resp = r
        .http
        .post(format!("{}/v1/call/trace.list", r.url))
        .bearer_auth(&key)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
    r.http
        .post(format!("{}/v1/chat/completions", r.url))
        .bearer_auth(&key)
        .json(&json!({"model": "qwen", "mm_trace": true, "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert!(r.traces().await.is_empty(), "a peer's request is not traced");
    assert!(r.fake.seen.lock()[0].get("mm_trace").is_none());
}

#[tokio::test]
async fn the_operations_are_there_and_work() {
    let r = rig().await;
    let ops: Value = r.get("/v1/operations").await.1;
    let ids: Vec<&str> = ops["operations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap())
        .collect();
    for id in [
        "trace.list",
        "trace.get",
        "trace.tokens",
        "trace.rerun",
        "trace.diff",
        "trace.settings",
        "trace.export",
        "trace.delete",
    ] {
        assert!(ids.contains(&id), "{id}");
    }
    r.chat(json!({"mm_trace": true})).send().await.unwrap();
    let (code, v) = r.post("/v1/call/trace.list", json!({"limit": 5})).await;
    assert_eq!((code, v["result"]["total"].as_i64()), (200, Some(1)));
    let id = v["result"]["items"][0]["id"].as_str().unwrap().to_string();
    let (_, e) = r.post("/v1/call/trace.export", json!({})).await;
    assert_eq!(e["result"]["count"], 1);
    let text = std::fs::read_to_string(e["result"]["path"].as_str().unwrap()).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(text.lines().next().unwrap()).unwrap()["tokens"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    let (code, _) = r.post("/v1/call/trace.get", json!({"id": "nope"})).await;
    assert_eq!(code, 404);
    let export = r
        .http
        .get(format!("{}/v1/native/export", r.url))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(export.headers()["content-type"], "application/x-ndjson");
    let (code, _) = r.post("/v1/call/trace.delete", json!({"id": id})).await;
    assert_eq!(code, 200);
    assert!(r.traces().await.is_empty());
}

#[tokio::test]
async fn rerun_with_other_settings_and_diff() {
    let r = rig().await;
    r.chat(json!({"mm_trace": true, "temperature": 0.8}))
        .send()
        .await
        .unwrap();
    let id = r.traces().await[0]["id"].as_str().unwrap().to_string();
    let (code, v) = r
        .post(
            &format!("/v1/native/traces/{id}/rerun"),
            json!({"overrides": {"temperature": 0}}),
        )
        .await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["answer"], "The answer is 5");
    assert_eq!(v["rerun_of"], id.as_str());
    let d = &v["diff"];
    assert_eq!(d["answer"]["equal"], false);
    assert_eq!(d["params"][0]["key"], "temperature");
    assert_eq!(d["tokens"]["first_divergence"]["index"], 7);
    assert_eq!(d["tokens"]["first_divergence"]["a"]["text"], " 4");
    assert_eq!(d["tokens"]["first_divergence"]["b"]["text"], " 5");
    assert_eq!(d["same_prompt"], true);
    let new_id = v["trace"].as_str().unwrap().to_string();
    let (_, t) = r.get(&format!("/v1/native/traces/{new_id}")).await;
    assert_eq!(t["parent_id"], id.as_str());
    assert_eq!(t["meta"]["source"], "rerun");
    // the original stays as it was
    assert_eq!(
        r.get(&format!("/v1/native/traces/{id}")).await.1["answer"],
        "The answer is 4"
    );
    // diff on its own
    let (code, d2) = r.get(&format!("/v1/native/diff?a={id}&b={new_id}")).await;
    assert_eq!(code, 200);
    assert_eq!(d2["answer"], d["answer"]);
    // an adapter scaled to 0 reaches the model as a per-request lora scale
    let (_, v) = r
        .post(
            &format!("/v1/native/traces/{id}/rerun"),
            json!({"adapter_scales": {"0": 0}}),
        )
        .await;
    assert_eq!(v["answer"], "The answer is 42");
    assert_eq!(
        r.fake.seen.lock().last().unwrap()["lora"],
        json!([{"id": 0, "scale": 0.0}])
    );
    // removing adapters from a trace that had none is said plainly
    let (code, v) = r
        .post(
            &format!("/v1/native/traces/{id}/rerun"),
            json!({"remove_adapters": true}),
        )
        .await;
    assert_eq!(code, 400, "{v}");
    assert_eq!(r.post("/v1/native/traces/nope/rerun", json!({})).await.0, 404);
    assert_eq!(r.get("/v1/native/diff?a=x").await.0, 400);
}

#[test]
fn adapters_are_named_by_index_or_file_name() {
    let list = vec![
        json!({"index": 0, "name": "rust.gguf"}),
        json!({"index": 1, "name": "poetry.gguf"}),
    ];
    assert_eq!(api::named_adapters(&json!(true), &list).unwrap(), [0, 1]);
    assert_eq!(api::named_adapters(&json!(["poet", 0]), &list).unwrap(), [1, 0]);
    assert!(api::named_adapters(&json!([5]), &list).is_err());
    assert!(api::named_adapters(&json!("x"), &list).is_err());
}

#[tokio::test]
async fn the_live_stream_carries_a_request_as_it_happens() {
    let r = rig().await;
    let resp = r
        .http
        .get(format!("{}/v1/native/stream", r.url))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    let mut body = resp.bytes_stream();
    let first = String::from_utf8_lossy(&body.next().await.unwrap().unwrap()).to_string();
    assert!(first.contains("event: hello"), "{first}");
    r.chat(json!({"mm_trace": true, "stream": true}))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let mut seen = String::new();
    while !seen.contains("trace.finish") {
        let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), body.next())
            .await
            .expect("the finish event arrives")
            .unwrap()
            .unwrap();
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
    for ev in [
        "event: trace.start",
        "event: stage",
        "event: tokens",
        "event: update",
        "event: trace.finish",
    ] {
        assert!(seen.contains(ev), "{ev} in {seen}");
    }
    // a token event holds the token with its alternatives
    let line = seen
        .lines()
        .rfind(|l| l.starts_with("data:") && l.contains("\"type\":\"tokens\""))
        .unwrap();
    let v: Value = serde_json::from_str(line.trim_start_matches("data:").trim()).unwrap();
    assert!(v["tokens"][0]["alts"].as_array().is_some());
}

#[tokio::test]
async fn the_dashboard_is_served_and_the_page_needs_no_token() {
    let r = rig().await;
    std::fs::create_dir_all(r.home.join("ui/assets")).unwrap();
    std::fs::write(r.home.join("ui/index.html"), "<h1>dash</h1>").unwrap();
    std::fs::write(r.home.join("ui/assets/app.js"), "1").unwrap();
    let no_redirect = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = no_redirect.get(format!("{}/ui", r.url)).send().await.unwrap();
    assert_eq!(
        (resp.status().as_u16(), resp.headers()["location"].to_str().unwrap()),
        (308, "/ui/")
    );
    let resp = r.http.get(format!("{}/ui/", r.url)).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    assert!(resp.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .contains("default-src 'self'"));
    assert_eq!(resp.text().await.unwrap(), "<h1>dash</h1>");
    let js = r.http.get(format!("{}/ui/assets/app.js", r.url)).send().await.unwrap();
    assert!(js.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/javascript"));
    assert_eq!(
        r.http
            .get(format!("{}/ui/assets/none.js", r.url))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        r.http
            .get(format!("{}/ui/..%2Fcontrol.json", r.url))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    // the API behind it still wants the token
    assert_eq!(
        r.http
            .get(format!("{}/v1/native/traces", r.url))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
}

// ---- expert routing ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn expert_routing_is_asked_for_recorded_and_summed_per_layer() {
    let r = rig().await;
    // not asked: the model's server is not asked either, and nothing is recorded
    r.chat(json!({"mm_trace": true})).send().await.unwrap();
    assert!(r.fake.seen.lock()[0].get("mm_routing").is_none());
    let id = r.traces().await[0]["id"].as_str().unwrap().to_string();
    let (_, t) = r.get(&format!("/v1/native/traces/{id}")).await;
    assert!(t["events"].as_array().unwrap().iter().all(|e| e["kind"] != "routing"));

    // the client asks: it gets the routing back, and the trace has it
    let v: Value = r
        .chat(json!({"mm_trace": true, "mm_routing": true}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r.fake.seen.lock().last().unwrap()["mm_routing"], true);
    assert_eq!(v["choices"][0]["mm_routing"].as_array().unwrap().len(), 8);
    let items = r.traces().await;
    let id = items.iter().find(|t| t["id"] != id.as_str()).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, toks) = r.get(&format!("/v1/native/traces/{id}/tokens")).await;
    let routing = &toks["tokens"][2]["routing"];
    assert_eq!(routing[0]["layer"], 0);
    assert_eq!(routing[0]["experts"], json!([2, 4]));
    assert_eq!(routing[0]["weights"], json!([0.75, 0.25]));
    assert_eq!(routing[1]["experts"], json!([1, 2]));
    let (_, t) = r.get(&format!("/v1/native/traces/{id}")).await;
    let ev: Vec<&Value> = t["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "routing")
        .collect();
    assert_eq!(ev.len(), 2, "one event per layer");
    let l1 = ev.iter().find(|e| e["data"]["layer"] == 1).unwrap();
    assert_eq!(l1["data"]["experts"], json!([1, 2]));
    assert_eq!(l1["data"]["weights"], json!([4.0, 4.0]), "8 tokens at 0.5 each");
    assert_eq!(l1["data"]["tokens"], 8);
    let l0 = ev.iter().find(|e| e["data"]["layer"] == 0).unwrap();
    assert_eq!(l0["data"]["experts"], json!([0, 1, 2, 3, 4, 5]));
}

#[tokio::test]
async fn the_tracing_setting_asks_for_routing_without_the_client_seeing_it() {
    let r = rig().await;
    tracer(&r.home)
        .set_settings(TraceSettings {
            routing: true,
            ..Default::default()
        })
        .unwrap();
    let v: Value = r
        .chat(json!({"mm_trace": true}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r.fake.seen.lock()[0]["mm_routing"], true);
    assert!(v["choices"][0].get("mm_routing").is_none(), "the client did not ask");

    let plain = r
        .chat(json!({"stream": true}))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let traced = r
        .chat(json!({"stream": true, "mm_trace": true}))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(traced, plain, "the stream is unchanged");
    assert!(!traced.contains("mm_routing"));
    let items = r.traces().await;
    assert_eq!(items.len(), 2);
    let id = items[0]["id"].as_str().unwrap();
    let (_, t) = r.get(&format!("/v1/native/traces/{id}")).await;
    let n = t["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "routing")
        .count();
    assert_eq!(n, 2, "the streamed trace has its routing too");
    let (_, toks) = r.get(&format!("/v1/native/traces/{id}/tokens")).await;
    assert_eq!(toks["tokens"][7]["routing"][0]["experts"], json!([3, 5]));
}

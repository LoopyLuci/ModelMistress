//! Tests for the Ollama-compatible API: the shapes (unit level) and the routes over a hub with stand-ins for
//! llama-server (no real model needed).

use std::time::Duration;

use serde_json::{json, Value};

use crate::hub::{self, Hub};
use crate::ollama_api::*;
use crate::testing::{self, stand_in, TOKEN};

const MODELFILE: &str = "# a comment\nFROM tiny\nADAPTER style.gguf\nTEMPLATE \"\"\"{{ if .System }}<s>{{ .System }}</s>{{ end }}{{ .Prompt }}\n<a>\"\"\"\nSYSTEM \"\"\"You are terse.\nReally.\"\"\"\nPARAMETER temperature 0.2\nPARAMETER num_ctx 4096\nPARAMETER stop <|end|>\nPARAMETER stop \"###\"\nMESSAGE user Hi there\nMESSAGE assistant \"\"\"Hello.\nHow can I help?\"\"\"\nLICENSE MIT\n";

#[test]
fn modelfile_parses_every_instruction() {
    let p = parse_modelfile(MODELFILE).unwrap();
    assert_eq!(p.from, "tiny");
    assert_eq!(p.adapters, ["style.gguf"]);
    assert_eq!(p.system.as_deref(), Some("You are terse.\nReally."));
    assert_eq!(
        p.template.as_deref().unwrap(),
        "{{ if .System }}<s>{{ .System }}</s>{{ end }}{{ .Prompt }}\n<a>"
    );
    assert_eq!(p.parameters["temperature"], json!(0.2));
    assert_eq!(p.parameters["num_ctx"], json!(4096));
    assert_eq!(p.parameters["stop"], json!(["<|end|>", "###"]));
    assert_eq!(p.messages[1]["content"], "Hello.\nHow can I help?");
    assert_eq!(p.license.as_deref(), Some("MIT"));
}

#[test]
fn modelfile_round_trips() {
    let p = parse_modelfile(MODELFILE).unwrap();
    let again = parse_modelfile(&render_modelfile(&p)).unwrap();
    assert_eq!(p, again);
}

#[test]
fn modelfile_errors_are_clear() {
    assert!(parse_modelfile("SYSTEM hi").unwrap_err().contains("FROM"));
    assert!(parse_modelfile("FROM x\nBOGUS 1").unwrap_err().contains("BOGUS"));
    assert!(parse_modelfile("FROM x\nPARAMETER temperature hot").is_err());
    assert!(parse_modelfile("FROM x\nMESSAGE robot hi").is_err());
}

#[test]
fn templates_and_options_map() {
    let t = "{{ if .System }}<s>{{ .System }}</s>{{ end }}{{- .Prompt }}";
    assert_eq!(render_template(t, "S", "P"), "<s>S</s>P");
    assert_eq!(render_template(t, "", "P"), "P");
    let mut b = json!({});
    let o = json!({"num_predict": 7, "temperature": 0.5, "num_ctx": 99, "stop": ["x"]});
    apply_options(&mut b, o.as_object().unwrap());
    assert_eq!(b, json!({"max_tokens": 7, "temperature": 0.5, "stop": ["x"]}));
    assert_eq!(keep_alive_s(&json!("5m")), Some(300));
    assert_eq!(keep_alive_s(&json!(0)), Some(0));
    assert_eq!(keep_alive_s(&json!("0")), Some(0));
    assert_eq!(keep_alive_s(&json!(-1)), Some(-1));
}

#[test]
fn chat_request_maps_to_openai() {
    let p = parse_modelfile("FROM tiny\nSYSTEM be brief\nPARAMETER temperature 0.1\nMESSAGE user q\n").unwrap();
    let req = json!({"model": "x", "stream": false, "options": {"temperature": 0.9},
        "messages": [{"role": "user", "content": "hi", "images": ["AAAA"]},
                     {"role": "assistant", "content": "", "tool_calls": [{"function": {"name": "f", "arguments": {"a": 1}}}]}]});
    let b = chat_body(&req, "tiny", Some(&p));
    assert_eq!(b["model"], "tiny");
    assert_eq!(b["temperature"], 0.9); // the request wins over the preset
    assert!(b.get("stream").is_none());
    assert_eq!(b["messages"][0], json!({"role": "system", "content": "be brief"}));
    assert_eq!(b["messages"][1]["content"], "q");
    assert_eq!(
        b["messages"][2]["content"][1]["image_url"]["url"],
        "data:image/png;base64,AAAA"
    );
    assert_eq!(b["messages"][3]["tool_calls"][0]["function"]["arguments"], "{\"a\":1}");
    let s = chat_body(&json!({"messages": []}), "tiny", None);
    assert_eq!(s["stream"], true); // Ollama streams by default
}

#[test]
fn sse_becomes_ndjson_however_the_bytes_are_cut() {
    let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"length\"}]}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":5},\"timings\":{\"predicted_ms\":2.0}}\n\ndata: [DONE]\n\n";
    for cut in [1, 7, 1000] {
        let mut n = Ndjson::new(Kind::Chat, "m");
        let mut out = vec![];
        for part in sse.as_bytes().chunks(cut) {
            out.extend(n.push(part));
        }
        out.extend(n.finish());
        assert_eq!(out.len(), 3, "cut {cut}: {out:?}");
        assert_eq!(out[0]["message"]["content"], "Hel");
        assert_eq!(out[0]["done"], false);
        assert_eq!(out[1]["message"]["content"], "lo");
        let last = &out[2];
        assert_eq!(last["done"], true);
        assert_eq!(last["done_reason"], "length");
        assert_eq!(last["prompt_eval_count"], 3);
        assert_eq!(last["eval_count"], 5);
        assert_eq!(last["eval_duration"], 2_000_000);
    }
    // no [DONE]: finish() still ends the stream with one done line
    let mut n = Ndjson::new(Kind::Generate, "m");
    assert_eq!(
        n.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n").len(),
        1
    );
    let end = n.finish();
    assert_eq!(end.len(), 1);
    assert_eq!(end[0]["response"], "");
    assert!(n.finish().is_empty());
}

#[test]
fn answers_and_embeddings_map() {
    let v = json!({"choices": [{"finish_reason": "stop", "message": {"role": "assistant", "content": "yo",
        "tool_calls": [{"function": {"name": "f", "arguments": "{\"a\":1}"}}]}}],
        "usage": {"prompt_tokens": 2, "completion_tokens": 1}});
    let a = answer(Kind::Chat, "m", &v, Duration::from_millis(1));
    assert_eq!(a["message"]["content"], "yo");
    assert_eq!(a["message"]["tool_calls"][0]["function"]["arguments"], json!({"a": 1}));
    assert_eq!(a["eval_count"], 1);
    let g = answer(Kind::Generate, "m", &v, Duration::ZERO);
    assert_eq!(g["response"], "yo");
    let e = json!({"data": [{"embedding": [0.1, 0.2]}, {"embedding": [0.3]}], "usage": {"prompt_tokens": 4}});
    let n = embed_answer(false, "m", &e, Duration::ZERO);
    assert_eq!(n["embeddings"], json!([[0.1, 0.2], [0.3]]));
    assert_eq!(n["prompt_eval_count"], 4);
    assert_eq!(
        embed_answer(true, "m", &e, Duration::ZERO),
        json!({"embedding": [0.1, 0.2]})
    );
    assert_eq!(
        embed_body(&json!({"prompt": "x"}), "m"),
        json!({"model": "m", "input": "x"})
    );
}

async fn call(h: &Hub, method: &str, path: &str, body: Option<Value>) -> (u16, Value, String) {
    use tower::ServiceExt;
    let mut r = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {TOKEN}"));
    let b = match body {
        Some(v) => {
            r = r.header("content-type", "application/json");
            axum::body::Body::from(v.to_string())
        }
        None => axum::body::Body::empty(),
    };
    let resp = hub::router(h.clone()).oneshot(r.body(b).unwrap()).await.unwrap();
    let st = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22).await.unwrap();
    let text = String::from_utf8_lossy(&bytes).to_string();
    (st, serde_json::from_str(&text).unwrap_or(Value::Null), text)
}

#[tokio::test]
async fn tags_version_show_ps_answer() {
    let home = testing::temp_home();
    let h = testing::hub(&home, &["tiny"]);
    let (st, v, _) = call(&h, "GET", "/api/version", None).await;
    assert_eq!((st, v["version"].as_str()), (200, Some(hub::VERSION)));
    let (_, v, _) = call(&h, "GET", "/api/tags", None).await;
    let m = &v["models"][0];
    assert_eq!(m["name"], "tiny");
    assert_eq!(m["details"]["format"], "gguf");
    assert!(m["digest"].as_str().unwrap().len() == 64 && m["size"].as_u64().unwrap() > 0);
    let (st, v, _) = call(&h, "POST", "/api/show", Some(json!({"model": "tiny"}))).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["details"]["family"], "llama");
    assert!(v["modelfile"].as_str().unwrap().starts_with("FROM "));
    assert_eq!(v["capabilities"][0], "completion");
    assert_eq!(v["model_info"]["general.architecture"], "llama");
    let (st, v, _) = call(&h, "POST", "/api/show", Some(json!({"name": "nope"}))).await;
    assert_eq!(st, 404);
    assert!(v["error"].as_str().unwrap().contains("nope"));
    assert_eq!(call(&h, "GET", "/api/ps", None).await.1, json!({"models": []}));
    h.engine.adopt("tiny", 1, "k", None);
    let (_, v, _) = call(&h, "GET", "/api/ps", None).await;
    assert_eq!(v["models"][0]["name"], "tiny");
    assert_eq!(v["models"][0]["expires_at"], "0001-01-01T00:00:00Z");
    h.engine.unload_all();
}

#[tokio::test]
async fn the_api_needs_the_token() {
    use tower::ServiceExt;
    let home = testing::temp_home();
    let h = testing::hub(&home, &["tiny"]);
    for path in ["/api/tags", "/api/version", "/api/ps"] {
        let r = axum::http::Request::get(path).body(axum::body::Body::empty()).unwrap();
        let resp = hub::router(h.clone()).oneshot(r).await.unwrap();
        assert_eq!(resp.status(), 401, "{path}");
    }
}

#[tokio::test]
async fn chat_and_generate_stream_ndjson_and_answer_whole() {
    let home = testing::temp_home();
    let h = testing::hub(&home, &["tiny"]);
    let s = stand_in("pong", 0).await;
    h.engine.adopt("tiny", s.port, "k", Some(s.on_stop()));
    let msgs = json!([{"role": "user", "content": "ping"}]);
    let (st, v, _) = call(
        &h,
        "POST",
        "/api/chat",
        Some(json!({"model": "tiny", "messages": msgs, "stream": false})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["message"], json!({"role": "assistant", "content": "pong"}));
    assert_eq!((v["done"].as_bool(), v["eval_count"].as_u64()), (Some(true), Some(5)));
    let (_, _, text) = call(
        &h,
        "POST",
        "/api/chat",
        Some(json!({"model": "tiny", "messages": msgs})),
    )
    .await;
    let rows: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(rows.len(), 4, "{text}"); // three content events and the done line
    assert!(rows[..3]
        .iter()
        .all(|r| r["done"] == false && r["message"]["content"] == "pong"));
    assert_eq!(
        (rows[3]["done"].clone(), rows[3]["eval_count"].clone()),
        (json!(true), json!(5))
    );
    // generate: through the chat path, then raw through the completions path
    let body = json!({"model": "tiny", "prompt": "hi", "system": "s", "stream": false});
    let (_, v, _) = call(&h, "POST", "/api/generate", Some(body)).await;
    assert_eq!(v["response"], "pong");
    let body = json!({"model": "tiny", "prompt": "hi", "raw": true, "stream": false});
    let (_, v, _) = call(&h, "POST", "/api/generate", Some(body)).await;
    assert_eq!(v["done"], true);
    let body = json!({"model": "tiny", "prompt": "hi", "template": "{{ .Prompt }}!"});
    let (_, _, text) = call(&h, "POST", "/api/generate", Some(body)).await;
    assert_eq!(text.lines().count(), 4);
    // keep_alive 0 on an empty request unloads
    let (_, v, _) = call(&h, "POST", "/api/chat", Some(json!({"model": "tiny", "keep_alive": 0}))).await;
    assert_eq!(v["done_reason"], "unload");
    assert!(!h.engine.is_loaded("tiny"));
    // an unknown model is Ollama's {"error"}
    let (st, v, _) = call(
        &h,
        "POST",
        "/api/chat",
        Some(json!({"model": "ghost", "messages": msgs})),
    )
    .await;
    assert!(st >= 400 && v["error"].is_string(), "{st} {v}");
}

#[tokio::test]
async fn create_copy_use_and_delete_a_preset() {
    let home = testing::temp_home();
    let h = testing::hub(&home, &["tiny"]);
    let s = stand_in("pong", 0).await;
    h.engine.adopt("tiny", s.port, "k", Some(s.on_stop()));
    let mf = "FROM tiny\nSYSTEM be brief\nPARAMETER temperature 0.3\n";
    let body = json!({"model": "brief", "modelfile": mf, "stream": false});
    let (st, v, _) = call(&h, "POST", "/api/create", Some(body)).await;
    assert_eq!((st, v["status"].as_str()), (200, Some("success")), "{v}");
    let (_, v, _) = call(&h, "GET", "/api/tags", None).await;
    let names: Vec<&str> = v["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["name"].as_str())
        .collect();
    assert_eq!(names, ["tiny", "brief:latest"]);
    assert_eq!(v["models"][1]["details"]["parent_model"], "tiny");
    let (_, v, _) = call(&h, "POST", "/api/show", Some(json!({"model": "brief:latest"}))).await;
    assert!(v["modelfile"].as_str().unwrap().contains("SYSTEM \"\"\"be brief\"\"\""));
    assert_eq!(v["parameters"], "temperature 0.3\n");
    // it runs its base model
    let body = json!({"model": "brief", "messages": [{"role": "user", "content": "x"}], "stream": false});
    let (st, v, _) = call(&h, "POST", "/api/chat", Some(body)).await;
    assert_eq!((st, v["message"]["content"].as_str()), (200, Some("pong")), "{v}");
    // copy, in both directions
    let cp = |a: &str, b: &str| json!({"source": a, "destination": b});
    assert_eq!(call(&h, "POST", "/api/copy", Some(cp("brief", "brief2"))).await.0, 200);
    assert_eq!(call(&h, "POST", "/api/copy", Some(cp("tiny", "alias"))).await.0, 200);
    assert_eq!(call(&h, "POST", "/api/copy", Some(cp("ghost", "z"))).await.0, 404);
    assert_eq!(call(&h, "POST", "/api/copy", Some(cp("brief", "tiny"))).await.0, 409);
    assert_eq!(get_preset(&home, "brief2").unwrap().system.as_deref(), Some("be brief"));
    assert_eq!(get_preset(&home, "alias").unwrap().from, "tiny");
    // a preset on a model that is not here, a bad Modelfile, a missing adapter
    for mf in ["FROM ghost\n", "SYSTEM x\n", "FROM tiny\nADAPTER nothing.gguf\n"] {
        let body = json!({"model": "bad", "modelfile": mf});
        let (st, v, _) = call(&h, "POST", "/api/create", Some(body)).await;
        assert!(st >= 400 && v["error"].is_string(), "{mf}: {st} {v}");
    }
    // delete removes presets (the weights stay)
    assert_eq!(
        call(&h, "DELETE", "/api/delete", Some(json!({"model": "brief:latest"})))
            .await
            .0,
        200
    );
    assert!(get_preset(&home, "brief").is_none());
    assert_eq!(call(&h, "GET", "/api/tags", None).await.1["models"][0]["name"], "tiny");
    h.engine.unload_all();
}

#[tokio::test]
async fn the_v1_routes_and_operations_still_work() {
    let home = testing::temp_home();
    let h = testing::hub(&home, &["tiny"]);
    let (st, v, _) = call(&h, "GET", "/v1/models", None).await;
    assert_eq!((st, v["data"][0]["id"].as_str()), (200, Some("tiny")));
    let (_, v, _) = call(&h, "POST", "/v1/call/model.show", Some(json!({"model": "tiny"}))).await;
    assert_eq!(v["result"]["details"]["format"], "gguf");
}

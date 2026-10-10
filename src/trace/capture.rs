//! Capturing the hub's chat path.
//!
//! When tracing is on for a request, the hub asks llama-server for log probabilities (`logprobs`, `top_logprobs`),
//! lets the answer through unchanged (the log probabilities the client did not ask for are taken out again), and
//! records the prompt, the model's reasoning apart from its answer, every token with its alternatives and entropy,
//! the timings, the adapters attached, and RAM at the start and the end. Streaming and not streaming both work.
//! With tracing off none of this runs: [`wants`] is the only cost.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use futures::StreamExt;
use serde_json::{json, Value};

use super::store::{entropy, now_ms, NO_ID};
use super::{tracer, Alt, Scope, Stage, TokenRec, Tracer};
use crate::engine::{Engine, Fail, Target};
use crate::hub::Hub;

/// The hub's chat route calls this first: should this request be traced, and for which job/task? Removes the
/// fields that are ours (`mm_trace`, `mm_job`, `mm_task`) either way. A request that names a job or task (or sends
/// the `x-mm-job` / `x-mm-task` headers; the KM factory's model calls do) is traced unless `mm_trace` is false.
pub fn wants(home: &Path, headers: &HeaderMap, body: &mut Value) -> Option<Scope> {
    let (mut field, mut job, mut task) = (None, None, None);
    if let Some(o) = body.as_object_mut() {
        field = o.remove("mm_trace");
        job = o.remove("mm_job");
        task = o.remove("mm_task");
    }
    let hdr = |n: &str| headers.get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
    let text = |v: Option<Value>, h: &str| v.and_then(|v| v.as_str().map(str::to_string)).or_else(|| hdr(h));
    let scope = Scope {
        job: text(job, "x-mm-job"),
        task: text(task, "x-mm-task"),
    };
    let truthy = |s: &str| matches!(s.trim(), "1" | "true" | "yes" | "on");
    let asked = match field {
        Some(Value::Bool(b)) => Some(b),
        Some(Value::String(s)) => Some(truthy(&s)),
        _ => hdr("x-mm-trace").map(|h| truthy(&h)),
    };
    let on = match asked {
        Some(b) => b,
        None => scope.job.is_some() || scope.task.is_some() || tracer(home).enabled(),
    };
    on.then_some(scope)
}

// ---- reading llama-server's answers --------------------------------------------------------------------------

/// Which tokens are reasoning: from `<think>` to `</think>` (Qwen and others), or from the start when the chat
/// template opened the tag in the prompt (then the server's `reasoning_content` says so).
#[derive(Default)]
struct Phase {
    reasoning: bool,
}

impl Phase {
    fn mark(&mut self, text: &str) -> u8 {
        if text.contains("<think>") {
            self.reasoning = true;
            return 1;
        }
        if text.contains("</think>") {
            self.reasoning = false;
            return 1;
        }
        self.reasoning as u8
    }
}

fn token_text(t: &Value) -> String {
    if let Some(b) = t["bytes"].as_array() {
        let v: Vec<u8> = b.iter().filter_map(|x| x.as_u64().map(|n| n as u8)).collect();
        if !v.is_empty() {
            return String::from_utf8_lossy(&v).into_owned();
        }
    }
    t["token"].as_str().unwrap_or("").to_string()
}

fn id_of(t: &Value) -> u32 {
    t["id"].as_u64().map(|v| v as u32).unwrap_or(NO_ID)
}

fn lp_of(t: &Value) -> f32 {
    t["logprob"].as_f64().unwrap_or(0.0).max(-1.0e4) as f32
}

/// The tokens of one `logprobs.content` array.
fn tokens_of(content: &Value, k: usize, ph: &mut Phase) -> Vec<TokenRec> {
    let Some(arr) = content.as_array() else { return vec![] };
    arr.iter()
        .map(|t| {
            let text = token_text(t);
            let alts: Vec<Alt> = t["top_logprobs"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .take(k)
                        .map(|x| Alt {
                            id: id_of(x),
                            text: token_text(x),
                            logprob: lp_of(x),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let phase = ph.mark(&text);
            TokenRec {
                id: id_of(t),
                logprob: lp_of(t),
                entropy: entropy(alts.iter().map(|a| a.logprob)),
                phase,
                alts,
                routing: vec![],
                text,
            }
        })
        .collect()
}

/// Split generated text into (reasoning, answer) at the think tags.
pub fn split_think(content: &str) -> (String, String) {
    if let Some(close) = content.find("</think>") {
        let before = &content[..close];
        let reasoning = before.split_once("<think>").map(|(_, r)| r).unwrap_or(before);
        return (
            reasoning.trim().to_string(),
            content[close + "</think>".len()..].trim_start().to_string(),
        );
    }
    if let Some((_, r)) = content.split_once("<think>") {
        return (r.trim().to_string(), String::new()); // cut off while still thinking
    }
    (String::new(), content.to_string())
}

/// Make a response what the client asked for: no `logprobs` unless they asked, and no more alternatives than they
/// asked for. Returns whether anything changed.
fn shape(v: &mut Value, client_top: Option<usize>) -> bool {
    let mut changed = false;
    let Some(choices) = v.get_mut("choices").and_then(Value::as_array_mut) else {
        return false;
    };
    for c in choices {
        let Some(o) = c.as_object_mut() else { continue };
        match client_top {
            None => changed |= o.remove("logprobs").is_some(),
            Some(n) => {
                if let Some(items) = o
                    .get_mut("logprobs")
                    .and_then(|l| l.get_mut("content"))
                    .and_then(Value::as_array_mut)
                {
                    for it in items {
                        if let Some(a) = it.get_mut("top_logprobs").and_then(Value::as_array_mut) {
                            if a.len() > n {
                                a.truncate(n);
                                changed = true;
                            }
                        }
                    }
                }
            }
        }
    }
    changed
}

// ---- resources ------------------------------------------------------------------------------------------------

/// RAM of the hub, of each model server it runs, and of the machine. GPU memory is not reported by llama-server,
/// so it is left out rather than guessed.
fn sample(engine: &Engine) -> Value {
    use sysinfo::{Pid, System};
    let mut sys = System::new();
    sys.refresh_memory();
    let mb = |b: u64| (b as f64 / 1_048_576.0 * 10.0).round() / 10.0;
    let rss = |sys: &mut System, pid: u32| {
        let p = Pid::from_u32(pid);
        sys.refresh_process(p);
        sys.process(p).map(|p| mb(p.memory()))
    };
    let models: Vec<Value> = engine
        .loaded()
        .iter()
        .filter(|i| i.pid != 0)
        .map(|i| json!({"model": i.model, "pid": i.pid, "rss_mb": rss(&mut sys, i.pid)}))
        .collect();
    json!({"ts_ms": now_ms(), "hub": {"pid": std::process::id(), "rss_mb": rss(&mut sys, std::process::id())},
           "models": models,
           "system": {"total_mb": mb(sys.total_memory()), "available_mb": mb(sys.available_memory())}})
}

/// The adapters on a model's server, as they apply to this request (a request's own `lora` changes the scales).
fn adapters(engine: &Engine, model: &str, request: &Value) -> Value {
    let Some(inst) = engine.loaded().into_iter().find(|i| i.model == model) else {
        return json!([]);
    };
    let overrides = request
        .get("lora")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let list: Vec<Value> = inst
        .extras
        .lora
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let eff = overrides
                .iter()
                .find(|o| o["id"].as_u64() == Some(i as u64))
                .and_then(|o| o["scale"].as_f64())
                .unwrap_or(l.scale as f64);
            json!({"index": i, "path": l.path, "name": l.path.file_name().map(|n| n.to_string_lossy().to_string()),
                   "scale": l.scale, "effective_scale": eff})
        })
        .collect();
    json!(list)
}

// ---- one traced request ---------------------------------------------------------------------------------------

struct Rec {
    tr: Arc<Tracer>,
    engine: Arc<Engine>,
    id: String,
    model: String,
    request: Value,
    t0: Instant,
    k: usize,
    phase: Phase,
    pending: Vec<TokenRec>,
    flushed: Instant,
    stage: Option<Stage>,
    reasoning_hint: String,
    content: String,
    timings: Value,
    usage: Value,
    finish_reason: Value,
    first_token: Option<Instant>,
    sent: Option<Instant>,
    res_start: Value,
    done: bool,
}

impl Rec {
    fn enter(&mut self, name: &str) {
        if let Some(s) = self.stage.take() {
            self.tr.stage_end(&s, Value::Null);
        }
        self.stage = self.tr.stage_start(&self.id, name);
    }

    fn flush(&mut self) {
        if !self.pending.is_empty() {
            self.tr.tokens(&self.id, &self.pending);
            self.pending.clear();
        }
        self.flushed = Instant::now();
    }

    fn push(&mut self, toks: Vec<TokenRec>) {
        self.pending.extend(toks);
        if self.pending.len() >= 16 || self.flushed.elapsed() > Duration::from_millis(100) {
            self.flush();
        }
    }

    /// One streamed event (a parsed `data:` payload).
    fn feed_chunk(&mut self, v: &Value) {
        if let Some(t) = v.get("timings").filter(|t| t.is_object()) {
            self.timings = t.clone();
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            self.usage = u.clone();
        }
        let Some(c) = v["choices"].get(0) else { return };
        if !c["finish_reason"].is_null() {
            self.finish_reason = c["finish_reason"].clone();
        }
        let delta = &c["delta"];
        let r = delta["reasoning_content"].as_str().unwrap_or("");
        let t = delta["content"].as_str().unwrap_or("");
        if self.first_token.is_none() && (!r.is_empty() || !t.is_empty() || c["logprobs"].is_object()) {
            self.first_token = Some(Instant::now());
            self.enter("generation");
        }
        if !r.is_empty() {
            self.phase.reasoning = true;
            self.reasoning_hint.push_str(r);
        } else if !t.is_empty() && !t.contains("</think>") && !t.contains("<think>") {
            self.phase.reasoning = false;
        }
        self.content.push_str(t);
        let toks = tokens_of(&c["logprobs"]["content"], self.k, &mut self.phase);
        self.push(toks);
    }

    /// A whole answer that was not streamed.
    fn feed_whole(&mut self, v: &Value) {
        self.timings = v.get("timings").cloned().unwrap_or(Value::Null);
        self.usage = v.get("usage").cloned().unwrap_or(Value::Null);
        let Some(c) = v["choices"].get(0) else { return };
        self.finish_reason = c["finish_reason"].clone();
        let m = &c["message"];
        self.content = m["content"].as_str().unwrap_or("").to_string();
        self.reasoning_hint = m["reasoning_content"].as_str().unwrap_or("").to_string();
        let first = c["logprobs"]["content"].get(0).map(token_text).unwrap_or_default();
        self.phase.reasoning = !self.reasoning_hint.is_empty() && !first.contains("<think>");
        let toks = tokens_of(&c["logprobs"]["content"], self.k, &mut self.phase);
        self.push(toks);
    }

    fn finish(&mut self, status: &str, error: Option<&str>, res_end: Option<Value>) {
        if self.done {
            return;
        }
        self.done = true;
        self.flush();
        let (reasoning, answer) = if self.reasoning_hint.trim().is_empty() {
            split_think(&self.content)
        } else {
            (
                self.reasoning_hint.trim().to_string(),
                self.content.trim_start().to_string(),
            )
        };
        if let Some(s) = self.stage.take() {
            let meta = if self.first_token.is_none() && self.sent.is_some() {
                json!({"timings": self.timings})
            } else {
                Value::Null
            };
            self.tr.stage_end(&s, meta);
        }
        let res_end = res_end.unwrap_or_else(|| sample(&self.engine));
        let mut timings = if self.timings.is_object() {
            self.timings.clone()
        } else {
            json!({})
        };
        if let Some(o) = timings.as_object_mut() {
            o.insert("wall_ms".into(), json!(self.t0.elapsed().as_millis() as u64));
            if let (Some(s), Some(f)) = (self.sent, self.first_token) {
                o.insert("first_token_ms".into(), json!(f.duration_since(s).as_millis() as u64));
            }
            o.insert("finish_reason".into(), self.finish_reason.clone());
            if self.usage.is_object() {
                o.insert("usage".into(), self.usage.clone());
            }
        }
        self.tr.update(
            &self.id,
            json!({"answer": answer, "reasoning": reasoning, "timings": timings,
                   "resources": {"start": self.res_start, "end": res_end},
                   "adapters": adapters(&self.engine, &self.model, &self.request)}),
        );
        self.tr.finish(&self.id, status, error);
    }
}

impl Drop for Rec {
    fn drop(&mut self) {
        // the client went away (or a task was cancelled) before the answer ended
        self.finish("aborted", Some("the response was not read to its end"), None);
    }
}

struct Run {
    rec: Rec,
    /// What goes to llama-server.
    body: Value,
    /// What the client sees: None = no log probabilities, Some(n) = n alternatives.
    client_top: Option<usize>,
    stream: bool,
}

fn last_user_text(body: &Value) -> Option<String> {
    let msgs = body["messages"].as_array()?;
    let m = msgs.iter().rev().find(|m| m["role"] == "user")?;
    match &m["content"] {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => None,
    }
}

const PARAM_KEYS: &[&str] = &[
    "temperature",
    "top_p",
    "top_k",
    "min_p",
    "seed",
    "max_tokens",
    "max_completion_tokens",
    "presence_penalty",
    "frequency_penalty",
    "repeat_penalty",
    "stop",
    "lora",
    "tools",
    "response_format",
    "chat_template_kwargs",
];

/// Set a request up for tracing: resolve the model, open the trace, find the rendered prompt.
async fn begin(hub: &Hub, orig: &Value, parent: Option<&str>, meta: Value, scope: &Scope) -> Result<Run, Fail> {
    let untraceable = |m: &str| Fail::new(409, "untraceable", m.to_string());
    let name = orig["model"].as_str().unwrap_or("");
    let entry = match hub.engine.resolve(name)? {
        Target::Llama(e) => e,
        Target::Ollama(_) => return Err(untraceable("a model served by Ollama cannot be traced token by token")),
    };
    if !orig["messages"].is_array() {
        return Err(untraceable("only chat requests are traced"));
    }
    let tr = tracer(&hub.engine.home);
    tr.store().map_err(Fail::unavailable)?;
    let k = tr.settings().top_k as usize;
    let stream = orig["stream"].as_bool().unwrap_or(false);
    let client_top = orig["logprobs"]
        .as_bool()
        .unwrap_or(false)
        .then(|| orig["top_logprobs"].as_u64().unwrap_or(0) as usize);
    let mut body = orig.clone();
    body["logprobs"] = json!(true);
    body["top_logprobs"] = json!(k.max(client_top.unwrap_or(0)));
    body["model"] = json!(entry.id);

    let mut params = serde_json::Map::new();
    for key in PARAM_KEYS {
        if let Some(v) = orig.get(*key) {
            params.insert((*key).into(), v.clone());
        }
    }
    let mut meta = meta;
    meta["stream"] = json!(stream);
    let id = tr
        .start_in("chat", &entry.id, orig, parent, &meta, scope)
        .ok_or_else(|| Fail::unavailable("the trace could not be started"))?;
    let t0 = Instant::now();
    let engine = hub.engine.clone();
    let res_start = {
        let e = engine.clone();
        tokio::task::spawn_blocking(move || sample(&e))
            .await
            .unwrap_or(Value::Null)
    };
    let mut rec = Rec {
        tr: tr.clone(),
        engine: engine.clone(),
        id: id.clone(),
        model: entry.id.clone(),
        request: orig.clone(),
        t0,
        k,
        phase: Phase::default(),
        pending: vec![],
        flushed: Instant::now(),
        stage: None,
        reasoning_hint: String::new(),
        content: String::new(),
        timings: Value::Null,
        usage: Value::Null,
        finish_reason: Value::Null,
        first_token: None,
        sent: None,
        res_start,
        done: false,
    };
    tr.update(&id, json!({"params": params, "prompt": last_user_text(orig)}));
    rec.enter("prepare");
    // the chat template, applied by the model's own server (this also loads the model if it is not loaded yet)
    let mut tbody = json!({"model": entry.id, "messages": orig["messages"]});
    if let Some(t) = orig.get("tools") {
        tbody["tools"] = t.clone();
    }
    if let Some(t) = orig.get("chat_template_kwargs") {
        tbody["chat_template_kwargs"] = t.clone();
    }
    if let Ok((_, resp, _lease)) = hub.engine.forward("/apply-template", tbody).await {
        if resp.status().is_success() {
            if let Ok(v) = resp.json::<Value>().await {
                if let Some(p) = v["prompt"].as_str() {
                    tr.update(&id, json!({"rendered_prompt": p}));
                }
            }
        }
    }
    Ok(Run {
        rec,
        body,
        client_top,
        stream,
    })
}

/// A body that is not ours to change: what to send back to the client, with the upstream status.
pub struct Reply {
    pub status: u16,
    pub content_type: Option<axum::http::HeaderValue>,
    pub body: Bytes,
}

async fn send(hub: &Hub, run: &mut Run) -> Result<(reqwest::Response, Option<crate::engine::Lease>), Fail> {
    if run.stream {
        run.rec.enter("prompt_eval");
    } else if let Some(s) = run.rec.stage.take() {
        run.rec.tr.stage_end(&s, Value::Null);
    }
    run.rec.sent = Some(Instant::now());
    match hub.engine.forward("/v1/chat/completions", run.body.clone()).await {
        Ok((_, resp, lease)) => Ok((resp, lease)),
        Err(f) => {
            run.rec.finish("error", Some(&f.message), None);
            Err(f)
        }
    }
}

async fn read_whole(hub: &Hub, mut run: Run) -> Result<(Reply, String), Fail> {
    let t0 = run.rec.t0;
    let (resp, _lease) = send(hub, &mut run).await?;
    let status = resp.status().as_u16();
    let ctype = resp.headers().get("content-type").cloned();
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(x) => {
            let f = Fail::unavailable(x.to_string());
            run.rec.finish("error", Some(&f.message), None);
            return Err(f);
        }
    };
    let id = run.rec.id.clone();
    let ok = (200..300).contains(&status);
    let parsed: Option<Value> = serde_json::from_slice(&bytes).ok();
    hub.engine.record(
        &run.rec.model,
        ok,
        t0.elapsed().as_millis() as u64,
        parsed.as_ref().and_then(|v| v.get("usage")),
    );
    let out = match parsed {
        Some(mut v) if ok => {
            run.rec.feed_whole(&v);
            let e = hub.engine.clone();
            let res = tokio::task::spawn_blocking(move || sample(&e)).await.ok();
            // not streamed: the server timed the two halves, so show them as stages (the wall clock cannot)
            if let Some(sent) = run.rec.sent {
                let t = &run.rec.timings;
                let prompt_ms = t["prompt_ms"].as_f64().unwrap_or(0.0) as i64;
                let predicted_ms = t["predicted_ms"].as_f64().unwrap_or(0.0) as i64;
                let started = now_ms() - sent.elapsed().as_millis() as i64;
                let tr = run.rec.tr.clone();
                if prompt_ms > 0 && predicted_ms > 0 {
                    tr.stage_span(
                        &id,
                        "prompt_eval",
                        started,
                        started + prompt_ms,
                        json!({"derived": true}),
                    );
                    tr.stage_span(
                        &id,
                        "generation",
                        started + prompt_ms,
                        started + prompt_ms + predicted_ms,
                        json!({}),
                    );
                } else {
                    tr.stage_span(&id, "inference", started, now_ms(), json!({}));
                }
            }
            run.rec.finish("ok", None, res);
            if shape(&mut v, run.client_top) {
                Bytes::from(serde_json::to_vec(&v).unwrap_or_else(|_| bytes.to_vec()))
            } else {
                bytes
            }
        }
        _ => {
            let msg = String::from_utf8_lossy(&bytes).chars().take(300).collect::<String>();
            run.rec.finish("error", Some(&msg), None);
            bytes
        }
    };
    Ok((
        Reply {
            status,
            content_type: ctype,
            body: out,
        },
        id,
    ))
}

/// Splits a server-sent-event byte stream into its events.
#[derive(Default)]
struct Sse {
    buf: Vec<u8>,
}

impl Sse {
    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = vec![];
        while let Some(i) = self.buf.windows(2).position(|w| w == b"\n\n") {
            let ev: Vec<u8> = self.buf.drain(..i + 2).collect();
            out.push(String::from_utf8_lossy(&ev).into_owned());
        }
        out
    }
}

fn data_of(event: &str) -> Option<&str> {
    event.lines().find_map(|l| l.strip_prefix("data:")).map(str::trim)
}

fn as_response(status: StatusCode, ctype: Option<axum::http::HeaderValue>, body: Body) -> Response {
    let mut out = Response::new(body);
    *out.status_mut() = status;
    if let Some(c) = ctype {
        out.headers_mut().insert("content-type", c);
    }
    out
}

async fn read_stream(hub: &Hub, mut run: Run) -> Result<Response, Fail> {
    let t0 = run.rec.t0;
    let (resp, lease) = send(hub, &mut run).await?;
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let ctype = resp.headers().get("content-type").cloned();
    if !status.is_success() {
        let bytes = resp.bytes().await.unwrap_or_default();
        let msg = String::from_utf8_lossy(&bytes).chars().take(300).collect::<String>();
        run.rec.finish("error", Some(&msg), None);
        hub.engine
            .record(&run.rec.model, false, t0.elapsed().as_millis() as u64, None);
        return Ok(as_response(status, ctype, Body::from(bytes)));
    }
    hub.engine
        .record(&run.rec.model, true, t0.elapsed().as_millis() as u64, None);
    let engine = hub.engine.clone();
    let client_top = run.client_top;
    let k = run.rec.k;
    let mut up = resp.bytes_stream();
    let body = async_stream::stream! {
        let _lease = lease; // the model stays up until the last byte has gone out
        let mut rec = run.rec;
        let mut sse = Sse::default();
        let mut failed: Option<String> = None;
        while let Some(chunk) = up.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(x) => { failed = Some(x.to_string()); break; }
            };
            let untouched = client_top.is_some_and(|n| n >= k);
            for ev in sse.push(&chunk) {
                let mut out = ev.clone();
                if let Some(d) = data_of(&ev) {
                    if let Ok(mut v) = serde_json::from_str::<Value>(d) {
                        rec.feed_chunk(&v);
                        if !untouched && shape(&mut v, client_top) {
                            out = format!("data: {v}\n\n");
                        }
                    }
                }
                yield Ok::<_, std::io::Error>(Bytes::from(out));
            }
        }
        let res = { let e = engine.clone(); tokio::task::spawn_blocking(move || sample(&e)).await.ok() };
        match failed {
            None => rec.finish("ok", None, res),
            Some(m) => rec.finish("error", Some(&m), res),
        }
    };
    Ok(as_response(status, ctype, Body::from_stream(body)))
}

/// What the hub's chat route does with a traced request.
pub enum Outcome {
    Handled(Result<Response, Fail>),
    /// Not traced after all (an Ollama model, a store that will not open): serve it the ordinary way.
    Skip(Value),
}

pub async fn chat(hub: &Hub, body: Value, scope: Scope) -> Outcome {
    let run = match begin(hub, &body, None, json!({"source": "api"}), &scope).await {
        Ok(r) => r,
        Err(_) => return Outcome::Skip(body),
    };
    if run.stream {
        return Outcome::Handled(read_stream(hub, run).await);
    }
    Outcome::Handled(read_whole(hub, run).await.map(|(r, _)| {
        as_response(
            StatusCode::from_u16(r.status).unwrap_or(StatusCode::BAD_GATEWAY),
            r.content_type,
            Body::from(r.body),
        )
    }))
}

/// Run a request again as a traced, non-streaming call (the rerun and ablation hook). Returns the new trace's id
/// and the model's answer.
pub async fn run_traced(
    hub: &Hub,
    mut body: Value,
    parent: Option<&str>,
    meta: Value,
    scope: &Scope,
) -> Result<(String, Value), Fail> {
    body["stream"] = json!(false);
    let run = begin(hub, &body, parent, meta, scope).await?;
    let (reply, id) = read_whole(hub, run).await?;
    let v: Value = serde_json::from_slice(&reply.body).unwrap_or(Value::Null);
    if !(200..300).contains(&reply.status) {
        return Err(Fail::new(
            reply.status,
            "upstream",
            v["error"]["message"]
                .as_str()
                .unwrap_or("the model's server refused the request")
                .to_string(),
        ));
    }
    Ok((id, v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn think_tags_split_reasoning_from_the_answer() {
        assert_eq!(
            split_think("<think>\nlet me see\n</think>\n\nSeven."),
            ("let me see".into(), "Seven.".into())
        );
        // the template opened the tag in the prompt
        assert_eq!(split_think("hmm\n</think>Yes"), ("hmm".into(), "Yes".into()));
        assert_eq!(split_think("plain"), (String::new(), "plain".into()));
        assert_eq!(split_think("<think>cut off"), ("cut off".into(), String::new()));
    }

    #[test]
    fn phase_follows_the_tags() {
        let mut p = Phase::default();
        let seq: Vec<u8> = ["<think>", "a", "</think>", "b"].iter().map(|t| p.mark(t)).collect();
        assert_eq!(seq, [1, 1, 1, 0]);
    }

    #[test]
    fn the_client_sees_what_it_asked_for() {
        let lp = json!({"content": [{"token": "a", "logprob": -0.1, "top_logprobs": [{"token": "a"}, {"token": "b"}, {"token": "c"}]}]});
        let mut v = json!({"choices": [{"index": 0, "logprobs": lp}]});
        assert!(shape(&mut v, None));
        assert!(v["choices"][0].get("logprobs").is_none());
        let mut v = json!({"choices": [{"index": 0, "logprobs": lp}]});
        assert!(shape(&mut v, Some(1)));
        assert_eq!(
            v["choices"][0]["logprobs"]["content"][0]["top_logprobs"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let mut v = json!({"choices": [{"index": 0, "logprobs": lp}]});
        assert!(!shape(&mut v, Some(5)));
    }

    #[test]
    fn events_are_split_across_chunks() {
        let mut s = Sse::default();
        assert!(s.push(b"data: {\"a\":").is_empty());
        let evs = s.push(b"1}\n\ndata: [DONE]\n\n");
        assert_eq!(evs.len(), 2);
        assert_eq!(data_of(&evs[0]), Some("{\"a\":1}"));
        assert_eq!(data_of(&evs[1]), Some("[DONE]"));
    }

    #[test]
    fn tokens_carry_alternatives_and_entropy() {
        let c = json!([{"token": "Hi", "logprob": -0.1, "bytes": [72, 105],
                        "top_logprobs": [{"token": "Hi", "logprob": -0.69}, {"token": "Yo", "logprob": -0.69}]}]);
        let t = tokens_of(&c, 8, &mut Phase::default());
        assert_eq!(t[0].text, "Hi");
        assert_eq!(t[0].alts.len(), 2);
        assert!((t[0].entropy - 0.693).abs() < 0.01, "{}", t[0].entropy);
        assert_eq!(tokens_of(&c, 1, &mut Phase::default())[0].alts.len(), 1);
    }
}

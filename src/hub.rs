//! The hub: ModelMistress's one HTTP server. It is both the control API (ABP's module contract, v1) and an
//! OpenAI-compatible API, and the hub's token is also the OpenAI API key.
//!
//! It listens on 127.0.0.1 unless told otherwise (a random free port unless one is given), makes a random token
//! (or uses MM_TOKEN), and writes `<home>/control.json` = {url, token, pid, version, api}. Everything but health
//! needs `Authorization: Bearer <token>`.
//!
//!   GET  /v1/health                 open: {ok, pid, version, uptime_s}
//!   GET  /v1/operations             every operation, with a JSON Schema for its input
//!   POST /v1/call/{op}              run one: the body is its input; the answer is {"result": ...}
//!   POST /v1/service/stop           unload everything and stop
//!   GET  /v1/models                 OpenAI: the models on this machine (+ a running Ollama's, as ollama/<name>)
//!   POST /v1/chat/completions       OpenAI, streaming or not
//!   POST /v1/completions            OpenAI
//!   POST /v1/embeddings             OpenAI (the model must be loaded with embeddings: true)
//!   POST /v1/audio/transcriptions   OpenAI speech to text (multipart; WAV or MP3), whisper.cpp (feature `whisper`)
//!   POST /v1/audio/translations     the same, into English
//!
//! Errors are {"error": {"code", "message"}} with a 4xx/5xx status (OpenAI clients read `message`).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::engine::{Engine, Fail, LoadOpts, Settings, Target};
use crate::{audio, speech};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
pub struct Hub {
    pub engine: Arc<Engine>,
    pub jobs: Arc<crate::pull::Jobs>,
    pub token: Arc<String>,
    pub started: Instant,
    pub stop: Arc<tokio::sync::Notify>,
}

pub struct ApiError(Fail);

impl From<Fail> for ApiError {
    fn from(f: Fail) -> Self {
        Self(f)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.0.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (
            status,
            Json(json!({"error": {"code": self.0.code, "message": self.0.message, "type": self.0.code}})),
        )
            .into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

pub fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn auth(State(hub): State<Hub>, req: Request, next: Next) -> Response {
    let h = req.headers();
    let given = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| h.get("x-api-key").and_then(|v| v.to_str().ok()));
    if !given.is_some_and(|t| same(t.as_bytes(), hub.token.as_bytes())) {
        return ApiError(Fail::new(
            401,
            "unauthorized",
            "this needs the hub's token (it is also the API key)",
        ))
        .into_response();
    }
    next.run(req).await
}

// ---- operations ---------------------------------------------------------------------------------------------

fn op(id: &str, summary: &str, mutating: bool, destructive: bool, props: Value, required: &[&str]) -> Value {
    json!({"id": id, "group": id.split('.').next().unwrap_or(id), "summary": summary, "mutating": mutating,
           "destructive": destructive,
           "input_schema": {"type": "object", "properties": props, "required": required, "additionalProperties": false}})
}

pub fn operations() -> Vec<Value> {
    let model = json!({"type": "string", "description": "a model id from model.list, or ollama/<name>"});
    let load_props = json!({
        "model": model,
        "ctx_size": {"type": "integer", "minimum": 0, "description": "context tokens (0 = the model's own)"},
        "gpu_layers": {"type": "integer", "description": "layers on the GPU (999 = all, 0 = CPU only)"},
        "parallel": {"type": "integer", "minimum": 1, "description": "parallel request slots"},
        "threads": {"type": "integer", "minimum": 1},
        "embeddings": {"type": "boolean", "description": "serve /v1/embeddings instead of chat"},
        "extra_args": {"type": "array", "items": {"type": "string"}, "description": "more llama-server arguments"}
    });
    vec![
        op(
            "service.status",
            "Version, uptime, the engine, loaded models and counts",
            false,
            false,
            json!({}),
            &[],
        ),
        op(
            "backend.list",
            "The backends (llama.cpp, Ollama) and whether each is available",
            false,
            false,
            json!({}),
            &[],
        ),
        op(
            "model.list",
            "Every model on this machine that can be served",
            false,
            false,
            json!({
                "refresh": {"type": "boolean", "description": "rescan the disk first"},
                "source": {"type": "string", "enum": ["ollama", "huggingface", "folder"]},
                "include_ollama": {"type": "boolean", "description": "also list a running Ollama's models as ollama/<name>"}
            }),
            &[],
        ),
        op(
            "model.info",
            "One model: its file, size and GGUF metadata",
            false,
            false,
            json!({"model": model}),
            &["model"],
        ),
        op(
            "model.loaded",
            "The models loaded now, with their processes",
            false,
            false,
            json!({}),
            &[],
        ),
        op(
            "model.load",
            "Load a model (it stays loaded until unloaded, evicted or idle)",
            true,
            false,
            load_props,
            &["model"],
        ),
        op(
            "model.unload",
            "Unload a model, or every model with \"*\"",
            true,
            false,
            json!({"model": model}),
            &["model"],
        ),
        op(
            "model.pull",
            "Download a model: hf.co/<org>/<repo>[:<quant>] from Hugging Face, or <name>[:<tag>] from the Ollama \
             library. Runs as a job (job.get shows progress) unless wait is true",
            true,
            false,
            json!({
                "model": {"type": "string", "description": "hf.co/org/repo[:Q4_K_M], org/repo[:quant], or name[:tag]"},
                "source": {"type": "string", "enum": ["huggingface", "ollama"], "description": "when the name is ambiguous"},
                "file": {"type": "string", "description": "Hugging Face: this exact .gguf file instead of a quant"},
                "projector": {"type": "boolean", "description": "Hugging Face: also the vision projector (default true)"},
                "wait": {"type": "boolean", "description": "answer only when the download has finished"}
            }),
            &["model"],
        ),
        op(
            "model.search",
            "Search Hugging Face for GGUF models, most downloaded first",
            false,
            false,
            json!({"query": {"type": "string"}, "limit": {"type": "integer", "minimum": 1, "maximum": 100}}),
            &["query"],
        ),
        op(
            "model.files",
            "A Hugging Face repo's GGUF files, with sizes, quants and the name to pull each by",
            false,
            false,
            json!({"model": {"type": "string", "description": "hf.co/org/repo or org/repo"}}),
            &["model"],
        ),
        op(
            "model.delete",
            "Delete a model ModelMistress downloaded (models other programs own are refused)",
            true,
            true,
            json!({"model": model}),
            &["model"],
        ),
        op(
            "job.list",
            "Downloads: running and finished",
            false,
            false,
            json!({}),
            &[],
        ),
        op(
            "job.get",
            "One download's progress",
            false,
            false,
            json!({"job": {"type": "string"}}),
            &["job"],
        ),
        op(
            "job.cancel",
            "Stop a download (what is downloaded so far is kept, and a new pull resumes it)",
            true,
            false,
            json!({"job": {"type": "string"}}),
            &["job"],
        ),
        op(
            "chat.complete",
            "Chat with a model and wait for the whole answer",
            false,
            false,
            json!({
                "model": model,
                "prompt": {"type": "string", "description": "one user message (or give messages)"},
                "system": {"type": "string"},
                "messages": {"type": "array", "items": {"type": "object"}},
                "max_tokens": {"type": "integer", "minimum": 1},
                "temperature": {"type": "number", "minimum": 0}
            }),
            &[],
        ),
        op(
            "bench.run",
            "Measure prompt and generation speed with llama-bench",
            false,
            false,
            json!({
                "model": model,
                "prompt_tokens": {"type": "integer", "minimum": 0},
                "gen_tokens": {"type": "integer", "minimum": 0},
                "gpu_layers": {"type": "integer"}
            }),
            &["model"],
        ),
        op(
            "metrics.get",
            "Requests, errors, tokens and time, per model",
            false,
            false,
            json!({}),
            &[],
        ),
        op(
            "logs.tail",
            "The last lines of a loaded model's llama-server log",
            false,
            false,
            json!({
                "model": model, "lines": {"type": "integer", "minimum": 1, "maximum": 2000}
            }),
            &["model"],
        ),
        op(
            "config.get",
            "The settings (<home>/config.toml)",
            false,
            false,
            json!({}),
            &[],
        ),
        op(
            "speech.transcribe",
            "Speech to text with the built-in whisper.cpp: transcribe (or translate to English) an audio file (WAV or MP3) on the hub's machine",
            false,
            false,
            json!({
                "file": {"type": "string", "description": "path of the audio file"},
                "model": {"type": "string", "description": "whisper model, e.g. base.en (default: whisper_model setting)"},
                "language": {"type": "string", "description": "ISO-639-1 code; default: detected"},
                "prompt": {"type": "string", "description": "text that guides the style or spelling"},
                "translate": {"type": "boolean", "description": "translate to English instead of transcribing"},
                "format": {"type": "string", "description": "json, text, verbose_json, srt or vtt; the answer's `rendered` is in it"}
            }),
            &["file"],
        ),
        op(
            "config.set",
            "Change settings; only the fields given change, and they are saved",
            true,
            false,
            json!({
                "settings": {"type": "object", "description": "fields of config.get's answer"}
            }),
            &["settings"],
        ),
    ]
}

fn s<'a>(v: &'a Value, k: &str) -> ApiResult<&'a str> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| Fail::invalid(format!("{k} is required")).into())
}

fn llama_entry(hub: &Hub, name: &str) -> ApiResult<crate::catalog::ModelEntry> {
    match hub.engine.resolve(name)? {
        Target::Llama(e) => Ok(e),
        Target::Ollama(_) => Err(Fail::invalid("Ollama's models are managed by Ollama").into()),
    }
}

pub async fn call(hub: &Hub, id: &str, a: &Value) -> ApiResult<Value> {
    let e = &hub.engine;
    Ok(match id {
        "service.status" => {
            let loaded: Vec<Value> = e.loaded().iter().map(|i| i.describe()).collect();
            json!({"version": VERSION, "pid": std::process::id(), "uptime_s": hub.started.elapsed().as_secs(),
                   "home": e.home, "llama_server": e.llama_server(), "llama_version": e.llama_version().await,
                   "llama_built_in": crate::engine::bundled::version(),
                   "models_known": e.catalog().len(), "loaded": loaded})
        }
        "backend.list" => {
            let st = e.settings();
            let ollama = e.ollama_models().await;
            let running = !st.ollama_url.is_empty()
                && e.http
                    .get(format!("{}/api/version", st.ollama_url.trim_end_matches('/')))
                    .timeout(Duration::from_secs(3))
                    .send()
                    .await
                    .is_ok();
            json!([
                {"id": "llama.cpp", "available": e.llama_server().is_some(), "path": e.llama_server(),
                 "version": e.llama_version().await, "built_in": crate::engine::bundled::version(),
                 "models": e.catalog().len(), "loaded": e.loaded().len()},
                {"id": "ollama", "available": running, "url": st.ollama_url, "models": ollama.len(),
                 "note": "models served as ollama/<name>, passed through to Ollama"}
            ])
        }
        "model.list" => {
            if a["refresh"].as_bool().unwrap_or(false) {
                let roots = e.clone();
                tokio::task::spawn_blocking(move || roots.refresh())
                    .await
                    .map_err(|x| Fail::new(500, "io", x.to_string()))?;
            }
            let src = a["source"].as_str();
            let mut out: Vec<Value> = e
                .catalog()
                .iter()
                .filter(|m| src.is_none_or(|s| m.source == s))
                .map(|m| m.summary(e.is_loaded(&m.id)))
                .collect();
            if a["include_ollama"].as_bool().unwrap_or(false) {
                for m in e.ollama_models().await {
                    out.push(json!({"id": format!("ollama/{}", m["name"].as_str().unwrap_or("")), "source": "ollama-daemon",
                                    "size_gb": m["size"].as_f64().map(|b| (b / 1e7).round() / 100.0), "loaded": Value::Null}));
                }
            }
            json!(out)
        }
        "model.info" => {
            let m = llama_entry(hub, s(a, "model")?)?;
            json!({"id": m.id, "source": m.source, "path": m.path, "mmproj": m.mmproj, "size_bytes": m.size_bytes,
                   "summary": m.summary(e.is_loaded(&m.id)), "metadata": m.meta})
        }
        "model.loaded" => json!(e.loaded().iter().map(|i| i.describe()).collect::<Vec<_>>()),
        "model.load" => {
            let m = llama_entry(hub, s(a, "model")?)?;
            let mut args = a.clone();
            if let Some(o) = args.as_object_mut() {
                o.remove("model");
            }
            let opts: LoadOpts = serde_json::from_value(args).map_err(|x| Fail::invalid(x.to_string()))?;
            let t0 = Instant::now();
            let inst = e.ensure_loaded(&m, &opts, true).await?;
            json!({"loaded": inst.describe(), "load_ms": t0.elapsed().as_millis() as u64})
        }
        "model.unload" => {
            let name = s(a, "model")?;
            if name == "*" {
                json!({"unloaded": e.unload_all()})
            } else {
                let m = llama_entry(hub, name)?;
                json!({"unloaded": e.unload(&m.id) as u32, "model": m.id})
            }
        }
        "chat.complete" => {
            let messages = match (a.get("messages"), a.get("prompt").and_then(Value::as_str)) {
                (Some(Value::Array(ms)), _) => ms.clone(),
                (_, Some(p)) => {
                    let mut v = vec![];
                    if let Some(sys) = a.get("system").and_then(Value::as_str) {
                        v.push(json!({"role": "system", "content": sys}));
                    }
                    v.push(json!({"role": "user", "content": p}));
                    v
                }
                _ => return Err(Fail::invalid("give prompt or messages").into()),
            };
            let body = json!({"model": a.get("model").cloned().unwrap_or(json!("")), "messages": messages,
                              "max_tokens": a.get("max_tokens").cloned().unwrap_or(json!(512)),
                              "temperature": a.get("temperature").cloned().unwrap_or(json!(0.7)), "stream": false});
            let t0 = Instant::now();
            let (model, resp) = e.forward("/v1/chat/completions", body).await?;
            let status = resp.status();
            let v: Value = resp.json().await.unwrap_or(Value::Null);
            let ms = t0.elapsed().as_millis() as u64;
            e.record(&model, status.is_success(), ms, v.get("usage"));
            if !status.is_success() {
                return Err(Fail::new(
                    502,
                    "backend_error",
                    format!("{model}: {}", v["error"]["message"].as_str().unwrap_or(&v.to_string())),
                )
                .into());
            }
            let msg = &v["choices"][0]["message"];
            let completion = v["usage"]["completion_tokens"].as_f64().unwrap_or(0.0);
            json!({"model": model, "text": msg["content"], "reasoning": msg.get("reasoning_content"),
                   "finish_reason": v["choices"][0]["finish_reason"], "usage": v["usage"], "ms": ms,
                   "tokens_per_s": v["timings"]["predicted_per_second"].as_f64()
                       .or_else(|| (ms > 0).then(|| completion * 1000.0 / ms as f64))})
        }
        "bench.run" => {
            let m = llama_entry(hub, s(a, "model")?)?;
            let p = a["prompt_tokens"].as_u64().unwrap_or(512) as u32;
            let g = a["gen_tokens"].as_u64().unwrap_or(128) as u32;
            let ngl = a["gpu_layers"].as_i64().unwrap_or(e.settings().gpu_layers as i64) as i32;
            e.bench(&m, p, g, ngl).await?
        }
        "metrics.get" => json!(*e.metrics.lock()),
        "logs.tail" => {
            let m = llama_entry(hub, s(a, "model")?)?;
            let n = a["lines"].as_u64().unwrap_or(50).min(2000) as usize;
            let inst = e
                .loaded()
                .into_iter()
                .find(|i| i.model == m.id)
                .ok_or_else(|| Fail::new(409, "not_loaded", format!("{} is not loaded", m.id)))?;
            json!({"model": m.id, "log": inst.log, "text": crate::engine::tail(&inst.log, n)})
        }
        "model.pull" => {
            let spec = s(a, "model")?;
            let plan = match crate::pull::source_of(spec, a["source"].as_str())? {
                "ollama" => crate::pull::plan_ollama(e, spec).await?,
                _ => {
                    crate::pull::plan_hf(e, spec, a["file"].as_str(), a["projector"].as_bool().unwrap_or(true)).await?
                }
            };
            let job = hub.jobs.start(e.clone(), plan);
            if a["wait"].as_bool().unwrap_or(false) {
                while job.status() == "running" {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
            job.describe()
        }
        "model.search" => crate::pull::search_hf(e, s(a, "query")?, a["limit"].as_u64().unwrap_or(20) as u32).await?,
        "model.files" => crate::pull::files_hf(e, s(a, "model")?).await?,
        "model.delete" => {
            let id = llama_entry(hub, s(a, "model")?)?.id;
            let engine = e.clone();
            tokio::task::spawn_blocking(move || crate::pull::delete(&engine, &id))
                .await
                .map_err(|x| Fail::new(500, "io", x.to_string()))??
        }
        "job.list" => json!(hub.jobs.list()),
        "job.get" => {
            let id = s(a, "job")?;
            hub.jobs
                .get(id)
                .ok_or_else(|| Fail::not_found(format!("no job {id}")))?
                .describe()
        }
        "job.cancel" => {
            let id = s(a, "job")?;
            json!({"job": id, "cancelled": hub.jobs.cancel(id)})
        }
        "speech.transcribe" => {
            let path = s(a, "file")?;
            let file = std::fs::read(path).map_err(|x| Fail::invalid(format!("{path}: {x}")))?;
            let opt = |k: &str| {
                a.get(k)
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                    .map(String::from)
            };
            let req = audio::Request {
                file,
                filename: path.to_string(),
                model: opt("model").unwrap_or_default(),
                language: opt("language").map(|l| l.to_lowercase()),
                prompt: opt("prompt"),
                format: audio::ResponseFormat::parse(&opt("format").unwrap_or_else(|| "json".into()))
                    .map_err(Fail::invalid)?,
                temperature: 0.0,
            };
            let format = req.format;
            let translate = a.get("translate").and_then(Value::as_bool).unwrap_or(false);
            let out = run_speech(hub, req, translate).await?;
            let mut v = out.verbose();
            v["rendered"] = json!(out.render(format));
            v
        }
        "config.get" => shown_settings(&e.settings()),
        "config.set" => {
            let patch = a
                .get("settings")
                .and_then(Value::as_object)
                .ok_or_else(|| Fail::invalid("settings must be an object"))?;
            let mut cur = serde_json::to_value(e.settings()).map_err(|x| Fail::new(500, "io", x.to_string()))?;
            for (k, v) in patch {
                if k == "hf_token" && v == "(set)" {
                    continue; // config.get's placeholder, sent back unchanged
                }
                if cur.get(k).is_none() {
                    return Err(Fail::invalid(format!("no setting {k}")).into());
                }
                cur[k] = v.clone();
            }
            let new: Settings = serde_json::from_value(cur).map_err(|x| Fail::invalid(x.to_string()))?;
            let engine = e.clone();
            let saved = new.clone();
            tokio::task::spawn_blocking(move || engine.set_settings(saved))
                .await
                .map_err(|x| Fail::new(500, "io", x.to_string()))??;
            shown_settings(&new)
        }
        _ => return Err(Fail::not_found(format!("no operation {id}")).into()),
    })
}

/// The settings as config.get shows them: a secret only says whether it is set.
fn shown_settings(st: &Settings) -> Value {
    let mut v = json!(st);
    if st.hf_token.as_deref().is_some_and(|t| !t.is_empty()) {
        v["hf_token"] = json!("(set)");
    }
    v
}

// ---- routes -------------------------------------------------------------------------------------------------

async fn health(State(hub): State<Hub>) -> Json<Value> {
    Json(
        json!({"ok": true, "pid": std::process::id(), "version": VERSION, "uptime_s": hub.started.elapsed().as_secs()}),
    )
}

async fn list_ops() -> Json<Value> {
    Json(json!({"api": 1, "operations": operations()}))
}

async fn call_op(State(hub): State<Hub>, Path(id): Path<String>, body: Option<Json<Value>>) -> ApiResult<Json<Value>> {
    let args = body.map(|Json(v)| v).unwrap_or(json!({}));
    if !args.is_object() {
        return Err(Fail::invalid("the body must be a JSON object").into());
    }
    Ok(Json(json!({"result": call(&hub, &id, &args).await?})))
}

async fn stop(State(hub): State<Hub>) -> Json<Value> {
    hub.stop.notify_one();
    Json(json!({"result": {"stopping": true}}))
}

async fn openai_models(State(hub): State<Hub>) -> Json<Value> {
    let e = &hub.engine;
    let mut data: Vec<Value> = e
        .catalog()
        .iter()
        .map(|m| {
            json!({"id": m.id, "object": "model", "created": 0, "owned_by": m.source,
                         "meta": m.summary(e.is_loaded(&m.id))})
        })
        .collect();
    for m in e.ollama_models().await {
        data.push(
            json!({"id": format!("ollama/{}", m["name"].as_str().unwrap_or("")), "object": "model",
                         "created": 0, "owned_by": "ollama"}),
        );
    }
    Json(json!({"object": "list", "data": data}))
}

async fn passthrough(hub: Hub, path: &'static str, body: Value) -> ApiResult<Response> {
    let streaming = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let t0 = Instant::now();
    let (model, resp) = hub.engine.forward(path, body).await?;
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let ctype = resp.headers().get("content-type").cloned();
    let mut out = if streaming && status.is_success() {
        hub.engine.record(&model, true, t0.elapsed().as_millis() as u64, None);
        Response::new(Body::from_stream(resp.bytes_stream()))
    } else {
        let bytes = resp.bytes().await.map_err(|x| Fail::unavailable(x.to_string()))?;
        let usage = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v.get("usage").cloned());
        hub.engine.record(
            &model,
            status.is_success(),
            t0.elapsed().as_millis() as u64,
            usage.as_ref(),
        );
        Response::new(Body::from(bytes))
    };
    *out.status_mut() = status;
    if let Some(c) = ctype {
        out.headers_mut().insert("content-type", c);
    }
    Ok(out)
}

async fn chat(State(hub): State<Hub>, Json(body): Json<Value>) -> ApiResult<Response> {
    passthrough(hub, "/v1/chat/completions", body).await
}

async fn completions(State(hub): State<Hub>, Json(body): Json<Value>) -> ApiResult<Response> {
    passthrough(hub, "/v1/completions", body).await
}

async fn embeddings(State(hub): State<Hub>, Json(body): Json<Value>) -> ApiResult<Response> {
    passthrough(hub, "/v1/embeddings", body).await
}

/// The largest audio upload (OpenAI's own limit is 25 MB; local files are often longer).
const MAX_AUDIO_BYTES: usize = 512 << 20;

/// `POST /v1/audio/transcriptions` and `/v1/audio/translations`: multipart in, the transcript out.
async fn speech_to_text(hub: Hub, headers: HeaderMap, body: Bytes, translate: bool) -> ApiResult<Response> {
    if !speech::AVAILABLE {
        return Err(Fail::new(501, "not_implemented", speech::NOT_BUILT).into());
    }
    let ctype = headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("");
    let req = audio::parse_request(ctype, &body).map_err(Fail::invalid)?;
    let format = req.format;
    let out = run_speech(&hub, req, translate).await?;
    Ok(([("content-type", format.content_type())], out.render(format)).into_response())
}

/// Find the model, decode the audio and transcribe it (off the async threads: it takes a while).
async fn run_speech(hub: &Hub, req: audio::Request, translate: bool) -> ApiResult<audio::Transcript> {
    if !speech::AVAILABLE {
        return Err(Fail::new(501, "not_implemented", speech::NOT_BUILT).into());
    }
    let (dirs, preferred) = (hub.engine.whisper_dirs(), hub.engine.settings().whisper_model);
    let model = speech::find_model(&dirs, &req.model, preferred.as_deref())?;
    let name = speech::model_name(&model).unwrap_or_else(|| "whisper".into());
    let t0 = Instant::now();
    let out = tokio::task::spawn_blocking(move || -> Result<audio::Transcript, Fail> {
        let samples = audio::decode(&req.file, &req.filename).map_err(Fail::invalid)?;
        speech::transcribe(&model, &samples, &req, translate)
    })
    .await
    .map_err(|x| Fail::new(500, "inference_error", x.to_string()))?;
    hub.engine
        .record(&name, out.is_ok(), t0.elapsed().as_millis() as u64, None);
    Ok(out?)
}

async fn transcriptions(State(hub): State<Hub>, headers: HeaderMap, body: Bytes) -> ApiResult<Response> {
    speech_to_text(hub, headers, body, false).await
}

async fn translations(State(hub): State<Hub>, headers: HeaderMap, body: Bytes) -> ApiResult<Response> {
    speech_to_text(hub, headers, body, true).await
}

pub fn router(hub: Hub) -> Router {
    let private = Router::new()
        .route("/v1/operations", get(list_ops))
        .route("/v1/call/:op", post(call_op))
        .route("/v1/service/stop", post(stop))
        .route("/v1/models", get(openai_models))
        .route("/v1/chat/completions", post(chat))
        .route("/v1/completions", post(completions))
        .route("/v1/embeddings", post(embeddings))
        .route(
            "/v1/audio/transcriptions",
            post(transcriptions).layer(DefaultBodyLimit::max(MAX_AUDIO_BYTES)),
        )
        .route(
            "/v1/audio/translations",
            post(translations).layer(DefaultBodyLimit::max(MAX_AUDIO_BYTES)),
        )
        .route_layer(middleware::from_fn_with_state(hub.clone(), auth));
    Router::new()
        .route("/v1/health", get(health))
        .merge(private)
        .with_state(hub)
}

pub struct ServeOpts {
    pub home: PathBuf,
    pub host: std::net::IpAddr,
    pub port: u16,
}

pub async fn serve(hub: Hub, o: ServeOpts) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(SocketAddr::new(o.host, o.port)).await?;
    let addr = listener.local_addr()?;
    let shown = if o.host.is_unspecified() {
        SocketAddr::from(([127, 0, 0, 1], addr.port()))
    } else {
        addr
    };
    let control = o.home.join("control.json");
    let doc = json!({"url": format!("http://{shown}"), "token": hub.token.as_str(), "pid": std::process::id(),
                     "version": VERSION, "api": 1});
    let tmp = control.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&doc).unwrap_or_default())?;
    std::fs::rename(&tmp, &control)?;
    eprintln!(
        "model-mistress {VERSION}: listening on http://{addr} (control file {})",
        control.display()
    );
    if hub.engine.llama_server().is_none() {
        eprintln!("model-mistress: llama-server was not found; set llama_server with config.set");
    }
    let stop = hub.stop.clone();
    let engine = hub.engine.clone();
    let housekeeping = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        loop {
            tick.tick().await;
            engine.housekeeping();
        }
    });
    let engine = hub.engine.clone();
    let result = axum::serve(listener, router(hub))
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = stop.notified() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        })
        .await;
    housekeeping.abort();
    let n = engine.unload_all();
    let _ = std::fs::remove_file(&control);
    eprintln!("model-mistress stopped ({n} model(s) unloaded)");
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    const TOKEN: &str = "test-token-0123456789";

    fn hub(home: &std::path::Path) -> Hub {
        Hub {
            engine: Engine::new(home.to_path_buf(), Settings::default()),
            jobs: Arc::new(Default::default()),
            token: Arc::new(TOKEN.into()),
            started: Instant::now(),
            stop: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn wav(samples: &[i16]) -> Vec<u8> {
        let mut v = b"RIFF".to_vec();
        v.extend((36 + samples.len() as u32 * 2).to_le_bytes());
        v.extend(b"WAVEfmt ");
        v.extend([16, 0, 0, 0, 1, 0, 1, 0]);
        v.extend(16_000u32.to_le_bytes());
        v.extend(32_000u32.to_le_bytes());
        v.extend([2, 0, 16, 0]);
        v.extend(b"data");
        v.extend((samples.len() as u32 * 2).to_le_bytes());
        samples.iter().for_each(|s| v.extend(s.to_le_bytes()));
        v
    }

    fn form(fields: &[(&str, Option<&str>, &[u8])]) -> Vec<u8> {
        let mut b = Vec::new();
        for (name, file, data) in fields {
            b.extend(b"--B\r\n");
            match file {
                Some(f) => b.extend(
                    format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\n\r\n").as_bytes(),
                ),
                None => b.extend(format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes()),
            }
            b.extend(*data);
            b.extend(b"\r\n");
        }
        b.extend(b"--B--\r\n");
        b
    }

    async fn post(hub: &Hub, path: &str, ctype: &str, body: Vec<u8>) -> (StatusCode, String, String) {
        let req = axum::http::Request::post(path)
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("content-type", ctype)
            .body(Body::from(body))
            .unwrap();
        let resp = router(hub.clone()).oneshot(req).await.unwrap();
        let status = resp.status();
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 24).await.unwrap();
        (status, ct, String::from_utf8_lossy(&bytes).to_string())
    }

    const MP: &str = "multipart/form-data; boundary=B";

    fn temp_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!("mm-hub-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[tokio::test]
    async fn audio_needs_the_token() {
        let home = temp_home();
        let h = hub(&home);
        let req = axum::http::Request::post("/v1/audio/transcriptions")
            .body(Body::empty())
            .unwrap();
        assert_eq!(router(h).oneshot(req).await.unwrap().status(), 401);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(not(feature = "whisper"))]
    #[tokio::test]
    async fn without_the_feature_speech_is_501() {
        let home = temp_home();
        let h = hub(&home);
        let body = form(&[("file", Some("a.wav"), &wav(&[0; 3200])), ("model", None, b"base.en")]);
        for path in ["/v1/audio/transcriptions", "/v1/audio/translations"] {
            let (st, _, text) = post(&h, path, MP, body.clone()).await;
            assert_eq!(st, 501, "{text}");
            assert!(text.contains("--features whisper"));
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(feature = "whisper")]
    #[tokio::test]
    async fn bad_speech_requests_get_clear_errors() {
        let home = temp_home();
        let h = hub(&home);
        let t = "/v1/audio/transcriptions";
        let (st, _, text) = post(&h, t, "application/json", b"{}".to_vec()).await;
        assert_eq!((st.as_u16(), text.contains("multipart")), (400, true), "{text}");
        let (st, _, text) = post(&h, t, MP, form(&[("model", None, b"base.en")])).await;
        assert_eq!((st.as_u16(), text.contains("file")), (400, true), "{text}");
        let bad_format = form(&[("file", Some("a.wav"), b"x"), ("response_format", None, b"xml")]);
        let (st, _, text) = post(&h, t, MP, bad_format).await;
        assert_eq!((st.as_u16(), text.contains("response_format")), (400, true), "{text}");
        // the model is looked up before the audio is read: unknown model, 404
        let ok_audio = wav(&[0; 3200]);
        let unknown = form(&[("file", Some("a.wav"), &ok_audio), ("model", None, b"nope")]);
        let (st, _, text) = post(&h, t, MP, unknown).await;
        assert_eq!((st.as_u16(), text.contains("nope")), (404, true), "{text}");
        let none = form(&[("file", Some("a.wav"), &ok_audio)]);
        let (st, _, _) = post(&h, "/v1/audio/translations", MP, none).await;
        assert_eq!(st, 404, "no whisper model in an empty home");
        // a model file is there, but the audio is not WAV or MP3: 400 before the model is ever loaded
        std::fs::create_dir_all(home.join("whisper")).unwrap();
        std::fs::write(home.join("whisper").join("ggml-fake.bin"), b"not a model").unwrap();
        let ogg = form(&[("file", Some("a.ogg"), b"OggS....."), ("model", None, b"fake")]);
        let (st, _, text) = post(&h, t, MP, ogg).await;
        assert_eq!((st.as_u16(), text.contains("Ogg")), (400, true), "{text}");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Needs a real whisper model and a short recording; see docs/speech.md. Run:
    ///   MM_TEST_WHISPER_MODEL=<ggml-base.en.bin> MM_TEST_WHISPER_WAV=<speech.wav> MM_TEST_WHISPER_EXPECT=<a word said>
    ///   cargo test --features whisper -- --ignored transcribes_real_speech
    #[cfg(feature = "whisper")]
    #[tokio::test]
    #[ignore = "needs a whisper model and a WAV file (MM_TEST_WHISPER_MODEL, MM_TEST_WHISPER_WAV, MM_TEST_WHISPER_EXPECT)"]
    async fn transcribes_real_speech() {
        let model = std::env::var("MM_TEST_WHISPER_MODEL").expect("MM_TEST_WHISPER_MODEL");
        let wav_path = std::env::var("MM_TEST_WHISPER_WAV").expect("MM_TEST_WHISPER_WAV");
        let expect = std::env::var("MM_TEST_WHISPER_EXPECT")
            .unwrap_or_default()
            .to_lowercase();
        let home = temp_home();
        let h = hub(&home);
        let audio = std::fs::read(&wav_path).unwrap();
        let fields = |fmt: &'static str| {
            form(&[
                ("file", Some("speech.wav"), &audio),
                ("model", None, model.as_bytes()),
                ("response_format", None, fmt.as_bytes()),
            ])
        };
        let (st, ct, text) = post(&h, "/v1/audio/transcriptions", MP, fields("json")).await;
        assert_eq!(st, 200, "{text}");
        assert_eq!(ct, "application/json");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert!(v["text"].as_str().unwrap().to_lowercase().contains(&expect), "{text}");
        let (st, _, text) = post(&h, "/v1/audio/transcriptions", MP, fields("verbose_json")).await;
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!((st.as_u16(), v["language"].as_str()), (200, Some("english")), "{text}");
        assert!(v["duration"].as_f64().unwrap() > 0.5 && !v["segments"].as_array().unwrap().is_empty());
        let (_, ct, text) = post(&h, "/v1/audio/transcriptions", MP, fields("srt")).await;
        assert!(
            text.starts_with("1\n00:00:00,") && ct.starts_with("text/plain"),
            "{text}"
        );
        let (_, _, text) = post(&h, "/v1/audio/transcriptions", MP, fields("vtt")).await;
        assert!(text.starts_with("WEBVTT"), "{text}");
        // an English-only model cannot translate
        let (st, _, text) = post(&h, "/v1/audio/translations", MP, fields("json")).await;
        assert!(st == 200 || (st == 400 && text.contains("English only")), "{st} {text}");
        let _ = std::fs::remove_dir_all(&home);
    }
}

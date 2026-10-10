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
//!   POST /v1/embeddings             OpenAI (a model loaded for it: autoloaded, or load with embeddings: true)
//!   POST /v1/rerank, /rerank        Jina/llama-server style reranking (a reranker model; reranking: true)
//!   POST /v1/audio/transcriptions   OpenAI speech to text (multipart; WAV or MP3), whisper.cpp (feature `whisper`)
//!   POST /v1/audio/translations     the same, into English
//!   POST /v1/audio/speech           OpenAI text to speech (JSON in, WAV or raw PCM out), llama.cpp's llama-tts
//!   POST /v1/images/generations     OpenAI image generation (PNG as b64_json or a url), stable-diffusion.cpp's sd-cli
//!   GET  /v1/images/files/{name}    a generated image that a url points to
//!   POST /v1/images/edits           answers 501 (see docs/images.md)
//!   POST /v1/vision/classify        image recognition through ONNX Runtime (feature `vision`; see docs/vision.md)
//!   POST /v1/vision/detect          the same: objects with boxes
//!   POST /v1/vision/embed           the same: an embedding vector of the picture
//!
//! A peer key (see `hosting`) may use only the OpenAI routes above; the owner's token can do everything.
//!
//! Errors are {"error": {"code", "message"}} with a 4xx/5xx status (OpenAI clients read `message`).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Extension, Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde_json::{json, Value};

use crate::engine::{Engine, Fail, Lease, LoadOpts, Settings, Target};
use crate::hosting::{Keys, Permit, Quotas, Verdict};
use crate::{audio, image, speech, tools, tts, vision};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
pub struct Hub {
    pub engine: Arc<Engine>,
    pub jobs: Arc<crate::pull::Jobs>,
    pub token: Arc<String>,
    pub started: Instant,
    pub stop: Arc<tokio::sync::Notify>,
    /// The peer keys (see `hosting`).
    pub keys: Arc<Keys>,
}

impl Hub {
    pub fn new(engine: Arc<Engine>, token: String) -> Self {
        let keys = Keys::open(&engine.home);
        Self {
            engine,
            jobs: Arc::new(Default::default()),
            token: Arc::new(token),
            started: Instant::now(),
            stop: Arc::new(tokio::sync::Notify::new()),
            keys,
        }
    }
}

/// Who is calling: the owner (None) or a peer with a key.
#[derive(Clone)]
pub struct Caller(pub Option<Arc<Permit>>);

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

/// The routes a peer key may use.
fn peer_route(path: &str) -> bool {
    matches!(
        path,
        "/v1/models"
            | "/v1/chat/completions"
            | "/v1/completions"
            | "/v1/embeddings"
            | "/v1/rerank"
            | "/rerank"
            | "/v1/audio/speech"
            | "/v1/audio/transcriptions"
            | "/v1/audio/translations"
            | "/v1/images/generations"
            | "/v1/images/edits"
            | "/v1/vision/classify"
            | "/v1/vision/detect"
            | "/v1/vision/embed"
    ) || path.starts_with("/v1/images/files/")
}

/// The owner's token is accepted from this machine only. (No address known, as in tests that call the router
/// directly: this machine.)
pub fn owner_allowed(peer: Option<SocketAddr>) -> bool {
    peer.is_none_or(|a| a.ip().to_canonical().is_loopback())
}

async fn auth(State(hub): State<Hub>, mut req: Request, next: Next) -> Response {
    let from = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0);
    let h = req.headers();
    let given = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| h.get("x-api-key").and_then(|v| v.to_str().ok()))
        .map(str::to_string);
    let unauthorized = || {
        ApiError(Fail::new(
            401,
            "unauthorized",
            "this needs the hub's token or a peer key (either is also the API key)",
        ))
        .into_response()
    };
    let Some(given) = given else { return unauthorized() };
    if same(given.as_bytes(), hub.token.as_bytes()) {
        if !owner_allowed(from) {
            return ApiError(Fail::new(
                403,
                "owner_token_local_only",
                "the hub's own token is accepted from this machine only; use a peer key (model-mistress keys create)",
            ))
            .into_response();
        }
        req.extensions_mut().insert(Caller(None));
        return next.run(req).await;
    }
    if !hub.keys.knows(&given) {
        return unauthorized();
    }
    if !peer_route(req.uri().path()) {
        return ApiError(Fail::new(
            403,
            "forbidden",
            "a peer key can use the model routes only (/v1/models, chat, completions, embeddings, rerank, audio)",
        ))
        .into_response();
    }
    match hub.keys.check(&given) {
        Verdict::Admitted(permit) => {
            req.extensions_mut().insert(Caller(Some(permit)));
            next.run(req).await
        }
        Verdict::Revoked => ApiError(Fail::new(401, "key_revoked", "this key was revoked")).into_response(),
        Verdict::Unknown => unauthorized(),
        Verdict::Rejected {
            retry_after,
            code,
            message,
        } => (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", retry_after.to_string())],
            Json(json!({"error": {"code": code, "message": message, "type": code, "retry_after_s": retry_after}})),
        )
            .into_response(),
    }
}

// ---- operations ---------------------------------------------------------------------------------------------

fn op(id: &str, summary: &str, mutating: bool, destructive: bool, props: Value, required: &[&str]) -> Value {
    json!({"id": id, "group": id.split('.').next().unwrap_or(id), "summary": summary, "mutating": mutating,
           "destructive": destructive,
           "input_schema": {"type": "object", "properties": props, "required": required, "additionalProperties": false}})
}

/// Every operation: the hub's own, then the Knowledge Module ones (`km.*`).
pub fn operations() -> Vec<Value> {
    let mut all = core_operations();
    all.extend(crate::km::ops::operations());
    all
}

fn core_operations() -> Vec<Value> {
    let model = json!({"type": "string", "description": "a model id from model.list, or ollama/<name>"});
    let load_props = json!({
        "model": model,
        "ctx_size": {"type": "integer", "minimum": 0, "description": "context tokens (0 = the model's own)"},
        "gpu_layers": {"type": "integer", "description": "layers on the GPU (999 = all, 0 = CPU only)"},
        "parallel": {"type": "integer", "minimum": 1, "description": "parallel request slots"},
        "threads": {"type": "integer", "minimum": 1},
        "embeddings": {"type": "boolean", "description": "serve /v1/embeddings instead of chat"},
        "reranking": {"type": "boolean", "description": "serve /v1/rerank instead of chat (a reranker model)"},
        "extra_args": {"type": "array", "items": {"type": "string"}, "description": "more llama-server arguments"},
        "reload": {"type": "boolean", "description": "hot swap: load a fresh copy beside the running one and switch over when it is ready; requests in progress finish on the old copy"}
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
            "speech.synthesize",
            "Text to speech with llama.cpp's llama-tts: speak a text into an audio file (WAV or raw PCM) on the hub's machine",
            false,
            false,
            json!({
                "input": {"type": "string", "description": "the text to speak (up to 4096 characters)"},
                "output": {"type": "string", "description": "path of the audio file to write"},
                "model": {"type": "string", "description": "a speech model (default: the tts_model setting, else the only one)"},
                "voice": {"type": "string", "description": "a recording in <home>/voices (name without extension) or a full path"},
                "response_format": {"type": "string", "enum": ["wav", "pcm"]},
                "speed": {"type": "number", "minimum": 0.25, "maximum": 4},
                "language": {"type": "string", "description": "ISO-639-1 code, for models that take one"},
                "max_frames": {"type": "integer", "minimum": 1, "description": "limit on the generated audio"}
            }),
            &["input", "output"],
        ),
        op(
            "image.generate",
            "Generate images with the built-in stable-diffusion.cpp. Runs as a job (job.get shows the sampling step, \
             job.cancel stops it) unless wait is true; the PNG files are in the result",
            true,
            false,
            json!({
                "prompt": {"type": "string", "description": "what to draw (up to 4000 characters)"},
                "model": {"type": "string", "description": "an image model (default: the image_model setting, else the only one; see docs/images.md)"},
                "negative_prompt": {"type": "string"},
                "n": {"type": "integer", "minimum": 1, "maximum": 10},
                "size": {"type": "string", "description": "WIDTHxHEIGHT, each a multiple of 8 from 64 to 2048 (default 512x512)"},
                "steps": {"type": "integer", "minimum": 1, "maximum": 150},
                "quality": {"type": "string", "enum": ["low", "medium", "high", "auto"], "description": "10, 20 or 30 steps; steps wins"},
                "cfg_scale": {"type": "number", "description": "guidance (1 for FLUX models)"},
                "sampler": {"type": "string", "description": "sd.cpp sampling method, e.g. euler, euler_a, dpm++2m"},
                "seed": {"type": "integer", "description": "negative or unset: random"},
                "output": {"type": "string", "description": "path of the PNG to write on the hub's machine (n > 1: _0, _1, ... before .png); default: <home>/generated"},
                "wait": {"type": "boolean", "description": "answer only when it has finished"}
            }),
            &["prompt"],
        ),
        op(
            "vision.analyze",
            "Recognise a picture with an ONNX model from <home>/vision (feature `vision`; see docs/vision.md): the              model's kind decides the answer, predictions (classifier), boxes (detector) or a vector (embedding)",
            false,
            false,
            json!({
                "model": {"type": "string", "description": "a vision model (default: the only one in <home>/vision)"},
                "image": {"type": "string", "description": "a PNG or JPEG as base64 or a data URL"},
                "file": {"type": "string", "description": "or the path of the picture on the hub's machine"},
                "top_k": {"type": "integer", "minimum": 1, "maximum": 1000, "description": "predictions (default 5) or detections (default 100) to return"},
                "threshold": {"type": "number", "minimum": 0, "maximum": 1, "description": "the least score of a box (default 0.25) or prediction"}
            }),
            &[],
        ),
        op(
            "quantize",
            "Make a smaller copy of a GGUF model with llama-quantize. Runs as a job (job.get shows the tensors done) \
             unless wait is true",
            true,
            false,
            json!({
                "model": model,
                "input": {"type": "string", "description": "or the path of a .gguf file"},
                "type": {"type": "string", "description": "Q4_K_M, Q5_K_M, Q6_K, Q8_0, IQ4_XS, F16, ... (see docs/tools.md)"},
                "output": {"type": "string", "description": "default: <home>/models/<name>-<TYPE>.gguf"},
                "overwrite": {"type": "boolean"},
                "allow_requantize": {"type": "boolean", "description": "the input is already quantized (loses quality)"},
                "leave_output_tensor": {"type": "boolean"},
                "pure": {"type": "boolean", "description": "one type for every tensor, no k-quant mixtures"},
                "imatrix": {"type": "string", "description": "path of an importance matrix file"},
                "include_weights": {"type": "array", "items": {"type": "string"}},
                "exclude_weights": {"type": "array", "items": {"type": "string"}},
                "output_tensor_type": {"type": "string", "description": "ggml type for output.weight, e.g. q8_0"},
                "token_embedding_type": {"type": "string"},
                "threads": {"type": "integer", "minimum": 1},
                "wait": {"type": "boolean", "description": "answer only when it has finished"}
            }),
            &["type"],
        ),
        op(
            "lora",
            "LoRA adapters of a model: list, attach, detach, or change the scale of one. A loaded model is loaded again \
             (scale changes need no reload)",
            true,
            false,
            json!({
                "model": model,
                "action": {"type": "string", "enum": ["list", "attach", "detach", "scale"]},
                "adapter": {"type": "string", "description": "a LoRA .gguf: a path, or a file in <home>/lora"},
                "scale": {"type": "number", "description": "attach: default 1; scale: the new scale (0 switches it off)"}
            }),
            &["model"],
        ),
        op(
            "speculative",
            "Speculative decoding: use a small draft model to speed up a model (a loaded model is loaded again). \
             Without draft_model or off it shows the setting",
            true,
            false,
            json!({
                "model": model,
                "draft_model": {"type": "string", "description": "a model id or .gguf path; it must share the model's vocabulary"},
                "off": {"type": "boolean", "description": "stop using a draft model"},
                "n_max": {"type": "integer", "minimum": 0, "description": "most tokens drafted at a time"},
                "n_min": {"type": "integer", "minimum": 0},
                "gpu_layers": {"type": "integer", "description": "draft model layers on the GPU (-1 = auto)"}
            }),
            &["model"],
        ),
        op(
            "embeddings.create",
            "Embed text with an embedding model (autoloaded for it; the same as POST /v1/embeddings)",
            false,
            false,
            json!({
                "model": model,
                "input": {"description": "a string, a list of strings, or token ids"},
                "encoding_format": {"type": "string", "enum": ["float", "base64"]}
            }),
            &["input"],
        ),
        op(
            "rerank.run",
            "Rank documents by relevance to a query with a reranker model (autoloaded for it; the same as POST /v1/rerank)",
            false,
            false,
            json!({
                "model": model,
                "query": {"type": "string"},
                "documents": {"type": "array", "items": {"type": "string"}},
                "top_n": {"type": "integer", "minimum": 1}
            }),
            &["query", "documents"],
        ),
        op(
            "model.convert",
            "Convert a Hugging Face model to GGUF (not built in: it needs Python and PyTorch, so this answers 501 with \
             what to install)",
            true,
            false,
            json!({
                "source": {"type": "string", "description": "folder of the Hugging Face model"},
                "output": {"type": "string"}
            }),
            &[],
        ),
        op(
            "key.create",
            "Make an API key for a peer (another machine) with optional quotas and an allow-list of models. The key is shown once, here; only its hash is kept",
            true,
            false,
            json!({
                "peer": {"type": "string", "description": "who the key is for (1 to 64 characters)"},
                "requests_per_min": {"type": "integer", "minimum": 1},
                "tokens_per_day": {"type": "integer", "minimum": 1, "description": "prompt + completion tokens per UTC day"},
                "concurrent": {"type": "integer", "minimum": 1, "description": "requests at a time (a stream counts until it ends)"},
                "models": {"type": "array", "items": {"type": "string"},
                           "description": "model ids the key may use; a trailing * is a prefix; none = every model"}
            }),
            &["peer"],
        ),
        op(
            "key.list",
            "The peer keys (never the secrets): id, peer, quotas, allow-list, revoked",
            false,
            false,
            json!({}),
            &[],
        ),
        op(
            "key.revoke",
            "Revoke a peer key by id, or every key of a peer by name; it stops working at once",
            true,
            true,
            json!({"key": {"type": "string", "description": "a key id from key.list, or a peer name"}}),
            &["key"],
        ),
        op(
            "usage.get",
            "What each peer key has used: requests, errors, refused requests, tokens (also today's), in flight, per model",
            false,
            false,
            json!({"key": {"type": "string", "description": "a key id or peer name (default: all)"}}),
            &[],
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
            if src.is_none() && vision::AVAILABLE {
                out.extend(vision_entries(&e.vision_dirs()));
            }
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
            let (model, resp, _lease) = e.forward("/v1/chat/completions", body).await?;
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
        "speech.synthesize" => {
            let req = tts::SpeechRequest::from_json(a).map_err(Fail::invalid)?;
            let output = s(a, "output")?;
            let (bytes, seconds, model) = run_tts(hub, &req).await?;
            std::fs::write(output, &bytes).map_err(|x| Fail::invalid(format!("{output}: {x}")))?;
            json!({"output": output, "format": req.format.content_type(), "bytes": bytes.len(),
                   "seconds": (seconds * 100.0).round() / 100.0, "model": model})
        }
        "image.generate" => image_generate(hub, a).await?,
        "vision.analyze" => {
            let req = vision::VisionRequest::from_json(a, true).map_err(Fail::invalid)?;
            run_vision(hub, req, None).await?
        }
        "quantize" => quantize(hub, a).await?,
        "lora" => {
            let m = llama_entry(hub, s(a, "model")?)?;
            lora(hub, &m, tools::LoraCmd::from_json(a).map_err(Fail::invalid)?).await?
        }
        "speculative" => {
            let m = llama_entry(hub, s(a, "model")?)?;
            speculative(hub, &m, tools::DraftCmd::from_json(a).map_err(Fail::invalid)?).await?
        }
        "embeddings.create" => {
            tools::check_embeddings(a).map_err(Fail::invalid)?;
            forward_json(hub, "/v1/embeddings", a.clone()).await?
        }
        "rerank.run" => {
            let body = tools::check_rerank(a).map_err(Fail::invalid)?;
            forward_json(hub, "/v1/rerank", body).await?
        }
        "model.convert" => return Err(Fail::new(501, "not_implemented", CONVERT_HELP).into()),
        "key.create" | "key.list" | "key.revoke" | "usage.get" => key_call(&hub.keys, id, a).map_err(ApiError::from)?,
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
        id if id.starts_with("km.") => crate::km::ops::call(hub, id, a).await?,
        _ => return Err(Fail::not_found(format!("no operation {id}")).into()),
    })
}

fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str, Fail> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| Fail::invalid(format!("{k} is required")))
}

/// The key operations, on any key store (the CLI also runs them without a hub, on the files in the home folder).
pub fn key_call(keys: &Keys, id: &str, a: &Value) -> Result<Value, Fail> {
    Ok(match id {
        "key.create" => {
            let n = |k: &str| a.get(k).and_then(Value::as_u64);
            let quotas = Quotas {
                requests_per_min: n("requests_per_min").map(|v| v.min(u32::MAX as u64) as u32),
                tokens_per_day: n("tokens_per_day"),
                concurrent: n("concurrent").map(|v| v.min(u32::MAX as u64) as u32),
            };
            let models: Vec<String> = match a.get("models") {
                None | Some(Value::Null) => vec![],
                Some(Value::Array(l)) => l
                    .iter()
                    .map(|m| m.as_str().map(str::to_string))
                    .collect::<Option<_>>()
                    .ok_or_else(|| Fail::invalid("models must be a list of strings"))?,
                Some(_) => return Err(Fail::invalid("models must be a list of strings")),
            };
            let (mut info, secret) = keys.create(text(a, "peer")?, quotas, models).map_err(Fail::invalid)?;
            info["key"] = json!(secret);
            info["note"] = json!("this is the only time the key is shown");
            info
        }
        "key.list" => json!(keys.list()),
        "key.revoke" => {
            let n = keys.revoke(text(a, "key")?).map_err(Fail::invalid)?;
            if n == 0 && keys.usage(Some(text(a, "key")?)).is_empty() {
                return Err(Fail::not_found("no such key or peer (key.list shows them)"));
            }
            json!({"revoked": n})
        }
        "usage.get" => json!(keys.usage(a.get("key").and_then(Value::as_str))),
        _ => return Err(Fail::not_found(format!("no operation {id}"))),
    })
}

/// What model.convert answers: converting Hugging Face weights needs Python and PyTorch, which are not built in.
const CONVERT_HELP: &str =
    "Converting a Hugging Face model to GGUF is not built in: llama.cpp's converter is a Python \
script that needs Python and PyTorch. To do it yourself: `python -m pip install torch transformers safetensors \
sentencepiece gguf numpy`, then `python convert_hf_to_gguf.py <model folder> --outfile model-f16.gguf --outtype f16` \
(the script is in the llama.cpp sources, engines/llama.cpp in this repository), then `model-mistress quantize` \
for a smaller file. Or download a model that is already in GGUF format with model.pull.";

/// Sends a body to a model's llama-server and returns its JSON answer.
async fn forward_json(hub: &Hub, path: &'static str, body: Value) -> ApiResult<Value> {
    let t0 = Instant::now();
    let (model, resp, _lease) = hub.engine.forward(path, body).await?;
    let status = resp.status();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    hub.engine.record(
        &model,
        status.is_success(),
        t0.elapsed().as_millis() as u64,
        v.get("usage"),
    );
    if !status.is_success() {
        return Err(Fail::new(
            502,
            "backend_error",
            format!("{model}: {}", v["error"]["message"].as_str().unwrap_or(&v.to_string())),
        )
        .into());
    }
    Ok(v)
}

/// Speaks the text: finds the model, voice and tool, runs llama-tts, applies speed and format. Returns the audio,
/// its length in seconds and the model used.
async fn run_tts(hub: &Hub, req: &tts::SpeechRequest) -> ApiResult<(Vec<u8>, f64, String)> {
    let e = &hub.engine;
    let st = e.settings();
    let entry = if tts::is_generic_model(&req.model) {
        tts::pick_default(&e.catalog(), st.tts_model.as_deref())?
    } else {
        llama_entry(hub, &req.model)?
    };
    let (model, mmproj) = tts::files_of(&entry)?;
    let speaker = tts::find_voice(&e.home.join("voices"), &req.voice, st.tts_voice.as_deref())?;
    let tmp = e.home.join("tmp");
    std::fs::create_dir_all(&tmp).map_err(|x| Fail::new(500, "io", format!("{}: {x}", tmp.display())))?;
    let out = tmp.join(format!("tts-{}.wav", uuid::Uuid::new_v4().simple()));
    let args = tts::tool_args(&model, &mmproj, req, speaker.as_deref(), &out, st.gpu_layers);
    let _one_at_a_time = e.tool_lock.lock().await;
    let t0 = Instant::now();
    let ran = e
        .run_tool("llama-tts", &args, Duration::from_secs(1800), &mut |_| true)
        .await;
    let wav = ran.and_then(|_| {
        std::fs::read(&out).map_err(|x| Fail::new(500, "tool_failed", format!("llama-tts wrote no audio: {x}")))
    });
    let _ = std::fs::remove_file(&out);
    e.record(&entry.id, wav.is_ok(), t0.elapsed().as_millis() as u64, None);
    let (bytes, seconds) = tts::finish(&wav?, req).map_err(|x| Fail::new(500, "tool_failed", x))?;
    Ok((bytes, seconds, entry.id))
}

/// What one run of `sd-cli` made.
struct Generated {
    files: Vec<PathBuf>,
    model: String,
    seed: i64,
    seconds: f64,
}

/// Draws the images: finds the model and the tool, runs `sd-cli` (one at a time), and returns the PNG files in
/// `<home>/generated`. With a job, shows the sampling step on it and stops when it is cancelled.
async fn run_image(
    e: &Arc<Engine>,
    req: &image::ImageRequest,
    job: Option<Arc<crate::pull::Job>>,
) -> Result<Generated, Fail> {
    let st = e.settings();
    let model = image::find_model(&e.image_dirs(), &e.catalog(), &req.model, st.image_model.as_deref())?;
    let dir = e.generated_dir();
    std::fs::create_dir_all(&dir).map_err(|x| Fail::new(500, "io", format!("{}: {x}", dir.display())))?;
    sweep_generated(&dir);
    let seed = match req.seed {
        Some(s) if s >= 0 => s,
        _ => rand::random::<u32>() as i64,
    };
    let pattern = std::path::absolute(&dir)
        .unwrap_or(dir)
        .join(format!("image-{}_%d.png", uuid::Uuid::new_v4().simple()));
    let args = image::tool_args(&model, req, seed, &pattern, st.gpu_layers == 0);
    let _one_at_a_time = e.tool_lock.lock().await;
    let t0 = Instant::now();
    let ran = e
        .run_tool("sd-cli", &args, Duration::from_secs(6 * 3600), &mut |line| {
            if let (Some(job), Some((done, total))) = (&job, image::progress(line)) {
                job.progress(done, total, format!("step {done} of {total}"));
            }
            !job.as_ref().is_some_and(|j| j.cancelled())
        })
        .await;
    let files = image::output_files(&pattern, req.n);
    let made = match ran {
        Ok(()) if files.len() == req.n as usize => Ok(()),
        Ok(()) => Err(Fail::new(
            500,
            "tool_failed",
            format!("sd-cli made {} of the {} images", files.len(), req.n),
        )),
        Err(f) => Err(f),
    };
    e.record(&model.id, made.is_ok(), t0.elapsed().as_millis() as u64, None);
    if let Err(f) = made {
        files.iter().for_each(|f| drop(std::fs::remove_file(f)));
        return Err(f);
    }
    Ok(Generated {
        files,
        model: model.id,
        seed,
        seconds: t0.elapsed().as_secs_f64(),
    })
}

/// Removes generated images that are over a day old.
fn sweep_generated(dir: &std::path::Path) {
    let day = Duration::from_secs(24 * 3600);
    for f in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let old = f
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > day);
        if old && f.path().extension().is_some_and(|x| x == "png") {
            let _ = std::fs::remove_file(f.path());
        }
    }
}

/// `image.generate`: a job running sd-cli.
async fn image_generate(hub: &Hub, a: &Value) -> ApiResult<Value> {
    let e = &hub.engine;
    let req = image::ImageRequest::from_json(a).map_err(Fail::invalid)?;
    let output = match a.get("output") {
        None | Some(Value::Null) => None,
        Some(Value::String(p)) if !p.trim().is_empty() => Some(PathBuf::from(p.trim())),
        Some(_) => return Err(Fail::invalid("output must be a file path").into()),
    };
    if let Some(o) = &output {
        if o.extension().and_then(|x| x.to_str()).map(str::to_lowercase).as_deref() != Some("png") {
            return Err(Fail::invalid("output must be a .png file").into());
        }
    }
    let engine = e.clone();
    let job = hub.jobs.start_task(
        "image",
        format!("image-{}", uuid::Uuid::new_v4().simple()),
        move |job| async move {
            let g = run_image(&engine, &req, Some(job)).await?;
            let mut files = g.files.clone();
            if let Some(o) = &output {
                files.clear();
                for (i, f) in g.files.iter().enumerate() {
                    let to = if g.files.len() == 1 {
                        o.clone()
                    } else {
                        let stem = o.file_stem().and_then(|s| s.to_str()).unwrap_or("image");
                        o.with_file_name(format!("{stem}_{i}.png"))
                    };
                    if let Some(dir) = to.parent().filter(|d| !d.as_os_str().is_empty()) {
                        std::fs::create_dir_all(dir)
                            .map_err(|x| Fail::new(500, "io", format!("{}: {x}", dir.display())))?;
                    }
                    std::fs::copy(f, &to).map_err(|x| Fail::new(500, "io", format!("{}: {x}", to.display())))?;
                    let _ = std::fs::remove_file(f);
                    files.push(std::path::absolute(&to).unwrap_or(to));
                }
            }
            Ok(json!({"files": files, "model": g.model, "seed": g.seed, "seconds": (g.seconds * 10.0).round() / 10.0}))
        },
    );
    if a["wait"].as_bool().unwrap_or(false) {
        while job.status() == "running" {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    Ok(job.describe())
}

/// `quantize`: a job running llama-quantize.
async fn quantize(hub: &Hub, a: &Value) -> ApiResult<Value> {
    let e = &hub.engine;
    let req = tools::QuantizeRequest::from_json(a).map_err(Fail::invalid)?;
    let input = match (&req.model, &req.input) {
        (Some(m), _) => llama_entry(hub, m)?.path,
        (_, Some(p)) => {
            if !p.is_file() {
                return Err(Fail::not_found(format!("no file {}", p.display())).into());
            }
            std::path::absolute(p).unwrap_or_else(|_| p.clone())
        }
        _ => return Err(Fail::invalid("give model or input").into()),
    };
    let output = match &req.output {
        Some(o) => std::path::absolute(o).unwrap_or_else(|_| o.clone()),
        None => tools::default_output(&input, &e.home.join("models"), req.quant),
    };
    if output
        .extension()
        .and_then(|x| x.to_str())
        .map(str::to_lowercase)
        .as_deref()
        != Some("gguf")
    {
        return Err(Fail::invalid("output must be a .gguf file").into());
    }
    if output == input {
        return Err(Fail::invalid("output is the input file").into());
    }
    if output.exists() && !req.overwrite {
        return Err(Fail::new(
            409,
            "exists",
            format!("{} exists; give overwrite: true to replace it", output.display()),
        )
        .into());
    }
    if e.tool("llama-quantize").is_none() {
        return Err(Fail::new(
            501,
            "not_implemented",
            "llama-quantize was not found: build ModelMistress with the bundled-engine feature, or put llama-quantize \
             next to llama-server",
        )
        .into());
    }
    if let Some(dir) = output.parent() {
        std::fs::create_dir_all(dir).map_err(|x| Fail::new(500, "io", format!("{}: {x}", dir.display())))?;
    }
    let args = req.tool_args(&input, &output);
    let engine = e.clone();
    let out = output.clone();
    let in_size = std::fs::metadata(&input).map(|m| m.len()).unwrap_or(0);
    let quant = req.quant;
    let job = hub
        .jobs
        .start_task("quantize", output.display().to_string(), move |job| async move {
            let t0 = Instant::now();
            let watch = job.clone();
            let ran = engine
                .run_tool("llama-quantize", &args, Duration::from_secs(24 * 3600), &mut |line| {
                    if let Some((done, total)) = tools::quantize_progress(line) {
                        watch.progress(done, total, format!("tensor {done} of {total}"));
                    }
                    !watch.cancelled()
                })
                .await;
            if let Err(f) = ran {
                let _ = std::fs::remove_file(&out); // a partial file
                return Err(f);
            }
            let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
            let refresher = engine.clone();
            let _ = tokio::task::spawn_blocking(move || refresher.refresh()).await;
            Ok(
                json!({"output": out, "type": quant, "input_bytes": in_size, "output_bytes": size,
                      "seconds": t0.elapsed().as_secs()}),
            )
        });
    if req.wait {
        while job.status() == "running" {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    Ok(job.describe())
}

/// A .gguf file: a path, or a name in `<home>/<folder>` (with or without `.gguf`).
fn find_gguf(home: &std::path::Path, folder: &str, name: &str) -> ApiResult<PathBuf> {
    let direct = PathBuf::from(name);
    let found = if direct.is_file() {
        Some(direct)
    } else {
        [name.to_string(), format!("{name}.gguf")]
            .iter()
            .map(|n| home.join(folder).join(n))
            .find(|p| p.is_file())
    };
    let p = found.ok_or_else(|| {
        Fail::not_found(format!(
            "no file {name} (give a path, or put it in {})",
            home.join(folder).display()
        ))
    })?;
    if p.extension().and_then(|x| x.to_str()).map(str::to_lowercase).as_deref() != Some("gguf") {
        return Err(Fail::invalid(format!("{} is not a .gguf file", p.display())).into());
    }
    if p.to_string_lossy().contains(',') {
        return Err(Fail::invalid("llama-server cannot take a path with a comma; move the file").into());
    }
    Ok(std::path::absolute(&p).unwrap_or(p))
}

/// Is this the adapter that `name` (a path, a file name or a file stem) means?
fn is_adapter(l: &crate::engine::Lora, name: &str) -> bool {
    l.path.as_path() == std::path::Path::new(name)
        || l.path.file_name().is_some_and(|f| f == name)
        || l.path.file_stem().is_some_and(|f| f == name)
}

/// `lora`: adapters attached to a model.
async fn lora(hub: &Hub, m: &crate::catalog::ModelEntry, cmd: tools::LoraCmd) -> ApiResult<Value> {
    use tools::LoraCmd::*;
    let e = &hub.engine;
    let mut x = e.extras(&m.id);
    let shown =
        |x: &crate::engine::Extras, reloaded: bool| json!({"model": m.id, "adapters": x.lora, "reloaded": reloaded});
    match cmd {
        List => {
            let live = match e.loaded().into_iter().find(|i| i.model == m.id) {
                Some(i) => e.lora_get(&i).await.ok(),
                None => None,
            };
            let mut v = shown(&x, false);
            v["loaded"] = json!(e.is_loaded(&m.id));
            v["live"] = live.unwrap_or(Value::Null);
            Ok(v)
        }
        Attach { adapter, scale } => {
            let path = find_gguf(&e.home, "lora", &adapter)?;
            x.lora.retain(|l| l.path != path);
            x.lora.push(crate::engine::Lora { path, scale });
            let reloaded = e.change_extras(m, x.clone()).await?;
            Ok(shown(&x, reloaded))
        }
        Detach { adapter } => {
            match adapter {
                None => x.lora.clear(),
                Some(name) => {
                    let before = x.lora.len();
                    x.lora.retain(|l| !is_adapter(l, &name));
                    if x.lora.len() == before {
                        return Err(Fail::not_found(format!("{name} is not attached to {}", m.id)).into());
                    }
                }
            }
            let reloaded = e.change_extras(m, x.clone()).await?;
            Ok(shown(&x, reloaded))
        }
        Scale { adapter, scale } => {
            let mut hit = false;
            for l in x.lora.iter_mut().filter(|l| is_adapter(l, &adapter)) {
                l.scale = scale;
                hit = true;
            }
            if !hit {
                return Err(Fail::not_found(format!("{adapter} is not attached to {}", m.id)).into());
            }
            e.set_extras(&m.id, x.clone())?;
            // a loaded server takes the new scale without a reload
            if let Some(i) = e.loaded().into_iter().find(|i| i.model == m.id) {
                e.lora_post(&i, &x.lora).await?;
            }
            Ok(shown(&x, false))
        }
    }
}

/// `speculative`: a draft model for a model.
async fn speculative(hub: &Hub, m: &crate::catalog::ModelEntry, cmd: tools::DraftCmd) -> ApiResult<Value> {
    use tools::DraftMode::*;
    let e = &hub.engine;
    let mut x = e.extras(&m.id);
    let reloaded = match cmd.mode {
        Show => return Ok(json!({"model": m.id, "draft": x.draft, "reloaded": false})),
        Off => {
            x.draft = None;
            e.change_extras(m, x.clone()).await?
        }
        Set(name) => {
            let path = if std::path::Path::new(&name).is_file() {
                find_gguf(&e.home, "", &name)?
            } else {
                match e.resolve(&name)? {
                    Target::Llama(d) if d.id == m.id => {
                        return Err(Fail::invalid("a model cannot be its own draft model").into())
                    }
                    Target::Llama(d) => d.path,
                    Target::Ollama(_) => return Err(Fail::invalid("Ollama's models are managed by Ollama").into()),
                }
            };
            x.draft = Some(crate::engine::Draft {
                path,
                n_max: cmd.n_max,
                n_min: cmd.n_min,
                gpu_layers: cmd.gpu_layers,
            });
            e.change_extras(m, x.clone()).await?
        }
    };
    Ok(json!({"model": m.id, "draft": x.draft, "reloaded": reloaded}))
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

async fn openai_models(State(hub): State<Hub>, Extension(who): Extension<Caller>) -> Json<Value> {
    let e = &hub.engine;
    let allowed = |id: &str| who.0.as_ref().is_none_or(|p| p.allows(id, id));
    let mut data: Vec<Value> = e
        .catalog()
        .iter()
        .filter(|m| allowed(&m.id))
        .map(|m| {
            json!({"id": m.id, "object": "model", "created": 0, "owned_by": m.source,
                         "meta": m.summary(e.is_loaded(&m.id))})
        })
        .collect();
    for m in e.ollama_models().await {
        if !allowed(&format!("ollama/{}", m["name"].as_str().unwrap_or(""))) {
            continue;
        }
        data.push(
            json!({"id": format!("ollama/{}", m["name"].as_str().unwrap_or("")), "object": "model",
                         "created": 0, "owned_by": "ollama"}),
        );
    }
    Json(json!({"object": "list", "data": data}))
}

/// Refuse a peer a model its key does not allow.
fn check_model(hub: &Hub, who: &Caller, asked: &str) -> ApiResult<()> {
    let Some(p) = &who.0 else { return Ok(()) };
    let resolved = match hub.engine.resolve(asked) {
        Ok(Target::Llama(e)) => e.id,
        Ok(Target::Ollama(n)) => format!("ollama/{n}"),
        Err(_) => asked.to_string(),
    };
    if p.allows(asked, &resolved) {
        return Ok(());
    }
    Err(Fail::new(
        403,
        "model_not_allowed",
        format!(
            "this key may not use \"{asked}\" (its models: {})",
            p.allow_list().join(", ")
        ),
    )
    .into())
}

/// Ends a request that was handed to a model: keeps the model from being stopped under it, and counts the outcome
/// (and the tokens, from the answer's `usage`, or an estimate from the stream) for the peer's key when it is dropped.
struct Tracker {
    _lease: Option<Lease>,
    who: Option<Arc<Permit>>,
    model: String,
    ok: bool,
    tail: String,
    events: u64,
}

impl Tracker {
    fn observe(&mut self, chunk: &[u8]) {
        if self.who.is_none() {
            return;
        }
        let text = String::from_utf8_lossy(chunk);
        self.events += text.matches("data:").count() as u64;
        self.tail.push_str(&text);
        if self.tail.len() > 4096 {
            let mut cut = self.tail.len() - 4096;
            while !self.tail.is_char_boundary(cut) {
                cut += 1;
            }
            self.tail.drain(..cut);
        }
    }
}

impl Drop for Tracker {
    fn drop(&mut self) {
        let Some(p) = &self.who else { return };
        let usage = self.tail.rfind("\"usage\":").and_then(|i| {
            serde_json::Deserializer::from_str(self.tail[i + 8..].trim_start())
                .into_iter::<Value>()
                .next()?
                .ok()
        });
        let n = |k: &str| usage.as_ref().and_then(|u| u[k].as_u64());
        let (prompt, completion) = match (n("prompt_tokens"), n("completion_tokens")) {
            (None, None) => (0, self.events.saturating_sub(1)), // about a token an event, less the [DONE]
            (a, b) => (a.unwrap_or(0), b.unwrap_or(0)),
        };
        p.finish(&self.model, self.ok, prompt, completion);
    }
}

async fn passthrough(hub: Hub, who: Caller, path: &'static str, body: Value) -> ApiResult<Response> {
    let streaming = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let asked = body.get("model").and_then(Value::as_str).unwrap_or("").to_string();
    check_model(&hub, &who, &asked)?;
    let t0 = Instant::now();
    let (model, resp, lease) = match hub.engine.forward(path, body).await {
        Ok(x) => x,
        Err(f) => {
            if let Some(p) = &who.0 {
                p.finish(&asked, false, 0, 0);
            }
            return Err(f.into());
        }
    };
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let ctype = resp.headers().get("content-type").cloned();
    let mut tracker = Tracker {
        _lease: lease,
        who: who.0.clone(),
        model: model.clone(),
        ok: status.is_success(),
        tail: String::new(),
        events: 0,
    };
    let mut out = if streaming && status.is_success() {
        hub.engine.record(&model, true, t0.elapsed().as_millis() as u64, None);
        // the tracker lives as long as the stream: the model stays up, and the peer's request stays counted as
        // running, until the last byte has gone out
        Response::new(Body::from_stream(resp.bytes_stream().map(move |chunk| {
            match &chunk {
                Ok(b) => tracker.observe(b),
                Err(_) => tracker.ok = false,
            }
            chunk
        })))
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
        tracker.observe(&bytes);
        Response::new(Body::from(bytes))
    };
    *out.status_mut() = status;
    if let Some(c) = ctype {
        out.headers_mut().insert("content-type", c);
    }
    Ok(out)
}

async fn chat(
    State(hub): State<Hub>,
    Extension(who): Extension<Caller>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    passthrough(hub, who, "/v1/chat/completions", body).await
}

async fn completions(
    State(hub): State<Hub>,
    Extension(who): Extension<Caller>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    passthrough(hub, who, "/v1/completions", body).await
}

async fn embeddings(
    State(hub): State<Hub>,
    Extension(who): Extension<Caller>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    tools::check_embeddings(&body).map_err(Fail::invalid)?;
    passthrough(hub, who, "/v1/embeddings", body).await
}

async fn rerank(
    State(hub): State<Hub>,
    Extension(who): Extension<Caller>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let body = tools::check_rerank(&body).map_err(Fail::invalid)?;
    passthrough(hub, who, "/v1/rerank", body).await
}

/// Audio has no model of its own to resolve: the name in the request (`whisper-1`, `tts-1` when it says none) is
/// what a peer key's allow-list is checked against.
fn check_audio_model(hub: &Hub, who: &Caller, asked: &str, generic: &str) -> ApiResult<()> {
    check_model(hub, who, if asked.is_empty() { generic } else { asked })
}

fn finish_audio(who: &Caller, model: &str, ok: bool) {
    if let Some(p) = &who.0 {
        p.finish(model, ok, 0, 0);
    }
}

/// `POST /v1/audio/speech`: JSON in, the audio out.
async fn speech(
    State(hub): State<Hub>,
    Extension(who): Extension<Caller>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let req = tts::SpeechRequest::from_json(&body).map_err(Fail::invalid)?;
    check_audio_model(&hub, &who, &req.model, "tts-1")?;
    let ran = run_tts(&hub, &req).await;
    finish_audio(&who, &req.model, ran.is_ok());
    let (bytes, _, _) = ran?;
    Ok(([("content-type", req.format.content_type())], bytes).into_response())
}

/// `POST /v1/images/generations`: JSON in, the PNGs out (as base64, or as urls of `/v1/images/files/`).
async fn image_generations(
    State(hub): State<Hub>,
    Extension(who): Extension<Caller>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let req = image::ImageRequest::from_json(&body).map_err(Fail::invalid)?;
    check_audio_model(&hub, &who, &req.model, "dall-e-3")?;
    let ran = run_image(&hub.engine, &req, None).await;
    finish_audio(&who, &req.model, ran.is_ok());
    let g = ran?;
    let host = headers.get("host").and_then(|h| h.to_str().ok()).unwrap_or("127.0.0.1");
    let mut data = Vec::new();
    for f in &g.files {
        match req.format {
            image::Format::B64Json => {
                let bytes =
                    std::fs::read(f).map_err(|x| Fail::new(500, "tool_failed", format!("{}: {x}", f.display())))?;
                let _ = std::fs::remove_file(f);
                data.push(json!({"b64_json": image::base64(&bytes)}));
            }
            image::Format::Url => {
                let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
                data.push(json!({"url": format!("http://{host}/v1/images/files/{name}")}));
            }
        }
    }
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Ok(Json(json!({"created": created, "data": data, "model": g.model, "seed": g.seed})).into_response())
}

/// `GET /v1/images/files/{name}`: an image that a `url` answer points to (they are removed after a day).
async fn image_file(State(hub): State<Hub>, Path(name): Path<String>) -> ApiResult<Response> {
    let ok = name.ends_with(".png") && name.chars().all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c));
    if !ok || name.contains("..") {
        return Err(Fail::not_found("no such image").into());
    }
    let bytes = std::fs::read(hub.engine.generated_dir().join(&name)).map_err(|_| Fail::not_found("no such image"))?;
    Ok(([("content-type", "image/png")], bytes).into_response())
}

/// `POST /v1/images/edits`: not available. sd.cpp's image-to-image and inpainting exist, but OpenAI's mask marks the
/// part to change by transparency, which needs a PNG decoder (and the opposite mask polarity) that is not built in.
async fn image_edits() -> ApiResult<Response> {
    Err(Fail::new(
        501,
        "not_implemented",
        "image edits are not available: only /v1/images/generations is (see docs/images.md)",
    )
    .into())
}

/// The vision models for `model.list`: kind "vision", and what the sidecar says the model does (else null: the
/// model's outputs tell, once it is loaded).
fn vision_entries(dirs: &[PathBuf]) -> Vec<Value> {
    vision::list_models(dirs)
        .into_iter()
        .map(|(name, path)| {
            let task = vision::Sidecar::load(&path).ok().and_then(|s| s.forced_kind());
            json!({"id": name, "source": "vision", "kind": "vision", "task": task.map(vision::Kind::name),
                   "size_gb": std::fs::metadata(&path).map(|m| (m.len() as f64 / 1e7).round() / 100.0).ok(),
                   "loaded": Value::Null})
        })
        .collect()
}

/// Find the model and run it on the picture (off the async threads: it takes a while). `want` is the kind a route
/// asked for.
async fn run_vision(hub: &Hub, req: vision::VisionRequest, want: Option<vision::Kind>) -> ApiResult<Value> {
    if !vision::AVAILABLE {
        return Err(Fail::new(501, "not_implemented", vision::NOT_BUILT).into());
    }
    let (name, path) = vision::find_model(&hub.engine.vision_dirs(), &req.model)?;
    let t0 = Instant::now();
    let out = tokio::task::spawn_blocking(move || vision::analyze(&path, &req, want))
        .await
        .map_err(|x| Fail::new(500, "inference_error", x.to_string()))?;
    hub.engine
        .record(&name, out.is_ok(), t0.elapsed().as_millis() as u64, None);
    Ok(out?)
}

/// `POST /v1/vision/{classify,detect,embed}`: JSON in, the recognition result out.
async fn vision_route(hub: Hub, who: Caller, body: Value, want: vision::Kind) -> ApiResult<Response> {
    if !vision::AVAILABLE {
        return Err(Fail::new(501, "not_implemented", vision::NOT_BUILT).into());
    }
    let req = vision::VisionRequest::from_json(&body, false).map_err(Fail::invalid)?;
    check_audio_model(&hub, &who, &req.model, "vision")?;
    let asked = req.model.clone();
    let ran = run_vision(&hub, req, Some(want)).await;
    finish_audio(&who, &asked, ran.is_ok());
    Ok(Json(ran?).into_response())
}

async fn vision_classify(
    State(hub): State<Hub>,
    Extension(who): Extension<Caller>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    vision_route(hub, who, body, vision::Kind::Classify).await
}

async fn vision_detect(
    State(hub): State<Hub>,
    Extension(who): Extension<Caller>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    vision_route(hub, who, body, vision::Kind::Detect).await
}

async fn vision_embed(
    State(hub): State<Hub>,
    Extension(who): Extension<Caller>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    vision_route(hub, who, body, vision::Kind::Embed).await
}

/// The largest vision request: a picture as base64 (4/3 of the bytes) and a little JSON.
const MAX_VISION_BODY: usize = vision::MAX_IMAGE_BYTES / 3 * 4 + (1 << 16);

/// The largest audio upload (OpenAI's own limit is 25 MB; local files are often longer).
const MAX_AUDIO_BYTES: usize = 512 << 20;

/// `POST /v1/audio/transcriptions` and `/v1/audio/translations`: multipart in, the transcript out.
async fn speech_to_text(
    hub: Hub,
    who: Caller,
    headers: HeaderMap,
    body: Bytes,
    translate: bool,
) -> ApiResult<Response> {
    if !speech::AVAILABLE {
        return Err(Fail::new(501, "not_implemented", speech::NOT_BUILT).into());
    }
    let ctype = headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("");
    let req = audio::parse_request(ctype, &body).map_err(Fail::invalid)?;
    let format = req.format;
    check_audio_model(&hub, &who, &req.model, "whisper-1")?;
    let asked = req.model.clone();
    let ran = run_speech(&hub, req, translate).await;
    finish_audio(&who, &asked, ran.is_ok());
    let out = ran?;
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

async fn transcriptions(
    State(hub): State<Hub>,
    Extension(who): Extension<Caller>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    speech_to_text(hub, who, headers, body, false).await
}

async fn translations(
    State(hub): State<Hub>,
    Extension(who): Extension<Caller>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    speech_to_text(hub, who, headers, body, true).await
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
        .route("/v1/rerank", post(rerank))
        .route("/rerank", post(rerank))
        .route("/v1/audio/speech", post(speech))
        .route("/v1/images/generations", post(image_generations))
        .route("/v1/images/edits", post(image_edits))
        .route("/v1/images/files/:name", get(image_file))
        .route(
            "/v1/vision/classify",
            post(vision_classify).layer(DefaultBodyLimit::max(MAX_VISION_BODY)),
        )
        .route(
            "/v1/vision/detect",
            post(vision_detect).layer(DefaultBodyLimit::max(MAX_VISION_BODY)),
        )
        .route(
            "/v1/vision/embed",
            post(vision_embed).layer(DefaultBodyLimit::max(MAX_VISION_BODY)),
        )
        .route(
            "/v1/audio/transcriptions",
            post(transcriptions).layer(DefaultBodyLimit::max(MAX_AUDIO_BYTES)),
        )
        .route(
            "/v1/audio/translations",
            post(translations).layer(DefaultBodyLimit::max(MAX_AUDIO_BYTES)),
        )
        .merge(crate::km::ops::routes())
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
    crate::hosting::check_bind(o.host, hub.engine.settings().allow_network, hub.keys.active())
        .map_err(|m| std::io::Error::new(std::io::ErrorKind::PermissionDenied, m))?;
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
    let keys = hub.keys.clone();
    let housekeeping = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        loop {
            tick.tick().await;
            engine.housekeeping();
            keys.flush();
        }
    });
    let engine = hub.engine.clone();
    let keys = hub.keys.clone();
    let result = axum::serve(
        listener,
        router(hub).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        tokio::select! {
            _ = stop.notified() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    })
    .await;
    housekeeping.abort();
    keys.flush();
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
            keys: Keys::in_memory(),
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

    // ---- text to speech and the llama.cpp tools --------------------------------------------------------------

    /// A hub whose llama-server (and so every tool beside it) is certainly missing, with models given by name.
    fn hub_without_tools(home: &std::path::Path, models: &[&str]) -> Hub {
        std::fs::create_dir_all(home.join("models")).unwrap();
        for m in models {
            crate::catalog::tests_support::gguf(&home.join("models").join(format!("{m}.gguf")), "llama", m);
        }
        Hub {
            engine: Engine::new(
                home.to_path_buf(),
                Settings {
                    llama_server: Some(home.join("no-such-llama-server")),
                    ollama_url: String::new(),
                    ..Default::default()
                },
            ),
            jobs: Arc::new(Default::default()),
            token: Arc::new(TOKEN.into()),
            started: Instant::now(),
            stop: Arc::new(tokio::sync::Notify::new()),
            keys: Keys::in_memory(),
        }
    }

    const JSON: &str = "application/json";

    async fn json_post(hub: &Hub, path: &str, body: Value) -> (u16, Value) {
        let (st, _, text) = post(hub, path, JSON, serde_json::to_vec(&body).unwrap()).await;
        (st.as_u16(), serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    fn message(v: &Value) -> String {
        v["error"]["message"].as_str().unwrap_or("").to_string()
    }

    #[test]
    fn the_tools_are_operations() {
        let ids: Vec<String> = operations()
            .iter()
            .map(|o| o["id"].as_str().unwrap().to_string())
            .collect();
        for id in [
            "speech.synthesize",
            "vision.analyze",
            "quantize",
            "lora",
            "speculative",
            "embeddings.create",
            "rerank.run",
            "model.convert",
        ] {
            assert!(ids.iter().any(|i| i == id), "{id}");
        }
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "operation ids are unique");
        // every operation has an object schema whose required fields are properties
        for o in operations() {
            let schema = &o["input_schema"];
            for r in schema["required"].as_array().unwrap() {
                assert!(schema["properties"].get(r.as_str().unwrap()).is_some(), "{o}");
            }
        }
    }

    #[tokio::test]
    async fn speech_requests_get_clear_errors() {
        let home = temp_home();
        for (dir, name) in [("chat", "plainchat"), ("tts", "fake-tts")] {
            std::fs::create_dir_all(home.join("models").join(dir)).unwrap();
            crate::catalog::tests_support::gguf(
                &home.join("models").join(dir).join(format!("{name}.gguf")),
                "llama",
                name,
            );
        }
        std::fs::write(home.join("models").join("tts").join("mmproj-fake.gguf"), b"x").unwrap();
        let h = hub_without_tools(&home, &[]);
        let url = "/v1/audio/speech";
        let (st, v) = json_post(&h, url, json!({})).await;
        assert!(st == 400 && message(&v).contains("input"), "{v}");
        let (st, v) = json_post(&h, url, json!({"input": "hi", "response_format": "mp3"})).await;
        assert!(st == 400 && message(&v).contains("mp3"), "{v}");
        let (st, v) = json_post(&h, url, json!({"input": "hi", "speed": 10})).await;
        assert!(st == 400 && message(&v).contains("speed"), "{v}");
        // a model that does not exist, and one that has no projector
        let (st, v) = json_post(&h, url, json!({"input": "hi", "model": "nope"})).await;
        assert!(st == 404 && message(&v).contains("nope"), "{st} {v}");
        let (st, v) = json_post(&h, url, json!({"input": "hi", "model": "plainchat"})).await;
        assert!(st == 400 && message(&v).contains("projector"), "{st} {v}");
        // the tts_model setting names a model that is not there
        let mut settings = h.engine.settings();
        settings.tts_model = Some("gone".into());
        h.engine.set_settings(settings).unwrap();
        let (st, v) = json_post(&h, url, json!({"input": "hi", "model": "tts-1"})).await;
        assert!(st == 404 && message(&v).contains("gone"), "{st} {v}");
        // a model with its projector, but an unknown voice, and then no llama-tts
        let (st, v) = json_post(&h, url, json!({"input": "hi", "model": "fake-tts", "voice": "anna"})).await;
        assert!(st == 404 && message(&v).contains("voice"), "{st} {v}");
        let (st, v) = json_post(&h, url, json!({"input": "hi", "model": "fake-tts"})).await;
        assert!(st == 501 && message(&v).contains("llama-tts"), "{st} {v}");
        // the operation takes the same input and also needs an output path
        let (st, v) = json_post(&h, "/v1/call/speech.synthesize", json!({"input": "hi"})).await;
        assert!(st == 400 && message(&v).contains("output"), "{st} {v}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn speech_needs_the_token() {
        let home = temp_home();
        let h = hub(&home);
        for path in ["/v1/audio/speech", "/v1/rerank", "/rerank"] {
            let req = axum::http::Request::post(path).body(Body::empty()).unwrap();
            assert_eq!(router(h.clone()).oneshot(req).await.unwrap().status(), 401, "{path}");
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(not(feature = "vision"))]
    #[tokio::test]
    async fn without_the_feature_vision_is_501() {
        let home = temp_home();
        let h = hub(&home);
        for path in ["/v1/vision/classify", "/v1/vision/detect", "/v1/vision/embed"] {
            let (st, v) = json_post(&h, path, json!({"image": "AAAA"})).await;
            assert!(
                st == 501 && message(&v).contains("--features vision"),
                "{path} {st} {v}"
            );
            let req = axum::http::Request::post(path).body(Body::empty()).unwrap();
            assert_eq!(router(h.clone()).oneshot(req).await.unwrap().status(), 401, "{path}");
        }
        let (st, v) = json_post(&h, "/v1/call/vision.analyze", json!({"image": "AAAA"})).await;
        assert!(st == 501 && message(&v).contains("--features vision"), "{st} {v}");
        // model.list does not offer what cannot run
        std::fs::create_dir_all(home.join("vision")).unwrap();
        std::fs::write(home.join("vision").join("net.onnx"), b"x").unwrap();
        let (_, v) = json_post(&h, "/v1/call/model.list", json!({})).await;
        assert!(!v.to_string().contains("\"vision\""), "{v}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(feature = "vision")]
    #[tokio::test]
    async fn vision_requests_get_clear_errors_and_answers() {
        let home = temp_home();
        let h = hub(&home);
        let (url, image) = ("/v1/vision/classify", crate::image::base64(b"not a picture"));
        let (st, v) = json_post(&h, url, json!({})).await;
        assert!(st == 400 && message(&v).contains("image is required"), "{st} {v}");
        let (st, v) = json_post(&h, url, json!({"image": "!!"})).await;
        assert!(st == 400 && message(&v).contains("base64"), "{st} {v}");
        let (st, v) = json_post(&h, url, json!({"image": image, "top_k": 0})).await;
        assert!(st == 400 && message(&v).contains("top_k"), "{st} {v}");
        // no model anywhere, then one that is not there
        let (st, v) = json_post(&h, url, json!({"image": image})).await;
        assert!(st == 404 && message(&v).contains("no vision model"), "{st} {v}");
        let (st, v) = json_post(&h, url, json!({"image": image, "model": "nope"})).await;
        assert!(st == 404 && message(&v).contains("nope"), "{st} {v}");
        // the operation reads `file`; the routes (which peers may call) do not
        let (st, v) = json_post(&h, "/v1/call/vision.analyze", json!({"file": "/no/such/picture.png"})).await;
        assert!(st == 400 && message(&v).contains("picture.png"), "{st} {v}");
        let (st, v) = json_post(&h, url, json!({"file": "/no/such/picture.png"})).await;
        assert!(st == 400 && message(&v).contains("image is required"), "{st} {v}");

        // a real (tiny) model: listed, classifies, and the other routes refuse it
        let dir = home.join("vision");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("shares.onnx"), crate::vision::test_model::color_share_model()).unwrap();
        std::fs::write(
            dir.join("shares.json"),
            r#"{"labels": ["red", "green", "blue"], "mean": [0, 0, 0], "std": [1, 1, 1], "scale": 0.01}"#,
        )
        .unwrap();
        let (_, v) = json_post(&h, "/v1/call/model.list", json!({})).await;
        let listed = v["result"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == "shares")
            .cloned();
        assert!(
            listed.is_some_and(|m| m["kind"] == "vision" && m["source"] == "vision"),
            "{v}"
        );
        let mut png = Vec::new();
        ::image::RgbImage::from_pixel(6, 6, ::image::Rgb([0, 0, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), ::image::ImageFormat::Png)
            .unwrap();
        let data_url = format!("data:image/png;base64,{}", crate::image::base64(&png));
        let (st, v) = json_post(&h, url, json!({"image": data_url, "top_k": 1})).await;
        assert!(st == 200 && v["predictions"][0]["label"] == "blue", "{st} {v}");
        let (st, v) = json_post(
            &h,
            "/v1/call/vision.analyze",
            json!({"image": data_url, "model": "shares"}),
        )
        .await;
        assert!(
            st == 200 && v["result"]["kind"] == "classify" && v["result"]["model"] == "shares",
            "{st} {v}"
        );
        let (st, v) = json_post(&h, "/v1/vision/embed", json!({"image": data_url})).await;
        assert!(st == 400 && message(&v).contains("/v1/vision/classify"), "{st} {v}");
        let (st, v) = json_post(&h, "/v1/vision/detect", json!({"image": data_url})).await;
        assert_eq!(st, 400, "{v}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn image_requests_get_clear_errors() {
        let home = temp_home();
        let h = hub_without_tools(&home, &[]);
        let url = "/v1/images/generations";
        let (st, v) = json_post(&h, url, json!({})).await;
        assert!(st == 400 && message(&v).contains("prompt"), "{v}");
        let (st, v) = json_post(&h, url, json!({"prompt": "a cat", "size": "100x100"})).await;
        assert!(st == 400 && message(&v).contains("size"), "{v}");
        let (st, v) = json_post(&h, url, json!({"prompt": "a cat", "n": 99})).await;
        assert!(st == 400 && message(&v).contains("n must"), "{v}");
        // no image model anywhere, then a named one that is not there
        let (st, v) = json_post(&h, url, json!({"prompt": "a cat"})).await;
        assert!(st == 404 && message(&v).contains("no image model"), "{st} {v}");
        let (st, v) = json_post(&h, url, json!({"prompt": "a cat", "model": "nope"})).await;
        assert!(st == 404 && message(&v).contains("nope"), "{st} {v}");
        // a model file, but no sd-cli
        std::fs::create_dir_all(home.join("images")).unwrap();
        std::fs::write(home.join("images").join("tiny-sd.safetensors"), b"x").unwrap();
        let (st, v) = json_post(&h, url, json!({"prompt": "a cat", "model": "dall-e-3"})).await;
        assert!(st == 501 && message(&v).contains("sd-cli"), "{st} {v}");
        // the operation checks the same input; the job fails the same way
        let (st, v) = json_post(&h, "/v1/call/image.generate", json!({})).await;
        assert!(st == 400 && message(&v).contains("prompt"), "{st} {v}");
        let (st, v) = json_post(&h, "/v1/call/image.generate", json!({"prompt": "a cat", "wait": true})).await;
        assert!(st == 200 && v["result"]["status"] == "failed", "{st} {v}");
        assert!(v["result"]["message"].as_str().unwrap().contains("sd-cli"), "{v}");
        let (st, v) = json_post(&h, "/v1/call/image.generate", json!({"prompt": "a", "output": "x.jpg"})).await;
        assert!(st == 400 && message(&v).contains(".png"), "{st} {v}");
        // edits are not available
        let (st, v) = json_post(&h, "/v1/images/edits", json!({})).await;
        assert!(st == 501 && message(&v).contains("edits"), "{st} {v}");
        // the files route serves only what the hub generated
        let get = |path: &str| {
            let req = axum::http::Request::get(path)
                .header("authorization", format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap();
            router(h.clone()).oneshot(req)
        };
        assert_eq!(get("/v1/images/files/nope.png").await.unwrap().status(), 404);
        assert_eq!(get("/v1/images/files/x.txt").await.unwrap().status(), 404);
        std::fs::create_dir_all(h.engine.generated_dir()).unwrap();
        std::fs::write(h.engine.generated_dir().join("image-a_0.png"), b"png").unwrap();
        let resp = get("/v1/images/files/image-a_0.png").await.unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers()["content-type"], "image/png");
        for path in ["/v1/images/generations", "/v1/images/edits"] {
            let req = axum::http::Request::post(path).body(Body::empty()).unwrap();
            assert_eq!(router(h.clone()).oneshot(req).await.unwrap().status(), 401, "{path}");
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn peers_may_draw_but_not_manage() {
        assert!(peer_route("/v1/images/generations") && peer_route("/v1/images/files/x.png"));
        assert!(!peer_route("/v1/call/image.generate"));
        assert!(peer_route("/v1/vision/classify") && peer_route("/v1/vision/detect") && peer_route("/v1/vision/embed"));
        assert!(!peer_route("/v1/call/vision.analyze"));
    }

    #[tokio::test]
    async fn embeddings_and_rerank_bodies_are_checked() {
        let home = temp_home();
        let h = hub_without_tools(&home, &[]);
        let (st, v) = json_post(&h, "/v1/embeddings", json!({"model": "m"})).await;
        assert!(st == 400 && message(&v).contains("input"), "{v}");
        let (st, v) = json_post(&h, "/v1/rerank", json!({"model": "m", "query": "q", "documents": []})).await;
        assert!(st == 400 && message(&v).contains("documents"), "{v}");
        let (st, v) = json_post(&h, "/rerank", json!({"query": "q"})).await;
        assert!(st == 400 && message(&v).contains("documents"), "{v}");
        // valid bodies get as far as the model
        let (st, v) = json_post(&h, "/v1/embeddings", json!({"model": "nope", "input": "hi"})).await;
        assert!(st == 404 && message(&v).contains("nope"), "{st} {v}");
        let ok = json!({"model": "nope", "query": "q", "documents": ["a", {"text": "b"}]});
        let (st, v) = json_post(&h, "/v1/rerank", ok.clone()).await;
        assert!(st == 404 && message(&v).contains("nope"), "{st} {v}");
        for (op, body) in [
            ("embeddings.create", json!({"input": ""})),
            ("rerank.run", json!({"query": "q", "documents": [1]})),
        ] {
            let (st, _) = json_post(&h, &format!("/v1/call/{op}"), body).await;
            assert_eq!(st, 400, "{op}");
        }
        let (st, _) = json_post(&h, "/v1/call/rerank.run", ok).await;
        assert_eq!(st, 404);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn convert_says_what_to_install() {
        let home = temp_home();
        let h = hub(&home);
        let (st, v) = json_post(&h, "/v1/call/model.convert", json!({})).await;
        let m = message(&v);
        assert!(
            st == 501 && m.contains("Python") && m.contains("convert_hf_to_gguf.py") && m.contains("model.pull"),
            "{v}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn quantize_checks_before_it_starts() {
        let home = temp_home();
        let h = hub_without_tools(&home, &["small"]);
        let input = home.join("models").join("small.gguf");
        let out = home.join("out");
        let q = |v: Value| json_post(&h, "/v1/call/quantize", v);
        let (st, v) = q(json!({"model": "small"})).await;
        assert!(st == 400 && message(&v).contains("type"), "{v}");
        let (st, v) = q(json!({"model": "small", "type": "Q9"})).await;
        assert!(st == 400 && message(&v).contains("Q4_K_M"), "{v}");
        let (st, _) = q(json!({"model": "nope", "type": "Q4_0"})).await;
        assert_eq!(st, 404);
        let (st, _) = q(json!({"input": home.join("none.gguf"), "type": "Q4_0"})).await;
        assert_eq!(st, 404);
        let (st, v) = q(json!({"input": &input, "type": "Q4_0", "output": out.join("x.bin")})).await;
        assert!(st == 400 && message(&v).contains(".gguf"), "{v}");
        let (st, v) = q(json!({"input": &input, "type": "Q4_0", "output": &input})).await;
        assert!(st == 400 && message(&v).contains("input"), "{v}");
        // the default output name is taken: needs overwrite
        let taken = home.join("models").join("small-Q4_0.gguf");
        std::fs::write(&taken, b"x").unwrap();
        let (st, v) = q(json!({"model": "small", "type": "q4_0"})).await;
        assert!(st == 409 && message(&v).contains("overwrite"), "{v}");
        // everything is right, but there is no llama-quantize
        let (st, v) = q(json!({"model": "small", "type": "Q4_0", "overwrite": true})).await;
        assert!(st == 501 && message(&v).contains("llama-quantize"), "{v}");
        let (st, _) = q(json!({"input": &input, "type": "Q8_0", "output": out.join("x.gguf")})).await;
        assert_eq!(st, 501);
        assert!(!out.join("x.gguf").exists());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn lora_adapters_are_kept_per_model() {
        let home = temp_home();
        let h = hub_without_tools(&home, &["base", "other"]);
        std::fs::create_dir_all(home.join("lora")).unwrap();
        crate::catalog::tests_support::gguf(&home.join("lora").join("style.gguf"), "llama", "style");
        std::fs::write(home.join("lora").join("notes.txt"), b"x").unwrap();
        let l = |v: Value| json_post(&h, "/v1/call/lora", v);
        let adapters = |v: &Value| v["result"]["adapters"].as_array().unwrap().clone();

        let (st, v) = l(json!({"model": "base"})).await;
        assert!(
            st == 200 && adapters(&v).is_empty() && v["result"]["loaded"] == false,
            "{v}"
        );
        // by name from <home>/lora, with and without the extension
        let (st, v) = l(json!({"model": "base", "action": "attach", "adapter": "style", "scale": 0.5})).await;
        assert!(st == 200 && v["result"]["reloaded"] == false, "{v}");
        assert_eq!(adapters(&v).len(), 1);
        assert_eq!(adapters(&v)[0]["scale"], 0.5);
        let (_, v) = l(json!({"model": "base", "action": "attach", "adapter": "style.gguf"})).await;
        assert_eq!(adapters(&v).len(), 1, "the same adapter is replaced, not added twice");
        assert_eq!(adapters(&v)[0]["scale"], 1.0);
        let (_, v) = l(json!({"model": "other"})).await;
        assert!(adapters(&v).is_empty(), "adapters belong to one model");
        // it survives a restart
        assert!(home.join("model_options.json").is_file());
        let again = hub_without_tools(&home, &["base", "other"]);
        assert_eq!(again.engine.extras("base").lora.len(), 1);

        let (st, v) = l(json!({"model": "base", "action": "scale", "adapter": "style", "scale": 0})).await;
        assert!(st == 200 && adapters(&v)[0]["scale"] == 0.0, "{v}");
        let (st, _) = l(json!({"model": "base", "action": "scale", "adapter": "unknown", "scale": 1})).await;
        assert_eq!(st, 404);
        let (st, v) = l(json!({"model": "base", "action": "attach", "adapter": "missing"})).await;
        assert!(st == 404 && message(&v).contains("missing"), "{v}");
        let (st, v) = l(json!({"model": "base", "action": "attach", "adapter": "notes.txt"})).await;
        assert!(st == 400 && message(&v).contains(".gguf"), "{v}");
        let (st, _) = l(json!({"model": "base", "action": "detach", "adapter": "nothing"})).await;
        assert_eq!(st, 404);
        let (st, v) = l(json!({"model": "base", "action": "detach", "adapter": "style"})).await;
        assert!(st == 200 && adapters(&v).is_empty(), "{v}");
        assert!(again.engine.extras("other").lora.is_empty());
        let (st, _) = l(json!({"model": "nope"})).await;
        assert_eq!(st, 404);
        let (st, _) = l(json!({"model": "base", "action": "merge"})).await;
        assert_eq!(st, 400);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn a_draft_model_is_kept_per_model() {
        let home = temp_home();
        let h = hub_without_tools(&home, &["big", "tiny"]);
        let sp = |v: Value| json_post(&h, "/v1/call/speculative", v);
        let (st, v) = sp(json!({"model": "big"})).await;
        assert!(st == 200 && v["result"]["draft"].is_null(), "{v}");
        let (st, v) = sp(json!({"model": "big", "draft_model": "tiny", "n_max": 8, "gpu_layers": 0})).await;
        assert!(st == 200 && v["result"]["reloaded"] == false, "{v}");
        let d = &v["result"]["draft"];
        assert!(
            d["path"].as_str().unwrap().ends_with("tiny.gguf") && d["n_max"] == 8 && d["gpu_layers"] == 0,
            "{v}"
        );
        assert_eq!(h.engine.extras("big").draft.unwrap().n_max, Some(8));
        let (st, v) = sp(json!({"model": "big", "draft_model": "big"})).await;
        assert!(st == 400 && message(&v).contains("own draft"), "{v}");
        let (st, _) = sp(json!({"model": "big", "draft_model": "nope"})).await;
        assert_eq!(st, 404);
        let (st, _) = sp(json!({"model": "big", "draft_model": "tiny", "off": true})).await;
        assert_eq!(st, 400);
        let (st, v) = sp(json!({"model": "big", "off": true})).await;
        assert!(st == 200 && v["result"]["draft"].is_null(), "{v}");
        assert_eq!(h.engine.extras("big"), crate::engine::Extras::default());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Needs a model; see docs/tools.md. Run, with a built-in llama.cpp (the bundled-engine feature):
    ///   MM_TEST_EMBED_MODEL=<an embedding model's .gguf> cargo test --features bundled-engine -- --ignored embeds_text
    #[tokio::test]
    #[ignore = "needs an embedding model (MM_TEST_EMBED_MODEL) and a llama.cpp (feature bundled-engine)"]
    async fn embeds_text() {
        let model = std::env::var("MM_TEST_EMBED_MODEL").expect("MM_TEST_EMBED_MODEL");
        let home = temp_home();
        let dir = std::path::Path::new(&model).parent().unwrap().to_path_buf();
        let h = Hub {
            engine: Engine::new(
                home.clone(),
                Settings {
                    model_dirs: vec![dir],
                    ollama_url: String::new(),
                    ..Default::default()
                },
            ),
            jobs: Arc::new(Default::default()),
            token: Arc::new(TOKEN.into()),
            started: Instant::now(),
            stop: Arc::new(tokio::sync::Notify::new()),
            keys: Keys::in_memory(),
        };
        let id = std::path::Path::new(&model)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let (st, v) = json_post(
            &h,
            "/v1/embeddings",
            json!({"model": id, "input": ["a cat", "a kitten", "a tax form"]}),
        )
        .await;
        assert_eq!(st, 200, "{v}");
        let vec_of = |i: usize| -> Vec<f64> {
            v["data"][i]["embedding"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_f64().unwrap())
                .collect()
        };
        let cos = |a: &[f64], b: &[f64]| {
            let dot: f64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
            dot / (a.iter().map(|x| x * x).sum::<f64>().sqrt() * b.iter().map(|x| x * x).sum::<f64>().sqrt())
        };
        let (cat, kitten, tax) = (vec_of(0), vec_of(1), vec_of(2));
        assert!(
            cat.len() > 16 && cos(&cat, &kitten) > cos(&cat, &tax),
            "a cat is nearer a kitten than a tax form"
        );
        // the same model cannot rerank: it is loaded for embeddings
        let (st, v) = json_post(&h, "/v1/rerank", json!({"model": id, "query": "q", "documents": ["a"]})).await;
        assert_eq!(st, 409, "{v}");
        h.engine.unload_all();
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A hub with the built-in llama.cpp and the folder of a model.
    fn real_hub(home: &std::path::Path, model: &str) -> Hub {
        let dir = std::path::Path::new(model).parent().unwrap().to_path_buf();
        Hub {
            engine: Engine::new(
                home.to_path_buf(),
                Settings {
                    model_dirs: vec![dir],
                    ollama_url: String::new(),
                    ctx_size: 512,
                    ..Default::default()
                },
            ),
            jobs: Arc::new(Default::default()),
            token: Arc::new(TOKEN.into()),
            started: Instant::now(),
            stop: Arc::new(tokio::sync::Notify::new()),
            keys: Keys::in_memory(),
        }
    }

    /// Needs a GGUF model (any; a small one is quick) and a built-in llama.cpp; see docs/tools.md. Run:
    ///   MM_TEST_QUANTIZE_MODEL=<a .gguf> cargo test --features bundled-engine -- --ignored quantizes_a_real_model
    #[tokio::test]
    #[ignore = "needs a .gguf model (MM_TEST_QUANTIZE_MODEL) and a llama.cpp (feature bundled-engine)"]
    async fn quantizes_a_real_model() {
        let model = std::env::var("MM_TEST_QUANTIZE_MODEL").expect("MM_TEST_QUANTIZE_MODEL");
        let home = temp_home();
        let h = real_hub(&home, &model);
        let out = home.join("quantized").join("small.gguf");
        let body = json!({"input": &model, "type": "Q4_0", "output": &out, "allow_requantize": true, "wait": true});
        let (st, v) = json_post(&h, "/v1/call/quantize", body.clone()).await;
        assert_eq!(st, 200, "{v}");
        let job = &v["result"];
        assert_eq!(job["status"], "done", "{job}");
        assert!(
            job["total_bytes"].as_u64().unwrap() > 0 && job["done_bytes"] == job["total_bytes"],
            "{job}"
        );
        let written = std::fs::read(&out).unwrap();
        assert_eq!(&written[..4], b"GGUF");
        assert_eq!(job["result"]["output_bytes"].as_u64().unwrap(), written.len() as u64);
        // the job is listed, and a second run needs overwrite
        let (_, v) = json_post(&h, "/v1/call/job.list", json!({})).await;
        assert_eq!(v["result"].as_array().unwrap().len(), 1);
        let (st, _) = json_post(&h, "/v1/call/quantize", body).await;
        assert_eq!(st, 409);
        h.engine.unload_all();
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Needs a chat model and a LoRA adapter made for it; see docs/tools.md. Run:
    ///   MM_TEST_LORA_MODEL=<a .gguf> MM_TEST_LORA_ADAPTER=<an adapter .gguf> \
    ///   cargo test --features bundled-engine -- --ignored lora_on_a_real_model
    #[tokio::test]
    #[ignore = "needs a model and its LoRA adapter (MM_TEST_LORA_MODEL, MM_TEST_LORA_ADAPTER) and feature bundled-engine"]
    async fn lora_on_a_real_model() {
        let model = std::env::var("MM_TEST_LORA_MODEL").expect("MM_TEST_LORA_MODEL");
        let adapter = std::env::var("MM_TEST_LORA_ADAPTER").expect("MM_TEST_LORA_ADAPTER");
        let home = temp_home();
        let h = real_hub(&home, &model);
        let id = std::path::Path::new(&model)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let call = |op: &'static str, body: Value| json_post(&h, op, body);
        let (st, v) = call(
            "/v1/call/model.load",
            json!({"model": id, "ctx_size": 512, "gpu_layers": 0}),
        )
        .await;
        assert_eq!(st, 200, "{v}");
        // attaching reloads the loaded model with the adapter, scaled
        let (st, v) = call(
            "/v1/call/lora",
            json!({"model": id, "action": "attach", "adapter": adapter, "scale": 0.5}),
        )
        .await;
        assert!(st == 200 && v["result"]["reloaded"] == true, "{v}");
        let (_, v) = call("/v1/call/lora", json!({"model": id})).await;
        let live = v["result"]["live"].as_array().unwrap();
        assert!(live.len() == 1 && live[0]["scale"] == 0.5, "{v}");
        let (st, v) = call(
            "/v1/call/chat.complete",
            json!({"model": id, "prompt": "Say hi", "max_tokens": 8}),
        )
        .await;
        assert_eq!(st, 200, "{v}");
        // a new scale needs no reload
        let (st, v) = call(
            "/v1/call/lora",
            json!({"model": id, "action": "scale", "adapter": adapter, "scale": 0}),
        )
        .await;
        assert!(st == 200 && v["result"]["reloaded"] == false, "{v}");
        let (_, v) = call("/v1/call/lora", json!({"model": id})).await;
        assert_eq!(v["result"]["live"][0]["scale"], 0.0, "{v}");
        let (st, v) = call("/v1/call/lora", json!({"model": id, "action": "detach"})).await;
        assert!(st == 200 && v["result"]["reloaded"] == true, "{v}");
        let (_, v) = call("/v1/call/lora", json!({"model": id})).await;
        assert!(v["result"]["live"].as_array().unwrap().is_empty(), "{v}");
        h.engine.unload_all();
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Needs a chat model and a draft model with the same vocabulary (the model itself will do, as a path); see
    /// docs/tools.md. Run:
    ///   MM_TEST_SPEC_MODEL=<a .gguf> MM_TEST_SPEC_DRAFT=<a .gguf> \
    ///   cargo test --features bundled-engine -- --ignored speculative_decoding_on_a_real_model
    #[tokio::test]
    #[ignore = "needs a model and a draft model (MM_TEST_SPEC_MODEL, MM_TEST_SPEC_DRAFT) and feature bundled-engine"]
    async fn speculative_decoding_on_a_real_model() {
        let model = std::env::var("MM_TEST_SPEC_MODEL").expect("MM_TEST_SPEC_MODEL");
        let draft = std::env::var("MM_TEST_SPEC_DRAFT").expect("MM_TEST_SPEC_DRAFT");
        let home = temp_home();
        let h = real_hub(&home, &model);
        let id = std::path::Path::new(&model)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let (st, v) = json_post(
            &h,
            "/v1/call/model.load",
            json!({"model": id, "ctx_size": 512, "gpu_layers": 0}),
        )
        .await;
        assert_eq!(st, 200, "{v}");
        let body = json!({"model": id, "draft_model": draft, "n_max": 4, "gpu_layers": 0});
        let (st, v) = json_post(&h, "/v1/call/speculative", body).await;
        assert!(st == 200 && v["result"]["reloaded"] == true, "{v}");
        let (_, v) = json_post(&h, "/v1/call/model.loaded", json!({})).await;
        let args = v["result"][0]["args"].to_string();
        assert!(
            args.contains("--spec-draft-model") && args.contains("--spec-draft-n-max"),
            "{args}"
        );
        let (st, v) = json_post(
            &h,
            "/v1/call/chat.complete",
            json!({"model": id, "prompt": "Say hi", "max_tokens": 8}),
        )
        .await;
        assert_eq!(st, 200, "{v}");
        let (st, v) = json_post(&h, "/v1/call/speculative", json!({"model": id, "off": true})).await;
        assert!(st == 200 && v["result"]["reloaded"] == true, "{v}");
        h.engine.unload_all();
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Needs an image model (Stable Diffusion or FLUX; for FLUX its VAE and text encoders beside it) and a built-in
    /// stable-diffusion.cpp; see docs/images.md. Run:
    ///   MM_TEST_IMAGE_MODEL=<the model file> cargo test --features bundled-engine -- --ignored draws_with_a_real_model
    #[tokio::test]
    #[ignore = "needs an image model (MM_TEST_IMAGE_MODEL) and feature bundled-engine"]
    async fn draws_with_a_real_model() {
        let model = std::env::var("MM_TEST_IMAGE_MODEL").expect("MM_TEST_IMAGE_MODEL");
        let home = temp_home();
        let h = real_hub(&home, &model);
        let body = json!({"model": model, "prompt": "a red apple on a table", "size": "256x256", "steps": 4,
                          "seed": 7, "cfg_scale": 1.0});
        let (st, ct, bytes) = json_post_raw(&h, "/v1/images/generations", body).await;
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(st == 200 && ct.starts_with("application/json"), "{st} {v}");
        assert_eq!(v["data"].as_array().unwrap().len(), 1);
        let png = png_of(v["data"][0]["b64_json"].as_str().unwrap());
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "not a PNG");
        // the width and height are in the IHDR chunk
        let dim = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
        assert_eq!((dim(16), dim(20)), (256, 256));
        // the operation is a job with progress, and writes where it is told
        let out = home.join("apple.png");
        let (st, v) = json_post(
            &h,
            "/v1/call/image.generate",
            json!({"model": model, "prompt": "a red apple", "size": "256x256", "steps": 4, "output": &out,
                   "wait": true}),
        )
        .await;
        let job = &v["result"];
        assert!(st == 200 && job["status"] == "done", "{v}");
        assert!(std::fs::read(&out).unwrap().starts_with(b"\x89PNG"), "{v}");
        assert!(
            job["total_bytes"].as_u64().unwrap_or(0) >= 4,
            "the sampling steps were counted: {job}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    fn png_of(b64: &str) -> Vec<u8> {
        let val = |c: u8| match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            _ => 63,
        };
        let mut out = Vec::new();
        for c in b64.trim_end_matches('=').as_bytes().chunks(4) {
            let n = c.iter().fold(0u32, |n, &x| n << 6 | val(x) as u32) << (6 * (4 - c.len()));
            out.extend(&n.to_be_bytes()[1..c.len()]);
        }
        out
    }

    /// Needs a text-to-speech model with its mmproj*.gguf beside it, and a built-in llama.cpp; see
    /// docs/text-to-speech.md. Run:
    ///   MM_TEST_TTS_MODEL=<the model's .gguf> [MM_TEST_TTS_VOICE=<a speaker recording>] \
    ///   cargo test --features bundled-engine -- --ignored speaks_with_a_real_model
    #[tokio::test]
    #[ignore = "needs a speech model and its projector (MM_TEST_TTS_MODEL) and feature bundled-engine"]
    async fn speaks_with_a_real_model() {
        let model = std::env::var("MM_TEST_TTS_MODEL").expect("MM_TEST_TTS_MODEL");
        let voice = std::env::var("MM_TEST_TTS_VOICE").unwrap_or_default();
        let home = temp_home();
        let h = real_hub(&home, &model);
        let id = std::path::Path::new(&model)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let say = |extra: Value| {
            let mut b =
                json!({"model": id, "input": "Hello there, this is a test.", "voice": voice, "max_frames": 200});
            b.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            json_post_raw(&h, "/v1/audio/speech", b)
        };
        let (st, ct, wav) = say(json!({})).await;
        assert!(st == 200 && ct == "audio/wav" && wav.starts_with(b"RIFF"), "{st} {ct}");
        let normal = crate::tts::parse_wav(&wav).unwrap();
        let seconds = normal.samples.len() as f64 / normal.rate as f64;
        assert!(
            seconds > 0.3 && normal.samples.iter().any(|s| s.abs() > 100),
            "{seconds} s"
        );
        // twice as fast is about half as long; pcm is the same samples without the header
        let (_, _, fast) = say(json!({"speed": 2.0})).await;
        let fast = crate::tts::parse_wav(&fast).unwrap();
        let ratio = fast.samples.len() as f64 / normal.samples.len() as f64;
        assert!((0.35..0.7).contains(&ratio), "{ratio}");
        let (st, ct, pcm) = say(json!({"response_format": "pcm"})).await;
        assert!(st == 200 && ct == "audio/pcm" && !pcm.starts_with(b"RIFF"));
        // the operation writes a file
        let out = home.join("hello.wav");
        let (st, v) = json_post(
            &h,
            "/v1/call/speech.synthesize",
            json!({"model": id, "input": "Hello.", "voice": voice, "output": &out, "max_frames": 100}),
        )
        .await;
        assert!(st == 200 && std::fs::read(&out).unwrap().starts_with(b"RIFF"), "{v}");
        let _ = std::fs::remove_dir_all(&home);
    }

    async fn json_post_raw(hub: &Hub, path: &str, body: Value) -> (u16, String, Vec<u8>) {
        let req = axum::http::Request::post(path)
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("content-type", JSON)
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = router(hub.clone()).oneshot(req).await.unwrap();
        let status = resp.status().as_u16();
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        (
            status,
            ct,
            axum::body::to_bytes(resp.into_body(), 1 << 28).await.unwrap().to_vec(),
        )
    }
}

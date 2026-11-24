//! The engine: serves catalog models through llama.cpp, one managed `llama-server` process per loaded model,
//! and passes `ollama/<name>` models through to a running Ollama.
//!
//! Each `llama-server` listens on 127.0.0.1 on a free port with its own random API key, so nothing else on the
//! machine can use it directly; only this hub talks to it. The processes belong to the hub: on Windows they are
//! in a job object that closes with the hub, on Linux they get SIGKILL when the hub dies, and `unload`/shutdown
//! stop them. Models load on first use (`autoload`) and the least recently used one is unloaded when
//! `max_loaded` would be exceeded.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::catalog::{self, ModelEntry, Roots};
use crate::serving::{self, Plan, Resident, Resolved, ServeFlags, ServeProfile, Serving};

/// An error with the HTTP status the hub should answer with.
#[derive(Debug)]
pub struct Fail {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
    /// Seconds to wait before trying again: sent as `Retry-After` and as `retry_after_s`.
    pub retry_after: Option<u64>,
    /// Fields the error carries besides its code and message, such as a context size (an object, or Null).
    pub fields: Value,
}

impl Fail {
    pub fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retry_after: None,
            fields: Value::Null,
        }
    }

    /// The error object of the hub's error shape: `code`, `message` and `type`, then the fields and `retry_after_s`.
    pub fn error_object(&self) -> Value {
        let mut e = json!({"code": self.code, "message": self.message, "type": self.code});
        self.add_fields(&mut e);
        e
    }

    /// Adds this error's fields and `retry_after_s` to a JSON object.
    pub fn add_fields(&self, obj: &mut Value) {
        if let Some(extra) = self.fields.as_object() {
            for (k, v) in extra {
                obj[k.as_str()] = v.clone();
            }
        }
        if let Some(wait) = self.retry_after {
            obj["retry_after_s"] = json!(wait);
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(400, "invalid", message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(404, "not_found", message)
    }
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(503, "unavailable", message)
    }
}

pub type Result<T> = std::result::Result<T, Fail>;

/// `<home>/config.toml`. Every field has a default, so an empty or missing file is fine.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Path to `llama-server`. Unset: MM_LLAMA_SERVER, then the one built into this binary, then
    /// `<home>/engines/llama.cpp/`, then PATH.
    pub llama_server: Option<PathBuf>,
    /// Path to `rigel-server`, which serves the models whose serving profile says `engine = "rigel"` (Rigel: a build
    /// of a llama.cpp fork with paged MoE, for Windows and AMD GPUs). Unset: MM_RIGEL_SERVER, then the one built into
    /// this binary (feature `rigel-engine`), then `<home>/engines/rigel/`, then PATH.
    pub rigel_server: Option<PathBuf>,
    /// Path to the Prism `llama-server` (PrismML's llama.cpp fork), which serves the models whose serving profile says
    /// `engine = "prism"`: it runs every ternary Bonsai packing (PTQ1_0, PQ2_0, Q2_0). Unset: MM_PRISM_SERVER, then
    /// `<home>/engines/prism/` (where `model-mistress misty setup` puts it), then PATH is not searched (the name would
    /// be the stock server's).
    pub prism_server: Option<PathBuf>,
    /// Extra folders of .gguf files.
    pub model_dirs: Vec<PathBuf>,
    /// Extra Ollama stores (the folder that holds `manifests` and `blobs`).
    pub ollama_dirs: Vec<PathBuf>,
    /// Use Ollama models at all: the stores found on this machine, `ollama_dirs`, the download folder's `ollama`
    /// store, and `ollama/<name>` passed to a running Ollama. Off: no model with source "ollama" is listed or served.
    pub ollama_discovery: bool,
    /// Extra Hugging Face hub caches.
    pub hf_dirs: Vec<PathBuf>,
    /// A running Ollama to pass `ollama/<name>` models to ("" = never).
    pub ollama_url: String,
    /// The most models kept loaded at once.
    pub max_loaded: usize,
    /// Context size for a load that does not give one (0 = the model's own).
    pub ctx_size: u32,
    /// Layers on the GPU (999 = all that fit, 0 = CPU only).
    pub gpu_layers: i32,
    /// Parallel request slots per model.
    pub parallel: u32,
    /// Serving options for every model (KV-cache types, flash attention, prompt cache, batch sizes, ...): the
    /// `[serving]` table. Unset options add no flag.
    pub serving: ServeProfile,
    /// Serving options per model id: `[profiles."<model id>"]`. They win over `serving`.
    pub profiles: BTreeMap<String, ServeProfile>,
    /// What `load` found wrong in `[serving]` and `[profiles]` and worked around (each bad field uses its default).
    /// Not part of the file; service.status and config.get show it.
    #[serde(skip)]
    pub warnings: Vec<String>,
    /// Refuse a load whose estimated VRAM does not fit the GPU (unloading the least recently used models first)
    /// instead of letting it spill into shared memory. Needs the engine to list devices; else it does nothing.
    pub vram_guard: bool,
    /// MiB of VRAM left free beyond a load's estimate.
    pub vram_margin_mib: u64,
    /// MiB of each GPU's memory that models never use, left for the system (the desktop, the driver): the models
    /// together hold at most the card's total minus this (24,560 MiB card, 2048 reserved: 22,512 MiB, about 22 GB).
    pub vram_reserve_mib: u64,
    /// Give each llama-server a thread count (`-t`) that leaves the cores a CPU model and a GPU model both need (see
    /// [`serving::thread_budget`]). A thread setting of a profile, a load or `extra_args` wins. Off: llama-server's own
    /// default (all physical cores) for every server.
    pub thread_budget: bool,
    /// Load a model on its first request.
    pub autoload: bool,
    pub load_timeout_s: u64,
    /// Unload a model after this many idle seconds (0 = never).
    pub idle_unload_s: u64,
    /// Models loaded for embeddings or reranking that may stay up next to the `max_loaded` chat models (they are
    /// counted apart, so an embeddings request does not evict a chat model that fits).
    pub max_loaded_embedding: usize,
    /// Answer `busy` at once (503, Retry-After 1) when a model's llama-server has no idle slot, instead of letting the
    /// request wait in the server's queue. Off by default: the request waits, as it always did. The slot is checked with
    /// `GET /slots?fail_on_no_slot=1`, so a slot that frees up between that check and the request is still queued.
    pub fail_fast_busy: bool,
    /// Windows only: a hub that was started at below-normal or idle priority (a Scheduled Task's default is below
    /// normal) raises itself to normal priority, so the model servers it starts after that inherit normal priority
    /// too. Off by default: a hub started low stays low. It never goes above normal and never lowers a priority.
    pub raise_priority: bool,
    /// `serve` ends the hub when its own `/v1/health` has not answered for this many seconds (0 = never), so that
    /// whatever started it (a Scheduled Task, a service manager) starts it again, and leaves `logs/hang-<time>.txt`
    /// to say where every thread was stuck. Read when the hub starts.
    pub watchdog_s: u64,
    /// When a load fails because of the GPU (it ran out of memory, or the driver crashed the server), try again with
    /// more room kept free and then on the CPU, and remember a crash so that the next load goes straight to the CPU.
    /// Off: the failure is the answer.
    pub gpu_fallback: bool,
    /// Let requests from this machine (the peer address is 127.0.0.1 or ::1) call the hub without a token, so
    /// Ollama clients that send no Authorization header work. Off by default; see the README.
    pub allow_loopback_no_token: bool,
    /// Used when a request names no model.
    pub default_model: Option<String>,
    /// Extra arguments for every llama-server.
    pub extra_args: Vec<String>,
    /// Where model.pull saves models (default: the home folder). Its `hub` and `ollama` folders are scanned.
    pub download_dir: Option<PathBuf>,
    /// A Hugging Face token for gated or private repos (else HF_TOKEN). config.get never shows it.
    pub hf_token: Option<String>,
    /// The whisper model (`base.en` for `ggml-base.en.bin`) that speech to text uses when a request names none or
    /// says `whisper-1`. Unset: the only whisper model found, else the first of small, base, tiny.
    pub whisper_model: Option<String>,
    /// The speech model (a model id; it needs its `mmproj*.gguf` beside it) that text to speech uses when a request
    /// names none or says `tts-1`. Unset: the only speech model found.
    pub tts_model: Option<String>,
    /// The voice (a recording in `<home>/voices`, or a full path) used when a request names none or an OpenAI voice.
    pub tts_voice: Option<String>,
    /// The image model (an id, a file name in `<home>/images`, or a path) that image generation uses when a request
    /// names none or an OpenAI one (`dall-e-3`). Unset: the only image model found.
    pub image_model: Option<String>,
    /// Allow `serve --host` to listen beyond this machine (it also needs at least one peer key). Off by default.
    pub allow_network: bool,
    /// Knowledge Modules: trusted keys, budgets, embedder, selection weights (see docs/native/km-runtime.md).
    pub km: crate::km::KmSettings,
    /// The Forge (training): where its Python environment lives (`<forge_dir>/env`). Unset: MM_FORGE_DIR, else
    /// `<home>/forge`.
    pub forge_dir: Option<PathBuf>,
    /// The Forge's weights, checkpoints and outputs. Unset: MM_FORGE_DATA_DIR, else `<forge_dir>/data`.
    pub forge_data_dir: Option<PathBuf>,
    /// The `forge` folder holding `mm_forge/` and the requirements. Unset: MM_FORGE_SRC, else looked for near the program.
    pub forge_src: Option<PathBuf>,
    /// The Python that creates the Forge environment. Unset: MM_FORGE_PYTHON, else `python` on PATH.
    pub forge_python: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            llama_server: None,
            rigel_server: None,
            prism_server: None,
            model_dirs: vec![],
            ollama_dirs: vec![],
            ollama_discovery: true,
            hf_dirs: vec![],
            ollama_url: "http://127.0.0.1:11434".into(),
            max_loaded: 2,
            ctx_size: 8192,
            gpu_layers: 999,
            parallel: 2,
            serving: ServeProfile::default(),
            profiles: BTreeMap::new(),
            warnings: vec![],
            vram_guard: true,
            vram_margin_mib: 768,
            vram_reserve_mib: 2048,
            thread_budget: true,
            autoload: true,
            load_timeout_s: 300,
            idle_unload_s: 0,
            max_loaded_embedding: 1,
            fail_fast_busy: false,
            raise_priority: false,
            watchdog_s: 90,
            gpu_fallback: true,
            allow_loopback_no_token: false,
            default_model: None,
            extra_args: vec![],
            download_dir: None,
            hf_token: None,
            whisper_model: None,
            tts_model: None,
            tts_voice: None,
            image_model: None,
            allow_network: false,
            km: Default::default(),
            forge_dir: None,
            forge_data_dir: None,
            forge_src: None,
            forge_python: None,
        }
    }
}

impl Settings {
    pub fn load(home: &Path) -> std::result::Result<Self, String> {
        match std::fs::read_to_string(home.join("config.toml")) {
            Ok(s) => {
                let path = home.join("config.toml");
                let bad = |e: toml::de::Error| format!("{}: {e}", path.display());
                let mut table: toml::Table = toml::from_str(&s).map_err(bad)?;
                // [serving] and [profiles] are read leniently (a typo there is a warning); everything else is strict
                let (serving, profiles) = (table.remove("serving"), table.remove("profiles"));
                let mut st: Self = toml::Value::Table(table).try_into().map_err(bad)?;
                let mut warnings = vec![];
                match serving {
                    None => {}
                    Some(toml::Value::Table(t)) => {
                        let (p, w) = ServeProfile::lenient(&t, "serving");
                        st.serving = p;
                        warnings.extend(w);
                    }
                    Some(_) => warnings.push("serving: not a table, ignored (defaults used)".into()),
                }
                match profiles {
                    None => {}
                    Some(toml::Value::Table(all)) => {
                        for (id, v) in all {
                            let what = format!("profiles.\"{id}\"");
                            match v {
                                toml::Value::Table(t) => {
                                    let (p, w) = ServeProfile::lenient(&t, &what);
                                    st.profiles.insert(id, p);
                                    warnings.extend(w);
                                }
                                _ => warnings.push(format!("{what}: not a table, ignored")),
                            }
                        }
                    }
                    Some(_) => warnings.push("profiles: not a table, ignored".into()),
                }
                for w in &warnings {
                    eprintln!("model-mistress: warning: {}: {w}", path.display());
                }
                st.warnings = warnings;
                Ok(st)
            }
            Err(_) => Ok(Self::default()),
        }
    }

    /// Checks `[serving]` and every `[profiles]` entry.
    pub fn check_serving(&self) -> std::result::Result<(), String> {
        self.serving.check("serving")?;
        for (id, p) in &self.profiles {
            p.check(&format!("profiles.\"{id}\""))?;
        }
        Ok(())
    }

    /// The profile for a model id (exact, else case-insensitive).
    pub fn profile_for(&self, id: &str) -> Option<&ServeProfile> {
        self.profiles.get(id).or_else(|| {
            self.profiles
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(id))
                .map(|(_, v)| v)
        })
    }

    pub fn save(&self, home: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).map_err(|e| Fail::new(500, "io", e.to_string()))?;
        let path = home.join("config.toml");
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)
            .and_then(|_| std::fs::rename(&tmp, &path))
            .map_err(|e| Fail::new(500, "io", format!("{}: {e}", path.display())))
    }
}

/// How to load one model. Unset fields come from [`Settings`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadOpts {
    pub ctx_size: Option<u32>,
    pub gpu_layers: Option<i32>,
    pub parallel: Option<u32>,
    pub threads: Option<u32>,
    /// Serve /v1/embeddings (llama.cpp then serves embeddings only).
    #[serde(default)]
    pub embeddings: bool,
    /// Serve /v1/rerank (llama.cpp then serves reranking only; the model must be a reranker).
    #[serde(default)]
    pub reranking: bool,
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Keep every expert tensor of a mixture-of-experts model in system memory (llama-server's `--cpu-moe`) and the
    /// rest on the GPU. Unset: on for MM's own (native) MoE models, off otherwise.
    #[serde(default)]
    pub cpu_moe: Option<bool>,
    /// Only the experts of the first N layers on the CPU (`--n-cpu-moe N`); wins over `cpu_moe`.
    #[serde(default)]
    pub n_cpu_moe: Option<u32>,
    /// Hot swap: if the model is loaded, start the new one beside it and switch over when it is ready. Requests
    /// already running finish on the old one; new ones go to the new one.
    #[serde(default)]
    pub reload: bool,
    /// Serving options of this load (KV-cache types, flash attention, prompt cache, batch sizes, ...); they win over
    /// the model's profile. They are plain fields of the request: `{"model": "x", "cache_type_k": "q8_0"}`.
    #[serde(default, flatten)]
    pub serve: ServeFlags,
    /// Load even if the VRAM planner says it does not fit.
    #[serde(default)]
    pub skip_vram_check: bool,
    /// Try the GPU again for a model that was moved to the CPU after a crash (see the `gpu_fallback` setting).
    #[serde(default)]
    pub retry_gpu: bool,
}

/// A LoRA adapter attached to a model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lora {
    pub path: PathBuf,
    pub scale: f32,
}

/// A draft model for speculative decoding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Draft {
    pub path: PathBuf,
    pub n_max: Option<u32>,
    pub n_min: Option<u32>,
    pub gpu_layers: Option<i32>,
}

/// What a model is loaded with besides its [`LoadOpts`]: kept per model in `<home>/model_options.json`, so it
/// survives an unload (autoload) and a restart.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Extras {
    #[serde(default)]
    pub lora: Vec<Lora>,
    #[serde(default)]
    pub draft: Option<Draft>,
    /// Load this GGUF instead of the catalog entry's file: the core assembled with experts KMs (km/assemble.rs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_path: Option<PathBuf>,
}

/// llama-server arguments for LoRA adapters. They are loaded without being applied (`--lora-init-without-apply`);
/// the scales are set through the server's /lora-adapters API once it is up, which is also how a scale changes later.
pub fn lora_args(lora: &[Lora]) -> Vec<String> {
    if lora.is_empty() {
        return vec![];
    }
    let mut a = vec!["--lora-init-without-apply".to_string()];
    for l in lora {
        a.extend(["--lora".into(), l.path.display().to_string()]);
    }
    a
}

/// llama-server arguments for a draft model.
/// `--cpu-moe` / `--n-cpu-moe N` for a mixture-of-experts model: what the load asks for, else on for a native one
/// (experts in RAM, attention and the shared expert on the GPU: the 24 GB Dusty Q8_0 does not fit a 24 GB card otherwise).
pub fn moe_args(entry: &ModelEntry, opts: &LoadOpts) -> Vec<String> {
    if entry.experts().is_none() {
        return vec![];
    }
    match (opts.n_cpu_moe, opts.cpu_moe) {
        (Some(n), _) if n > 0 => vec!["--n-cpu-moe".into(), n.to_string()],
        (Some(_), None) | (_, Some(false)) => vec![],
        (_, Some(true)) => vec!["--cpu-moe".into()],
        (None, None) if entry.native.is_some() => vec!["--cpu-moe".into()],
        _ => vec![],
    }
}

pub fn draft_args(d: &Draft, default_gpu_layers: i32) -> Vec<String> {
    // the pinned llama-server loads a draft model but does not use it unless `--spec-type` says how (default: none)
    let mut a = vec![
        "--spec-type".to_string(),
        "draft-simple".to_string(),
        "--spec-draft-model".to_string(),
        d.path.display().to_string(),
    ];
    if let Some(n) = d.n_max {
        a.extend(["--spec-draft-n-max".into(), n.to_string()]);
    }
    if let Some(n) = d.n_min {
        a.extend(["--spec-draft-n-min".into(), n.to_string()]);
    }
    a.extend([
        "--spec-draft-ngl".into(),
        d.gpu_layers.unwrap_or(default_gpu_layers).to_string(),
    ]);
    a
}

/// The body of POST /lora-adapters: every adapter by its position, with its scale.
pub fn lora_scales(lora: &[Lora]) -> Value {
    json!(lora
        .iter()
        .enumerate()
        .map(|(i, l)| json!({"id": i, "scale": l.scale}))
        .collect::<Vec<_>>())
}

/// A server that has started and is not ready: what [`Engine::loading`] reports on.
struct Loading {
    model: String,
    pid: u32,
    size_mib: u64,
    started: Instant,
}

/// Takes a model off the list of loading servers when its load ends, whichever way.
struct LoadingGuard<'a> {
    list: &'a Mutex<Vec<Loading>>,
    model: String,
}

impl Drop for LoadingGuard<'_> {
    fn drop(&mut self) {
        self.list.lock().retain(|l| l.model != self.model);
    }
}

pub struct Instance {
    pub model: String,
    pub port: u16,
    pub pid: u32,
    key: String,
    child: Mutex<Option<tokio::process::Child>>,
    pub started: Instant,
    pub args: Vec<String>,
    pub log: PathBuf,
    pub embeddings: bool,
    pub reranking: bool,
    pub opts: LoadOpts,
    pub extras: Extras,
    /// How it is served: context, slots, KV types, VRAM (see [`Serving`]).
    serving: RwLock<Serving>,
    last_used: AtomicU64,
    /// The idle-unload override of a request's keep_alive: 0 = none (the global `idle_unload_s` applies), -1 =
    /// keep forever, else the unix second it expires at.
    keep: AtomicI64,
    /// Requests being served now (see [`Lease`]).
    inflight: AtomicUsize,
    /// No longer in `loaded`: it stops when its last request is done.
    retired: AtomicBool,
    stopped: AtomicBool,
    /// Runs when it stops (a server that is not our child, such as a stand-in in tests).
    on_stop: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

/// A request in progress on an instance. The instance is not stopped while any lease on it is held, even when a hot
/// swap or an eviction has already taken it out of service.
pub struct Lease(Arc<Instance>);

impl Drop for Lease {
    fn drop(&mut self) {
        if self.0.inflight.fetch_sub(1, Ordering::AcqRel) == 1 && self.0.retired.load(Ordering::Acquire) {
            self.0.stop();
        }
    }
}

impl Instance {
    fn touch(&self) {
        self.last_used.store(now_s(), Ordering::Relaxed);
    }
    /// Whether the idle unload takes this model now, given the global `idle` seconds (0 = never).
    pub fn expired(&self, idle: u64) -> bool {
        match self.keep.load(Ordering::Relaxed) {
            0 => idle > 0 && self.idle_s() > idle,
            k if k < 0 => false,
            at => now_s() as i64 >= at && self.inflight() == 0,
        }
    }
    /// Seconds until the idle unload takes this model, given the global `idle` (None: never).
    pub fn expires_in(&self, idle: u64) -> Option<u64> {
        match self.keep.load(Ordering::Relaxed) {
            0 => (idle > 0).then(|| idle.saturating_sub(self.idle_s())),
            k if k < 0 => None,
            at => Some((at as u64).saturating_sub(now_s())),
        }
    }
    pub fn idle_s(&self) -> u64 {
        now_s().saturating_sub(self.last_used.load(Ordering::Relaxed))
    }
    pub fn describe(&self) -> Value {
        json!({"model": self.model, "backend": "llama.cpp", "pid": self.pid, "port": self.port,
               "uptime_s": self.started.elapsed().as_secs(), "idle_s": self.idle_s(),
               "embeddings": self.embeddings, "reranking": self.reranking, "args": self.args, "log": self.log,
               "lora": self.extras.lora, "draft": self.extras.draft, "serving": self.serving()})
    }
    pub fn serving(&self) -> Serving {
        self.serving.read().clone()
    }
    pub fn inflight(&self) -> usize {
        self.inflight.load(Ordering::Acquire)
    }
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(mut c) = self.child.lock().take() {
            let _ = c.start_kill();
        }
        let on_stop = self.on_stop.lock().take();
        if let Some(f) = on_stop {
            f();
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ModelMetrics {
    pub requests: u64,
    pub errors: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_ms: u64,
    pub loads: u64,
    pub last_load_ms: u64,
}

/// Where a request goes.
pub enum Target {
    Llama(ModelEntry),
    Ollama(String),
}

pub struct Engine {
    pub home: PathBuf,
    settings: RwLock<Settings>,
    catalog: RwLock<Vec<ModelEntry>>,
    loaded: Mutex<HashMap<String, Arc<Instance>>>,
    /// Instances taken out of service that still have requests running, with when they were retired.
    draining: Mutex<Vec<(Instant, Arc<Instance>)>>,
    load_lock: tokio::sync::Mutex<()>,
    pub metrics: Mutex<BTreeMap<String, ModelMetrics>>,
    pub http: reqwest::Client,
    llama_version: Mutex<Option<(PathBuf, String)>>,
    /// The llama-server built into this binary (feature `bundled-engine`), once written to disk.
    bundled: Option<PathBuf>,
    /// The rigel-server built into this binary (feature `rigel-engine`), once written to disk.
    bundled_rigel: Option<PathBuf>,
    /// LoRA adapters and draft models per model id (see [`Extras`]).
    extras: Mutex<HashMap<String, Extras>>,
    /// Models whose load crashed on the GPU, and that go to the CPU until the engine changes (`placements.json`).
    learned: Mutex<BTreeMap<String, Learned>>,
    /// What each GPU has shown it lets one process hold, by `device|server program` (`gpu_limits.json`).
    limits: Mutex<BTreeMap<String, serving::GpuLimit>>,
    /// Servers that have started and are not ready yet, for [`Engine::loading`].
    loading: Mutex<Vec<Loading>>,
    /// The cards as last probed, so that listing models does not start a process per request.
    device_cache: Mutex<Option<(Instant, Vec<serving::Device>)>>,
    /// Models Misty keeps on the CPU (the embedder, the reranker), so that the card is left to the language models. A
    /// model with GPU layers set in its profile or in the request is not moved.
    cpu_hints: Mutex<std::collections::HashSet<String>>,
    /// A test's cards, in place of what `llama-server --list-devices` and the performance counters say.
    #[cfg(test)]
    fake_devices: Mutex<Option<Vec<serving::Device>>>,
    /// A test's commit headroom (None: there is no such limit).
    #[cfg(test)]
    fake_commit: Mutex<Option<u64>>,
    /// Which options a server program has, from its `--help`, by `path|flag`.
    help_flags: Mutex<HashMap<String, bool>>,
    /// The llama.cpp tools (llama-tts, llama-quantize) that take the machine's memory run one at a time per kind.
    pub tool_lock: tokio::sync::Mutex<()>,
    #[cfg(windows)]
    job: Option<job::Job>,
}

fn now_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

impl Engine {
    pub fn new(home: PathBuf, settings: Settings) -> Arc<Self> {
        if settings.raise_priority {
            raise_to_normal_priority();
        }
        let e = Arc::new(Self {
            settings: RwLock::new(settings),
            catalog: RwLock::new(vec![]),
            loaded: Mutex::new(HashMap::new()),
            draining: Mutex::new(Vec::new()),
            load_lock: tokio::sync::Mutex::new(()),
            metrics: Mutex::new(BTreeMap::new()),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
            llama_version: Mutex::new(None),
            bundled: bundled::install(&home),
            bundled_rigel: bundled::install_rigel(&home),
            extras: Mutex::new(load_extras(&home)),
            learned: Mutex::new(load_learned(&home)),
            limits: Mutex::new(load_limits(&home)),
            loading: Mutex::new(vec![]),
            device_cache: Mutex::new(None),
            cpu_hints: Mutex::new(Default::default()),
            #[cfg(test)]
            fake_devices: Mutex::new(None),
            #[cfg(test)]
            fake_commit: Mutex::new(None),
            help_flags: Mutex::new(HashMap::new()),
            tool_lock: tokio::sync::Mutex::new(()),
            home,
            #[cfg(windows)]
            job: job::Job::new(),
        });
        e.refresh();
        e
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().clone()
    }

    pub fn set_settings(&self, s: Settings) -> Result<()> {
        s.check_serving().map_err(Fail::invalid)?;
        let mut s = s;
        s.warnings.clear(); // what was saved is valid
        s.save(&self.home)?;
        if s.raise_priority {
            raise_to_normal_priority(); // turned on while the hub runs: the next model server starts at normal
        }
        *self.settings.write() = s;
        *self.llama_version.lock() = None;
        self.refresh();
        Ok(())
    }

    /// The folders found on this machine: `OLLAMA_MODELS`, the home folder's `.ollama`, the Hugging Face caches.
    /// Tests get none, so a test never depends on the models installed where it runs; it names the folders it
    /// needs in its settings.
    #[cfg(not(test))]
    fn machine_roots() -> Roots {
        Roots::detected()
    }

    #[cfg(test)]
    fn machine_roots() -> Roots {
        Roots::default()
    }

    pub fn roots(&self) -> Roots {
        let s = self.settings.read();
        let mut r = Self::machine_roots();
        if s.ollama_discovery {
            r.ollama.splice(0..0, s.ollama_dirs.iter().cloned());
        } else {
            r.ollama.clear();
        }
        r.huggingface.splice(0..0, s.hf_dirs.iter().cloned());
        r.folders.splice(0..0, s.model_dirs.iter().cloned());
        r.folders.push(self.home.join("models"));
        let downloads = s.download_dir.clone().unwrap_or_else(|| self.home.clone());
        r.huggingface.push(downloads.join("hub"));
        if s.ollama_discovery {
            r.ollama.push(downloads.join("ollama"));
        }
        r.native.push(self.forge_data_dir());
        r
    }

    /// The Forge's data folder (setting, else MM_FORGE_DATA_DIR, else `<forge_dir>/data`), where MM's own models are.
    pub fn forge_data_dir(&self) -> PathBuf {
        let s = self.settings.read();
        let set = |p: &Option<PathBuf>, var: &str| {
            p.clone()
                .filter(|p| !p.as_os_str().is_empty())
                .or_else(|| std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from))
        };
        set(&s.forge_data_dir, "MM_FORGE_DATA_DIR").unwrap_or_else(|| {
            set(&s.forge_dir, "MM_FORGE_DIR")
                .unwrap_or_else(|| self.home.join("forge"))
                .join("data")
        })
    }

    /// Where image models are looked for: `<home>/images` first (everything in it is an image model), then the
    /// model folders (a file counts there when its name says it is a diffusion model).
    pub fn image_dirs(&self) -> Vec<PathBuf> {
        let s = self.settings.read();
        let mut d = vec![self.home.join("images")];
        d.extend(s.model_dirs.iter().cloned());
        d.push(self.home.join("models"));
        d
    }

    /// Where vision models (`.onnx` files) are looked for: `<home>/vision` first, then the model folders.
    pub fn vision_dirs(&self) -> Vec<PathBuf> {
        let s = self.settings.read();
        let mut d = vec![self.home.join("vision")];
        d.extend(s.model_dirs.iter().cloned());
        d.push(self.home.join("models"));
        d
    }

    /// Where generated images are kept until the owner deletes them.
    pub fn generated_dir(&self) -> PathBuf {
        self.home.join("generated")
    }

    /// Where whisper models (`ggml-*.bin`) are looked for.
    pub fn whisper_dirs(&self) -> Vec<PathBuf> {
        let s = self.settings.read();
        let mut d = vec![self.home.join("whisper")];
        d.extend(s.model_dirs.iter().cloned());
        d.push(self.home.join("models"));
        d.push(
            s.download_dir
                .clone()
                .unwrap_or_else(|| self.home.clone())
                .join("whisper"),
        );
        d
    }

    /// Rescan the disk. Returns how many models were found.
    pub fn refresh(&self) -> usize {
        let found = catalog::scan(&self.roots());
        let n = found.len();
        *self.catalog.write() = found;
        n
    }

    pub fn catalog(&self) -> Vec<ModelEntry> {
        self.catalog.read().clone()
    }

    pub fn is_loaded(&self, id: &str) -> bool {
        self.loaded.lock().contains_key(id)
    }

    pub fn loaded(&self) -> Vec<Arc<Instance>> {
        let mut v: Vec<_> = self.loaded.lock().values().cloned().collect();
        v.sort_by(|a, b| a.model.cmp(&b.model));
        v
    }

    // ---- llama.cpp ------------------------------------------------------------------------------------------

    pub fn llama_server(&self) -> Option<PathBuf> {
        if let Some(p) = &self.settings.read().llama_server {
            return p.is_file().then(|| p.clone());
        }
        if let Some(p) = std::env::var_os("MM_LLAMA_SERVER").map(PathBuf::from) {
            if p.is_file() {
                return Some(p);
            }
        }
        if let Some(p) = self.bundled.as_ref().filter(|p| p.is_file()) {
            return Some(p.clone());
        }
        let bundled = self.home.join("engines").join("llama.cpp").join(exe("llama-server"));
        if bundled.is_file() {
            return Some(bundled);
        }
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|d| d.join(exe("llama-server")))
            .find(|p| p.is_file())
    }

    /// `rigel-server`: the `rigel_server` setting, MM_RIGEL_SERVER, the built-in one, `<home>/engines/rigel/`, PATH.
    pub fn rigel_server(&self) -> Option<PathBuf> {
        if let Some(p) = &self.settings.read().rigel_server {
            return p.is_file().then(|| p.clone());
        }
        if let Some(p) = std::env::var_os("MM_RIGEL_SERVER").map(PathBuf::from) {
            if p.is_file() {
                return Some(p);
            }
        }
        if let Some(p) = self.bundled_rigel.as_ref().filter(|p| p.is_file()) {
            return Some(p.clone());
        }
        let own = self.home.join("engines").join("rigel").join(exe("rigel-server"));
        if own.is_file() {
            return Some(own);
        }
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|d| d.join(exe("rigel-server")))
            .find(|p| p.is_file())
    }

    /// The Prism `llama-server`: the `prism_server` setting, MM_PRISM_SERVER, then `<home>/engines/prism/` (the zip's
    /// files may sit in that folder or one level down).
    pub fn prism_server(&self) -> Option<PathBuf> {
        if let Some(p) = &self.settings.read().prism_server {
            return p.is_file().then(|| p.clone());
        }
        if let Some(p) = std::env::var_os("MM_PRISM_SERVER").map(PathBuf::from) {
            if p.is_file() {
                return Some(p);
            }
        }
        let base = self.home.join("engines").join("prism");
        let direct = base.join(exe("llama-server"));
        if direct.is_file() {
            return Some(direct);
        }
        std::fs::read_dir(&base)
            .ok()?
            .flatten()
            .map(|e| e.path().join(exe("llama-server")))
            .find(|p| p.is_file())
    }

    /// The Prism `llama-server` with the Vulkan backend (`misty setup --install prism-gpu`): MM_PRISM_GPU_SERVER, else
    /// `<home>/engines/prism-gpu/` (the zip's files may sit in that folder or one level down). Ternary files run on the
    /// GPU with it; the CPU build ([`prism_server`](Self::prism_server)) serves them when no layer goes to the GPU.
    pub fn prism_gpu_server(&self) -> Option<PathBuf> {
        if let Some(p) = std::env::var_os("MM_PRISM_GPU_SERVER").map(PathBuf::from) {
            if p.is_file() {
                return Some(p);
            }
        }
        let base = self.home.join("engines").join("prism-gpu");
        let direct = base.join(exe("llama-server"));
        if direct.is_file() {
            return Some(direct);
        }
        std::fs::read_dir(&base)
            .ok()?
            .flatten()
            .map(|e| e.path().join(exe("llama-server")))
            .find(|p| p.is_file())
    }

    /// `llama-server --version`, cached per path.
    pub async fn llama_version(&self) -> Option<String> {
        let exe = self.llama_server()?;
        if let Some((p, v)) = self.llama_version.lock().clone() {
            if p == exe {
                return Some(v);
            }
        }
        let mut cmd = tokio::process::Command::new(&exe);
        cmd.arg("--version");
        no_window(&mut cmd);
        let out = tokio::time::timeout(Duration::from_secs(20), cmd.output())
            .await
            .ok()?
            .ok()?;
        let text = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
        let v = text
            .lines()
            .find(|l| l.starts_with("version:"))
            .unwrap_or("unknown")
            .trim()
            .to_string();
        *self.llama_version.lock() = Some((exe, v.clone()));
        Some(v)
    }

    /// A program that is built with llama-server and lives beside it (`llama-quantize`, `llama-tts`, and
    /// stable-diffusion.cpp's `sd-cli`, which MM_SD_CLI can point elsewhere).
    pub fn tool(&self, name: &str) -> Option<PathBuf> {
        if name == "sd-cli" {
            if let Some(p) = std::env::var_os("MM_SD_CLI").map(PathBuf::from).filter(|p| p.is_file()) {
                return Some(p);
            }
        }
        let p = self.llama_server()?.with_file_name(exe(name));
        p.is_file().then_some(p)
    }

    /// Runs one of those programs to the end. Every line it prints (the tools log to stderr) goes to `on_line`, which
    /// returns false to cancel. Fails with 501 when the program is missing, 504 after `timeout`, 500 when it exits
    /// with an error (the last lines of its output are in the message).
    pub async fn run_tool(
        &self,
        name: &str,
        args: &[String],
        timeout: Duration,
        on_line: &mut (dyn FnMut(&str) -> bool + Send),
    ) -> Result<()> {
        use tokio::io::{AsyncRead, AsyncReadExt};
        let exe = self.tool(name).ok_or_else(|| {
            Fail::new(
                501,
                "not_implemented",
                format!(
                    "{name} was not found: build ModelMistress with the bundled-engine feature, or put {name} next to                      llama-server"
                ),
            )
        })?;
        let mut cmd = tokio::process::Command::new(&exe);
        cmd.args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(dir) = exe.parent() {
            cmd.current_dir(dir);
        }
        no_window(&mut cmd);
        #[cfg(target_os = "linux")]
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| Fail::unavailable(format!("cannot start {}: {e}", exe.display())))?;
        #[cfg(windows)]
        if let (Some(job), Some(h)) = (&self.job, child.raw_handle()) {
            job.assign(h);
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        fn pump<R: AsyncRead + Unpin + Send + 'static>(r: R, tx: tokio::sync::mpsc::UnboundedSender<String>) {
            tokio::spawn(async move {
                // A line ends at LF or CR: a progress bar redraws its line with CR, and each redraw is a line here.
                let mut r = r;
                let mut chunk = [0u8; 4096];
                let mut line: Vec<u8> = Vec::new();
                loop {
                    let n = match r.read(&mut chunk).await {
                        Ok(n) if n > 0 => n,
                        _ => break,
                    };
                    for &b in &chunk[..n] {
                        if b == 0x0a || b == 0x0d {
                            if !line.is_empty() {
                                let _ = tx.send(String::from_utf8_lossy(&line).trim_end().to_string());
                                line.clear();
                            }
                        } else {
                            line.push(b);
                        }
                    }
                }
                if !line.is_empty() {
                    let _ = tx.send(String::from_utf8_lossy(&line).trim_end().to_string());
                }
            });
        }
        if let Some(o) = child.stdout.take() {
            pump(o, tx.clone());
        }
        if let Some(e) = child.stderr.take() {
            pump(e, tx.clone());
        }
        drop(tx);
        let mut tail: std::collections::VecDeque<String> = Default::default();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            tokio::select! {
                line = rx.recv() => match line {
                    Some(l) => {
                        if tail.len() >= 15 {
                            tail.pop_front();
                        }
                        tail.push_back(l.clone());
                        if !on_line(&l) {
                            let _ = child.kill().await;
                            return Err(Fail::new(409, "cancelled", format!("{name} was cancelled")));
                        }
                    }
                    None => break,
                },
                _ = tokio::time::sleep_until(deadline) => {
                    let _ = child.kill().await;
                    return Err(Fail::new(504, "timeout", format!("{name} took more than {} s", timeout.as_secs())));
                }
            }
        }
        let status = child
            .wait()
            .await
            .map_err(|e| Fail::new(500, "io", format!("{name}: {e}")))?;
        if status.success() {
            Ok(())
        } else {
            let text = tail.into_iter().collect::<Vec<_>>().join("\n");
            let mut f = Fail::new(500, "tool_failed", format!("{name} failed ({status}):\n{text}"));
            // what a retry on the CPU can act on (see `run_gpu_tool`)
            f.fields = json!({"exit_code": status.code(),
                              "gpu_failure": gpu_failure(&text, &status).map(GpuFailure::name)});
            Err(f)
        }
    }

    /// [`run_tool`](Self::run_tool) for a tool that can use the GPU (llama-tts, sd-cli). `args_for(cpu)` are its
    /// arguments, for the CPU only when `cpu` is true. When it fails because of the GPU (out of memory, or a driver
    /// crash) and `gpu_fallback` is on, it is run once more on the CPU. `gpu_layers` 0 is a tool that never had the GPU.
    pub async fn run_gpu_tool(
        &self,
        name: &str,
        gpu_layers: i32,
        args_for: &(dyn Fn(bool) -> Vec<String> + Sync),
        timeout: Duration,
        on_line: &mut (dyn FnMut(&str) -> bool + Send),
    ) -> Result<()> {
        let Err(first) = self.run_tool(name, &args_for(gpu_layers == 0), timeout, on_line).await else {
            return Ok(());
        };
        let Some(why) = self.cpu_retry_for_tool(gpu_layers, &first) else {
            return Err(first);
        };
        self.run_tool(name, &args_for(true), timeout, on_line)
            .await
            .map_err(|mut f| {
                f.message = format!("{} (before this, on the GPU: {why})", f.message);
                f
            })
    }

    /// Whether a tool that failed is run again on the CPU, and what to say about the first failure.
    fn cpu_retry_for_tool(&self, gpu_layers: i32, f: &Fail) -> Option<String> {
        if gpu_layers == 0 || !self.settings().gpu_fallback || f.code != "tool_failed" {
            return None;
        }
        Some(match f.fields["gpu_failure"].as_str()? {
            "crash" => "the tool crashed".to_string(),
            _ => "the GPU ran out of memory".to_string(),
        })
    }

    /// The LoRA adapters and draft model a model is loaded with.
    pub fn extras(&self, id: &str) -> Extras {
        self.extras.lock().get(id).cloned().unwrap_or_default()
    }

    pub fn set_extras(&self, id: &str, x: Extras) -> Result<()> {
        let mut all = self.extras.lock();
        if x == Extras::default() {
            all.remove(id);
        } else {
            all.insert(id.to_string(), x);
        }
        let path = self.home.join("model_options.json");
        let text = serde_json::to_vec_pretty(&*all).map_err(|e| Fail::new(500, "io", e.to_string()))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)
            .and_then(|_| std::fs::rename(&tmp, &path))
            .map_err(|e| Fail::new(500, "io", format!("{}: {e}", path.display())))
    }

    /// Saves new LoRA adapters / draft model for a model and, if it is loaded, loads it again with them (same
    /// options otherwise). Returns whether it was reloaded. If the new setup does not load, the old one is
    /// restored (and the model loaded again) and the error returned.
    pub async fn change_extras(&self, entry: &ModelEntry, new: Extras) -> Result<bool> {
        let old = self.extras(&entry.id);
        let was = self.loaded.lock().get(&entry.id).map(|i| i.opts.clone());
        self.set_extras(&entry.id, new)?;
        let Some(opts) = was else { return Ok(false) };
        // a hot swap: the old setup serves until the new one is ready, and stays if the new one does not load
        let opts = LoadOpts { reload: true, ..opts };
        match self.ensure_loaded(entry, &opts, true).await {
            Ok(_) => Ok(true),
            Err(f) => {
                let _ = self.set_extras(&entry.id, old);
                Err(f)
            }
        }
    }

    // ---- resolving names ----------------------------------------------------------------------------------

    pub fn resolve(&self, name: &str) -> Result<Target> {
        let name = name.trim();
        if let Some(rest) = name.strip_prefix("ollama/") {
            if !self.settings.read().ollama_discovery {
                return Err(Fail::unavailable(
                    "Ollama models are turned off (ollama_discovery is false)",
                ));
            }
            if self.settings.read().ollama_url.is_empty() {
                return Err(Fail::unavailable(
                    "passing models to Ollama is turned off (ollama_url is empty)",
                ));
            }
            return Ok(Target::Ollama(rest.to_string()));
        }
        let name = if name.is_empty() {
            match self.settings.read().default_model.clone() {
                Some(d) => d,
                None => {
                    let loaded = self.loaded.lock();
                    match loaded.keys().collect::<Vec<_>>().as_slice() {
                        [one] => (*one).clone(),
                        _ => return Err(Fail::invalid("no model given, and no default_model is set")),
                    }
                }
            }
        } else {
            name.to_string()
        };
        let cat = self.catalog.read();
        let lower = name.to_lowercase();
        let hit = cat
            .iter()
            .find(|e| e.id == name)
            .or_else(|| cat.iter().find(|e| e.id.to_lowercase() == lower))
            .or_else(|| cat.iter().find(|e| e.id.to_lowercase() == format!("{lower}:latest")))
            .or_else(|| {
                let m: Vec<_> = cat.iter().filter(|e| e.id.to_lowercase().starts_with(&lower)).collect();
                (m.len() == 1).then(|| m[0])
            });
        match hit {
            Some(e) => Ok(Target::Llama(e.clone())),
            None => Err(Fail::not_found(format!(
                "no model \"{name}\" (model.list shows what is on this machine; ollama/<name> uses a running Ollama)"
            ))),
        }
    }

    // ---- loading ------------------------------------------------------------------------------------------

    pub async fn ensure_loaded(&self, entry: &ModelEntry, opts: &LoadOpts, explicit: bool) -> Result<Arc<Instance>> {
        let reload = explicit && opts.reload;
        let current = |i: &Instance| !(explicit && (opts.embeddings != i.embeddings || opts.reranking != i.reranking));
        if !reload {
            if let Some(i) = self.loaded.lock().get(&entry.id).cloned() {
                if current(&i) {
                    i.touch();
                    return Ok(i);
                }
            }
        }
        if !explicit && !self.settings.read().autoload {
            return Err(Fail::new(
                409,
                "not_loaded",
                format!("{} is not loaded (autoload is off)", entry.id),
            ));
        }
        let _guard = self.load_lock.lock().await;
        if !reload {
            if let Some(i) = self.loaded.lock().get(&entry.id).cloned() {
                if current(&i) {
                    return Ok(i);
                }
            }
            self.unload(&entry.id);
            self.make_room(opts.embeddings || opts.reranking);
        }
        // A reload keeps the old instance serving while the new one loads (both are in memory for a while), then
        // `install` switches over.
        let devices = self.fit_vram(entry, opts).await?;
        let t0 = Instant::now();
        let inst = Arc::new(self.load_with_fallback(entry, opts, &devices).await?);
        self.install(inst.clone());
        let mut m = self.metrics.lock();
        let mm = m.entry(entry.id.clone()).or_default();
        mm.loads += 1;
        mm.last_load_ms = t0.elapsed().as_millis() as u64;
        Ok(inst)
    }

    /// Put an instance into service as its model's one. One already there is retired: it takes no new requests
    /// (they find this one) and stops when those it is serving are done.
    fn install(&self, inst: Arc<Instance>) {
        let old = self.loaded.lock().insert(inst.model.clone(), inst);
        if let Some(old) = old {
            self.retire(old);
        }
    }

    /// Take an instance that is no longer in `loaded` out of service without dropping what it is serving.
    fn retire(&self, inst: Arc<Instance>) {
        inst.retired.store(true, Ordering::Release);
        if inst.inflight() == 0 {
            inst.stop();
        } else {
            self.draining.lock().push((Instant::now(), inst));
        }
    }

    /// Count a request on `inst`, if it is still the one in service (else the caller asks `ensure_loaded` again).
    pub fn lease(&self, inst: &Arc<Instance>) -> Option<Lease> {
        let loaded = self.loaded.lock();
        match loaded.get(&inst.model) {
            Some(cur) if Arc::ptr_eq(cur, inst) => {
                inst.inflight.fetch_add(1, Ordering::AcqRel);
                Some(Lease(inst.clone()))
            }
            _ => None,
        }
    }

    /// Use a llama-server that is already listening on 127.0.0.1:`port` as `model`'s instance, replacing (hot
    /// swapping) the one in service. For stand-ins in tests; `on_stop` runs when it is stopped.
    #[doc(hidden)]
    pub fn adopt(&self, model: &str, port: u16, key: &str, on_stop: Option<Box<dyn FnOnce() + Send>>) -> Arc<Instance> {
        self.adopt_as(model, port, key, false, on_stop)
    }

    /// [`adopt`](Self::adopt) for a stand-in that serves `/v1/embeddings` (`embeddings`) instead of chat.
    #[doc(hidden)]
    pub fn adopt_as(
        &self,
        model: &str,
        port: u16,
        key: &str,
        embeddings: bool,
        on_stop: Option<Box<dyn FnOnce() + Send>>,
    ) -> Arc<Instance> {
        let inst = Arc::new(Instance {
            model: model.to_string(),
            port,
            pid: 0,
            key: key.to_string(),
            child: Mutex::new(None),
            started: Instant::now(),
            args: vec![],
            log: PathBuf::new(),
            embeddings,
            reranking: false,
            opts: LoadOpts::default(),
            extras: Extras::default(),
            serving: RwLock::new(Serving::default()),
            last_used: AtomicU64::new(now_s()),
            keep: AtomicI64::new(0),
            inflight: AtomicUsize::new(0),
            retired: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            on_stop: Mutex::new(on_stop),
        });
        self.install(inst.clone());
        inst
    }

    /// Make room for one more model of the same class: chat models are counted against `max_loaded`, embedding and
    /// reranking ones against `max_loaded_embedding`, so one kind does not evict the other. The most idle of the
    /// class goes first.
    fn make_room(&self, embedding_class: bool) {
        let max = {
            let s = self.settings.read();
            if embedding_class {
                s.max_loaded_embedding
            } else {
                s.max_loaded
            }
            .max(1)
        };
        loop {
            let victim = {
                let l = self.loaded.lock();
                let same = || l.values().filter(|i| (i.embeddings || i.reranking) == embedding_class);
                if same().count() < max {
                    return;
                }
                same().max_by_key(|i| i.idle_s()).map(|i| i.model.clone())
            };
            match victim {
                Some(v) => {
                    self.unload(&v);
                }
                None => return,
            }
        }
    }

    /// A request's `keep_alive` for a loaded model: 0 unloads it now, a negative value keeps it forever, seconds
    /// set its idle-unload deadline from now. `None` goes back to the global `idle_unload_s`.
    pub fn set_keep_alive(&self, id: &str, secs: Option<i64>) {
        let Some(inst) = self.loaded.lock().get(id).cloned() else {
            return;
        };
        match secs {
            Some(0) => {
                self.unload(id);
            }
            Some(s) if s < 0 => inst.keep.store(-1, Ordering::Relaxed),
            Some(s) => inst.keep.store(now_s() as i64 + s, Ordering::Relaxed),
            None => inst.keep.store(0, Ordering::Relaxed),
        }
    }

    /// Take a model out of service. A request it is serving finishes first.
    pub fn unload(&self, id: &str) -> bool {
        let inst = self.loaded.lock().remove(id);
        match inst {
            Some(i) => {
                self.retire(i);
                true
            }
            None => false,
        }
    }

    /// Stop every model now, including those still finishing requests (shutdown).
    pub fn unload_all(&self) -> usize {
        let all: Vec<_> = self.loaded.lock().drain().map(|(_, i)| i).collect();
        let draining: Vec<_> = self.draining.lock().drain(..).map(|(_, i)| i).collect();
        for i in all.iter().chain(&draining) {
            i.retired.store(true, Ordering::Release);
            i.stop();
        }
        all.len()
    }

    // ---- serving profiles and the VRAM planner ------------------------------------------------------------

    /// The options a load of `entry` with `opts` ends up with (see [`serving`]).
    pub fn resolve_serving(&self, s: &Settings, entry: &ModelEntry, opts: &LoadOpts) -> Resolved {
        // a model Misty keeps on the CPU, unless the request or the model's profile says where its layers go
        let on_cpu = opts.gpu_layers.is_none()
            && s.profile_for(&entry.id).is_none_or(|p| p.gpu_layers.is_none())
            && self.cpu_hints.lock().contains(&entry.id);
        let mut r = Resolved::new(
            (
                opts.ctx_size,
                if on_cpu { Some(0) } else { opts.gpu_layers },
                opts.parallel,
                &opts.serve,
            ),
            s.profile_for(&entry.id),
            &s.serving,
            (s.ctx_size, s.gpu_layers, s.parallel),
        );
        // A ternary Bonsai file is served by the Prism build when it is installed, unless the profile names an engine:
        // that build runs every packing, and the pinned llama.cpp may not run any correctly.
        if r.flags.engine.is_none() && crate::misty::roster::pack_of(entry).is_some() && self.prism_server().is_some() {
            r.flags.engine = Some("prism".into());
        }
        r
    }

    /// What a model would be served with if autoload loaded it now, for `/v1/models`: the profile's context window
    /// and slots before anything is loaded.
    pub fn serving_preview(&self, entry: &ModelEntry) -> Serving {
        let s = self.settings();
        let r = self.resolve_serving(&s, entry, &LoadOpts::default());
        let mut v = Serving::from_resolved(&r);
        if r.ctx_total == 0 {
            // 0 = the model's own, divided between the slots as llama-server does
            let own = entry.context_length().unwrap_or(0);
            v.n_ctx = own;
            v.n_ctx_per_slot = if v.kv_unified { own } else { own / v.slots.max(1) };
        }
        v.source = "profile";
        v
    }

    /// The VRAM a load is expected to take (None: the GGUF header lacks what the estimate needs, or no layer is on
    /// the GPU).
    pub fn estimate_for(&self, entry: &ModelEntry, opts: &LoadOpts, r: &Resolved) -> Option<serving::VramEstimate> {
        let shape = serving::shape_from_meta(&entry.meta, entry.size_bytes)?;
        let own = entry.context_length().unwrap_or(4096) as u32;
        let ctx = if r.ctx_total == 0 { own } else { r.ctx_total };
        let extras = self.extras(&entry.id);
        let draft = extras
            .draft
            .as_ref()
            .filter(|d| d.gpu_layers.unwrap_or(r.gpu_layers) != 0 && !opts.embeddings && !opts.reranking)
            .and_then(|d| std::fs::metadata(&d.path).ok())
            .map(|m| m.len())
            .unwrap_or(0);
        let native_moe = entry.native.is_some() && entry.experts().is_some();
        // Rigel with paging keeps the routed experts out of the model's VRAM (they stream into a slot pool that
        // `moe_slots` sizes), so the estimate leaves them out as it does for --cpu-moe.
        let paged = r.flags.is_rigel() && r.flags.moe_direct_io == Some(true);
        let cpu_moe = match (opts.n_cpu_moe, opts.cpu_moe) {
            _ if paged => true,
            (Some(n), _) if n > 0 => false,
            (_, Some(c)) => c,
            _ => native_moe,
        };
        let placement = serving::Placement {
            ctx_total: ctx as u64,
            slots: r.slots as u64,
            gpu_layers: r.gpu_layers,
            cache_type_k: r.flags.cache_type_k.clone().unwrap_or_else(|| "f16".into()),
            cache_type_v: r.flags.cache_type_v.clone().unwrap_or_else(|| "f16".into()),
            ubatch: r.flags.ubatch_size.unwrap_or(512) as u64,
            cpu_moe,
            n_cpu_moe: opts.n_cpu_moe.unwrap_or(0) as u64,
            extra_gpu_bytes: draft,
        };
        Some(serving::estimate_vram(&shape, &placement)).filter(|e| e.total_mib > 0)
    }

    /// The GPUs and their free memory, from `llama-server --list-devices` (empty: a CPU build, or it cannot tell).
    pub async fn gpu_devices(&self) -> Vec<serving::Device> {
        #[cfg(test)]
        if let Some(d) = self.fake_devices.lock().clone() {
            return d;
        }
        let Some(exe) = self.llama_server() else {
            return vec![];
        };
        let mut cmd = tokio::process::Command::new(&exe);
        cmd.arg("--list-devices")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        if let Some(dir) = exe.parent() {
            cmd.current_dir(dir);
        }
        no_window(&mut cmd);
        let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(30), cmd.output()).await else {
            return vec![];
        };
        let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        let mut devices = serving::parse_devices(&text);
        // llama-server's "free" does not see memory other processes hold (measured: 18.5 GB taken, 5 GB fewer
        // reported). With one GPU, Windows' own counter of dedicated memory in use is the truth.
        if let [d] = &mut devices[..] {
            if let Some(used) = typeperf(r"\GPU Adapter Memory(*)\Dedicated Usage")
                .await
                .and_then(|t| serving::adapter_used_mib(&t))
            {
                d.free_mib = d.total_mib.saturating_sub(used);
            }
        }
        devices
    }

    /// [`gpu_devices`](Self::gpu_devices) for a listing: the cards as probed in the last 20 seconds.
    pub async fn gpu_devices_recent(&self) -> Vec<serving::Device> {
        #[cfg(test)]
        if self.fake_devices.lock().is_some() {
            return self.gpu_devices().await;
        }
        if let Some((at, d)) = &*self.device_cache.lock() {
            if at.elapsed() < Duration::from_secs(20) {
                return d.clone();
            }
        }
        let d = self.gpu_devices().await;
        *self.device_cache.lock() = Some((Instant::now(), d.clone()));
        d
    }

    /// Whether the program that serves `r` can use a GPU. The stock llama.cpp builds can when they see a card. The
    /// PrismML build `misty setup` installs is the CPU one; another build is taken for a GPU build when its folder
    /// says so.
    fn can_use_gpu(&self, r: &Resolved, devices: &[serving::Device]) -> bool {
        if r.flags.is_prism() {
            // the GPU build (`misty setup --install prism-gpu`), or a Prism program set by hand whose path says it is one
            let named_gpu = |p: Option<PathBuf>| {
                p.map(|p| p.display().to_string().to_lowercase())
                    .is_some_and(|p| ["vulkan", "hip", "cuda", "metal"].iter().any(|k| p.contains(k)))
            };
            return (self.prism_gpu_server().is_some() && !devices.is_empty()) || named_gpu(self.prism_server());
        }
        !devices.is_empty()
    }

    /// The hardware label of a model for listings (`model.list`, `/v1/models`): the engine, what it could run on,
    /// where it runs now (loaded) or would go (not loaded), and the memory it takes or is expected to take.
    pub fn hardware_json(&self, entry: &ModelEntry, devices: &[serving::Device]) -> Value {
        let s = self.settings();
        let opts = LoadOpts::default();
        let r = self.resolve_serving(&s, entry, &opts);
        let engine = Serving::from_resolved(&r).engine;
        let gpu_ok = self.can_use_gpu(&r, devices);
        let file_mib = entry.size_bytes / (1024 * 1024);
        let loaded = self.loaded().into_iter().find(|i| i.model == entry.id);
        let (runs_on, gpu_share, vram, ram): (&str, Option<f64>, Option<u64>, Option<u64>) = if let Some(i) = &loaded {
            let sv = i.serving();
            match sv.placement {
                "gpu" => ("gpu", Some(1.0), sv.vram_mib.map(|v| v.round() as u64), Some(0)),
                "partial" => ("split", None, sv.vram_mib.map(|v| v.round() as u64), None),
                "cpu" => ("cpu", Some(0.0), Some(0), Some(file_mib)),
                _ => ("unknown", None, sv.vram_mib.map(|v| v.round() as u64), None),
            }
        } else if r.gpu_layers == 0 || !gpu_ok {
            ("cpu", Some(0.0), Some(0), Some(file_mib))
        } else {
            let est = self.estimate_for(entry, &opts, &r);
            match self.placement_for(entry, &opts, devices) {
                Some(serving::Fit::Gpu) | Some(serving::Fit::GpuAfter(..)) => {
                    ("gpu", Some(1.0), est.map(|e| e.total_mib), Some(0))
                }
                Some(serving::Fit::Split { gpu_share }) => (
                    "split",
                    Some((gpu_share * 100.0).round() / 100.0),
                    est.map(|e| (e.total_mib as f64 * gpu_share).round() as u64),
                    Some((file_mib as f64 * (1.0 - gpu_share)).round() as u64),
                ),
                Some(serving::Fit::Cpu) => ("cpu", Some(0.0), Some(0), Some(file_mib)),
                Some(serving::Fit::No { .. }) => ("none", None, est.map(|e| e.total_mib), None),
                // no estimate (the header lacks the shape): the GPU is tried first, as a load would
                None => ("gpu", None, None, None),
            }
        };
        let chip = match (runs_on, gpu_share) {
            ("split", Some(g)) => format!("GPU {}% + CPU", (g * 100.0).round()),
            ("split", None) => "GPU + CPU".to_string(),
            ("gpu", _) => "GPU".to_string(),
            ("cpu", _) => "CPU".to_string(),
            ("none", _) => "does not fit".to_string(),
            _ => "?".to_string(),
        };
        json!({
            "engine": engine,
            "can_run_on": if gpu_ok { json!(["gpu", "cpu"]) } else { json!(["cpu"]) },
            "runs_on": runs_on,
            "now": loaded.is_some(),
            "gpu_share": gpu_share,
            "vram_mib": vram,
            "ram_mib": ram,
            "device": devices.first().map(|d| d.name.clone()),
            "chip": chip,
            "label": format!("{} · {chip}", entry.label()),
        })
    }

    /// Plans a load against the GPU's free memory: the estimate, the devices, and what to do.
    pub async fn plan_load(
        &self,
        entry: &ModelEntry,
        opts: &LoadOpts,
    ) -> (Option<serving::VramEstimate>, Vec<serving::Device>, Plan) {
        let devices = self.gpu_devices().await;
        let (est, plan) = self.plan_for(entry, opts, &devices);
        (est, devices, plan)
    }

    /// [`plan_load`](Self::plan_load) for devices already listed.
    pub fn plan_for(
        &self,
        entry: &ModelEntry,
        opts: &LoadOpts,
        devices: &[serving::Device],
    ) -> (Option<serving::VramEstimate>, Plan) {
        let s = self.settings();
        let r = self.resolve_serving(&s, entry, opts);
        let Some(est) = self.estimate_for(entry, opts, &r) else {
            return (None, Plan::Fits);
        };
        if devices.is_empty() {
            return (Some(est), Plan::Fits);
        }
        let free = self.plannable_free(devices, &s, &entry.id);
        let plan = serving::plan(est.total_mib, free, s.vram_margin_mib, &self.residents(&entry.id));
        (Some(est), plan)
    }

    /// The free VRAM a load is planned against: what the cards have free, held to what models may use of them (each
    /// card's total less `vram_reserve_mib`, which stays free for the system, less what the other loaded models hold).
    pub fn plannable_free(&self, devices: &[serving::Device], s: &Settings, except: &str) -> u64 {
        let others: u64 = self.residents(except).iter().map(|r| r.vram_mib).sum();
        let free = serving::plannable_free_mib(devices, s.vram_reserve_mib, others, s.vram_margin_mib);
        // on Windows the device memory of a load is charged against the system's commit as well
        match self.commit_budget_mib() {
            Some(c) => free.min(c.saturating_add(s.vram_margin_mib)),
            None => free,
        }
    }

    /// The models that are to stay on the CPU (replaces the earlier set). A model already loaded is not moved: this
    /// decides its next load.
    pub fn set_cpu_hints(&self, models: impl IntoIterator<Item = String>) {
        *self.cpu_hints.lock() = models.into_iter().collect();
    }

    /// The video memory models may use in all, in MiB: what the cards have free plus what the loaded models already
    /// hold, less the reserve kept for the system on each card. Unlike [`plannable_free`](Self::plannable_free) it
    /// does not shrink when a model is loaded, so a model that is loaded still fits (Misty's roster asks this).
    pub fn gpu_room_mib(&self, devices: &[serving::Device]) -> u64 {
        let reserve = self.settings().vram_reserve_mib;
        let held: u64 = self.residents("").iter().map(|r| r.vram_mib).sum();
        let free: u64 = devices.iter().map(|d| d.free_mib).sum();
        let usable: u64 = devices.iter().map(|d| d.total_mib.saturating_sub(reserve)).sum();
        free.saturating_add(held).min(usable)
    }

    /// What holds a GPU load below the card's free memory, for `model.plan`: the reserve kept for the system, what
    /// models may use, the system's commit headroom and what it allows, and what the card has shown it can hold.
    pub fn memory_json(&self, devices: &[serving::Device], r: &Resolved, model: &str) -> Value {
        let s = self.settings();
        let limit = self.gpu_limit(devices, r);
        json!({
            "reserve_mib": s.vram_reserve_mib,
            "usable_mib": (!devices.is_empty()).then(|| self.plannable_free(devices, &s, model)),
            "commit_headroom_mib": self.commit_headroom_mib(),
            "commit_budget_mib": self.commit_budget_mib(),
            "learned_budget_mib": self.learned_budget_mib(devices, r),
            "gpu_limit": limit,
        })
    }

    /// What a load must do about VRAM, given the devices: Ok(None) = go ahead, Ok(Some(models)) = unload these
    /// first, Err = refuse.
    pub fn vram_decision(
        &self,
        entry: &ModelEntry,
        opts: &LoadOpts,
        devices: &[serving::Device],
    ) -> Result<Option<(Vec<String>, u64)>> {
        let (est, plan) = self.plan_for(entry, opts, devices);
        let Some(est) = est else { return Ok(None) };
        let free = self.plannable_free(devices, &self.settings(), &entry.id);
        match plan {
            Plan::Fits => Ok(None),
            Plan::Evict(models) => Ok(Some((models, est.total_mib + self.settings().vram_margin_mib))),
            Plan::Refuse { reclaimable_mib } => Err(Fail::new(
                503,
                "insufficient_vram",
                format!(
                    concat!(
                        "{} needs about {} MiB of VRAM (weights {}, KV cache {}, state {}, compute {}) and the GPU ",
                        "has {} MiB for models (its free memory, less the vram_reserve_mib kept for the system) with ",
                        "{} MiB more in models that could be unloaded. Lower ctx_size or ",
                        "parallel, use a smaller KV-cache type (cache_type_k/cache_type_v), or load with ",
                        "skip_vram_check: true to try anyway."
                    ),
                    entry.id,
                    est.total_mib,
                    est.weights_mib,
                    est.kv_mib,
                    est.state_mib,
                    est.compute_mib,
                    free,
                    reclaimable_mib
                ),
            )),
        }
    }

    /// Before a load: make sure it has somewhere to go. A load whose GPU layers are the hub's to choose (`gpu_layers`
    /// 999, and a llama-server that fits them itself) is placed: all on the GPU, after unloading idle models if that
    /// makes it fit; else split between the GPU and RAM; else on the CPU; and only refused when RAM cannot hold the
    /// rest either. A load with a fixed number of layers must fit the GPU's free VRAM (idle models are unloaded for
    /// it) or is refused. Does nothing when the guard is off, the load says to skip it, or the estimate or devices are
    /// unknown. Returns the GPUs that were listed (none: a CPU build, or nothing to list).
    async fn fit_vram(&self, entry: &ModelEntry, opts: &LoadOpts) -> Result<Vec<serving::Device>> {
        let s = self.settings();
        let r = self.resolve_serving(&s, entry, opts);
        if r.gpu_layers == 0 {
            return Ok(vec![]);
        }
        let devices = self.gpu_devices().await;
        if !s.vram_guard || opts.skip_vram_check {
            return Ok(devices);
        }
        let decision = if r.is_auto() && self.fits_itself(&r).await {
            self.placement_decision(entry, opts, &devices)?
        } else {
            self.vram_decision(entry, opts, &devices)?
        };
        let Some((models, want)) = decision else {
            return Ok(devices);
        };
        for m in &models {
            self.unload(m);
        }
        // an unloaded server gives its memory back a moment later
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let now: u64 = self.gpu_devices().await.iter().map(|d| d.free_mib).sum();
            if now >= want {
                return Ok(devices);
            }
            if Instant::now() > deadline {
                return Err(Fail::new(
                    503,
                    "insufficient_vram",
                    format!(
                        "{} needs about {} MiB of VRAM; after unloading {} the GPU has {now} MiB free",
                        entry.id,
                        want,
                        models.join(", ")
                    ),
                ));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// The servers that are loading now, with how much of the file each has read (its memory in use, which grows as the
    /// file is read) and when it should be done (see [`serving::load_progress`]).
    pub fn loading(&self) -> Vec<Value> {
        use sysinfo::{Pid, System};
        let list: Vec<(String, u32, u64, u64)> = self
            .loading
            .lock()
            .iter()
            .map(|l| (l.model.clone(), l.pid, l.size_mib, l.started.elapsed().as_secs()))
            .collect();
        if list.is_empty() {
            return vec![];
        }
        let mut sys = System::new();
        list.into_iter()
            .map(|(model, pid, size_mib, elapsed_s)| {
                let pid = Pid::from_u32(pid);
                sys.refresh_process(pid);
                let read_mib = sys.process(pid).map(|p| p.memory() / (1024 * 1024)).unwrap_or(0);
                let mut v = serving::load_progress(size_mib, read_mib, elapsed_s);
                v["model"] = json!(model);
                v["elapsed_s"] = json!(elapsed_s);
                v
            })
            .collect()
    }

    /// The models that are loaded, as the planner sees them (the model being loaded is left out).
    fn residents(&self, except: &str) -> Vec<Resident> {
        self.loaded()
            .iter()
            .filter(|i| i.model != except)
            .map(|i| {
                let sv = i.serving();
                Resident {
                    model: i.model.clone(),
                    idle_s: i.idle_s(),
                    // a model measured on the CPU frees no VRAM when it is unloaded, whatever was estimated for it
                    vram_mib: if sv.placement == "cpu" {
                        0
                    } else {
                        sv.vram_mib
                            .map(|v| v.round() as u64)
                            .or(sv.estimated_vram_mib)
                            .unwrap_or(0)
                    },
                    busy: i.inflight() > 0,
                }
            })
            .collect()
    }

    /// Where a load with automatic GPU layers goes (see [`serving::place`]), with the numbers it used.
    pub fn placement_for(
        &self,
        entry: &ModelEntry,
        opts: &LoadOpts,
        devices: &[serving::Device],
    ) -> Option<serving::Fit> {
        let s = self.settings();
        let r = self.resolve_serving(&s, entry, opts);
        let est = self.estimate_for(entry, opts, &r)?;
        if devices.is_empty() {
            return None;
        }
        let mut free = self.plannable_free(devices, &s, &entry.id);
        // a card that has run out of memory before is not counted on for more than it has held
        if let Some(b) = self.learned_budget_mib(devices, &r) {
            free = free.min(b.saturating_add(s.vram_margin_mib));
        }
        Some(serving::place(
            &est,
            free,
            s.vram_margin_mib,
            &self.residents(&entry.id),
            free_ram_mib(),
        ))
    }

    /// `vram_decision` for a load whose GPU layers are chosen automatically: Ok(None) = go ahead, Ok(Some(models)) =
    /// unload these first, Err = not even the CPU side fits.
    pub fn placement_decision(
        &self,
        entry: &ModelEntry,
        opts: &LoadOpts,
        devices: &[serving::Device],
    ) -> Result<Option<(Vec<String>, u64)>> {
        match self.placement_for(entry, opts, devices) {
            None | Some(serving::Fit::Gpu) | Some(serving::Fit::Split { .. }) | Some(serving::Fit::Cpu) => Ok(None),
            Some(serving::Fit::GpuAfter(models, want)) => Ok(Some((models, want))),
            Some(serving::Fit::No {
                need_ram_mib,
                free_ram_mib,
            }) => {
                let free = self.plannable_free(devices, &self.settings(), &entry.id);
                Err(Fail::new(
                    503,
                    "insufficient_memory",
                    format!(
 "{} does not fit this machine as asked: with the {free} MiB of VRAM that models may use it would still need about {need_ram_mib} MiB of RAM, and {free_ram_mib} MiB are available. Unload other models, lower ctx_size or parallel, use a smaller KV-cache type (cache_type_k/cache_type_v), or take a smaller quantization.",
                        entry.id
                    ),
                ))
            }
        }
    }

    /// The `-t` for a server that is about to start (see [`serving::thread_budget`]), from what is loaded now.
    async fn thread_share(&self, entry: &ModelEntry, opts: &LoadOpts, r: &Resolved) -> Option<u32> {
        let (mut gpu, mut cpu) = (0, 0);
        for i in self.loaded().iter().filter(|i| i.model != entry.id) {
            match i.serving().placement {
                "gpu" => gpu += 1,
                "cpu" => cpu += 1,
                _ => {}
            }
        }
        let uses_gpu = r.gpu_layers != 0;
        let wholly = uses_gpu && {
            let devices = self.gpu_devices_recent().await;
            matches!(self.placement_for(entry, opts, &devices), Some(serving::Fit::Gpu))
        };
        serving::thread_budget(crate::machine::physical_cores(), uses_gpu, wholly, gpu, cpu)
    }

    /// Whether llama-server can choose the GPU layers itself (`--fit`): the stock llama.cpp, in a build that has it.
    async fn fits_itself(&self, r: &Resolved) -> bool {
        if r.flags.is_rigel() || r.flags.is_prism() {
            return false;
        }
        let Some(exe) = self.llama_server() else {
            return false;
        };
        self.server_has_flag(&exe, "--fit").await
    }

    /// Whether `exe --help` lists `flag`. Only a real answer is remembered.
    async fn server_has_flag(&self, exe: &Path, flag: &str) -> bool {
        let key = format!("{}|{flag}", exe.display());
        if let Some(known) = self.help_flags.lock().get(&key).copied() {
            return known;
        }
        let mut cmd = tokio::process::Command::new(exe);
        cmd.arg("--help").stdin(std::process::Stdio::null()).kill_on_drop(true);
        if let Some(dir) = exe.parent() {
            cmd.current_dir(dir);
        }
        no_window(&mut cmd);
        let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(20), cmd.output()).await else {
            return false;
        };
        let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        if text.trim().is_empty() {
            return false;
        }
        let has = text.contains(flag);
        self.help_flags.lock().insert(key, has);
        has
    }

    /// Once a server is up: what it says about itself (`/props`) and the memory it took (its log).
    async fn learn_serving(&self, inst: &Instance, estimate: Option<u64>) {
        let props: Option<Value> = match self
            .http
            .get(format!("http://127.0.0.1:{}/props", inst.port))
            .bearer_auth(&inst.key)
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r.json().await.ok(),
            _ => None,
        };
        let log = std::fs::read(&inst.log)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let process = if inst.pid > 0 {
            typeperf(&format!(r"\GPU Process Memory(pid_{}_*)\Dedicated Usage", inst.pid))
                .await
                .and_then(|t| serving::process_used_mib(&t))
        } else {
            None
        };
        let mut sv = inst.serving.write();
        if let Some(p) = props {
            sv.apply_props(&p);
        }
        sv.vram_mib = process.map(|m| m as f64).or_else(|| serving::vram_from_log(&log));
        sv.estimated_vram_mib = estimate;
    }

    /// The server program a load with `r` runs.
    fn server_for(&self, r: &Resolved) -> Option<PathBuf> {
        if r.flags.is_prism() {
            // the GPU build when layers go to the GPU, the CPU build when none do (and as the stand-in for the other)
            if r.gpu_layers != 0 {
                self.prism_gpu_server().or_else(|| self.prism_server())
            } else {
                self.prism_server().or_else(|| self.prism_gpu_server())
            }
        } else if r.flags.is_rigel() {
            self.rigel_server()
        } else {
            self.llama_server()
        }
    }

    /// Names the server program a placement was learned with (path, size, date): another program, or an update,
    /// gets a new try on the GPU.
    fn engine_key(&self, r: &Resolved) -> String {
        let Some(p) = self.server_for(r) else {
            return String::new();
        };
        let meta = std::fs::metadata(&p).ok();
        let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let when = meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!("{}|{len}|{when}", p.display())
    }

    /// Why this model goes to the CPU, if an earlier load of it crashed on the GPU with this same server program.
    pub fn learned_why(&self, entry: &ModelEntry, r: &Resolved) -> Option<String> {
        let key = self.engine_key(r);
        self.learned
            .lock()
            .get(&entry.id)
            .filter(|l| !key.is_empty() && l.engine == key)
            .map(|l| l.why.clone())
    }

    fn learn(&self, entry: &ModelEntry, r: &Resolved, why: String) {
        let mut all = self.learned.lock();
        all.insert(
            entry.id.clone(),
            Learned {
                engine: self.engine_key(r),
                why,
                at: now_s(),
            },
        );
        save_learned(&self.home, &all);
    }

    fn forget(&self, id: &str) {
        let mut all = self.learned.lock();
        if all.remove(id).is_some() {
            save_learned(&self.home, &all);
        }
    }

    // ---- what the GPU really lets a process hold ----------------------------------------------------------------

    /// The one card a load is planned against. With several, the free memory is a sum that no single process is held
    /// to, so nothing is learned about them and no limit is applied.
    fn the_card(devices: &[serving::Device]) -> Option<&serving::Device> {
        match devices {
            [d] => Some(d),
            _ => None,
        }
    }

    /// The key a card's limit is kept under: the device and the server program (another build may behave differently).
    fn limit_key(&self, d: &serving::Device, r: &Resolved) -> String {
        format!("{}|{}", d.name, self.engine_key(r))
    }

    /// What the card has shown it lets a process hold (a failure is believed for a day, see [`serving::GpuLimit`]).
    pub fn gpu_limit(&self, devices: &[serving::Device], r: &Resolved) -> Option<serving::GpuLimit> {
        let d = Self::the_card(devices)?;
        self.limits.lock().get(&self.limit_key(d, r)).cloned()
    }

    /// What loads on the card are held to for now: the learned limit, if a load failed lately.
    pub fn learned_budget_mib(&self, devices: &[serving::Device], r: &Resolved) -> Option<u64> {
        self.gpu_limit(devices, r).and_then(|l| l.budget_mib(now_s()))
    }

    /// The system's commit headroom, in MiB (None: not Windows, so no such limit; a test sets it).
    pub fn commit_headroom_mib(&self) -> Option<u64> {
        #[cfg(test)]
        {
            *self.fake_commit.lock()
        }
        #[cfg(not(test))]
        {
            crate::machine::commit_headroom_mib()
        }
    }

    /// The most device memory a load can take with the commit headroom the system has (None: no such limit here).
    pub fn commit_budget_mib(&self) -> Option<u64> {
        self.commit_headroom_mib()
            .map(crate::machine::gpu_budget_from_commit_mib)
    }

    /// A load of `need_mib` of device memory ran out of memory: whether the system's commit explains it, in words.
    fn commit_hint(&self, need_mib: Option<u64>) -> Option<String> {
        let headroom = self.commit_headroom_mib()?;
        let want = crate::machine::commit_for_gpu_load_mib(need_mib?) + crate::machine::COMMIT_MARGIN_MIB;
        (headroom < want).then(|| {
            format!(
                "Windows has only {headroom} MiB of memory commit left and this load needs about {want} MiB of it (the programs that are open hold the rest, and the page files are the limit): close programs, or give Windows a larger page file"
            )
        })
    }

    /// A load that asked `asked_mib` of the card ran out of memory. Returns what the next load should be held to.
    fn note_gpu_failure(&self, d: &serving::Device, r: &Resolved, asked_mib: u64) -> Option<u64> {
        let now = now_s();
        let mut all = self.limits.lock();
        let l = all.entry(self.limit_key(d, r)).or_default();
        let learned = l.failed(asked_mib, now);
        let budget = l.budget_mib(now);
        if learned {
            save_limits(&self.home, &all);
        }
        budget
    }

    /// A server loaded on the card and holds `held_mib` of its memory.
    fn note_gpu_success(&self, devices: &[serving::Device], r: &Resolved, held_mib: f64) {
        let Some(d) = Self::the_card(devices) else { return };
        if held_mib < 128.0 {
            return;
        }
        let mut all = self.limits.lock();
        let l = all.entry(self.limit_key(d, r)).or_default();
        if l.worked(held_mib.round() as u64) {
            save_limits(&self.home, &all);
        }
    }

    /// The first try of a load, when something holds the card to less than the load needs: the reserve kept for the
    /// system, the system's commit headroom, or a load of this size having run out of memory on the card lately. The
    /// load is held to what is left, and the rest of the model stays in RAM. None: nothing limits it, or what would
    /// be left for RAM does not fit (the load as asked then fails with the reason, which is better than a load that
    /// cannot work).
    fn held_first_try(
        &self,
        s: &Settings,
        r: &Resolved,
        ground: &Ground<'_>,
        ignore_learned: bool,
    ) -> Option<(Try, String)> {
        let d = Self::the_card(ground.devices)?;
        let need = ground.need_mib?;
        let others: u64 = self.residents(&ground.model).iter().map(|x| x.vram_mib).sum();
        let mut budget = d.total_mib.saturating_sub(s.vram_reserve_mib).saturating_sub(others);
        let mut why = format!("{} MiB of the card are kept for the system, so", s.vram_reserve_mib);
        if let Some(c) = self.commit_budget_mib() {
            if c < budget {
                budget = c;
                why = format!(
                    "Windows has {} MiB of memory commit left and a GPU load is charged about a fifth more than its device memory, so",
                    self.commit_headroom_mib().unwrap_or(0)
                );
            }
        }
        if let Some(l) = self.learned_budget_mib(ground.devices, r).filter(|_| !ignore_learned) {
            if l < budget {
                budget = l;
                why = format!("this card has not held more than about {l} MiB for one program lately, so");
            }
        }
        if budget >= need || budget < serving::MIN_SPLIT_MIB {
            return None;
        }
        let in_ram = need - budget;
        if in_ram + serving::RAM_OVERHEAD_MIB > free_ram_mib() {
            return None;
        }
        let target = serving::fit_target_for(d.free_mib, budget, s.vram_margin_mib.max(512));
        Some((
            Try::Held { target, budget },
            format!("{why} about {in_ram} MiB of the model stays in RAM"),
        ))
    }

    /// After a load ran out of memory: a split held below the ask that failed (at most [`serving::MAX_SPLIT_TRIES`]
    /// times), when the cards are known and the part that stays in RAM fits there. None: nothing to go by, or the
    /// split cannot work, and the ladder goes on.
    fn split_after_oom(
        &self,
        s: &Settings,
        r: &Resolved,
        how: &Try,
        ground: &Ground<'_>,
        tries: usize,
    ) -> Option<(Try, String)> {
        let d = Self::the_card(ground.devices)?;
        let need = ground.need_mib?;
        if tries >= serving::MAX_SPLIT_TRIES {
            return None;
        }
        let floor = s.vram_margin_mib.max(512);
        let target = match how {
            Try::Asked => floor,
            Try::Roomier(t) => *t,
            Try::Held { target, .. } => *target,
            Try::Cpu => return None,
        };
        let asked = serving::asked_of_card(need, d.free_mib, target);
        let mut budget = self.note_gpu_failure(d, r, asked)?;
        // the system's commit may be what ran out: what is left of it now is the better figure
        if let Some(c) = self.commit_budget_mib() {
            budget = budget.min(c);
        }
        if budget < serving::MIN_SPLIT_MIB || budget >= asked {
            return None;
        }
        let in_ram = need.saturating_sub(budget);
        if in_ram + serving::RAM_OVERHEAD_MIB > free_ram_mib() {
            return None;
        }
        Some((
            Try::Held {
                target: serving::fit_target_for(d.free_mib, budget, floor),
                budget,
            },
            format!(
                "the GPU ran out of memory with {asked} MiB asked of it, so the load was held to {budget} MiB and about {in_ram} MiB of the model stays in RAM"
            ),
        ))
    }

    /// Starts the server for a load. A card that is known to hold less than the load needs (it ran a load out of
    /// memory before, or the reserve for the system leaves it less) is asked for what it can give, and the rest of the
    /// model stays in RAM. When the GPU is what stops a load (out of memory, or a driver crash that kills the server)
    /// and `gpu_fallback` is on, it tries again: held lower each time, then on the CPU. A crash is remembered, so that
    /// the next load of that model goes straight to the CPU (`retry_gpu` tries the GPU again); what the card held is
    /// remembered too, so that the next load of a big model starts at what works.
    async fn load_with_fallback(
        &self,
        entry: &ModelEntry,
        opts: &LoadOpts,
        devices: &[serving::Device],
    ) -> Result<Instance> {
        let s = self.settings();
        let r = self.resolve_serving(&s, entry, opts);
        let counted = !devices.is_empty();
        let ground = Ground {
            devices,
            need_mib: self.estimate_for(entry, opts, &r).map(|e| e.total_mib),
            model: entry.id.clone(),
        };
        let mut how = Try::Asked;
        let mut note: Option<String> = None;
        if r.is_auto() && s.gpu_fallback && !opts.retry_gpu {
            if let Some(why) = self.learned_why(entry, &r) {
                how = Try::Cpu;
                note = Some(format!(
                    "on the CPU: {why}. Load it with retry_gpu: true to try the GPU again"
                ));
            }
        }
        if how == Try::Asked && r.is_auto() && s.vram_guard && !opts.skip_vram_check && self.fits_itself(&r).await {
            if let Some((held, why)) = self.held_first_try(&s, &r, &ground, opts.retry_gpu) {
                how = held;
                note = Some(why);
            }
        }
        let mut history: Vec<String> = vec![];
        let mut crash_why: Option<String> = None;
        loop {
            match self.spawn(entry, opts, &how, counted).await {
                Ok(inst) => {
                    if let (Some(why), Try::Cpu) = (&crash_why, &how) {
                        self.learn(entry, &r, why.clone());
                    } else if opts.retry_gpu && how != Try::Cpu {
                        self.forget(&entry.id);
                    }
                    if how != Try::Cpu {
                        if let Some(v) = inst.serving().vram_mib {
                            self.note_gpu_success(devices, &r, v);
                        }
                    }
                    if !history.is_empty() {
                        let tail = if how == Try::Cpu { "; it runs on the CPU" } else { "" };
                        note = Some(format!("{}{tail}", history.join("; ")));
                    }
                    if note.is_some() {
                        inst.serving.write().placement_note = note;
                    }
                    return Ok(inst);
                }
                Err(mut f) => {
                    let Some((next, why, crash)) = self
                        .after_failure(&s, &r, entry, &how, &f, &ground, history.len())
                        .await
                    else {
                        if !history.is_empty() {
                            f.message = format!("{} (before this: {})", f.message, history.join("; "));
                        }
                        // on Windows the system's commit often explains a GPU that "ran out of memory" with room to spare
                        let ran_out = f.fields["gpu_failure"].as_str() == Some("memory")
                            || history.iter().any(|h| h.contains("ran out of memory"));
                        if ran_out {
                            if let Some(hint) = self.commit_hint(ground.need_mib) {
                                f.message = format!("{} ({hint})", f.message);
                            }
                        }
                        return Err(f);
                    };
                    if crash {
                        crash_why = Some(why.clone());
                    }
                    history.push(why);
                    how = next;
                }
            }
        }
    }

    /// What to try after a failed load: None = the failure is the answer. Only a failure that the GPU caused is
    /// retried. Out of memory first gets a split, held below what failed, where the cards are known (`tries` is how
    /// many retries came before); where they are not, llama-server is asked to keep a fixed 2 GiB more free. After
    /// that the CPU, and only when the CPU's memory can hold the model. The last field says the server crashed.
    #[allow(clippy::too_many_arguments)]
    async fn after_failure(
        &self,
        s: &Settings,
        r: &Resolved,
        entry: &ModelEntry,
        how: &Try,
        f: &Fail,
        ground: &Ground<'_>,
        tries: usize,
    ) -> Option<(Try, String, bool)> {
        if !s.gpu_fallback || r.gpu_layers == 0 || *how == Try::Cpu {
            return None;
        }
        let kind = f.fields["gpu_failure"].as_str()?;
        if kind == "memory" && r.is_auto() && self.fits_itself(r).await {
            if let Some((next, why)) = self.split_after_oom(s, r, how, ground, tries) {
                return Some((next, why, false));
            }
            if *how == Try::Asked {
                let target = s.vram_margin_mib.max(512) + 2048;
                return Some((
                    Try::Roomier(target),
                    format!("the GPU ran out of memory, so llama-server was asked to keep {target} MiB free"),
                    false,
                ));
            }
        }
        // the CPU has to hold the weights it is given: if it cannot, the GPU's error is the answer
        if entry.size_bytes / (1024 * 1024) + 1024 > free_ram_mib() {
            return None;
        }
        let crash = kind == "crash";
        let cause = if crash {
            let code = f.fields["exit_code"]
                .as_i64()
                .map(|c| format!(" (exit {:#X})", c as i32 as u32));
            format!("its load on the GPU crashed llama-server{}", code.unwrap_or_default())
        } else {
            "the GPU ran out of memory".to_string()
        };
        Some((Try::Cpu, cause, crash))
    }

    async fn spawn(&self, entry: &ModelEntry, opts: &LoadOpts, how: &Try, gpu_counted: bool) -> Result<Instance> {
        let mut resolved = self.resolve_serving(&self.settings(), entry, opts);
        resolved
            .validate()
            .map_err(|e| Fail::invalid(format!("{}: {e}", entry.id)))?;
        if *how == Try::Cpu {
            resolved.gpu_layers = 0;
        }
        let rigel = resolved.flags.is_rigel();
        if !resolved.flags.is_prism() && !rigel {
            // ternary weights on a llama.cpp that predates their kernels load without an error and answer wrongly
            if let Some(pack) = crate::misty::roster::pack_of(entry) {
                let stock_ok = pack == "Q2_0"
                    && self
                        .llama_version()
                        .await
                        .and_then(|v| crate::misty::parse_build(&v))
                        .is_some_and(|b| b >= crate::misty::STOCK_Q2_BUILD);
                if !stock_ok {
                    return Err(Fail::new(
                        409,
                        "engine_cannot_run",
                        format!(
                            "{}: this {pack} file needs the PrismML llama.cpp build; the llama.cpp ModelMistress uses is older than                              the kernels these weights need and would answer wrongly without any warning. Install it with                              `model-mistress misty setup --install prism`",
                            entry.id
                        ),
                    ));
                }
            }
        }
        let exe = if resolved.flags.is_prism() {
            self.server_for(&resolved).ok_or_else(|| {
                Fail::unavailable(format!(
                    "{}: its profile says engine = \"prism\", but the Prism llama-server was not found: run                      `model-mistress misty setup`, or set prism_server with config.set",
                    entry.id
                ))
            })?
        } else if rigel {
            self.rigel_server().ok_or_else(|| {
                Fail::unavailable(format!(
                    "{}: its profile says engine = \"rigel\", but rigel-server was not found: set rigel_server with \
                     config.set, put it in <home>/engines/rigel, or build ModelMistress with the rigel-engine feature",
                    entry.id
                ))
            })?
        } else {
            self.llama_server().ok_or_else(|| {
                Fail::unavailable(
                    "llama-server was not found: set llama_server with config.set, put it in <home>/engines/llama.cpp, \
                     or on PATH",
                )
            })?
        };
        let s = self.settings();
        // llama-server chooses the number of GPU layers itself where it can: it measures what is free on each device,
        // which is more exact than a header-based estimate, and it moves experts of a mixture-of-experts model to RAM.
        if resolved.is_auto() && self.fits_itself(&resolved).await {
            resolved.fit_target_mib = Some(match how {
                Try::Roomier(mib) => *mib,
                Try::Held { target, .. } => *target,
                _ => s.vram_margin_mib.max(512),
            });
        }
        let port = free_port().map_err(|e| Fail::new(500, "io", format!("no free port: {e}")))?;
        let key = crate::hub::new_token();
        let extras = self.extras(&entry.id);
        let mut args: Vec<String> = vec![
            "-m".into(),
            extras.model_path.as_ref().unwrap_or(&entry.path).display().to_string(),
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            port.to_string(),
            "-a".into(),
            entry.id.clone(),
        ];
        args.extend(resolved.args());
        // nothing on the GPU: leave the GPU backend out altogether, so a driver that crashes the server is not touched
        // (`-ngl 0` alone still starts it)
        if resolved.gpu_layers == 0
            && !rigel
            && !resolved.flags.is_prism()
            && self.server_has_flag(&exe, "--device").await
        {
            args.extend(["-dev".into(), "none".into()]);
        }
        args.push("--metrics".into());
        let user_threads =
            opts.threads.is_some() || serving::sets_threads(&s.extra_args) || serving::sets_threads(&opts.extra_args);
        if let Some(t) = opts.threads {
            args.extend(["-t".into(), t.to_string()]);
        } else if s.thread_budget && !user_threads {
            if let Some(t) = self.thread_share(entry, opts, &resolved).await {
                args.extend(["-t".into(), t.to_string()]);
            }
        }
        args.extend(moe_args(entry, opts));
        if opts.reranking {
            args.push("--reranking".into());
        } else if opts.embeddings {
            args.push("--embeddings".into());
        } else {
            if let Some(p) = &entry.mmproj {
                args.extend(["--mmproj".into(), p.display().to_string()]);
            }
            args.extend(lora_args(&extras.lora));
            if let Some(d) = &extras.draft {
                args.extend(draft_args(d, s.gpu_layers));
            }
        }
        args.extend(s.extra_args.iter().cloned());
        args.extend(opts.extra_args.iter().cloned());

        let logs = self.home.join("logs");
        let _ = std::fs::create_dir_all(&logs);
        let log = logs.join(format!("{}.log", safe_name(&entry.id)));
        let out = std::fs::File::create(&log).map_err(|e| Fail::new(500, "io", format!("{}: {e}", log.display())))?;
        let err = out.try_clone().map_err(|e| Fail::new(500, "io", e.to_string()))?;

        let mut cmd = tokio::process::Command::new(&exe);
        cmd.args(&args)
            .env("LLAMA_API_KEY", &key)
            .envs(resolved.env())
            .stdin(std::process::Stdio::null())
            .stdout(out)
            .stderr(err)
            .kill_on_drop(true);
        if let Some(dir) = exe.parent() {
            cmd.current_dir(dir);
        }
        if rigel {
            if let Some(path) = hip_runtime_path(std::env::var_os("HIP_PATH"), std::env::var_os("PATH")) {
                cmd.env("PATH", path);
            }
        }
        no_window(&mut cmd);
        #[cfg(target_os = "linux")]
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
        let child = cmd
            .spawn()
            .map_err(|e| Fail::unavailable(format!("cannot start {}: {e}", exe.display())))?;
        let pid = child.id().unwrap_or(0);
        #[cfg(windows)]
        if let (Some(job), Some(h)) = (&self.job, child.raw_handle()) {
            job.assign(h);
        }
        self.loading.lock().push(Loading {
            model: entry.id.clone(),
            pid,
            size_mib: entry.size_bytes / (1024 * 1024),
            started: Instant::now(),
        });
        let _loading = LoadingGuard {
            list: &self.loading,
            model: entry.id.clone(),
        };
        let inst = Instance {
            model: entry.id.clone(),
            port,
            pid,
            key,
            child: Mutex::new(Some(child)),
            started: Instant::now(),
            args,
            log: log.clone(),
            embeddings: opts.embeddings,
            reranking: opts.reranking,
            opts: LoadOpts {
                reload: false,
                ..opts.clone()
            },
            extras: if opts.embeddings || opts.reranking {
                Extras::default()
            } else {
                extras
            },
            serving: RwLock::new(Serving::from_resolved(&resolved)),
            last_used: AtomicU64::new(now_s()),
            keep: AtomicI64::new(0),
            inflight: AtomicUsize::new(0),
            retired: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            on_stop: Mutex::new(None),
        };
        // Wait until it answers /health with 200 (it answers 503 while loading).
        let deadline = Instant::now() + Duration::from_secs(s.load_timeout_s.max(5));
        let url = format!("http://127.0.0.1:{port}/health");
        let estimate = self.estimate_for(entry, opts, &resolved).map(|e| e.total_mib);
        loop {
            // The status is read into a variable first. With the lock in the `if let` itself its guard would live through
            // the body, and `stop()` locks `child` again: a server that exited while loading (a GPU out of memory, a
            // driver crash, a file it cannot read) then froze this worker thread for good, and with it the hub.
            let exited = inst.child.lock().as_mut().and_then(|c| c.try_wait().ok().flatten());
            if let Some(status) = exited {
                inst.stop();
                let text = tail(&log, 15);
                let mut f = Fail::new(
                    500,
                    "load_failed",
                    format!("llama-server exited ({status}) while loading {}:\n{text}", entry.id),
                );
                // what a retry can act on: the GPU ran out of memory, or the server died the way a driver crash kills it
                f.fields = json!({"exit_code": status.code(),
                                  "gpu_failure": gpu_failure(&text, &status).map(GpuFailure::name)});
                return Err(f);
            }
            if let Ok(r) = self.http.get(&url).timeout(Duration::from_secs(2)).send().await {
                if r.status().is_success() {
                    if let Err(f) = self.apply_lora(&inst).await {
                        inst.stop();
                        return Err(f);
                    }
                    self.learn_serving(&inst, estimate).await;
                    let layers = serving::shape_from_meta(&entry.meta, entry.size_bytes).map(|sh| sh.layers);
                    let every_layer = Resolved {
                        gpu_layers: 999,
                        fit_target_mib: None,
                        ..resolved.clone()
                    };
                    // what moves between the GPU and the CPU (not the compute buffers and the context's fixed cost: they
                    // are most of a small model's memory and are on the GPU whichever layers are)
                    let full = self
                        .estimate_for(entry, opts, &every_layer)
                        .map(|e| e.weights_mib + e.kv_mib + e.state_mib);
                    {
                        let mut sv = inst.serving.write();
                        sv.placement =
                            serving::placement_of(sv.vram_mib, gpu_counted, full, resolved.gpu_layers, layers);
                    }
                    return Ok(inst);
                }
            }
            if Instant::now() > deadline {
                inst.stop();
                return Err(Fail::new(
                    504,
                    "load_timeout",
                    format!(
                        "{} did not finish loading in {} s:\n{}",
                        entry.id,
                        s.load_timeout_s,
                        tail(&log, 15)
                    ),
                ));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Sets the scales of the LoRA adapters a server was started with (they start switched off).
    async fn apply_lora(&self, inst: &Instance) -> Result<()> {
        if inst.extras.lora.is_empty() {
            return Ok(());
        }
        self.lora_post(inst, &inst.extras.lora).await
    }

    /// POST /lora-adapters: set the scale of each adapter the server holds.
    pub async fn lora_post(&self, inst: &Instance, lora: &[Lora]) -> Result<()> {
        let r = self
            .http
            .post(format!("http://127.0.0.1:{}/lora-adapters", inst.port))
            .bearer_auth(&inst.key)
            .json(&lora_scales(lora))
            .send()
            .await
            .map_err(|e| Fail::unavailable(format!("{} did not answer: {e}", inst.model)))?;
        if r.status().is_success() {
            return Ok(());
        }
        let text = r.text().await.unwrap_or_default();
        Err(Fail::new(
            500,
            "lora_failed",
            format!("{} refused the LoRA scales: {text}", inst.model),
        ))
    }

    /// GET /lora-adapters of a loaded model: [{id, path, scale}].
    pub async fn lora_get(&self, inst: &Instance) -> Result<Value> {
        let r = self
            .http
            .get(format!("http://127.0.0.1:{}/lora-adapters", inst.port))
            .bearer_auth(&inst.key)
            .send()
            .await
            .map_err(|e| Fail::unavailable(format!("{} did not answer: {e}", inst.model)))?;
        r.json()
            .await
            .map_err(|e| Fail::new(502, "backend_error", e.to_string()))
    }

    /// Drop instances whose process has exited, and unload idle ones.
    pub fn housekeeping(&self) {
        let idle = self.settings.read().idle_unload_s;
        let mut gone = vec![];
        for i in self.loaded() {
            let dead = i
                .child
                .lock()
                .as_mut()
                .map(|c| c.try_wait().ok().flatten().is_some())
                .unwrap_or(true);
            if dead || i.expired(idle) {
                gone.push(i.model.clone());
            }
        }
        for g in gone {
            self.unload(&g);
        }
        // Retired instances: forget the stopped ones, and stop any whose request has run for 15 minutes.
        self.draining.lock().retain(|(since, i)| {
            if since.elapsed() > Duration::from_secs(900) {
                i.stop();
            }
            !i.is_stopped()
        });
    }

    // ---- requests -----------------------------------------------------------------------------------------

    /// Send an OpenAI-style request (`path` like "/v1/chat/completions") to where `body.model` lives.
    /// The [`Lease`] (for a model of ours) must be held until the answer has been read to its end: until then the
    /// model is not stopped by a hot swap or an eviction.
    /// Does this instance's llama-server have an idle slot? `GET /slots?fail_on_no_slot=1` answers 503 when none is idle.
    /// Any other answer, and no answer at all, counts as free: the server then queues the request as it always did.
    async fn has_free_slot(&self, inst: &Instance) -> bool {
        let url = format!("http://127.0.0.1:{}/slots?fail_on_no_slot=1", inst.port);
        match self
            .http
            .get(url)
            .bearer_auth(&inst.key)
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            Ok(r) => r.status() != reqwest::StatusCode::SERVICE_UNAVAILABLE,
            Err(_) => true,
        }
    }

    pub async fn forward(&self, path: &str, mut body: Value) -> Result<(String, reqwest::Response, Option<Lease>)> {
        let name = body.get("model").and_then(Value::as_str).unwrap_or("").to_string();
        let target = self.resolve(&name)?;
        let mut lease = None;
        let (id, req) = match target {
            Target::Llama(entry) => {
                let opts = LoadOpts {
                    embeddings: path.ends_with("/embeddings"),
                    reranking: path.ends_with("/rerank"),
                    ..Default::default()
                };
                let mut inst = self.ensure_loaded(&entry, &opts, false).await?;
                // a hot swap may retire the instance between the two calls: then the new one is the answer
                for _ in 0..8 {
                    lease = self.lease(&inst);
                    if lease.is_some() {
                        break;
                    }
                    inst = self.ensure_loaded(&entry, &opts, false).await?;
                }
                if lease.is_none() {
                    return Err(Fail::unavailable(format!("{} is being replaced; try again", entry.id)));
                }
                let (want, have) = ((opts.embeddings, opts.reranking), (inst.embeddings, inst.reranking));
                if want != (false, false) && want != have {
                    let (what, flag) = if opts.reranking {
                        ("rerank", "reranking")
                    } else {
                        ("embed", "embeddings")
                    };
                    return Err(Fail::new(
                        409,
                        "wrong_mode",
                        format!(
                            "{} is loaded for another use; load it with {flag}: true to {what}",
                            entry.id
                        ),
                    ));
                }
                let fail_fast = self.settings.read().fail_fast_busy;
                if fail_fast && path.starts_with("/v1/") && !self.has_free_slot(&inst).await {
                    return Err(Fail {
                        retry_after: Some(1),
                        ..Fail::new(
                            503,
                            "busy",
                            format!("{} has no free slot; try again in a moment", entry.id),
                        )
                    });
                }
                inst.touch();
                body["model"] = json!(entry.id);
                (
                    entry.id.clone(),
                    self.http
                        .post(format!("http://127.0.0.1:{}{path}", inst.port))
                        .bearer_auth(&inst.key),
                )
            }
            Target::Ollama(n) => {
                let base = self.settings.read().ollama_url.trim_end_matches('/').to_string();
                body["model"] = json!(n);
                (format!("ollama/{n}"), self.http.post(format!("{base}{path}")))
            }
        };
        let resp = req
            .json(&body)
            .send()
            .await
            .map_err(|e| Fail::unavailable(format!("{id} did not answer: {e}")))?;
        Ok((id, resp, lease))
    }

    pub fn record(&self, id: &str, ok: bool, ms: u64, usage: Option<&Value>) {
        let mut m = self.metrics.lock();
        let e = m.entry(id.to_string()).or_default();
        e.requests += 1;
        e.total_ms += ms;
        if !ok {
            e.errors += 1;
        }
        if let Some(u) = usage {
            e.prompt_tokens += u["prompt_tokens"].as_u64().unwrap_or(0);
            e.completion_tokens += u["completion_tokens"].as_u64().unwrap_or(0);
        }
    }

    /// Models a running Ollama has (empty when it is not running).
    pub async fn ollama_models(&self) -> Vec<Value> {
        let (base, on) = {
            let s = self.settings.read();
            (s.ollama_url.trim_end_matches('/').to_string(), s.ollama_discovery)
        };
        if base.is_empty() || !on {
            return vec![];
        }
        let Ok(r) = self
            .http
            .get(format!("{base}/api/tags"))
            .timeout(Duration::from_secs(3))
            .send()
            .await
        else {
            return vec![];
        };
        let v: Value = r.json().await.unwrap_or(Value::Null);
        v["models"].as_array().cloned().unwrap_or_default()
    }

    /// `llama-bench` (next to llama-server) on one model.
    pub async fn bench(&self, entry: &ModelEntry, prompt: u32, gen: u32, gpu_layers: i32) -> Result<Value> {
        let server = self
            .llama_server()
            .ok_or_else(|| Fail::unavailable("llama-server was not found"))?;
        let bench = server.with_file_name(exe("llama-bench"));
        if !bench.is_file() {
            return Err(Fail::unavailable(format!("{} is missing", bench.display())));
        }
        let mut cmd = tokio::process::Command::new(&bench);
        cmd.args([
            "-m",
            &entry.path.display().to_string(),
            "-p",
            &prompt.to_string(),
            "-n",
            &gen.to_string(),
        ])
        .args(["-ngl", &gpu_layers.to_string(), "-o", "json", "-r", "2"])
        .kill_on_drop(true);
        no_window(&mut cmd);
        let out = tokio::time::timeout(Duration::from_secs(900), cmd.output())
            .await
            .map_err(|_| Fail::new(504, "timeout", "llama-bench took over 15 minutes"))?
            .map_err(|e| Fail::unavailable(e.to_string()))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let tail: Vec<&str> = err.lines().rev().take(10).collect();
            return Err(Fail::new(
                500,
                "bench_failed",
                tail.into_iter().rev().collect::<Vec<_>>().join("\n"),
            ));
        }
        let rows: Value =
            serde_json::from_slice(&out.stdout).map_err(|e| Fail::new(500, "bench_failed", e.to_string()))?;
        let summary: Vec<Value> = rows
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| {
                        let kind = if r["n_prompt"].as_u64().unwrap_or(0) > 0 { "prompt" } else { "generate" };
                        json!({"test": kind, "tokens": r["n_prompt"].as_u64().unwrap_or(0) + r["n_gen"].as_u64().unwrap_or(0),
                               "tokens_per_s": r["avg_ts"], "stddev": r["stddev_ts"], "backend": r["backends"],
                               "gpu": r["gpu_info"], "gpu_layers": r["n_gpu_layers"]})
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({"model": entry.id, "results": summary}))
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.unload_all();
    }
}

/// How one start of a model server is made (see `load_with_fallback`).
#[derive(Debug, Clone, PartialEq)]
enum Try {
    /// As configured. Where llama-server can choose the GPU layers itself (`--fit`), it does.
    Asked,
    /// llama-server chooses again, keeping this many MiB free on each device (the first try ran out of memory and
    /// nothing is known about the card to go by).
    Roomier(u64),
    /// The card is held to `budget` MiB for this load, because loads of this size have run out of memory on it:
    /// llama-server is told to keep `target` MiB free, which leaves it the layers that fit, and the rest stays in RAM.
    Held { target: u64, budget: u64 },
    /// Nothing on the GPU.
    Cpu,
}

/// What a load is placed against: the cards, and what the load needs of one.
struct Ground<'a> {
    devices: &'a [serving::Device],
    need_mib: Option<u64>,
    /// The model being loaded (the other loaded models are the ones that hold memory).
    model: String,
}

/// What the GPU did to a load that failed.
#[derive(Debug, Clone, Copy, PartialEq)]
enum GpuFailure {
    /// An allocation on the device failed.
    Memory,
    /// The server died the way a driver crash kills a process.
    Crash,
}

impl GpuFailure {
    fn name(self) -> &'static str {
        match self {
            GpuFailure::Memory => "memory",
            GpuFailure::Crash => "crash",
        }
    }
}

/// Whether a server that exited while loading was stopped by the GPU: its log says an allocation on a device failed,
/// or it died the way a driver crash kills a process (an access-violation or fail-fast exit code, or a signal).
fn gpu_failure(log_tail: &str, status: &std::process::ExitStatus) -> Option<GpuFailure> {
    let t = log_tail.to_ascii_lowercase();
    let on_device = ["vulkan", "cuda", "rocm", "hip", "metal", "sycl", "device memory", "gpu"]
        .iter()
        .any(|k| t.contains(k));
    let no_memory = [
        "outofdevicememory",
        "out of memory",
        "failed to allocate",
        "unable to allocate",
        "cudamalloc",
    ]
    .iter()
    .any(|k| t.contains(k));
    if on_device && no_memory {
        return Some(GpuFailure::Memory);
    }
    let crashed = match status.code() {
        Some(c) => (c as u32) >= 0xC000_0000,
        None => true,
    };
    crashed.then_some(GpuFailure::Crash)
}

/// A model that crashed on the GPU, and the server program it crashed with.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Learned {
    engine: String,
    why: String,
    at: u64,
}

fn load_learned(home: &Path) -> BTreeMap<String, Learned> {
    std::fs::read(home.join("placements.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_learned(home: &Path, all: &BTreeMap<String, Learned>) {
    let path = home.join("placements.json");
    let tmp = path.with_extension("json.tmp");
    if let Ok(text) = serde_json::to_vec_pretty(all) {
        let _ = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &path));
    }
}

fn load_limits(home: &Path) -> BTreeMap<String, serving::GpuLimit> {
    std::fs::read(home.join("gpu_limits.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_limits(home: &Path, all: &BTreeMap<String, serving::GpuLimit>) {
    let path = home.join("gpu_limits.json");
    let tmp = path.with_extension("json.tmp");
    if let Ok(text) = serde_json::to_vec_pretty(all) {
        let _ = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &path));
    }
}

/// RAM that programs can still get, in MiB (free memory and the file cache that gives way).
fn free_ram_mib() -> u64 {
    crate::misty::Misty::free_ram() / (1024 * 1024)
}

fn load_extras(home: &Path) -> HashMap<String, Extras> {
    std::fs::read(home.join("model_options.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// One sample of a Windows performance counter, as `typeperf` prints it. None elsewhere, or if it fails.
async fn typeperf(counter: &str) -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    let mut cmd = tokio::process::Command::new("typeperf");
    cmd.args([counter, "-sc", "1"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    no_window(&mut cmd);
    let out = tokio::time::timeout(Duration::from_secs(10), cmd.output())
        .await
        .ok()?
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn free_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

fn safe_name(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub fn tail(path: &Path, lines: usize) -> String {
    let text = std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let v: Vec<&str> = text.lines().collect();
    v[v.len().saturating_sub(lines)..].join("\n")
}

/// Whether a Windows priority class is one the `raise_priority` setting lifts to normal: idle and below normal only.
/// Normal, above normal, high and real-time are left alone, and so is a class this does not know.
#[cfg(windows)]
fn priority_class_is_low(class: u32) -> bool {
    use windows_sys::Win32::System::Threading::{BELOW_NORMAL_PRIORITY_CLASS, IDLE_PRIORITY_CLASS};
    class == IDLE_PRIORITY_CLASS || class == BELOW_NORMAL_PRIORITY_CLASS
}

/// Lift this process from idle or below-normal priority to normal, so the model servers it starts next inherit
/// normal priority (a Windows child process takes the low class of its parent). Returns true when it changed the
/// class. It does nothing on other systems, where a process may not raise its own priority without extra rights.
pub(crate) fn raise_to_normal_priority() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcess, GetPriorityClass, SetPriorityClass, NORMAL_PRIORITY_CLASS,
        };
        // SAFETY: GetCurrentProcess returns a pseudo handle for this process that needs no closing; GetPriorityClass
        // and SetPriorityClass only read and set this process's own priority class.
        unsafe {
            let me = GetCurrentProcess();
            if priority_class_is_low(GetPriorityClass(me)) && SetPriorityClass(me, NORMAL_PRIORITY_CLASS) != 0 {
                return true;
            }
        }
    }
    false
}

pub(crate) fn no_window(cmd: &mut tokio::process::Command) {
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// The PATH a Rigel server starts with: the HIP runtime's `bin` folder first (amdhip64, hipBLAS, rocBLAS and its
/// kernel library, which a HIP build loads at run time), then the hub's own PATH. None when HIP_PATH is unset: the
/// runtime must then be on PATH already.
pub fn hip_runtime_path(
    hip_path: Option<std::ffi::OsString>,
    path: Option<std::ffi::OsString>,
) -> Option<std::ffi::OsString> {
    let hip = PathBuf::from(hip_path.filter(|p| !p.is_empty())?);
    let mut dirs = vec![hip.join("bin")];
    if let Some(p) = path {
        dirs.extend(std::env::split_paths(&p));
    }
    std::env::join_paths(dirs).ok()
}

/// The llama.cpp programs built into this binary (feature `bundled-engine`; see build.rs). They are written to
/// `<home>/engines/bundled/<hash>/` the first time they are needed, so a new ModelMistress brings its own build
/// and an old one keeps working with its own; folders of other builds are removed.
pub mod bundled {
    use std::path::{Path, PathBuf};

    #[cfg(feature = "bundled-engine")]
    const FILES: &[(&str, &[u8])] = &[
        ("llama-server", include_bytes!(env!("MM_BUNDLED_SERVER"))),
        ("llama-bench", include_bytes!(env!("MM_BUNDLED_BENCH"))),
        ("llama-quantize", include_bytes!(env!("MM_BUNDLED_QUANTIZE"))),
        ("llama-tts", include_bytes!(env!("MM_BUNDLED_TTS"))),
        ("sd-cli", include_bytes!(env!("MM_BUNDLED_SD"))),
    ];
    #[cfg(not(feature = "bundled-engine"))]
    const FILES: &[(&str, &[u8])] = &[];

    /// The llama.cpp commit and GPU backend built in, or None.
    pub fn version() -> Option<&'static str> {
        #[cfg(feature = "bundled-engine")]
        return Some(env!("MM_BUNDLED_VERSION"));
        #[cfg(not(feature = "bundled-engine"))]
        None
    }

    /// The stable-diffusion.cpp commit built in, or None.
    pub fn sd_version() -> Option<&'static str> {
        #[cfg(feature = "bundled-engine")]
        return Some(env!("MM_BUNDLED_SD_VERSION"));
        #[cfg(not(feature = "bundled-engine"))]
        None
    }

    /// Writes the programs out if they are not there yet; returns llama-server's path.
    pub fn install(home: &Path) -> Option<PathBuf> {
        let (_, server) = FILES.first()?;
        let tag = blake3::hash(server).to_hex()[..16].to_string();
        let root = home.join("engines").join("bundled");
        let dir = root.join(&tag);
        for (name, bytes) in FILES {
            if let Err(e) = write_once(&dir.join(super::exe(name)), bytes) {
                eprintln!("model-mistress: could not write the built-in {name}: {e}");
                return None;
            }
        }
        if let Ok(rd) = std::fs::read_dir(&root) {
            for old in rd.flatten().filter(|d| d.file_name() != tag.as_str()) {
                let _ = std::fs::remove_dir_all(old.path()); // an older build's; fails harmlessly while in use
            }
        }
        Some(dir.join(super::exe("llama-server")))
    }

    #[cfg(feature = "rigel-engine")]
    const RIGEL: &[u8] = include_bytes!(env!("MM_BUNDLED_RIGEL"));

    /// The source commit and GPU targets of the built-in Rigel, or None.
    pub fn rigel_version() -> Option<&'static str> {
        #[cfg(feature = "rigel-engine")]
        return Some(env!("MM_BUNDLED_RIGEL_VERSION"));
        #[cfg(not(feature = "rigel-engine"))]
        None
    }

    /// Writes the built-in rigel-server (feature `rigel-engine`) to `<home>/engines/rigel-bundled/<hash>/` if it is
    /// not there yet, and removes other builds' folders; returns its path.
    pub fn install_rigel(home: &Path) -> Option<PathBuf> {
        #[cfg(not(feature = "rigel-engine"))]
        {
            let _ = home;
            None
        }
        #[cfg(feature = "rigel-engine")]
        {
            let tag = blake3::hash(RIGEL).to_hex()[..16].to_string();
            let root = home.join("engines").join("rigel-bundled");
            let path = root.join(&tag).join(super::exe("rigel-server"));
            if let Err(e) = write_once(&path, RIGEL) {
                eprintln!("model-mistress: could not write the built-in rigel-server: {e}");
                return None;
            }
            if let Ok(rd) = std::fs::read_dir(&root) {
                for old in rd.flatten().filter(|d| d.file_name() != tag.as_str()) {
                    let _ = std::fs::remove_dir_all(old.path());
                }
            }
            Some(path)
        }
    }

    fn write_once(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        if std::fs::metadata(path).is_ok_and(|m| m.len() == bytes.len() as u64) {
            return Ok(());
        }
        std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
        let tmp = path.with_extension("part");
        std::fs::write(&tmp, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&tmp, path)
    }
}

#[cfg(windows)]
mod job {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// A job object that kills its processes when the hub exits, however it exits.
    pub struct Job(HANDLE);
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn new() -> Option<Self> {
            unsafe {
                let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if h.is_null() {
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                SetInformationJobObject(
                    h,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                Some(Job(h))
            }
        }

        pub fn assign(&self, process: std::os::windows::io::RawHandle) {
            unsafe {
                AssignProcessToJobObject(self.0, process as HANDLE);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_test_engine_looks_in_no_folder_of_the_machine() {
        // The host's OLLAMA_MODELS, ~/.ollama and Hugging Face caches must not reach a test's roots: every folder
        // here is under the test's own home (the default settings' `downloads` is the home itself).
        let d = crate::testing::TestDir::new("mm-no-host-roots");
        let r = super::Engine::new(d.to_path_buf(), super::Settings::default()).roots();
        for dirs in [&r.ollama, &r.huggingface, &r.folders, &r.native] {
            assert!(dirs.iter().all(|p| p.starts_with(&*d)), "{dirs:?}");
        }
    }

    #[test]
    fn raise_priority_is_off_by_default_and_read_from_config_toml() {
        assert!(!Settings::default().raise_priority);
        let d = crate::testing::temp_home();
        std::fs::write(d.join("config.toml"), "raise_priority = true\n").unwrap();
        assert!(Settings::load(&d).unwrap().raise_priority);
    }

    #[cfg(windows)]
    #[test]
    fn only_idle_and_below_normal_are_raised() {
        use windows_sys::Win32::System::Threading::*;
        assert!(priority_class_is_low(IDLE_PRIORITY_CLASS));
        assert!(priority_class_is_low(BELOW_NORMAL_PRIORITY_CLASS));
        for class in [
            NORMAL_PRIORITY_CLASS,
            ABOVE_NORMAL_PRIORITY_CLASS,
            HIGH_PRIORITY_CLASS,
            REALTIME_PRIORITY_CLASS,
            0,
        ] {
            assert!(!priority_class_is_low(class), "{class:#x}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn raising_a_below_normal_process_makes_it_normal_and_a_second_call_does_nothing() {
        use windows_sys::Win32::System::Threading::*;
        // SAFETY: only this process's own priority class is read and set, and it is put back at the end.
        unsafe {
            let me = GetCurrentProcess();
            let before = GetPriorityClass(me);
            SetPriorityClass(me, BELOW_NORMAL_PRIORITY_CLASS);
            assert!(raise_to_normal_priority());
            assert_eq!(GetPriorityClass(me), NORMAL_PRIORITY_CLASS);
            assert!(!raise_to_normal_priority(), "already normal: nothing to change");
            SetPriorityClass(me, before);
        }
    }

    #[test]
    fn a_model_measured_on_the_cpu_is_not_counted_as_vram_that_unloading_would_free() {
        let home = crate::testing::temp_home();
        let e = Engine::new(home.to_path_buf(), Settings::default());
        e.adopt("on-gpu", 1, "k", None);
        e.adopt("on-cpu", 1, "k", None);
        for i in e.loaded() {
            let mut sv = i.serving.write();
            sv.estimated_vram_mib = Some(5000);
            if i.model == "on-cpu" {
                sv.placement = "cpu";
            }
        }
        let held = e.residents("another");
        let mib = |m: &str| held.iter().find(|r| r.model == m).unwrap().vram_mib;
        assert_eq!((mib("on-gpu"), mib("on-cpu")), (5000, 0));
        // a model that is being loaded is not its own neighbour
        assert!(e.residents("on-gpu").iter().all(|r| r.model != "on-gpu"));
    }

    #[tokio::test]
    async fn an_embedding_model_does_not_evict_the_chat_model() {
        let home = crate::testing::temp_home();
        let e = Engine::new(
            home.to_path_buf(),
            Settings {
                max_loaded: 1,
                max_loaded_embedding: 1,
                ..Default::default()
            },
        );
        e.adopt("chat", 1, "k", None);
        e.adopt_as("emb", 1, "k", true, None);
        // room for a second embedding model: only the embedding one goes
        e.make_room(true);
        assert!(e.is_loaded("chat") && !e.is_loaded("emb"));
        e.adopt_as("emb", 1, "k", true, None);
        // room for a second chat model: only the chat one goes (LRU within the class)
        e.make_room(false);
        assert!(!e.is_loaded("chat") && e.is_loaded("emb"));
        // under budget: nothing goes
        e.adopt("chat", 1, "k", None);
        e.set_settings(Settings {
            max_loaded: 2,
            max_loaded_embedding: 2,
            ..Default::default()
        })
        .unwrap();
        e.make_room(true);
        e.make_room(false);
        assert!(e.is_loaded("chat") && e.is_loaded("emb"));
    }

    fn moe_entry(native: bool, experts: u64) -> ModelEntry {
        let mut meta = serde_json::Map::new();
        meta.insert("general.architecture".into(), "qwen35moe".into());
        meta.insert("qwen35moe.expert_count".into(), experts.into());
        ModelEntry {
            id: "m".into(),
            source: "folder",
            path: PathBuf::from("m.gguf"),
            size_bytes: 1,
            mmproj: None,
            meta,
            native: native.then(|| crate::catalog::Native {
                family: "dusty".into(),
                name: "Dusty-1.0".into(),
            }),
        }
    }

    #[test]
    fn experts_go_to_the_cpu_for_native_moe_models_and_when_asked() {
        let none = LoadOpts::default();
        // native MoE: on by default; off when the load says so; a count wins
        assert_eq!(moe_args(&moe_entry(true, 64), &none), ["--cpu-moe"]);
        let off = LoadOpts {
            cpu_moe: Some(false),
            ..none.clone()
        };
        assert!(moe_args(&moe_entry(true, 64), &off).is_empty());
        let n = LoadOpts {
            n_cpu_moe: Some(12),
            cpu_moe: Some(true),
            ..none.clone()
        };
        assert_eq!(moe_args(&moe_entry(true, 64), &n), ["--n-cpu-moe", "12"]);
        // any other MoE model: only when asked
        assert!(moe_args(&moe_entry(false, 64), &none).is_empty());
        let on = LoadOpts {
            cpu_moe: Some(true),
            ..none.clone()
        };
        assert_eq!(moe_args(&moe_entry(false, 64), &on), ["--cpu-moe"]);
        // a dense model never gets the flag
        assert!(moe_args(&moe_entry(true, 0), &on).is_empty());
    }
    use super::*;

    #[test]
    fn settings_round_trip_and_defaults() {
        let d = crate::testing::TestDir::new("mm-settings");
        assert_eq!(Settings::load(&d).unwrap().max_loaded, 2);
        std::fs::write(d.join("config.toml"), "max_loaded = 5\nollama_url = \"\"\n").unwrap();
        let s = Settings::load(&d).unwrap();
        assert_eq!((s.max_loaded, s.ollama_url.as_str(), s.gpu_layers), (5, "", 999));
        s.save(&d).unwrap();
        assert_eq!(Settings::load(&d).unwrap().max_loaded, 5);
        std::fs::write(d.join("config.toml"), "max_loaded = \"x\"").unwrap();
        assert!(Settings::load(&d).is_err());
    }

    fn coder_entry() -> ModelEntry {
        let mut meta = serde_json::Map::new();
        for (k, v) in [
            ("general.architecture", json!("tmoe")),
            ("tmoe.block_count", json!(48)),
            ("tmoe.context_length", json!(262144)),
            ("tmoe.embedding_length", json!(2048)),
            ("tmoe.attention.head_count", json!(32)),
            ("tmoe.attention.head_count_kv", json!(4)),
            ("tmoe.attention.key_length", json!(128)),
            ("tmoe.attention.value_length", json!(128)),
            ("tmoe.expert_count", json!(128)),
            ("tmoe.expert_feed_forward_length", json!(768)),
        ] {
            meta.insert(k.into(), v);
        }
        ModelEntry {
            id: "coder".into(),
            source: "folder",
            path: PathBuf::from("coder.gguf"),
            size_bytes: 17 << 30,
            mmproj: None,
            meta,
            native: None,
        }
    }

    #[test]
    fn profiles_come_from_config_toml_and_shape_the_server_arguments() {
        let d = crate::testing::TestDir::new("mm-profiles");
        std::fs::write(
            d.join("config.toml"),
            "ctx_size = 4096
[serving]
flash_attn = \"on\"
[profiles.coder]
ctx_per_slot = 32768
parallel = 4
             cache_type_k = \"q8_0\"
cache_type_v = \"q8_0\"
cache_prompt = true
cache_reuse = 256
",
        )
        .unwrap();
        let s = Settings::load(&d).unwrap();
        let e = Engine::new(d.clone(), s.clone());
        let entry = coder_entry();
        let r = e.resolve_serving(&s, &entry, &LoadOpts::default());
        assert_eq!(
            r.args().join(" "),
            "-c 131072 -ngl 999 -np 4 -ctk q8_0 -ctv q8_0 -fa on --cache-prompt --cache-reuse 256"
        );
        // a model without a profile gets [serving] and the old settings
        let mut other = coder_entry();
        other.id = "other".into();
        assert_eq!(
            e.resolve_serving(&s, &other, &LoadOpts::default()).args().join(" "),
            "-c 4096 -ngl 999 -np 2 -fa on"
        );
        // the load request wins over the profile
        let opts: LoadOpts =
            serde_json::from_value(json!({"parallel": 1, "cache_type_k": "f16", "ctx_size": 8192})).unwrap();
        assert_eq!(opts.serve.cache_type_k.as_deref(), Some("f16"));
        let r = e.resolve_serving(&s, &entry, &opts);
        assert_eq!(
            r.args().join(" "),
            "-c 8192 -ngl 999 -np 1 -ctk f16 -ctv q8_0 -fa on --cache-prompt --cache-reuse 256"
        );
        // what /v1/models shows before a load
        let sv = e.serving_preview(&entry);
        assert_eq!((sv.n_ctx, sv.n_ctx_per_slot, sv.slots), (131_072, 32_768, 4));
        assert_eq!(
            (sv.cache_type_k.as_str(), sv.flash_attn.as_str(), sv.source),
            ("q8_0", "on", "profile")
        );
        // the estimate sees the quantised cache: 4 kv heads x 48 layers x 256 x 34/32 B x 131072 tokens = 6528 MiB
        let est = e
            .estimate_for(
                &entry,
                &LoadOpts::default(),
                &e.resolve_serving(&s, &entry, &LoadOpts::default()),
            )
            .unwrap();
        assert_eq!(est.kv_mib, 6528);
        // saved and read back
        s.save(&d).unwrap();
        assert_eq!(Settings::load(&d).unwrap().profiles, s.profiles);
    }

    #[test]
    fn a_bad_profile_is_reported_and_a_good_config_is_unchanged() {
        let d = crate::testing::TestDir::new("mm-profile-bad");
        for (toml, what) in [
            (
                "[profiles.m]
cache_type_k = \"q9\"
",
                "cache_type_k",
            ),
            (
                "[profiles.m]
cache_tyep_k = \"q8_0\"
",
                "cache_tyep_k",
            ),
            (
                "[serving]
parallel = 0
",
                "parallel",
            ),
            (
                "[profiles.m]
cache_type_v = \"q4_0\"
flash_attn = \"off\"
",
                "flash attention",
            ),
        ] {
            std::fs::write(d.join("config.toml"), toml).unwrap();
            // a typo in a serving table is a warning and that field keeps its default; it never stops the hub
            let s = Settings::load(&d).unwrap();
            assert!(s.warnings.iter().any(|w| w.contains(what)), "{toml}: {:?}", s.warnings);
            let bad_field = |p: &ServeProfile| {
                p.parallel == Some(0) || p.flags.cache_type_k.is_some() || p.flags.cache_type_v.is_some()
            };
            assert!(!bad_field(&s.serving) && !s.profiles.values().any(bad_field), "{toml}");
            // ... and the hub-side engine starts with those settings
            let eng = Engine::new(d.clone(), s);
            assert!(!eng.settings().warnings.is_empty());
        }
        // a bad value type, a bad enum value and an unknown key: only the bad fields fall back
        std::fs::write(
            d.join("config.toml"),
            "max_loaded = 3
[serving]
parallel = \"many\"
cache_type_k = \"q8_0\"
[profiles.m]
ctx_per_slot = 4096
cache_type_v = \"q9\"
flashattn = true
",
        )
        .unwrap();
        let s = Settings::load(&d).unwrap();
        assert_eq!((s.max_loaded, s.serving.parallel), (3, None));
        assert_eq!(s.serving.flags.cache_type_k.as_deref(), Some("q8_0"));
        let m = &s.profiles["m"];
        assert_eq!(
            (m.flags.ctx_per_slot, m.flags.cache_type_v.as_deref()),
            (Some(4096), None)
        );
        assert_eq!(s.warnings.len(), 3, "{:?}", s.warnings);
        s.check_serving().unwrap();
        // top-level mistakes are still errors
        std::fs::write(
            d.join("config.toml"),
            "max_loaded = \"x\"\n[serving]\nparallel = \"y\"\n",
        )
        .unwrap();
        assert!(Settings::load(&d).is_err());
        std::fs::write(d.join("config.toml"), "[serving\n").unwrap();
        assert!(Settings::load(&d).is_err());
        // config.set goes through set_settings: refused, and nothing saved
        let eng = Engine::new(d.clone(), Settings::default());
        let mut bad = Settings::default();
        bad.profiles.insert(
            "m".into(),
            ServeProfile {
                parallel: Some(0),
                ..Default::default()
            },
        );
        assert_eq!(eng.set_settings(bad).unwrap_err().status, 400);
        // a config from before profiles existed has none, and the same arguments as always
        std::fs::write(
            d.join("config.toml"),
            "ctx_size = 16384
parallel = 1
",
        )
        .unwrap();
        let s = Settings::load(&d).unwrap();
        assert!(s.profiles.is_empty() && s.serving == ServeProfile::default() && s.vram_guard);
        let r = eng.resolve_serving(&s, &coder_entry(), &LoadOpts::default());
        assert_eq!(r.args(), ["-c", "16384", "-ngl", "999", "-np", "1"]);
    }

    #[test]
    fn a_load_that_does_not_fit_is_planned_not_spilled() {
        let d = crate::testing::TestDir::new("mm-plan");
        let e = Engine::new(
            d.clone(),
            Settings {
                ollama_url: String::new(),
                ..Default::default()
            },
        );
        let gpu = |free| {
            vec![serving::Device {
                name: "Vulkan0: test".into(),
                total_mib: 48_000,
                free_mib: free,
            }]
        };
        let entry = coder_entry(); // ~17 GiB of weights, 8 GiB of KV at 131072 tokens in f16
        let big = LoadOpts {
            ctx_size: Some(131_072),
            ..Default::default()
        };
        // 5000 MiB free, nothing loaded to unload: refused with the numbers and the way out
        let f = e.vram_decision(&entry, &big, &gpu(5000)).unwrap_err();
        assert_eq!((f.status, f.code), (503, "insufficient_vram"));
        assert!(
            f.message.contains("skip_vram_check") && f.message.contains("5000 MiB for models"),
            "{}",
            f.message
        );
        // plenty free: go ahead
        assert_eq!(e.vram_decision(&entry, &big, &gpu(40_000)).unwrap(), None);
        // experts in system memory: a small load that fits where the whole model does not
        let moe = LoadOpts {
            cpu_moe: Some(true),
            ..Default::default()
        };
        assert_eq!(e.vram_decision(&entry, &moe, &gpu(5000)).unwrap(), None);
        // no devices listed (a CPU build): the guard has nothing to go on and lets the load through
        assert_eq!(e.vram_decision(&entry, &big, &[]).unwrap(), None);
        // a loaded model that is idle is unloaded to make room, a busy one is not
        let other = e.adopt("other", 1, "k", None);
        other.serving.write().vram_mib = Some(30_000.0);
        let (models, want) = e.vram_decision(&entry, &big, &gpu(5000)).unwrap().unwrap();
        assert_eq!(models, ["other"]);
        assert!(want > 17_000);
        let _lease = e.lease(&other).unwrap();
        assert!(e.vram_decision(&entry, &big, &gpu(5000)).is_err());
    }

    #[test]
    fn resolving_names() {
        let d = crate::testing::TestDir::new("mm-resolve");
        let e = Engine::new(
            d.clone(),
            Settings {
                ollama_url: String::new(),
                ..Default::default()
            },
        );
        *e.catalog.write() = ["llama3.2:1b", "qwen3:1.7b", "qwen3:8b", "tool:latest"]
            .iter()
            .map(|id| ModelEntry {
                id: id.to_string(),
                source: "ollama",
                path: d.join("x"),
                size_bytes: 1,
                mmproj: None,
                meta: Default::default(),
                native: None,
            })
            .collect();
        let id = |n: &str| match e.resolve(n) {
            Ok(Target::Llama(m)) => m.id,
            Ok(Target::Ollama(n)) => format!("ollama:{n}"),
            Err(f) => format!("err {}", f.code),
        };
        assert_eq!(id("llama3.2:1b"), "llama3.2:1b");
        assert_eq!(id("LLAMA3.2:1B"), "llama3.2:1b");
        assert_eq!(id("llama"), "llama3.2:1b"); // a unique prefix
        assert_eq!(id("qwen3"), "err not_found"); // two match
        assert_eq!(id("tool"), "tool:latest");
        assert_eq!(id(""), "err invalid");
        assert_eq!(id("ollama/x"), "err unavailable"); // passing to Ollama is off here
        assert_eq!(safe_name("unsloth/Qwen:Q4"), "unsloth_Qwen_Q4");
    }

    #[test]
    fn ollama_discovery_off_lists_and_passes_no_ollama_model() {
        let d = crate::testing::TestDir::new("mm-ollama-discovery");
        let store = d.join("store");
        std::fs::create_dir_all(store.join("blobs")).unwrap();
        crate::catalog::tests_support::gguf(&store.join("blobs/sha256-aa"), "llama", "L");
        let p = store.join("manifests/registry.ollama.ai/library/llama3.2");
        std::fs::create_dir_all(&p).unwrap();
        let layer = r#"{"layers":[{"mediaType":"application/vnd.ollama.image.model","digest":"sha256:aa"}]}"#;
        std::fs::write(p.join("1b"), layer).unwrap();
        let settings = |discovery: bool| Settings {
            ollama_dirs: vec![store.to_path_buf()],
            ollama_discovery: discovery,
            ..Default::default()
        };
        std::fs::write(d.join("config.toml"), "ollama_discovery = false\n").unwrap();
        assert!(!Settings::load(&d).unwrap().ollama_discovery);
        assert!(Settings::default().ollama_discovery);
        let on = Engine::new(d.to_path_buf(), settings(true));
        let off = Engine::new(d.to_path_buf(), settings(false));
        let ollama_ids = |e: &Engine| -> Vec<String> {
            catalog::scan(&e.roots())
                .into_iter()
                .filter(|m| m.source == "ollama")
                .map(|m| m.id)
                .collect()
        };
        assert_eq!(ollama_ids(&on), ["llama3.2:1b"]);
        assert!(ollama_ids(&off).is_empty());
        assert!(off.roots().ollama.is_empty());
        assert!(matches!(on.resolve("ollama/qwen3:8b"), Ok(Target::Ollama(n)) if n == "qwen3:8b"));
        match off.resolve("ollama/qwen3:8b") {
            Err(f) => assert_eq!((f.status, f.code), (503, "unavailable")),
            Ok(_) => panic!("pass-through must be refused with ollama_discovery off"),
        }
    }

    #[test]
    fn llama_server_arguments_for_lora_and_draft_models() {
        assert!(lora_args(&[]).is_empty());
        let lora = [
            Lora {
                path: PathBuf::from("a.gguf"),
                scale: 0.5,
            },
            Lora {
                path: PathBuf::from("b.gguf"),
                scale: 1.0,
            },
        ];
        assert_eq!(
            lora_args(&lora),
            ["--lora-init-without-apply", "--lora", "a.gguf", "--lora", "b.gguf"]
        );
        // the scales go in by position, through the server's API
        assert_eq!(
            lora_scales(&lora),
            json!([{"id": 0, "scale": 0.5}, {"id": 1, "scale": 1.0}])
        );
        let d = Draft {
            path: PathBuf::from("d.gguf"),
            n_max: Some(16),
            n_min: None,
            gpu_layers: None,
        };
        assert_eq!(
            draft_args(&d, 99),
            [
                "--spec-type",
                "draft-simple",
                "--spec-draft-model",
                "d.gguf",
                "--spec-draft-n-max",
                "16",
                "--spec-draft-ngl",
                "99"
            ]
        );
        let d = Draft {
            n_min: Some(2),
            gpu_layers: Some(0),
            n_max: None,
            ..d
        };
        assert_eq!(
            draft_args(&d, 99),
            [
                "--spec-type",
                "draft-simple",
                "--spec-draft-model",
                "d.gguf",
                "--spec-draft-n-min",
                "2",
                "--spec-draft-ngl",
                "0"
            ]
        );
    }

    #[test]
    fn rigel_starts_with_the_hip_runtime_first_on_path() {
        let hub_path = std::env::join_paths([PathBuf::from("first"), PathBuf::from("second")]).unwrap();
        let p = hip_runtime_path(Some("hip".into()), Some(hub_path)).unwrap();
        let dirs: Vec<PathBuf> = std::env::split_paths(&p).collect();
        assert_eq!(
            dirs,
            [PathBuf::from("hip").join("bin"), "first".into(), "second".into()]
        );
        assert_eq!(hip_runtime_path(None, Some("x".into())), None);
        assert_eq!(hip_runtime_path(Some("".into()), Some("x".into())), None);
    }

    #[test]
    fn rigel_server_is_found_in_the_setting_then_the_home_folder() {
        let d = crate::testing::TestDir::new("mm-rigel");
        let e = Engine::new(
            d.clone(),
            Settings {
                rigel_server: Some(d.join("no-such-rigel-server")),
                ..Default::default()
            },
        );
        assert_eq!(
            e.rigel_server(),
            None,
            "a setting that names a missing file finds nothing"
        );
        let own = d.join("engines").join("rigel").join(exe("rigel-server"));
        std::fs::create_dir_all(own.parent().unwrap()).unwrap();
        std::fs::write(&own, b"").unwrap();
        assert_eq!(
            e.rigel_server(),
            None,
            "the setting wins even when it names a missing file"
        );
        let mut s = e.settings();
        s.rigel_server = None;
        e.set_settings(s).unwrap();
        if std::env::var_os("MM_RIGEL_SERVER").is_none() {
            assert_eq!(e.rigel_server(), Some(own));
        }
    }

    #[tokio::test]
    async fn extras_are_saved_and_a_missing_tool_is_501() {
        let d = crate::testing::TestDir::new("mm-extras");
        let settings = Settings {
            llama_server: Some(d.join("no-such-llama-server")),
            ollama_url: String::new(),
            ..Default::default()
        };
        let e = Engine::new(d.clone(), settings.clone());
        assert_eq!(e.extras("m"), Extras::default());
        let x = Extras {
            lora: vec![Lora {
                path: PathBuf::from("a.gguf"),
                scale: 0.25,
            }],
            draft: Some(Draft {
                path: PathBuf::from("d.gguf"),
                n_max: Some(4),
                n_min: None,
                gpu_layers: Some(-1),
            }),
            model_path: Some(PathBuf::from("assembled.gguf")),
        };
        e.set_extras("m", x.clone()).unwrap();
        drop(e);
        let e = Engine::new(d.clone(), settings);
        assert_eq!(e.extras("m"), x);
        e.set_extras("m", Extras::default()).unwrap();
        assert_eq!(
            Engine::new(d.clone(), Settings::default()).extras("m"),
            Extras::default()
        );
        // no llama-server, so no tools beside it
        assert!(e.tool("llama-quantize").is_none());
        let f = e
            .run_tool("llama-quantize", &[], Duration::from_secs(5), &mut |_| true)
            .await
            .unwrap_err();
        assert!(f.status == 501 && f.message.contains("llama-quantize"), "{}", f.message);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A llama-server stand-in that ends with an error as soon as it starts (what a GPU out of memory or a driver crash
    /// looks like to the hub: the process is gone before it answers `/health`).
    fn exiting_server(dir: &Path) -> PathBuf {
        #[cfg(windows)]
        {
            let p = dir.join("llama-server.cmd");
            std::fs::write(&p, "@echo off\r\nexit /b 3\r\n").unwrap();
            p
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.join("llama-server");
            std::fs::write(&p, "#!/bin/sh\nexit 3\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }
    }

    /// A llama-server that exits while it loads must end the load with an error. It once froze the worker thread that
    /// ran the load (`stop()` locks `child`, and the exit check still held that lock), and with that thread the whole
    /// hub: nothing answered, not even /v1/health, until the process was restarted.
    #[test]
    fn a_server_that_exits_while_loading_fails_the_load_and_leaves_the_hub_running() {
        let d = crate::testing::TestDir::new("mm-load-exit");
        let stub = exiting_server(&d);
        let home = d.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        // The runtime lives on a thread of its own so that this test's timeout does not depend on it: a stuck worker
        // takes tokio's timers and its I/O with it, which is also why the hub went deaf.
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let outcome = rt.block_on(async {
                let e = Arc::new(Engine::new(
                    home.clone(),
                    Settings {
                        llama_server: Some(stub),
                        ollama_url: String::new(),
                        ..Default::default()
                    },
                ));
                let entry = ModelEntry {
                    id: "m".into(),
                    source: "test",
                    path: home.join("m.gguf"),
                    size_bytes: 1,
                    mmproj: None,
                    meta: Default::default(),
                    native: None,
                };
                let first = tokio::spawn({
                    let (e, entry) = (e.clone(), entry.clone());
                    async move { e.ensure_loaded(&entry, &LoadOpts::default(), true).await.map(|_| ()) }
                })
                .await;
                // the runtime still runs other work, and a second try ends the same way
                let other = tokio::spawn(async { 7 }).await;
                let again = e.ensure_loaded(&entry, &LoadOpts::default(), true).await.is_err();
                (first, other, again, e.loaded().len())
            });
            let _ = tx.send(outcome);
        });
        let (first, other, again, loaded) = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the load never ended: a worker thread is stuck");
        let f = first
            .expect("the load task panicked")
            .expect_err("a server that exits cannot be loaded");
        assert_eq!((f.status, f.code), (500, "load_failed"), "{}", f.message);
        assert_eq!(other.unwrap(), 7);
        assert!(again, "a second load must fail the same way");
        assert_eq!(loaded, 0);
    }

    fn exited(n: i32) -> std::process::ExitStatus {
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(n as u32)
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(n << 8)
        }
    }

    /// How a driver crash ends a process: a fail-fast / access-violation code on Windows, a signal elsewhere.
    fn crashed() -> std::process::ExitStatus {
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(0xC000_0409)
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(11)
        }
    }

    #[test]
    fn a_failed_load_is_told_apart_by_what_the_gpu_did() {
        let vulkan = "ggml_vulkan: Device memory allocation of size 17000000000 failed.\n\
                      ggml_vulkan: vk::Device::allocateMemory: ErrorOutOfDeviceMemory\n\
                      llama_model_load: error loading model";
        let cuda = "CUDA error: out of memory\n  current device: 0, in function alloc at ggml-cuda.cu:420\n  cudaMalloc failed";
        // the log says an allocation on a device failed: memory, whatever the exit code is
        assert_eq!(gpu_failure(vulkan, &exited(1)), Some(GpuFailure::Memory));
        assert_eq!(gpu_failure(cuda, &exited(1)), Some(GpuFailure::Memory));
        assert_eq!(gpu_failure(vulkan, &crashed()), Some(GpuFailure::Memory));
        // a server that is killed by the driver leaves nothing to read: the exit code says it
        assert_eq!(gpu_failure("", &crashed()), Some(GpuFailure::Crash));
        assert_eq!(
            gpu_failure("ggml_vulkan: Found 1 Vulkan devices:", &crashed()),
            Some(GpuFailure::Crash)
        );
        // anything else is the model's or the user's to fix: a retry on the CPU would not help
        assert_eq!(gpu_failure("", &exited(3)), None);
        assert_eq!(
            gpu_failure("llama_model_load: unknown model architecture: 'x'", &exited(1)),
            None
        );
        // running out of RAM is not the GPU's doing
        assert_eq!(
            gpu_failure("failed to allocate CPU buffer of size 99999", &exited(1)),
            None
        );
        assert_eq!(GpuFailure::Memory.name(), "memory");
        assert_eq!(GpuFailure::Crash.name(), "crash");
    }

    #[test]
    fn the_gpu_settings_default_to_a_fallback_and_a_watchdog_and_are_read_from_config_toml() {
        let s = Settings::default();
        assert!(s.gpu_fallback);
        assert_eq!(s.watchdog_s, 90);
        let d = crate::testing::temp_home();
        std::fs::write(d.join("config.toml"), "gpu_fallback = false\nwatchdog_s = 0\n").unwrap();
        let s = Settings::load(&d).unwrap();
        assert!(!s.gpu_fallback);
        assert_eq!(s.watchdog_s, 0);
    }

    #[test]
    fn a_remembered_crash_survives_a_restart_and_a_change_of_server_program() {
        let d = crate::testing::TestDir::new("mm-learned");
        let stub = gpu_crashing_server(&d);
        let settings = Settings {
            llama_server: Some(stub.clone()),
            ollama_url: String::new(),
            ..Default::default()
        };
        let entry = stand_in_entry(&d, 1);
        let e = Engine::new(d.to_path_buf(), settings.clone());
        let r = e.resolve_serving(&e.settings(), &entry, &LoadOpts::default());
        assert!(e.learned_why(&entry, &r).is_none());
        e.learn(&entry, &r, "its load crashed".into());
        assert_eq!(e.learned_why(&entry, &r).as_deref(), Some("its load crashed"));
        // written to disk: a new hub knows it
        assert!(d.join("placements.json").is_file());
        let again = Engine::new(d.to_path_buf(), settings.clone());
        assert_eq!(again.learned_why(&entry, &r).as_deref(), Some("its load crashed"));
        // another model is not affected
        let other = ModelEntry {
            id: "other".into(),
            ..entry.clone()
        };
        assert!(again.learned_why(&other, &r).is_none());
        // a different server program (an update, another build) gets a new try on the GPU
        std::fs::write(&stub, "a different program, of another size\r\n").unwrap();
        assert!(again.learned_why(&entry, &r).is_none());
        // forgetting it removes it from the disk as well
        e.forget("m");
        assert!(load_learned(&d).is_empty());
        // a damaged file is no reason to fail
        std::fs::write(d.join("placements.json"), "{ not json").unwrap();
        assert!(load_learned(&d).is_empty());
    }

    fn stand_in_entry(home: &Path, size_bytes: u64) -> ModelEntry {
        ModelEntry {
            id: "m".into(),
            source: "test",
            path: home.join("m.gguf"),
            size_bytes,
            mmproj: None,
            meta: Default::default(),
            native: None,
        }
    }

    /// A llama-server stand-in for the placement ladder. It answers `--help` with the flags the hub looks for and
    /// `--list-devices` with no devices; every other start is written to `calls.txt` and ends: with a driver-crash
    /// code when it is started for the GPU, with 3 when it is started without one (`-dev none`). With a file named
    /// `oom` beside it, a start for the GPU ends with the words of a Vulkan out-of-memory error instead.
    fn gpu_crashing_server(dir: &Path) -> PathBuf {
        #[cfg(windows)]
        {
            let p = dir.join("llama-server.cmd");
            std::fs::write(
                &p,
                "@echo off\r\n\
                 if \"%1\"==\"--help\" goto help\r\n\
                 if \"%1\"==\"--list-devices\" exit /b 0\r\n\
                 echo %* >>\"%~dp0calls.txt\"\r\n\
                 echo %* | findstr /C:\"-dev none\" >nul\r\n\
                 if not errorlevel 1 exit /b 3\r\n\
                 if exist \"%~dp0oom\" (echo ggml_vulkan: vk::Device::allocateMemory: ErrorOutOfDeviceMemory& exit /b 1)\r\n\
                 exit /b -1073740791\r\n\
                 :help\r\n\
                 echo --fit --device\r\n\
                 exit /b 0\r\n",
            )
            .unwrap();
            p
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.join("llama-server");
            std::fs::write(
                &p,
                "#!/bin/sh\n\
                 case \"$1\" in --help) echo '--fit --device'; exit 0;; --list-devices) exit 0;; esac\n\
                 here=$(dirname \"$0\")\n\
                 echo \"$@\" >> \"$here/calls.txt\"\n\
                 case \"$*\" in *'-dev none'*) exit 3;; esac\n\
                 if [ -e \"$here/oom\" ]; then echo 'ggml_vulkan: vk::Device::allocateMemory: ErrorOutOfDeviceMemory'; exit 1; fi\n\
                 kill -SEGV $$\n",
            )
            .unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }
    }

    /// The starts the stand-in has seen, one line each.
    fn starts(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("calls.txt"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    }

    fn ladder_engine(d: &Path, gpu_fallback: bool) -> Arc<Engine> {
        Engine::new(
            d.to_path_buf(),
            Settings {
                llama_server: Some(gpu_crashing_server(d)),
                ollama_url: String::new(),
                gpu_fallback,
                ..Default::default()
            },
        )
    }

    /// A load that crashes the server on the GPU is tried again on the CPU, with the GPU backend left out.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_gpu_crash_on_load_is_retried_on_the_cpu() {
        let d = crate::testing::TestDir::new("mm-ladder-crash");
        let e = ladder_engine(&d, true);
        let entry = stand_in_entry(&d, 1);
        let f = e
            .ensure_loaded(&entry, &LoadOpts::default(), true)
            .await
            .err()
            .expect("the stand-in never answers");
        // its CPU start fails too (it exits with 3), and that is the error: with the GPU's story in front of it
        assert_eq!((f.status, f.code), (500, "load_failed"), "{}", f.message);
        assert!(
            f.message.contains("exit code: 3") || f.message.contains("exit status: 3"),
            "{}",
            f.message
        );
        assert!(
            f.message
                .contains("(before this: its load on the GPU crashed llama-server (exit 0xC0000409))")
                || cfg!(not(windows)),
            "{}",
            f.message
        );
        let seen = starts(&d);
        assert_eq!(seen.len(), 2, "{seen:?}");
        // first as asked: llama-server fits the layers itself, keeping the margin free
        let margin = e.settings().vram_margin_mib.max(512);
        assert!(
            seen[0].contains(&format!("--fit on --fit-target {margin}")),
            "{}",
            seen[0]
        );
        assert!(
            !seen[0].contains("-ngl") && !seen[0].contains("-dev none"),
            "{}",
            seen[0]
        );
        // then nothing on the GPU, and no GPU backend either
        assert!(
            seen[1].contains("-ngl 0") && seen[1].contains("-dev none"),
            "{}",
            seen[1]
        );
        assert!(!seen[1].contains("--fit"), "{}", seen[1]);
        // a crash is remembered only when the CPU start worked, and here it did not
        assert!(!d.join("placements.json").exists());
        assert_eq!(e.loaded().len(), 0);
    }

    /// An out-of-memory error first gets more room on the GPU, and only then the CPU.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_gpu_out_of_memory_gets_more_room_before_the_cpu() {
        let d = crate::testing::TestDir::new("mm-ladder-oom");
        std::fs::write(d.join("oom"), "").unwrap();
        let e = ladder_engine(&d, true);
        let entry = stand_in_entry(&d, 1);
        let f = e
            .ensure_loaded(&entry, &LoadOpts::default(), true)
            .await
            .err()
            .expect("the stand-in never answers");
        assert!(
            f.message
                .contains("the GPU ran out of memory, so llama-server was asked to keep"),
            "{}",
            f.message
        );
        assert!(
            f.message.contains("the GPU ran out of memory;") || f.message.contains("the GPU ran out of memory)"),
            "{}",
            f.message
        );
        let seen = starts(&d);
        assert_eq!(seen.len(), 3, "{seen:?}");
        let margin = e.settings().vram_margin_mib.max(512);
        let roomier = margin + 2048;
        assert!(seen[0].contains(&format!("--fit-target {margin} ")), "{}", seen[0]);
        assert!(seen[1].contains(&format!("--fit-target {roomier} ")), "{}", seen[1]);
        assert!(seen[2].contains("-dev none"), "{}", seen[2]);
        // running out of memory is about the load, not about the driver: nothing is remembered
        assert!(!d.join("placements.json").exists());
    }

    /// A 3 GiB model the planner can estimate (32 layers, 4 KV heads, 128-wide heads): 4,084 MiB with the default
    /// context of 8192 tokens in 2 slots.
    fn sized_entry(home: &Path) -> ModelEntry {
        let mut e = stand_in_entry(home, 3 << 30);
        for (k, v) in [
            ("general.architecture", json!("tiny")),
            ("tiny.block_count", json!(32)),
            ("tiny.embedding_length", json!(2048)),
            ("tiny.attention.head_count", json!(16)),
            ("tiny.attention.head_count_kv", json!(4)),
            ("tiny.attention.key_length", json!(128)),
            ("tiny.attention.value_length", json!(128)),
        ] {
            e.meta.insert(k.into(), v);
        }
        e
    }

    fn card(total_mib: u64, free_mib: u64) -> Vec<serving::Device> {
        vec![serving::Device {
            name: "Vulkan0: test card".into(),
            total_mib,
            free_mib,
        }]
    }

    /// The `--fit-target` of each start the stand-in saw (None: the start had none, as a CPU start).
    fn fit_targets(starts: &[String]) -> Vec<Option<u64>> {
        starts
            .iter()
            .map(|l| {
                let mut w = l.split_whitespace();
                w.by_ref().find(|x| *x == "--fit-target")?;
                w.next()?.parse().ok()
            })
            .collect()
    }

    /// The 2 GiB the system keeps are never handed to a model: the first start is already held under them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_reserve_for_the_system_holds_the_first_load_below_the_card() {
        let d = crate::testing::TestDir::new("mm-ladder-reserve");
        std::fs::write(d.join("oom"), "").unwrap();
        let e = ladder_engine(&d, true);
        // 6,000 MiB with 5,600 free: models may hold 6,000 - 2,048 = 3,952 of it, and the load needs 4,084
        *e.fake_devices.lock() = Some(card(6_000, 5_600));
        let entry = sized_entry(&d);
        let r = e.resolve_serving(&e.settings(), &entry, &LoadOpts::default());
        assert_eq!(
            e.estimate_for(&entry, &LoadOpts::default(), &r).unwrap().total_mib,
            4_084
        );
        assert!(e.ensure_loaded(&entry, &LoadOpts::default(), true).await.is_err());
        let targets = fit_targets(&starts(&d));
        // llama-server keeps 5,600 - 3,952 = 1,648 MiB free from the first start on, not just the 768 MiB margin
        assert_eq!(targets[0], Some(1_648), "{targets:?}");
        // when that run out of memory too, the next try keeps more free still
        assert!(targets[1].is_some_and(|t| t > 1_648), "{targets:?}");
    }

    /// An out-of-memory error is answered with a split held lower each time, then the CPU; what failed is kept, and the
    /// next load of the model starts where it worked.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_card_that_runs_out_of_memory_is_asked_for_less_each_time_and_the_limit_is_kept() {
        let d = crate::testing::TestDir::new("mm-ladder-split");
        std::fs::write(d.join("oom"), "").unwrap();
        let e = ladder_engine(&d, true);
        *e.fake_devices.lock() = Some(card(10_000, 9_500));
        let entry = sized_entry(&d);
        let f = e
            .ensure_loaded(&entry, &LoadOpts::default(), true)
            .await
            .err()
            .expect("the stand-in never answers");
        assert!(
            f.message
                .contains("so the load was held to 3574 MiB and about 510 MiB of the model stays in RAM"),
            "{}",
            f.message
        );
        let seen = starts(&d);
        let targets = fit_targets(&seen);
        // as asked (the 768 MiB margin), then three splits, each keeping more free than the last: the ask that failed
        // was 4,084, then 3,574, then 3,128, each held an eighth under
        assert_eq!(
            &targets[..4],
            &[Some(768), Some(5_926), Some(6_372), Some(6_763)],
            "{seen:?}"
        );
        // the CPU only when its RAM holds the file
        if let Some(cpu) = seen.get(4) {
            assert!(cpu.contains("-dev none"), "{cpu}");
        }
        assert!(seen.len() <= 5, "{seen:?}");
        let kept = load_limits(&d);
        assert_eq!(kept.len(), 1, "{kept:?}");
        let l = kept.values().next().unwrap();
        assert_eq!((l.ok_mib, l.failed_mib), (0, 3_128));
        assert!(kept.keys().next().unwrap().starts_with("Vulkan0: test card|"));

        // another run of the hub (same server program) starts from what it learned: held to 3128 - 391 = 2737 at once
        std::fs::remove_file(d.join("calls.txt")).unwrap();
        let stub = e.settings().llama_server.clone().unwrap();
        let again = Engine::new(
            d.to_path_buf(),
            Settings {
                llama_server: Some(stub),
                ollama_url: String::new(),
                ..Default::default()
            },
        );
        *again.fake_devices.lock() = Some(card(10_000, 9_500));
        assert!(again.ensure_loaded(&entry, &LoadOpts::default(), true).await.is_err());
        let first = &starts(&d)[0];
        assert!(first.contains("--fit-target 6763 "), "{first}");
        // retry_gpu goes back to asking the driver's figure
        std::fs::remove_file(d.join("calls.txt")).unwrap();
        let opts = LoadOpts {
            retry_gpu: true,
            ..Default::default()
        };
        assert!(again.ensure_loaded(&entry, &opts, true).await.is_err());
        assert!(starts(&d)[0].contains("--fit-target 768 "), "{:?}", starts(&d));
    }

    /// A ternary file is served by the Prism GPU build when layers go to the card and by the CPU build when none do, and
    /// the label follows: CPU only with the CPU build alone, the card with the GPU build.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_ternary_file_runs_on_the_prism_gpu_build_for_the_card_and_the_cpu_build_for_the_cpu() {
        let d = crate::testing::TestDir::new("mm-prism-gpu");
        let place = |dir: &str| {
            let p = d.join("engines").join(dir).join(exe("llama-server"));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"stand-in").unwrap();
            p
        };
        let cpu_build = place("prism");
        let e = Engine::new(
            d.to_path_buf(),
            Settings {
                ollama_url: String::new(),
                ..Default::default()
            },
        );
        let mut entry = stand_in_entry(&d, 1 << 29);
        entry.id = "Ternary-Bonsai-2-2B-PQ2_0".into();
        let (s, opts) = (e.settings(), LoadOpts::default());
        let mut r = e.resolve_serving(&s, &entry, &opts);
        assert!(r.flags.is_prism());
        // the CPU build alone serves it, and it has no card to use
        assert_eq!(e.server_for(&r), Some(cpu_build.clone()));
        assert!(!e.can_use_gpu(&r, &card(24_000, 23_000)));
        assert_eq!(e.hardware_json(&entry, &card(24_000, 23_000))["runs_on"], "cpu");
        // with the GPU build, a load that puts layers on the card uses it, and one that puts none does not
        let gpu_build = place("prism-gpu");
        assert_eq!(e.server_for(&r), Some(gpu_build.clone()));
        r.gpu_layers = 0;
        assert_eq!(e.server_for(&r), Some(cpu_build));
        assert!(e.can_use_gpu(&r, &card(24_000, 23_000)));
        assert!(!e.can_use_gpu(&r, &[]));
        let h = e.hardware_json(&entry, &card(24_000, 23_000));
        assert_eq!(
            (h["runs_on"].as_str(), h["can_run_on"].clone()),
            (Some("gpu"), json!(["gpu", "cpu"])),
            "{h}"
        );
    }

    /// A model's hardware label says what it can run on and where it would go: the CPU without a card, the GPU on a
    /// roomy one, a split on a card the reserve leaves too little of.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_model_is_labelled_with_the_hardware_it_runs_on() {
        let d = crate::testing::TestDir::new("mm-hw-label");
        let e = ladder_engine(&d, true);
        let entry = sized_entry(&d);
        let h = e.hardware_json(&entry, &[]);
        assert_eq!(
            (h["runs_on"].as_str(), h["chip"].as_str()),
            (Some("cpu"), Some("CPU")),
            "{h}"
        );
        assert_eq!(h["can_run_on"], json!(["cpu"]));
        // a roomy card
        let h = e.hardware_json(&entry, &card(24_000, 23_000));
        assert_eq!(
            (h["runs_on"].as_str(), h["chip"].as_str()),
            (Some("gpu"), Some("GPU")),
            "{h}"
        );
        assert_eq!(h["can_run_on"], json!(["gpu", "cpu"]));
        assert_eq!(h["vram_mib"], 4_084);
        assert!(h["label"].as_str().unwrap().ends_with("· GPU"), "{h}");
        // a card the system's reserve leaves too little of: part of it runs from RAM
        let h = e.hardware_json(&entry, &card(5_000, 4_800));
        assert_eq!(h["runs_on"], "split", "{h}");
        assert!(h["gpu_share"].as_f64().unwrap() < 1.0, "{h}");
        assert!(h["ram_mib"].as_u64().unwrap() > 0, "{h}");
    }

    /// The system's commit is a limit on a GPU load like the card's free memory: a load is held to what the commit
    /// leaves, and when it still fails the error says so.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_systems_commit_holds_a_gpu_load_and_is_named_when_it_runs_out() {
        let d = crate::testing::TestDir::new("mm-ladder-commit");
        std::fs::write(d.join("oom"), "").unwrap();
        let e = ladder_engine(&d, true);
        // a roomy card (policy cap 7,952 MiB), but only 6,000 MiB of commit headroom: (6,000 - 3,072) / 1.2 = 2,440
        *e.fake_devices.lock() = Some(card(10_000, 9_500));
        *e.fake_commit.lock() = Some(6_000);
        assert_eq!(e.commit_budget_mib(), Some(2_440));
        let entry = sized_entry(&d);
        let f = e
            .ensure_loaded(&entry, &LoadOpts::default(), true)
            .await
            .err()
            .expect("the stand-in never answers");
        let targets = fit_targets(&starts(&d));
        // the first start keeps 9,500 - 2,440 = 7,060 MiB free, not the 768 MiB margin
        assert_eq!(targets[0], Some(7_060), "{targets:?}");
        // the planner sees the same limit
        let s = e.settings();
        assert_eq!(e.plannable_free(&card(10_000, 9_500), &s, "m"), 2_440 + 768);
        // and the error names the commit, with what to do about it
        assert!(
            f.message.contains("Windows has only 6000 MiB of memory commit left"),
            "{}",
            f.message
        );
        assert!(f.message.contains("larger page file"), "{}", f.message);
        // with commit to spare there is nothing to say and nothing held
        *e.fake_commit.lock() = Some(60_000);
        assert!(e.commit_hint(Some(4_084)).is_none());
        *e.fake_commit.lock() = None;
        assert_eq!(e.commit_budget_mib(), None);
    }

    /// What loaded is remembered as what the card can hold, a figure too small to be a model's is ignored, and one that
    /// reaches what failed lifts the limit.
    #[test]
    fn a_load_that_worked_is_remembered_as_what_the_card_can_hold() {
        let d = crate::testing::TestDir::new("mm-limits");
        let e = ladder_engine(&d, true);
        let entry = sized_entry(&d);
        let r = e.resolve_serving(&e.settings(), &entry, &LoadOpts::default());
        let devs = card(24_560, 23_000);
        e.note_gpu_success(&devs, &r, 100.0);
        assert!(
            load_limits(&d).is_empty(),
            "a server that holds next to nothing says nothing"
        );
        e.note_gpu_success(&devs, &r, 18_195.0);
        assert_eq!(load_limits(&d).values().next().unwrap().ok_mib, 18_195);
        // two cards: nothing is learned or applied, the free memory is a sum
        let two = [card(8_000, 7_000), card(8_000, 7_000)].concat();
        e.note_gpu_success(&two, &r, 5_000.0);
        assert_eq!(load_limits(&d).len(), 1);
        assert!(e.gpu_limit(&two, &r).is_none());
        // a failure far above what worked is a limit; the budget is held under it, never below what worked
        let d0 = &devs[0];
        assert_eq!(e.note_gpu_failure(d0, &r, 20_594), Some(18_195));
        assert_eq!(e.gpu_limit(&devs, &r).unwrap().failed_mib, 20_594);
        // a load that holds what once failed lifts it
        e.note_gpu_success(&devs, &r, 20_700.0);
        assert!(e.learned_budget_mib(&devs, &r).is_none());
    }

    /// With the fallback off, a failed load stays a failed load (one start, the GPU's own error).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn with_the_fallback_off_a_gpu_failure_is_reported_as_it_is() {
        let d = crate::testing::TestDir::new("mm-ladder-off");
        let e = ladder_engine(&d, false);
        let entry = stand_in_entry(&d, 1);
        let f = e
            .ensure_loaded(&entry, &LoadOpts::default(), true)
            .await
            .err()
            .expect("the stand-in never answers");
        assert_eq!(f.code, "load_failed");
        assert!(!f.message.contains("before this"), "{}", f.message);
        assert_eq!(starts(&d).len(), 1);
    }

    /// A model that crashed the GPU before starts on the CPU at once, until a load asks for the GPU again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_model_that_crashed_the_gpu_before_starts_on_the_cpu_until_asked_to_retry() {
        let d = crate::testing::TestDir::new("mm-ladder-learned");
        let e = ladder_engine(&d, true);
        let entry = stand_in_entry(&d, 1);
        let r = e.resolve_serving(&e.settings(), &entry, &LoadOpts::default());
        e.learn(
            &entry,
            &r,
            "its load on the GPU crashed llama-server (exit 0xC0000409)".into(),
        );
        assert!(e.ensure_loaded(&entry, &LoadOpts::default(), true).await.is_err());
        let seen = starts(&d);
        assert_eq!(seen.len(), 1, "no start on the GPU: {seen:?}");
        assert!(
            seen[0].contains("-ngl 0") && seen[0].contains("-dev none"),
            "{}",
            seen[0]
        );
        // retry_gpu: the GPU first (it crashes again), then the CPU
        let opts = LoadOpts {
            retry_gpu: true,
            ..Default::default()
        };
        assert!(e.ensure_loaded(&entry, &opts, true).await.is_err());
        let seen = starts(&d);
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert!(seen[1].contains("--fit on"), "{}", seen[1]);
        assert!(seen[2].contains("-dev none"), "{}", seen[2]);
    }

    /// A tool (speech, images) that failed on the GPU for the GPU's reasons is run again on the CPU, once.
    #[test]
    fn a_tool_that_the_gpu_stopped_is_run_again_on_the_cpu() {
        let d = crate::testing::TestDir::new("mm-tool-retry");
        let on = Engine::new(d.to_path_buf(), Settings::default());
        let off = Engine::new(
            d.to_path_buf(),
            Settings {
                gpu_fallback: false,
                ..Default::default()
            },
        );
        let fail = |code: &'static str, kind: Option<&str>| {
            let mut f = Fail::new(500, code, "x");
            f.fields = json!({"gpu_failure": kind});
            f
        };
        let crash = fail("tool_failed", Some("crash"));
        let oom = fail("tool_failed", Some("memory"));
        assert_eq!(on.cpu_retry_for_tool(999, &crash).as_deref(), Some("the tool crashed"));
        assert_eq!(
            on.cpu_retry_for_tool(20, &oom).as_deref(),
            Some("the GPU ran out of memory")
        );
        // nothing to gain: it never had the GPU, the failure was not the GPU's, it was cancelled or timed out
        assert!(on.cpu_retry_for_tool(0, &crash).is_none());
        assert!(on.cpu_retry_for_tool(999, &fail("tool_failed", None)).is_none());
        assert!(on.cpu_retry_for_tool(999, &fail("cancelled", Some("crash"))).is_none());
        // the fallback is off
        assert!(off.cpu_retry_for_tool(999, &crash).is_none());
    }

    /// What `after_failure` makes of a failed start.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_a_failure_the_gpu_caused_is_retried_and_only_where_it_can_help() {
        let d = crate::testing::TestDir::new("mm-ladder-rules");
        let g = Ground {
            devices: &[],
            need_mib: None,
            model: "m".into(),
        };
        let e = ladder_engine(&d, true);
        let entry = stand_in_entry(&d, 1);
        let s = e.settings();
        let r = e.resolve_serving(&s, &entry, &LoadOpts::default());
        let fail = |kind: Option<&str>, code: i32| {
            let mut f = Fail::new(500, "load_failed", "x");
            f.fields = json!({"exit_code": code, "gpu_failure": kind});
            f
        };
        let roomier = s.vram_margin_mib.max(512) + 2048;
        // out of memory: more room first, then the CPU
        let (next, _, crash) = e
            .after_failure(&s, &r, &entry, &Try::Asked, &fail(Some("memory"), 1), &g, 0)
            .await
            .unwrap();
        assert_eq!((next, crash), (Try::Roomier(roomier), false));
        let (next, why, crash) = e
            .after_failure(&s, &r, &entry, &Try::Roomier(roomier), &fail(Some("memory"), 1), &g, 0)
            .await
            .unwrap();
        assert_eq!((next, crash), (Try::Cpu, false));
        assert_eq!(why, "the GPU ran out of memory");
        // a crash goes to the CPU at once, and is the kind that is remembered
        let (next, why, crash) = e
            .after_failure(&s, &r, &entry, &Try::Asked, &fail(Some("crash"), -1_073_740_791), &g, 0)
            .await
            .unwrap();
        assert_eq!((next, crash), (Try::Cpu, true));
        assert_eq!(why, "its load on the GPU crashed llama-server (exit 0xC0000409)");
        // layers somebody fixed are not left to --fit: out of memory goes straight to the CPU
        let fixed = e.resolve_serving(
            &s,
            &entry,
            &LoadOpts {
                gpu_layers: Some(20),
                ..Default::default()
            },
        );
        let (next, _, _) = e
            .after_failure(&s, &fixed, &entry, &Try::Asked, &fail(Some("memory"), 1), &g, 0)
            .await
            .unwrap();
        assert_eq!(next, Try::Cpu);
        // nothing to retry: the CPU start failed, no layers were on the GPU, or the failure was not the GPU's
        assert!(e
            .after_failure(&s, &r, &entry, &Try::Cpu, &fail(Some("crash"), 1), &g, 0)
            .await
            .is_none());
        let cpu = e.resolve_serving(
            &s,
            &entry,
            &LoadOpts {
                gpu_layers: Some(0),
                ..Default::default()
            },
        );
        assert!(e
            .after_failure(&s, &cpu, &entry, &Try::Asked, &fail(Some("crash"), 1), &g, 0)
            .await
            .is_none());
        assert!(e
            .after_failure(&s, &r, &entry, &Try::Asked, &fail(None, 3), &g, 0)
            .await
            .is_none());
        // the fallback is off
        let off = Settings {
            gpu_fallback: false,
            ..s.clone()
        };
        assert!(e
            .after_failure(&off, &r, &entry, &Try::Asked, &fail(Some("crash"), 1), &g, 0)
            .await
            .is_none());
        // the CPU cannot hold it either: the GPU's error is the answer
        let huge = stand_in_entry(&d, u64::MAX / 4);
        assert!(e
            .after_failure(&s, &r, &huge, &Try::Asked, &fail(Some("crash"), 1), &g, 0)
            .await
            .is_none());
    }

    /// Runs the real llama-quantize that is built in: a failure carries the program's own words, and a cancel kills it.
    #[cfg(feature = "bundled-engine")]
    #[tokio::test]
    async fn running_the_built_in_tools() {
        let d = crate::testing::TestDir::new("mm-tool");
        let e = Engine::new(
            d.clone(),
            Settings {
                ollama_url: String::new(),
                ..Default::default()
            },
        );
        assert!(e.tool("llama-quantize").is_some() && e.tool("llama-tts").is_some());
        let mut lines = vec![];
        let args = vec!["--no-such-option".to_string()];
        let f = e
            .run_tool("llama-quantize", &args, Duration::from_secs(60), &mut |l| {
                lines.push(l.to_string());
                true
            })
            .await
            .unwrap_err();
        assert!(
            f.status == 500 && f.code == "tool_failed" && f.message.contains("llama-quantize failed"),
            "{}",
            f.message
        );
        assert!(!lines.is_empty());
        // the first line of output cancels it
        let f = e
            .run_tool("llama-quantize", &args, Duration::from_secs(60), &mut |_| false)
            .await
            .unwrap_err();
        assert_eq!(f.code, "cancelled");
        // llama-tts without a model says what is missing
        let f = e
            .run_tool("llama-tts", &[], Duration::from_secs(60), &mut |_| true)
            .await
            .unwrap_err();
        assert!(
            f.message.contains("prompt") || f.message.contains("failed"),
            "{}",
            f.message
        );
        e.unload_all();
        let _ = std::fs::remove_dir_all(&d);
    }
}

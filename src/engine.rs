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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::catalog::{self, ModelEntry, Roots};

/// An error with the HTTP status the hub should answer with.
#[derive(Debug)]
pub struct Fail {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}

impl Fail {
    pub fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
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
    /// Extra folders of .gguf files.
    pub model_dirs: Vec<PathBuf>,
    /// Extra Ollama stores (the folder that holds `manifests` and `blobs`).
    pub ollama_dirs: Vec<PathBuf>,
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
    /// Load a model on its first request.
    pub autoload: bool,
    pub load_timeout_s: u64,
    /// Unload a model after this many idle seconds (0 = never).
    pub idle_unload_s: u64,
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
            model_dirs: vec![],
            ollama_dirs: vec![],
            hf_dirs: vec![],
            ollama_url: "http://127.0.0.1:11434".into(),
            max_loaded: 2,
            ctx_size: 8192,
            gpu_layers: 999,
            parallel: 2,
            autoload: true,
            load_timeout_s: 300,
            idle_unload_s: 0,
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
            Ok(s) => toml::from_str(&s).map_err(|e| format!("{}: {e}", home.join("config.toml").display())),
            Err(_) => Ok(Self::default()),
        }
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
    let mut a = vec!["--spec-draft-model".to_string(), d.path.display().to_string()];
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
    last_used: AtomicU64,
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
    pub fn idle_s(&self) -> u64 {
        now_s().saturating_sub(self.last_used.load(Ordering::Relaxed))
    }
    pub fn describe(&self) -> Value {
        json!({"model": self.model, "backend": "llama.cpp", "pid": self.pid, "port": self.port,
               "uptime_s": self.started.elapsed().as_secs(), "idle_s": self.idle_s(),
               "embeddings": self.embeddings, "reranking": self.reranking, "args": self.args, "log": self.log,
               "lora": self.extras.lora, "draft": self.extras.draft})
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
        if let Some(f) = self.on_stop.lock().take() {
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
    /// LoRA adapters and draft models per model id (see [`Extras`]).
    extras: Mutex<HashMap<String, Extras>>,
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
            extras: Mutex::new(load_extras(&home)),
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
        s.save(&self.home)?;
        *self.settings.write() = s;
        *self.llama_version.lock() = None;
        self.refresh();
        Ok(())
    }

    pub fn roots(&self) -> Roots {
        let s = self.settings.read();
        let mut r = Roots::detected();
        r.ollama.splice(0..0, s.ollama_dirs.iter().cloned());
        r.huggingface.splice(0..0, s.hf_dirs.iter().cloned());
        r.folders.splice(0..0, s.model_dirs.iter().cloned());
        r.folders.push(self.home.join("models"));
        let downloads = s.download_dir.clone().unwrap_or_else(|| self.home.clone());
        r.huggingface.push(downloads.join("hub"));
        r.ollama.push(downloads.join("ollama"));
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
            Err(Fail::new(
                500,
                "tool_failed",
                format!(
                    "{name} failed ({status}):\n{}",
                    tail.into_iter().collect::<Vec<_>>().join("\n")
                ),
            ))
        }
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
            self.make_room();
        }
        // A reload keeps the old instance serving while the new one loads (both are in memory for a while), then
        // `install` switches over.
        let t0 = Instant::now();
        let inst = Arc::new(self.spawn(entry, opts).await?);
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
            last_used: AtomicU64::new(now_s()),
            inflight: AtomicUsize::new(0),
            retired: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            on_stop: Mutex::new(on_stop),
        });
        self.install(inst.clone());
        inst
    }

    fn make_room(&self) {
        let max = self.settings.read().max_loaded.max(1);
        loop {
            let victim = {
                let l = self.loaded.lock();
                if l.len() < max {
                    return;
                }
                l.values().max_by_key(|i| i.idle_s()).map(|i| i.model.clone())
            };
            match victim {
                Some(v) => {
                    self.unload(&v);
                }
                None => return,
            }
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

    async fn spawn(&self, entry: &ModelEntry, opts: &LoadOpts) -> Result<Instance> {
        let exe = self.llama_server().ok_or_else(|| {
            Fail::unavailable(
                "llama-server was not found: set llama_server with config.set, put it in <home>/engines/llama.cpp, \
                 or on PATH",
            )
        })?;
        let s = self.settings();
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
            "-c".into(),
            opts.ctx_size.unwrap_or(s.ctx_size).to_string(),
            "-ngl".into(),
            opts.gpu_layers.unwrap_or(s.gpu_layers).to_string(),
            "-np".into(),
            opts.parallel.unwrap_or(s.parallel).max(1).to_string(),
            "--metrics".into(),
        ];
        if let Some(t) = opts.threads {
            args.extend(["-t".into(), t.to_string()]);
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
            .stdin(std::process::Stdio::null())
            .stdout(out)
            .stderr(err)
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
        let child = cmd
            .spawn()
            .map_err(|e| Fail::unavailable(format!("cannot start {}: {e}", exe.display())))?;
        let pid = child.id().unwrap_or(0);
        #[cfg(windows)]
        if let (Some(job), Some(h)) = (&self.job, child.raw_handle()) {
            job.assign(h);
        }
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
            last_used: AtomicU64::new(now_s()),
            inflight: AtomicUsize::new(0),
            retired: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            on_stop: Mutex::new(None),
        };
        // Wait until it answers /health with 200 (it answers 503 while loading).
        let deadline = Instant::now() + Duration::from_secs(s.load_timeout_s.max(5));
        let url = format!("http://127.0.0.1:{port}/health");
        loop {
            if let Some(status) = inst.child.lock().as_mut().and_then(|c| c.try_wait().ok().flatten()) {
                inst.stop();
                return Err(Fail::new(
                    500,
                    "load_failed",
                    format!(
                        "llama-server exited ({status}) while loading {}:\n{}",
                        entry.id,
                        tail(&log, 15)
                    ),
                ));
            }
            if let Ok(r) = self.http.get(&url).timeout(Duration::from_secs(2)).send().await {
                if r.status().is_success() {
                    if let Err(f) = self.apply_lora(&inst).await {
                        inst.stop();
                        return Err(f);
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
            if dead || (idle > 0 && i.idle_s() > idle) {
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
        let base = self.settings.read().ollama_url.trim_end_matches('/').to_string();
        if base.is_empty() {
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

fn load_extras(home: &Path) -> HashMap<String, Extras> {
    std::fs::read(home.join("model_options.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
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

pub(crate) fn no_window(cmd: &mut tokio::process::Command) {
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = cmd;
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
        let d = std::env::temp_dir().join(format!("mm-settings-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(Settings::load(&d).unwrap().max_loaded, 2);
        std::fs::write(d.join("config.toml"), "max_loaded = 5\nollama_url = \"\"\n").unwrap();
        let s = Settings::load(&d).unwrap();
        assert_eq!((s.max_loaded, s.ollama_url.as_str(), s.gpu_layers), (5, "", 999));
        s.save(&d).unwrap();
        assert_eq!(Settings::load(&d).unwrap().max_loaded, 5);
        std::fs::write(d.join("config.toml"), "max_loaded = \"x\"").unwrap();
        assert!(Settings::load(&d).is_err());
    }

    #[test]
    fn resolving_names() {
        let d = std::env::temp_dir().join(format!("mm-resolve-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
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
                "--spec-draft-model",
                "d.gguf",
                "--spec-draft-n-min",
                "2",
                "--spec-draft-ngl",
                "0"
            ]
        );
    }

    #[tokio::test]
    async fn extras_are_saved_and_a_missing_tool_is_501() {
        let d = std::env::temp_dir().join(format!("mm-extras-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
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

    /// Runs the real llama-quantize that is built in: a failure carries the program's own words, and a cancel kills it.
    #[cfg(feature = "bundled-engine")]
    #[tokio::test]
    async fn running_the_built_in_tools() {
        let d = std::env::temp_dir().join(format!("mm-tool-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
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

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
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// Path to `llama-server`. Unset: MM_LLAMA_SERVER, then `<home>/engines/llama.cpp/`, then PATH.
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
    #[serde(default)]
    pub extra_args: Vec<String>,
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
    last_used: AtomicU64,
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
               "embeddings": self.embeddings, "args": self.args, "log": self.log})
    }
    fn stop(&self) {
        if let Some(mut c) = self.child.lock().take() {
            let _ = c.start_kill();
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
    load_lock: tokio::sync::Mutex<()>,
    pub metrics: Mutex<BTreeMap<String, ModelMetrics>>,
    pub http: reqwest::Client,
    llama_version: Mutex<Option<(PathBuf, String)>>,
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
            home,
            settings: RwLock::new(settings),
            catalog: RwLock::new(vec![]),
            loaded: Mutex::new(HashMap::new()),
            load_lock: tokio::sync::Mutex::new(()),
            metrics: Mutex::new(BTreeMap::new()),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
            llama_version: Mutex::new(None),
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
        r
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
        if let Some(i) = self.loaded.lock().get(&entry.id).cloned() {
            if !(explicit && opts.embeddings != i.embeddings) {
                i.touch();
                return Ok(i);
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
        if let Some(i) = self.loaded.lock().get(&entry.id).cloned() {
            if !(explicit && opts.embeddings != i.embeddings) {
                return Ok(i);
            }
        }
        self.unload(&entry.id);
        self.make_room();
        let t0 = Instant::now();
        let inst = Arc::new(self.spawn(entry, opts).await?);
        self.loaded.lock().insert(entry.id.clone(), inst.clone());
        let mut m = self.metrics.lock();
        let mm = m.entry(entry.id.clone()).or_default();
        mm.loads += 1;
        mm.last_load_ms = t0.elapsed().as_millis() as u64;
        Ok(inst)
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

    pub fn unload(&self, id: &str) -> bool {
        let inst = self.loaded.lock().remove(id);
        match inst {
            Some(i) => {
                i.stop();
                true
            }
            None => false,
        }
    }

    pub fn unload_all(&self) -> usize {
        let all: Vec<_> = self.loaded.lock().drain().map(|(_, i)| i).collect();
        for i in &all {
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
        let mut args: Vec<String> = vec![
            "-m".into(),
            entry.path.display().to_string(),
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
        if opts.embeddings {
            args.push("--embeddings".into());
        } else if let Some(p) = &entry.mmproj {
            args.extend(["--mmproj".into(), p.display().to_string()]);
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
            last_used: AtomicU64::new(now_s()),
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
    }

    // ---- requests -----------------------------------------------------------------------------------------

    /// Send an OpenAI-style request (`path` like "/v1/chat/completions") to where `body.model` lives.
    pub async fn forward(&self, path: &str, mut body: Value) -> Result<(String, reqwest::Response)> {
        let name = body.get("model").and_then(Value::as_str).unwrap_or("").to_string();
        let target = self.resolve(&name)?;
        let (id, req) = match target {
            Target::Llama(entry) => {
                let opts = LoadOpts {
                    embeddings: path.ends_with("/embeddings"),
                    ..Default::default()
                };
                let inst = self.ensure_loaded(&entry, &opts, false).await?;
                if path.ends_with("/embeddings") && !inst.embeddings {
                    return Err(Fail::new(
                        409,
                        "wrong_mode",
                        format!(
                            "{} is loaded for chat; load it with embeddings: true to embed",
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
        Ok((id, resp))
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

fn no_window(cmd: &mut tokio::process::Command) {
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = cmd;
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
}

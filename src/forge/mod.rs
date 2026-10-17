//! The Forge: ModelMistress's training system (docs/native/architecture.md section 4). A job runner in the hub plus a
//! managed Python worker (PyTorch on ROCm) in its own environment. The hub owns scheduling, runs and telemetry; the
//! worker computes. They talk JSON lines on stdio (`protocol`).
//!
//!   forge.status       where the Forge lives, whether it is installed, the last preflight, running runs
//!   forge.install      job: create the environment, install the pinned requirements, run the preflight
//!   forge.preflight    prove that an optimizer step runs on the GPU (waits; the answer is the result)
//!   forge.run          job: run a pipeline step (`python -m mm_forge <step>`) with JSON arguments
//!   forge.runs         the runs, newest first
//!   forge.run.get      one run: result, metrics, artifacts, log
//!   forge.run.cancel   stop a run (its whole process tree)
//!   forge.load         pick the engine for a model by its format and load it (GGUF: llama.cpp; Hugging Face weights
//!                      llama.cpp cannot run: the Forge worker); records where it runs
//!   forge.generate     tokens with logprobs from a model the Forge worker serves
//!   forge.unload       stop such a model's worker
//!
//! Settings (config.toml, or the environment variable in brackets): `forge_dir` [MM_FORGE_DIR] holds the Python
//! environment (`<forge_dir>/env`); `forge_data_dir` [MM_FORGE_DATA_DIR] holds weights, checkpoints and outputs;
//! `forge_src` [MM_FORGE_SRC] is the `forge` folder with `mm_forge/` and the requirements; `forge_python`
//! [MM_FORGE_PYTHON] is the interpreter that creates the environment. Defaults: under `<home>/forge`.

pub mod protocol;
pub mod runner;
pub mod runtime;
pub mod serve;
pub mod store;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::engine::{Fail, Result};
use crate::hub::Hub;
use runner::{exec, Cmd, Ctl};
use store::{Filter, Store};

/// One per home folder.
pub struct Forge {
    pub store: Arc<Store>,
    active: Mutex<HashMap<String, Arc<Ctl>>>,
    servers: tokio::sync::Mutex<HashMap<String, Arc<serve::Server>>>,
    placements: Mutex<HashMap<String, Value>>,
}

static FORGES: OnceLock<Mutex<HashMap<PathBuf, Arc<Forge>>>> = OnceLock::new();

pub fn forge_for(hub: &Hub) -> Arc<Forge> {
    let home = hub.engine.home.clone();
    FORGES
        .get_or_init(Default::default)
        .lock()
        .entry(home.clone())
        .or_insert_with(|| {
            Arc::new(Forge {
                store: Arc::new(Store::new(home.join("forge").join("runs.db"))),
                active: Default::default(),
                servers: Default::default(),
                placements: Default::default(),
            })
        })
        .clone()
}

/// Let go of the forge of a home folder (tests, before they remove it).
pub(crate) fn forget_forge(home: &Path) {
    if let Some(m) = FORGES.get() {
        m.lock().remove(home);
    }
}

// ---- where things are --------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Paths {
    pub dir: PathBuf,
    pub data: PathBuf,
    pub src: Option<PathBuf>,
    pub env: PathBuf,
    pub python: PathBuf,
}

fn pick(setting: &Option<PathBuf>, var: &str) -> Option<PathBuf> {
    setting
        .clone()
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from))
}

fn has_package(p: &Path) -> bool {
    p.join("mm_forge").join("__init__.py").is_file()
}

/// The `forge` folder: the setting, the environment, `<forge_dir>/src`, or the one next to this program or the
/// current folder (a source checkout), looking up a few levels.
fn find_src(setting: Option<PathBuf>, dir: &Path) -> Option<PathBuf> {
    if let Some(p) = pick(&setting, "MM_FORGE_SRC") {
        return has_package(&p).then_some(p);
    }
    let mut starts: Vec<PathBuf> = vec![dir.join("src")];
    if let Ok(exe) = std::env::current_exe() {
        starts.extend(exe.parent().map(Path::to_path_buf));
    }
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    for s in starts {
        let mut p = Some(s.as_path());
        for _ in 0..8 {
            let Some(q) = p else { break };
            if has_package(q) {
                return Some(q.to_path_buf());
            }
            if has_package(&q.join("forge")) {
                return Some(q.join("forge"));
            }
            p = q.parent();
        }
    }
    None
}

pub fn paths(hub: &Hub) -> Paths {
    let st = hub.engine.settings();
    let dir = pick(&st.forge_dir, "MM_FORGE_DIR").unwrap_or_else(|| hub.engine.home.join("forge"));
    let data = pick(&st.forge_data_dir, "MM_FORGE_DATA_DIR").unwrap_or_else(|| dir.join("data"));
    let env = dir.join("env");
    let python = if cfg!(windows) {
        env.join("Scripts").join("python.exe")
    } else {
        env.join("bin").join("python")
    };
    Paths {
        src: find_src(st.forge_src.clone(), &dir),
        dir,
        data,
        env,
        python,
    }
}

fn requirements_file(src: &Path) -> PathBuf {
    src.join(if cfg!(windows) {
        "requirements-rocm-windows.txt"
    } else {
        "requirements-rocm-linux.txt"
    })
}

fn installed(p: &Paths) -> bool {
    p.python.is_file()
}

fn require_installed(p: &Paths) -> Result<PathBuf> {
    let src = p.src.clone().ok_or_else(|| {
        Fail::new(
            409,
            "no_forge_src",
            "the Forge's Python package (a folder holding mm_forge/) was not found: set forge_src or MM_FORGE_SRC",
        )
    })?;
    if !installed(p) {
        return Err(Fail::new(
            409,
            "not_installed",
            format!(
                "the Forge environment is not installed ({}): run forge.install",
                p.env.display()
            ),
        ));
    }
    Ok(src)
}

/// The command for `python -m mm_forge <step>`.
fn worker_cmd(hub: &Hub, p: &Paths, src: &Path, step: &str, stdin: Option<String>) -> Cmd {
    let e = &hub.engine;
    let mut env = vec![
        ("PYTHONPATH".to_string(), src.display().to_string()),
        ("PYTHONIOENCODING".into(), "utf-8".into()),
        ("PYTHONUNBUFFERED".into(), "1".into()),
        ("MM_FORGE_DATA".into(), p.data.display().to_string()),
        ("MM_FORGE_HUB_HOME".into(), e.home.display().to_string()),
    ];
    if let Some(q) = e.tool("llama-quantize") {
        env.push(("MM_FORGE_LLAMA_QUANTIZE".into(), q.display().to_string()));
    }
    if let Some(s) = e.llama_server() {
        env.push(("MM_FORGE_LLAMA_SERVER".into(), s.display().to_string()));
    }
    if let Some(l) = runtime::llama_cpp_src(Some(src)) {
        env.push(("MM_FORGE_LLAMA_CPP".into(), l.display().to_string()));
    }
    let _ = std::fs::create_dir_all(&p.data);
    Cmd {
        program: p.python.clone(),
        args: ["-u", "-m", "mm_forge", step].map(String::from).to_vec(),
        env,
        cwd: Some(p.data.clone()),
        stdin,
        protocol: true,
        timeout: None,
    }
}

fn new_run_id() -> String {
    format!(
        "run-{}-{}",
        chrono::Utc::now().format("%Y%m%d-%H%M%S"),
        &uuid::Uuid::new_v4().simple().to_string()[..4]
    )
}

async fn wait(job: &Arc<crate::pull::Job>, limit: Duration) {
    let t = std::time::Instant::now();
    while job.status() == "running" && t.elapsed() < limit {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn failure_of(status: &str, code: &str, message: String) -> Fail {
    let _ = status;
    match code {
        "cancelled" => Fail::new(409, "cancelled", message),
        _ => Fail::new(500, "failed", message),
    }
}

/// Starts a pipeline step as a run and a job. Returns the run id and the job.
async fn start_step(
    hub: &Hub,
    step: &str,
    args: Value,
    timeout: Option<Duration>,
) -> Result<(String, Arc<crate::pull::Job>)> {
    if step.is_empty()
        || !step
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(Fail::invalid("step must be lowercase letters, digits and underscores"));
    }
    let forge = forge_for(hub);
    let p = paths(hub);
    let src = require_installed(&p)?;
    let id = new_run_id();
    forge.store.create_run(&id, step, &args)?;
    let mut cmd = worker_cmd(hub, &p, &src, step, Some(args.to_string()));
    cmd.timeout = timeout;
    let ctl = Arc::new(Ctl::default());
    forge.active.lock().insert(id.clone(), ctl.clone());
    let (f2, id2, step2) = (forge.clone(), id.clone(), step.to_string());
    let job = hub.jobs.start_task("forge", id.clone(), move |job| async move {
        let out = exec(&f2.store, &id2, &cmd, &ctl, Some(&job)).await;
        f2.active.lock().remove(&id2);
        if out.success(true) {
            let _ = f2
                .store
                .finish(&id2, "done", out.exit_code, out.result.as_ref(), None, None);
            Ok(json!({"run": id2, "step": step2, "result": out.result}))
        } else {
            let (code, msg) = out.failure(true);
            let status = if code == "cancelled" { "cancelled" } else { "failed" };
            let _ = f2.store.finish(
                &id2,
                status,
                out.exit_code,
                out.result.as_ref(),
                Some(&msg),
                Some(&code),
            );
            Err(failure_of(status, &code, msg))
        }
    });
    Ok((id, job))
}

fn with_run(mut job: Value, run: &str) -> Value {
    job["run"] = json!(run);
    job
}

// ---- operations --------------------------------------------------------------------------------------------------

fn op(id: &str, summary: &str, mutating: bool, destructive: bool, props: Value, required: &[&str]) -> Value {
    json!({"id": id, "group": "forge", "summary": summary, "mutating": mutating, "destructive": destructive,
           "input_schema": {"type": "object", "properties": props, "required": required, "additionalProperties": false}})
}

pub fn operations() -> Vec<Value> {
    vec![
        op("forge.status", "The Forge: where it lives, whether it is installed, the last preflight, the runs in progress", false, false, json!({}), &[]),
        op(
            "forge.install",
            "Install the Forge's Python environment (PyTorch for ROCm) and run the preflight. Runs as a job unless wait is true",
            true,
            false,
            json!({
                "python": {"type": "string", "description": "the interpreter that creates the environment (default: forge_python, MM_FORGE_PYTHON, else python)"},
                "requirements": {"type": "string", "description": "a requirements file (default: forge/requirements-rocm-windows.txt)"},
                "pip_args": {"type": "array", "items": {"type": "string"}},
                "skip_preflight": {"type": "boolean"},
                "wait": {"type": "boolean"}
            }),
            &[],
        ),
        op(
            "forge.preflight",
            "Prove that an optimizer step really runs on the GPU (forward, backward, step, compared with the CPU); reports the device, VRAM, torch and HIP versions and bf16",
            true,
            false,
            json!({"seed": {"type": "integer"}, "steps": {"type": "integer", "minimum": 1, "maximum": 50}, "memory_gib": {"type": "number"}}),
            &[],
        ),
        op(
            "forge.run",
            "Run a pipeline step (python -m mm_forge <step>) with JSON arguments, as a job with progress, logs and metrics. Steps: env_info, preflight, dusty_convert, dusty_equiv, generate, generate_serve, lora_train, qlora_train, merge_lora, merged_to_gguf, lora_to_gguf",
            true,
            false,
            json!({
                "step": {"type": "string", "description": "the step's name"},
                "args": {"type": "object", "description": "the step's arguments"},
                "timeout_s": {"type": "integer", "minimum": 1},
                "wait": {"type": "boolean", "description": "answer only when it has finished"}
            }),
            &["step"],
        ),
        op(
            "forge.runs",
            "The Forge's runs, newest first",
            false,
            false,
            json!({"step": {"type": "string"}, "status": {"type": "string", "enum": ["running", "done", "failed", "cancelled", "interrupted"]}, "limit": {"type": "integer", "minimum": 1, "maximum": 500}}),
            &[],
        ),
        op(
            "forge.run.get",
            "One run: arguments, status, progress, result, metrics (summary and points), artifacts and the end of its log",
            false,
            false,
            json!({
                "run": {"type": "string"},
                "metric": {"type": "string", "description": "only this metric"},
                "max_points": {"type": "integer", "minimum": 2, "maximum": 5000},
                "log_lines": {"type": "integer", "minimum": 0, "maximum": 2000}
            }),
            &["run"],
        ),
        op("forge.run.cancel", "Stop a run: its process and everything the process started", true, true, json!({"run": {"type": "string"}}), &["run"]),
        op(
            "forge.load",
            "Load a model in any format on the engine that fits: GGUF on llama.cpp; Hugging Face safetensors/PyTorch on llama.cpp after conversion when it knows the architecture, else on the Forge worker (PyTorch, GPU or CPU); ONNX is planned only. Records where it runs",
            true,
            false,
            json!({
                "model": {"type": "string", "description": "a model id from model.list, or the path of a file or folder"},
                "device": {"type": "string", "enum": ["auto", "gpu", "cpu"]},
                "dtype": {"type": "string", "description": "Forge worker: auto, bfloat16, float16 or float32"},
                "plan_only": {"type": "boolean", "description": "only say what would be done"}
            }),
            &["model"],
        ),
        op(
            "forge.generate",
            "Generate with a model the Forge worker serves: tokens with logprobs (and the top alternatives)",
            false,
            false,
            json!({
                "model": {"type": "string", "description": "the model as given to forge.load"},
                "prompt": {"type": "string"},
                "prompt_ids": {"type": "array", "items": {"type": "integer"}},
                "messages": {"type": "array", "items": {"type": "object"}},
                "max_tokens": {"type": "integer", "minimum": 1},
                "temperature": {"type": "number", "minimum": 0},
                "top_p": {"type": "number"},
                "seed": {"type": "integer"},
                "top_logprobs": {"type": "integer", "minimum": 0, "maximum": 20},
                "stop_ids": {"type": "array", "items": {"type": "integer"}}
            }),
            &["model"],
        ),
        op("forge.unload", "Stop the Forge worker serving a model", true, false, json!({"model": {"type": "string"}}), &["model"]),
    ]
}

fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| Fail::invalid(format!("{k} is required")))
}

pub async fn call(hub: &Hub, id: &str, a: &Value) -> Result<Value> {
    let forge = forge_for(hub);
    match id {
        "forge.status" => status(hub, &forge).await,
        "forge.install" => install(hub, &forge, a).await,
        "forge.preflight" => {
            let (run, job) = start_step(hub, "preflight", a.clone(), Some(Duration::from_secs(900))).await?;
            wait(&job, Duration::from_secs(960)).await;
            let d = job.describe();
            match d["status"].as_str() {
                Some("done") => {
                    let r = d["result"]["result"].clone();
                    Ok(json!({"run": run, "ok": r["ok"], "result": r}))
                }
                _ => Err(Fail::new(
                    500,
                    "failed",
                    format!("{run}: {}", d["message"].as_str().unwrap_or("failed")),
                )),
            }
        }
        "forge.run" => {
            let step = s(a, "step")?;
            let args = a.get("args").cloned().unwrap_or(json!({}));
            if !args.is_object() {
                return Err(Fail::invalid("args must be an object"));
            }
            let timeout = a.get("timeout_s").and_then(Value::as_u64).map(Duration::from_secs);
            let (run, job) = start_step(hub, step, args, timeout).await?;
            if a["wait"].as_bool().unwrap_or(false) {
                wait(&job, Duration::from_secs(48 * 3600)).await;
            }
            Ok(with_run(job.describe(), &run))
        }
        "forge.runs" => {
            let f = Filter {
                step: a.get("step").and_then(Value::as_str).map(String::from),
                status: a.get("status").and_then(Value::as_str).map(String::from),
                limit: a.get("limit").and_then(Value::as_u64).unwrap_or(50).min(500) as usize,
            };
            Ok(json!(forge.store.list_runs(&f)?))
        }
        "forge.run.get" => {
            let run = s(a, "run")?;
            let mut v = forge
                .store
                .get_run(run)?
                .ok_or_else(|| Fail::not_found(format!("no run {run}")))?;
            let pts = a.get("max_points").and_then(Value::as_u64).unwrap_or(200) as usize;
            v["metrics"] = forge.store.metrics(run, a.get("metric").and_then(Value::as_str), pts)?;
            v["artifacts"] = json!(forge.store.artifacts(run)?);
            let n = a.get("log_lines").and_then(Value::as_u64).unwrap_or(50) as usize;
            v["log"] = json!(forge.store.log_tail(run, n)?);
            Ok(v)
        }
        "forge.run.cancel" => {
            let run = s(a, "run")?;
            let ctl = forge.active.lock().get(run).cloned();
            match ctl {
                Some(c) => {
                    c.cancel();
                    Ok(json!({"run": run, "cancelled": true}))
                }
                None => {
                    let v = forge
                        .store
                        .get_run(run)?
                        .ok_or_else(|| Fail::not_found(format!("no run {run}")))?;
                    Ok(json!({"run": run, "cancelled": false, "status": v["status"],
                              "note": "the run is not running in this hub"}))
                }
            }
        }
        "forge.load" => load(hub, &forge, a).await,
        "forge.generate" => generate(&forge, a).await,
        "forge.unload" => {
            let key = s(a, "model")?;
            let srv = forge.servers.lock().await.remove(key);
            forge.placements.lock().remove(key);
            if let Some(sv) = &srv {
                sv.stop();
            }
            Ok(json!({"model": key, "unloaded": srv.is_some()}))
        }
        _ => Err(Fail::not_found(format!("no operation {id}"))),
    }
}

async fn status(hub: &Hub, forge: &Arc<Forge>) -> Result<Value> {
    let p = paths(hub);
    let last = forge.store.last_result("preflight")?;
    let running: Vec<String> = forge.active.lock().keys().cloned().collect();
    let servers: Vec<Value> = forge
        .servers
        .lock()
        .await
        .iter()
        .map(|(k, v)| json!({"model": k, "run": v.run, "alive": v.alive()}))
        .collect();
    Ok(json!({
        "forge_dir": p.dir, "data_dir": p.data, "src": p.src, "env": p.env, "python": p.python,
        "installed": installed(&p),
        "requirements": p.src.as_deref().map(requirements_file),
        "runs_db": forge.store.path(),
        "runs_total": forge.store.count_runs()?,
        "running": running,
        "last_preflight": last.map(|(run, r)| json!({
            "run": run, "ok": r["ok"], "device_kind": r["device_kind"], "device_name": r["device_name"],
            "arch": r["arch"], "vram_total_bytes": r["vram_total_bytes"], "torch": r["torch"], "hip": r["hip"],
            "bf16_supported": r["bf16_supported"]})),
        "servers": servers,
        "placements": forge.placements.lock().clone(),
    }))
}

async fn install(hub: &Hub, forge: &Arc<Forge>, a: &Value) -> Result<Value> {
    let p = paths(hub);
    let src = p.src.clone().ok_or_else(|| {
        Fail::new(
            409,
            "no_forge_src",
            "the Forge's Python package was not found: set forge_src or MM_FORGE_SRC",
        )
    })?;
    let st = hub.engine.settings();
    let base: PathBuf = a
        .get("python")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| pick(&st.forge_python, "MM_FORGE_PYTHON"))
        .unwrap_or_else(|| PathBuf::from("python"));
    let req = a
        .get("requirements")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| requirements_file(&src));
    if !req.is_file() {
        return Err(Fail::invalid(format!(
            "no requirements file {}: give requirements (the Forge ships one per platform it supports)",
            req.display()
        )));
    }
    let pip_args: Vec<String> = a
        .get("pip_args")
        .and_then(Value::as_array)
        .map(|l| l.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let skip_preflight = a["skip_preflight"].as_bool().unwrap_or(false);
    let id = new_run_id();
    forge.store.create_run(&id, "install", a)?;
    let ctl = Arc::new(Ctl::default());
    forge.active.lock().insert(id.clone(), ctl.clone());
    let (f2, id2, hub2) = (forge.clone(), id.clone(), hub.clone());
    let tmp = p.data.join("tmp");
    let job = hub.jobs.start_task("forge-install", id.clone(), move |job| async move {
        let _ = std::fs::create_dir_all(&tmp);
        let step = |program: PathBuf, args: Vec<String>| Cmd {
            program,
            args,
            // pip's downloads and builds go beside the data, not onto the system drive
            env: vec![
                ("TMP".into(), tmp.display().to_string()),
                ("TEMP".into(), tmp.display().to_string()),
                ("PYTHONIOENCODING".into(), "utf-8".into()),
            ],
            protocol: false,
            ..Default::default()
        };
        let mut phases: Vec<(&str, Cmd)> = vec![];
        if !installed(&p) {
            phases.push((
                "creating the environment",
                step(
                    base.clone(),
                    vec!["-m".into(), "venv".into(), p.env.display().to_string()],
                ),
            ));
        }
        let mut pip = vec![
            "-m".to_string(),
            "pip".into(),
            "install".into(),
            "--no-cache-dir".into(),
        ];
        pip.extend(pip_args);
        pip.extend(["-r".to_string(), req.display().to_string()]);
        phases.push(("installing the requirements", step(p.python.clone(), pip)));
        let total = phases.len() as u64 + u64::from(!skip_preflight);
        for (i, (what, cmd)) in phases.into_iter().enumerate() {
            job.progress(i as u64, total, what);
            let _ = f2.store.add_log(&id2, "info", what);
            let out = exec(&f2.store, &id2, &cmd, &ctl, Some(&job)).await;
            if !out.success(false) {
                f2.active.lock().remove(&id2);
                let (code, msg) = out.failure(false);
                let status = if code == "cancelled" { "cancelled" } else { "failed" };
                let _ = f2
                    .store
                    .finish(&id2, status, out.exit_code, None, Some(&msg), Some(&code));
                return Err(failure_of(status, &code, format!("{what}: {msg}")));
            }
        }
        f2.active.lock().remove(&id2);
        let mut result = json!({"env": p.env, "python": p.python});
        if !skip_preflight {
            job.progress(total - 1, total, "preflight");
            let (run, pj) = start_step(&hub2, "preflight", json!({}), Some(Duration::from_secs(900))).await?;
            wait(&pj, Duration::from_secs(960)).await;
            result["preflight_run"] = json!(run);
            result["preflight"] = pj.describe()["result"]["result"].clone();
        }
        let _ = f2.store.finish(&id2, "done", Some(0), Some(&result), None, None);
        Ok(result)
    });
    if a["wait"].as_bool().unwrap_or(false) {
        wait(&job, Duration::from_secs(48 * 3600)).await;
    }
    Ok(with_run(job.describe(), &id))
}

// ---- forge.load / forge.generate -------------------------------------------------------------------------------

async fn load(hub: &Hub, forge: &Arc<Forge>, a: &Value) -> Result<Value> {
    let name = s(a, "model")?;
    let device = a.get("device").and_then(Value::as_str).unwrap_or("auto");
    if !["auto", "gpu", "cpu"].contains(&device) {
        return Err(Fail::invalid("device must be auto, gpu or cpu"));
    }
    let e = &hub.engine;
    let p = paths(hub);
    // a model the hub already knows is a GGUF (or Ollama's) model
    if let Ok(crate::engine::Target::Llama(entry)) = e.resolve(name) {
        let plan = runtime::plan(&entry.path, None);
        if a["plan_only"].as_bool().unwrap_or(false) {
            return Ok(json!({"model": entry.id, "plan": plan.to_json()}));
        }
        let opts = crate::engine::LoadOpts {
            gpu_layers: match device {
                "gpu" => Some(999),
                "cpu" => Some(0),
                _ => None,
            },
            ..Default::default()
        };
        let layers = opts.gpu_layers.unwrap_or(e.settings().gpu_layers);
        let inst = e.ensure_loaded(&entry, &opts, true).await?;
        let placement = json!({"engine": "llama.cpp", "format": "gguf", "device_requested": device,
                               "device": if layers == 0 { "cpu" } else { "gpu" }, "gpu_layers": layers,
                               "model": entry.id});
        forge.placements.lock().insert(entry.id.clone(), placement.clone());
        return Ok(
            json!({"model": entry.id, "status": "loaded", "plan": plan.to_json(), "placement": placement,
                         "loaded": inst.describe()}),
        );
    }
    let path = PathBuf::from(name);
    if !path.exists() {
        return Err(Fail::not_found(format!(
            "{name} is neither a model of this hub nor a path"
        )));
    }
    let known = runtime::llama_cpp_src(p.src.as_deref()).and_then(|d| runtime::convertible_architectures(&d));
    let plan = runtime::plan(&path, known.as_deref());
    let base = json!({"model": name, "plan": plan.to_json()});
    if a["plan_only"].as_bool().unwrap_or(false) {
        return Ok(base);
    }
    let merge = |mut v: Value, extra: Value| {
        if let (Some(o), Some(x)) = (v.as_object_mut(), extra.as_object()) {
            o.extend(x.clone());
        }
        v
    };
    match plan.engine {
        "llama.cpp" => Ok(merge(
            base,
            json!({"status": "needs_registration",
                   "next": "put the file's folder in model_dirs (config.set) so that model.list shows it, then forge.load its id"}),
        )),
        "llama.cpp (after conversion)" => Ok(merge(
            base,
            json!({"status": "needs_conversion",
                   "next": "convert with llama.cpp's convert_hf_to_gguf.py (the Forge's dusty_convert step shows how), then load the GGUF; or load it on the Forge worker by giving model_path directly to forge.run generate"}),
        )),
        "forge" => {
            if let Some(sv) = forge.servers.lock().await.get(name).filter(|s| s.alive()) {
                return Ok(merge(
                    base,
                    json!({"status": "loaded", "placement": forge.placements.lock().get(name), "run": sv.run}),
                ));
            }
            let src = require_installed(&p)?;
            let init = json!({"model_path": name, "device": device, "dtype": a.get("dtype").cloned().unwrap_or(json!("auto"))});
            let run = new_run_id();
            forge.store.create_run(&run, "generate_serve", &init)?;
            let cmd = worker_cmd(hub, &p, &src, "generate_serve", Some(init.to_string()));
            let sv = serve::Server::start(forge.store.clone(), run.clone(), cmd, Duration::from_secs(1800)).await?;
            let placement = json!({"engine": "forge", "format": plan.format.name(), "device_requested": device,
                                   "device": sv.info["device"], "dtype": sv.info["dtype"], "architecture": sv.info["architecture"],
                                   "run": run, "model": name});
            forge.placements.lock().insert(name.to_string(), placement.clone());
            forge.servers.lock().await.insert(name.to_string(), sv.clone());
            Ok(merge(
                base,
                json!({"status": "loaded", "placement": placement, "info": sv.info}),
            ))
        }
        "onnxruntime" => Ok(merge(
            base,
            json!({"status": "not_available", "next": "ONNX language models are not wired in yet; the plan is recorded"}),
        )),
        _ => Err(Fail::invalid(format!("{name}: {}", plan.reason))),
    }
}

async fn generate(forge: &Arc<Forge>, a: &Value) -> Result<Value> {
    let name = s(a, "model")?;
    let sv = forge
        .servers
        .lock()
        .await
        .get(name)
        .filter(|s| s.alive())
        .cloned()
        .ok_or_else(|| {
            Fail::new(
                409,
                "not_loaded",
                format!("{name} is not served by the Forge worker: forge.load it"),
            )
        })?;
    let mut req = a.clone();
    if let Some(o) = req.as_object_mut() {
        o.remove("model");
    }
    sv.request(req, Duration::from_secs(3600)).await
}

#[cfg(test)]
mod tests;

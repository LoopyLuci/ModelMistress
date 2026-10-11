//! Downloading models, so ModelMistress needs no other program to get them.
//!
//! Two sources, written in the same layouts their own tools use, under `<download_dir>` (default `<home>`), so the
//! catalog lists them with the usual ids:
//! - **Hugging Face**: `hf.co/<org>/<repo>[:<quant>]` (or `<org>/<repo>[:<quant>]`). Picks the GGUF for the quant
//!   (default: Q4_K_M, else the closest common one), every part of a split model, and the repo's vision projector.
//!   Stored as `<download_dir>/hub/models--<org>--<repo>/snapshots/<commit>/<file>`; the id is `<org>/<repo>:<stem>`.
//!   Gated repos need a token (`hf_token`, or HF_TOKEN).
//! - **The Ollama library**: `<name>[:<tag>]` or `<namespace>/<name>[:<tag>]` with `source: "ollama"`. Stored as an
//!   Ollama store under `<download_dir>/ollama` (manifests and blobs); the id is Ollama's own name.
//!
//! Every pull is a background job with progress; it resumes a half-finished file, checks each file's SHA-256
//! against what the source publishes, and refuses to start when the disk has too little free space.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::engine::{Engine, Fail, Result};

const HF: &str = "https://huggingface.co";
const OLLAMA_REGISTRY: &str = "https://registry.ollama.ai";
/// When no quant is asked for: the first of these the repo has.
const QUANT_PREFERENCE: &[&str] = &["Q4_K_M", "Q4_K_S", "Q5_K_M", "Q4_0", "Q6_K", "Q8_0", "IQ4_XS", "Q3_K_M"];
const ROOM_TO_SPARE: u64 = 512 << 20;

// ---- what to fetch ----------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PlanFile {
    pub url: String,
    pub dest: PathBuf,
    pub size: u64,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub source: &'static str,
    /// The id the catalog will list it under.
    pub id: String,
    pub files: Vec<PlanFile>,
    /// Small files written once every download has finished (Ollama's manifest).
    pub after: Vec<(PathBuf, Vec<u8>)>,
}

impl Plan {
    pub fn total(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// Which source a name means: an explicit `source`, else `hf.co/...` and `org/repo` are Hugging Face and a bare
/// `name[:tag]` is the Ollama library.
pub fn source_of(spec: &str, source: Option<&str>) -> Result<&'static str> {
    match source {
        Some("huggingface") | Some("hf") => Ok("huggingface"),
        Some("ollama") => Ok("ollama"),
        Some(other) => Err(Fail::invalid(format!("source {other:?}: use huggingface or ollama"))),
        None if hf_spec(spec).is_some() || spec.contains('/') => Ok("huggingface"),
        None => Ok("ollama"),
    }
}

fn hf_spec(spec: &str) -> Option<&str> {
    ["https://huggingface.co/", "huggingface.co/", "https://hf.co/", "hf.co/"]
        .iter()
        .find_map(|p| spec.strip_prefix(p))
}

/// `org/repo[:quant]` -> (repo, quant)
pub fn split_hf(spec: &str) -> Result<(String, Option<String>)> {
    let s = hf_spec(spec).unwrap_or(spec).trim().trim_end_matches('/');
    let (repo, quant) = match s.rsplit_once(':') {
        Some((r, q)) if !q.is_empty() => (r, Some(q.to_string())),
        _ => (s, None),
    };
    let parts: Vec<&str> = repo.split('/').collect();
    if parts.len() != 2
        || parts
            .iter()
            .any(|p| p.is_empty() || p.starts_with('.') || p.contains('\\'))
    {
        return Err(Fail::invalid(format!("{spec:?} is not a Hugging Face repo (org/name)")));
    }
    Ok((repo.to_string(), quant))
}

/// `[namespace/]name[:tag]` -> (namespace, name, tag)
pub fn split_ollama(spec: &str) -> Result<(String, String, String)> {
    let s = spec.trim();
    let s = s.strip_prefix("registry.ollama.ai/").unwrap_or(s);
    let (path, tag) = match s.rsplit_once(':') {
        Some((p, t)) if !t.is_empty() && !t.contains('/') => (p, t),
        _ => (s, "latest"),
    };
    let (ns, name) = match path.split_once('/') {
        Some((n, m)) => (n, m),
        None => ("library", path),
    };
    let ok = |x: &str| {
        !x.is_empty() && x.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) && !x.starts_with('.')
    };
    if !ok(ns) || !ok(name) || !ok(tag) {
        return Err(Fail::invalid(format!(
            "{spec:?} is not an Ollama model name (name[:tag])"
        )));
    }
    Ok((ns.to_string(), name.to_string(), tag.to_string()))
}

/// The quant label in a GGUF file name (`Qwen3-8B-Q4_K_M.gguf` -> Q4_K_M), upper-cased.
pub fn quant_of(file: &str) -> Option<String> {
    let stem = file.rsplit('/').next()?.strip_suffix(".gguf")?;
    let stem = match crate::catalog::split_part_of(stem) {
        Some(head) => head,
        None => stem,
    };
    let up = stem.to_uppercase();
    up.rsplit(['-', '.'])
        .next()
        .filter(|t| {
            t.starts_with('Q') || t.starts_with("IQ") || t.starts_with("BF16") || t.starts_with("F16") || t == &"F32"
        })
        .map(str::to_string)
}

/// Picks the model files (all parts) and the projector from a repo's file list (`(path, size, sha256)`).
pub fn choose_hf_files(
    files: &[(String, u64, Option<String>)],
    quant: Option<&str>,
    file: Option<&str>,
) -> Result<(Vec<usize>, Option<usize>)> {
    let is_gguf = |p: &str| p.to_lowercase().ends_with(".gguf");
    let is_proj = |p: &str| p.rsplit('/').next().unwrap_or(p).to_lowercase().starts_with("mmproj");
    let models: Vec<usize> = (0..files.len())
        .filter(|&i| is_gguf(&files[i].0) && !is_proj(&files[i].0))
        .collect();
    if models.is_empty() {
        return Err(Fail::not_found("this repo has no GGUF model files"));
    }
    // a model's "head": its file without a split suffix
    let head = |p: &str| {
        let stem = p.strip_suffix(".gguf").unwrap_or(p);
        crate::catalog::split_part_of(stem).unwrap_or(stem).to_string()
    };
    let wanted_head = if let Some(f) = file {
        let hit = models
            .iter()
            .find(|&&i| files[i].0 == f || files[i].0.rsplit('/').next() == Some(f));
        head(&files[*hit.ok_or_else(|| Fail::not_found(format!("no file {f:?} in this repo")))?].0)
    } else {
        let quants: Vec<(usize, String)> = models
            .iter()
            .filter_map(|&i| quant_of(&files[i].0).map(|q| (i, q)))
            .collect();
        let pick = |q: &str| {
            let q = q.to_uppercase();
            let mut hits: Vec<usize> = quants.iter().filter(|(_, x)| *x == q).map(|(i, _)| *i).collect();
            hits.sort_by_key(|&i| files[i].0.len()); // "Model-Q4_K_M" before "Model-UD-Q4_K_M"
            hits.first().copied()
        };
        let i = match quant {
            Some(q) => pick(q).ok_or_else(|| {
                let mut have: Vec<String> = quants.iter().map(|(_, q)| q.clone()).collect();
                have.sort();
                have.dedup();
                Fail::not_found(format!("no {q} file in this repo; it has: {}", have.join(", ")))
            })?,
            None => QUANT_PREFERENCE.iter().find_map(|q| pick(q)).unwrap_or(models[0]),
        };
        head(&files[i].0)
    };
    let mut chosen: Vec<usize> = models
        .into_iter()
        .filter(|&i| head(&files[i].0) == wanted_head)
        .collect();
    chosen.sort_by(|a, b| files[*a].0.cmp(&files[*b].0));
    let projs: Vec<usize> = (0..files.len())
        .filter(|&i| is_gguf(&files[i].0) && is_proj(&files[i].0))
        .collect();
    let proj = ["F16", "BF16", "Q8_0"]
        .iter()
        .find_map(|q| {
            projs
                .iter()
                .find(|&&i| quant_of(&files[i].0).as_deref() == Some(*q))
                .copied()
        })
        .or_else(|| projs.first().copied());
    Ok((chosen, proj))
}

pub async fn plan_hf(engine: &Engine, spec: &str, file: Option<&str>, with_projector: bool) -> Result<Plan> {
    let (repo, quant) = split_hf(spec)?;
    let v = hf_get(engine, &format!("{HF}/api/models/{repo}/revision/main?blobs=true")).await?;
    let sha = v["sha"]
        .as_str()
        .ok_or_else(|| Fail::new(502, "bad_answer", "Hugging Face gave no revision"))?;
    let files: Vec<(String, u64, Option<String>)> = v["siblings"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|s| {
            let path = s["rfilename"].as_str()?.to_string();
            let size = s["lfs"]["size"].as_u64().or_else(|| s["size"].as_u64()).unwrap_or(0);
            Some((path, size, s["lfs"]["sha256"].as_str().map(str::to_string)))
        })
        .collect();
    let (chosen, proj) = choose_hf_files(&files, quant.as_deref(), file)?;
    let snap = download_dir(engine)
        .join("hub")
        .join(format!("models--{}", repo.replace('/', "--")))
        .join("snapshots")
        .join(sha);
    let mut pick: Vec<usize> = chosen.clone();
    if with_projector {
        pick.extend(proj);
    }
    let out = pick
        .iter()
        .map(|&i| {
            let (path, size, sha256) = &files[i];
            PlanFile {
                url: format!("{HF}/{repo}/resolve/{sha}/{path}"),
                dest: snap.join(safe_rel(path)),
                size: *size,
                sha256: sha256.clone(),
            }
        })
        .collect();
    let first = &files[chosen[0]].0;
    let stem = first
        .rsplit('/')
        .next()
        .unwrap_or(first)
        .strip_suffix(".gguf")
        .unwrap_or(first);
    let stem = crate::catalog::split_part_of(stem).unwrap_or(stem);
    Ok(Plan {
        source: "huggingface",
        id: format!("{repo}:{stem}"),
        files: out,
        after: vec![],
    })
}

pub async fn plan_ollama(engine: &Engine, spec: &str) -> Result<Plan> {
    let (ns, name, tag) = split_ollama(spec)?;
    let url = format!("{OLLAMA_REGISTRY}/v2/{ns}/{name}/manifests/{tag}");
    let r = engine
        .http
        .get(&url)
        .header("accept", "application/vnd.docker.distribution.manifest.v2+json")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| Fail::unavailable(format!("the Ollama library did not answer: {e}")))?;
    if r.status().as_u16() == 404 {
        return Err(Fail::not_found(format!("the Ollama library has no {ns}/{name}:{tag}")));
    }
    if !r.status().is_success() {
        return Err(Fail::new(
            502,
            "bad_answer",
            format!("the Ollama library answered {}", r.status()),
        ));
    }
    let manifest = r.bytes().await.map_err(|e| Fail::unavailable(e.to_string()))?.to_vec();
    let doc: Value = serde_json::from_slice(&manifest).map_err(|e| Fail::new(502, "bad_answer", e.to_string()))?;
    let mut layers: Vec<Value> = doc["layers"].as_array().cloned().unwrap_or_default();
    if !layers
        .iter()
        .any(|l| l["mediaType"] == "application/vnd.ollama.image.model")
    {
        return Err(Fail::invalid(format!(
            "{name}:{tag} has no weights to download (a cloud model?)"
        )));
    }
    if doc["config"].is_object() {
        layers.push(doc["config"].clone());
    }
    let store = download_dir(engine).join("ollama");
    let mut files = vec![];
    for l in layers {
        let digest = l["digest"].as_str().unwrap_or("");
        let hex = digest
            .strip_prefix("sha256:")
            .filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()))
            .ok_or_else(|| Fail::new(502, "bad_answer", format!("unexpected layer digest {digest:?}")))?;
        files.push(PlanFile {
            url: format!("{OLLAMA_REGISTRY}/v2/{ns}/{name}/blobs/{digest}"),
            dest: store.join("blobs").join(format!("sha256-{hex}")),
            size: l["size"].as_u64().unwrap_or(0),
            sha256: Some(hex.to_lowercase()),
        });
    }
    files.dedup_by(|a, b| a.dest == b.dest);
    let manifest_path = store
        .join("manifests")
        .join("registry.ollama.ai")
        .join(&ns)
        .join(&name)
        .join(&tag);
    Ok(Plan {
        source: "ollama",
        id: if ns == "library" {
            format!("{name}:{tag}")
        } else {
            format!("{ns}/{name}:{tag}")
        },
        files,
        after: vec![(manifest_path, manifest)],
    })
}

/// Hugging Face's API: GGUF repos matching a query, most downloaded first.
pub async fn search_hf(engine: &Engine, query: &str, limit: u32) -> Result<Value> {
    let mut url = reqwest::Url::parse(&format!("{HF}/api/models")).map_err(|e| Fail::invalid(e.to_string()))?;
    url.query_pairs_mut()
        .append_pair("search", query)
        .append_pair("filter", "gguf")
        .append_pair("sort", "downloads")
        .append_pair("direction", "-1")
        .append_pair("limit", &limit.clamp(1, 100).to_string());
    let v = hf_get(engine, url.as_str()).await?;
    Ok(json!(v
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(
            |m| json!({"repo": m["id"], "pull": format!("hf.co/{}", m["id"].as_str().unwrap_or("")),
                        "downloads": m["downloads"], "likes": m["likes"], "updated": m["lastModified"]})
        )
        .collect::<Vec<_>>()))
}

/// A Hugging Face repo's GGUF files: name, size, quant, and the name to pull it by.
pub async fn files_hf(engine: &Engine, spec: &str) -> Result<Value> {
    let (repo, _) = split_hf(spec)?;
    let v = hf_get(engine, &format!("{HF}/api/models/{repo}/revision/main?blobs=true")).await?;
    let out: Vec<Value> = v["siblings"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|s| {
            let f = s["rfilename"].as_str()?;
            f.to_lowercase().ends_with(".gguf").then(|| {
                let q = quant_of(f);
                let proj = f.rsplit('/').next().unwrap_or(f).to_lowercase().starts_with("mmproj");
                json!({"file": f, "size_gb": (s["lfs"]["size"].as_f64().or_else(|| s["size"].as_f64()).unwrap_or(0.0) / 1e7).round() / 100.0,
                       "quant": q, "projector": proj,
                       "pull": (!proj).then(|| format!("hf.co/{repo}:{}", q.clone().unwrap_or_default()))})
            })
        })
        .collect();
    Ok(json!(out))
}

async fn hf_get(engine: &Engine, url: &str) -> Result<Value> {
    let mut req = engine.http.get(url).timeout(Duration::from_secs(30));
    if let Some(t) = hf_token(engine) {
        req = req.bearer_auth(t);
    }
    let r = req
        .send()
        .await
        .map_err(|e| Fail::unavailable(format!("Hugging Face did not answer: {e}")))?;
    match r.status().as_u16() {
        200 => r.json().await.map_err(|e| Fail::new(502, "bad_answer", e.to_string())),
        401 | 403 => Err(Fail::new(
            403,
            "forbidden",
            "Hugging Face refused: the repo is gated or private (set hf_token, or HF_TOKEN)",
        )),
        404 => Err(Fail::not_found("Hugging Face has no such repo")),
        s => Err(Fail::new(502, "bad_answer", format!("Hugging Face answered {s}"))),
    }
}

fn hf_token(engine: &Engine) -> Option<String> {
    engine
        .settings()
        .hf_token
        .filter(|t| !t.is_empty())
        .or_else(|| std::env::var("HF_TOKEN").ok().filter(|t| !t.is_empty()))
}

pub fn download_dir(engine: &Engine) -> PathBuf {
    engine.settings().download_dir.unwrap_or_else(|| engine.home.clone())
}

/// A repo path made safe to join under a folder: no absolute parts, no `..`.
fn safe_rel(p: &str) -> PathBuf {
    p.split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != "." && *s != ".." && !s.contains(':'))
        .collect()
}

// ---- jobs -------------------------------------------------------------------------------------------------

pub struct Job {
    pub id: String,
    pub model: String,
    pub source: &'static str,
    pub started: u64,
    began: Instant,
    pub done_bytes: AtomicU64,
    pub total_bytes: AtomicU64,
    cancel: AtomicBool,
    state: Mutex<(String, String, Option<Value>)>, // (status, message, result)
}

impl Job {
    pub fn describe(&self) -> Value {
        let (status, message, result) = self.state.lock().clone();
        let done = self.done_bytes.load(Ordering::Relaxed);
        let total = self.total_bytes.load(Ordering::Relaxed);
        let secs = self.began.elapsed().as_secs_f64().max(0.001);
        json!({"job": self.id, "model": self.model, "source": self.source, "status": status, "message": message,
               "result": result, "done_bytes": done, "total_bytes": total,
               "percent": if total > 0 { ((done as f64 / total as f64) * 1000.0).round() / 10.0 } else { 0.0 },
               "mb_per_s": ((done as f64 / 1e6 / secs) * 10.0).round() / 10.0, "started": self.started})
    }
    pub fn status(&self) -> String {
        self.state.lock().0.clone()
    }
    /// Has `job.cancel` been asked for?
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    /// For a task: how far it is (`done` of `total` steps; job.get shows them as done_bytes and total_bytes) and a note.
    pub fn progress(&self, done: u64, total: u64, note: impl Into<String>) {
        self.done_bytes.store(done, Ordering::Relaxed);
        self.total_bytes.store(total, Ordering::Relaxed);
        let mut st = self.state.lock();
        if st.0 == "running" {
            st.1 = note.into();
        }
    }
    fn set(&self, status: &str, message: impl Into<String>, result: Option<Value>) {
        *self.state.lock() = (status.to_string(), message.into(), result);
    }
}

#[derive(Default)]
pub struct Jobs {
    map: Mutex<BTreeMap<String, Arc<Job>>>,
    next: AtomicU64,
}

impl Jobs {
    pub fn list(&self) -> Vec<Value> {
        self.map.lock().values().map(|j| j.describe()).collect()
    }
    pub fn get(&self, id: &str) -> Option<Arc<Job>> {
        self.map.lock().get(id).cloned()
    }
    pub fn cancel(&self, id: &str) -> bool {
        match self.get(id) {
            Some(j) if j.status() == "running" => {
                j.cancel.store(true, Ordering::Relaxed);
                true
            }
            _ => false,
        }
    }

    /// Registers a job; a running one with the same label is returned instead (as the error).
    fn register(
        self: &Arc<Self>,
        kind: &str,
        label: String,
        source: &'static str,
        total: u64,
    ) -> std::result::Result<Arc<Job>, Arc<Job>> {
        let mut map = self.map.lock();
        if let Some(j) = map.values().find(|j| j.model == label && j.status() == "running") {
            return Err(j.clone());
        }
        let n = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let job = Arc::new(Job {
            id: format!("{kind}-{n}"),
            model: label,
            source,
            started: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            began: Instant::now(),
            done_bytes: AtomicU64::new(0),
            total_bytes: AtomicU64::new(total),
            cancel: AtomicBool::new(false),
            state: Mutex::new(("running".into(), "starting".into(), None)),
        });
        map.insert(job.id.clone(), job.clone());
        Ok(job)
    }

    /// Starts a task that is not a download (`quantize`): `work` gets the job to report progress on and to check
    /// for cancelling, and its result becomes the job's. A running task with the same label is returned instead.
    pub fn start_task<F, Fut>(self: &Arc<Self>, kind: &str, label: String, work: F) -> Arc<Job>
    where
        F: FnOnce(Arc<Job>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Value>> + Send + 'static,
    {
        let job = match self.register(kind, label, "task", 0) {
            Ok(j) => j,
            Err(running) => return running,
        };
        let j = job.clone();
        tokio::spawn(async move {
            match work(j.clone()).await {
                Ok(v) => j.set("done", "finished", Some(v)),
                Err(f) if f.code == "cancelled" => j.set("cancelled", f.message, None),
                Err(f) => j.set("failed", f.message, None),
            }
        });
        job
    }

    /// Starts a pull. A pull of the same model that is still running is returned instead of a second one.
    pub fn start(self: &Arc<Self>, engine: Arc<Engine>, plan: Plan) -> Arc<Job> {
        let job = match self.register("pull", plan.id.clone(), plan.source, plan.total()) {
            Ok(j) => j,
            Err(running) => return running,
        };
        let j = job.clone();
        tokio::spawn(async move {
            match run(&engine, &plan, &j).await {
                Ok(()) => {
                    let e = engine.clone();
                    let _ = tokio::task::spawn_blocking(move || e.refresh()).await;
                    let listed = engine.catalog().iter().any(|m| m.id == plan.id);
                    j.set(
                        "done",
                        if listed {
                            "downloaded"
                        } else {
                            "downloaded, but the catalog does not list it"
                        },
                        Some(
                            json!({"model": plan.id, "files": plan.files.iter().map(|f| &f.dest).collect::<Vec<_>>()}),
                        ),
                    );
                }
                Err(f) if f.code == "cancelled" => j.set("cancelled", f.message, None),
                Err(f) => j.set("failed", f.message, None),
            }
        });
        job
    }
}

async fn run(engine: &Engine, plan: &Plan, job: &Job) -> Result<()> {
    let need: u64 = plan
        .files
        .iter()
        .filter(|f| !complete(f))
        .map(|f| {
            f.size
                .saturating_sub(std::fs::metadata(part_of(&f.dest)).map(|m| m.len()).unwrap_or(0))
        })
        .sum();
    if let Some(dest) = plan.files.first().map(|f| f.dest.clone()) {
        if let Some(free) = free_space(&dest) {
            if free < need + ROOM_TO_SPARE {
                return Err(Fail::new(
                    507,
                    "no_space",
                    format!(
                        "{:.1} GB is needed and {:.1} GB is free where it would go; set download_dir to a bigger disk",
                        need as f64 / 1e9,
                        free as f64 / 1e9
                    ),
                ));
            }
        }
    }
    for (i, f) in plan.files.iter().enumerate() {
        let name = f
            .dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        job.set(
            "running",
            format!("file {} of {}: {name}", i + 1, plan.files.len()),
            None,
        );
        if complete(f) {
            job.done_bytes.fetch_add(f.size, Ordering::Relaxed);
            continue;
        }
        fetch(engine, f, job).await?;
    }
    for (path, bytes) in &plan.after {
        write_atomic(path, bytes).map_err(|e| Fail::new(500, "io", format!("{}: {e}", path.display())))?;
    }
    Ok(())
}

/// Already downloaded (the right size; it was checked when it was written).
fn complete(f: &PlanFile) -> bool {
    std::fs::metadata(&f.dest).is_ok_and(|m| m.is_file() && (f.size == 0 || m.len() == f.size))
}

fn part_of(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

async fn fetch(engine: &Engine, f: &PlanFile, job: &Job) -> Result<()> {
    let io = |e: std::io::Error| Fail::new(500, "io", format!("{}: {e}", f.dest.display()));
    let dir = f.dest.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(io)?;
    let part = part_of(&f.dest);
    let mut hasher = Sha256::new();
    let mut have = 0u64;
    if let Ok(mut old) = std::fs::File::open(&part) {
        // resume: the bytes already here go into the hash first
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = old.read(&mut buf).map_err(io)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            have += n as u64;
        }
        if f.size > 0 && have > f.size {
            have = 0;
            hasher = Sha256::new();
            let _ = std::fs::remove_file(&part);
        }
    }
    job.done_bytes.fetch_add(have, Ordering::Relaxed);
    let mut req = engine.http.get(&f.url);
    if f.url.starts_with(HF) {
        if let Some(t) = hf_token(engine) {
            req = req.bearer_auth(t);
        }
    }
    if have > 0 {
        req = req.header("range", format!("bytes={have}-"));
    }
    let r = req
        .send()
        .await
        .map_err(|e| Fail::unavailable(format!("download failed: {e}")))?;
    let status = r.status().as_u16();
    if status == 200 && have > 0 {
        // the server ignored the range: start the file again
        job.done_bytes.fetch_sub(have, Ordering::Relaxed);
        have = 0;
        hasher = Sha256::new();
    } else if status == 416 && f.size > 0 && have == f.size {
        // the whole file was already here
    } else if !(status == 200 || status == 206) {
        return Err(Fail::new(
            502,
            "download_failed",
            format!("{} answered {status}", host(&f.url)),
        ));
    }
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(have > 0)
        .truncate(have == 0)
        .open(&part)
        .map_err(io)?;
    if status != 416 {
        let mut stream = r.bytes_stream();
        let mut idle = Instant::now();
        loop {
            if job.cancel.load(Ordering::Relaxed) {
                return Err(Fail::new(
                    499,
                    "cancelled",
                    "cancelled; the part downloaded so far is kept for a resume",
                ));
            }
            let next = tokio::time::timeout(Duration::from_secs(60), stream.next()).await;
            let chunk = match next {
                Ok(Some(c)) => c.map_err(|e| Fail::unavailable(format!("download interrupted: {e}")))?,
                Ok(None) => break,
                Err(_) => {
                    return Err(Fail::unavailable(format!(
                        "no data for {} s from {}",
                        idle.elapsed().as_secs(),
                        host(&f.url)
                    )))
                }
            };
            idle = Instant::now();
            out.write_all(&chunk).map_err(io)?;
            hasher.update(&chunk);
            have += chunk.len() as u64;
            job.done_bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        }
    }
    out.flush().map_err(io)?;
    drop(out);
    if f.size > 0 && have != f.size {
        return Err(Fail::unavailable(format!(
            "{}: got {have} of {} bytes; pull again to resume",
            f.dest.display(),
            f.size
        )));
    }
    if let Some(want) = &f.sha256 {
        let got = format!("{:x}", hasher.finalize());
        if !got.eq_ignore_ascii_case(want) {
            let _ = std::fs::remove_file(&part);
            return Err(Fail::new(
                502,
                "checksum",
                format!(
                    "{}: the SHA-256 does not match what the source publishes; the file was discarded",
                    f.dest.display()
                ),
            ));
        }
    }
    std::fs::rename(&part, &f.dest).map_err(io)
}

fn host(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    let tmp = part_of(path);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Free bytes on the disk that holds `path` (or the nearest folder above it that exists).
pub(crate) fn free_space(path: &Path) -> Option<u64> {
    let mut p = path.to_path_buf();
    while !p.exists() {
        p = p.parent()?.to_path_buf();
    }
    let p = p.canonicalize().ok()?;
    let p = crate::catalog::plain_path(p);
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .iter()
        .filter(|d| p.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space())
}

// ---- deleting what was downloaded ---------------------------------------------------------------------------

/// Deletes a model ModelMistress downloaded (only files under `download_dir`; anything else is refused). For an
/// Ollama model, its manifest goes, and each blob of it that no other manifest in the store uses.
pub fn delete(engine: &Engine, id: &str) -> Result<Value> {
    let entry = engine
        .catalog()
        .into_iter()
        .find(|e| e.id == id)
        .ok_or_else(|| Fail::not_found(format!("no model {id:?}")))?;
    let root = download_dir(engine);
    let root = crate::catalog::plain_path(root.canonicalize().unwrap_or(root));
    let inside = |p: &Path| {
        p.canonicalize()
            .map(crate::catalog::plain_path)
            .is_ok_and(|c| c.starts_with(&root))
    };
    if !inside(&entry.path) {
        return Err(Fail::new(
            403,
            "not_ours",
            format!(
                "{id} was not downloaded by ModelMistress ({}); delete it with the program that owns it",
                entry.path.display()
            ),
        ));
    }
    engine.unload(id);
    let io = |p: &Path, e: std::io::Error| Fail::new(500, "io", format!("{}: {e}", p.display()));
    let mut removed: Vec<PathBuf> = vec![];
    match entry.source {
        "ollama" => {
            let store = root.join("ollama");
            let manifests = store.join("manifests");
            let mut all = vec![];
            walk_files(&manifests, 5, &mut all);
            let digests = |p: &Path| -> Vec<String> {
                let v: Value = std::fs::read(p)
                    .ok()
                    .and_then(|b| serde_json::from_slice(&b).ok())
                    .unwrap_or(Value::Null);
                let mut out: Vec<String> = v["layers"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|l| l["digest"].as_str().map(|d| d.replace(':', "-")))
                    .collect();
                out.extend(v["config"]["digest"].as_str().map(|d| d.replace(':', "-")));
                out
            };
            let model_blob = entry
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let (mine, others): (Vec<PathBuf>, Vec<PathBuf>) =
                all.into_iter().partition(|m| digests(m).contains(&model_blob));
            let keep: Vec<String> = others.iter().flat_map(|m| digests(m)).collect();
            for m in &mine {
                for d in digests(m) {
                    let blob = store.join("blobs").join(&d);
                    if !keep.contains(&d) && blob.is_file() {
                        std::fs::remove_file(&blob).map_err(|e| io(&blob, e))?;
                        removed.push(blob);
                    }
                }
                std::fs::remove_file(m).map_err(|e| io(m, e))?;
                removed.push(m.clone());
            }
        }
        _ => {
            let dir = entry.path.parent().map(Path::to_path_buf).unwrap_or_default();
            let stem = entry
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let head = crate::catalog::split_part_of(&stem).unwrap_or(&stem).to_string();
            let mut files = vec![];
            walk_files(&dir, 1, &mut files);
            let gguf = |p: &Path| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("gguf"));
            for f in &files {
                let s = f
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if gguf(f) && (s == stem || crate::catalog::split_part_of(&s) == Some(head.as_str())) && inside(f) {
                    std::fs::remove_file(f).map_err(|e| io(f, e))?;
                    removed.push(f.clone());
                }
            }
            // the projector goes too when no other model in the folder is left to use it
            let mut left = vec![];
            walk_files(&dir, 1, &mut left);
            let models_left = left.iter().any(|f| {
                gguf(f)
                    && !f
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().to_lowercase().starts_with("mmproj"))
            });
            if !models_left {
                for f in left.iter().filter(|f| gguf(f) && inside(f)) {
                    std::fs::remove_file(f).map_err(|e| io(f, e))?;
                    removed.push(f.clone());
                }
            }
        }
    }
    engine.refresh();
    Ok(json!({"model": id, "removed": removed}))
}

fn walk_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() && depth > 1 => walk_files(&p, depth - 1, out),
            Ok(t) if t.is_file() => out.push(p),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(source_of("qwen3:8b", None).unwrap(), "ollama");
        assert_eq!(
            source_of("hf.co/unsloth/Qwen3-8B-GGUF:Q4_K_M", None).unwrap(),
            "huggingface"
        );
        assert_eq!(source_of("unsloth/Qwen3-8B-GGUF", None).unwrap(), "huggingface");
        assert_eq!(source_of("someone/model:tag", Some("ollama")).unwrap(), "ollama");
        assert_eq!(
            split_hf("hf.co/unsloth/Qwen3-8B-GGUF:q4_k_m").unwrap(),
            ("unsloth/Qwen3-8B-GGUF".to_string(), Some("q4_k_m".to_string()))
        );
        assert!(split_hf("hf.co/../etc").is_err());
        assert!(split_hf("one/two/three").is_err());
        assert_eq!(
            split_ollama("qwen3").unwrap(),
            ("library".into(), "qwen3".into(), "latest".into())
        );
        assert_eq!(
            split_ollama("someone/tool:1b").unwrap(),
            ("someone".into(), "tool".into(), "1b".into())
        );
        assert!(split_ollama("../x").is_err());
        assert_eq!(safe_rel("a/../b/./c.gguf"), PathBuf::from("a").join("b").join("c.gguf"));
        assert_eq!(quant_of("sub/Qwen3-8B-Q4_K_M.gguf").as_deref(), Some("Q4_K_M"));
        assert_eq!(quant_of("Big-Q8_0-00001-of-00002.gguf").as_deref(), Some("Q8_0"));
        assert_eq!(quant_of("model.gguf"), None);
    }

    #[test]
    fn choosing_files() {
        let f = |p: &str| (p.to_string(), 1u64, None);
        let files = vec![
            f("README.md"),
            f("M-Q8_0.gguf"),
            f("M-UD-Q4_K_M.gguf"),
            f("M-Q4_K_M.gguf"),
            f("Q6/M-Q6_K-00001-of-00002.gguf"),
            f("Q6/M-Q6_K-00002-of-00002.gguf"),
            f("mmproj-BF16.gguf"),
            f("mmproj-F16.gguf"),
        ];
        let names = |(m, p): (Vec<usize>, Option<usize>)| {
            (
                m.iter().map(|&i| files[i].0.clone()).collect::<Vec<_>>(),
                p.map(|i| files[i].0.clone()),
            )
        };
        assert_eq!(
            names(choose_hf_files(&files, None, None).unwrap()),
            (vec!["M-Q4_K_M.gguf".to_string()], Some("mmproj-F16.gguf".to_string()))
        );
        assert_eq!(
            names(choose_hf_files(&files, Some("q6_k"), None).unwrap()).0,
            ["Q6/M-Q6_K-00001-of-00002.gguf", "Q6/M-Q6_K-00002-of-00002.gguf"]
        );
        assert_eq!(
            names(choose_hf_files(&files, None, Some("M-Q8_0.gguf")).unwrap()).0,
            ["M-Q8_0.gguf"]
        );
        let err = choose_hf_files(&files, Some("Q2_K"), None).unwrap_err();
        assert!(
            err.message.contains("Q4_K_M") && err.message.contains("Q6_K"),
            "{}",
            err.message
        );
    }

    #[test]
    fn deleting_refuses_models_it_did_not_download() {
        let home = crate::testing::TestDir::lazy("mm-pull-del");
        let elsewhere = crate::testing::TestDir::new("mm-pull-other");
        std::fs::create_dir_all(&elsewhere).unwrap();
        crate::catalog::tests_support::gguf(&elsewhere.join("theirs.gguf"), "llama", "T");
        let snap = home.join("hub/models--org--M-GGUF/snapshots/abc");
        std::fs::create_dir_all(&snap).unwrap();
        crate::catalog::tests_support::gguf(&snap.join("M-Q4_K_M.gguf"), "llama", "M");
        crate::catalog::tests_support::gguf(&snap.join("mmproj-F16.gguf"), "clip", "P");
        let e = Engine::new(
            home.clone(),
            crate::engine::Settings {
                ollama_url: String::new(),
                model_dirs: vec![elsewhere.clone()],
                hf_dirs: vec![],
                ..Default::default()
            },
        );
        let err = delete(&e, "theirs").unwrap_err();
        assert_eq!(err.code, "not_ours");
        assert!(elsewhere.join("theirs.gguf").is_file());
        let done = delete(&e, "org/M-GGUF:M-Q4_K_M").unwrap();
        assert_eq!(done["removed"].as_array().unwrap().len(), 2); // the model and its projector
        assert!(!snap.join("M-Q4_K_M.gguf").exists());
        assert!(e.catalog().iter().all(|m| m.id != "org/M-GGUF:M-Q4_K_M"));
    }

    #[tokio::test]
    async fn tasks_report_progress_and_finish() {
        let jobs = Arc::new(Jobs::default());
        let (go, wait) = tokio::sync::oneshot::channel::<()>();
        let job = jobs.start_task("quantize", "out.gguf".into(), |j| async move {
            j.progress(3, 10, "tensor 3 of 10");
            let _ = wait.await;
            Ok(json!({"output": "out.gguf"}))
        });
        assert!(job.id.starts_with("quantize-"));
        while job.describe()["done_bytes"] != 3 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let d = job.describe();
        assert_eq!(
            (d["status"].as_str(), d["percent"].as_f64(), d["message"].as_str()),
            (Some("running"), Some(30.0), Some("tensor 3 of 10"))
        );
        // the same label while it runs is the same job
        let again = jobs.start_task("quantize", "out.gguf".into(), |_| async { Ok(json!(null)) });
        assert_eq!(again.id, job.id);
        go.send(()).unwrap();
        while job.status() == "running" {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let d = job.describe();
        assert_eq!(
            (d["status"].as_str(), d["result"]["output"].as_str()),
            (Some("done"), Some("out.gguf"))
        );
        assert_eq!(jobs.list().len(), 1);
    }

    #[tokio::test]
    async fn tasks_can_be_cancelled_and_can_fail() {
        let jobs = Arc::new(Jobs::default());
        let job = jobs.start_task("quantize", "a".into(), |j| async move {
            while !j.cancelled() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            Err(Fail::new(409, "cancelled", "stopped"))
        });
        assert!(jobs.cancel(&job.id));
        while job.status() == "running" {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(job.status(), "cancelled");
        assert!(!jobs.cancel(&job.id), "a finished job cannot be cancelled");
        let bad = jobs.start_task("quantize", "a".into(), |_| async {
            Err(Fail::new(500, "tool_failed", "boom"))
        });
        assert_ne!(bad.id, job.id, "the label is free once the job is over");
        while bad.status() == "running" {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let d = bad.describe();
        assert_eq!(
            (d["status"].as_str(), d["message"].as_str()),
            (Some("failed"), Some("boom"))
        );
    }
}

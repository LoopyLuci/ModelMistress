//! Observability ("the model's thoughts"): a trace of what the models do, with the per-token view.
//!
//! * [`store`]: SQLite + compact per-token segment files, retention, JSONL export.
//! * [`Tracer`]: the API other modules call (the chat path here; the KM runtime and the training Forge later):
//!   start a trace, mark stages, record events (a kind and JSON), batches of tokens, specialist calls, finish.
//! * [`capture`]: records the hub's chat path (logprobs, reasoning, timings, resources) without changing the answer.
//! * [`api`]: `/v1/native/...`, the live stream and the `trace.*` operations. [`ui`]: the dashboard at `/ui`.
//!
//! See docs/native/observability.md.

pub mod api;
pub mod capture;
pub mod diff;
pub mod store;
pub mod ui;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::broadcast;

pub use store::{Alt, RouteLayer, TokenRec};

/// What the owner can set (`trace.settings`, kept in `<home>/trace/settings.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TraceSettings {
    /// Trace every chat request. A request can ask for it alone with `"mm_trace": true` (or the `x-mm-trace: 1`
    /// header), and `"mm_trace": false` opts one out.
    pub enabled: bool,
    /// Alternatives recorded per token (llama-server's `top_logprobs`), 1 to 20.
    pub top_k: u32,
    /// Record which experts a mixture-of-experts model picks for each token of a traced request (a request can ask
    /// for it alone with `"mm_routing": true`). It costs a copy off the device per layer and token.
    pub routing: bool,
    /// Delete traces older than this many days (0 = keep).
    pub max_age_days: u32,
    /// Delete the oldest traces when the stored size passes this many MB (0 = no limit).
    pub max_total_mb: u64,
}

impl Default for TraceSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            top_k: 8,
            routing: false,
            max_age_days: 30,
            max_total_mb: 2048,
        }
    }
}

impl TraceSettings {
    fn clamped(mut self) -> Self {
        self.top_k = self.top_k.clamp(1, 20);
        self
    }
}

/// What a trace belongs to besides itself: a job (one pipeline step) and the task it is part of (for the KM factory,
/// `factory.task.*` ids), so everything a task's model calls did can be browsed from the task.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scope {
    pub job: Option<String>,
    pub task: Option<String>,
}

impl Scope {
    pub fn new(job: Option<&str>, task: Option<&str>) -> Self {
        Self {
            job: job.map(str::to_string),
            task: task.map(str::to_string),
        }
    }
}

/// A stage of a trace that is running.
#[derive(Debug, Clone)]
pub struct Stage {
    pub id: i64,
    pub trace: String,
    pub name: String,
}

pub struct Tracer {
    home: PathBuf,
    store: OnceLock<Result<store::Store, String>>,
    settings: RwLock<TraceSettings>,
    on: AtomicBool,
    tx: broadcast::Sender<Value>,
}

static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Arc<Tracer>>>> = OnceLock::new();

/// The tracer of a hub home folder (one per folder in the process). Nothing is created on disk until it is used.
pub fn tracer(home: &Path) -> Arc<Tracer> {
    let reg = REGISTRY.get_or_init(Default::default);
    let mut m = reg.lock();
    if let Some(t) = m.get(home) {
        return t.clone();
    }
    let t = Arc::new(Tracer::new(home));
    m.insert(home.to_path_buf(), t.clone());
    t
}

impl Tracer {
    fn new(home: &Path) -> Self {
        let settings = std::fs::read(home.join("trace").join("settings.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<TraceSettings>(&b).ok())
            .unwrap_or_default()
            .clamped();
        let (tx, _) = broadcast::channel(2048);
        Self {
            home: home.to_path_buf(),
            store: OnceLock::new(),
            on: AtomicBool::new(settings.enabled),
            settings: RwLock::new(settings),
            tx,
        }
    }

    /// Is tracing on for every request? (One atomic load.)
    pub fn enabled(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    pub fn settings(&self) -> TraceSettings {
        self.settings.read().clone()
    }

    pub fn set_settings(&self, new: TraceSettings) -> Result<TraceSettings, String> {
        let new = new.clamped();
        let dir = self.home.join("trace");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let tmp = dir.join("settings.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&new).unwrap_or_default()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, dir.join("settings.json")).map_err(|e| e.to_string())?;
        *self.settings.write() = new.clone();
        self.on.store(new.enabled, Ordering::Relaxed);
        if let Ok(s) = self.store() {
            let _ = s.retain(&new);
        }
        Ok(new)
    }

    pub fn store(&self) -> Result<&store::Store, String> {
        self.store
            .get_or_init(|| {
                let s = store::Store::open(&self.home.join("trace"))?;
                s.sweep_orphans();
                let _ = s.retain(&self.settings.read());
                Ok(s)
            })
            .as_ref()
            .map_err(|e| format!("the trace store could not be opened: {e}"))
    }

    // ---- live ---------------------------------------------------------------------------------------------

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.tx.subscribe()
    }

    fn listening(&self) -> bool {
        self.tx.receiver_count() > 0
    }

    fn publish(&self, v: Value) {
        let _ = self.tx.send(v);
    }

    fn warn<T>(&self, what: &str, r: Result<T, String>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("trace {what}: {e}");
                None
            }
        }
    }

    // ---- the API other modules call -----------------------------------------------------------------------

    /// Start a trace of `kind` ("chat", and later "km", "train", ...). Returns its id. `request` is what is
    /// stored as the trace's request (what a rerun sends again); `parent` links a rerun to its original.
    pub fn start(
        &self,
        kind: &str,
        model: &str,
        request: &Value,
        parent: Option<&str>,
        meta: &Value,
    ) -> Option<String> {
        self.start_in(kind, model, request, parent, meta, &Scope::default())
    }

    /// [`start`](Self::start) for a trace that belongs to a job and/or task (a KM factory task's model calls).
    pub fn start_in(
        &self,
        kind: &str,
        model: &str,
        request: &Value,
        parent: Option<&str>,
        meta: &Value,
        scope: &Scope,
    ) -> Option<String> {
        let id = format!("tr_{}", &uuid::Uuid::new_v4().simple().to_string()[..16]);
        let at = self.warn(
            "start",
            self.store()
                .and_then(|s| s.insert(&id, kind, model, request, parent, meta, scope)),
        )?;
        self.publish(
            json!({"type": "trace.start", "trace": id, "kind": kind, "model": model, "ts": at,
                            "parent": parent, "job": scope.job, "task": scope.task, "meta": meta}),
        );
        Some(id)
    }

    pub fn stage_start(&self, trace: &str, name: &str) -> Option<Stage> {
        let at = store::now_ms();
        let id = self.warn("stage", self.store().and_then(|s| s.stage_start(trace, name, at)))?;
        self.publish(json!({"type": "stage", "trace": trace, "name": name, "state": "start", "ts": at}));
        Some(Stage {
            id,
            trace: trace.into(),
            name: name.into(),
        })
    }

    pub fn stage_end(&self, stage: &Stage, meta: Value) {
        let at = store::now_ms();
        self.warn("stage", self.store().and_then(|s| s.stage_end(stage.id, at, &meta)));
        self.publish(
            json!({"type": "stage", "trace": stage.trace, "name": stage.name, "state": "end", "ts": at,
                            "meta": meta}),
        );
    }

    /// A stage whose start and end are known after the fact (unix milliseconds).
    pub fn stage_span(&self, trace: &str, name: &str, start_ms: i64, end_ms: i64, meta: Value) {
        let r = self.store().and_then(|s| {
            let id = s.stage_start(trace, name, start_ms)?;
            s.stage_end(id, end_ms, &meta)
        });
        self.warn("stage", r);
        self.publish(
            json!({"type": "stage", "trace": trace, "name": name, "state": "span", "start": start_ms,
                            "end": end_ms, "meta": meta}),
        );
    }

    /// Record an event: `kind` names what happened (for instance `selection`, `residency`, `routing`, `km.load`),
    /// `data` is anything JSON.
    pub fn event(&self, trace: &str, kind: &str, data: Value) {
        let at = store::now_ms();
        self.warn(
            "event",
            self.store().and_then(|s| s.add_event(trace, at, kind, None, &data)),
        );
        self.publish(json!({"type": "event", "trace": trace, "kind": kind, "ts": at, "data": data}));
    }

    pub fn specialist_call(&self, trace: &str, specialist: &str, input: Value, output: Value, ms: u64, ok: bool) {
        let r = self
            .store()
            .and_then(|s| s.add_call(trace, specialist, &input, &output, ms, ok));
        self.warn("specialist call", r);
        self.publish(
            json!({"type": "specialist", "trace": trace, "specialist": specialist, "ms": ms, "ok": ok,
                            "input": input, "output": output}),
        );
    }

    /// Append a batch of tokens (they are written to the trace's segment file at once).
    pub fn tokens(&self, trace: &str, batch: &[TokenRec]) {
        if batch.is_empty() {
            return;
        }
        let Some(total) = self.warn("tokens", self.store().and_then(|s| s.append_tokens(trace, batch))) else {
            return;
        };
        if self.listening() {
            let first = total as usize - batch.len();
            let toks: Vec<Value> = batch.iter().enumerate().map(|(i, t)| t.to_json(first + i)).collect();
            self.publish(json!({"type": "tokens", "trace": trace, "offset": first, "tokens": toks}));
        }
    }

    /// Set fields of the trace: prompt, rendered_prompt, answer, reasoning, params, timings, resources, adapters,
    /// meta, model.
    pub fn update(&self, trace: &str, patch: Value) {
        self.warn("update", self.store().and_then(|s| s.patch(trace, &patch)));
        let mut shown = patch;
        if let Some(o) = shown.as_object_mut() {
            o.remove("request");
        }
        self.publish(json!({"type": "update", "trace": trace, "patch": shown}));
    }

    /// End the trace: `status` is "ok", "error" or "aborted" (or anything a module defines). Applies retention.
    pub fn finish(&self, trace: &str, status: &str, error: Option<&str>) {
        self.warn("finish", self.store().and_then(|s| s.finish(trace, status, error)));
        self.publish(
            json!({"type": "trace.finish", "trace": trace, "status": status, "error": error,
                            "ts": store::now_ms()}),
        );
        let st = self.settings();
        if let Ok(s) = self.store() {
            self.warn("retention", s.retain(&st));
        }
    }
}

#[cfg(test)]
mod tests;

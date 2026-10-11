//! The Knowledge Module factory (`docs/native/km-factory.md` sections 2 and 5): tasks that turn sources into `data`,
//! `memory`, `skill` and `tool` KMs (and, through the Forge, `adapter` KMs) with good records and correct tags.
//!
//! A task is a pipeline of eight resumable steps run as one hub job: [`plan`], [`collect`] (gather), [`generate`],
//! [`verify`], [`tag`], [`build`], [`evaluate`] and [`publish`]. Each step keeps its state in `<home>/factory/factory.db`
//! ([`store`]), so a crash, a restart, a cancel or an exhausted budget leaves a task that `factory.task.resume`
//! continues. Every model call goes through [`run::Run::ask`]: budgets are checked, the trace is filed under the task
//! and the job, tokens are counted. [`ops`] makes it all hub operations (so MCP tools) and REST routes under
//! `/v1/factory/...`; [`cli`] is `model-mistress factory ...`. None of it is in the peer allowlist.
//!
//! The Forge plugs in through two traits in [`model`]: [`model::Generate`] ("generate with model X", for models the
//! bundled llama.cpp cannot serve) and [`model::Trainer`] ("train an adapter").

pub mod build;
pub mod checks;
pub mod cli;
pub mod collect;
pub mod embed;
pub mod evaluate;
pub mod gather;
pub mod generate;
pub mod model;
pub mod ops;
pub mod plan;
pub mod publish;
pub mod run;
pub mod spec;
pub mod store;
pub mod tag;
pub mod util;
pub mod verify;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use checks::Check;
use model::{Generate, HubGenerator, Trainer};
use run::{Halt, Run};
use spec::TaskSpec;
use store::Db;

use crate::engine::{Engine, Fail};
use crate::hub::Hub;

pub type Res<T> = Result<T, Fail>;

pub const STEPS: [&str; 8] = [
    "plan", "gather", "generate", "verify", "tag", "build", "evaluate", "publish",
];

/// How a step ended when it ended normally.
pub enum StepEnd {
    Done(Value),
    Skipped(String),
}

/// How a whole run ended when it was not stopped.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Done,
    /// Stopped on purpose until the owner acts (the plan needs approval).
    Waiting(String),
}

/// `<home>/factory/settings.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FactorySettings {
    /// Where vectors come from: `km` (the KM runtime's embedder: `km.embedder`), `hub` (the model in `embed_model`,
    /// asked in batches) or `hash` (built in, offline).
    pub embedder: String,
    /// The hub model that embeds when `embedder` is `hub`; also the id KMs' `routing.embed_model` carries.
    pub embed_model: String,
    /// Promotion policy when a task names none: gated, propose, auto or off.
    pub promotion: String,
    /// What `gated` needs: the least uplift (0..1) the evaluation must show.
    pub min_uplift: f64,
    /// Add tag values the author model proposes to the vocabulary at once (otherwise they wait for approval).
    pub auto_add_vocabulary: bool,
    /// Records below this quality (or not fully supported) go to the review queue.
    pub review_threshold: f64,
    /// Tags at or above this confidence are attached; between `tag_floor` and this they go to the review queue.
    pub tag_threshold: f64,
    pub tag_floor: f64,
    /// Embedding similarity at which two records (or a record and an evaluation question) are the same.
    pub dedupe_threshold: f64,
    pub leak_threshold: f64,
    pub chunk_chars: usize,
    pub max_file_bytes: u64,
    pub max_files: usize,
    pub max_total_bytes: u64,
    pub max_url_bytes: u64,
    /// Times a model is asked again when its JSON is not valid.
    pub max_retries: u32,
    pub gen_max_tokens: u32,
    /// Notes put in front of the base model in the evaluation.
    pub eval_notes: usize,
}

impl Default for FactorySettings {
    fn default() -> Self {
        Self {
            embedder: "km".into(),
            embed_model: "mm-embed-1".into(),
            promotion: "gated".into(),
            min_uplift: 0.05,
            auto_add_vocabulary: false,
            review_threshold: 0.6,
            tag_threshold: 0.6,
            tag_floor: 0.3,
            dedupe_threshold: 0.92,
            leak_threshold: 0.95,
            chunk_chars: 1200,
            max_file_bytes: 512 * 1024,
            max_files: 500,
            max_total_bytes: 16 * 1024 * 1024,
            max_url_bytes: 1024 * 1024,
            max_retries: 3,
            gen_max_tokens: 1500,
            eval_notes: 3,
        }
    }
}

pub struct Factory {
    pub home: PathBuf,
    pub dir: PathBuf,
    pub db: Db,
    generator: Mutex<Arc<dyn Generate>>,
    trainer: Mutex<Option<Arc<dyn Trainer>>>,
    checks: Mutex<Vec<Arc<dyn Check>>>,
}

static RUNTIMES: OnceLock<Mutex<HashMap<PathBuf, Arc<Factory>>>> = OnceLock::new();

/// The factory of a hub's home folder (made on first use; a task left running by a hub that is gone is marked
/// interrupted then).
pub fn runtime(engine: &Arc<Engine>) -> Res<Arc<Factory>> {
    let map = RUNTIMES.get_or_init(Default::default);
    let mut m = map.lock();
    if let Some(f) = m.get(&engine.home) {
        return Ok(f.clone());
    }
    let f = Factory::open(&engine.home)?;
    m.insert(engine.home.clone(), f.clone());
    Ok(f)
}

impl Factory {
    pub fn open(home: &Path) -> Res<Arc<Factory>> {
        let dir = home.join("factory");
        let db = Db::open(&dir)?;
        db.interrupt_orphans()?;
        Ok(Arc::new(Factory {
            home: home.to_path_buf(),
            dir,
            db,
            generator: Mutex::new(Arc::new(HubGenerator)),
            trainer: Mutex::new(None),
            checks: Mutex::new(checks::default_checks()),
        }))
    }

    pub fn settings(&self) -> FactorySettings {
        std::fs::read(self.dir.join("settings.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn set_settings(&self, patch: &Value) -> Res<FactorySettings> {
        let mut cur = serde_json::to_value(self.settings()).map_err(|e| Fail::new(500, "io", e.to_string()))?;
        let Some(p) = patch.as_object() else {
            return Err(Fail::invalid("settings must be an object"));
        };
        for (k, v) in p {
            if cur.get(k).is_none() {
                return Err(Fail::invalid(format!("no factory setting {k}")));
            }
            cur[k] = v.clone();
        }
        let new: FactorySettings = serde_json::from_value(cur).map_err(|e| Fail::invalid(e.to_string()))?;
        if !matches!(new.embedder.as_str(), "km" | "hub" | "hash") {
            return Err(Fail::invalid("embedder must be km, hub or hash"));
        }
        if !matches!(new.promotion.as_str(), "gated" | "propose" | "auto" | "off") {
            return Err(Fail::invalid("promotion must be gated, propose, auto or off"));
        }
        let text = serde_json::to_vec_pretty(&new).map_err(|e| Fail::new(500, "io", e.to_string()))?;
        let path = self.dir.join("settings.json");
        let tmp = self.dir.join("settings.json.tmp");
        std::fs::write(&tmp, text)
            .and_then(|_| std::fs::rename(&tmp, &path))
            .map_err(|e| Fail::new(500, "io", format!("{}: {e}", path.display())))?;
        Ok(new)
    }

    // ---- what the Forge and tests plug in ------------------------------------------------------------------

    pub fn generator(&self) -> Arc<dyn Generate> {
        self.generator.lock().clone()
    }
    pub fn set_generator(&self, g: Arc<dyn Generate>) {
        *self.generator.lock() = g;
    }
    pub fn trainer(&self) -> Option<Arc<dyn Trainer>> {
        self.trainer.lock().clone()
    }
    pub fn set_trainer(&self, t: Option<Arc<dyn Trainer>>) {
        *self.trainer.lock() = t;
    }
    pub fn checks(&self) -> Vec<Arc<dyn Check>> {
        self.checks.lock().clone()
    }
    pub fn add_check(&self, c: Arc<dyn Check>) {
        self.checks.lock().push(c);
    }
}

/// Runs the steps that are not done yet, in order.
pub async fn drive(run: &Run) -> Result<Outcome, Halt> {
    for (i, name) in STEPS.iter().enumerate() {
        let st = run.f.db.step(&run.id, name);
        if st
            .as_ref()
            .is_some_and(|s| matches!(s.status.as_str(), "done" | "skipped"))
        {
            continue;
        }
        run.check()?;
        // a plan that needs approval is not built on until it has it
        if *name != "plan" && run.spec.plan_approval {
            if let Some((rev, _, approved)) = run.f.db.plan(&run.id) {
                if !approved {
                    return Ok(Outcome::Waiting(format!(
                        "plan revision {rev} needs approval (factory.task.plan.edit with approve), then resume"
                    )));
                }
            }
        }
        run.set_step(name);
        run.f.db.set_step(&run.id, name, "running", None, None)?;
        run.f.db.set_state(&run.id, "running", Some(name), None)?;
        run.progress(i as u64, STEPS.len() as u64, "starting");
        run.log("info", &format!("step {name} starts"));
        let res = match *name {
            "plan" => plan::run(run).await,
            "gather" => collect::run(run).await,
            "generate" => generate::run(run).await,
            "verify" => verify::run(run).await,
            "tag" => tag::run(run).await,
            "build" => build::run(run).await,
            "evaluate" => evaluate::run(run).await,
            _ => publish::run(run).await,
        };
        match res {
            Ok(StepEnd::Done(v)) => {
                run.f.db.set_step(&run.id, name, "done", Some(&v), None)?;
                run.log("info", &format!("step {name} done"));
            }
            Ok(StepEnd::Skipped(why)) => {
                run.f
                    .db
                    .set_step(&run.id, name, "skipped", Some(&json!({"skipped": why})), None)?;
                run.log("info", &format!("step {name} skipped: {why}"));
            }
            Err(Halt::Fail(f)) => {
                run.f.db.set_step(&run.id, name, "failed", None, Some(&f.message))?;
                run.log("error", &format!("step {name} failed: {}", f.message));
                return Err(Halt::Fail(f));
            }
            Err(h) => {
                run.f.db.set_step(&run.id, name, "interrupted", None, None)?;
                run.log("warn", &format!("step {name} stopped: {h:?}"));
                return Err(h);
            }
        }
    }
    Ok(Outcome::Done)
}

/// What the owner wants to see when a task is done.
pub fn summarize(run: &Run) -> Value {
    let db = &run.f.db;
    let rows = db.records(&run.id, None);
    let count = |f: &dyn Fn(&store::RecordRow) -> bool| rows.iter().filter(|r| f(r)).count();
    let step = |n: &str| db.step(&run.id, n).map(|s| s.result).unwrap_or(Value::Null);
    let eval = step("evaluate");
    let built = step("build");
    let kms: Vec<Value> = publish::finals(run)
        .into_iter()
        .map(
            |(id, version, kind)| json!({"id": id, "version": version, "kind": kind, "key": format!("{id}@{version}")}),
        )
        .collect();
    let (_, open_items) = (0, db.review_open_count(&run.id));
    json!({
        "records": {"generated": rows.len(), "rejected": count(&|r| r.status == "rejected"),
                    "approved": count(&|r| r.status == "approved"), "pending": count(&|r| r.status == "pending"),
                    "heldout": count(&|r| r.split == "heldout" && r.status != "rejected"),
                    "verified": step("verify")},
        "chunks": db.chunks(&run.id).len(),
        "tags": step("tag"),
        "kms": kms,
        "adapter": built["adapter"],
        "uplift": eval["uplift"],
        "evaluation": {"with": eval["score_with_km"], "without": eval["score_without_km"], "n": eval["evaluated"]},
        "publish": step("publish"),
        "review_open": open_items,
        "usage": run.f.db.task(&run.id).map(|t| t.usage).unwrap_or(Value::Null),
    })
}

/// Records how a run ended: the task's state and, for the job, its result.
pub fn finish(run: &Run, out: Result<Outcome, Halt>) -> Res<Value> {
    let db = &run.f.db;
    match out {
        Ok(Outcome::Done) => {
            let s = summarize(run);
            db.set_summary(&run.id, &s)?;
            db.set_state(&run.id, "done", None, None)?;
            Ok(s)
        }
        Ok(Outcome::Waiting(why)) => {
            db.set_state(&run.id, "awaiting_approval", None, Some(&why))?;
            Ok(json!({"waiting": why}))
        }
        Err(Halt::Cancelled) => {
            db.set_state(&run.id, "cancelled", None, None)?;
            let _ = db.set_cancel(&run.id, false);
            Err(Fail::new(
                499,
                "cancelled",
                "the task was cancelled; resume continues it",
            ))
        }
        Err(Halt::Budget(m)) => {
            db.set_state(&run.id, "budget", None, Some(&m))?;
            Err(Fail::new(429, "budget", format!("{m}; raise the budget and resume")))
        }
        Err(Halt::Fail(f)) => {
            db.set_state(&run.id, "failed", None, Some(&f.message))?;
            Err(f)
        }
    }
}

/// Starts the task's pipeline as a hub job; returns the job. Steps already done are not run again.
pub fn start(hub: &Hub, f: &Arc<Factory>, id: &str) -> Res<Value> {
    let t = f.db.task(id)?;
    if let Some(j) = t.job.as_deref().and_then(|j| hub.jobs.get(j)) {
        if j.status() == "running" {
            return Err(Fail::new(
                409,
                "running",
                format!("task {id} is already running as job {}", j.id),
            ));
        }
    }
    let spec = TaskSpec::from_json(&t.spec)?;
    let km = crate::km::runtime(&hub.engine)?;
    f.db.set_cancel(id, false)?;
    f.db.set_state(id, "running", None, None)?;
    let (h, ff, tid) = (hub.clone(), f.clone(), id.to_string());
    let job = hub.jobs.start_task("factory", id.to_string(), move |job| async move {
        let _ = ff.db.set_job(&tid, Some(&job.id));
        let run = Run::new(h, ff, km, &tid, spec, Some(job));
        let out = drive(&run).await;
        finish(&run, out)
    });
    f.db.set_job(id, Some(&job.id))?;
    Ok(json!({"task": id, "job": job.id, "state": "running"}))
}

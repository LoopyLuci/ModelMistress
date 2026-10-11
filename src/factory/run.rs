//! What a pipeline step works with: the task, the job it runs in, budgets, and the only way to call a model
//! (`ask`), which checks cancellation and budgets first, files the trace under the task and job, and counts tokens.

use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use serde_json::{json, Value};

use super::model::{extract_json, Device, GenReply, GenRequest};
use super::spec::TaskSpec;
use super::util;
use super::Factory;
use crate::engine::Fail;
use crate::hub::Hub;
use crate::km::embed::Embedder;
use crate::km::Km;
use crate::pull::Job;
use crate::trace::Scope;

/// Why a step stopped before it finished. All three leave the task resumable.
#[derive(Debug)]
pub enum Halt {
    Cancelled,
    /// A budget (tokens or wall time) was reached; the text says which.
    Budget(String),
    Fail(Fail),
}

impl From<Fail> for Halt {
    fn from(f: Fail) -> Self {
        Halt::Fail(f)
    }
}

pub type StepRes<T> = Result<T, Halt>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role {
    Author,
    Reviewer,
    /// The model the evaluation answers with.
    Base,
}

#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub tokens: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub calls: u64,
    pub retries: u64,
    /// Wall time of earlier runs of this task.
    pub wall_before_ms: u64,
}

impl Usage {
    pub fn from_json(v: &Value) -> Usage {
        let n = |k: &str| v[k].as_u64().unwrap_or(0);
        Usage {
            tokens: n("tokens"),
            prompt_tokens: n("prompt_tokens"),
            completion_tokens: n("completion_tokens"),
            calls: n("calls"),
            retries: n("retries"),
            wall_before_ms: n("wall_ms"),
        }
    }
}

pub struct Run {
    pub hub: Hub,
    pub f: Arc<Factory>,
    pub km: Arc<Km>,
    pub id: String,
    pub spec: TaskSpec,
    pub job: Option<Arc<Job>>,
    pub job_id: String,
    pub started: Instant,
    usage: Mutex<Usage>,
    step: Mutex<String>,
    embedder: Mutex<Option<Arc<dyn Embedder>>>,
}

impl Run {
    pub fn new(hub: Hub, f: Arc<Factory>, km: Arc<Km>, id: &str, spec: TaskSpec, job: Option<Arc<Job>>) -> Run {
        let usage = f.db.task(id).map(|t| Usage::from_json(&t.usage)).unwrap_or_default();
        let job_id = job.as_ref().map(|j| j.id.clone()).unwrap_or_default();
        Run {
            hub,
            f,
            km,
            id: id.to_string(),
            spec,
            job,
            job_id,
            started: Instant::now(),
            usage: Mutex::new(usage),
            step: Mutex::new(String::new()),
            embedder: Mutex::new(None),
        }
    }

    pub fn home(&self) -> &std::path::Path {
        &self.hub.engine.home
    }

    pub fn set_step(&self, name: &str) {
        *self.step.lock() = name.to_string();
    }

    pub fn log(&self, level: &str, msg: &str) {
        self.f.db.log(&self.id, &self.step.lock(), level, msg);
    }

    pub fn progress(&self, done: u64, total: u64, note: &str) {
        if let Some(j) = &self.job {
            j.progress(done, total, format!("{}: {note}", self.step.lock()));
        }
    }

    pub fn usage(&self) -> Usage {
        self.usage.lock().clone()
    }

    pub fn wall_ms(&self) -> u64 {
        self.usage.lock().wall_before_ms + self.started.elapsed().as_millis() as u64
    }

    fn save_usage(&self) {
        let u = self.usage.lock().clone();
        let _ = self.f.db.set_usage(
            &self.id,
            &json!({"tokens": u.tokens, "prompt_tokens": u.prompt_tokens, "completion_tokens": u.completion_tokens,
                    "calls": u.calls, "retries": u.retries,
                    "wall_ms": u.wall_before_ms + self.started.elapsed().as_millis() as u64}),
        );
    }

    /// Stops the step if the task was cancelled or a budget is used up.
    pub fn check(&self) -> StepRes<()> {
        if self.job.as_ref().is_some_and(|j| j.cancelled()) || self.f.db.cancel_requested(&self.id) {
            return Err(Halt::Cancelled);
        }
        let max_tokens = self.spec.size.max_tokens;
        if max_tokens > 0 && self.usage.lock().tokens >= max_tokens {
            return Err(Halt::Budget(format!(
                "token budget reached ({} of {max_tokens} tokens)",
                self.usage.lock().tokens
            )));
        }
        let wall = self.spec.budget.wall_s;
        if wall > 0 && self.wall_ms() >= wall * 1000 {
            return Err(Halt::Budget(format!(
                "wall time budget reached ({} s of {wall} s)",
                self.wall_ms() / 1000
            )));
        }
        Ok(())
    }

    pub fn model_of(&self, role: Role) -> (String, Device) {
        match role {
            Role::Author => (self.spec.author.clone(), self.spec.dev("author")),
            Role::Reviewer => (self.spec.reviewer_model(), self.spec.dev("reviewer")),
            // `base` may name a core (dusty) rather than a hub model: then the author answers
            Role::Base => {
                let b = self.spec.base_model();
                let m = if self.hub.engine.resolve(&b).is_ok() {
                    b
                } else {
                    self.spec.author.clone()
                };
                (m, self.spec.dev("base"))
            }
        }
    }

    /// One model call, traced under this task and job.
    pub async fn ask_messages(
        &self,
        role: Role,
        purpose: &str,
        messages: Vec<Value>,
        max_tokens: u32,
        json_out: bool,
    ) -> StepRes<GenReply> {
        self.check()?;
        let (model, device) = self.model_of(role);
        let req = GenRequest {
            model,
            messages,
            max_tokens,
            temperature: if purpose.starts_with("generate") { 0.8 } else { 0.2 },
            json: json_out,
            think: self.spec.think,
            device,
            scope: Scope::new(
                Some(&self.job_id).filter(|j| !j.is_empty()).map(String::as_str),
                Some(&self.id),
            ),
            purpose: format!("{}:{purpose}", self.step.lock()),
        };
        let gen = self.f.generator();
        match gen.generate(&self.hub, req).await {
            Ok(r) => {
                {
                    let mut u = self.usage.lock();
                    u.calls += 1;
                    u.prompt_tokens += r.prompt_tokens;
                    u.completion_tokens += r.completion_tokens;
                    u.tokens += r.prompt_tokens + r.completion_tokens;
                }
                self.save_usage();
                Ok(r)
            }
            Err(f) => {
                if matches!(self.check(), Err(Halt::Cancelled)) {
                    return Err(Halt::Cancelled);
                }
                Err(Halt::Fail(f))
            }
        }
    }

    pub async fn ask(&self, role: Role, purpose: &str, system: &str, user: &str, max_tokens: u32) -> StepRes<GenReply> {
        self.ask_messages(
            role,
            purpose,
            vec![
                json!({"role": "system", "content": system}),
                json!({"role": "user", "content": user}),
            ],
            max_tokens,
            false,
        )
        .await
    }

    /// Asks for a JSON object and checks it with `validate`; an answer that is not JSON, or that `validate` refuses,
    /// is sent back to the model with the reason, up to `max_retries` times. The outer result is a halt (cancel,
    /// budget, a failed call); the inner one is "the model never produced a valid answer" with the last reason.
    #[allow(clippy::too_many_arguments)]
    pub async fn ask_json(
        &self,
        role: Role,
        purpose: &str,
        system: &str,
        user: &str,
        max_tokens: u32,
        validate: &(dyn Fn(&Value) -> Result<(), String> + Sync),
    ) -> StepRes<Result<(Value, GenReply), String>> {
        let mut messages = vec![
            json!({"role": "system", "content": system}),
            json!({"role": "user", "content": user}),
        ];
        let mut last = String::new();
        let tries = self.f.settings().max_retries;
        for attempt in 0..=tries {
            let reply = self
                .ask_messages(role, purpose, messages.clone(), max_tokens, true)
                .await?;
            match extract_json(&reply.text).and_then(|v| validate(&v).map(|_| v)) {
                Ok(v) => return Ok(Ok((v, reply))),
                Err(e) => {
                    last = e.clone();
                    self.log(
                        "warn",
                        &format!(
                            "{purpose}: answer refused ({e}); attempt {} of {}",
                            attempt + 1,
                            tries + 1
                        ),
                    );
                    {
                        self.usage.lock().retries += 1;
                    }
                    messages.push(json!({"role": "assistant", "content": util::truncate(&reply.text, 1500)}));
                    messages.push(json!({"role": "user", "content":
                        format!("That answer was not accepted: {e}. Answer again with the corrected JSON object only.")}));
                }
            }
        }
        self.save_usage();
        Ok(Err(last))
    }

    // ---- embeddings ----------------------------------------------------------------------------------------

    pub fn embedder(&self) -> Arc<dyn Embedder> {
        let mut g = self.embedder.lock();
        if let Some(e) = g.clone() {
            return e;
        }
        let s = self.f.settings();
        let e: Arc<dyn Embedder> = match s.embedder.as_str() {
            "hash" => Arc::new(crate::km::embed::HashEmbedder::default()),
            "hub" => Arc::new(super::embed::HubEmbedding::new(
                self.hub.engine.clone(),
                &s.embed_model,
                self.spec.dev("embedder"),
            )),
            _ => self.km.embedder(),
        };
        *g = Some(e.clone());
        e
    }

    pub async fn embed(&self, texts: &[String]) -> StepRes<Vec<Vec<f32>>> {
        self.check()?;
        if texts.is_empty() {
            return Ok(vec![]);
        }
        Ok(self.embedder().embed(texts).await?)
    }

    // ---- the plan and the tools ----------------------------------------------------------------------------

    pub fn plan(&self) -> StepRes<Value> {
        self.f
            .db
            .plan(&self.id)
            .map(|p| p.1)
            .ok_or_else(|| Halt::Fail(Fail::new(409, "no_plan", "the task has no plan yet")))
    }

    pub fn tools(&self) -> Vec<super::checks::ToolDef> {
        self.f
            .db
            .chunks(&self.id)
            .into_iter()
            .filter(|c| c.role == "tool")
            .filter_map(|c| super::checks::ToolDef::from_json(&c.origin["def"]))
            .collect()
    }

    pub fn dir(&self) -> std::path::PathBuf {
        self.f.dir.join("tasks").join(&self.id)
    }
}

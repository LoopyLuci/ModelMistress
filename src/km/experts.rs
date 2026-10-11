//! Attaching experts KMs to a core: assemble the base GGUF plus every experts KM resident on that core, then reload
//! the core from the assembled file through the engine's hot swap (the old instance serves until the new one is
//! ready, and stays if the new one does not load). Evicting the last one puts the base file back.
//!
//! [`ExpertsAttacher`] keeps the set of experts KMs per core itself; [`ComposedAttacher`] sends a LoRA adapter's file
//! to the engine's LoRA mechanism and an experts KM's folder here, so both kinds can be on one core.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::future::BoxFuture;

use super::assemble::{self, ExpertsKm};
use super::residency::Attacher;
use super::Res;
use crate::engine::{Engine, Fail, Target};

/// What the attacher needs of the engine: where a core's own GGUF is, and how to make it run from another file.
pub trait CoreModel: Send + Sync {
    fn home(&self) -> PathBuf;
    /// The core's original GGUF.
    fn base(&self, core: &str) -> Res<PathBuf>;
    /// Runs the core from `model` (the assembled file), or from its base file again for `None`. A hot swap when the
    /// core is loaded; an error leaves the previous model serving.
    fn swap<'a>(&'a self, core: &'a str, model: Option<PathBuf>) -> BoxFuture<'a, Res<()>>;
}

pub struct EngineModel(pub Arc<Engine>);

impl EngineModel {
    fn entry(&self, core: &str) -> Res<crate::catalog::ModelEntry> {
        match self.0.resolve(core)? {
            Target::Llama(e) => Ok(e),
            Target::Ollama(_) => Err(Fail::invalid("Ollama's models take no experts from ModelMistress")),
        }
    }
}

impl CoreModel for EngineModel {
    fn home(&self) -> PathBuf {
        self.0.home.clone()
    }
    fn base(&self, core: &str) -> Res<PathBuf> {
        Ok(self.entry(core)?.path)
    }
    fn swap<'a>(&'a self, core: &'a str, model: Option<PathBuf>) -> BoxFuture<'a, Res<()>> {
        Box::pin(async move {
            let entry = self.entry(core)?;
            let mut x = self.0.extras(&entry.id);
            if x.model_path == model {
                return Ok(());
            }
            x.model_path = model;
            self.0.change_extras(&entry, x).await.map(|_| ())
        })
    }
}

pub struct ExpertsAttacher {
    model: Arc<dyn CoreModel>,
    /// Per core: the experts KM folders attached, in attach order. Held across the swap, so changes queue.
    sets: tokio::sync::Mutex<HashMap<String, Vec<PathBuf>>>,
}

impl ExpertsAttacher {
    pub fn new(model: Arc<dyn CoreModel>) -> Self {
        Self {
            model,
            sets: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Makes the core run with exactly `dirs` assembled into it.
    async fn apply(&self, core: &str, dirs: &[PathBuf]) -> Res<()> {
        let home = self.model.home();
        if dirs.is_empty() {
            self.model.swap(core, None).await?;
            assemble::prune(&home, core, None);
            return Ok(());
        }
        let base = self.model.base(core)?;
        let (h, c, d) = (home.clone(), core.to_string(), dirs.to_vec());
        let made = tokio::task::spawn_blocking(move || {
            let kms = d.iter().map(|p| ExpertsKm::open(p)).collect::<Res<Vec<_>>>()?;
            assemble::assemble(&h, &c, &base, &kms)
        })
        .await
        .map_err(|e| Fail::new(500, "internal", e.to_string()))??;
        self.model.swap(core, Some(made.path.clone())).await?;
        assemble::prune(&home, core, Some(&made.path));
        Ok(())
    }
}

impl Attacher for ExpertsAttacher {
    fn attach<'a>(&'a self, core: &'a str, dir: &'a Path, _scale: f32) -> BoxFuture<'a, Res<()>> {
        Box::pin(async move {
            let mut sets = self.sets.lock().await;
            let mut next = sets.get(core).cloned().unwrap_or_default();
            if !next.iter().any(|p| p == dir) {
                next.push(dir.to_path_buf());
            }
            self.apply(core, &next).await?;
            sets.insert(core.to_string(), next);
            Ok(())
        })
    }
    fn detach<'a>(&'a self, core: &'a str, dir: &'a Path) -> BoxFuture<'a, Res<()>> {
        Box::pin(async move {
            let mut sets = self.sets.lock().await;
            let mut next = sets.get(core).cloned().unwrap_or_default();
            next.retain(|p| p != dir);
            self.apply(core, &next).await?;
            sets.insert(core.to_string(), next);
            Ok(())
        })
    }
}

#[cfg(test)]
#[path = "experts_tests.rs"]
mod tests;

/// An experts KM is attached by its folder (it has a `km.toml`); a LoRA adapter by its GGUF file.
pub fn is_experts_dir(path: &Path) -> bool {
    path.join("km.toml").is_file()
}

pub struct ComposedAttacher {
    pub lora: Arc<dyn Attacher>,
    pub experts: Arc<dyn Attacher>,
}

impl ComposedAttacher {
    /// The real thing: the engine's LoRA mechanism plus experts assembly into the same engine's cores.
    pub fn for_engine(engine: &Arc<Engine>) -> Self {
        Self {
            lora: Arc::new(super::residency::EngineAttacher(engine.clone())),
            experts: Arc::new(ExpertsAttacher::new(Arc::new(EngineModel(engine.clone())))),
        }
    }
    fn pick(&self, path: &Path) -> &Arc<dyn Attacher> {
        if is_experts_dir(path) {
            &self.experts
        } else {
            &self.lora
        }
    }
}

impl Attacher for ComposedAttacher {
    fn attach<'a>(&'a self, core: &'a str, path: &'a Path, scale: f32) -> BoxFuture<'a, Res<()>> {
        self.pick(path).attach(core, path, scale)
    }
    fn detach<'a>(&'a self, core: &'a str, path: &'a Path) -> BoxFuture<'a, Res<()>> {
        self.pick(path).detach(core, path)
    }
}

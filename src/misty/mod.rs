//! Misty: ModelMistress's own agent: a CPU-first roster of local models behind a router, local retrieval, system tools
//! behind an approval policy, and pipelines. It lives in the hub, so everything that reaches the hub reaches her.
//! The design, with the research behind it, is in `docs/misty/DESIGN.md`.
//!
//!   roster    which model fills which role, and whether it fits and can run here
//!   router    which role handles a request (pure code, explainable)
//!   rag       the document index (SQLite, BM25 plus optional vectors)
//!   toolbox   the tools, their classes and the guards that hold whatever a model says
//!   cactus    Needle 3 / Whistle / Mirage through the Cactus runtime library
//!   agent     the loop: route, retrieve, reflex, generate, act, escalate
//!   auto      the router without the agent: the model name `auto`, `router.status`, `router.pick`
//!   runs      live run logs, cancellation and approvals
//!   state     sessions and finished runs
//!   pipeline  short declarative chains of steps
//!   setup     what to download, where it goes, and checks that it works
//!   ops       the `misty.*` operations, REST routes, the OpenAI `misty` model
//!   cli       `model-mistress misty ...`

pub mod agent;
pub mod auto;
pub mod cactus;
pub mod cli;
pub mod config;
pub mod ops;
pub mod pipeline;
pub mod rag;
pub mod roster;
pub mod router;
pub mod runs;
pub mod setup;
pub mod state;
pub mod toolbox;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use serde_json::{json, Value};

use crate::engine::Fail;
use crate::hub::Hub;
use roster::{Engines, Resolution, Role};

/// llama.cpp's first build with the Hadamard activation runtime that `Q2_0` weights need.
pub const STOCK_Q2_BUILD: u32 = 11556;

pub struct Misty {
    /// `<home>/misty`
    pub dir: PathBuf,
    pub home: PathBuf,
    pub cfg: RwLock<config::Config>,
    pub rag: rag::Rag,
    pub state: state::Store,
    pub cactus: Arc<dyn cactus::CactusRuntime>,
    pub runs: runs::Runs,
    /// The language-model client the agent uses; `None` is the hub's own chat path. Tests put a scripted one here.
    llm: Mutex<Option<Arc<dyn agent::Llm>>>,
    warmed: AtomicBool,
}

impl Misty {
    pub fn open(home: &Path) -> Result<Self, String> {
        Self::open_with(home, Arc::new(cactus::DllRuntime::new(home)))
    }

    pub fn open_with(home: &Path, rt: Arc<dyn cactus::CactusRuntime>) -> Result<Self, String> {
        let dir = home.join("misty");
        std::fs::create_dir_all(dir.join("workspace")).map_err(|e| format!("{}: {e}", dir.display()))?;
        let cfg = config::Config::load(&dir)?;
        Ok(Self {
            rag: rag::Rag::open(&dir.join("rag.db"))?,
            state: state::Store::open(&dir.join("state.db"))?,
            cfg: RwLock::new(cfg),
            cactus: rt,
            runs: runs::Runs::default(),
            llm: Mutex::new(None),
            warmed: AtomicBool::new(false),
            dir,
            home: home.to_path_buf(),
        })
    }

    /// The hub's Misty, opened on first use.
    pub fn get(hub: &Hub) -> Result<Arc<Misty>, Fail> {
        let mut slot = hub.misty.lock();
        if let Some(m) = slot.as_ref() {
            return Ok(m.clone());
        }
        let m = Arc::new(Misty::open(&hub.engine.home).map_err(|e| Fail::new(500, "misty_state", e))?);
        let threads = m.cfg.read().threads as i32;
        m.cactus.set_threads(threads);
        *slot = Some(m.clone());
        Ok(m)
    }

    pub fn set_llm(&self, llm: Arc<dyn agent::Llm>) {
        *self.llm.lock() = Some(llm);
    }

    /// The model client for a run: the scripted one if a test set it, else the hub's chat path.
    pub fn llm(&self, hub: &Hub) -> Arc<dyn agent::Llm> {
        self.llm
            .lock()
            .clone()
            .unwrap_or_else(|| Arc::new(agent::HubLlm(hub.clone())))
    }

    /// Loads the `.cact` archives that are installed, once. Loading reads tens of megabytes, so it is off the async threads.
    pub async fn warm(self: &Arc<Self>) {
        if self.warmed.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        let _ = tokio::task::spawn_blocking(move || {
            for (role, file, r) in cactus::load_archives(me.cactus.as_ref(), &me.home) {
                if let Err(e) = r {
                    tracing::warn!("misty: {role} ({file}) did not load: {e}");
                }
            }
        })
        .await;
    }

    pub fn free_ram() -> u64 {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        sys.available_memory()
    }

    /// What the machine's llama.cpp builds can run.
    pub async fn engines(&self, hub: &Hub) -> Engines {
        let build = hub.engine.llama_version().await.and_then(|v| parse_build(&v));
        Engines {
            stock_q2: build.map(|b| b >= STOCK_Q2_BUILD).unwrap_or(false),
            prism: hub.engine.prism_server().is_some(),
        }
    }

    pub async fn resolutions(&self, hub: &Hub) -> Vec<Resolution> {
        let cfg = self.cfg.read().clone();
        let engines = self.engines(hub).await;
        roster::resolve(
            &hub.engine.catalog(),
            &cfg.roles,
            engines,
            Self::free_ram(),
            cfg.ram_margin_mib * 1024 * 1024,
        )
    }

    /// The router's view of the machine, and the resolutions it came from.
    pub async fn fleet(&self, hub: &Hub) -> (router::Fleet, Vec<Resolution>) {
        let res = self.resolutions(hub).await;
        let st = self.cactus.status();
        let ready = res
            .iter()
            .filter(|r| r.role.is_brain() && r.status == "ready")
            .map(|r| r.role)
            .collect();
        let warm = res
            .iter()
            .filter(|r| r.role.is_brain() && r.model.as_deref().map(|m| hub.engine.is_loaded(m)).unwrap_or(false))
            .map(|r| r.role)
            .collect();
        let mut cactus = vec![];
        for (role, _, mask) in cactus::ARCHIVES {
            if st.has(mask) {
                cactus.push(Role::parse(role).expect("a known role"));
            }
        }
        (router::Fleet { ready, warm, cactus }, res)
    }

    /// Everything `misty.status` shows.
    pub async fn report(&self, hub: &Hub) -> Value {
        let (fleet, res) = self.fleet(hub).await;
        let st = self.cactus.status();
        let engines = self.engines(hub).await;
        let archives: Vec<Value> = cactus::ARCHIVES
            .iter()
            .map(|(role, file, mask)| {
                json!({"role": role, "file": file, "present": cactus::archive_dir(&self.home).join(file).is_file(), "loaded": st.has(*mask)})
            })
            .collect();
        let roster: Vec<Value> = res
            .iter()
            .map(|r| {
                let mut v = serde_json::to_value(r).unwrap_or(Value::Null);
                v["loaded"] = json!(r.model.as_deref().map(|m| hub.engine.is_loaded(m)).unwrap_or(false));
                v
            })
            .collect();
        let mut next = vec![];
        if fleet.ready.is_empty() {
            next.push(
                "no language model is ready: run `model-mistress misty setup` to see what to install".to_string(),
            );
        }
        if !st.available {
            next.push("the Cactus runtime is not installed: Needle (tool choice), Whistle (speech) and Mirage (images) are off".to_string());
        }
        let cfg = self.cfg.read().clone();
        json!({
            "ready": !fleet.ready.is_empty(),
            "roster": roster,
            "cactus": {"available": st.available, "library": st.library, "models": st.models, "error": st.error, "archives": archives},
            "engines": {"stock_q2": engines.stock_q2, "prism": engines.prism, "llama_version": hub.engine.llama_version().await},
            "free_ram_mb": Self::free_ram() / (1024 * 1024),
            "rag": {"collections": self.rag.collections().unwrap_or_default().into_iter()
                        .map(|(k, (s, c))| json!({"collection": k, "sources": s, "chunks": c})).collect::<Vec<_>>()},
            "policy": cfg.policy,
            "roots": cfg.roots_or_default(&self.dir),
            "runs_active": self.runs.active().len(),
            "approvals_pending": self.runs.pending().len(),
            "next": next,
        })
    }

    // ---- retrieval, with vectors when an embedding model is installed ------------------------------------------

    /// Embeds `texts` with the roster's `embed` model. `None` when there is none (retrieval then stays lexical).
    pub async fn embed_texts(&self, hub: &Hub, texts: &[String]) -> Result<Option<(String, Vec<Vec<f32>>)>, String> {
        let res = self.resolutions(hub).await;
        let Some(model) = res
            .iter()
            .find(|r| r.role == Role::Embed && r.status == "ready")
            .and_then(|r| r.model.clone())
        else {
            return Ok(None);
        };
        let mut all = vec![];
        for batch in texts.chunks(16) {
            // An embedder takes at most its physical batch of tokens per input (512 unless the model's profile says
            // more). A passage that does not fit is cut, longest first, until it does: a vector of the start of a
            // chunk finds the chunk; no vector would not.
            let mut cur: Vec<String> = batch.to_vec();
            let mut cuts = 0;
            let vecs = loop {
                match self.embed_batch(hub, &model, &cur).await {
                    Ok(v) => break v,
                    Err(e) if cuts < MAX_EMBED_CUTS && input_too_large(&e) => {
                        let longest = cur.iter().map(|t| t.chars().count()).max().unwrap_or(0);
                        if longest <= 64 {
                            return Err(e);
                        }
                        cuts += 1;
                        cur = clip_all(&cur, longest / 2);
                    }
                    Err(e) => return Err(e),
                }
            };
            all.extend(vecs);
        }
        if all.len() != texts.len() {
            return Err(format!("asked for {} embeddings, got {}", texts.len(), all.len()));
        }
        Ok(Some((model, all)))
    }

    /// One `/v1/embeddings` request for `batch`.
    async fn embed_batch(&self, hub: &Hub, model: &str, batch: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let body = json!({"model": model, "input": batch});
        let (_, resp, _lease) = hub
            .engine
            .forward("/v1/embeddings", body)
            .await
            .map_err(|f| f.message)?;
        let status = resp.status();
        let v: Value = resp.json().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(v["error"]["message"]
                .as_str()
                .unwrap_or("the embedding model failed")
                .to_string());
        }
        let mut out = vec![];
        for item in v["data"].as_array().cloned().unwrap_or_default() {
            let e: Vec<f32> = item["embedding"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect())
                .unwrap_or_default();
            if e.is_empty() {
                return Err("the embedding model returned an empty vector".into());
            }
            out.push(e);
        }
        Ok(out)
    }

    pub async fn search(&self, hub: &Hub, collection: &str, query: &str, k: usize) -> Result<Vec<rag::Hit>, String> {
        let qv = match self.embed_texts(hub, &[query.to_string()]).await {
            Ok(Some((m, mut v))) => Some((m, v.remove(0))),
            _ => None,
        };
        self.rag.search(
            collection,
            query,
            k,
            qv.as_ref().map(|(m, v)| (m.as_str(), v.as_slice())),
        )
    }

    /// Gives every chunk of `collection` that lacks one a vector, when an embedding model is installed. Returns how
    /// many it embedded and, if it stopped early, why. The chunks without a vector stay searchable by keyword, and the
    /// next call carries on where this one stopped.
    pub async fn embed_pending(&self, hub: &Hub, collection: &str) -> (usize, Option<String>) {
        let res = self.resolutions(hub).await;
        let Some(model) = res
            .iter()
            .find(|r| r.role == Role::Embed && r.status == "ready")
            .and_then(|r| r.model.clone())
        else {
            return (0, None);
        };
        let mut done = 0;
        loop {
            let todo = match self.rag.unembedded(collection, &model, 64) {
                Ok(t) => t,
                Err(e) => return (done, Some(e)),
            };
            if todo.is_empty() {
                return (done, None);
            }
            let texts: Vec<String> = todo.iter().map(|(_, t)| t.chars().take(2000).collect()).collect();
            let (m, vecs) = match self.embed_texts(hub, &texts).await {
                Ok(Some(x)) => x,
                Ok(None) => return (done, None),
                Err(e) => return (done, Some(e)),
            };
            let items: Vec<(i64, Vec<f32>)> = todo.iter().map(|(id, _)| *id).zip(vecs).collect();
            if let Err(e) = self.rag.set_vectors(&m, &items) {
                return (done, Some(e));
            }
            done += items.len();
        }
    }

    pub async fn ingest_path(&self, hub: &Hub, collection: &str, path: &Path) -> Result<Value, String> {
        let report = self.rag.ingest_path(collection, path)?;
        let (embedded, embed_error) = self.embed_pending(hub, collection).await;
        let mut v = serde_json::to_value(&report).map_err(|e| e.to_string())?;
        v.as_object_mut().map(|o| o.remove("new_chunks"));
        v["collection"] = json!(collection);
        v["embedded"] = json!(embedded);
        if let Some(e) = embed_error {
            v["embed_error"] = json!(e);
        }
        Ok(v)
    }

    pub async fn ingest_text(&self, hub: &Hub, collection: &str, uri: &str, text: &str) -> Result<Value, String> {
        let r = self.rag.ingest_text(collection, uri, text)?;
        let (embedded, embed_error) = self.embed_pending(hub, collection).await;
        let mut v = json!({"collection": collection, "source": r.source, "status": r.status, "chunks": r.chunks, "embedded": embedded});
        if let Some(e) = embed_error {
            v["embed_error"] = json!(e);
        }
        Ok(v)
    }
}

/// How many times a batch that does not fit the embedder is cut in half before giving up.
const MAX_EMBED_CUTS: usize = 4;

/// Does an embedder's error say the input did not fit (llama-server: "input (541 tokens) is too large to process")?
fn input_too_large(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("too large") || m.contains("physical batch") || m.contains("too many tokens")
}

/// Every text cut to at most `max` characters (on a character boundary).
fn clip_all(texts: &[String], max: usize) -> Vec<String> {
    texts.iter().map(|t| t.chars().take(max).collect()).collect()
}

/// `version: 11272 (7fee178)` to 11272.
pub fn parse_build(v: &str) -> Option<u32> {
    let rest = v.trim().strip_prefix("version:").unwrap_or(v).trim();
    let digits: String = rest
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_build_number_is_read_from_the_version_line() {
        assert_eq!(parse_build("version: 11272 (7fee178)"), Some(11272));
        assert_eq!(parse_build("version: b11556"), Some(11556));
        assert_eq!(parse_build("unknown"), None);
        const { assert!(11272 < STOCK_Q2_BUILD && 11556 >= STOCK_Q2_BUILD) };
    }

    #[test]
    fn an_embedder_size_error_is_recognised_and_a_text_is_cut_on_a_character_boundary() {
        assert!(input_too_large(
            "nomic-embed-text:latest: input (541 tokens) is too large to process. increase the physical batch size \
             (current batch size: 512)"
        ));
        assert!(!input_too_large(
            "nomic-embed-text:latest did not answer: connection refused"
        ));
        let cut = clip_all(&["héllo wörld".to_string(), "ab".to_string()], 4);
        assert_eq!(cut, ["héll", "ab"]);
    }

    /// A stand-in embedder that refuses an input over 100 characters the way llama-server does, else answers
    /// `[length, 1]`. Returns its port and the server task.
    async fn small_batch_embedder() -> (u16, tokio::task::JoinHandle<()>) {
        use axum::http::StatusCode;
        let app = axum::Router::new().route(
            "/v1/embeddings",
            axum::routing::post(|axum::Json(req): axum::Json<Value>| async move {
                let inputs = req["input"].as_array().cloned().unwrap_or_default();
                let len = |t: &Value| t.as_str().map(|s| s.chars().count()).unwrap_or(0);
                if inputs.iter().any(|t| len(t) > 100) {
                    let msg = "input (541 tokens) is too large to process. increase the physical batch size (current \
                               batch size: 512)";
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        axum::Json(json!({"error": {"message": msg}})),
                    );
                }
                let data: Vec<Value> = inputs
                    .iter()
                    .enumerate()
                    .map(|(i, t)| json!({"object": "embedding", "index": i, "embedding": [len(t) as f32, 1.0]}))
                    .collect();
                (StatusCode::OK, axum::Json(json!({"object": "list", "data": data})))
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        (
            port,
            tokio::spawn(async move {
                let _ = axum::serve(l, app).await;
            }),
        )
    }

    #[tokio::test]
    async fn a_passage_the_embedder_cannot_fit_is_cut_until_it_can_and_a_failure_is_reported_not_hidden() {
        let dir = crate::testing::TestDir::new("misty-embed-cut");
        let hub = crate::testing::hub(&dir, &["stub-embed"]);
        let misty = Misty::open_with(&dir, Arc::new(cactus::fake::Fake::with_models(0))).unwrap();
        let (port, server) = small_batch_embedder().await;
        let abort = server.abort_handle();
        hub.engine
            .adopt_as("stub-embed", port, "k", true, Some(Box::new(move || abort.abort())));
        let long = "x".repeat(900);
        let r = misty.embed_texts(&hub, &[long, "short".to_string()]).await;
        let (model, vecs) = r
            .expect("the long passage is cut and embedded")
            .expect("an embedder is installed");
        assert_eq!(model, "stub-embed");
        assert_eq!(vecs.len(), 2);
        assert!(vecs[0][0] <= 100.0 && vecs[0][0] > 0.0, "cut to fit: {:?}", vecs[0]);
        assert_eq!(vecs[1][0], 5.0, "a short text is untouched");

        // the index route: chunks get vectors, and a failing embedder shows up in the report
        let report = misty
            .ingest_text(&hub, "c", "note://a", &"word ".repeat(300))
            .await
            .unwrap();
        assert!(report["embedded"].as_u64().unwrap() >= 1, "{report}");
        assert!(report.get("embed_error").is_none(), "{report}");
        server.abort();
        hub.engine.unload("stub-embed");
        let report = misty
            .ingest_text(&hub, "c", "note://b", &"other ".repeat(300))
            .await
            .unwrap();
        assert_eq!(report["embedded"], 0, "{report}");
        assert!(report["embed_error"].is_string(), "the reason is reported: {report}");
    }

    #[tokio::test]
    async fn a_ternary_file_is_refused_by_the_load_path_until_the_prism_engine_is_installed() {
        let dir = crate::testing::TestDir::new("misty-guard");
        let hub = crate::testing::hub(&dir, &["Ternary-Bonsai-2-9B-PQ2_0", "plain-model"]);
        let body = json!({"model": "Ternary-Bonsai-2-9B-PQ2_0", "messages": [{"role": "user", "content": "hi"}]});
        let e = hub
            .engine
            .forward("/v1/chat/completions", body.clone())
            .await
            .err()
            .expect("the load is refused");
        assert_eq!(e.code, "engine_cannot_run", "{}", e.message);
        assert!(e.message.contains("misty setup --install prism"), "{}", e.message);
        // an ordinary model is not held back by the guard (it fails later, for want of a llama-server)
        let e = hub
            .engine
            .forward("/v1/chat/completions", json!({"model": "plain-model", "messages": []}))
            .await
            .err()
            .unwrap();
        assert_ne!(e.code, "engine_cannot_run");
    }

    #[test]
    fn a_ternary_file_is_served_by_the_prism_engine_once_it_is_installed() {
        let dir = crate::testing::TestDir::new("misty-prism");
        let hub = crate::testing::hub(&dir, &["Ternary-Bonsai-2-9B-PQ2_0", "plain-model"]);
        let entry = |id: &str| hub.engine.catalog().into_iter().find(|m| m.id == id).unwrap();
        assert_eq!(
            hub.engine.serving_preview(&entry("Ternary-Bonsai-2-9B-PQ2_0")).engine,
            "llama.cpp"
        );
        let folder = dir.join("engines").join("prism");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join(if cfg!(windows) {
                "llama-server.exe"
            } else {
                "llama-server"
            }),
            b"x",
        )
        .unwrap();
        assert_eq!(
            hub.engine.serving_preview(&entry("Ternary-Bonsai-2-9B-PQ2_0")).engine,
            "prism"
        );
        assert_eq!(hub.engine.serving_preview(&entry("plain-model")).engine, "llama.cpp");
    }
}

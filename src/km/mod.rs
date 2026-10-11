//! Knowledge Modules: the runtime for `docs/native/km-spec.md` (`mmkm/1`).
//!
//! * [`manifest`] parses and validates `km.toml`; [`pack`] packs, unpacks and verifies `.mmkm` archives and checks
//!   signatures; [`store`] keeps the installed KMs and their SQLite index (FTS5, tags with origin and confidence,
//!   atomic channels, pins and bans); [`vocab`] is the controlled tag vocabulary.
//! * [`select`] ranks KMs for a request with every score term recorded; [`embed`] is where its vectors come from.
//! * [`residency`] decides what is in RAM and VRAM within budgets and records every decision as an event;
//!   [`resolve`] works out `requires`, `conflicts` and bundles.
//! * [`memory`] and [`data`] are what memory and data KMs do; [`ops`] makes all of it hub operations and REST routes;
//!   [`cli`] is `model-mistress km ...`; [`sample`] builds tiny KMs of every kind for tests.
//!
//! [`Km`] ties them together for one home folder. Nothing in a KM is ever executed or opened as an address.

pub mod cli;
pub mod data;
pub mod embed;
pub mod manifest;
pub mod memory;
pub mod ops;
pub mod pack;
pub mod residency;
pub mod resolve;
pub mod sample;
pub mod select;
pub mod specialist;
pub mod store;
pub mod vocab;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::engine::{Engine, Fail};
use embed::{Embedder, HashEmbedder, HubEmbedder};
use manifest::{Kind, Manifest};
use pack::{SigState, Source};
use residency::{AttachSpec, Attacher, Budgets, Candidate, EngineAttacher, Guard, Manager, SystemClock, Tier};
use select::{Budget, Query, QueryVec, Weights};
use store::{Record, Store, Trust};

pub type Res<T> = Result<T, Fail>;

pub fn sha256_hex(b: &[u8]) -> String {
    pack::hex(&Sha256::digest(b))
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---- settings (`[km]` in config.toml) ------------------------------------------------------------------------------

/// A core's own budgets; 0 = no limit beyond the global one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CoreBudget {
    pub ram_bytes: u64,
    pub vram_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KmSettings {
    /// Public keys (hex) whose signatures are trusted.
    pub trusted_keys: Vec<String>,
    /// Where selection and memory queries get vectors: `hash` (built in, offline) or `hub` (the embedding model named
    /// in `embed_model`, served by the hub).
    pub embedder: String,
    /// The hub model that embeds (embedder = "hub"); it is also the id KMs' `routing.embed_model` must equal.
    pub embed_model: Option<String>,
    /// Byte budgets: disk for the whole store, RAM and VRAM for what is resident. 0 = no limit.
    pub budget_disk_bytes: u64,
    pub budget_ram_bytes: u64,
    pub budget_vram_bytes: u64,
    /// Budgets per core, by core name (`dusty`, `misty`, or a hub model id).
    pub core_budgets: BTreeMap<String, CoreBudget>,
    /// Selection weights.
    pub w_vec: f64,
    pub w_lex: f64,
    pub w_route: f64,
    pub w_tag: f64,
    pub pin_bonus: f64,
    /// Half-life of the use count that eviction ranks by.
    pub heat_half_life_s: u64,
    /// The core adapters attach to when a load names none.
    pub default_core: Option<String>,
    /// Refuse KMs with more payload than this (0 = the per-kind limits only).
    pub max_km_bytes: u64,
}

impl Default for KmSettings {
    fn default() -> Self {
        Self {
            trusted_keys: vec![],
            embedder: "hash".into(),
            embed_model: None,
            budget_disk_bytes: 0,
            budget_ram_bytes: 4 << 30,
            budget_vram_bytes: 2 << 30,
            core_budgets: BTreeMap::new(),
            w_vec: 1.0,
            w_lex: 0.25,
            w_route: 1.0,
            w_tag: 0.5,
            pin_bonus: 2.0,
            heat_half_life_s: 600,
            default_core: None,
            max_km_bytes: 0,
        }
    }
}

// ---- the runtime -------------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct LoadReq {
    pub id: String,
    pub version: Option<String>,
    pub channel: Option<String>,
    pub tier: Option<Tier>,
    pub core: Option<String>,
    pub scale: f32,
    /// Load a KM marked `misdeclared` anyway (the owner knows better).
    pub force: bool,
    pub no_evict: bool,
    pub protect: HashSet<String>,
}

impl LoadReq {
    pub fn new(id: &str) -> Self {
        Self {
            id: id.into(),
            version: None,
            channel: None,
            tier: None,
            core: None,
            scale: 1.0,
            force: false,
            no_evict: false,
            protect: HashSet::new(),
        }
    }
}

/// `id`, `id@version` or `id:channel` as one string.
pub fn split_ref(s: &str) -> (String, Option<String>, Option<String>) {
    if let Some((id, v)) = s.split_once('@') {
        (id.to_string(), Some(v.to_string()), None)
    } else if let Some((id, c)) = s.split_once(':') {
        (id.to_string(), None, Some(c.to_string()))
    } else {
        (s.to_string(), None, None)
    }
}

pub struct Km {
    pub engine: Arc<Engine>,
    pub store: Store,
    pub data: data::DataStore,
    pub manager: Arc<Manager>,
    embedder: Mutex<Option<Arc<dyn Embedder>>>,
    attacher: Mutex<Option<Arc<dyn Attacher>>>,
    backend: Mutex<Option<Arc<dyn specialist::Backend>>>,
    /// Content ids whose files were verified in this process (km-spec.md section 9).
    verified: Mutex<HashSet<String>>,
    ready: tokio::sync::OnceCell<()>,
}

static RUNTIMES: OnceLock<Mutex<HashMap<PathBuf, Arc<Km>>>> = OnceLock::new();

/// The KM runtime of an engine's home folder (made on first use).
pub fn runtime(engine: &Arc<Engine>) -> Res<Arc<Km>> {
    let map = RUNTIMES.get_or_init(Default::default);
    let mut m = map.lock();
    if let Some(k) = m.get(&engine.home) {
        return Ok(k.clone());
    }
    let k = Km::open(engine.clone())?;
    m.insert(engine.home.clone(), k.clone());
    Ok(k)
}

impl Km {
    pub fn open(engine: Arc<Engine>) -> Res<Arc<Km>> {
        let store = Store::open(&engine.home)?;
        let data = data::DataStore::open(&engine.home)?;
        let km = Arc::new(Km {
            engine,
            store,
            data,
            manager: Manager::new(Arc::new(SystemClock)),
            embedder: Mutex::new(None),
            attacher: Mutex::new(None),
            backend: Mutex::new(None),
            verified: Mutex::new(HashSet::new()),
            ready: tokio::sync::OnceCell::new(),
        });
        km.sync();
        Ok(km)
    }

    pub fn settings(&self) -> KmSettings {
        self.engine.settings().km
    }

    /// Applies the current settings and the owner's pins to the residency manager.
    pub fn sync(&self) {
        let s = self.settings();
        self.manager.configure(
            Budgets {
                ram: s.budget_ram_bytes,
                vram: s.budget_vram_bytes,
                core: s
                    .core_budgets
                    .iter()
                    .map(|(c, b)| (c.clone(), (b.ram_bytes, b.vram_bytes)))
                    .collect(),
            },
            s.heat_half_life_s,
        );
        self.manager.set_pinned_ids(
            self.store
                .owner_all()
                .into_iter()
                .filter(|(_, o)| o.pinned)
                .map(|(id, _)| id)
                .collect(),
        );
    }

    pub fn embedder(&self) -> Arc<dyn Embedder> {
        if let Some(e) = self.embedder.lock().clone() {
            return e;
        }
        let s = self.settings();
        match (s.embedder.as_str(), s.embed_model) {
            ("hub", Some(model)) if !model.is_empty() => Arc::new(HubEmbedder {
                engine: self.engine.clone(),
                model,
            }),
            _ => Arc::new(HashEmbedder::default()),
        }
    }

    pub fn set_embedder(&self, e: Option<Arc<dyn Embedder>>) {
        *self.embedder.lock() = e;
    }

    pub fn attacher(&self) -> Arc<dyn Attacher> {
        self.attacher
            .lock()
            .clone()
            .unwrap_or_else(|| Arc::new(EngineAttacher(self.engine.clone())))
    }

    pub fn set_attacher(&self, a: Option<Arc<dyn Attacher>>) {
        *self.attacher.lock() = a;
    }

    pub fn vocabulary(&self) -> Res<vocab::Vocabulary> {
        vocab::Vocabulary::load(&self.store.root)
    }

    // ---- attached adapters survive a restart as engine settings, so they are recorded and cleaned up --------------

    fn attached_file(&self) -> PathBuf {
        self.store.root.join("attached.json")
    }

    fn write_attached(&self) {
        let v: Vec<Value> = self
            .manager
            .attached()
            .into_iter()
            .map(|(c, p)| json!({"core": c, "path": p}))
            .collect();
        let _ = std::fs::write(self.attached_file(), serde_json::to_vec(&v).unwrap_or_default());
    }

    /// Adapters attached by an earlier run are no longer tracked (residency starts empty), so detach them once.
    async fn ready(&self) {
        self.ready
            .get_or_init(|| async {
                let stale: Vec<Value> = std::fs::read(self.attached_file())
                    .ok()
                    .and_then(|b| serde_json::from_slice(&b).ok())
                    .unwrap_or_default();
                let a = self.attacher();
                for s in stale {
                    if let (Some(core), Some(path)) = (s["core"].as_str(), s["path"].as_str()) {
                        let r = a.detach(core, Path::new(path)).await;
                        self.manager.events().emit(
                            "detach",
                            path,
                            None,
                            0,
                            "detached an adapter left attached by an earlier run",
                            json!({"core": core, "ok": r.is_ok()}),
                        );
                    }
                }
                let _ = std::fs::remove_file(self.attached_file());
            })
            .await;
    }

    // ---- import / export ---------------------------------------------------------------------------------------

    /// Installs a KM from a folder or a `.mmkm` file. Blocking (it hashes the payload).
    pub fn import_sync(&self, path: &str, source: Source, allow_unsigned: bool, channel: Option<&str>) -> Res<Value> {
        let st = self.settings();
        let p = Path::new(path);
        let (stage, m) = if p.is_file() {
            pack::unpack(
                p,
                &self.store.tmp_dir(),
                (st.max_km_bytes > 0).then_some(st.max_km_bytes),
            )?
        } else if p.is_dir() {
            let m = Manifest::from_file(&p.join("km.toml"))?;
            let stage = self.store.stage_dir(p, &m)?;
            (stage, m)
        } else {
            return Err(Fail::not_found(format!("{path}: no such file or folder")));
        };
        if st.max_km_bytes > 0 && m.total_bytes() > st.max_km_bytes {
            let _ = std::fs::remove_dir_all(&stage);
            return Err(Fail::invalid(format!(
                "{} has {} payload bytes; km.max_km_bytes is {}",
                m.key(),
                m.total_bytes(),
                st.max_km_bytes
            )));
        }
        let sig = pack::check_signature(&stage.join(m.key()), &st.trusted_keys);
        let overridden = match pack::policy(source, &sig, allow_unsigned) {
            Ok(o) => o,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&stage);
                self.manager.events().emit(
                    "refuse",
                    &m.key(),
                    None,
                    0,
                    &format!("import refused: {}", e.message),
                    json!({}),
                );
                return Err(e);
            }
        };
        let trust = Trust {
            source,
            sig: sig.clone(),
            overridden,
        };
        let (rec, already) = self.store.install(&stage, &m, &trust, st.budget_disk_bytes)?;
        if let Some(c) = channel {
            self.store.channel_set(&m.id, c, &m.version.to_string(), None)?;
        }
        let vocab = self.vocabulary()?;
        Ok(
            json!({"key": rec.key, "content_id": rec.content_id, "already_installed": already,
                  "kind": rec.kind, "bytes": rec.bytes, "signature": sig.to_json(), "source": source.as_str(),
                  "unsigned_override": overridden, "channel": channel, "warnings": m.warnings,
                  "unknown_keys": m.unknown, "unapproved_tags": vocab.unapproved(&m.tags)}),
        )
    }

    pub fn export_sync(
        &self,
        id: &str,
        version: Option<&str>,
        channel: Option<&str>,
        path: Option<&str>,
    ) -> Res<Value> {
        let rec = self.store.resolve(id, version, channel)?;
        let out = match path {
            Some(p) => PathBuf::from(p),
            None => self.store.root.join("export").join(format!("{}.mmkm", rec.key)),
        };
        let (m, bytes) = pack::pack_dir(&self.store.dir_of(&rec.key), &out)?;
        Ok(json!({"key": rec.key, "path": out, "bytes": bytes, "content_id": m.content_id}))
    }

    // ---- loading -----------------------------------------------------------------------------------------------

    fn resident_records(&self) -> Vec<Record> {
        self.manager
            .resident_keys()
            .iter()
            .filter_map(|k| self.store.get(k).ok().flatten())
            .collect()
    }

    fn refuse(&self, key: &str, reason: String) -> Fail {
        self.manager.events().emit("refuse", key, None, 0, &reason, json!({}));
        Fail::new(409, "refused", format!("{key} was not loaded: {reason}"))
    }

    /// Verifies a KM and works out how it would be held. Blocking.
    fn prepare(&self, rec: &Record, req: &LoadReq, root: bool) -> Res<Candidate> {
        let st = self.settings();
        let dir = self.store.dir_of(&rec.key);
        let m = Manifest::from_file(&dir.join("km.toml"))?;
        if m.content_id != rec.content_id {
            return Err(self.refuse(
                &rec.key,
                "km.toml no longer matches the index (the store was changed by hand)".into(),
            ));
        }
        // verify: hashes once per process per content id, the signature policy every time
        let t0 = std::time::Instant::now();
        let cached = self.verified.lock().contains(&rec.content_id);
        if !cached {
            if let Err(e) = pack::verify_dir(&dir, &m) {
                self.manager.events().emit(
                    "verify",
                    &rec.key,
                    None,
                    0,
                    &format!("failed: {}", e.message),
                    json!({"ok": false}),
                );
                return Err(self.refuse(&rec.key, format!("verification failed: {}", e.message)));
            }
            self.verified.lock().insert(rec.content_id.clone());
        }
        let sig = pack::check_signature(&dir, &st.trusted_keys);
        let source = Source::parse(&rec.source).unwrap_or(Source::Download);
        if let Err(e) = pack::policy(source, &sig, rec.overridden) {
            return Err(self.refuse(&rec.key, format!("trust: {}", e.message)));
        }
        self.manager.events().emit(
            "verify",
            &rec.key,
            None,
            m.total_bytes(),
            "ok",
            json!({"ok": true, "ms": t0.elapsed().as_millis() as u64, "cached_hashes": cached, "signature": sig.label()}),
        );
        // measured against declared; a KM already marked misdeclared is refused without measuring again
        let measured = residency::measured(&rec.kind, m.total_bytes());
        match &rec.misdeclared {
            Some(why) if !req.force => {
                return Err(self.refuse(
                    &rec.key,
                    format!("marked misdeclared ({why}); load it with force to override"),
                ));
            }
            Some(_) => {}
            None => {
                if let Some(why) = residency::misdeclared(rec, measured) {
                    let _ = self.store.set_misdeclared(&rec.key, Some(&why));
                    self.manager
                        .events()
                        .emit("misdeclared", &rec.key, None, 0, &why, json!({}));
                    return Err(self.refuse(&rec.key, format!("misdeclared: {why}")));
                }
            }
        }
        // a tool KM only refers to something the host provides: check it is there
        if let Some(t) = &m.tool {
            if t.provider == "hub" && !crate::hub::operations().iter().any(|o| o["id"] == t.name.as_str()) {
                return Err(self.refuse(&rec.key, format!("tool {} is not a hub operation", t.name)));
            }
        }
        let kind = Kind::parse(&rec.kind).unwrap_or(Kind::Data);
        let gpu = matches!(kind, Kind::Adapter | Kind::Experts | Kind::Projector | Kind::Router);
        let core = req.core.clone().or_else(|| st.default_core.clone());
        let want = if root { req.tier } else { None };
        let tier = match (kind, want) {
            (Kind::Adapter, None) => {
                if core.is_some() {
                    Tier::Vram
                } else {
                    Tier::Ram
                }
            }
            (_, Some(Tier::Vram)) if gpu => Tier::Vram,
            _ => Tier::Ram,
        };
        let attach = match (kind, tier) {
            (Kind::Adapter, Tier::Vram) => {
                let lora = m.payload_of("lora").or(m.payload.first());
                lora.map(|p| AttachSpec {
                    path: std::path::absolute(dir.join(&p.path)).unwrap_or_else(|_| dir.join(&p.path)),
                    scale: req.scale,
                })
            }
            _ => None,
        };
        let attach_state = match (kind, tier) {
            (Kind::Adapter, Tier::Vram) => "attached",
            (Kind::Adapter, _) => "staged",
            (Kind::Specialist, _) => "attached",
            (Kind::Experts | Kind::Projector | Kind::Router | Kind::Tool, _) => "pending",
            (Kind::Memory, _) => "on-demand",
            _ => "n/a",
        };
        let data: Option<Arc<dyn std::any::Any + Send + Sync>> = if kind == Kind::Memory {
            Some(Arc::new(memory::MemIndex::load(&dir, &m)?))
        } else {
            None
        };
        Ok(Candidate {
            key: rec.key.clone(),
            id: rec.id.clone(),
            kind: rec.kind.clone(),
            tier,
            core: if tier == Tier::Vram || req.core.is_some() {
                core
            } else {
                None
            },
            measured,
            attach,
            attach_state,
            data,
            no_evict: req.no_evict,
        })
    }

    /// Loads a KM and what it requires. All-or-nothing as far as residency goes: if a later member fails, the ones
    /// this call loaded are unloaded again (evictions already made to fit them are not undone).
    pub async fn load(self: &Arc<Self>, req: LoadReq) -> Res<Value> {
        self.ready().await;
        self.sync();
        let rec = self
            .store
            .resolve(&req.id, req.version.as_deref(), req.channel.as_deref())?;
        if self.store.owner(&rec.id).banned {
            return Err(self.refuse(&rec.key, "banned by the owner".into()));
        }
        let set =
            resolve::closure(&self.store, std::slice::from_ref(&rec), &self.resident_records()).inspect_err(|e| {
                self.manager
                    .events()
                    .emit("refuse", &rec.key, None, 0, &e.message, json!({"code": e.code}));
            })?;
        let mut protect: HashSet<String> = set.iter().map(|r| r.key.clone()).collect();
        protect.extend(req.protect.iter().cloned());
        let attacher = self.attacher();
        let (mut out, mut fresh): (Vec<Value>, Vec<String>) = (vec![], vec![]);
        for r in &set {
            let root = r.key == rec.key;
            let cand = {
                let me = self.clone();
                let (r, req) = (r.clone(), req.clone());
                tokio::task::spawn_blocking(move || me.prepare(&r, &req, root))
                    .await
                    .map_err(|e| Fail::new(500, "internal", e.to_string()))?
            };
            let cand = match cand {
                Ok(c) => c,
                Err(e) => {
                    self.rollback(&fresh, &r.key, &e.message).await;
                    return Err(e);
                }
            };
            let (tier, bytes, state) = (cand.tier, cand.measured, cand.attach_state);
            match self.manager.admit(cand, &*attacher, &protect).await {
                Ok(a) => {
                    if !a.already {
                        fresh.push(r.key.clone());
                    }
                    // another version of the same KM is replaced
                    for other in self.manager.resident_keys() {
                        if other != r.key && other.split('@').next() == Some(r.id.as_str()) {
                            let _ = self
                                .manager
                                .unload(&other, &*attacher, &format!("replaced by {}", r.key))
                                .await;
                        }
                    }
                    out.push(
                        json!({"key": r.key, "tier": tier.as_str(), "ram_bytes": bytes.0, "vram_bytes": bytes.1,
                                    "already_resident": a.already, "evicted": a.evicted, "attach": state}),
                    );
                }
                Err(e) => {
                    self.rollback(&fresh, &r.key, &e.message).await;
                    return Err(e);
                }
            }
        }
        self.write_attached();
        Ok(json!({"requested": rec.key, "loaded": out}))
    }

    async fn rollback(&self, fresh: &[String], failed: &str, why: &str) {
        let a = self.attacher();
        for k in fresh.iter().rev() {
            let _ = self
                .manager
                .unload(k, &*a, &format!("rolled back: {failed} failed ({why})"))
                .await;
        }
        self.write_attached();
    }

    pub async fn unload(self: &Arc<Self>, id: &str, version: Option<&str>, all: bool) -> Res<Value> {
        self.ready().await;
        let a = self.attacher();
        let mut keys: Vec<String> = self
            .manager
            .resident_keys()
            .into_iter()
            .filter(|k| {
                all || (k.split('@').next() == Some(id) && version.is_none_or(|v| k.ends_with(&format!("@{v}"))))
            })
            .collect();
        // a bundle takes its members with it
        for k in keys.clone() {
            if let Ok(Some(rec)) = self.store.get(&k) {
                if rec.kind == "bundle" {
                    if let Ok(m) = Manifest::from_file(&self.store.dir_of(&k).join("km.toml")) {
                        for member in m.members {
                            if let Ok((mid, req)) = manifest::parse_ref(&member) {
                                keys.extend(self.manager.resident_keys().into_iter().filter(|x| {
                                    x.split('@').next() == Some(mid.as_str())
                                        && x.split('@')
                                            .nth(1)
                                            .and_then(|v| Version::parse(v).ok())
                                            .is_some_and(|v| req.matches(&v))
                                }));
                            }
                        }
                    }
                }
            }
        }
        keys.sort();
        keys.dedup();
        let mut done = vec![];
        for k in keys {
            if self.manager.unload(&k, &*a, "unloaded by the owner").await? {
                done.push(k);
            }
        }
        self.write_attached();
        Ok(json!({"unloaded": done}))
    }

    /// Loads the KMs of one request and protects the whole set from eviction until the guard is dropped.
    pub async fn open_request(self: &Arc<Self>, refs: &[LoadReq]) -> Res<Guard> {
        let mut protect: HashSet<String> = HashSet::new();
        for r in refs {
            protect.insert(
                self.store
                    .resolve(&r.id, r.version.as_deref(), r.channel.as_deref())?
                    .key,
            );
        }
        let mut guard = self.manager.guard(&[]);
        for r in refs {
            let mut r = r.clone();
            r.protect = protect.clone();
            let out = self.load(r).await?;
            for l in out["loaded"].as_array().into_iter().flatten() {
                if let Some(k) = l["key"].as_str() {
                    guard.add(k);
                    self.manager.touch(k);
                }
            }
        }
        Ok(guard)
    }

    /// Warms KMs into RAM where there is room, without evicting anything.
    pub async fn prefetch(self: &Arc<Self>, keys: &[String]) -> Res<Value> {
        self.sync();
        let mut out = vec![];
        for k in keys {
            let (id, v, c) = split_ref(k);
            let rec = match self.store.resolve(&id, v.as_deref(), c.as_deref()) {
                Ok(r) => r,
                Err(e) => {
                    out.push(json!({"key": k, "result": "skipped", "reason": e.message}));
                    continue;
                }
            };
            if self.manager.is_resident(&rec.key) {
                out.push(json!({"key": rec.key, "result": "already resident"}));
                continue;
            }
            let (ram, _) = residency::declared(&rec);
            if rec.misdeclared.is_some() || self.store.owner(&rec.id).banned {
                out.push(json!({"key": rec.key, "result": "skipped", "reason": "misdeclared or banned"}));
                continue;
            }
            if !self.manager.has_room(Tier::Ram, None, ram) {
                self.manager.events().emit(
                    "prefetch",
                    &rec.key,
                    Some(Tier::Ram),
                    ram,
                    "skipped: no room in RAM",
                    json!({}),
                );
                out.push(json!({"key": rec.key, "result": "skipped", "reason": "no room in RAM"}));
                continue;
            }
            let mut req = LoadReq::new(&rec.id);
            req.version = Some(rec.version.clone());
            req.tier = Some(Tier::Ram);
            req.no_evict = true;
            match self.load(req).await {
                Ok(_) => {
                    self.manager.events().emit(
                        "prefetch",
                        &rec.key,
                        Some(Tier::Ram),
                        ram,
                        "warmed into RAM",
                        json!({}),
                    );
                    out.push(json!({"key": rec.key, "result": "warmed"}));
                }
                Err(e) => out.push(json!({"key": rec.key, "result": "skipped", "reason": e.message})),
            }
        }
        Ok(json!(out))
    }

    // ---- selection ---------------------------------------------------------------------------------------------

    pub fn weights(&self) -> Weights {
        let s = self.settings();
        Weights {
            vec: s.w_vec,
            lex: s.w_lex,
            route: s.w_route,
            pin_bonus: s.pin_bonus,
            tag: s.w_tag,
        }
    }

    /// The biggest single KM that could ever be resident for a core.
    pub fn budget_for(&self, core: Option<&str>) -> Budget {
        let s = self.settings();
        let c = core.and_then(|c| s.core_budgets.get(c));
        let min = |g: u64, c: u64| match (g, c) {
            (0, x) | (x, 0) => x,
            (a, b) => a.min(b),
        };
        Budget {
            ram: min(s.budget_ram_bytes, c.map(|c| c.ram_bytes).unwrap_or(0)),
            vram: min(s.budget_vram_bytes, c.map(|c| c.vram_bytes).unwrap_or(0)),
        }
    }

    pub async fn select(&self, q: &Query, core: Option<&str>) -> Res<select::Selection> {
        let free = select::parse_structured(&q.text).text;
        let qvec = if free.trim().is_empty() {
            Ok(None)
        } else {
            let e = self.embedder();
            match e.embed(std::slice::from_ref(&free)).await {
                Ok(mut v) => Ok(v.pop().map(|vector| QueryVec {
                    embedder: e.model_id(),
                    vector,
                })),
                Err(f) => Err(f.message),
            }
        };
        select::rank(
            &self.store,
            q,
            &self.weights(),
            self.budget_for(core.or(q.base.as_deref())),
            qvec,
        )
    }

    // ---- memory ------------------------------------------------------------------------------------------------

    pub async fn memory_query(
        self: &Arc<Self>,
        req: LoadReq,
        text: Option<&str>,
        vector: Option<Vec<f32>>,
        top_k: usize,
        min_score: f32,
    ) -> Res<Value> {
        let rec = self
            .store
            .resolve(&req.id, req.version.as_deref(), req.channel.as_deref())?;
        if rec.kind != "memory" {
            return Err(Fail::invalid(format!(
                "{} is a {} KM, not a memory KM",
                rec.key, rec.kind
            )));
        }
        let mut r = req.clone();
        r.version = Some(rec.version.clone());
        r.channel = None;
        r.tier = Some(Tier::Ram);
        let _hold = {
            let mut g = self.manager.guard(&[]);
            let out = self.load(r).await?;
            for l in out["loaded"].as_array().into_iter().flatten() {
                if let Some(k) = l["key"].as_str() {
                    g.add(k);
                }
            }
            g
        };
        self.manager.touch(&rec.key);
        let idx = self
            .manager
            .data_of(&rec.key)
            .and_then(|d| d.downcast::<memory::MemIndex>().ok())
            .ok_or_else(|| Fail::new(500, "internal", format!("{} is resident without its index", rec.key)))?;
        let (q, embedder) = match (vector, text) {
            (Some(v), _) => (v, "caller".to_string()),
            (None, Some(t)) => {
                let e = self.embedder();
                if e.model_id() != idx.embed_model {
                    return Err(Fail::new(
                        409,
                        "embed_model_mismatch",
                        format!(
                            "{} was embedded with {}, the selector embeds with {}; vectors are not comparable (give a \
                             vector made with {}, or set km.embedder)",
                            rec.key,
                            idx.embed_model,
                            e.model_id(),
                            idx.embed_model
                        ),
                    ));
                }
                let v = e.embed(&[t.to_string()]).await?.pop().unwrap_or_default();
                (v, e.model_id())
            }
            _ => return Err(Fail::invalid("give text or vector")),
        };
        let hits = idx.query(&q, top_k.clamp(1, 100), min_score)?;
        Ok(
            json!({"km": rec.key, "embedder": embedder, "embed_model": idx.embed_model,
                  "index": if idx.index == "hnsw" { "flat (hnsw asked for; this build scans exactly)" } else { "flat" },
                  "chunks": idx.chunks.len(),
                  "hits": hits.iter().map(|h| h.to_json(&rec.key)).collect::<Vec<_>>()}),
        )
    }

    // ---- data --------------------------------------------------------------------------------------------------

    /// The data KM an operation is about; its working copy is opened if it has none.
    pub fn data_target(&self, id: &str, version: Option<&str>, channel: Option<&str>) -> Res<Record> {
        let rec = match self.data.work(id) {
            Some((base, _)) if version.is_none() && channel.is_none() => self
                .store
                .get(&format!("{id}@{base}"))?
                .ok_or_else(|| Fail::not_found(format!("{id}@{base} is not installed")))?,
            _ => self.store.resolve(id, version, channel)?,
        };
        if rec.kind != "data" {
            return Err(Fail::invalid(format!(
                "{} is a {} KM, not a data KM",
                rec.key, rec.kind
            )));
        }
        let m = Manifest::from_file(&self.store.dir_of(&rec.key).join("km.toml"))?;
        let records = m
            .payload_of("records")
            .ok_or_else(|| Fail::invalid(format!("{} has no records payload", rec.key)))?;
        self.data
            .ensure_seeded(&rec.id, &rec.version, &self.store.dir_of(&rec.key).join(&records.path))?;
        Ok(rec)
    }

    pub fn data_trust() -> Trust {
        Trust {
            source: Source::Own,
            sig: SigState::Unsigned,
            overridden: false,
        }
    }
}

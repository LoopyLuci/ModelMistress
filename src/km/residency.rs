//! Residency (km-spec.md §7): which KMs are in RAM or VRAM, within byte budgets.
//!
//! Tiers are disk (everything installed), RAM (memory-mapped, page cache) and VRAM (attached to a running core).
//! A resident KM occupies one tier: RAM-tier KMs count their measured RAM bytes, VRAM-tier KMs their measured VRAM
//! bytes. Budgets exist per tier (global) and per core; 0 means no limit.
//!
//! * Eviction takes the coldest evictable KM first. Heat is a use count that decays with a half-life, so recent and
//!   frequent use both keep a KM; a pinned KM, a KM used by an in-flight request ([`Guard`]) and a KM in the caller's
//!   protected set are never evicted.
//! * Admission plans every eviction first and applies none unless the newcomer fits; an adapter is attached before
//!   anything is removed from the books, so a failed load leaves the previous state untouched.
//! * Every load, unload, eviction and refusal is an [`Event`], broadcast and kept in a ring buffer.
//!
//! This module knows nothing about the store or about verification; `Km::load` does those and hands over a
//! [`Candidate`].

use std::any::Any;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};

use super::store::Record;
use super::{now_ms, Res};
use crate::engine::{Engine, Fail, Lora, Target};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Disk,
    Ram,
    Vram,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Disk => "disk",
            Tier::Ram => "ram",
            Tier::Vram => "vram",
        }
    }
    pub fn parse(s: &str) -> Option<Tier> {
        match s {
            "disk" => Some(Tier::Disk),
            "ram" => Some(Tier::Ram),
            "vram" => Some(Tier::Vram),
            _ => None,
        }
    }
}

fn gpu_kind(kind: &str) -> bool {
    matches!(kind, "adapter" | "experts" | "projector" | "router")
}

/// What a KM declares it needs, `(ram, vram)`; a missing declaration is estimated from the payload size (GPU kinds
/// count it for VRAM too).
pub fn declared(r: &Record) -> (u64, u64) {
    (
        r.ram_bytes.unwrap_or(r.bytes),
        r.vram_bytes.unwrap_or(if gpu_kind(&r.kind) { r.bytes } else { 0 }),
    )
}

/// What a load of this KM actually takes: its payload is mapped (RAM), and for GPU kinds sent to VRAM.
pub fn measured(kind: &str, payload_bytes: u64) -> (u64, u64) {
    (payload_bytes, if gpu_kind(kind) { payload_bytes } else { 0 })
}

/// More than 10% over what was declared (only for what was declared).
pub fn misdeclared(r: &Record, measured: (u64, u64)) -> Option<String> {
    let over = |m: u64, d: u64| m > d + d / 10;
    if let Some(d) = r.ram_bytes {
        if over(measured.0, d) {
            return Some(format!(
                "declared ram_bytes {d}, measured {} (over by more than 10%)",
                measured.0
            ));
        }
    }
    if let Some(d) = r.vram_bytes {
        if over(measured.1, d) {
            return Some(format!(
                "declared vram_bytes {d}, measured {} (over by more than 10%)",
                measured.1
            ));
        }
    }
    None
}

// ---- clock -------------------------------------------------------------------------------------------------------

pub trait Clock: Send + Sync {
    fn now_s(&self) -> f64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_s(&self) -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }
}

/// A clock that only moves when told to (tests).
pub struct ManualClock(Mutex<f64>);

impl ManualClock {
    pub fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(1_000_000.0)))
    }
    pub fn advance(&self, s: f64) {
        *self.0.lock() += s;
    }
}

impl Clock for ManualClock {
    fn now_s(&self) -> f64 {
        *self.0.lock()
    }
}

// ---- events ------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub seq: u64,
    /// Unix milliseconds.
    pub at: i64,
    /// load, unload, evict, refuse, verify, misdeclared, attach, detach, prefetch, error
    pub kind: &'static str,
    pub key: String,
    pub tier: Option<&'static str>,
    pub bytes: u64,
    pub reason: String,
    pub detail: Value,
}

pub const RING: usize = 1000;

pub struct Events {
    tx: tokio::sync::broadcast::Sender<Event>,
    ring: Mutex<VecDeque<Event>>,
    seq: AtomicU64,
}

impl Events {
    fn new() -> Self {
        Self {
            tx: tokio::sync::broadcast::channel(256).0,
            ring: Mutex::new(VecDeque::new()),
            seq: AtomicU64::new(0),
        }
    }

    pub fn emit(&self, kind: &'static str, key: &str, tier: Option<Tier>, bytes: u64, reason: &str, detail: Value) {
        let e = Event {
            seq: self.seq.fetch_add(1, Ordering::SeqCst) + 1,
            at: now_ms(),
            kind,
            key: key.to_string(),
            tier: tier.map(Tier::as_str),
            bytes,
            reason: reason.to_string(),
            detail,
        };
        {
            let mut r = self.ring.lock();
            if r.len() >= RING {
                r.pop_front();
            }
            r.push_back(e.clone());
        }
        let _ = self.tx.send(e); // nobody listening is fine
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    /// The newest `limit` events after `since` (a sequence number), oldest first.
    pub fn recent(&self, limit: usize, since: Option<u64>, kind: Option<&str>) -> Vec<Event> {
        let r = self.ring.lock();
        let mut v: Vec<Event> = r
            .iter()
            .filter(|e| since.is_none_or(|s| e.seq > s) && kind.is_none_or(|k| e.kind == k))
            .cloned()
            .collect();
        let n = v.len();
        if n > limit {
            v.drain(..n - limit);
        }
        v
    }
}

// ---- attaching ---------------------------------------------------------------------------------------------------

/// How an adapter KM gets onto a running core. The real one uses the engine's LoRA mechanism.
pub trait Attacher: Send + Sync {
    fn attach<'a>(&'a self, core: &'a str, path: &'a Path, scale: f32) -> BoxFuture<'a, Res<()>>;
    fn detach<'a>(&'a self, core: &'a str, path: &'a Path) -> BoxFuture<'a, Res<()>>;
}

pub struct EngineAttacher(pub Arc<Engine>);

impl EngineAttacher {
    fn entry(&self, core: &str) -> Res<crate::catalog::ModelEntry> {
        match self.0.resolve(core)? {
            Target::Llama(e) => Ok(e),
            Target::Ollama(_) => Err(Fail::invalid(
                "Ollama's models take no LoRA adapters from ModelMistress",
            )),
        }
    }
}

impl Attacher for EngineAttacher {
    fn attach<'a>(&'a self, core: &'a str, path: &'a Path, scale: f32) -> BoxFuture<'a, Res<()>> {
        Box::pin(async move {
            if path.to_string_lossy().contains(',') {
                return Err(Fail::invalid(
                    "llama-server cannot take a path with a comma; move the KM store",
                ));
            }
            let entry = self.entry(core)?;
            let mut x = self.0.extras(&entry.id);
            x.lora.retain(|l| l.path != path);
            x.lora.push(Lora {
                path: path.to_path_buf(),
                scale,
            });
            self.0.change_extras(&entry, x).await.map(|_| ())
        })
    }
    fn detach<'a>(&'a self, core: &'a str, path: &'a Path) -> BoxFuture<'a, Res<()>> {
        Box::pin(async move {
            let entry = self.entry(core)?;
            let mut x = self.0.extras(&entry.id);
            let before = x.lora.len();
            x.lora.retain(|l| l.path != path);
            if x.lora.len() == before {
                return Ok(());
            }
            self.0.change_extras(&entry, x).await.map(|_| ())
        })
    }
}

/// Used where no engine is available: attaching is refused with a reason.
pub struct NoAttacher;

impl Attacher for NoAttacher {
    fn attach<'a>(&'a self, core: &'a str, _: &'a Path, _: f32) -> BoxFuture<'a, Res<()>> {
        Box::pin(async move { Err(Fail::unavailable(format!("no engine to attach to {core}"))) })
    }
    fn detach<'a>(&'a self, _: &'a str, _: &'a Path) -> BoxFuture<'a, Res<()>> {
        Box::pin(async { Ok(()) })
    }
}

// ---- the manager -------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Budgets {
    pub ram: u64,
    pub vram: u64,
    /// Per core: `(ram, vram)`.
    pub core: BTreeMap<String, (u64, u64)>,
}

#[derive(Clone)]
pub struct AttachSpec {
    pub path: PathBuf,
    pub scale: f32,
}

/// A verified KM ready to be admitted.
#[derive(Clone)]
pub struct Candidate {
    pub key: String,
    pub id: String,
    pub kind: String,
    pub tier: Tier,
    pub core: Option<String>,
    /// Measured bytes `(ram, vram)`.
    pub measured: (u64, u64),
    pub attach: Option<AttachSpec>,
    /// `attached` for adapters on a core, `pending` for kinds whose assembly into a core is a later phase, `n/a`
    /// for kinds that are used on demand.
    pub attach_state: &'static str,
    pub data: Option<Arc<dyn Any + Send + Sync>>,
    /// Never evict to make room (prefetch): refuse if it does not fit as things are.
    pub no_evict: bool,
}

struct Resident {
    c: Candidate,
    loaded_at: i64,
    last_used: f64,
    heat: f64,
    refs: usize,
}

impl Resident {
    fn bytes(&self) -> u64 {
        match self.c.tier {
            Tier::Ram => self.c.measured.0,
            Tier::Vram => self.c.measured.1,
            Tier::Disk => 0,
        }
    }
    fn heat_at(&self, now: f64, half_life: f64) -> f64 {
        self.heat * 0.5f64.powf(((now - self.last_used).max(0.0)) / half_life.max(1.0))
    }
}

struct State {
    resident: HashMap<String, Resident>,
    budgets: Budgets,
    pinned: HashSet<String>,
    half_life: f64,
}

pub struct Manager {
    state: Mutex<State>,
    gate: tokio::sync::Mutex<()>,
    events: Events,
    clock: Arc<dyn Clock>,
}

#[derive(Debug)]
pub struct Admitted {
    pub already: bool,
    pub evicted: Vec<String>,
    pub replaced: Option<String>,
}

/// Holds a request's KMs: none of them is evicted until the guard is dropped.
pub struct Guard {
    m: Arc<Manager>,
    keys: Vec<String>,
}

impl Guard {
    /// Protects one more resident KM. Returns false if it is not resident.
    pub fn add(&mut self, key: &str) -> bool {
        let mut st = self.m.state.lock();
        match st.resident.get_mut(key) {
            Some(r) => {
                r.refs += 1;
                self.keys.push(key.to_string());
                true
            }
            None => false,
        }
    }
    pub fn keys(&self) -> &[String] {
        &self.keys
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let mut st = self.m.state.lock();
        for k in &self.keys {
            if let Some(r) = st.resident.get_mut(k) {
                r.refs = r.refs.saturating_sub(1);
            }
        }
    }
}

fn refusal(m: &Manager, c: &Candidate, reason: String, detail: Value) -> Fail {
    m.events
        .emit("refuse", &c.key, Some(c.tier), tier_bytes(c), &reason, detail);
    Fail::new(409, "refused", format!("{} was not loaded: {reason}", c.key))
}

fn tier_bytes(c: &Candidate) -> u64 {
    match c.tier {
        Tier::Ram => c.measured.0,
        Tier::Vram => c.measured.1,
        Tier::Disk => 0,
    }
}

impl Manager {
    pub fn new(clock: Arc<dyn Clock>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                resident: HashMap::new(),
                budgets: Budgets::default(),
                pinned: HashSet::new(),
                half_life: 600.0,
            }),
            gate: tokio::sync::Mutex::new(()),
            events: Events::new(),
            clock,
        })
    }

    pub fn events(&self) -> &Events {
        &self.events
    }

    pub fn configure(&self, budgets: Budgets, half_life_s: u64) {
        let mut st = self.state.lock();
        st.budgets = budgets;
        st.half_life = half_life_s.max(1) as f64;
    }

    pub fn set_pinned_ids(&self, ids: HashSet<String>) {
        self.state.lock().pinned = ids;
    }

    pub fn is_resident(&self, key: &str) -> bool {
        self.state.lock().resident.contains_key(key)
    }

    pub fn resident_keys(&self) -> Vec<String> {
        let mut v: Vec<String> = self.state.lock().resident.keys().cloned().collect();
        v.sort();
        v
    }

    /// Adapters attached to a core now, as `(core, file)`.
    pub fn attached(&self) -> Vec<(String, PathBuf)> {
        let st = self.state.lock();
        let mut v: Vec<(String, PathBuf)> = st
            .resident
            .values()
            .filter(|r| r.c.tier == Tier::Vram)
            .filter_map(|r| Some((r.c.core.clone()?, r.c.attach.as_ref()?.path.clone())))
            .collect();
        v.sort();
        v
    }

    pub fn resident_ids(&self) -> HashSet<String> {
        self.state.lock().resident.values().map(|r| r.c.id.clone()).collect()
    }

    pub fn data_of(&self, key: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        self.state.lock().resident.get(key).and_then(|r| r.c.data.clone())
    }

    pub fn refs_of(&self, key: &str) -> usize {
        self.state.lock().resident.get(key).map(|r| r.refs).unwrap_or(0)
    }

    pub fn touch(&self, key: &str) {
        let now = self.clock.now_s();
        let mut st = self.state.lock();
        let hl = st.half_life;
        if let Some(r) = st.resident.get_mut(key) {
            r.heat = r.heat_at(now, hl) + 1.0;
            r.last_used = now;
        }
    }

    /// Protects the resident ones among `keys` until the guard is dropped.
    pub fn guard(self: &Arc<Self>, keys: &[String]) -> Guard {
        let mut g = Guard {
            m: self.clone(),
            keys: vec![],
        };
        for k in keys {
            g.add(k);
        }
        g
    }

    /// Does `bytes` fit in the tier (and the core's share of it) without evicting anything?
    pub fn has_room(&self, tier: Tier, core: Option<&str>, bytes: u64) -> bool {
        let st = self.state.lock();
        Self::violations(&st, tier, core, bytes, &HashSet::new()).is_empty()
    }

    fn used(st: &State, tier: Tier, core: Option<&str>, skip: &HashSet<String>) -> u64 {
        st.resident
            .values()
            .filter(|r| {
                r.c.tier == tier && !skip.contains(&r.c.key) && core.is_none_or(|c| r.c.core.as_deref() == Some(c))
            })
            .map(Resident::bytes)
            .sum()
    }

    /// The limits that would be passed by adding `need` (excluding `skip`, KMs that go away), as
    /// `(core or None, used, budget)`.
    fn violations(
        st: &State,
        tier: Tier,
        core: Option<&str>,
        need: u64,
        skip: &HashSet<String>,
    ) -> Vec<(Option<String>, u64, u64)> {
        let mut out = vec![];
        let global = match tier {
            Tier::Ram => st.budgets.ram,
            Tier::Vram => st.budgets.vram,
            Tier::Disk => 0,
        };
        if global > 0 {
            let used = Self::used(st, tier, None, skip);
            if used + need > global {
                out.push((None, used, global));
            }
        }
        if let Some(c) = core {
            if let Some(&(r, v)) = st.budgets.core.get(c) {
                let b = if tier == Tier::Ram { r } else { v };
                if b > 0 {
                    let used = Self::used(st, tier, Some(c), skip);
                    if used + need > b {
                        out.push((Some(c.to_string()), used, b));
                    }
                }
            }
        }
        out
    }

    /// Makes `c` resident, evicting colder KMs if the budget needs it. `protect` KMs are never evicted.
    pub async fn admit(&self, c: Candidate, attacher: &dyn Attacher, protect: &HashSet<String>) -> Res<Admitted> {
        let _one_at_a_time = self.gate.lock().await;
        let need = tier_bytes(&c);
        if c.tier == Tier::Vram && c.core.is_none() {
            return Err(refusal(
                self,
                &c,
                "the VRAM tier needs a core to attach to".into(),
                json!({}),
            ));
        }
        let now = self.clock.now_s();
        // already there?
        let (replaced, victims) = {
            let mut st = self.state.lock();
            let hl = st.half_life;
            if let Some(r) = st.resident.get_mut(&c.key) {
                if r.c.tier == c.tier && r.c.core == c.core {
                    r.heat = r.heat_at(now, hl) + 1.0;
                    r.last_used = now;
                    return Ok(Admitted {
                        already: true,
                        evicted: vec![],
                        replaced: None,
                    });
                }
            }
            let replaced = st.resident.contains_key(&c.key).then(|| c.key.clone());
            // never fits, even alone
            for (core, _, budget) in Self::violations(
                &st,
                c.tier,
                c.core.as_deref(),
                need,
                &st.resident.keys().cloned().collect(),
            ) {
                if need > budget {
                    drop(st);
                    let what = core.map(|c| format!("core {c}'s ")).unwrap_or_default();
                    return Err(refusal(
                        self,
                        &c,
                        format!(
                            "it can never fit: it needs {need} bytes of {} and {what}budget is {budget}",
                            c.tier.as_str()
                        ),
                        json!({"need": need, "budget": budget, "never_fits": true}),
                    ));
                }
            }
            // plan evictions
            let mut skip: HashSet<String> = replaced.iter().cloned().collect();
            let mut victims: Vec<String> = vec![];
            loop {
                let v = Self::violations(&st, c.tier, c.core.as_deref(), need, &skip);
                let Some((vcore, used, budget)) = v.into_iter().next() else {
                    break;
                };
                if c.no_evict {
                    drop(st);
                    return Err(refusal(
                        self,
                        &c,
                        format!("no room without evicting ({used} of {budget} bytes used, {need} needed)"),
                        json!({"need": need, "used": used, "budget": budget, "no_evict": true}),
                    ));
                }
                let mut pool: Vec<(&String, f64, f64)> = st
                    .resident
                    .values()
                    .filter(|r| {
                        r.c.tier == c.tier
                            && !skip.contains(&r.c.key)
                            && vcore.as_deref().is_none_or(|k| r.c.core.as_deref() == Some(k))
                            && r.refs == 0
                            && !st.pinned.contains(&r.c.id)
                            && !protect.contains(&r.c.key)
                    })
                    .map(|r| (&r.c.key, r.heat_at(now, hl), r.last_used))
                    .collect();
                pool.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.2.total_cmp(&b.2)).then(a.0.cmp(b.0)));
                match pool.first() {
                    Some((k, ..)) => {
                        let k = (*k).clone();
                        skip.insert(k.clone());
                        victims.push(k);
                    }
                    None => {
                        let busy: Vec<String> = st
                            .resident
                            .values()
                            .filter(|r| r.c.tier == c.tier && !skip.contains(&r.c.key))
                            .map(|r| {
                                let why = if r.refs > 0 {
                                    "in flight"
                                } else if st.pinned.contains(&r.c.id) {
                                    "pinned"
                                } else {
                                    "protected"
                                };
                                format!("{} ({why})", r.c.key)
                            })
                            .collect();
                        drop(st);
                        let scope = vcore.map(|c| format!(" for core {c}")).unwrap_or_default();
                        return Err(refusal(
                            self,
                            &c,
                            format!(
                                "no room in {}{scope}: {used} of {budget} bytes used, {need} needed, and nothing left to \
                                 evict (kept: {})",
                                c.tier.as_str(),
                                if busy.is_empty() { "none".to_string() } else { busy.join(", ") }
                            ),
                            json!({"need": need, "used": used, "budget": budget, "kept": busy}),
                        ));
                    }
                }
            }
            (replaced, victims)
        };

        // attach before anything leaves the books: a failure here changes nothing
        let mut attached = false;
        if let (Some(a), Some(core), Tier::Vram) = (&c.attach, &c.core, c.tier) {
            if let Err(e) = attacher.attach(core, &a.path, a.scale).await {
                self.events.emit(
                    "error",
                    &c.key,
                    Some(c.tier),
                    need,
                    &format!("attach to {core} failed"),
                    json!({"error": e.message}),
                );
                return Err(refusal(
                    self,
                    &c,
                    format!("attaching to {core} failed: {}", e.message),
                    json!({"attach_error": e.message}),
                ));
            }
            attached = true;
        }

        // commit, if the plan still holds
        let mut gone: Vec<Resident> = vec![];
        let stale = {
            let mut st = self.state.lock();
            let stale = victims.iter().any(|k| {
                st.resident
                    .get(k)
                    .is_none_or(|r| r.refs > 0 || st.pinned.contains(&r.c.id))
            });
            if !stale {
                for k in victims.iter().chain(replaced.iter()) {
                    if let Some(r) = st.resident.remove(k) {
                        gone.push(r);
                    }
                }
                st.resident.insert(
                    c.key.clone(),
                    Resident {
                        c: c.clone(),
                        loaded_at: now_ms(),
                        last_used: now,
                        heat: 1.0,
                        refs: 0,
                    },
                );
            }
            stale
        };
        if stale {
            if attached {
                if let (Some(a), Some(core)) = (&c.attach, &c.core) {
                    let _ = attacher.detach(core, &a.path).await;
                }
            }
            return Err(refusal(
                self,
                &c,
                "the resident set changed while loading; try again".into(),
                json!({"retry": true}),
            ));
        }
        let mut evicted = vec![];
        for r in gone {
            let is_replaced = replaced.as_deref() == Some(r.c.key.as_str());
            if is_replaced {
                self.events.emit(
                    "unload",
                    &r.c.key,
                    Some(r.c.tier),
                    r.bytes(),
                    &format!("moved to {}", c.tier.as_str()),
                    json!({}),
                );
            } else {
                evicted.push(r.c.key.clone());
                self.events.emit(
                    "evict",
                    &r.c.key,
                    Some(r.c.tier),
                    r.bytes(),
                    &format!("{} needs {need} bytes of {}", c.key, c.tier.as_str()),
                    json!({"for": c.key, "heat": r.heat_at(now, self.state.lock().half_life), "idle_s": now - r.last_used}),
                );
            }
            self.detach_quietly(&r.c, attacher).await;
        }
        self.events.emit(
            "load",
            &c.key,
            Some(c.tier),
            need,
            "loaded",
            json!({"core": c.core, "attach": c.attach_state, "ram": c.measured.0, "vram": c.measured.1,
                   "evicted": evicted}),
        );
        Ok(Admitted {
            already: false,
            evicted,
            replaced,
        })
    }

    async fn detach_quietly(&self, c: &Candidate, attacher: &dyn Attacher) {
        if c.tier != Tier::Vram {
            return;
        }
        if let (Some(a), Some(core)) = (&c.attach, &c.core) {
            match attacher.detach(core, &a.path).await {
                Ok(()) => self.events.emit(
                    "detach",
                    &c.key,
                    Some(c.tier),
                    0,
                    &format!("detached from {core}"),
                    json!({}),
                ),
                Err(e) => self.events.emit(
                    "error",
                    &c.key,
                    Some(c.tier),
                    0,
                    &format!("detaching from {core} failed"),
                    json!({"error": e.message}),
                ),
            }
        }
    }

    /// Unloads a KM. Refused while a request holds it.
    pub async fn unload(&self, key: &str, attacher: &dyn Attacher, reason: &str) -> Res<bool> {
        let _one_at_a_time = self.gate.lock().await;
        let r = {
            let mut st = self.state.lock();
            match st.resident.get(key) {
                None => return Ok(false),
                Some(r) if r.refs > 0 => {
                    let n = r.refs;
                    drop(st);
                    self.events
                        .emit("refuse", key, None, 0, &format!("in use by {n} request(s)"), json!({}));
                    return Err(Fail::new(
                        409,
                        "in_use",
                        format!("{key} is in use by {n} request(s) and cannot be unloaded now"),
                    ));
                }
                Some(_) => st.resident.remove(key),
            }
        };
        if let Some(r) = r {
            self.events
                .emit("unload", key, Some(r.c.tier), r.bytes(), reason, json!({}));
            self.detach_quietly(&r.c, attacher).await;
        }
        Ok(true)
    }

    pub fn snapshot(&self, disk_used: u64, disk_budget: u64) -> Value {
        let now = self.clock.now_s();
        let st = self.state.lock();
        let mut rows: Vec<&Resident> = st.resident.values().collect();
        rows.sort_by(|a, b| a.c.key.cmp(&b.c.key));
        let tier = |t: Tier, budget: u64| {
            let members: Vec<&&Resident> = rows.iter().filter(|r| r.c.tier == t).collect();
            json!({"budget": budget, "used": members.iter().map(|r| r.bytes()).sum::<u64>(),
                   "count": members.len(), "free": if budget > 0 {
                       json!(budget.saturating_sub(members.iter().map(|r| r.bytes()).sum::<u64>()))
                   } else { Value::Null }})
        };
        let mut pinned: Vec<&String> = st.pinned.iter().collect();
        pinned.sort();
        let cores: BTreeMap<String, Value> = st
            .budgets
            .core
            .iter()
            .map(|(c, (ram, vram))| {
                let used = |t: Tier| -> u64 {
                    rows.iter()
                        .filter(|r| r.c.tier == t && r.c.core.as_deref() == Some(c.as_str()))
                        .map(|r| r.bytes())
                        .sum()
                };
                (
                    c.clone(),
                    json!({"ram": {"budget": ram, "used": used(Tier::Ram)}, "vram": {"budget": vram, "used": used(Tier::Vram)}}),
                )
            })
            .collect();
        json!({
            "tiers": {"disk": {"budget": disk_budget, "used": disk_used},
                      "ram": tier(Tier::Ram, st.budgets.ram), "vram": tier(Tier::Vram, st.budgets.vram)},
            "cores": cores,
            "half_life_s": st.half_life,
            "resident": rows.iter().map(|r| json!({
                "key": r.c.key, "id": r.c.id, "kind": r.c.kind, "tier": r.c.tier.as_str(), "core": r.c.core,
                "bytes": r.bytes(), "ram": r.c.measured.0, "vram": r.c.measured.1,
                "heat": (r.heat_at(now, st.half_life) * 1000.0).round() / 1000.0,
                "idle_s": (now - r.last_used).round(), "in_flight": r.refs,
                "pinned": st.pinned.contains(&r.c.id), "attach": r.c.attach_state, "loaded_at": r.loaded_at})).collect::<Vec<_>>(),
            "pinned": pinned,
        })
    }
}

#[cfg(test)]
pub mod tests_support {
    use super::*;

    /// Records attach/detach calls; can be told to fail.
    #[derive(Default)]
    pub struct Recorder {
        pub log: Mutex<Vec<String>>,
        pub fail_attach: std::sync::atomic::AtomicBool,
        pub fail_detach: std::sync::atomic::AtomicBool,
    }

    impl Attacher for Recorder {
        fn attach<'a>(&'a self, core: &'a str, path: &'a Path, scale: f32) -> BoxFuture<'a, Res<()>> {
            Box::pin(async move {
                if self.fail_attach.load(Ordering::SeqCst) {
                    return Err(Fail::unavailable("core did not answer"));
                }
                self.log
                    .lock()
                    .push(format!("attach {core} {} {scale}", path.display()));
                Ok(())
            })
        }
        fn detach<'a>(&'a self, core: &'a str, path: &'a Path) -> BoxFuture<'a, Res<()>> {
            Box::pin(async move {
                if self.fail_detach.load(Ordering::SeqCst) {
                    return Err(Fail::unavailable("core did not answer"));
                }
                self.log.lock().push(format!("detach {core} {}", path.display()));
                Ok(())
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::Recorder;
    use super::*;

    fn cand(key: &str, tier: Tier, bytes: u64) -> Candidate {
        let id = key.split('@').next().unwrap().to_string();
        Candidate {
            key: key.into(),
            id,
            kind: "memory".into(),
            tier,
            core: None,
            measured: (bytes, 0),
            attach: None,
            attach_state: "n/a",
            data: None,
            no_evict: false,
        }
    }

    fn vcand(key: &str, core: &str, bytes: u64) -> Candidate {
        Candidate {
            kind: "adapter".into(),
            core: Some(core.into()),
            measured: (bytes, bytes),
            attach: Some(AttachSpec {
                path: PathBuf::from(format!("/store/{key}/adapter.gguf")),
                scale: 1.0,
            }),
            attach_state: "attached",
            ..cand(key, Tier::Vram, bytes)
        }
    }

    fn mgr(ram: u64, vram: u64) -> (Arc<Manager>, Arc<ManualClock>) {
        let clock = ManualClock::new();
        let m = Manager::new(clock.clone());
        m.configure(
            Budgets {
                ram,
                vram,
                core: BTreeMap::new(),
            },
            600,
        );
        (m, clock)
    }

    fn none() -> HashSet<String> {
        HashSet::new()
    }

    #[tokio::test]
    async fn the_sum_of_resident_bytes_stays_within_the_budget() {
        let (m, clock) = mgr(100, 0);
        let a = NoAttacher;
        for (i, k) in ["a@1.0.0", "b@1.0.0", "c@1.0.0"].iter().enumerate() {
            m.admit(cand(k, Tier::Ram, 40), &a, &none()).await.unwrap();
            clock.advance(1.0);
            let snap = m.snapshot(0, 0);
            assert!(
                snap["tiers"]["ram"]["used"].as_u64().unwrap() <= 100,
                "after {i}: {snap}"
            );
        }
        // c needed room: the coldest (a) went
        assert_eq!(m.resident_keys(), vec!["b@1.0.0", "c@1.0.0"]);
    }

    #[tokio::test]
    async fn eviction_takes_the_coldest_by_decayed_use() {
        let (m, clock) = mgr(100, 0);
        let a = NoAttacher;
        m.admit(cand("old-but-popular@1.0.0", Tier::Ram, 40), &a, &none())
            .await
            .unwrap();
        m.admit(cand("recent-once@1.0.0", Tier::Ram, 40), &a, &none())
            .await
            .unwrap();
        // many uses long ago for the first, one use just now for the second
        for _ in 0..30 {
            m.touch("old-but-popular@1.0.0");
        }
        clock.advance(60.0);
        m.touch("recent-once@1.0.0");
        // the popular one is still hotter after a minute (half-life 10 minutes): the recent-once one is evicted
        let r = m.admit(cand("new@1.0.0", Tier::Ram, 40), &a, &none()).await.unwrap();
        assert_eq!(r.evicted, vec!["recent-once@1.0.0"]);
        // after hours of silence the popular one has decayed below a fresh use
        clock.advance(3.0 * 3600.0);
        m.touch("new@1.0.0");
        let r = m.admit(cand("newer@1.0.0", Tier::Ram, 40), &a, &none()).await.unwrap();
        assert_eq!(r.evicted, vec!["old-but-popular@1.0.0"]);
    }

    #[tokio::test]
    async fn ties_go_to_the_least_recently_used() {
        let (m, clock) = mgr(80, 0);
        let a = NoAttacher;
        m.admit(cand("first@1.0.0", Tier::Ram, 40), &a, &none()).await.unwrap();
        clock.advance(5.0);
        m.admit(cand("second@1.0.0", Tier::Ram, 40), &a, &none()).await.unwrap();
        clock.advance(5.0);
        let r = m.admit(cand("third@1.0.0", Tier::Ram, 40), &a, &none()).await.unwrap();
        assert_eq!(r.evicted, vec!["first@1.0.0"]);
    }

    #[tokio::test]
    async fn pinned_kms_are_never_evicted_and_the_refusal_says_why() {
        let (m, _) = mgr(100, 0);
        let a = NoAttacher;
        m.set_pinned_ids(["keep".to_string()].into());
        m.admit(cand("keep@1.0.0", Tier::Ram, 60), &a, &none()).await.unwrap();
        m.admit(cand("other@1.0.0", Tier::Ram, 40), &a, &none()).await.unwrap();
        // 60 more would need both gone; keep is pinned
        let e = m
            .admit(cand("big@1.0.0", Tier::Ram, 60), &a, &none())
            .await
            .unwrap_err();
        assert_eq!(e.code, "refused");
        assert!(e.message.contains("keep@1.0.0 (pinned)"), "{}", e.message);
        // state untouched: nothing was evicted by the failed attempt
        assert_eq!(m.resident_keys(), vec!["keep@1.0.0", "other@1.0.0"]);
        let ev = m.events().recent(10, None, Some("refuse"));
        assert_eq!(ev.len(), 1);
        assert!(ev[0].reason.contains("no room"));
        assert!(m.events().recent(10, None, Some("evict")).is_empty());
    }

    #[tokio::test]
    async fn a_requests_whole_set_is_protected_until_the_guard_drops() {
        let (m, _) = mgr(100, 0);
        let a = NoAttacher;
        m.admit(cand("a@1.0.0", Tier::Ram, 50), &a, &none()).await.unwrap();
        m.admit(cand("b@1.0.0", Tier::Ram, 50), &a, &none()).await.unwrap();
        let guard = m.guard(&[
            "a@1.0.0".to_string(),
            "b@1.0.0".to_string(),
            "not-resident@1".to_string(),
        ]);
        assert_eq!(guard.keys().len(), 2);
        assert_eq!(m.refs_of("a@1.0.0"), 1);
        let e = m.admit(cand("c@1.0.0", Tier::Ram, 50), &a, &none()).await.unwrap_err();
        assert!(e.message.contains("(in flight)"), "{}", e.message);
        assert!(
            m.unload("a@1.0.0", &a, "test").await.is_err(),
            "no unloading under a request"
        );
        drop(guard);
        assert_eq!(m.refs_of("a@1.0.0"), 0);
        let r = m.admit(cand("c@1.0.0", Tier::Ram, 50), &a, &none()).await.unwrap();
        assert_eq!(r.evicted.len(), 1);
    }

    #[tokio::test]
    async fn the_callers_protected_set_is_not_evicted() {
        let (m, _) = mgr(100, 0);
        let a = NoAttacher;
        m.admit(cand("a@1.0.0", Tier::Ram, 50), &a, &none()).await.unwrap();
        m.admit(cand("b@1.0.0", Tier::Ram, 50), &a, &none()).await.unwrap();
        let protect: HashSet<String> = ["a@1.0.0".to_string()].into();
        let r = m.admit(cand("c@1.0.0", Tier::Ram, 50), &a, &protect).await.unwrap();
        assert_eq!(r.evicted, vec!["b@1.0.0"]);
    }

    #[tokio::test]
    async fn a_km_bigger_than_the_budget_is_refused_up_front() {
        let (m, _) = mgr(100, 0);
        let e = m
            .admit(cand("huge@1.0.0", Tier::Ram, 101), &NoAttacher, &none())
            .await
            .unwrap_err();
        assert!(e.message.contains("can never fit"), "{}", e.message);
        assert_eq!(m.events().recent(5, None, None)[0].detail["never_fits"], true);
    }

    #[tokio::test]
    async fn per_core_budgets_apply_inside_the_global_one() {
        let (m, _) = mgr(0, 1000);
        m.configure(
            Budgets {
                ram: 0,
                vram: 1000,
                core: [("dusty".to_string(), (0u64, 100u64))].into(),
            },
            600,
        );
        let rec = Recorder::default();
        m.admit(vcand("a@1.0.0", "dusty", 60), &rec, &none()).await.unwrap();
        m.admit(vcand("m@1.0.0", "misty", 500), &rec, &none()).await.unwrap(); // another core: no limit but global
        let r = m.admit(vcand("b@1.0.0", "dusty", 60), &rec, &none()).await.unwrap();
        assert_eq!(
            r.evicted,
            vec!["a@1.0.0"],
            "dusty's 100 bytes hold one of them; misty's was left alone"
        );
        assert!(m.is_resident("m@1.0.0"));
        let e = m
            .admit(vcand("x@1.0.0", "dusty", 101), &rec, &none())
            .await
            .unwrap_err();
        assert!(e.message.contains("core dusty"), "{}", e.message);
        // global VRAM: 1000
        let e = m
            .admit(vcand("y@1.0.0", "misty", 1001), &rec, &none())
            .await
            .unwrap_err();
        assert!(e.message.contains("can never fit"));
    }

    #[tokio::test]
    async fn adapters_attach_before_the_books_change_and_a_failed_attach_changes_nothing() {
        let (m, _) = mgr(0, 100);
        let rec = Recorder::default();
        m.admit(vcand("a@1.0.0", "dusty", 60), &rec, &none()).await.unwrap();
        rec.fail_attach.store(true, Ordering::SeqCst);
        let e = m.admit(vcand("b@1.0.0", "dusty", 60), &rec, &none()).await.unwrap_err();
        assert!(e.message.contains("attaching to dusty failed"), "{}", e.message);
        assert_eq!(
            m.resident_keys(),
            vec!["a@1.0.0"],
            "the old one was not evicted by the failed load"
        );
        assert_eq!(rec.log.lock().len(), 1, "only a's attach");
        rec.fail_attach.store(false, Ordering::SeqCst);
        m.admit(vcand("b@1.0.0", "dusty", 60), &rec, &none()).await.unwrap();
        let log = rec.log.lock().clone();
        assert!(log[1].starts_with("attach dusty") && log[1].contains("b@1.0.0"));
        assert!(
            log[2].starts_with("detach dusty") && log[2].contains("a@1.0.0"),
            "{log:?}"
        );
        // a detach that fails is an event, not a stuck load
        rec.fail_detach.store(true, Ordering::SeqCst);
        m.admit(vcand("c@1.0.0", "dusty", 60), &rec, &none()).await.unwrap();
        assert!(!m.events().recent(50, None, Some("error")).is_empty());
        assert_eq!(m.resident_keys(), vec!["c@1.0.0"]);
    }

    #[tokio::test]
    async fn the_vram_tier_needs_a_core() {
        let (m, _) = mgr(0, 100);
        let e = m
            .admit(cand("a@1.0.0", Tier::Vram, 10), &NoAttacher, &none())
            .await
            .unwrap_err();
        assert!(e.message.contains("needs a core"), "{}", e.message);
    }

    #[tokio::test]
    async fn moving_between_tiers_replaces_the_entry() {
        let (m, _) = mgr(100, 100);
        let rec = Recorder::default();
        let mut ram = vcand("a@1.0.0", "dusty", 60);
        ram.tier = Tier::Ram;
        m.admit(ram, &rec, &none()).await.unwrap();
        assert!(rec.log.lock().is_empty(), "RAM tier does not attach");
        let r = m.admit(vcand("a@1.0.0", "dusty", 60), &rec, &none()).await.unwrap();
        assert_eq!(r.replaced.as_deref(), Some("a@1.0.0"));
        let snap = m.snapshot(0, 0);
        assert_eq!(snap["tiers"]["ram"]["used"], 0);
        assert_eq!(snap["tiers"]["vram"]["used"], 60);
        // admitting the same thing again is a no-op that only warms it
        assert!(
            m.admit(vcand("a@1.0.0", "dusty", 60), &rec, &none())
                .await
                .unwrap()
                .already
        );
    }

    #[tokio::test]
    async fn events_are_broadcast_and_kept() {
        let (m, _) = mgr(50, 0);
        let mut rx = m.events().subscribe();
        let a = NoAttacher;
        m.admit(cand("a@1.0.0", Tier::Ram, 40), &a, &none()).await.unwrap();
        m.admit(cand("b@1.0.0", Tier::Ram, 40), &a, &none()).await.unwrap();
        m.unload("b@1.0.0", &a, "owner asked").await.unwrap();
        let mut kinds = vec![];
        while let Ok(e) = rx.try_recv() {
            kinds.push((e.kind, e.key.clone()));
        }
        assert_eq!(
            kinds,
            vec![
                ("load", "a@1.0.0".to_string()),
                ("evict", "a@1.0.0".to_string()),
                ("load", "b@1.0.0".to_string()),
                ("unload", "b@1.0.0".to_string())
            ]
        );
        let all = m.events().recent(100, None, None);
        assert_eq!(all.len(), 4);
        assert!(all.windows(2).all(|w| w[0].seq < w[1].seq));
        assert_eq!(m.events().recent(2, None, None).len(), 2);
        assert_eq!(m.events().recent(100, Some(all[1].seq), None).len(), 2);
        assert!(all[1].reason.contains("b@1.0.0 needs 40 bytes of ram"));
    }

    #[tokio::test]
    async fn the_ring_buffer_is_bounded() {
        let (m, _) = mgr(0, 0);
        for i in 0..(RING + 50) {
            m.events().emit("verify", &format!("k{i}"), None, 0, "x", json!({}));
        }
        let all = m.events().recent(usize::MAX, None, None);
        assert_eq!(all.len(), RING);
        assert_eq!(all[0].key, "k50");
    }

    #[tokio::test]
    async fn room_checks_and_unload() {
        let (m, _) = mgr(100, 0);
        let a = NoAttacher;
        m.admit(cand("a@1.0.0", Tier::Ram, 70), &a, &none()).await.unwrap();
        assert!(m.has_room(Tier::Ram, None, 30) && !m.has_room(Tier::Ram, None, 31));
        assert!(m.unload("a@1.0.0", &a, "done").await.unwrap());
        assert!(!m.unload("a@1.0.0", &a, "done").await.unwrap());
        assert!(m.has_room(Tier::Ram, None, 100));
    }

    #[test]
    fn misdeclared_is_more_than_ten_percent_over() {
        let mut r = crate::km::store::tests_record();
        r.ram_bytes = Some(1000);
        r.vram_bytes = Some(1000);
        assert!(misdeclared(&r, (1100, 1100)).is_none(), "exactly 10% is fine");
        assert!(misdeclared(&r, (1101, 0)).unwrap().contains("ram_bytes"));
        assert!(misdeclared(&r, (0, 1101)).unwrap().contains("vram_bytes"));
        r.ram_bytes = None;
        r.vram_bytes = None;
        assert!(
            misdeclared(&r, (u64::MAX / 2, u64::MAX / 2)).is_none(),
            "nothing declared, nothing to exceed"
        );
    }
}

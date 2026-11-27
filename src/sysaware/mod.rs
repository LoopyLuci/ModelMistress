//! System awareness: ModelMistress looks at the machine it runs on and adjusts itself, live, to what the machine is doing.
//!
//! Her runs (model servers, Forge training, image and video generation, quantizing, transcoding) are heavy, and the
//! machine is also somebody's desk. This module keeps the two from fighting:
//!
//! - **Looking.** Every tick (2 s) the hub reads the CPU, memory and commit it can see by itself (`probe`), what the user
//!   is doing at the screen (a full-screen game or a presentation), and, when SystemWatchdog runs, the GPU's load and
//!   temperature, the CPU's temperature, the drives, the video memory and which programs are the busy ones (`feed`).
//! - **Judging.** The readings become one of four levels, Calm, Busy, Strained, Critical, each with the reasons (`level`).
//!   A level rises after two looks and falls one step after five, so a flicker around a limit changes nothing.
//! - **Adjusting.** What each process may do at each level is a table (`policy`). The governor applies it to the hub's
//!   own processes, lowering a priority, turning efficiency mode on, pausing a worker at Critical, and puts back exactly
//!   what it changed (`governor`, `actuate`). It only lowers, only touches what the hub started, and gives way to
//!   anyone else who changes a priority.
//! - **Waiting.** A heavy job (a training run, image generation, quantizing) does not start while the machine is
//!   Strained or worse. It waits, says why in the job's message, and can be cancelled or told to start (`system.run_now`).
//! - **Shedding.** When memory, commit or video memory is the reason, a model that has sat idle is unloaded, one at a time.
//!
//! It works without Watchdog; Watchdog only adds what the hub cannot measure. Settings: `<home>/system.toml`, or
//! `system.config.set`. Status: `system.status`, `model-mistress system`.

pub mod actuate;
pub mod cli;
pub mod feed;
pub mod governor;
pub mod level;
pub mod ops;
pub mod policy;
pub mod probe;

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use serde_json::{json, Value};

use crate::engine::Engine;
use governor::{Event, Governor, Row, Target};
use level::{assess, level_of, Level, Reason, Signals, Tracker};
use policy::{is_helper, kind_of, Config, Mode};
use probe::Native;

/// The jobs that wait for the machine (the heavy ones; a download or an install does not).
pub const HEAVY: &[&str] = &["forge", "factory", "quantize", "image"];

fn now_s() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// What the last look found.
#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub level: Level,
    /// This look's level before it is steadied.
    pub raw: Level,
    pub looks: u64,
    pub at: u64,
    /// Since when the steady level has held (unix seconds).
    pub since: u64,
    pub reasons: Vec<Reason>,
    pub signals: Value,
    pub feed: Value,
    /// The processes held down now.
    pub holding: Vec<Row>,
    pub paused: usize,
    pub recent: Vec<Event>,
    /// Whether this platform lets the governor change processes.
    pub actuator: bool,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            level: Level::Calm,
            raw: Level::Calm,
            looks: 0,
            at: 0,
            since: now_s(),
            reasons: vec![],
            signals: json!({}),
            feed: json!({"state": "absent"}),
            holding: vec![],
            paused: 0,
            recent: vec![],
            actuator: false,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct Waiter {
    job: String,
    kind: String,
    label: String,
    since: u64,
}

/// How a job's wait at the gate ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admit {
    Ran,
    Cancelled,
}

/// What the governor and the rest of the hub share.
pub struct Shared {
    home: PathBuf,
    cfg: RwLock<Config>,
    cfg_error: Mutex<Option<String>>,
    cfg_mtime: Mutex<Option<SystemTime>>,
    snap: RwLock<Snapshot>,
    wake: tokio::sync::Notify,
    /// Bumped by `system.run_now`: a job that began waiting before it starts.
    release_gen: AtomicU64,
    waiting: Mutex<Vec<Waiter>>,
    sheds: Mutex<VecDeque<Event>>,
    /// A look has been made (before it, nothing is judged and no job waits).
    looked: AtomicBool,
    quit: AtomicBool,
}

static GLOBAL: OnceLock<Arc<Shared>> = OnceLock::new();

/// The running hub's state, when the hub started the governor (a test process has none).
pub fn shared() -> Option<&'static Arc<Shared>> {
    GLOBAL.get()
}

fn mtime(home: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(home.join("system.toml"))
        .and_then(|m| m.modified())
        .ok()
}

impl Shared {
    pub fn new(home: PathBuf) -> Arc<Self> {
        let (cfg, err) = match Config::load(&home) {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(e)),
        };
        let m = mtime(&home);
        Arc::new(Self {
            home,
            cfg: RwLock::new(cfg),
            cfg_error: Mutex::new(err),
            cfg_mtime: Mutex::new(m),
            snap: RwLock::new(Snapshot::default()),
            wake: tokio::sync::Notify::new(),
            release_gen: AtomicU64::new(0),
            waiting: Mutex::new(vec![]),
            sheds: Mutex::new(VecDeque::new()),
            looked: AtomicBool::new(false),
            quit: AtomicBool::new(false),
        })
    }

    pub fn cfg(&self) -> Config {
        self.cfg.read().clone()
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snap.read().clone()
    }

    /// Takes in an edit of `system.toml` made by hand. A damaged file keeps the last good settings and is reported.
    fn reload_if_changed(&self) {
        let now = mtime(&self.home);
        {
            let mut last = self.cfg_mtime.lock();
            if *last == now {
                return;
            }
            *last = now;
        }
        match Config::load(&self.home) {
            Ok(c) => {
                *self.cfg.write() = c;
                *self.cfg_error.lock() = None;
                eprintln!("model-mistress: system.toml reloaded");
            }
            Err(e) => {
                let first = self.cfg_error.lock().replace(e.clone()).as_deref() != Some(e.as_str());
                if first {
                    eprintln!("model-mistress: {e}; keeping the last good settings");
                }
            }
        }
    }

    /// Applies a merge patch to the settings and writes them. Unknown keys and out-of-order limits are refused.
    pub fn set_config(&self, patch: &Value) -> Result<Config, String> {
        let new = self.cfg().patched(patch)?;
        new.save(&self.home)?;
        *self.cfg.write() = new.clone();
        *self.cfg_error.lock() = None;
        *self.cfg_mtime.lock() = mtime(&self.home);
        self.wake.notify_one();
        Ok(new)
    }

    /// Lets the jobs that are waiting start now. Returns how many were waiting.
    pub fn run_now(&self) -> usize {
        self.release_gen.fetch_add(1, Ordering::AcqRel);
        self.waiting.lock().len()
    }

    /// A job at the gate. Returns when the machine is not strained, when `run_now` was called, when `max_wait_s` is up, or
    /// when the job was cancelled.
    pub async fn admit(
        &self,
        job: &str,
        kind: &str,
        label: &str,
        cancelled: impl Fn() -> bool,
        say: impl Fn(String),
    ) -> Admit {
        let begun = Instant::now();
        let gen = self.release_gen.load(Ordering::Acquire);
        let mut listed = false;
        let outcome = loop {
            let cfg = self.cfg();
            let (level, why) = {
                let s = self.snap.read();
                (
                    s.level,
                    s.reasons.iter().max_by_key(|r| r.level).map(|r| r.detail.clone()),
                )
            };
            let strained = self.looked.load(Ordering::Acquire) && cfg.mode != Mode::Off && level >= Level::Strained;
            if !strained || self.release_gen.load(Ordering::Acquire) != gen {
                break Admit::Ran;
            }
            if cancelled() {
                break Admit::Cancelled;
            }
            if cfg.max_wait_s > 0 && begun.elapsed() >= Duration::from_secs(cfg.max_wait_s) {
                break Admit::Ran;
            }
            if !listed {
                listed = true;
                self.waiting.lock().push(Waiter {
                    job: job.into(),
                    kind: kind.into(),
                    label: label.into(),
                    since: now_s(),
                });
                eprintln!(
                    "model-mistress: {kind} job {job} waits: the machine is {}",
                    level.name()
                );
            }
            say(format!(
                "waiting for the machine to quiet ({}s): {}. `system.run_now` starts it anyway",
                begun.elapsed().as_secs(),
                why.unwrap_or_else(|| format!("it is {}", level.name()))
            ));
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        if listed {
            self.waiting.lock().retain(|w| w.job != job);
        }
        outcome
    }

    /// For `system.status`.
    pub fn status_json(&self) -> Value {
        let cfg = self.cfg();
        let s = self.snapshot();
        let held: Vec<Value> = self.waiting.lock().iter().map(|w| json!({"job": w.job, "kind": w.kind, "label": w.label, "waiting_s": now_s().saturating_sub(w.since)})).collect();
        json!({
            "mode": cfg.mode,
            "level": s.level,
            "raw_level": s.raw,
            "for_s": now_s().saturating_sub(s.since),
            "reasons": s.reasons,
            "signals": s.signals,
            "watchdog": s.feed,
            "holding": s.holding,
            "paused": s.paused,
            "waiting_jobs": held,
            "recent": s.recent,
            "looks": s.looks,
            "tick_ms": cfg.tick_ms,
            "can_change_processes": s.actuator,
            "settings_error": *self.cfg_error.lock(),
            "settings_file": self.home.join("system.toml"),
        })
    }

    /// A few words for `machine.status` and the dashboard.
    pub fn summary(&self) -> Value {
        let cfg = self.cfg();
        let s = self.snapshot();
        json!({"mode": cfg.mode, "level": s.level, "reasons": s.reasons.iter().map(|r| r.detail.clone()).collect::<Vec<_>>(),
               "watchdog": s.feed["state"], "holding": s.holding.len(), "paused": s.paused})
    }

    fn shed_note(&self, model: &str, what: String) {
        let mut q = self.sheds.lock();
        if q.len() >= 20 {
            q.pop_front();
        }
        q.push_back(Event {
            at: now_s(),
            pid: 0,
            name: model.to_string(),
            what,
        });
    }
}

/// The gate `Jobs::start_task` calls for every task: a heavy job waits while the machine is strained.
pub async fn admit(kind: &str, job: &crate::pull::Job) -> Admit {
    let Some(sh) = shared() else { return Admit::Ran };
    if !HEAVY.contains(&kind) {
        return Admit::Ran;
    }
    sh.admit(
        &job.id,
        kind,
        &job.model,
        || job.cancelled(),
        |note| job.progress(0, 0, note),
    )
    .await
}

/// The readings of one look as signals (the hub's own measurements; Watchdog's are merged in after).
pub fn signals_of(r: &probe::Reading) -> Signals {
    Signals {
        cpu_total_pct: r.cpu_total_pct,
        cpu_mm_pct: r.cpu_mm_pct,
        // what the rest of the machine uses: the whole less ours
        cpu_foreign_pct: r.cpu_total_pct.map(|t| (t - r.cpu_mm_pct).max(0.0)),
        ram_total_mib: r.ram_total_mib,
        ram_avail_mib: r.ram_avail_mib,
        commit_headroom_mib: r.commit_headroom_mib,
        user: r.user,
        offenders: r.others.iter().take(3).map(|(n, c)| (n.clone(), *c, 0.0)).collect(),
        ..Default::default()
    }
}

/// The running governor. `stop` puts back everything it changed.
pub struct Handle {
    sh: Arc<Shared>,
    task: tokio::task::JoinHandle<()>,
}

impl Handle {
    pub async fn stop(self) {
        self.sh.quit.store(true, Ordering::Release);
        self.sh.wake.notify_one();
        if tokio::time::timeout(Duration::from_secs(5), self.task).await.is_err() {
            eprintln!("model-mistress: the system governor did not stop in 5 s");
        }
    }
}

/// Starts the governor for this hub. Call once.
pub fn start(engine: Arc<Engine>) -> Handle {
    let sh = Shared::new(engine.home.clone());
    if GLOBAL.set(sh.clone()).is_err() {
        eprintln!("model-mistress: the system governor was started twice");
    }
    let task = tokio::spawn(run(sh.clone(), engine));
    Handle { sh, task }
}

/// The loop. Public for the tests, which run it on a `Shared` of their own.
pub async fn run(sh: Arc<Shared>, engine: Arc<Engine>) {
    let mut native = Some(Native::new());
    let mut feed = feed::Feed::new();
    let c0 = sh.cfg();
    let mut tracker = Tracker::new(c0.rise_after, c0.fall_after);
    let mut tracker_cfg = (c0.rise_after, c0.fall_after);
    let mut feed_url = c0.watchdog_url.clone();
    let mut gov = Governor::new(Arc::from(actuate::native()));
    let (mut looks, mut since, mut shown) = (0u64, now_s(), Level::Calm);
    let mut last_shed: Option<Instant> = None;
    let own = std::process::id();
    eprintln!(
        "model-mistress: system awareness on (mode {:?}, a look every {} ms)",
        c0.mode, c0.tick_ms
    );
    while !sh.quit.load(Ordering::Acquire) {
        sh.reload_if_changed();
        let cfg = sh.cfg();
        if (cfg.rise_after, cfg.fall_after) != tracker_cfg {
            let at = tracker.level();
            tracker = Tracker::new(cfg.rise_after, cfg.fall_after);
            tracker.reset(at);
            tracker_cfg = (cfg.rise_after, cfg.fall_after);
        }
        if cfg.watchdog_url != feed_url {
            // pointed somewhere else: look there now, not after the wait a miss at the old address earned
            feed.retry_now();
            feed_url = cfg.watchdog_url.clone();
        }

        // the look: a blocking read of the system, off the async threads
        let mut n = native.take().unwrap_or_default();
        let Ok((n, r)) = tokio::task::spawn_blocking(move || {
            let r = n.sample();
            (n, r)
        })
        .await
        else {
            tokio::time::sleep(Duration::from_millis(cfg.tick_ms)).await;
            continue;
        };
        native = Some(n);
        looks += 1;

        let mut sig = signals_of(&r);
        let card = engine
            .gpu_devices_if_recent(Duration::from_secs(60))
            .and_then(|d| d.into_iter().max_by_key(|d| d.total_mib));
        if let Some(d) = card {
            sig.vram_total_mib = Some(d.total_mib);
            sig.vram_free_mib = Some(d.free_mib);
        }
        let mine: HashSet<u32> = r.tree.iter().map(|p| p.pid).chain([own]).collect();
        if let Some(w) = feed.poll(&cfg.watchdog_url, cfg.watchdog_max_age_s).await {
            feed::merge(&mut sig, &w, &|pid| mine.contains(&pid));
        }

        let reasons = assess(&sig, &cfg.thresholds, engine.settings().vram_reserve_mib);
        let raw = level_of(&reasons);
        let level = if cfg.mode == Mode::Off {
            tracker.reset(Level::Calm);
            Level::Calm
        } else {
            tracker.update(raw).0
        };
        if level != shown {
            since = now_s();
            let why = reasons
                .iter()
                .max_by_key(|x| x.level)
                .map(|x| x.detail.as_str())
                .unwrap_or("nothing now");
            eprintln!(
                "model-mistress: the machine is {} (was {}): {why}",
                level.name(),
                shown.name()
            );
            shown = level;
        }

        // adjust the hub's own processes
        let servers: HashSet<u32> = engine.loaded().iter().map(|i| i.pid).collect();
        let targets: Vec<Target> = r
            .tree
            .iter()
            .filter(|p| !is_helper(&p.name))
            .map(|p| {
                let (kind, pausable) = kind_of(&p.name, servers.contains(&p.pid));
                Target {
                    pid: p.pid,
                    started_s: p.started_s,
                    name: p.name.clone(),
                    kind,
                    pausable,
                }
            })
            .collect();
        gov.tick(&cfg, level, &targets, Instant::now());

        // shed an idle model when memory is what is short
        if cfg.shed
            && cfg.mode != Mode::Off
            && level >= Level::Strained
            && last_shed.is_none_or(|t| t.elapsed() >= Duration::from_secs(10))
        {
            let memory = reasons
                .iter()
                .any(|x| x.level >= Level::Strained && matches!(x.resource, "memory" | "commit" | "vram"));
            if memory {
                if let Some((model, idle)) = engine.shed_one(cfg.shed_idle_s, level == Level::Critical) {
                    let why = reasons
                        .iter()
                        .filter(|x| matches!(x.resource, "memory" | "commit" | "vram"))
                        .map(|x| x.detail.clone())
                        .next()
                        .unwrap_or_default();
                    sh.shed_note(&model, format!("unloaded after {idle}s idle: {why}"));
                    eprintln!("model-mistress: unloaded {model} (idle {idle}s): {why}");
                    last_shed = Some(Instant::now());
                }
            }
        }

        let mut recent = gov.events();
        recent.extend(sh.sheds.lock().iter().cloned());
        recent.sort_by_key(|e| std::cmp::Reverse(e.at));
        recent.truncate(30);
        *sh.snap.write() = Snapshot {
            level,
            raw,
            looks,
            at: now_s(),
            since,
            reasons,
            signals: serde_json::to_value(&sig).unwrap_or_else(|_| json!({})),
            feed: feed.state().to_json(&cfg.watchdog_url),
            holding: gov.rows(),
            paused: gov.paused(),
            recent,
            actuator: gov.active(),
        };
        sh.looked.store(true, Ordering::Release);

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(cfg.tick_ms)) => {}
            _ = sh.wake.notified() => {}
        }
    }
    let n = gov.release_all();
    if n > 0 {
        eprintln!("model-mistress: the system governor let go of {n} process(es)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestDir;

    fn shared_in(dir: &TestDir) -> Arc<Shared> {
        Shared::new(dir.to_path_buf())
    }

    fn strain(sh: &Shared, level: Level) {
        sh.snap.write().level = level;
        sh.looked.store(true, Ordering::Release);
    }

    #[test]
    fn the_foreign_cpu_is_the_machines_less_what_the_hub_started() {
        let r = probe::Reading {
            cpu_total_pct: Some(70.0),
            cpu_mm_pct: 25.0,
            ram_total_mib: 32_000,
            ram_avail_mib: 8_000,
            ..Default::default()
        };
        let s = signals_of(&r);
        assert_eq!(s.cpu_foreign_pct, Some(45.0));
        // never negative when the two clocks disagree
        let r = probe::Reading {
            cpu_total_pct: Some(10.0),
            cpu_mm_pct: 12.0,
            ..Default::default()
        };
        assert_eq!(signals_of(&r).cpu_foreign_pct, Some(0.0));
        // the first look has none
        assert_eq!(signals_of(&probe::Reading::default()).cpu_foreign_pct, None);
    }

    #[tokio::test]
    async fn a_heavy_job_starts_at_once_when_the_machine_is_calm_or_not_yet_looked_at() {
        let dir = TestDir::new("mm-sys-gate");
        let sh = shared_in(&dir);
        // not looked at yet: nothing is judged
        sh.snap.write().level = Level::Critical;
        assert_eq!(sh.admit("j1", "forge", "x", || false, |_| {}).await, Admit::Ran);
        strain(&sh, Level::Busy);
        assert_eq!(
            sh.admit("j2", "forge", "x", || false, |_| {}).await,
            Admit::Ran,
            "busy is not strained"
        );
        strain(&sh, Level::Calm);
        assert_eq!(sh.admit("j3", "forge", "x", || false, |_| {}).await, Admit::Ran);
    }

    #[tokio::test]
    async fn a_heavy_job_waits_while_strained_says_why_and_starts_when_it_eases() {
        let dir = TestDir::new("mm-sys-wait");
        let sh = shared_in(&dir);
        strain(&sh, Level::Strained);
        sh.snap.write().reasons = vec![Reason {
            resource: "cpu",
            level: Level::Strained,
            detail: "other programs use 85% of the CPU".into(),
        }];
        let said = Arc::new(Mutex::new(vec![]));
        let (s2, said2) = (sh.clone(), said.clone());
        let t = tokio::spawn(async move {
            s2.admit("j1", "forge", "train", || false, |m| said2.lock().push(m))
                .await
        });
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(!t.is_finished(), "it waits");
        assert_eq!(sh.status_json()["waiting_jobs"].as_array().unwrap().len(), 1);
        assert!(
            said.lock()
                .iter()
                .any(|m| m.contains("85% of the CPU") && m.contains("system.run_now")),
            "{:?}",
            said.lock()
        );
        strain(&sh, Level::Busy);
        assert_eq!(t.await.unwrap(), Admit::Ran);
        assert!(sh.status_json()["waiting_jobs"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn run_now_cancel_and_the_longest_wait_each_end_the_wait() {
        let dir = TestDir::new("mm-sys-end");
        let sh = shared_in(&dir);
        strain(&sh, Level::Critical);

        let s = sh.clone();
        let t = tokio::spawn(async move { s.admit("a", "quantize", "q", || false, |_| {}).await });
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert_eq!(sh.run_now(), 1);
        assert_eq!(t.await.unwrap(), Admit::Ran, "told to start");
        // a job that begins after run_now waits again: it was one release, not a switch
        let s = sh.clone();
        let t = tokio::spawn(async move { s.admit("b", "quantize", "q", || false, |_| {}).await });
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert!(!t.is_finished());
        t.abort();

        let flag = Arc::new(AtomicBool::new(false));
        let (s, f) = (sh.clone(), flag.clone());
        let t = tokio::spawn(async move {
            s.admit("c", "image", "i", move || f.load(Ordering::Relaxed), |_| {})
                .await
        });
        tokio::time::sleep(Duration::from_millis(700)).await;
        flag.store(true, Ordering::Relaxed);
        assert_eq!(t.await.unwrap(), Admit::Cancelled);

        sh.set_config(&json!({"max_wait_s": 1})).unwrap();
        let t0 = Instant::now();
        assert_eq!(
            sh.admit("d", "factory", "f", || false, |_| {}).await,
            Admit::Ran,
            "gives up waiting"
        );
        assert!(t0.elapsed() < Duration::from_secs(4), "{:?}", t0.elapsed());
    }

    #[tokio::test]
    async fn only_heavy_kinds_wait_and_the_off_mode_waits_for_nothing() {
        let dir = TestDir::new("mm-sys-kinds");
        let sh = shared_in(&dir);
        strain(&sh, Level::Critical);
        // the kinds are checked by `admit` (the free function); here the mode
        sh.set_config(&json!({"mode": "off"})).unwrap();
        assert_eq!(sh.admit("a", "forge", "x", || false, |_| {}).await, Admit::Ran);
        assert!(
            HEAVY.contains(&"forge")
                && HEAVY.contains(&"quantize")
                && HEAVY.contains(&"image")
                && HEAVY.contains(&"factory")
        );
        assert!(!HEAVY.contains(&"pull") && !HEAVY.contains(&"forge-install") && !HEAVY.contains(&"misty"));
    }

    #[test]
    fn settings_are_saved_reloaded_and_a_bad_edit_keeps_the_last_good_ones() {
        let dir = TestDir::new("mm-sys-cfg");
        let sh = shared_in(&dir);
        assert_eq!(sh.cfg().mode, Mode::Balanced);
        sh.set_config(&json!({"mode": "strict", "thresholds": {"cpu_busy_pct": 50.0}}))
            .unwrap();
        assert_eq!(sh.cfg().mode, Mode::Strict);
        assert_eq!(sh.cfg().thresholds.cpu_busy_pct, 50.0);
        assert_eq!(sh.cfg().thresholds.cpu_strained_pct, 80.0, "a nested patch merges");
        assert!(dir.join("system.toml").exists());
        assert_eq!(
            Shared::new(dir.to_path_buf()).cfg().mode,
            Mode::Strict,
            "it survives a restart"
        );

        // a misspelt setting and an impossible one are refused and change nothing
        assert!(sh.set_config(&json!({"modee": "off"})).is_err());
        assert!(sh.set_config(&json!({"thresholds": {"cpu_busy_pct": 99.0}})).is_err());
        assert_eq!(sh.cfg().mode, Mode::Strict);

        // a hand edit is taken in; a broken one is reported and ignored
        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(dir.join("system.toml"), "mode = \"gentle\"\n").unwrap();
        sh.reload_if_changed();
        assert_eq!(sh.cfg().mode, Mode::Gentle);
        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(dir.join("system.toml"), "mode = [broken").unwrap();
        sh.reload_if_changed();
        assert_eq!(sh.cfg().mode, Mode::Gentle);
        assert!(sh.status_json()["settings_error"].as_str().is_some());
    }

    /// The whole loop on this machine: the real probe, the real policy and, on Windows, the real actuator. The limits are
    /// set so that any CPU use at all is Critical; a process this test started is then lowered, and put back when the
    /// governor is switched off.
    #[cfg(windows)]
    #[tokio::test]
    async fn the_loop_lowers_a_real_child_when_strained_and_puts_it_back_when_switched_off() {
        use std::process::{Command, Stdio};
        let home = crate::testing::temp_home();
        let hub = crate::testing::hub(&home, &["alpha"]);
        let sh = Shared::new(home.to_path_buf());
        sh.set_config(
            &json!({"tick_ms": 250, "rise_after": 1, "fall_after": 1, "pause_after_s": 3600,
                              "thresholds": {"cpu_busy_pct": 0.0, "cpu_strained_pct": 0.0, "cpu_critical_pct": 0.0}}),
        )
        .unwrap();
        let mut child = Command::new("powershell")
            .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 120"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let act = actuate::native();
        let class_now = || act.state(pid, started).map(|s| s.class);
        let before = class_now().expect("readable");

        let task = tokio::spawn(run(sh.clone(), hub.engine.clone()));
        let mut lowered = false;
        for _ in 0..60 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if class_now() == Some(actuate::IDLE) {
                lowered = true;
                break;
            }
        }
        let snap = sh.snapshot();
        assert!(
            lowered,
            "the child was not lowered: class {:?}, level {:?}, reasons {:?}",
            class_now(),
            snap.level,
            snap.reasons
        );
        assert_eq!(snap.level, Level::Critical);
        assert!(snap.holding.iter().any(|r| r.pid == pid), "{:?}", snap.holding);

        sh.set_config(&json!({"mode": "off"})).unwrap();
        let mut back = false;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if class_now() == Some(before) {
                back = true;
                break;
            }
        }
        assert!(back, "not put back: now {:?}, was {before:#x}", class_now());
        assert!(sh.snapshot().holding.is_empty());

        sh.quit.store(true, Ordering::Release);
        sh.wake.notify_one();
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Memory is the short resource: the loop unloads the idle model and says why. When memory is fine (only the CPU is
    /// "strained") no model is touched, and with `shed = false` none is either.
    #[tokio::test]
    async fn the_loop_unloads_an_idle_model_only_when_memory_is_what_is_short() {
        async fn run_for(patch: Value, secs: u64) -> (Vec<String>, Vec<Event>) {
            let home = crate::testing::temp_home();
            let hub = crate::testing::hub(&home, &["alpha"]);
            hub.engine.adopt("idle-one", 1, "k", None);
            let sh = Shared::new(home.to_path_buf());
            let mut p = json!({"tick_ms": 250, "rise_after": 1, "fall_after": 1, "watchdog_url": "", "shed_idle_s": 0});
            p.as_object_mut().unwrap().extend(patch.as_object().unwrap().clone());
            sh.set_config(&p).unwrap();
            let task = tokio::spawn(run(sh.clone(), hub.engine.clone()));
            for _ in 0..secs * 4 {
                tokio::time::sleep(Duration::from_millis(250)).await;
                if hub.engine.loaded().is_empty() {
                    break;
                }
            }
            let (left, recent) = (
                hub.engine.loaded().iter().map(|i| i.model.clone()).collect(),
                sh.snapshot().recent,
            );
            sh.quit.store(true, Ordering::Release);
            sh.wake.notify_one();
            let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
            (left, recent)
        }
        let short = json!({"thresholds": {"ram_free_busy_pct": 99.0, "ram_free_strained_pct": 98.0, "ram_free_critical_pct": 97.0}});
        let (left, recent) = run_for(short.clone(), 15).await;
        assert!(left.is_empty(), "the idle model was not unloaded: {left:?}");
        let note = recent
            .iter()
            .find(|e| e.name == "idle-one")
            .expect("the unload is on record");
        assert!(
            note.what.contains("unloaded") && note.what.contains("free"),
            "{}",
            note.what
        );

        // the same pressure, but the owner turned shedding off
        let mut off = short;
        off["shed"] = json!(false);
        let (left, _) = run_for(off, 4).await;
        assert_eq!(left, ["idle-one"], "shed = false keeps the model");

        // the CPU is the short resource, not memory
        let cpu = json!({"thresholds": {"cpu_busy_pct": 0.0, "cpu_strained_pct": 0.0, "cpu_critical_pct": 0.0}});
        let (left, _) = run_for(cpu, 4).await;
        assert_eq!(left, ["idle-one"], "CPU pressure never costs a model its place");
    }

    #[tokio::test]
    async fn the_loop_makes_a_status_the_ops_can_show() {
        let home = crate::testing::temp_home();
        let hub = crate::testing::hub(&home, &["alpha"]);
        let sh = Shared::new(home.to_path_buf());
        sh.set_config(&json!({"tick_ms": 250, "watchdog_url": ""})).unwrap();
        let task = tokio::spawn(run(sh.clone(), hub.engine.clone()));
        for _ in 0..40 {
            if sh.snapshot().looks >= 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let st = sh.status_json();
        assert!(st["looks"].as_u64().unwrap() >= 3, "{st}");
        assert!(st["level"].is_string() && st["mode"] == "balanced");
        assert_eq!(st["watchdog"]["state"], "off");
        assert!(st["signals"]["ram_total_mib"].as_u64().unwrap() > 0);
        assert!(st["waiting_jobs"].as_array().unwrap().is_empty());
        sh.quit.store(true, Ordering::Release);
        sh.wake.notify_one();
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    }
}

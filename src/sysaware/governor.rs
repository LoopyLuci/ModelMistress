//! Applies the policy to the hub's processes, and undoes it.
//!
//! The governor keeps one record per process it has changed, naming what the process was before. Each tick it asks the
//! policy what every process should be held to at the current level and moves each one there, and only there:
//!
//! - it only ever lowers: a process already at or below the cap is left alone;
//! - it puts back exactly what it changed, when the level falls, when the mode is switched off, and at shutdown;
//! - it does not fight: if another program changes a priority the governor set, the governor lets go of that process;
//! - a process is named by pid and start time, so a pid Windows reused is a different process and is never touched;
//! - a pause is bounded (`max_pause_s`) and is never applied to a model server.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::actuate::{class_of, prio_of, Actuator};
use super::level::Level;
use super::policy::{desired, Config, Desired, Kind};

/// A process of the hub's, as the sampler saw it this tick.
#[derive(Clone, Debug)]
pub struct Target {
    pub pid: u32,
    pub started_s: u64,
    pub name: String,
    pub kind: Kind,
    pub pausable: bool,
}

/// Something the governor did, for `system.status` and the log.
#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub at: u64,
    pub pid: u32,
    pub name: String,
    pub what: String,
}

/// A process the governor is holding down now, for `system.status`.
#[derive(Clone, Debug, Serialize)]
pub struct Row {
    pub pid: u32,
    pub name: String,
    pub kind: Kind,
    /// The class it was lowered from and to, when the governor changed it.
    pub from: Option<&'static str>,
    pub to: Option<&'static str>,
    pub efficiency_mode: bool,
    pub paused_s: Option<u64>,
}

const DENIED_CLASS: u8 = 1;
const DENIED_ECO: u8 = 2;
const DENIED_PAUSE: u8 = 4;
const KEEP_EVENTS: usize = 60;

struct Rec {
    name: String,
    kind: Kind,
    pausable: bool,
    /// The class before the governor lowered it, and the class it set.
    orig_class: Option<u32>,
    set_class: Option<u32>,
    /// Set when the governor turned efficiency mode on (it was off).
    eco: bool,
    paused_at: Option<Instant>,
    /// The longest pause was reached at this level; no new pause until the level leaves Critical.
    pause_spent: bool,
    /// Changes the system refused; not tried again until the level moves.
    denied: u8,
}

impl Rec {
    fn holds_nothing(&self) -> bool {
        self.orig_class.is_none() && !self.eco && self.paused_at.is_none()
    }
}

pub struct Governor {
    act: Arc<dyn Actuator>,
    recs: HashMap<(u32, u64), Rec>,
    events: VecDeque<Event>,
    critical_since: Option<Instant>,
    last_level: Level,
    /// Changes made since the start (for the status).
    pub changes: u64,
}

fn now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Governor {
    pub fn new(act: Arc<dyn Actuator>) -> Self {
        Self {
            act,
            recs: HashMap::new(),
            events: VecDeque::new(),
            critical_since: None,
            last_level: Level::Calm,
            changes: 0,
        }
    }

    pub fn active(&self) -> bool {
        self.act.active()
    }

    pub fn events(&self) -> Vec<Event> {
        self.events.iter().rev().cloned().collect()
    }

    fn note(&mut self, pid: u32, name: &str, what: impl Into<String>) {
        if self.events.len() >= KEEP_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(Event {
            at: now_s(),
            pid,
            name: name.to_string(),
            what: what.into(),
        });
    }

    /// The processes held down now.
    pub fn rows(&self) -> Vec<Row> {
        let name = |c: u32| prio_of(c).name();
        let mut v: Vec<Row> = self
            .recs
            .iter()
            .filter(|(_, r)| !r.holds_nothing())
            .map(|((pid, _), r)| Row {
                pid: *pid,
                name: r.name.clone(),
                kind: r.kind,
                from: r.orig_class.map(name),
                to: r.set_class.map(name),
                efficiency_mode: r.eco,
                paused_s: r.paused_at.map(|t| t.elapsed().as_secs()),
            })
            .collect();
        v.sort_by_key(|r| r.pid);
        v
    }

    pub fn paused(&self) -> usize {
        self.recs.values().filter(|r| r.paused_at.is_some()).count()
    }

    /// One tick: move every target to what the policy wants at `level`.
    pub fn tick(&mut self, cfg: &Config, level: Level, targets: &[Target], now: Instant) {
        if level != self.last_level {
            // a change the system refused may work at another level (or be wanted no longer)
            for r in self.recs.values_mut() {
                r.denied = 0;
            }
            self.last_level = level;
        }
        match level {
            Level::Critical => {
                self.critical_since.get_or_insert(now);
            }
            _ => {
                self.critical_since = None;
                for r in self.recs.values_mut() {
                    r.pause_spent = false;
                }
            }
        }
        let due = self
            .critical_since
            .is_some_and(|t| now.duration_since(t) >= Duration::from_secs(cfg.pause_after_s));

        // a process the sampler no longer lists may still be alive and paused (a missed look): it is let go, not lost
        let listed: HashSet<(u32, u64)> = targets.iter().map(|t| (t.pid, t.started_s)).collect();
        let mut work: Vec<(Target, bool)> = targets.iter().cloned().map(|t| (t, false)).collect();
        let unlisted: Vec<(u32, u64)> = self.recs.keys().filter(|k| !listed.contains(k)).copied().collect();
        for key in unlisted {
            let rec = &self.recs[&key];
            if self.act.state(key.0, key.1).is_some() {
                work.push((
                    Target {
                        pid: key.0,
                        started_s: key.1,
                        name: rec.name.clone(),
                        kind: rec.kind,
                        pausable: rec.pausable,
                    },
                    true,
                ));
            } else {
                self.recs.remove(&key); // it exited: nothing to put back
            }
        }

        for (t, restore_only) in work {
            self.one(cfg, level, &t, due, restore_only, now);
        }
    }

    fn one(&mut self, cfg: &Config, level: Level, t: &Target, due: bool, restore_only: bool, now: Instant) {
        let key = (t.pid, t.started_s);
        let Some(cur) = self.act.state(t.pid, t.started_s) else {
            self.recs.remove(&key);
            return;
        };
        let rec = self.recs.entry(key).or_insert_with(|| Rec {
            name: t.name.clone(),
            kind: t.kind,
            pausable: t.pausable,
            orig_class: None,
            set_class: None,
            eco: false,
            paused_at: None,
            pause_spent: false,
            denied: 0,
        });
        if let Some(at) = rec.paused_at {
            if now.duration_since(at) >= Duration::from_secs(cfg.max_pause_s) {
                rec.pause_spent = true;
            }
        }
        let pause_due = due && !rec.pause_spent;
        let d = if restore_only {
            Desired::default()
        } else {
            desired(cfg, level, t.kind, t.pausable, pause_due)
        };
        let mut notes: Vec<String> = vec![];
        let (act, name) = (self.act.clone(), t.name.clone());
        let (pid, started) = key;

        // ---- pause and resume
        if d.pause && rec.paused_at.is_none() && rec.denied & DENIED_PAUSE == 0 {
            match act.suspend(pid, started) {
                Ok(()) => {
                    rec.paused_at = Some(now);
                    notes.push(format!(
                        "paused: the machine has been critical for {}s",
                        cfg.pause_after_s
                    ));
                }
                Err(e) => {
                    rec.denied |= DENIED_PAUSE;
                    notes.push(format!("could not pause: {e}"));
                }
            }
        } else if !d.pause && rec.paused_at.is_some() {
            match act.resume(pid, started) {
                Ok(()) => {
                    let secs = rec
                        .paused_at
                        .take()
                        .map(|a| now.duration_since(a).as_secs())
                        .unwrap_or(0);
                    notes.push(format!("resumed after {secs}s"));
                }
                Err(e) => notes.push(format!("could not resume (will try again): {e}")),
            }
        }

        // ---- priority class: lower only, put back exactly
        match rec.set_class {
            None => {
                if let Some(cap) = d.cap {
                    if prio_of(cur.class) > cap && rec.denied & DENIED_CLASS == 0 {
                        let to = class_of(cap);
                        match act.set_class(pid, started, to) {
                            Ok(()) => {
                                rec.orig_class = Some(cur.class);
                                rec.set_class = Some(to);
                                notes.push(format!("priority {} to {}", prio_of(cur.class).name(), cap.name()));
                            }
                            Err(e) => {
                                rec.denied |= DENIED_CLASS;
                                notes.push(format!("could not lower the priority: {e}"));
                            }
                        }
                    }
                }
            }
            Some(applied) if cur.class != applied => {
                // someone else set it since: they win, the governor lets go of it
                rec.orig_class = None;
                rec.set_class = None;
                notes.push("priority changed by another program; left alone".into());
            }
            Some(applied) => {
                let orig = rec.orig_class.unwrap_or(applied);
                let want = d.cap.filter(|c| prio_of(orig) > *c).map(class_of);
                match want {
                    Some(c) if c == applied => {}
                    Some(c) => match act.set_class(pid, started, c) {
                        Ok(()) => {
                            notes.push(format!("priority {} to {}", prio_of(applied).name(), prio_of(c).name()));
                            rec.set_class = Some(c);
                        }
                        Err(e) => notes.push(format!("could not change the priority: {e}")),
                    },
                    None => match act.set_class(pid, started, orig) {
                        Ok(()) => {
                            notes.push(format!("priority put back to {}", prio_of(orig).name()));
                            rec.orig_class = None;
                            rec.set_class = None;
                        }
                        Err(e) => notes.push(format!("could not put the priority back (will try again): {e}")),
                    },
                }
            }
        }

        // ---- efficiency mode
        if d.eco && !rec.eco && !cur.eco && rec.denied & DENIED_ECO == 0 {
            match act.set_eco(pid, started, true) {
                Ok(()) => {
                    rec.eco = true;
                    notes.push("efficiency mode on".into());
                }
                Err(e) => {
                    rec.denied |= DENIED_ECO;
                    notes.push(format!("could not set efficiency mode: {e}"));
                }
            }
        } else if rec.eco && !d.eco {
            match act.set_eco(pid, started, false) {
                Ok(()) => {
                    rec.eco = false;
                    notes.push("efficiency mode off".into());
                }
                Err(e) => notes.push(format!("could not clear efficiency mode (will try again): {e}")),
            }
        }
        // (the record, not a read-back, says whether efficiency mode is ours: Windows' read-back of it is not reliable)

        // a record that only remembers a refusal stays (so the refusal is not repeated every tick) until the process exits
        if rec.holds_nothing() && (rec.denied == 0 || restore_only) {
            self.recs.remove(&key);
        }
        for n in notes {
            if !n.starts_with("could not") {
                self.changes += 1;
            }
            self.note(pid, &name, n);
        }
    }

    /// Puts back everything the governor changed and resumes what it paused. Returns how many processes it let go.
    pub fn release_all(&mut self) -> usize {
        let keys: Vec<(u32, u64)> = self.recs.keys().copied().collect();
        let mut n = 0;
        for key in keys {
            let (pid, started) = key;
            if self.act.state(pid, started).is_none() {
                self.recs.remove(&key); // it exited: nothing to put back
                continue;
            }
            let Some(rec) = self.recs.get_mut(&key) else { continue };
            let name = rec.name.clone();
            let mut notes: Vec<String> = vec![];
            // what fails to be put back stays recorded, so the next release tries it again
            if rec.paused_at.is_some() && self.act.resume(pid, started).is_ok() {
                rec.paused_at = None;
                notes.push("resumed (released)".into());
            }
            if let (Some(orig), Some(set)) = (rec.orig_class, rec.set_class) {
                match self.act.state(pid, started) {
                    // still what the governor set: put it back
                    Some(s) if s.class == set => {
                        if self.act.set_class(pid, started, orig).is_ok() {
                            notes.push(format!("priority put back to {} (released)", prio_of(orig).name()));
                            rec.orig_class = None;
                            rec.set_class = None;
                        }
                    }
                    // someone else changed it: not ours to put back
                    _ => {
                        rec.orig_class = None;
                        rec.set_class = None;
                    }
                }
            }
            if rec.eco && self.act.set_eco(pid, started, false).is_ok() {
                rec.eco = false;
                notes.push("efficiency mode off (released)".into());
            }
            if rec.holds_nothing() {
                self.recs.remove(&key);
                n += 1;
            }
            for what in notes {
                self.note(pid, &name, what);
            }
        }
        self.critical_since = None;
        n
    }
}

#[cfg(test)]
mod tests {
    use super::super::actuate::{mock::Mock, ABOVE_NORMAL, BELOW_NORMAL, IDLE, NORMAL};
    use super::super::policy::Mode;
    use super::*;

    const T0: u64 = 1_000;

    fn setup() -> (Arc<Mock>, Governor, Config) {
        let m = Arc::new(Mock::default());
        let g = Governor::new(m.clone());
        (
            m,
            g,
            Config {
                pause_after_s: 20,
                max_pause_s: 60,
                ..Config::default()
            },
        )
    }

    fn worker(pid: u32) -> Target {
        Target {
            pid,
            started_s: T0,
            name: "python.exe".into(),
            kind: Kind::Work,
            pausable: true,
        }
    }
    fn server(pid: u32) -> Target {
        Target {
            pid,
            started_s: T0,
            name: "llama-server.exe".into(),
            kind: Kind::Serving,
            pausable: false,
        }
    }

    #[test]
    fn a_worker_runs_below_normal_from_the_start_and_a_server_is_left_alone_when_calm() {
        let (m, mut g, cfg) = setup();
        m.add(10, T0, NORMAL);
        m.add(11, T0, NORMAL);
        g.tick(&cfg, Level::Calm, &[worker(10), server(11)], Instant::now());
        assert_eq!(m.class(10), BELOW_NORMAL);
        assert_eq!(
            m.class(11),
            NORMAL,
            "the server that answers chat is not lowered when calm"
        );
        assert_eq!(g.rows().len(), 1);
    }

    #[test]
    fn strain_lowers_everything_further_and_calm_puts_it_back() {
        let (m, mut g, cfg) = setup();
        m.add(10, T0, NORMAL);
        m.add(11, T0, NORMAL);
        let ts = [worker(10), server(11)];
        let now = Instant::now();
        g.tick(&cfg, Level::Strained, &ts, now);
        assert_eq!(m.class(10), IDLE);
        assert!(m.eco(10), "a worker goes to efficiency mode when strained");
        assert_eq!(m.class(11), BELOW_NORMAL);
        assert!(!m.eco(11) && !m.paused(10) && !m.paused(11));

        g.tick(&cfg, Level::Calm, &ts, now + Duration::from_secs(60));
        assert_eq!(m.class(11), NORMAL, "the server is put back exactly");
        assert!(!m.eco(10), "efficiency mode is off again");
        assert_eq!(
            m.class(10),
            BELOW_NORMAL,
            "a worker settles at the calm cap, not at normal"
        );
        assert_eq!(g.rows().len(), 1);
    }

    #[test]
    fn it_never_raises_and_never_touches_what_is_already_low_enough() {
        let (m, mut g, cfg) = setup();
        m.add(10, T0, IDLE);
        m.add(11, T0, BELOW_NORMAL);
        let ts = [worker(10), server(11)];
        for l in [Level::Calm, Level::Strained, Level::Critical, Level::Calm] {
            g.tick(&cfg, l, &ts, Instant::now());
        }
        assert_eq!(m.class(10), IDLE);
        assert_eq!(m.class(11), BELOW_NORMAL);
        assert!(
            m.log.lock().iter().all(|l| !l.starts_with("class")),
            "no priority change was made: {:?}",
            m.log.lock()
        );
    }

    #[test]
    fn a_priority_above_normal_is_lowered_and_put_back_to_what_it_was() {
        let (m, mut g, cfg) = setup();
        m.add(11, T0, ABOVE_NORMAL);
        let ts = [server(11)];
        g.tick(&cfg, Level::Strained, &ts, Instant::now());
        assert_eq!(m.class(11), BELOW_NORMAL);
        g.tick(&cfg, Level::Calm, &ts, Instant::now());
        assert_eq!(m.class(11), ABOVE_NORMAL);
    }

    #[test]
    fn a_steady_level_makes_no_further_changes() {
        let (m, mut g, cfg) = setup();
        m.add(10, T0, NORMAL);
        let ts = [worker(10)];
        let now = Instant::now();
        g.tick(&cfg, Level::Strained, &ts, now);
        let n = m.changes();
        for i in 1..10 {
            g.tick(&cfg, Level::Strained, &ts, now + Duration::from_secs(i));
        }
        assert_eq!(m.changes(), n);
    }

    #[test]
    fn a_worker_is_paused_only_after_the_machine_has_been_critical_for_a_while_and_resumed_when_it_eases() {
        let (m, mut g, cfg) = setup();
        m.add(10, T0, NORMAL);
        m.add(11, T0, NORMAL);
        let ts = [worker(10), server(11)];
        let now = Instant::now();
        g.tick(&cfg, Level::Critical, &ts, now);
        assert!(!m.paused(10), "not at once");
        g.tick(&cfg, Level::Critical, &ts, now + Duration::from_secs(19));
        assert!(!m.paused(10));
        g.tick(&cfg, Level::Critical, &ts, now + Duration::from_secs(21));
        assert!(m.paused(10), "after pause_after_s");
        assert!(!m.paused(11), "a model server is never paused");
        assert_eq!(g.paused(), 1);
        g.tick(&cfg, Level::Strained, &ts, now + Duration::from_secs(30));
        assert!(!m.paused(10), "it runs again as soon as the level drops");
        assert_eq!(g.paused(), 0);
    }

    #[test]
    fn a_pause_is_bounded_and_does_not_come_back_while_the_level_stays_critical() {
        let (m, mut g, cfg) = setup(); // max_pause_s = 60
        m.add(10, T0, NORMAL);
        let ts = [worker(10)];
        let now = Instant::now();
        g.tick(&cfg, Level::Critical, &ts, now);
        g.tick(&cfg, Level::Critical, &ts, now + Duration::from_secs(21));
        assert!(m.paused(10));
        g.tick(&cfg, Level::Critical, &ts, now + Duration::from_secs(21 + 61));
        assert!(!m.paused(10), "resumed after max_pause_s");
        g.tick(&cfg, Level::Critical, &ts, now + Duration::from_secs(21 + 70));
        assert!(!m.paused(10), "and not paused again at the same level");
        // after it eases and gets bad again, a new pause is allowed
        g.tick(&cfg, Level::Calm, &ts, now + Duration::from_secs(200));
        g.tick(&cfg, Level::Critical, &ts, now + Duration::from_secs(201));
        g.tick(&cfg, Level::Critical, &ts, now + Duration::from_secs(230));
        assert!(m.paused(10));
    }

    #[test]
    fn the_off_mode_holds_nothing_and_lets_go_of_what_was_held() {
        let (m, mut g, mut cfg) = setup();
        m.add(10, T0, NORMAL);
        let ts = [worker(10)];
        g.tick(&cfg, Level::Strained, &ts, Instant::now());
        assert_eq!(m.class(10), IDLE);
        cfg.mode = Mode::Off;
        g.tick(&cfg, Level::Strained, &ts, Instant::now());
        assert_eq!(m.class(10), NORMAL);
        assert!(!m.eco(10));
        assert!(g.rows().is_empty());
    }

    #[test]
    fn another_program_changing_a_priority_is_respected() {
        let (m, mut g, cfg) = setup();
        m.add(10, T0, NORMAL);
        let ts = [server(10)];
        g.tick(&cfg, Level::Strained, &ts, Instant::now());
        assert_eq!(m.class(10), BELOW_NORMAL);
        // the user (or Task Manager) sets it to high
        m.procs.lock().get_mut(&10).unwrap().1.class = 0x80;
        g.tick(&cfg, Level::Strained, &ts, Instant::now());
        g.tick(&cfg, Level::Calm, &ts, Instant::now());
        assert_eq!(
            m.class(10),
            0x80,
            "neither fought over nor 'restored' over the user's choice"
        );
        assert!(g.rows().is_empty());
        assert!(g.events().iter().any(|e| e.what.contains("another program")));
    }

    #[test]
    fn a_reused_pid_is_a_different_process_and_is_not_touched() {
        let (m, mut g, cfg) = setup();
        m.add(10, T0, NORMAL);
        g.tick(&cfg, Level::Strained, &[worker(10)], Instant::now());
        assert_eq!(m.class(10), IDLE);
        // the worker exits and Windows gives the pid to a program that started later
        m.remove(10);
        m.add(10, T0 + 500, NORMAL);
        g.tick(&cfg, Level::Strained, &[], Instant::now());
        assert_eq!(m.class(10), NORMAL, "the new owner of the pid is untouched");
        assert!(g.rows().is_empty(), "and the old record is gone");
    }

    #[test]
    fn a_refusal_is_reported_once_and_not_retried_every_tick() {
        let (m, mut g, cfg) = setup();
        m.add(10, T0, NORMAL);
        *m.deny.lock() = true;
        let ts = [worker(10)];
        for i in 0..5 {
            g.tick(&cfg, Level::Strained, &ts, Instant::now() + Duration::from_secs(i));
        }
        let refused = g.events().iter().filter(|e| e.what.starts_with("could not")).count();
        assert_eq!(
            refused,
            2,
            "one for the priority and one for efficiency mode, once each: {:?}",
            g.events()
        );
        assert_eq!(m.class(10), NORMAL);
    }

    #[test]
    fn a_paused_process_the_sampler_lost_sight_of_is_still_resumed() {
        let (m, mut g, cfg) = setup();
        m.add(10, T0, NORMAL);
        let ts = [worker(10)];
        let now = Instant::now();
        g.tick(&cfg, Level::Critical, &ts, now);
        g.tick(&cfg, Level::Critical, &ts, now + Duration::from_secs(25));
        assert!(m.paused(10));
        // a look that missed it (the process is alive)
        g.tick(&cfg, Level::Critical, &[], now + Duration::from_secs(27));
        assert!(!m.paused(10), "it is let go, not left suspended for good");
        assert_eq!(m.class(10), NORMAL);
    }

    #[test]
    fn a_process_that_exited_while_held_is_forgotten_without_error() {
        let (m, mut g, cfg) = setup();
        m.add(10, T0, NORMAL);
        g.tick(&cfg, Level::Strained, &[worker(10)], Instant::now());
        m.remove(10);
        g.tick(&cfg, Level::Strained, &[], Instant::now());
        assert!(g.rows().is_empty());
    }

    #[test]
    fn release_all_puts_back_every_class_and_resumes_every_pause() {
        let (m, mut g, cfg) = setup();
        m.add(10, T0, NORMAL);
        m.add(11, T0, ABOVE_NORMAL);
        let ts = [worker(10), server(11)];
        let now = Instant::now();
        g.tick(&cfg, Level::Critical, &ts, now);
        g.tick(&cfg, Level::Critical, &ts, now + Duration::from_secs(25));
        assert!(m.paused(10) && m.eco(10));
        assert_eq!(g.release_all(), 2);
        assert!(!m.paused(10) && !m.eco(10));
        assert_eq!((m.class(10), m.class(11)), (NORMAL, ABOVE_NORMAL));
        assert!(g.rows().is_empty());
        assert_eq!(g.release_all(), 0);
    }

    #[test]
    fn a_failed_resume_is_kept_so_it_is_tried_again() {
        let (m, mut g, cfg) = setup();
        m.add(10, T0, NORMAL);
        let ts = [worker(10)];
        let now = Instant::now();
        g.tick(&cfg, Level::Critical, &ts, now);
        g.tick(&cfg, Level::Critical, &ts, now + Duration::from_secs(25));
        *m.deny.lock() = true;
        g.release_all();
        assert_eq!(
            g.paused(),
            1,
            "the record of the pause is not dropped when the resume fails"
        );
        *m.deny.lock() = false;
        g.release_all();
        assert!(!m.paused(10));
        assert_eq!(g.paused(), 0);
    }
}

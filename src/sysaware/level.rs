//! The pure core of system awareness: what the readings say (a level, and the reasons for it), and how a level changes.
//!
//! Nothing here reads the machine or changes anything, so every rule is testable with made-up readings.
//!
//! **Level.** `Calm` nothing else wants the machine. `Busy` another program is working hard: heavy work of ours keeps
//! running but at a lower priority. `Strained` the machine is short of something (CPU, memory, commit, video memory,
//! heat) or the user is in a full-screen game or a presentation: our own work gives way and new heavy jobs wait.
//! `Critical` the machine is about to fail for somebody (out of commit or memory, overheated): our background work is
//! paused.
//!
//! **Foreign load.** The CPU that counts is the CPU of *other* programs: the machine's total less the share of the hub and
//! the processes it started. A model generating at 100% on an otherwise idle PC is not pressure; the same model while a
//! game needs the cores is.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    #[default]
    Calm,
    Busy,
    Strained,
    Critical,
}

impl Level {
    pub fn name(self) -> &'static str {
        match self {
            Level::Calm => "calm",
            Level::Busy => "busy",
            Level::Strained => "strained",
            Level::Critical => "critical",
        }
    }
    /// One level quieter (Calm stays Calm).
    pub fn lower(self) -> Level {
        match self {
            Level::Critical => Level::Strained,
            Level::Strained => Level::Busy,
            _ => Level::Calm,
        }
    }
    /// One level louder (Critical stays Critical).
    pub fn raise(self) -> Level {
        match self {
            Level::Calm => Level::Busy,
            Level::Busy => Level::Strained,
            _ => Level::Critical,
        }
    }
}

/// What the user is doing at the screen, when the system can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UserState {
    /// A full-screen Direct3D program (a game) is running.
    FullScreenGame,
    /// A full-screen program, or Windows presentation settings.
    FullScreen,
    Presenting,
    /// The screen is locked or a screen saver runs.
    Away,
    /// An ordinary desktop.
    Normal,
}

impl UserState {
    pub fn wants_the_machine(self) -> bool {
        matches!(
            self,
            UserState::FullScreenGame | UserState::FullScreen | UserState::Presenting
        )
    }
}

/// One look at the machine, from the native probe and, when it runs, Watchdog's pressure feed. A field is None when
/// nothing measures it here.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Signals {
    /// The machine's CPU use, 0 to 100.
    pub cpu_total_pct: Option<f64>,
    /// The share of that used by the hub and the processes it started.
    pub cpu_mm_pct: f64,
    /// `cpu_total_pct` less `cpu_mm_pct`: what other programs use.
    pub cpu_foreign_pct: Option<f64>,
    pub cpu_busiest_core_pct: Option<f64>,
    pub cpu_temp_c: Option<f64>,
    pub ram_total_mib: u64,
    pub ram_avail_mib: u64,
    /// Windows: memory the system will still commit (see `machine`).
    pub commit_headroom_mib: Option<u64>,
    pub vram_total_mib: Option<u64>,
    pub vram_free_mib: Option<u64>,
    pub gpu_util_pct: Option<f64>,
    pub gpu_temp_c: Option<f64>,
    pub disk_busy_pct: Option<f64>,
    pub user: Option<UserState>,
    /// The program the user is working in (name only), when Watchdog tells.
    pub foreground: Option<String>,
    /// Watchdog's anomaly score and flag.
    pub anomaly: Option<(f64, bool)>,
    pub on_battery: bool,
    /// The busiest programs that are not ours: (name, cpu %, gpu %), busiest first.
    pub offenders: Vec<(String, f64, f64)>,
    /// Whether Watchdog's feed contributed to this reading.
    pub watchdog: bool,
}

/// Where each level starts. A field a reading lacks is never judged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Thresholds {
    /// Other programs' CPU use, percent of the whole machine.
    pub cpu_busy_pct: f64,
    pub cpu_strained_pct: f64,
    pub cpu_critical_pct: f64,
    /// Free physical memory, percent of the total (below these).
    pub ram_free_busy_pct: f64,
    pub ram_free_strained_pct: f64,
    pub ram_free_critical_pct: f64,
    /// Windows commit headroom, MiB (below these).
    pub commit_busy_mib: u64,
    pub commit_strained_mib: u64,
    pub commit_critical_mib: u64,
    /// Free video memory, MiB (below these). `Strained` is also reached when less than the hub's own `vram_reserve_mib`
    /// is free, whichever is larger.
    pub vram_busy_mib: u64,
    pub vram_strained_mib: u64,
    pub vram_critical_mib: u64,
    pub cpu_temp_strained_c: f64,
    pub cpu_temp_critical_c: f64,
    pub gpu_temp_strained_c: f64,
    pub gpu_temp_critical_c: f64,
    pub gpu_util_busy_pct: f64,
    pub disk_busy_pct: f64,
    /// A full-screen game or a presentation makes the level at least Strained.
    pub yield_to_fullscreen: bool,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            cpu_busy_pct: 60.0,
            cpu_strained_pct: 80.0,
            cpu_critical_pct: 93.0,
            ram_free_busy_pct: 15.0,
            ram_free_strained_pct: 8.0,
            ram_free_critical_pct: 4.0,
            commit_busy_mib: 12 * 1024,
            commit_strained_mib: 6 * 1024,
            commit_critical_mib: 3 * 1024,
            vram_busy_mib: 4096,
            vram_strained_mib: 2048,
            vram_critical_mib: 512,
            cpu_temp_strained_c: 88.0,
            cpu_temp_critical_c: 95.0,
            gpu_temp_strained_c: 92.0,
            gpu_temp_critical_c: 102.0,
            gpu_util_busy_pct: 90.0,
            disk_busy_pct: 90.0,
            yield_to_fullscreen: true,
        }
    }
}

/// One thing that raised the level.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Reason {
    pub resource: &'static str,
    pub level: Level,
    pub detail: String,
}

fn rise(out: &mut Vec<Reason>, resource: &'static str, level: Level, detail: String) {
    out.push(Reason {
        resource,
        level,
        detail,
    });
}

/// A value against three ascending limits where bigger is worse: (the level it reaches, the limit it passed).
fn high(v: f64, busy: f64, strained: f64, critical: f64) -> Option<(Level, f64)> {
    if v >= critical {
        Some((Level::Critical, critical))
    } else if v >= strained {
        Some((Level::Strained, strained))
    } else if v >= busy {
        Some((Level::Busy, busy))
    } else {
        None
    }
}

/// A value against three descending limits where smaller is worse.
fn low(v: f64, busy: f64, strained: f64, critical: f64) -> Option<(Level, f64)> {
    if v < critical {
        Some((Level::Critical, critical))
    } else if v < strained {
        Some((Level::Strained, strained))
    } else if v < busy {
        Some((Level::Busy, busy))
    } else {
        None
    }
}

/// Every reason the readings give for a level above Calm. `vram_reserve_mib` is the hub's own setting: video memory
/// below it is a reason by itself.
pub fn assess(s: &Signals, t: &Thresholds, vram_reserve_mib: u64) -> Vec<Reason> {
    let mut out = vec![];
    if let Some(f) = s.cpu_foreign_pct {
        if let Some((l, at)) = high(f, t.cpu_busy_pct, t.cpu_strained_pct, t.cpu_critical_pct) {
            let who = match s.offenders.first() {
                Some((name, cpu, _)) => format!(", mostly {name} ({cpu:.0}%)"),
                None => String::new(),
            };
            rise(
                &mut out,
                "cpu",
                l,
                format!("other programs use {f:.0}% of the CPU (limit {at:.0}%){who}"),
            );
        }
    }
    if s.ram_total_mib > 0 {
        let free = s.ram_avail_mib as f64 * 100.0 / s.ram_total_mib as f64;
        if let Some((l, at)) = low(
            free,
            t.ram_free_busy_pct,
            t.ram_free_strained_pct,
            t.ram_free_critical_pct,
        ) {
            rise(
                &mut out,
                "memory",
                l,
                format!("{} MiB of memory free, {free:.0}% (limit {at:.0}%)", s.ram_avail_mib),
            );
        }
    }
    if let Some(c) = s.commit_headroom_mib {
        if let Some((l, at)) = low(
            c as f64,
            t.commit_busy_mib as f64,
            t.commit_strained_mib as f64,
            t.commit_critical_mib as f64,
        ) {
            rise(
                &mut out,
                "commit",
                l,
                format!("{c} MiB of memory commit left (limit {at:.0} MiB)"),
            );
        }
    }
    if let Some(v) = s.vram_free_mib {
        let strained = t.vram_strained_mib.max(vram_reserve_mib);
        if let Some((l, at)) = low(
            v as f64,
            t.vram_busy_mib.max(strained) as f64,
            strained as f64,
            t.vram_critical_mib as f64,
        ) {
            rise(
                &mut out,
                "vram",
                l,
                format!("{v} MiB of video memory free (limit {at:.0} MiB)"),
            );
        }
    }
    if let Some(c) = s.cpu_temp_c {
        if let Some((l, at)) = high(c, f64::INFINITY, t.cpu_temp_strained_c, t.cpu_temp_critical_c) {
            rise(
                &mut out,
                "cpu_temp",
                l,
                format!("the CPU is at {c:.0} C (limit {at:.0} C)"),
            );
        }
    }
    if let Some(c) = s.gpu_temp_c {
        if let Some((l, at)) = high(c, f64::INFINITY, t.gpu_temp_strained_c, t.gpu_temp_critical_c) {
            rise(
                &mut out,
                "gpu_temp",
                l,
                format!("the GPU is at {c:.0} C (limit {at:.0} C)"),
            );
        }
    }
    if let Some(g) = s.gpu_util_pct {
        if g >= t.gpu_util_busy_pct {
            rise(&mut out, "gpu", Level::Busy, format!("the GPU is {g:.0}% busy"));
        }
    }
    if let Some(d) = s.disk_busy_pct {
        if d >= t.disk_busy_pct {
            rise(&mut out, "disk", Level::Busy, format!("a drive is {d:.0}% busy"));
        }
    }
    if t.yield_to_fullscreen {
        if let Some(u) = s.user.filter(|u| u.wants_the_machine()) {
            let what = match u {
                UserState::FullScreenGame => "a full-screen game is running",
                UserState::Presenting => "the screen is presenting",
                _ => "a full-screen program is running",
            };
            rise(&mut out, "user", Level::Strained, what.to_string());
        }
    }
    out
}

/// The highest level among the reasons (Calm for none).
pub fn level_of(reasons: &[Reason]) -> Level {
    reasons.iter().map(|r| r.level).max().unwrap_or(Level::Calm)
}

/// Turns the raw level of each look into a steady one: it rises after `up` looks in a row at a higher level and falls
/// one level after `down` looks in a row at a lower one, so a reading that flickers around a limit changes nothing and
/// a brief spike does not pause the work.
#[derive(Clone, Debug)]
pub struct Tracker {
    level: Level,
    up_needed: u32,
    down_needed: u32,
    up: u32,
    down: u32,
}

impl Tracker {
    pub fn new(up_needed: u32, down_needed: u32) -> Self {
        Self {
            level: Level::Calm,
            up_needed: up_needed.max(1),
            down_needed: down_needed.max(1),
            up: 0,
            down: 0,
        }
    }
    pub fn level(&self) -> Level {
        self.level
    }
    /// Takes one look's raw level and returns the steady level and whether it changed.
    pub fn update(&mut self, raw: Level) -> (Level, bool) {
        let before = self.level;
        if raw > self.level {
            self.down = 0;
            self.up += 1;
            if self.up >= self.up_needed {
                self.level = raw;
                self.up = 0;
            }
        } else if raw < self.level {
            self.up = 0;
            self.down += 1;
            if self.down >= self.down_needed {
                self.level = self.level.lower().max(raw);
                self.down = 0;
            }
        } else {
            self.up = 0;
            self.down = 0;
        }
        (self.level, self.level != before)
    }
    /// Forget everything and go to `level` at once (the governor was switched off, or an override began).
    pub fn reset(&mut self, level: Level) {
        self.level = level;
        self.up = 0;
        self.down = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet() -> Signals {
        Signals {
            cpu_total_pct: Some(10.0),
            cpu_mm_pct: 2.0,
            cpu_foreign_pct: Some(8.0),
            ram_total_mib: 65_536,
            ram_avail_mib: 40_000,
            commit_headroom_mib: Some(60_000),
            vram_free_mib: Some(20_000),
            ..Default::default()
        }
    }

    fn lvl(s: &Signals) -> Level {
        level_of(&assess(s, &Thresholds::default(), 2048))
    }

    #[test]
    fn a_quiet_machine_is_calm_and_a_reading_nobody_took_is_never_judged() {
        assert_eq!(lvl(&quiet()), Level::Calm);
        assert_eq!(lvl(&Signals::default()), Level::Calm);
    }

    #[test]
    fn only_other_programs_cpu_counts() {
        let mut s = quiet();
        // the hub's own model generating at full tilt on an idle machine: no pressure
        s.cpu_total_pct = Some(97.0);
        s.cpu_mm_pct = 94.0;
        s.cpu_foreign_pct = Some(3.0);
        assert_eq!(lvl(&s), Level::Calm);
        // the same total with others using it
        s.cpu_foreign_pct = Some(85.0);
        s.offenders = vec![("game.exe".into(), 70.0, 90.0)];
        let r = assess(&s, &Thresholds::default(), 2048);
        assert_eq!(level_of(&r), Level::Strained);
        assert!(r[0].detail.contains("game.exe"), "{:?}", r[0]);
        s.cpu_foreign_pct = Some(65.0);
        assert_eq!(lvl(&s), Level::Busy);
        s.cpu_foreign_pct = Some(95.0);
        assert_eq!(lvl(&s), Level::Critical);
    }

    #[test]
    fn memory_commit_and_video_memory_each_have_three_limits() {
        let mut s = quiet();
        s.ram_avail_mib = 65_536 * 12 / 100;
        assert_eq!(lvl(&s), Level::Busy);
        s.ram_avail_mib = 65_536 * 6 / 100;
        assert_eq!(lvl(&s), Level::Strained);
        s.ram_avail_mib = 65_536 * 3 / 100;
        assert_eq!(lvl(&s), Level::Critical);
        let mut s = quiet();
        s.commit_headroom_mib = Some(10_000);
        assert_eq!(lvl(&s), Level::Busy);
        s.commit_headroom_mib = Some(5_000);
        assert_eq!(lvl(&s), Level::Strained);
        s.commit_headroom_mib = Some(2_000);
        assert_eq!(lvl(&s), Level::Critical);
        let mut s = quiet();
        s.vram_free_mib = Some(3_000);
        assert_eq!(lvl(&s), Level::Busy);
        s.vram_free_mib = Some(1_500);
        assert_eq!(lvl(&s), Level::Strained);
        s.vram_free_mib = Some(300);
        assert_eq!(lvl(&s), Level::Critical);
    }

    #[test]
    fn less_video_memory_than_the_hubs_own_reserve_is_strain() {
        let mut s = quiet();
        s.vram_free_mib = Some(3_500);
        let big_reserve = assess(&s, &Thresholds::default(), 4_096);
        assert_eq!(level_of(&big_reserve), Level::Strained);
        assert_eq!(lvl(&s), Level::Busy);
    }

    #[test]
    fn heat_is_judged_only_where_it_is_measured() {
        let mut s = quiet();
        s.cpu_temp_c = Some(80.0);
        assert_eq!(lvl(&s), Level::Calm);
        s.cpu_temp_c = Some(90.0);
        assert_eq!(lvl(&s), Level::Strained);
        s.cpu_temp_c = Some(96.0);
        assert_eq!(lvl(&s), Level::Critical);
        let mut s = quiet();
        s.gpu_temp_c = Some(95.0);
        assert_eq!(lvl(&s), Level::Strained);
        s.gpu_temp_c = Some(103.0);
        assert_eq!(lvl(&s), Level::Critical);
    }

    #[test]
    fn a_busy_gpu_or_drive_is_only_busy() {
        let mut s = quiet();
        s.gpu_util_pct = Some(99.0);
        s.disk_busy_pct = Some(99.0);
        let r = assess(&s, &Thresholds::default(), 2048);
        assert_eq!(r.len(), 2);
        assert_eq!(level_of(&r), Level::Busy);
    }

    #[test]
    fn a_full_screen_game_or_a_presentation_yields_unless_told_not_to() {
        let mut s = quiet();
        for u in [UserState::FullScreenGame, UserState::FullScreen, UserState::Presenting] {
            s.user = Some(u);
            assert_eq!(lvl(&s), Level::Strained, "{u:?}");
        }
        for u in [UserState::Normal, UserState::Away] {
            s.user = Some(u);
            assert_eq!(lvl(&s), Level::Calm, "{u:?}");
        }
        s.user = Some(UserState::FullScreenGame);
        let t = Thresholds {
            yield_to_fullscreen: false,
            ..Default::default()
        };
        assert_eq!(level_of(&assess(&s, &t, 2048)), Level::Calm);
    }

    #[test]
    fn the_worst_reason_sets_the_level_and_all_are_listed() {
        let mut s = quiet();
        s.cpu_foreign_pct = Some(65.0);
        s.commit_headroom_mib = Some(2_000);
        let r = assess(&s, &Thresholds::default(), 2048);
        assert_eq!(r.len(), 2);
        assert_eq!(level_of(&r), Level::Critical);
    }

    #[test]
    fn levels_order_and_step() {
        assert!(Level::Calm < Level::Busy && Level::Busy < Level::Strained && Level::Strained < Level::Critical);
        assert_eq!(Level::Critical.lower(), Level::Strained);
        assert_eq!(Level::Calm.lower(), Level::Calm);
        assert_eq!(Level::Calm.raise(), Level::Busy);
        assert_eq!(Level::Critical.raise(), Level::Critical);
    }

    #[test]
    fn a_level_rises_after_two_looks_and_one_look_is_not_enough() {
        let mut t = Tracker::new(2, 4);
        assert_eq!(t.update(Level::Strained), (Level::Calm, false));
        assert_eq!(t.update(Level::Calm), (Level::Calm, false)); // the spike was one look
        assert_eq!(t.update(Level::Strained), (Level::Calm, false));
        assert_eq!(t.update(Level::Strained), (Level::Strained, true));
    }

    #[test]
    fn a_level_falls_one_step_at_a_time_and_slowly() {
        let mut t = Tracker::new(1, 3);
        t.update(Level::Critical);
        assert_eq!(t.level(), Level::Critical);
        for _ in 0..2 {
            assert_eq!(t.update(Level::Calm), (Level::Critical, false));
        }
        assert_eq!(t.update(Level::Calm), (Level::Strained, true));
        for _ in 0..2 {
            t.update(Level::Calm);
        }
        assert_eq!(t.update(Level::Calm).0, Level::Busy);
    }

    #[test]
    fn a_flicker_around_a_limit_changes_nothing() {
        let mut t = Tracker::new(2, 4);
        t.update(Level::Busy);
        t.update(Level::Busy);
        assert_eq!(t.level(), Level::Busy);
        for _ in 0..10 {
            t.update(Level::Calm); // below
            t.update(Level::Busy); // back: resets the fall
            t.update(Level::Busy);
        }
        assert_eq!(t.level(), Level::Busy);
    }

    #[test]
    fn it_never_falls_below_the_raw_level_and_reset_sets_it_at_once() {
        let mut t = Tracker::new(1, 1);
        t.update(Level::Critical);
        assert_eq!(t.update(Level::Strained).0, Level::Strained);
        assert_eq!(t.update(Level::Strained).0, Level::Strained);
        t.reset(Level::Calm);
        assert_eq!(t.level(), Level::Calm);
    }

    #[test]
    fn thresholds_reject_a_misspelt_key_and_keep_defaults_for_the_rest() {
        let t: Thresholds = serde_json::from_value(serde_json::json!({"cpu_busy_pct": 50.0})).unwrap();
        assert_eq!(t.cpu_busy_pct, 50.0);
        assert_eq!(t.cpu_strained_pct, Thresholds::default().cpu_strained_pct);
        assert!(serde_json::from_value::<Thresholds>(serde_json::json!({"cpu_bussy_pct": 50.0})).is_err());
    }
}

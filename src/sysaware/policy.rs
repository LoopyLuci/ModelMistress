//! The settings (`<home>/system.toml`) and the rules that turn a level into what happens to a process.
//!
//! The rules are a table, not code paths: `desired` answers, for a level, a mode and a kind of process, what the process
//! should be held to. The governor applies the answer and later undoes it; nothing else decides.
//!
//! What it may touch: only the hub's own descendants (the model servers and workers it started), and only downward. It
//! lowers a priority class, turns Windows' efficiency mode on, or pauses a process; it never raises a priority above what
//! the process had, and it puts back what it changed.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::level::{Level, Thresholds};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Does nothing: no priorities change, no job waits.
    Off,
    /// Lowers background work (workers), never the model servers that answer chat.
    Gentle,
    /// The default: background work yields at once; model servers yield when the machine is strained.
    #[default]
    Balanced,
    /// Everything yields one level earlier.
    Strict,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "off" => Some(Mode::Off),
            "gentle" => Some(Mode::Gentle),
            "balanced" => Some(Mode::Balanced),
            "strict" => Some(Mode::Strict),
            _ => None,
        }
    }
}

/// Windows priority classes the governor uses, from the lowest. Anything above Normal counts as Normal here: the
/// governor only needs to know how far down it may go.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Prio {
    Idle,
    BelowNormal,
    Normal,
}

impl Prio {
    pub fn name(self) -> &'static str {
        match self {
            Prio::Idle => "idle",
            Prio::BelowNormal => "below_normal",
            Prio::Normal => "normal",
        }
    }
}

/// What a process of ours is, for the rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A model server that answers requests (llama-server and its relatives).
    Serving,
    /// A worker that does a job (Forge training, image and video generation, quantizing, transcoding).
    Work,
    /// Anything else the hub started (a command Misty ran, a shell).
    Other,
}

/// What a process should be held to at a level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Desired {
    /// The highest priority class allowed (None: leave it as it is).
    pub cap: Option<Prio>,
    /// Windows' efficiency mode: lower clock, background scheduling.
    pub eco: bool,
    /// Paused.
    pub pause: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub mode: Mode,
    /// How often the machine is looked at, milliseconds.
    pub tick_ms: u64,
    /// Looks in a row at a higher level before the level rises, and at a lower one before it falls a step.
    pub rise_after: u32,
    pub fall_after: u32,
    /// Where heavy workers run when the machine is calm: `below_normal` (default) or `normal`. At below normal they
    /// still use every idle cycle; they only give way to a program that wants the CPU.
    pub background_priority: String,
    /// Seconds at Critical before background workers are paused, and the longest a pause lasts.
    pub pause_after_s: u64,
    pub max_pause_s: u64,
    /// At Strained or worse because of memory, commit or video memory, unload models idle this long (seconds).
    pub shed: bool,
    pub shed_idle_s: u64,
    /// The longest a heavy job waits for the machine to quiet (0: as long as it takes).
    pub max_wait_s: u64,
    /// Watchdog's pressure feed (empty: do not look for it).
    pub watchdog_url: String,
    /// A reading from Watchdog older than this is ignored (seconds).
    pub watchdog_max_age_s: f64,
    pub thresholds: Thresholds,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::Balanced,
            tick_ms: 2000,
            rise_after: 2,
            fall_after: 5,
            background_priority: "below_normal".into(),
            pause_after_s: 20,
            max_pause_s: 900,
            shed: true,
            shed_idle_s: 120,
            max_wait_s: 0,
            watchdog_url: "http://127.0.0.1:7781".into(),
            watchdog_max_age_s: 15.0,
            thresholds: Thresholds::default(),
        }
    }
}

const FILE: &str = "system.toml";

impl Config {
    /// The file's settings, or the defaults when there is none. A damaged file is an error the caller reports, and the
    /// defaults are used until it is fixed.
    pub fn load(home: &Path) -> Result<Config, String> {
        let p = home.join(FILE);
        let text = match std::fs::read_to_string(&p) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(format!("{}: {e}", p.display())),
        };
        let c: Config = toml::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?;
        c.check()?;
        Ok(c)
    }

    pub fn check(&self) -> Result<(), String> {
        if !(250..=60_000).contains(&self.tick_ms) {
            return Err("tick_ms is 250 to 60000".into());
        }
        if self.rise_after == 0 || self.fall_after == 0 {
            return Err("rise_after and fall_after are at least 1".into());
        }
        if !matches!(self.background_priority.as_str(), "below_normal" | "normal") {
            return Err("background_priority is below_normal or normal".into());
        }
        let t = &self.thresholds;
        let ordered = |a: f64, b: f64, c: f64| a <= b && b <= c;
        if !ordered(t.cpu_busy_pct, t.cpu_strained_pct, t.cpu_critical_pct) {
            return Err("the cpu limits must rise: busy, strained, critical".into());
        }
        if !ordered(t.ram_free_critical_pct, t.ram_free_strained_pct, t.ram_free_busy_pct) {
            return Err("the free-memory limits must fall: busy, strained, critical".into());
        }
        if !(t.commit_critical_mib <= t.commit_strained_mib && t.commit_strained_mib <= t.commit_busy_mib) {
            return Err("the commit limits must fall: busy, strained, critical".into());
        }
        if !(t.vram_critical_mib <= t.vram_strained_mib && t.vram_strained_mib <= t.vram_busy_mib) {
            return Err("the video memory limits must fall: busy, strained, critical".into());
        }
        Ok(())
    }

    /// Writes the file (through a temporary one, so a crash leaves the old one).
    pub fn save(&self, home: &Path) -> Result<(), String> {
        self.check()?;
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        let p = home.join(FILE);
        let tmp = home.join(format!("{FILE}.tmp"));
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))
    }

    /// This config with a JSON merge patch applied (an object's keys replace; a nested object merges). Unknown keys are
    /// refused, so a misspelt setting is an error and not a silent no-op.
    pub fn patched(&self, patch: &Value) -> Result<Config, String> {
        let mut doc = serde_json::to_value(self).map_err(|e| e.to_string())?;
        merge(&mut doc, patch);
        let c: Config = serde_json::from_value(doc).map_err(|e| e.to_string())?;
        c.check()?;
        Ok(c)
    }

    pub fn background_cap(&self) -> Option<Prio> {
        (self.background_priority == "below_normal").then_some(Prio::BelowNormal)
    }
}

fn merge(into: &mut Value, patch: &Value) {
    match (into, patch) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                if v.is_null() {
                    continue;
                }
                merge(a.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (slot, v) => *slot = v.clone(),
    }
}

/// What to hold a process of `kind` to at `level` under `mode`.
///
/// `pausable` is whether this process may be paused at all (a worker that does a job; never a server answering chat).
/// `paused_ok` is whether the pause is due: the level has been Critical for `pause_after_s` and the pause has not run
/// past `max_pause_s`.
pub fn desired(cfg: &Config, level: Level, kind: Kind, pausable: bool, paused_ok: bool) -> Desired {
    let level = match cfg.mode {
        Mode::Off => return Desired::default(),
        Mode::Strict => level.raise(),
        _ => level,
    };
    let calm_cap = cfg.background_cap();
    match kind {
        Kind::Serving => match (cfg.mode, level) {
            (Mode::Gentle, _) => Desired::default(),
            (_, Level::Strained | Level::Critical) => Desired {
                cap: Some(Prio::BelowNormal),
                ..Default::default()
            },
            _ => Desired::default(),
        },
        Kind::Work => match level {
            Level::Calm | Level::Busy => Desired {
                cap: calm_cap,
                ..Default::default()
            },
            Level::Strained => Desired {
                cap: Some(Prio::Idle),
                eco: true,
                pause: false,
            },
            Level::Critical => Desired {
                cap: Some(Prio::Idle),
                eco: true,
                pause: pausable && paused_ok && cfg.mode != Mode::Gentle,
            },
        },
        Kind::Other => match level {
            Level::Calm | Level::Busy => Desired::default(),
            Level::Strained => Desired {
                cap: Some(Prio::BelowNormal),
                ..Default::default()
            },
            Level::Critical => Desired {
                cap: Some(Prio::Idle),
                ..Default::default()
            },
        },
    }
}

/// Which of the hub's descendants is what. `is_server_pid`: the process is a model server the engine loaded.
pub fn kind_of(name: &str, is_server_pid: bool) -> (Kind, bool) {
    let n = name.to_ascii_lowercase();
    let n = n.trim_end_matches(".exe");
    if is_server_pid || n.starts_with("llama-server") || (n.contains("prism") && n.contains("server")) {
        return (Kind::Serving, false);
    }
    const WORKERS: [&str; 8] = [
        "python",
        "pythonw",
        "sd-cli",
        "sd",
        "ffmpeg",
        "llama-quantize",
        "llama-bench",
        "llama-tts",
    ];
    if WORKERS.iter().any(|w| {
        n == *w || (n.starts_with(w) && n[w.len()..].chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-'))
    }) {
        return (Kind::Work, true);
    }
    (Kind::Other, false)
}

/// A process the operating system starts on behalf of the hub's children (the console host of a command, the error
/// reporter of a crash). It does no work of its own, so the governor leaves it alone: lowering a console host only
/// slows the output of the program it serves. Its CPU still counts as the hub's own.
pub fn is_helper(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let n = n.trim_end_matches(".exe");
    matches!(
        n,
        "conhost" | "openconsole" | "werfault" | "wermgr" | "dllhost" | "ctfmon" | "csrss"
    )
}

/// The settings as JSON for `system.config.get`.
pub fn to_json(cfg: &Config) -> Value {
    serde_json::to_value(cfg).unwrap_or_else(|_| json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(mode: Mode, level: Level, kind: Kind, pausable: bool, ok: bool) -> Desired {
        let cfg = Config {
            mode,
            ..Default::default()
        };
        desired(&cfg, level, kind, pausable, ok)
    }

    #[test]
    fn workers_always_run_below_normal_and_lower_as_the_machine_strains() {
        let w = |l| d(Mode::Balanced, l, Kind::Work, true, false);
        assert_eq!(
            w(Level::Calm),
            Desired {
                cap: Some(Prio::BelowNormal),
                eco: false,
                pause: false
            }
        );
        assert_eq!(w(Level::Busy).cap, Some(Prio::BelowNormal));
        assert_eq!(
            w(Level::Strained),
            Desired {
                cap: Some(Prio::Idle),
                eco: true,
                pause: false
            }
        );
        assert_eq!(
            w(Level::Critical),
            Desired {
                cap: Some(Prio::Idle),
                eco: true,
                pause: false
            }
        ); // not yet due
    }

    #[test]
    fn a_worker_is_paused_only_at_critical_when_due_and_allowed() {
        assert!(d(Mode::Balanced, Level::Critical, Kind::Work, true, true).pause);
        assert!(
            !d(Mode::Balanced, Level::Critical, Kind::Work, false, true).pause,
            "not pausable"
        );
        assert!(
            !d(Mode::Gentle, Level::Critical, Kind::Work, true, true).pause,
            "gentle never pauses"
        );
        assert!(!d(Mode::Balanced, Level::Strained, Kind::Work, true, true).pause);
    }

    #[test]
    fn the_server_that_answers_chat_yields_only_when_strained_and_never_in_gentle() {
        assert_eq!(
            d(Mode::Balanced, Level::Busy, Kind::Serving, false, false),
            Desired::default()
        );
        assert_eq!(
            d(Mode::Balanced, Level::Strained, Kind::Serving, false, true).cap,
            Some(Prio::BelowNormal)
        );
        assert_eq!(
            d(Mode::Balanced, Level::Critical, Kind::Serving, false, true).cap,
            Some(Prio::BelowNormal)
        );
        assert!(
            !d(Mode::Balanced, Level::Critical, Kind::Serving, true, true).pause,
            "a server is never paused"
        );
        assert_eq!(
            d(Mode::Gentle, Level::Critical, Kind::Serving, false, false),
            Desired::default()
        );
    }

    #[test]
    fn strict_acts_one_level_earlier_and_off_does_nothing() {
        assert_eq!(
            d(Mode::Strict, Level::Busy, Kind::Serving, false, false).cap,
            Some(Prio::BelowNormal)
        );
        assert_eq!(
            d(Mode::Strict, Level::Busy, Kind::Work, true, false).cap,
            Some(Prio::Idle)
        );
        for l in [Level::Calm, Level::Busy, Level::Strained, Level::Critical] {
            for k in [Kind::Serving, Kind::Work, Kind::Other] {
                assert_eq!(d(Mode::Off, l, k, true, true), Desired::default());
            }
        }
    }

    #[test]
    fn other_children_are_lowered_but_never_paused() {
        assert_eq!(
            d(Mode::Balanced, Level::Calm, Kind::Other, false, false),
            Desired::default()
        );
        assert_eq!(
            d(Mode::Balanced, Level::Strained, Kind::Other, false, false).cap,
            Some(Prio::BelowNormal)
        );
        let c = d(Mode::Balanced, Level::Critical, Kind::Other, true, true);
        assert_eq!((c.cap, c.pause), (Some(Prio::Idle), false));
    }

    #[test]
    fn background_priority_normal_leaves_calm_workers_alone() {
        let cfg = Config {
            background_priority: "normal".into(),
            ..Default::default()
        };
        assert_eq!(desired(&cfg, Level::Calm, Kind::Work, true, false).cap, None);
        assert_eq!(
            desired(&cfg, Level::Strained, Kind::Work, true, false).cap,
            Some(Prio::Idle)
        );
    }

    #[test]
    fn processes_are_told_apart_by_what_they_are() {
        assert_eq!(kind_of("llama-server.exe", false), (Kind::Serving, false));
        assert_eq!(kind_of("whatever.exe", true), (Kind::Serving, false));
        assert_eq!(kind_of("prism-server.exe", false).0, Kind::Serving);
        for w in [
            "python.exe",
            "pythonw.exe",
            "python3.12.exe",
            "sd-cli.exe",
            "ffmpeg.exe",
            "llama-quantize.exe",
            "SD-CLI.EXE",
        ] {
            assert_eq!(kind_of(w, false), (Kind::Work, true), "{w}");
        }
        for o in [
            "cmd.exe",
            "cargo.exe",
            "model-mistress.exe",
            "pythonista.exe",
            "sdl.exe",
        ] {
            assert_eq!(kind_of(o, false), (Kind::Other, false), "{o}");
        }
    }

    #[test]
    fn the_console_host_of_a_child_is_a_helper_and_a_real_program_is_not() {
        for h in ["conhost.exe", "CONHOST.EXE", "OpenConsole.exe", "WerFault.exe"] {
            assert!(is_helper(h), "{h}");
        }
        for w in [
            "python.exe",
            "cmd.exe",
            "llama-server.exe",
            "conhost-ng.exe",
            "consoleapp.exe",
        ] {
            assert!(!is_helper(w), "{w}");
        }
    }

    #[test]
    fn the_settings_round_trip_through_a_file_and_a_patch_changes_only_what_it_names() {
        let dir = crate::testing::TestDir::new("sys-config");
        assert_eq!(Config::load(&dir).unwrap(), Config::default());
        let c = Config::default()
            .patched(&json!({"mode": "strict", "thresholds": {"cpu_busy_pct": 50.0}}))
            .unwrap();
        assert_eq!(c.mode, Mode::Strict);
        assert_eq!(c.thresholds.cpu_busy_pct, 50.0);
        assert_eq!(c.thresholds.cpu_strained_pct, Thresholds::default().cpu_strained_pct);
        c.save(&dir).unwrap();
        assert_eq!(Config::load(&dir).unwrap(), c);
    }

    #[test]
    fn a_bad_patch_or_file_is_an_error_not_a_silent_change() {
        let base = Config::default();
        assert!(base.patched(&json!({"mod": "off"})).is_err(), "misspelt key");
        assert!(base.patched(&json!({"mode": "loud"})).is_err());
        assert!(base.patched(&json!({"tick_ms": 5})).is_err());
        assert!(base.patched(&json!({"background_priority": "high"})).is_err());
        assert!(base
            .patched(&json!({"thresholds": {"cpu_busy_pct": 90.0, "cpu_strained_pct": 80.0}}))
            .is_err());
        let dir = crate::testing::TestDir::new("sys-config-bad");
        std::fs::write(dir.join("system.toml"), "mode = [").unwrap();
        assert!(Config::load(&dir).is_err());
        std::fs::write(dir.join("system.toml"), "wat = 1\n").unwrap();
        assert!(Config::load(&dir).is_err());
    }
}

//! What the hub can see of the machine by itself, with no other program: the CPU, the memory, the commit, the
//! processes it started, and what the user is doing at the screen.
//!
//! One `Native` is kept for the life of the sampler, because a CPU figure is a difference between two looks: the first
//! look has no CPU figures.

use std::collections::{HashMap, HashSet};

use sysinfo::{Pid, ProcessRefreshKind, System};

use super::level::UserState;

/// A process the hub started, directly or through another.
#[derive(Clone, Debug, PartialEq)]
pub struct Proc {
    pub pid: u32,
    pub parent: u32,
    pub name: String,
    /// CPU use as a percent of one core (above 100 when it runs several threads).
    pub cpu_pct: f64,
    /// When it started, unix seconds: with the pid it names one process for good, because Windows reuses pids.
    pub started_s: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Reading {
    /// None on the first look.
    pub cpu_total_pct: Option<f64>,
    pub cpu_mm_pct: f64,
    pub ram_total_mib: u64,
    pub ram_avail_mib: u64,
    pub commit_headroom_mib: Option<u64>,
    /// The hub's descendants (not the hub itself).
    pub tree: Vec<Proc>,
    /// The busiest processes that are not the hub or its descendants: (name, cpu % of the machine).
    pub others: Vec<(String, f64)>,
    pub user: Option<UserState>,
}

pub struct Native {
    sys: System,
    own: u32,
    ncpu: f64,
    looks: u32,
}

impl Default for Native {
    fn default() -> Self {
        Self::new()
    }
}

impl Native {
    pub fn new() -> Self {
        Self {
            sys: System::new(),
            own: std::process::id(),
            ncpu: crate::machine::logical_cores() as f64,
            looks: 0,
        }
    }

    pub fn sample(&mut self) -> Reading {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.sys
            .refresh_processes_specifics(ProcessRefreshKind::new().with_cpu());
        self.looks += 1;
        let warm = self.looks > 1;
        let mib = |b: u64| b / (1024 * 1024);

        // who is a descendant of the hub: walk up the parent links from every process
        let parents: HashMap<u32, u32> = self
            .sys
            .processes()
            .iter()
            .filter_map(|(pid, p)| p.parent().map(|pp| (pid.as_u32(), pp.as_u32())))
            .collect();
        let starts: HashMap<u32, u64> = self
            .sys
            .processes()
            .iter()
            .map(|(pid, p)| (pid.as_u32(), p.start_time()))
            .collect();
        let mut mine: HashSet<u32> = HashSet::from([self.own]);
        let mut tree = vec![];
        for (pid, p) in self.sys.processes() {
            let pid = pid.as_u32();
            if pid == self.own {
                continue;
            }
            if descends_from(self.own, pid, &parents, &starts) {
                mine.insert(pid);
                tree.push(Proc {
                    pid,
                    parent: parents.get(&pid).copied().unwrap_or(0),
                    name: p.name().to_string(),
                    cpu_pct: if warm { p.cpu_usage() as f64 } else { 0.0 },
                    started_s: p.start_time(),
                });
            }
        }
        let own_cpu = self
            .sys
            .process(Pid::from_u32(self.own))
            .map(|p| p.cpu_usage() as f64)
            .unwrap_or(0.0);
        let mm_cpu = if warm {
            (own_cpu + tree.iter().map(|t| t.cpu_pct).sum::<f64>()) / self.ncpu
        } else {
            0.0
        };
        let mut others: Vec<(String, f64)> = if warm {
            self.sys
                .processes()
                .iter()
                .filter(|(pid, _)| !mine.contains(&pid.as_u32()) && pid.as_u32() != 0)
                .map(|(_, p)| (p.name().to_string(), p.cpu_usage() as f64 / self.ncpu))
                .filter(|(n, c)| *c >= 1.0 && !n.eq_ignore_ascii_case("system idle process"))
                .collect()
        } else {
            vec![]
        };
        others.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        others.truncate(5);
        Reading {
            cpu_total_pct: warm.then(|| (self.sys.global_cpu_info().cpu_usage() as f64).clamp(0.0, 100.0)),
            cpu_mm_pct: mm_cpu,
            ram_total_mib: mib(self.sys.total_memory()),
            ram_avail_mib: mib(self.sys.available_memory()),
            commit_headroom_mib: crate::machine::commit_headroom_mib(),
            tree,
            others,
            user: user_state(),
        }
    }
}

/// Does `pid` descend from `own`, by the parent links?
///
/// Windows keeps the parent pid of a process after that parent has exited and later gives the same pid to some other
/// program, so a link is only believed when it can be true: a process cannot be older than its parent, and both start
/// times must be known. A broken link ends the walk, so a stale parent never makes an unrelated program (a daemon the
/// user started an hour ago) look like the hub's own. The walk is bounded, so a parent loop cannot hang the sampler.
fn descends_from(own: u32, pid: u32, parents: &HashMap<u32, u32>, starts: &HashMap<u32, u64>) -> bool {
    let (mut at, mut hops) = (pid, 0);
    while let Some(&up) = parents.get(&at) {
        let child_started = starts.get(&at).copied().unwrap_or(0);
        let parent_started = starts.get(&up).copied().unwrap_or(0);
        if child_started == 0 || parent_started == 0 || child_started < parent_started {
            return false;
        }
        if up == own {
            return true;
        }
        if up == at || hops > 32 {
            return false;
        }
        at = up;
        hops += 1;
    }
    false
}

/// What the user is doing at the screen (Windows: the shell's notification state). None where it cannot be told.
pub fn user_state() -> Option<UserState> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::{
            SHQueryUserNotificationState, QUNS_ACCEPTS_NOTIFICATIONS, QUNS_BUSY, QUNS_NOT_PRESENT,
            QUNS_PRESENTATION_MODE, QUNS_QUIET_TIME, QUNS_RUNNING_D3D_FULL_SCREEN,
        };
        let mut state = 0;
        // SAFETY: `state` is a valid out parameter for the call.
        let hr = unsafe { SHQueryUserNotificationState(&mut state) };
        if hr < 0 {
            return None;
        }
        Some(match state {
            QUNS_RUNNING_D3D_FULL_SCREEN => UserState::FullScreenGame,
            QUNS_BUSY => UserState::FullScreen,
            QUNS_PRESENTATION_MODE => UserState::Presenting,
            QUNS_NOT_PRESENT => UserState::Away,
            QUNS_ACCEPTS_NOTIFICATIONS | QUNS_QUIET_TIME => UserState::Normal,
            _ => UserState::Normal,
        })
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    #[test]
    fn the_first_look_has_no_cpu_figures_and_the_second_does() {
        let mut n = Native::new();
        let first = n.sample();
        assert!(first.cpu_total_pct.is_none() && first.cpu_mm_pct == 0.0);
        assert!(first.ram_total_mib > 0 && first.ram_avail_mib > 0 && first.ram_avail_mib <= first.ram_total_mib);
        std::thread::sleep(Duration::from_millis(400));
        let second = n.sample();
        let t = second.cpu_total_pct.expect("a second look has a CPU figure");
        assert!((0.0..=100.0).contains(&t), "{t}");
        assert!(
            second.cpu_mm_pct >= 0.0 && second.cpu_mm_pct <= 100.0 + 1e-6,
            "{}",
            second.cpu_mm_pct
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_reports_the_commit_headroom() {
        assert!(Native::new().sample().commit_headroom_mib.unwrap() > 0);
    }

    /// A child process that spins, and a grandchild: both are found as the test process's descendants, with their CPU.
    #[test]
    fn a_process_the_hub_started_is_in_its_tree_with_its_cpu() {
        let spin = if cfg!(windows) { "powershell" } else { "sh" };
        let script: &[&str] = if cfg!(windows) {
            &["-NoProfile", "-Command", "$x=0; while($true){$x++}"]
        } else {
            &["-c", "while :; do :; done"]
        };
        let mut child = Command::new(spin)
            .args(script)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn a spinner");
        let mut n = Native::new();
        n.sample();
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut found = None;
        while Instant::now() < deadline && found.is_none() {
            std::thread::sleep(Duration::from_millis(500));
            let r = n.sample();
            found = r
                .tree
                .iter()
                .find(|p| p.pid == child.id())
                .cloned()
                .filter(|p| p.cpu_pct > 10.0)
                .map(|p| (p, r.cpu_mm_pct));
        }
        let _ = child.kill();
        let _ = child.wait();
        let (p, mm) = found.expect("the spinner is a descendant using CPU");
        assert_eq!(p.parent, std::process::id());
        assert!(p.started_s > 0);
        assert!(mm > 0.0, "its CPU is counted as ours: {mm}");
    }

    fn map<const N: usize>(pairs: [(u32, u64); N]) -> HashMap<u32, u64> {
        pairs.into_iter().collect()
    }

    #[test]
    fn a_child_and_a_grandchild_descend_from_the_hub() {
        // hub 10 (started 1000) -> 20 (1005) -> 30 (1010)
        let parents = HashMap::from([(20, 10), (30, 20)]);
        let starts = map([(10, 1000), (20, 1005), (30, 1010)]);
        assert!(descends_from(10, 20, &parents, &starts));
        assert!(descends_from(10, 30, &parents, &starts));
        assert!(
            !descends_from(10, 10, &parents, &starts),
            "the hub is not its own descendant"
        );
        // started in the same second as its parent: Windows start times are whole seconds
        assert!(descends_from(10, 20, &parents, &map([(10, 1000), (20, 1000)])));
    }

    /// The case found on the live hub: `adb.exe` was started at 10:38 by a program that has since exited; its recorded parent pid
    /// was later reused by a console host the hub's child started at 12:06, so the walk reached the hub and lowered adb.
    #[test]
    fn a_process_older_than_its_recorded_parent_is_not_its_child() {
        // adb 30 (started 100) says its parent is 20, which is now a console host (started 5000), a child of hub 10 (4000)
        let parents = HashMap::from([(20, 10), (30, 20)]);
        let starts = map([(10, 4000), (20, 5000), (30, 100)]);
        assert!(
            descends_from(10, 20, &parents, &starts),
            "the console host really is the hub's child"
        );
        assert!(
            !descends_from(10, 30, &parents, &starts),
            "adb is older than the pid it names as its parent"
        );
    }

    #[test]
    fn a_link_whose_start_time_is_unknown_is_not_believed() {
        let parents = HashMap::from([(20, 10), (30, 20)]);
        assert!(!descends_from(10, 20, &parents, &map([(10, 1000), (20, 0)])));
        assert!(!descends_from(10, 20, &parents, &map([(10, 0), (20, 1005)])));
        assert!(
            !descends_from(10, 20, &parents, &map([(20, 1005)])),
            "a parent that is gone from the table"
        );
        // a break anywhere on the way up ends the walk
        assert!(!descends_from(
            10,
            30,
            &parents,
            &map([(10, 1000), (20, 0), (30, 1010)])
        ));
    }

    #[test]
    fn a_parent_loop_and_an_unrelated_chain_end_the_walk() {
        let looped = HashMap::from([(20, 30), (30, 20)]);
        let starts = map([(10, 1000), (20, 1005), (30, 1005)]);
        assert!(!descends_from(10, 20, &looped, &starts));
        let self_parent = HashMap::from([(20, 20)]);
        assert!(!descends_from(10, 20, &self_parent, &starts));
        let elsewhere = HashMap::from([(20, 40), (40, 1)]);
        assert!(!descends_from(
            10,
            20,
            &elsewhere,
            &map([(10, 1000), (20, 1005), (40, 900), (1, 1)])
        ));
    }

    #[test]
    fn this_process_is_not_in_its_own_tree() {
        let mut n = Native::new();
        n.sample();
        let r = n.sample();
        assert!(r.tree.iter().all(|p| p.pid != std::process::id()));
    }

    #[cfg(windows)]
    #[test]
    fn the_shell_says_what_the_user_is_doing() {
        assert!(user_state().is_some());
    }

    #[test]
    fn sampling_is_cheap_enough_to_run_every_two_seconds() {
        let mut n = Native::new();
        n.sample();
        // The best of several looks: a busy machine (this suite runs beside builds) can slow any one of them, but a look
        // that really costs half a second is never fast.
        let best = (0..6)
            .map(|_| {
                let t = Instant::now();
                n.sample();
                t.elapsed()
            })
            .min()
            .unwrap();
        println!("native look, best of 6: {best:?}");
        assert!(best < Duration::from_millis(500), "{best:?}");
    }
}

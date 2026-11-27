//! Changing a process: its priority class, Windows' efficiency mode, and pausing it.
//!
//! Everything goes through the `Actuator` trait so the governor's rules are tested against a recorder, and the real one
//! (Windows) is tested on a process the test starts itself. A process is named by its pid and the second it started: a
//! pid that was reused by another program since the governor looked is refused, not changed.
//!
//! Only Windows has an actuator. Elsewhere the governor still reads the machine, gates heavy jobs and sheds idle models,
//! but it changes no process (`Null`).

use super::policy::Prio;

/// Windows' priority classes, as numbers (the same on every platform so the rules can be tested anywhere).
pub const IDLE: u32 = 0x40;
pub const BELOW_NORMAL: u32 = 0x4000;
pub const NORMAL: u32 = 0x20;
pub const ABOVE_NORMAL: u32 = 0x8000;
pub const HIGH: u32 = 0x80;
pub const REALTIME: u32 = 0x100;

/// The class to set for a priority the governor wants.
pub fn class_of(p: Prio) -> u32 {
    match p {
        Prio::Idle => IDLE,
        Prio::BelowNormal => BELOW_NORMAL,
        Prio::Normal => NORMAL,
    }
}

/// How the governor sees a class: everything from normal up is "normal", a class it does not know is normal too (it
/// will not lower what it cannot place).
pub fn prio_of(class: u32) -> Prio {
    match class {
        IDLE => Prio::Idle,
        BELOW_NORMAL => Prio::BelowNormal,
        _ => Prio::Normal,
    }
}

/// What a process is at now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcState {
    pub class: u32,
    /// Efficiency mode forced on.
    pub eco: bool,
}

pub trait Actuator: Send + Sync {
    /// Whether this actuator changes anything at all.
    fn active(&self) -> bool {
        true
    }
    /// The process's state, or None when it is gone, is not the process that started at `started_s`, or cannot be read.
    fn state(&self, pid: u32, started_s: u64) -> Option<ProcState>;
    fn set_class(&self, pid: u32, started_s: u64, class: u32) -> Result<(), String>;
    /// `on`: force efficiency mode. Off puts it back under the system's own control.
    fn set_eco(&self, pid: u32, started_s: u64, on: bool) -> Result<(), String>;
    fn suspend(&self, pid: u32, started_s: u64) -> Result<(), String>;
    fn resume(&self, pid: u32, started_s: u64) -> Result<(), String>;
}

/// The actuator for this platform.
pub fn native() -> Box<dyn Actuator> {
    #[cfg(windows)]
    {
        Box::new(win::Windows)
    }
    #[cfg(not(windows))]
    {
        Box::new(Null)
    }
}

/// Changes nothing (a platform with no actuator).
#[allow(dead_code)]
pub struct Null;

impl Actuator for Null {
    fn active(&self) -> bool {
        false
    }
    fn state(&self, _: u32, _: u64) -> Option<ProcState> {
        None
    }
    fn set_class(&self, _: u32, _: u64, _: u32) -> Result<(), String> {
        Err("this platform cannot change a process's priority from here".into())
    }
    fn set_eco(&self, _: u32, _: u64, _: bool) -> Result<(), String> {
        Err("this platform has no efficiency mode".into())
    }
    fn suspend(&self, _: u32, _: u64) -> Result<(), String> {
        Err("this platform cannot pause a process from here".into())
    }
    fn resume(&self, _: u32, _: u64) -> Result<(), String> {
        Err("this platform cannot pause a process from here".into())
    }
}

#[cfg(windows)]
pub mod win {
    use super::*;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
    use windows_sys::Win32::System::Threading::{
        GetPriorityClass, GetProcessInformation, GetProcessTimes, OpenProcess, ProcessPowerThrottling,
        SetPriorityClass, SetProcessInformation, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE, PROCESS_QUERY_INFORMATION,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION, PROCESS_SUSPEND_RESUME,
    };

    // NtSuspendProcess and NtResumeProcess are not in windows-sys; they are in ntdll, which every process has loaded.
    #[link(name = "ntdll")]
    extern "system" {
        fn NtSuspendProcess(process: HANDLE) -> i32;
        fn NtResumeProcess(process: HANDLE) -> i32;
    }

    /// A process handle that closes itself.
    struct Owned(HANDLE);

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: the handle came from OpenProcess and is closed once, here.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// Seconds between 1601-01-01 (a FILETIME's zero) and 1970-01-01.
    const EPOCH_GAP_S: u64 = 11_644_473_600;

    fn started_of(h: &Owned) -> Option<u64> {
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut a, mut b, mut c) = (zero, zero, zero, zero);
        // SAFETY: the handle is open with query rights and the four out parameters are valid.
        if unsafe { GetProcessTimes(h.0, &mut created, &mut a, &mut b, &mut c) } == 0 {
            return None;
        }
        let ticks = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
        (ticks / 10_000_000).checked_sub(EPOCH_GAP_S)
    }

    /// Opens the process, and only when it is the one that started at `started_s` (a second either way: the two clocks
    /// are read apart).
    fn open(pid: u32, started_s: u64, access: u32) -> Result<Owned, String> {
        // SAFETY: OpenProcess takes plain values; a null return is a failure.
        let h = unsafe { OpenProcess(access, 0, pid) };
        if h.is_null() {
            return Err(format!("process {pid} cannot be opened (gone, or not ours to change)"));
        }
        let h = Owned(h);
        if started_s > 0 {
            match started_of(&h) {
                Some(s) if s.abs_diff(started_s) <= 2 => {}
                _ => return Err(format!("process {pid} is not the one that started at {started_s}")),
            }
        }
        Ok(h)
    }

    fn throttling(h: &Owned) -> Option<PROCESS_POWER_THROTTLING_STATE> {
        let mut st = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: 0,
            StateMask: 0,
        };
        // SAFETY: `st` is a valid, correctly sized out structure for this information class.
        let ok = unsafe {
            GetProcessInformation(
                h.0,
                ProcessPowerThrottling,
                &mut st as *mut _ as *mut core::ffi::c_void,
                std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            )
        };
        (ok != 0).then_some(st)
    }

    pub struct Windows;

    impl Actuator for Windows {
        fn state(&self, pid: u32, started_s: u64) -> Option<ProcState> {
            // GetProcessInformation (the efficiency-mode state) needs the full query right; the limited one still reads
            // the class, so fall back to it for a process that refuses the full one
            let h = open(pid, started_s, PROCESS_QUERY_INFORMATION)
                .or_else(|_| open(pid, started_s, PROCESS_QUERY_LIMITED_INFORMATION))
                .ok()?;
            // SAFETY: the handle is open with query rights.
            let class = unsafe { GetPriorityClass(h.0) };
            if class == 0 {
                return None;
            }
            let eco = throttling(&h)
                .map(|t| {
                    t.ControlMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0
                        && t.StateMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0
                })
                .unwrap_or(false);
            Some(ProcState { class, eco })
        }

        fn set_class(&self, pid: u32, started_s: u64, class: u32) -> Result<(), String> {
            let h = open(
                pid,
                started_s,
                PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION,
            )?;
            // SAFETY: the handle is open with set rights; the class is one of the documented constants.
            if unsafe { SetPriorityClass(h.0, class) } == 0 {
                return Err(format!(
                    "SetPriorityClass on {pid}: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(())
        }

        fn set_eco(&self, pid: u32, started_s: u64, on: bool) -> Result<(), String> {
            let h = open(
                pid,
                started_s,
                PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION,
            )?;
            let st = PROCESS_POWER_THROTTLING_STATE {
                Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
                // off: stop controlling it at all, so the system decides again
                ControlMask: if on {
                    PROCESS_POWER_THROTTLING_EXECUTION_SPEED
                } else {
                    0
                },
                StateMask: if on {
                    PROCESS_POWER_THROTTLING_EXECUTION_SPEED
                } else {
                    0
                },
            };
            // SAFETY: `st` is a valid structure of the size given, for this information class.
            let ok = unsafe {
                SetProcessInformation(
                    h.0,
                    ProcessPowerThrottling,
                    &st as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
                )
            };
            if ok == 0 {
                return Err(format!(
                    "SetProcessInformation on {pid}: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(())
        }

        fn suspend(&self, pid: u32, started_s: u64) -> Result<(), String> {
            let h = open(
                pid,
                started_s,
                PROCESS_SUSPEND_RESUME | PROCESS_QUERY_LIMITED_INFORMATION,
            )?;
            // SAFETY: the handle is open with suspend rights.
            let status = unsafe { NtSuspendProcess(h.0) };
            if status < 0 {
                return Err(format!("NtSuspendProcess on {pid}: status {status:#x}"));
            }
            Ok(())
        }

        fn resume(&self, pid: u32, started_s: u64) -> Result<(), String> {
            let h = open(
                pid,
                started_s,
                PROCESS_SUSPEND_RESUME | PROCESS_QUERY_LIMITED_INFORMATION,
            )?;
            // SAFETY: the handle is open with suspend rights.
            let status = unsafe { NtResumeProcess(h.0) };
            if status < 0 {
                return Err(format!("NtResumeProcess on {pid}: status {status:#x}"));
            }
            Ok(())
        }
    }
}

/// A recorder for the governor's tests: a table of processes, and every change made to it.
#[cfg(test)]
pub mod mock {
    use super::*;
    use parking_lot::Mutex;
    use std::collections::HashMap;

    #[derive(Default)]
    pub struct Mock {
        pub procs: Mutex<HashMap<u32, (u64, ProcState, bool)>>, // pid -> (started, state, paused)
        pub log: Mutex<Vec<String>>,
        /// Refuse every change (access denied).
        pub deny: Mutex<bool>,
    }

    impl Mock {
        pub fn add(&self, pid: u32, started_s: u64, class: u32) {
            self.procs
                .lock()
                .insert(pid, (started_s, ProcState { class, eco: false }, false));
        }
        pub fn remove(&self, pid: u32) {
            self.procs.lock().remove(&pid);
        }
        pub fn class(&self, pid: u32) -> u32 {
            self.procs.lock()[&pid].1.class
        }
        pub fn eco(&self, pid: u32) -> bool {
            self.procs.lock()[&pid].1.eco
        }
        pub fn paused(&self, pid: u32) -> bool {
            self.procs.lock()[&pid].2
        }
        pub fn changes(&self) -> usize {
            self.log.lock().len()
        }
        fn with<R>(
            &self,
            pid: u32,
            started_s: u64,
            f: impl FnOnce(&mut (u64, ProcState, bool)) -> R,
        ) -> Result<R, String> {
            if *self.deny.lock() {
                return Err("access denied".into());
            }
            let mut m = self.procs.lock();
            match m.get_mut(&pid) {
                Some(p) if p.0 == started_s => Ok(f(p)),
                _ => Err(format!("process {pid} is gone")),
            }
        }
    }

    impl Actuator for Mock {
        fn state(&self, pid: u32, started_s: u64) -> Option<ProcState> {
            self.procs.lock().get(&pid).filter(|p| p.0 == started_s).map(|p| p.1)
        }
        fn set_class(&self, pid: u32, started_s: u64, class: u32) -> Result<(), String> {
            self.with(pid, started_s, |p| p.1.class = class)?;
            self.log.lock().push(format!("class {pid} {class:#x}"));
            Ok(())
        }
        fn set_eco(&self, pid: u32, started_s: u64, on: bool) -> Result<(), String> {
            self.with(pid, started_s, |p| p.1.eco = on)?;
            self.log.lock().push(format!("eco {pid} {on}"));
            Ok(())
        }
        fn suspend(&self, pid: u32, started_s: u64) -> Result<(), String> {
            self.with(pid, started_s, |p| p.2 = true)?;
            self.log.lock().push(format!("suspend {pid}"));
            Ok(())
        }
        fn resume(&self, pid: u32, started_s: u64) -> Result<(), String> {
            self.with(pid, started_s, |p| p.2 = false)?;
            self.log.lock().push(format!("resume {pid}"));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_map_both_ways_and_nothing_above_normal_is_lowered_to_a_stranger() {
        assert_eq!(prio_of(IDLE), Prio::Idle);
        assert_eq!(prio_of(BELOW_NORMAL), Prio::BelowNormal);
        for c in [NORMAL, ABOVE_NORMAL, HIGH, REALTIME, 0xdead] {
            assert_eq!(prio_of(c), Prio::Normal);
        }
        for p in [Prio::Idle, Prio::BelowNormal, Prio::Normal] {
            assert_eq!(prio_of(class_of(p)), p);
        }
    }

    #[test]
    fn the_null_actuator_changes_nothing_and_says_so() {
        let n = Null;
        assert!(!n.active());
        assert!(n.state(1, 1).is_none());
        assert!(n.set_class(1, 1, IDLE).is_err() && n.suspend(1, 1).is_err());
    }

    #[cfg(windows)]
    mod real {
        use super::super::*;
        use std::process::{Command, Stdio};
        use std::time::{Duration, SystemTime, UNIX_EPOCH};

        fn sleeper() -> std::process::Child {
            Command::new("powershell")
                .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 60"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn a sleeper")
        }

        fn started_now() -> u64 {
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
        }

        /// The real thing, on a process this test started: lower it, read it back, put it back.
        #[test]
        fn a_real_process_is_lowered_made_efficient_paused_and_put_back() {
            let mut child = sleeper();
            let (pid, t0) = (child.id(), started_now());
            std::thread::sleep(Duration::from_millis(300));
            let a = win::Windows;
            let before = a.state(pid, t0).expect("readable");
            // a child takes its parent's class, and the tests run low on purpose
            assert!(
                [IDLE, BELOW_NORMAL, NORMAL, ABOVE_NORMAL, HIGH].contains(&before.class),
                "{:#x}",
                before.class
            );

            a.set_class(pid, t0, BELOW_NORMAL).unwrap();
            assert_eq!(a.state(pid, t0).unwrap().class, BELOW_NORMAL);
            a.set_class(pid, t0, IDLE).unwrap();
            assert_eq!(a.state(pid, t0).unwrap().class, IDLE);

            // the calls succeed; the state is not asserted from a read-back, which Windows 10 does not report reliably
            a.set_eco(pid, t0, true).unwrap();
            a.set_eco(pid, t0, false).unwrap();

            a.suspend(pid, t0).unwrap();
            a.resume(pid, t0).unwrap();

            a.set_class(pid, t0, before.class).unwrap();
            assert_eq!(a.state(pid, t0).unwrap(), before);
            let _ = child.kill();
            let _ = child.wait();
        }

        #[test]
        fn a_process_that_started_at_another_time_is_refused() {
            let mut child = sleeper();
            let pid = child.id();
            std::thread::sleep(Duration::from_millis(300));
            // the pid is right and the start second is an hour off: a reused pid looks like this
            let wrong = started_now() - 3600;
            assert!(win::Windows.state(pid, wrong).is_none());
            assert!(win::Windows.set_class(pid, wrong, IDLE).is_err());
            let _ = child.kill();
            let _ = child.wait();
        }

        #[test]
        fn a_process_that_is_gone_is_an_error_not_a_panic() {
            let mut child = sleeper();
            let pid = child.id();
            let t0 = started_now();
            let _ = child.kill();
            let _ = child.wait();
            // the test's own handle keeps an exited process readable
            drop(child);
            // Another program (a scanner, a monitor) may hold a handle to it for a moment, and an exited process stays
            // readable while any handle is open: give them time to let go.
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while win::Windows.state(pid, t0).is_some() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
            assert!(win::Windows.state(pid, t0).is_none());
            assert!(win::Windows.set_class(pid, t0, IDLE).is_err());
            assert!(win::Windows.resume(pid, t0).is_err());
        }
    }
}

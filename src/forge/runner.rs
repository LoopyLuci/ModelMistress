//! Running one command for a run: spawn it in its own process group (a Job Object on Windows), feed it JSON on stdin,
//! read its stdout and stderr line by line, turn protocol lines into store rows, and kill the whole process tree on
//! cancel, timeout, or when the hub exits.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use super::protocol::{parse_line, Event};
use super::store::Store;
use crate::pull::Job;

/// The process group of a run. Dropping it kills whatever is still in it.
pub struct Group(imp::Group);

impl Group {
    pub fn new() -> Option<Self> {
        imp::Group::new().map(Self)
    }
    pub fn kill(&self) {
        self.0.kill()
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub struct Group(HANDLE);
    unsafe impl Send for Group {}
    unsafe impl Sync for Group {}

    impl Group {
        pub fn new() -> Option<Self> {
            unsafe {
                let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if h.is_null() {
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                SetInformationJobObject(
                    h,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                Some(Group(h))
            }
        }
        pub fn assign(&self, process: std::os::windows::io::RawHandle) {
            unsafe {
                AssignProcessToJobObject(self.0, process as HANDLE);
            }
        }
        pub fn kill(&self) {
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Group {
        fn drop(&mut self) {
            // KILL_ON_JOB_CLOSE ends anything left in the job
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(unix)]
mod imp {
    pub struct Group(std::sync::atomic::AtomicI32);

    impl Group {
        pub fn new() -> Option<Self> {
            Some(Group(std::sync::atomic::AtomicI32::new(0)))
        }
        pub fn assign(&self, pid: u32) {
            self.0.store(pid as i32, std::sync::atomic::Ordering::Relaxed);
        }
        pub fn kill(&self) {
            let pid = self.0.load(std::sync::atomic::Ordering::Relaxed);
            if pid > 0 {
                let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGKILL);
            }
        }
    }

    impl Drop for Group {
        fn drop(&mut self) {
            self.kill();
        }
    }
}

/// What to run.
#[derive(Clone, Default)]
pub struct Cmd {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    /// Written to the process's stdin, which is then closed.
    pub stdin: Option<String>,
    /// stdout is the JSON-lines protocol (else every line is just log).
    pub protocol: bool,
    pub timeout: Option<Duration>,
}

/// Asked-for cancelling, shared between the run's task and whoever cancels it.
#[derive(Default)]
pub struct Ctl {
    cancel: AtomicBool,
}

impl Ctl {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub cancelled: bool,
    pub timed_out: bool,
    pub exit_code: Option<i32>,
    pub result: Option<Value>,
    /// (code, message) from an `error` event.
    pub error: Option<(String, String)>,
    /// The last lines the process printed, for failure messages.
    pub tail: Vec<String>,
    pub spawn_error: Option<String>,
}

impl Outcome {
    pub fn success(&self, protocol: bool) -> bool {
        !self.cancelled
            && !self.timed_out
            && self.spawn_error.is_none()
            && self.exit_code == Some(0)
            && self.error.is_none()
            && (!protocol || self.result.is_some())
    }

    /// Why it failed, in one message.
    pub fn failure(&self, protocol: bool) -> (String, String) {
        if let Some(e) = &self.spawn_error {
            return ("spawn".into(), e.clone());
        }
        if self.cancelled {
            return ("cancelled".into(), "cancelled".into());
        }
        if self.timed_out {
            return ("timeout".into(), "the step took longer than its timeout".into());
        }
        if let Some((c, m)) = &self.error {
            return (c.clone(), m.clone());
        }
        let tail = self.tail.join("\n");
        if self.exit_code == Some(0) && protocol {
            return (
                "no_result".into(),
                format!("the worker finished without a result\n{tail}"),
            );
        }
        (
            "exit".into(),
            format!(
                "the process exited with {}\n{tail}",
                self.exit_code.map_or("a signal".into(), |c| c.to_string())
            ),
        )
    }
}

/// Starts `cmd` in a new process group (stdin piped when `cmd.stdin` is set, or when `keep_stdin`).
pub fn spawn_cmd(cmd: &Cmd) -> Result<(tokio::process::Child, Option<Group>), String> {
    spawn_with(cmd, cmd.stdin.is_some())
}

pub fn spawn_with(cmd: &Cmd, pipe_stdin: bool) -> Result<(tokio::process::Child, Option<Group>), String> {
    let mut c = tokio::process::Command::new(&cmd.program);
    c.args(&cmd.args)
        .envs(cmd.env.iter().map(|(k, v)| (k, v)))
        .stdin(if pipe_stdin {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let Some(d) = &cmd.cwd {
        c.current_dir(d);
    }
    crate::engine::no_window(&mut c);
    #[cfg(unix)]
    c.process_group(0);
    let group = Group::new();
    let child = c
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", cmd.program.display()))?;
    if let Some(g) = &group {
        g.attach(&child);
    }
    Ok((child, group))
}

impl Group {
    pub fn attach(&self, child: &tokio::process::Child) {
        #[cfg(windows)]
        if let Some(h) = child.raw_handle() {
            self.0.assign(h);
        }
        #[cfg(unix)]
        if let Some(pid) = child.id() {
            self.0.assign(pid);
        }
    }
}

pub(super) fn pump_lines<R: AsyncRead + Unpin + Send + 'static>(
    r: R,
    err: bool,
    tx: tokio::sync::mpsc::UnboundedSender<(bool, String)>,
) {
    pump(r, err, tx)
}

fn pump<R: AsyncRead + Unpin + Send + 'static>(
    r: R,
    err: bool,
    tx: tokio::sync::mpsc::UnboundedSender<(bool, String)>,
) {
    tokio::spawn(async move {
        let mut r = r;
        let mut chunk = [0u8; 8192];
        let mut line: Vec<u8> = Vec::new();
        loop {
            let n = match r.read(&mut chunk).await {
                Ok(n) if n > 0 => n,
                _ => break,
            };
            for &b in &chunk[..n] {
                // stderr is where progress bars live: a CR ends a line there as well
                if b == b'\n' || (err && b == b'\r') {
                    if !line.is_empty() {
                        let _ = tx.send((err, String::from_utf8_lossy(&line).trim_end().to_string()));
                        line.clear();
                    }
                } else {
                    line.push(b);
                }
            }
        }
        if !line.is_empty() {
            let _ = tx.send((err, String::from_utf8_lossy(&line).trim_end().to_string()));
        }
    });
}

/// Runs `cmd` for run `id`, recording everything it reports in `store`. `job` (if any) shows the progress too and
/// can cancel.
pub async fn exec(store: &Store, id: &str, cmd: &Cmd, ctl: &Ctl, job: Option<&Arc<Job>>) -> Outcome {
    let mut out = Outcome::default();
    let (mut child, group) = match spawn_cmd(cmd) {
        Ok(x) => x,
        Err(e) => {
            out.spawn_error = Some(e);
            return out;
        }
    };
    let _ = store.set_pid(id, child.id());
    if let (Some(text), Some(mut si)) = (cmd.stdin.clone(), child.stdin.take()) {
        tokio::spawn(async move {
            let _ = si.write_all(text.as_bytes()).await;
            let _ = si.shutdown().await;
        });
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(bool, String)>();
    if let Some(o) = child.stdout.take() {
        pump(o, false, tx.clone());
    }
    if let Some(e) = child.stderr.take() {
        pump(e, true, tx.clone());
    }
    drop(tx);

    let deadline = cmd.timeout.map(|t| Instant::now() + t);
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    let mut last_progress = Instant::now() - Duration::from_secs(1);
    let mut killed_at: Option<Instant> = None;
    loop {
        tokio::select! {
            m = rx.recv() => {
                let Some((is_err, line)) = m else { break };
                if line.is_empty() { continue }
                out.tail.push(line.clone());
                if out.tail.len() > 20 { out.tail.remove(0); }
                let ev = if cmd.protocol && !is_err { parse_line(&line) } else { Event::Text(line) };
                match ev {
                    Event::Progress { done, total, note } => {
                        if last_progress.elapsed() >= Duration::from_millis(250) || (total > 0.0 && done >= total) {
                            last_progress = Instant::now();
                            let _ = store.set_progress(id, done, total, &note);
                        }
                        if let Some(j) = job { j.progress(done as u64, total as u64, note); }
                    }
                    Event::Metric { name, value, step } => { let _ = store.add_metric(id, &name, step, value); }
                    Event::Log { level, message } => { let _ = store.add_log(id, &level, &message); }
                    Event::Artifact { name, path, bytes, kind } => { let _ = store.add_artifact(id, &name, &path, bytes, &kind); }
                    Event::Result(v) => out.result = Some(v),
                    Event::Error { message, code } => {
                        let _ = store.add_log(id, "error", &message);
                        out.error = Some((code, message));
                    }
                    Event::Text(t) => { let _ = store.add_log(id, if is_err { "stderr" } else { "stdout" }, &t); }
                }
            }
            _ = tick.tick() => {
                if killed_at.is_none() {
                    let want_cancel = ctl.cancelled() || job.is_some_and(|j| j.cancelled());
                    let timed_out = deadline.is_some_and(|d| Instant::now() >= d);
                    if want_cancel || timed_out {
                        out.cancelled = want_cancel;
                        out.timed_out = timed_out && !want_cancel;
                        if let Some(g) = &group { g.kill(); }
                        let _ = child.start_kill();
                        killed_at = Some(Instant::now());
                    }
                } else if killed_at.is_some_and(|k| k.elapsed() > Duration::from_secs(5)) {
                    break; // a stray holder of the pipes; stop waiting for them
                }
            }
        }
    }
    out.exit_code = match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
        Ok(Ok(s)) => s.code(),
        _ => None,
    };
    if let Some(g) = &group {
        g.kill(); // anything the worker left behind
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MODE: &str = "MM_FORGE_TEST_MODE";

    /// Run as a child of the tests below (the test binary itself is the program, so no other tool is needed):
    /// `emit` prints protocol lines and noise, `tree` starts a grandchild and waits forever.
    #[test]
    #[allow(clippy::zombie_processes)] // the grandchild is meant to outlive this process unless the group is killed
    fn child_helper() {
        let Ok(mode) = std::env::var(MODE) else { return };
        match mode.as_str() {
            "emit" => {
                println!(r#"{{"type":"progress","done":1,"total":2,"note":"half"}}"#);
                println!("a stray line");
                println!(r#"{{"type":"metric","name":"loss","value":2.5,"step":1}}"#);
                println!(r#"{{"type":"metric","name":"loss","value":2.0,"step":2}}"#);
                println!(r#"{{"type":"artifact","name":"a","path":"p","bytes":3,"kind":"gguf"}}"#);
                eprintln!("to stderr");
                println!(r#"{{"type":"progress","done":2,"total":2,"note":"done"}}"#);
                println!(r#"{{"type":"result","data":{{"ok":true}}}}"#);
            }
            "fail" => {
                println!(r#"{{"type":"error","message":"it broke","code":"bad"}}"#);
                std::process::exit(1);
            }
            "silent" => {}
            "stdin" => {
                let mut s = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut s).unwrap();
                let v: Value = serde_json::from_str(&s).unwrap();
                println!("{}", json!({"type": "result", "data": {"got": v}}));
            }
            "tree" => {
                let me = std::env::current_exe().unwrap();
                let child = std::process::Command::new(me)
                    .args(["--exact", "forge::runner::tests::child_helper", "--nocapture"])
                    .env(MODE, "leaf")
                    .spawn()
                    .unwrap();
                println!(r#"{{"type":"log","message":"grandchild {}"}}"#, child.id());
                loop {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            "leaf" => loop {
                std::thread::sleep(Duration::from_millis(100));
            },
            _ => {}
        }
    }

    fn helper(mode: &str, stdin: Option<&str>) -> Cmd {
        Cmd {
            program: std::env::current_exe().unwrap(),
            args: ["--exact", "forge::runner::tests::child_helper", "--nocapture"]
                .map(String::from)
                .to_vec(),
            env: vec![(MODE.into(), mode.into())],
            stdin: stdin.map(String::from),
            protocol: true,
            ..Default::default()
        }
    }

    fn store() -> (Store, PathBuf) {
        let d = std::env::temp_dir().join(format!("mm-forge-run-{}", uuid::Uuid::new_v4()));
        let s = Store::new(d.join("runs.db"));
        s.create_run("r", "t", &json!({})).unwrap();
        (s, d)
    }

    #[tokio::test]
    async fn protocol_lines_become_rows_and_noise_becomes_log() {
        let (s, d) = store();
        let o = exec(&s, "r", &helper("emit", None), &Ctl::default(), None).await;
        assert!(o.success(true), "{o:?}");
        assert_eq!(o.result, Some(json!({"ok": true})));
        let m = s.metrics("r", None, 10).unwrap();
        assert_eq!(m["loss"]["count"], 2);
        assert_eq!(m["loss"]["last"], 2.0);
        assert_eq!(s.artifacts("r").unwrap()[0]["path"], "p");
        let run = s.get_run("r").unwrap().unwrap();
        assert_eq!(run["progress"]["done"], 2.0);
        assert!(run["pid"].is_number());
        let log: Vec<String> = s
            .log_tail("r", 100)
            .unwrap()
            .iter()
            .map(|l| format!("{}:{}", l["level"].as_str().unwrap(), l["message"].as_str().unwrap()))
            .collect();
        assert!(log.contains(&"stdout:a stray line".to_string()), "{log:?}");
        assert!(log.contains(&"stderr:to stderr".to_string()), "{log:?}");
        let _ = std::fs::remove_dir_all(d);
    }

    #[tokio::test]
    async fn stdin_reaches_the_worker() {
        let (s, d) = store();
        let o = exec(&s, "r", &helper("stdin", Some(r#"{"seed": 7}"#)), &Ctl::default(), None).await;
        assert_eq!(o.result, Some(json!({"got": {"seed": 7}})), "{o:?}");
        let _ = std::fs::remove_dir_all(d);
    }

    #[tokio::test]
    async fn errors_and_missing_results_are_failures_with_a_reason() {
        let (s, d) = store();
        let o = exec(&s, "r", &helper("fail", None), &Ctl::default(), None).await;
        assert!(!o.success(true));
        assert_eq!(o.failure(true), ("bad".to_string(), "it broke".to_string()));
        let o = exec(&s, "r", &helper("silent", None), &Ctl::default(), None).await;
        assert_eq!(o.exit_code, Some(0));
        assert!(!o.success(true) && o.success(false));
        assert_eq!(o.failure(true).0, "no_result");
        let mut missing = helper("emit", None);
        missing.program = d.join("no-such-program");
        let o = exec(&s, "r", &missing, &Ctl::default(), None).await;
        assert_eq!(o.failure(true).0, "spawn");
        let _ = std::fs::remove_dir_all(d);
    }

    #[tokio::test]
    async fn a_timeout_stops_the_process() {
        let (s, d) = store();
        let mut c = helper("leaf", None);
        c.timeout = Some(Duration::from_millis(600));
        let t = Instant::now();
        let o = exec(&s, "r", &c, &Ctl::default(), None).await;
        assert!(o.timed_out && !o.success(true), "{o:?}");
        assert!(t.elapsed() < Duration::from_secs(10));
        let _ = std::fs::remove_dir_all(d);
    }

    fn alive(pid: u32) -> bool {
        use sysinfo::{Pid, System};
        let mut sys = System::new();
        sys.refresh_processes();
        sys.process(Pid::from_u32(pid)).is_some()
    }

    #[tokio::test]
    async fn cancel_kills_the_whole_process_tree() {
        let (s, d) = store();
        let s = Arc::new(s);
        let ctl = Arc::new(Ctl::default());
        let (s2, c2) = (s.clone(), ctl.clone());
        let task = tokio::spawn(async move { exec(&s2, "r", &helper("tree", None), &c2, None).await });
        // wait for the grandchild's pid to be reported
        let mut gpid = None;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            gpid = s
                .log_tail("r", 50)
                .unwrap()
                .iter()
                .find_map(|l| l["message"].as_str()?.strip_prefix("grandchild ")?.parse::<u32>().ok());
            if gpid.is_some() {
                break;
            }
        }
        let gpid = gpid.expect("the worker did not report its grandchild");
        assert!(alive(gpid));
        ctl.cancel();
        let o = tokio::time::timeout(Duration::from_secs(20), task)
            .await
            .unwrap()
            .unwrap();
        assert!(o.cancelled && !o.success(true), "{o:?}");
        assert_eq!(o.failure(true).0, "cancelled");
        for _ in 0..50 {
            if !alive(gpid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(!alive(gpid), "the grandchild survived the cancel");
        let _ = std::fs::remove_dir_all(d);
    }
}

//! The app's hub lifecycle: use the hub that already runs (found through its control file), else start the
//! `model-mistress` program that ships next to the app, and stop it again on quit only if the app started it.
//!
//! The decisions (`plan`, `should_stop`, the control file and config parsing) are plain functions with tests;
//! `Shell` does the process work around them.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use model_mistress::client::Client;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::watch;

/// What `<home>/control.json` says about a hub.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Control {
    pub url: String,
    pub token: String,
    pub pid: Option<u32>,
}

pub fn parse_control(bytes: &[u8]) -> Option<Control> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    Some(Control {
        url: v["url"].as_str()?.trim_end_matches('/').to_string(),
        token: v["token"].as_str()?.to_string(),
        pid: v["pid"].as_u64().map(|p| p as u32),
    })
}

pub fn read_control(home: &Path) -> Option<Control> {
    parse_control(&std::fs::read(home.join("control.json")).ok()?)
}

/// What to do at start.
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    /// A hub answers: use it, leave it running at quit.
    Attach,
    /// Start this program as the hub.
    Spawn(PathBuf),
    /// No program shipped with the app (a development build): run the hub inside the app.
    Embedded,
}

pub fn plan(hub_answers: bool, bundled: Option<PathBuf>) -> Plan {
    match (hub_answers, bundled) {
        (true, _) => Plan::Attach,
        (false, Some(bin)) => Plan::Spawn(bin),
        (false, None) => Plan::Embedded,
    }
}

/// `model-mistress(.exe)` in the folder of the app's own executable.
pub fn bundled_hub(app_exe: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "model-mistress.exe"
    } else {
        "model-mistress"
    };
    let p = app_exe.parent()?.join(name);
    p.is_file().then_some(p)
}

/// A hub the app did not start is never stopped; one it started is, unless the user asked to keep it running.
pub fn should_stop(started_by_app: bool, stop_on_quit: bool) -> bool {
    started_by_app && stop_on_quit
}

/// The app's own settings, `<home>/desktop.json`: `{"stop_hub_on_quit": true}`. `MM_STOP_HUB_ON_QUIT=0|1` wins.
pub fn stop_on_quit(config: Option<&[u8]>, env: Option<&str>) -> bool {
    if let Some(e) = env {
        return !matches!(e.trim(), "0" | "false" | "no" | "off");
    }
    config
        .and_then(|b| serde_json::from_slice::<Value>(b).ok())
        .and_then(|v| v["stop_hub_on_quit"].as_bool())
        .unwrap_or(true)
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "phase", rename_all = "lowercase")]
pub enum Phase {
    Starting,
    Ready { url: String, started_by_app: bool },
    Failed { message: String },
}

pub struct Shell {
    pub home: PathBuf,
    child: Mutex<Option<Child>>,
    phase: watch::Sender<Phase>,
}

impl Shell {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            child: Mutex::new(None),
            phase: watch::channel(Phase::Starting).0,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase.borrow().clone()
    }

    fn set(&self, p: Phase) {
        self.phase.send_replace(p);
    }

    pub fn started_by_app(&self) -> bool {
        self.child.lock().unwrap().is_some()
    }

    /// Wait until the hub is up (or the start failed) and say where it is.
    pub async fn ready(&self) -> Result<Control> {
        let mut rx = self.phase.subscribe();
        loop {
            let p = rx.borrow_and_update().clone();
            match p {
                Phase::Ready { .. } => {
                    return read_control(&self.home).ok_or_else(|| anyhow!("the hub's control file is gone"))
                }
                Phase::Failed { message } => bail!(message),
                Phase::Starting => {}
            }
            rx.changed().await.map_err(|_| anyhow!("the app is closing"))?;
        }
    }

    /// Find or start the hub; the outcome is the phase. `embedded` runs the in-process hub (development builds).
    pub async fn start(&self, app_exe: Option<&Path>, embedded: impl std::future::Future<Output = Result<()>>) {
        self.set(Phase::Starting);
        match self.start_inner(app_exe, embedded).await {
            Ok(started) => {
                let url = read_control(&self.home).map(|c| c.url).unwrap_or_default();
                self.set(Phase::Ready {
                    url,
                    started_by_app: started,
                });
            }
            Err(e) => self.set(Phase::Failed {
                message: format!("{e:#}"),
            }),
        }
    }

    async fn start_inner(
        &self,
        app_exe: Option<&Path>,
        embedded: impl std::future::Future<Output = Result<()>>,
    ) -> Result<bool> {
        let answers = hub_answers(&self.home).await;
        match plan(answers, app_exe.and_then(bundled_hub)) {
            Plan::Attach => Ok(false),
            Plan::Embedded => {
                embedded.await?;
                Ok(false)
            }
            Plan::Spawn(bin) => {
                self.spawn(&bin).await?;
                Ok(true)
            }
        }
    }

    async fn spawn(&self, bin: &Path) -> Result<()> {
        std::fs::create_dir_all(&self.home)?;
        let _ = std::fs::remove_file(self.home.join("control.json")); // a dead hub's
        let log_path = self.home.join("desktop-hub.log");
        let log = std::fs::File::create(&log_path).with_context(|| format!("cannot write {}", log_path.display()))?;
        let mut cmd = Command::new(bin);
        cmd.args(["serve", "--host", "127.0.0.1", "--port", "0"])
            .env("MM_HOME", &self.home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let child = cmd
            .spawn()
            .with_context(|| format!("cannot start the hub program {}", bin.display()))?;
        let pid = child.id();
        *self.child.lock().unwrap() = Some(child);
        let deadline = Instant::now() + Duration::from_secs(90);
        while Instant::now() < deadline {
            if let Some(status) = self
                .child
                .lock()
                .unwrap()
                .as_mut()
                .and_then(|c| c.try_wait().ok().flatten())
            {
                *self.child.lock().unwrap() = None;
                bail!("the hub program exited at start ({status}).\n{}", log_tail(&log_path));
            }
            if read_control(&self.home).is_some_and(|c| c.pid == Some(pid)) && hub_answers(&self.home).await {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        self.kill_child();
        bail!(
            "the hub did not answer /v1/health within 90 seconds.\n{}",
            log_tail(&log_path)
        )
    }

    fn kill_child(&self) {
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    /// Stop the hub this app started (gracefully, so it unloads its models; killed if it does not leave in 10 s).
    /// A hub the app did not start is left alone. Returns whether one was stopped.
    pub async fn stop_owned(&self) -> bool {
        if !self.started_by_app() {
            return false;
        }
        if let Some(c) = read_control(&self.home) {
            let _ = Client::new(&c.url, &c.token).stop().await;
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let gone = {
                let mut g = self.child.lock().unwrap();
                match g.as_mut() {
                    Some(c) => c.try_wait().ok().flatten().is_some(),
                    None => true,
                }
            };
            if gone {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.kill_child();
        let _ = std::fs::remove_file(self.home.join("control.json"));
        true
    }

    /// At quit: stop the hub if policy says so.
    pub async fn on_quit(&self) {
        note(&self.home, &format!("quit: started_by_app={}", self.started_by_app()));
        let cfg = std::fs::read(self.home.join("desktop.json")).ok();
        let env = std::env::var("MM_STOP_HUB_ON_QUIT").ok();
        if should_stop(self.started_by_app(), stop_on_quit(cfg.as_deref(), env.as_deref())) {
            let stopped = self.stop_owned().await;
            note(&self.home, &format!("quit: hub stopped={stopped}"));
        }
    }
}

/// One line in `<home>/desktop-app.log`, for finding out afterwards what the app did.
pub fn note(home: &Path, line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join("desktop-app.log"))
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Does the hub named by the control file answer `/v1/health`?
pub async fn hub_answers(home: &Path) -> bool {
    let Some(c) = read_control(home) else { return false };
    let Ok(http) = reqwest::Client::builder().timeout(Duration::from_secs(3)).build() else {
        return false;
    };
    match http.get(format!("{}/v1/health", c.url)).send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

fn log_tail(p: &Path) -> String {
    let text = std::fs::read_to_string(p).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(8)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_file_is_parsed() {
        let c = parse_control(br#"{"url":"http://127.0.0.1:5000/","token":"t","pid":7,"api":1}"#).unwrap();
        assert_eq!(
            (c.url.as_str(), c.token.as_str(), c.pid),
            ("http://127.0.0.1:5000", "t", Some(7))
        );
        assert!(parse_control(br#"{"url":"x"}"#).is_none());
        assert!(parse_control(b"not json").is_none());
    }

    #[test]
    fn a_running_hub_is_used_before_anything_is_started() {
        let bin = Some(PathBuf::from("model-mistress.exe"));
        assert_eq!(plan(true, bin.clone()), Plan::Attach);
        assert_eq!(
            plan(false, bin.clone()),
            Plan::Spawn(PathBuf::from("model-mistress.exe"))
        );
        assert_eq!(plan(false, None), Plan::Embedded);
    }

    #[test]
    fn only_a_hub_the_app_started_is_stopped() {
        assert!(should_stop(true, true));
        assert!(!should_stop(true, false));
        assert!(!should_stop(false, true));
        assert!(!should_stop(false, false));
    }

    #[test]
    fn stop_on_quit_is_configurable() {
        assert!(stop_on_quit(None, None));
        assert!(!stop_on_quit(Some(br#"{"stop_hub_on_quit": false}"#), None));
        assert!(stop_on_quit(Some(b"garbage"), None));
        assert!(!stop_on_quit(Some(br#"{"stop_hub_on_quit": true}"#), Some("0")));
        assert!(stop_on_quit(Some(br#"{"stop_hub_on_quit": false}"#), Some("1")));
    }

    #[test]
    fn the_bundled_hub_is_the_program_next_to_the_app() {
        let d = model_mistress::testing::TestDir::new("mm-desktop-test");
        let exe = d.join("app.exe");
        assert!(bundled_hub(&exe).is_none());
        let name = if cfg!(windows) {
            "model-mistress.exe"
        } else {
            "model-mistress"
        };
        std::fs::write(d.join(name), b"x").unwrap();
        assert_eq!(bundled_hub(&exe), Some(d.join(name)));
    }

    #[tokio::test]
    async fn a_missing_hub_does_not_answer_and_ready_reports_failure() {
        let d = model_mistress::testing::TestDir::new("mm-desktop-test-home");
        assert!(!hub_answers(&d).await);
        let shell = Shell::new(d.clone());
        shell
            .start(None, async { Err(anyhow!("no hub could be started")) })
            .await;
        let e = shell.ready().await.unwrap_err();
        assert!(format!("{e}").contains("no hub could be started"));
        assert!(!shell.stop_owned().await, "nothing was started, so nothing is stopped");
    }
}

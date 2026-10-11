//! A long-running Forge worker that serves a model for generation (`generate_serve`): one request line in, one result
//! out, matched by id. The worker is a run like any other (its log and status are in the run store); stopping the
//! server kills its process tree.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};

use super::protocol::{parse_line, Event};
use super::runner::{pump_lines, spawn_with, Cmd, Group};
use super::store::Store;
use crate::engine::Fail;

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>;

pub struct Server {
    pub run: String,
    /// What the worker said about itself when it was ready (device, dtype, parameters...).
    pub info: Value,
    tx: mpsc::UnboundedSender<String>,
    pending: Pending,
    next: AtomicU64,
    alive: Arc<AtomicBool>,
    group: Arc<Option<Group>>,
}

impl Server {
    /// Starts the worker (`cmd.stdin` is its first line, the arguments) and waits until it says it is ready.
    pub async fn start(store: Arc<Store>, run: String, cmd: Cmd, ready_timeout: Duration) -> Result<Arc<Self>, Fail> {
        let (mut child, group) = spawn_with(&cmd, true).map_err(Fail::unavailable)?;
        let _ = store.set_pid(&run, child.id());
        let group = Arc::new(group);
        let pending: Pending = Default::default();
        let alive = Arc::new(AtomicBool::new(true));
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let mut stdin = child.stdin.take().expect("piped");
        let first = cmd.stdin.clone().unwrap_or_else(|| "{}".into());
        tokio::spawn(async move {
            let _ = stdin
                .write_all(format!("{}\n", first.replace('\n', " ")).as_bytes())
                .await;
            while let Some(line) = rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() || stdin.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });
        let (ltx, mut lrx) = mpsc::unbounded_channel::<(bool, String)>();
        if let Some(o) = child.stdout.take() {
            pump_lines(o, false, ltx.clone());
        }
        if let Some(e) = child.stderr.take() {
            pump_lines(e, true, ltx.clone());
        }
        drop(ltx);
        let (ready_tx, ready_rx) = oneshot::channel::<Result<Value, String>>();
        let ready_tx = Mutex::new(Some(ready_tx));
        let (p2, a2, s2, r2) = (pending.clone(), alive.clone(), store.clone(), run.clone());
        tokio::spawn(async move {
            let mut tail: Vec<String> = vec![];
            while let Some((is_err, line)) = lrx.recv().await {
                tail.push(line.clone());
                if tail.len() > 20 {
                    tail.remove(0);
                }
                let ev = if is_err { Event::Text(line) } else { parse_line(&line) };
                match ev {
                    Event::Result(v) => {
                        let id = v.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                        if id == "init" {
                            if let Some(t) = ready_tx.lock().take() {
                                let _ = t.send(Ok(v));
                            }
                        } else if let Some(w) = p2.lock().remove(&id) {
                            let _ = w.send(v);
                        }
                    }
                    Event::Error { message, .. } => {
                        let _ = s2.add_log(&r2, "error", &message);
                        if let Some(t) = ready_tx.lock().take() {
                            let _ = t.send(Err(message));
                        }
                    }
                    Event::Log { level, message } => {
                        let _ = s2.add_log(&r2, &level, &message);
                    }
                    Event::Text(t) => {
                        let _ = s2.add_log(&r2, if is_err { "stderr" } else { "stdout" }, &t);
                    }
                    _ => {}
                }
            }
            let code = child.wait().await.ok().and_then(|s| s.code());
            a2.store(false, Ordering::Relaxed);
            if let Some(t) = ready_tx.lock().take() {
                let _ = t.send(Err(format!(
                    "the worker exited ({code:?}) before it was ready\n{}",
                    tail.join("\n")
                )));
            }
            // answer whoever still waits
            for (_, w) in p2.lock().drain() {
                let _ = w.send(json!({"error": "the worker exited"}));
            }
            let _ = s2.finish(&r2, "done", code, None, None, None);
        });
        let fail = |group: &Arc<Option<Group>>, msg: String| {
            if let Some(g) = group.as_ref() {
                g.kill();
            }
            let _ = store.finish(&run, "failed", None, None, Some(&msg), Some("failed"));
            Fail::new(500, "failed", msg)
        };
        let info = match tokio::time::timeout(ready_timeout, ready_rx).await {
            Ok(Ok(Ok(v))) => v,
            Ok(Ok(Err(m))) => return Err(fail(&group, m)),
            Ok(Err(_)) => return Err(fail(&group, "the worker ended before it was ready".into())),
            Err(_) => return Err(fail(&group, "the worker was not ready in time".into())),
        };
        if let Some(e) = info.get("error").and_then(Value::as_str) {
            return Err(fail(&group, e.to_string()));
        }
        Ok(Arc::new(Self {
            run,
            info,
            tx,
            pending,
            next: AtomicU64::new(1),
            alive,
            group,
        }))
    }

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// One request, one answer. The answer has `error` if the worker could not do it.
    pub async fn request(&self, mut req: Value, timeout: Duration) -> Result<Value, Fail> {
        if !self.alive() {
            return Err(Fail::new(409, "not_running", "the model's worker is not running"));
        }
        let id = format!("q{}", self.next.fetch_add(1, Ordering::Relaxed));
        req["id"] = json!(id);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id.clone(), tx);
        if self.tx.send(req.to_string()).is_err() {
            self.pending.lock().remove(&id);
            return Err(Fail::new(409, "not_running", "the model's worker is not running"));
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(v)) => match v.get("error").and_then(Value::as_str) {
                Some(e) => Err(Fail::new(400, "generate_failed", e.to_string())),
                None => Ok(v),
            },
            _ => {
                self.pending.lock().remove(&id);
                Err(Fail::new(504, "timeout", "the worker did not answer in time"))
            }
        }
    }

    /// Stops the worker and everything it started.
    pub fn stop(&self) {
        if let Some(g) = self.group.as_ref() {
            g.kill();
        }
        self.alive.store(false, Ordering::Relaxed);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

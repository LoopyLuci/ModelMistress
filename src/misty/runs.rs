//! Live runs: the event log a client can follow, cancellation, and approvals a run is waiting for.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::{oneshot, Notify};

pub struct Run {
    pub id: String,
    pub started: Instant,
    pub started_unix: i64,
    events: Mutex<Vec<Value>>,
    notify: Notify,
    done: AtomicBool,
    cancel: AtomicBool,
    pending: Mutex<HashMap<String, Waiting>>,
}

/// A tool call that waits for the owner: the channel that carries the answer (taken when answered) and what it is.
type Waiting = (Option<oneshot::Sender<bool>>, Value);

impl Run {
    fn new(id: String) -> Self {
        Self {
            id,
            started: Instant::now(),
            started_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            events: Mutex::new(vec![]),
            notify: Notify::new(),
            done: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            pending: Mutex::new(HashMap::new()),
        }
    }

    pub fn emit(&self, mut ev: Value) {
        ev["t_ms"] = json!(self.started.elapsed().as_millis() as u64);
        self.events.lock().push(ev);
        self.notify.notify_waiters();
    }

    pub fn finish(&self) {
        self.done.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        // a run that waits for an approval stops waiting
        for (tx, _) in self.pending.lock().values_mut() {
            if let Some(tx) = tx.take() {
                let _ = tx.send(false);
            }
        }
        self.notify.notify_waiters();
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    pub fn events(&self) -> Vec<Value> {
        self.events.lock().clone()
    }

    /// The events after the first `from`, waiting for new ones. `None` when the run is over and all were read.
    pub async fn next(&self, from: usize) -> Option<Vec<Value>> {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let ev = self.events.lock();
                if ev.len() > from {
                    return Some(ev[from..].to_vec());
                }
            }
            if self.is_done() {
                return None;
            }
            notified.await;
        }
    }

    /// Registers an approval and returns its id and the answer channel.
    pub fn ask(&self, info: Value) -> (String, oneshot::Receiver<bool>) {
        let id = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id.clone(), (Some(tx), info));
        (id, rx)
    }

    pub fn forget(&self, approval: &str) {
        self.pending.lock().remove(approval);
    }

    /// The owner's answer. False when there is no such pending approval.
    pub fn answer(&self, approval: &str, allow: bool) -> bool {
        match self.pending.lock().get_mut(approval).and_then(|(tx, _)| tx.take()) {
            Some(tx) => tx.send(allow).is_ok(),
            None => false,
        }
    }

    pub fn pending(&self) -> Vec<Value> {
        self.pending
            .lock()
            .iter()
            .filter(|(_, (tx, _))| tx.is_some())
            .map(|(id, (_, info))| {
                let mut v = info.clone();
                v["approval"] = json!(id);
                v["run"] = json!(self.id);
                v
            })
            .collect()
    }
}

#[derive(Default)]
pub struct Runs {
    map: Mutex<BTreeMap<String, Arc<Run>>>,
}

impl Runs {
    pub fn create(&self) -> Arc<Run> {
        let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        let run = Arc::new(Run::new(id.clone()));
        let mut m = self.map.lock();
        m.insert(id, run.clone());
        // keep the last 100 finished runs in memory (the database keeps them all)
        let finished: Vec<String> = m.iter().filter(|(_, r)| r.is_done()).map(|(k, _)| k.clone()).collect();
        if finished.len() > 100 {
            for k in finished.iter().take(finished.len() - 100) {
                m.remove(k);
            }
        }
        run
    }

    pub fn get(&self, id: &str) -> Option<Arc<Run>> {
        self.map.lock().get(id).cloned()
    }

    pub fn active(&self) -> Vec<Arc<Run>> {
        self.map.lock().values().filter(|r| !r.is_done()).cloned().collect()
    }

    pub fn pending(&self) -> Vec<Value> {
        self.active().iter().flat_map(|r| r.pending()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_follower_gets_old_and_new_events_and_ends_when_the_run_does() {
        let runs = Runs::default();
        let run = runs.create();
        run.emit(json!({"type": "a"}));
        let first = run.next(0).await.unwrap();
        assert_eq!(first.len(), 1);
        let r2 = run.clone();
        let waiter = tokio::spawn(async move { r2.next(1).await });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        run.emit(json!({"type": "b"}));
        let got = waiter.await.unwrap().unwrap();
        assert_eq!(got[0]["type"], "b");
        run.finish();
        assert!(run.next(2).await.is_none());
        assert_eq!(run.events().len(), 2);
        assert!(runs.get(&run.id).is_some() && runs.active().is_empty());
    }

    #[tokio::test]
    async fn an_approval_is_answered_once_and_cancel_denies_what_is_waiting() {
        let run = Runs::default().create();
        let (id, rx) = run.ask(json!({"tool": "fs_write"}));
        assert_eq!(run.pending().len(), 1);
        assert!(run.answer(&id, true));
        assert!(rx.await.unwrap());
        assert!(!run.answer(&id, false), "answered once");
        assert!(!run.answer("nope", true));
        let (_, rx2) = run.ask(json!({"tool": "shell_run"}));
        run.cancel();
        assert!(!rx2.await.unwrap());
        assert!(run.cancelled());
    }
}

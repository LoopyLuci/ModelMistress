//! The approval queue for loads that a peer key asks for (docs/agents.md, item 10).
//!
//! A peer key cannot load a model. It can ask: `POST /v1/requests` queues a request, and nothing loads. The owner lists
//! the queue (`request.list`) and then approves (`request.approve`, which loads the model) or denies it (`request.deny`).
//! The queue is kept in `<home>/requests.json`: every pending request, and the newest `KEEP` decided ones.

use crate::engine::Fail;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Decided requests kept in the file. Pending ones are always kept.
pub const KEEP: usize = 200;
/// Requests one key may have waiting at once. Asking again for a model it already waits for returns the same request.
pub const PENDING_PER_KEY: usize = 20;
/// The longest reason a key may give, in characters.
const REASON_MAX: usize = 500;
const FILE: &str = "requests.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestState {
    Pending,
    Approved,
    Denied,
}

impl RequestState {
    /// The state a `request.list` filter names: `all` is no filter (None).
    pub fn filter(name: &str) -> Result<Option<RequestState>, Fail> {
        match name {
            "pending" => Ok(Some(RequestState::Pending)),
            "approved" => Ok(Some(RequestState::Approved)),
            "denied" => Ok(Some(RequestState::Denied)),
            "all" => Ok(None),
            other => Err(Fail::invalid(format!(
                "state is pending, approved, denied or all, not {other}"
            ))),
        }
    }

    fn word(self) -> &'static str {
        match self {
            RequestState::Pending => "pending",
            RequestState::Approved => "approved",
            RequestState::Denied => "denied",
        }
    }
}

/// One request a peer key made for a model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    /// The model, by the name the key used.
    pub model: String,
    /// The peer key's name: who asked.
    pub peer: String,
    /// The key's own reason, if it gave one.
    pub reason: String,
    pub state: RequestState,
    /// Unix seconds when the key asked, and when the owner decided.
    pub asked_at: i64,
    pub decided_at: Option<i64>,
    /// The owner's reason for a denial. Empty otherwise.
    pub note: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    /// The number the next request gets.
    next: u64,
    /// Oldest first.
    requests: Vec<Request>,
}

/// The queue of requests. `Default` is a queue kept in memory only (tests).
#[derive(Default)]
pub struct Queue {
    path: Option<PathBuf>,
    st: Mutex<Stored>,
}

impl Queue {
    /// The queue of a hub home. A file that cannot be read is set aside as `requests.json.bad`, and the queue starts empty.
    pub fn open(home: &Path) -> Self {
        let path = home.join(FILE);
        let st = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                eprintln!("requests.json is not readable ({e}); it is set aside as requests.json.bad");
                let _ = std::fs::rename(&path, path.with_extension("json.bad"));
                Stored::default()
            }),
            Err(_) => Stored::default(),
        };
        Self {
            path: Some(path),
            st: Mutex::new(st),
        }
    }

    /// A key asks for a model: a new pending request, or the one it already waits with for that model. Nothing loads.
    pub fn ask(&self, peer: &str, model: &str, reason: &str, now: i64) -> Result<Request, Fail> {
        let mut st = self.st.lock();
        if let Some(r) = st
            .requests
            .iter()
            .find(|r| r.state == RequestState::Pending && r.peer == peer && r.model == model)
        {
            return Ok(r.clone());
        }
        let waiting = st
            .requests
            .iter()
            .filter(|r| r.state == RequestState::Pending && r.peer == peer)
            .count();
        if waiting >= PENDING_PER_KEY {
            return Err(Fail::new(
                429,
                "too_many_requests",
                format!(
                    "this key has {waiting} requests waiting for the owner; wait for an answer before asking again"
                ),
            ));
        }
        st.next += 1;
        let request = Request {
            id: st.next,
            model: model.to_string(),
            peer: peer.to_string(),
            reason: reason.trim().chars().take(REASON_MAX).collect(),
            state: RequestState::Pending,
            asked_at: now,
            decided_at: None,
            note: String::new(),
        };
        st.requests.push(request.clone());
        trim(&mut st);
        self.save(&st);
        Ok(request)
    }

    /// The requests with this state (all with None), newest first.
    pub fn list(&self, state: Option<RequestState>) -> Vec<Request> {
        let st = self.st.lock();
        st.requests
            .iter()
            .rev()
            .filter(|r| state.is_none_or(|s| r.state == s))
            .cloned()
            .collect()
    }

    pub fn get(&self, id: u64) -> Option<Request> {
        self.st.lock().requests.iter().find(|r| r.id == id).cloned()
    }

    /// The owner's decision on a pending request. A request that is already decided cannot be decided again.
    pub fn decide(&self, id: u64, state: RequestState, note: &str, now: i64) -> Result<Request, Fail> {
        let mut st = self.st.lock();
        let Some(r) = st.requests.iter_mut().find(|r| r.id == id) else {
            return Err(Fail::not_found(format!("no request {id} (request.list shows them)")));
        };
        if r.state != RequestState::Pending {
            return Err(Fail::new(
                409,
                "not_pending",
                format!("request {id} is already {}", r.state.word()),
            ));
        }
        r.state = state;
        r.decided_at = Some(now);
        r.note = note.trim().chars().take(REASON_MAX).collect();
        let decided = r.clone();
        trim(&mut st);
        self.save(&st);
        Ok(decided)
    }

    /// Written whole, to a temporary file first. A write that fails is reported on stderr; the queue keeps working in memory.
    fn save(&self, st: &Stored) {
        let Some(path) = &self.path else { return };
        let tmp = path.with_extension("json.tmp");
        let written = serde_json::to_vec_pretty(st)
            .map_err(std::io::Error::other)
            .and_then(|bytes| std::fs::write(&tmp, bytes))
            .and_then(|_| std::fs::rename(&tmp, path));
        if let Err(e) = written {
            eprintln!("requests.json could not be written: {e}");
        }
    }
}

/// Keep every pending request and the newest `KEEP` decided ones (the oldest decided go first).
fn trim(st: &mut Stored) {
    let decided = st.requests.iter().filter(|r| r.state != RequestState::Pending).count();
    if decided <= KEEP {
        return;
    }
    let mut drop = decided - KEEP;
    st.requests.retain(|r| {
        if r.state != RequestState::Pending && drop > 0 {
            drop -= 1;
            false
        } else {
            true
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_waits_and_asking_again_for_the_same_model_returns_it() {
        let q = Queue::default();
        let a = q.ask("agent", "alpha", "summary pass", 10).unwrap();
        assert_eq!(
            (a.state, a.model.as_str(), a.peer.as_str()),
            (RequestState::Pending, "alpha", "agent")
        );
        let b = q.ask("agent", "alpha", "again", 11).unwrap();
        assert_eq!(a.id, b.id, "the same pending request, not a second one");
        assert_eq!(q.list(Some(RequestState::Pending)).len(), 1);
    }

    #[test]
    fn each_key_has_its_own_cap_on_waiting_requests() {
        let q = Queue::default();
        for i in 0..PENDING_PER_KEY {
            q.ask("agent", &format!("m{i}"), "", 1).unwrap();
        }
        let refused = q.ask("agent", "one-more", "", 2).unwrap_err();
        assert_eq!((refused.status, refused.code), (429, "too_many_requests"));
        q.ask("other", "one-more", "", 2).expect("another key is not held up");
    }

    #[test]
    fn only_a_pending_request_can_be_decided_and_the_answer_is_kept() {
        let q = Queue::default();
        let r = q.ask("agent", "alpha", "", 1).unwrap();
        let d = q.decide(r.id, RequestState::Denied, " not this week ", 5).unwrap();
        assert_eq!(
            (d.state, d.note.as_str(), d.decided_at),
            (RequestState::Denied, "not this week", Some(5))
        );
        assert_eq!(
            q.decide(r.id, RequestState::Approved, "", 6).unwrap_err().code,
            "not_pending"
        );
        assert_eq!(q.decide(999, RequestState::Approved, "", 6).unwrap_err().status, 404);
        assert!(q.list(Some(RequestState::Pending)).is_empty());
        assert_eq!(q.list(None).len(), 1);
    }

    #[test]
    fn the_queue_is_kept_in_the_home_and_the_history_is_trimmed_but_pending_stays() {
        let dir = crate::testing::TestDir::new("mm-requests");
        let q = Queue::open(&dir);
        let waiting = q.ask("agent", "keep-me", "", 1).unwrap();
        for i in 0..(KEEP + 5) {
            let r = q.ask("other", &format!("m{i}"), "", i as i64).unwrap();
            q.decide(r.id, RequestState::Approved, "", i as i64).unwrap();
        }
        let decided = q.list(None).iter().filter(|r| r.state != RequestState::Pending).count();
        assert_eq!(decided, KEEP, "the oldest decided requests go first");
        assert!(q.get(waiting.id).is_some(), "a pending request is never dropped");
        let reopened = Queue::open(&dir);
        assert_eq!(reopened.get(waiting.id).unwrap().model, "keep-me");
        assert_eq!(reopened.list(None).len(), KEEP + 1);
    }

    #[test]
    fn the_list_filter_names_the_states() {
        assert_eq!(RequestState::filter("pending").unwrap(), Some(RequestState::Pending));
        assert_eq!(RequestState::filter("all").unwrap(), None);
        assert_eq!(RequestState::filter("maybe").unwrap_err().code, "invalid");
    }
}

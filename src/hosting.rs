//! Hosting models for other machines: per-peer API keys, their quotas and allow-lists, and usage accounting.
//!
//! A key is a random secret (`mmk_...`) shown once, when it is made. `<home>/keys.json` keeps only its SHA-256 hash,
//! the peer it names, its quotas and the models it may use. The hub's own token (the owner) is not limited by any of
//! this; a peer key can use the OpenAI routes (models, chat, completions, embeddings, rerank, audio) and nothing else.
//!
//! Quotas: requests per minute (a sliding minute), tokens per day (a UTC day; the request that crosses the line is
//! served and the next ones are refused, because tokens are only known afterwards) and concurrent requests (a
//! streamed answer counts until its last byte). A refused request is a 429 with `Retry-After`. A revoked key stops
//! working at once. Usage is kept in `<home>/usage.json`, also for revoked keys.
//!
//! Every change to the keys is recorded in `keys.json` beside them, with who made it (the hub, or an offline command),
//! and saved in the same write. A change reads `keys.json` first, so one made by another process is kept rather than
//! overwritten, and a `keys.json` that cannot be read refuses key changes instead of being replaced.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Quotas {
    pub requests_per_min: Option<u32>,
    pub tokens_per_day: Option<u64>,
    pub concurrent: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyRec {
    id: String,
    peer: String,
    /// SHA-256 of the secret, in hex.
    hash: String,
    created: String,
    #[serde(default)]
    revoked: Option<String>,
    #[serde(default)]
    quotas: Quotas,
    /// Model ids this key may use: exact (case-insensitive) or `prefix*`; empty = every model.
    #[serde(default)]
    models: Vec<String>,
}

/// One change to the keys, kept in keys.json with them.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyEvent {
    at: String,
    /// `created` or `revoked`.
    event: String,
    key: String,
    peer: String,
    /// Who made the change: `hub` (the running hub) or `offline` (a command, with no hub answering).
    via: String,
}

/// The shape of keys.json.
#[derive(Deserialize)]
struct KeysFile {
    #[serde(default)]
    keys: Vec<KeyRec>,
    #[serde(default)]
    events: Vec<KeyEvent>,
}

/// A file's modified time and size: they change with every write.
type FileStamp = (Option<std::time::SystemTime>, u64);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ModelUse {
    requests: u64,
    tokens: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Usage {
    requests: u64,
    errors: u64,
    /// Requests refused for a quota.
    rejected: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    /// The UTC day `tokens_today` counts.
    day: String,
    tokens_today: u64,
    last_used: Option<String>,
    by_model: BTreeMap<String, ModelUse>,
}

#[derive(Default)]
struct Live {
    window: VecDeque<Instant>,
    inflight: u32,
}

#[derive(Default)]
struct State {
    keys: Vec<KeyRec>,
    events: Vec<KeyEvent>,
    /// keys.json as this hub last read or wrote it (None: no file).
    stamp: Option<FileStamp>,
    /// Why keys.json cannot be read, when it cannot. Key changes are refused until it can.
    unreadable: Option<String>,
    usage: HashMap<String, Usage>,
    live: HashMap<String, Live>,
    dirty: bool,
}

pub struct Keys {
    /// None: kept in memory only (tests).
    home: Option<PathBuf>,
    st: Mutex<State>,
}

/// What a presented key came to.
pub enum Verdict {
    Unknown,
    Revoked,
    /// A quota says no; try again in `retry_after` seconds.
    Rejected {
        retry_after: u64,
        code: &'static str,
        message: String,
    },
    Admitted(Arc<Permit>),
}

/// One admitted request of a peer. Dropping it ends the request for the concurrency quota.
pub struct Permit {
    keys: Arc<Keys>,
    pub id: String,
    pub peer: String,
    models: Vec<String>,
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

fn secs_to_midnight() -> u64 {
    use chrono::Timelike;
    let n = chrono::Utc::now();
    86_400u64.saturating_sub(n.num_seconds_from_midnight() as u64).max(1)
}

pub fn hash_of(secret: &str) -> String {
    let d = Sha256::digest(secret.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Does `pattern` (exact, or ending in `*`) name `model`? Case-insensitive.
pub fn pattern_matches(pattern: &str, model: &str) -> bool {
    let (p, m) = (pattern.trim().to_lowercase(), model.trim().to_lowercase());
    match p.strip_suffix('*') {
        Some(prefix) => m.starts_with(prefix),
        None => p == m,
    }
}

/// A file's stamp; None when there is no file.
fn stamp_of(path: &Path) -> Option<FileStamp> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok(), m.len()))
}

/// The keys and changes in `home`'s keys.json. A missing file is empty; one that does not parse is an error.
fn load(home: &Path) -> Result<(Vec<KeyRec>, Vec<KeyEvent>), String> {
    let path = home.join("keys.json");
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<KeysFile>(&bytes)
            .map(|f| (f.keys, f.events))
            .map_err(|e| format!("{} does not parse: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((vec![], vec![])),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Key changes are refused while keys.json cannot be read, so a damaged file is never replaced by an empty one.
fn refuse_while_unreadable(st: &State) -> Result<(), String> {
    match &st.unreadable {
        Some(e) => Err(format!("no key was changed: {e}; fix or move the file, then retry")),
        None => Ok(()),
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path)
}

impl Keys {
    pub fn in_memory() -> Arc<Self> {
        Arc::new(Self {
            home: None,
            st: Mutex::new(State::default()),
        })
    }

    /// The keys, their changes and usage kept in `home` (none yet: an empty set). A keys.json that cannot be read
    /// leaves the keys empty and refuses key changes (see `unreadable`).
    pub fn open(home: &Path) -> Arc<Self> {
        let stamp = stamp_of(&home.join("keys.json"));
        let (keys, events, unreadable) = match load(home) {
            Ok((keys, events)) => (keys, events, None),
            Err(e) => (vec![], vec![], Some(e)),
        };
        let read = |name: &str| std::fs::read(home.join(name)).ok();
        let usage: HashMap<String, Usage> = read("usage.json")
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Arc::new(Self {
            home: Some(home.to_path_buf()),
            st: Mutex::new(State {
                keys,
                events,
                stamp,
                unreadable,
                usage,
                ..Default::default()
            }),
        })
    }

    /// Why keys.json cannot be read, when it cannot. Key changes are refused until it can.
    pub fn unreadable(&self) -> Option<String> {
        self.st.lock().unreadable.clone()
    }

    /// Take in keys.json now. The hub calls this as it starts serving, so a key written while it was starting counts.
    pub fn refresh(&self) {
        let mut st = self.st.lock();
        self.reload(&mut st);
    }

    /// Take in keys.json if it changed since this hub last read or wrote it, so a change another process made is kept
    /// rather than overwritten by the next write.
    fn reload(&self, st: &mut State) {
        let Some(home) = &self.home else { return };
        let now = stamp_of(&home.join("keys.json"));
        if now == st.stamp {
            return;
        }
        st.stamp = now;
        match load(home) {
            Ok((keys, events)) => {
                st.keys = keys;
                st.events = events;
                st.unreadable = None;
            }
            Err(e) => st.unreadable = Some(e),
        }
    }

    fn save_keys(&self, st: &mut State) -> Result<(), String> {
        let Some(home) = &self.home else { return Ok(()) };
        std::fs::create_dir_all(home).map_err(|e| format!("{}: {e}", home.display()))?;
        let doc = json!({"keys": st.keys, "events": st.events});
        let path = home.join("keys.json");
        write_private(&path, &serde_json::to_vec_pretty(&doc).unwrap_or_default())
            .map_err(|e| format!("cannot save keys.json: {e}"))?;
        st.stamp = stamp_of(&path);
        Ok(())
    }

    /// Write the usage to disk if it changed (the hub calls this every few seconds and on exit).
    pub fn flush(&self) {
        let mut st = self.st.lock();
        if !st.dirty {
            return;
        }
        st.dirty = false;
        if let Some(home) = &self.home {
            let _ = write_private(
                &home.join("usage.json"),
                &serde_json::to_vec(&st.usage).unwrap_or_default(),
            );
        }
    }

    /// Keys that work now.
    pub fn active(&self) -> usize {
        self.st.lock().keys.iter().filter(|k| k.revoked.is_none()).count()
    }

    /// Make a key for `peer`, recorded as made by `via` (`hub` or `offline`). Returns its description and the secret,
    /// which is not kept anywhere.
    pub fn create(
        &self,
        peer: &str,
        quotas: Quotas,
        models: Vec<String>,
        via: &str,
    ) -> Result<(Value, String), String> {
        let peer = peer.trim();
        if peer.is_empty() || peer.len() > 64 || peer.chars().any(|c| c.is_control()) {
            return Err("peer must be a name of 1 to 64 characters".into());
        }
        if quotas.requests_per_min == Some(0) || quotas.tokens_per_day == Some(0) || quotas.concurrent == Some(0) {
            return Err("a quota of 0 would refuse every request; leave it out for no limit".into());
        }
        let models: Vec<String> = models
            .into_iter()
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .collect();
        let secret = format!("mmk_{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        let now = now_rfc3339();
        let rec = KeyRec {
            id: uuid::Uuid::new_v4().simple().to_string()[..8].to_string(),
            peer: peer.to_string(),
            hash: hash_of(&secret),
            created: now.clone(),
            revoked: None,
            quotas,
            models,
        };
        let event = KeyEvent {
            at: now,
            event: "created".into(),
            key: rec.id.clone(),
            peer: rec.peer.clone(),
            via: via.into(),
        };
        let mut guard = self.st.lock();
        let st = &mut *guard;
        self.reload(st);
        refuse_while_unreadable(st)?;
        st.keys.push(rec.clone());
        st.events.push(event);
        if let Err(e) = self.save_keys(st) {
            st.keys.pop();
            st.events.pop();
            return Err(e);
        }
        Ok((describe(&rec), secret))
    }

    pub fn list(&self) -> Vec<Value> {
        self.st.lock().keys.iter().map(describe).collect()
    }

    /// Revoke the key with this id, or every key of the peer with this name, recorded as made by `via`. Returns how
    /// many were revoked now.
    pub fn revoke(&self, key: &str, via: &str) -> Result<usize, String> {
        let mut guard = self.st.lock();
        let st = &mut *guard;
        self.reload(st);
        refuse_while_unreadable(st)?;
        let by_id = st.keys.iter().any(|k| k.id == key);
        let at = now_rfc3339();
        let (keys_before, events_before) = (st.keys.clone(), st.events.len());
        let mut n = 0;
        for k in st.keys.iter_mut() {
            let hit = if by_id {
                k.id == key
            } else {
                k.peer.eq_ignore_ascii_case(key)
            };
            if hit && k.revoked.is_none() {
                k.revoked = Some(at.clone());
                st.events.push(KeyEvent {
                    at: at.clone(),
                    event: "revoked".into(),
                    key: k.id.clone(),
                    peer: k.peer.clone(),
                    via: via.into(),
                });
                n += 1;
            }
        }
        if n > 0 {
            if let Err(e) = self.save_keys(st) {
                st.keys = keys_before;
                st.events.truncate(events_before);
                return Err(e);
            }
        }
        Ok(n)
    }

    /// Every change to the keys, oldest first, with who made it. Err: keys.json cannot be read.
    pub fn log(&self) -> Result<Vec<Value>, String> {
        let st = self.st.lock();
        match &st.unreadable {
            Some(e) => Err(e.clone()),
            None => Ok(st.events.iter().map(|e| json!(e)).collect()),
        }
    }

    /// Usage of the key with this id or of a peer's keys (None: all).
    pub fn usage(&self, key: Option<&str>) -> Vec<Value> {
        let st = self.st.lock();
        st.keys
            .iter()
            .filter(|k| key.is_none_or(|q| k.id == q || k.peer.eq_ignore_ascii_case(q)))
            .map(|k| {
                let u = st.usage.get(&k.id).cloned().unwrap_or_default();
                let live = st.live.get(&k.id);
                let last_min = live
                    .map(|l| {
                        l.window
                            .iter()
                            .filter(|t| t.elapsed() < Duration::from_secs(60))
                            .count()
                    })
                    .unwrap_or(0);
                let tokens_today = if u.day == today() { u.tokens_today } else { 0 };
                json!({"id": k.id, "peer": k.peer, "revoked": k.revoked, "requests": u.requests, "errors": u.errors,
                       "rejected": u.rejected, "prompt_tokens": u.prompt_tokens,
                       "completion_tokens": u.completion_tokens, "tokens_today": tokens_today,
                       "requests_last_minute": last_min, "in_flight": live.map(|l| l.inflight).unwrap_or(0),
                       "quotas": k.quotas, "last_used": u.last_used, "by_model": u.by_model})
            })
            .collect()
    }

    /// Is this the secret of a key (revoked or not)?
    pub fn knows(&self, secret: &str) -> bool {
        let h = hash_of(secret);
        self.st.lock().keys.iter().any(|k| same(&k.hash, &h))
    }

    /// Check a presented secret and its quotas; an admitted request is counted.
    pub fn check(self: &Arc<Self>, secret: &str) -> Verdict {
        let h = hash_of(secret);
        let mut st = self.st.lock();
        let st = &mut *st;
        let Some(k) = st.keys.iter().find(|k| same(&k.hash, &h)).cloned() else {
            return Verdict::Unknown;
        };
        if k.revoked.is_some() {
            return Verdict::Revoked;
        }
        let day = today();
        let u = st.usage.entry(k.id.clone()).or_default();
        if u.day != day {
            u.day = day;
            u.tokens_today = 0;
        }
        let live = st.live.entry(k.id.clone()).or_default();
        let now = Instant::now();
        while live
            .window
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            live.window.pop_front();
        }
        let mut refuse = None;
        if let Some(limit) = k.quotas.tokens_per_day.filter(|l| u.tokens_today >= *l) {
            refuse = Some((
                secs_to_midnight(),
                "token_quota_exceeded",
                format!("this key has used its {limit} tokens for today (UTC)"),
            ));
        } else if let Some(limit) = k.quotas.concurrent.filter(|l| live.inflight >= *l) {
            refuse = Some((
                1,
                "too_many_concurrent_requests",
                format!("this key may have {limit} request(s) at a time"),
            ));
        } else if let Some(limit) = k.quotas.requests_per_min.filter(|l| live.window.len() >= *l as usize) {
            let wait = live
                .window
                .front()
                .map(|t| 60u64.saturating_sub(now.duration_since(*t).as_secs()))
                .unwrap_or(1)
                .max(1);
            refuse = Some((
                wait,
                "rate_limit_exceeded",
                format!("this key may make {limit} request(s) per minute"),
            ));
        }
        if let Some((retry_after, code, message)) = refuse {
            u.rejected += 1;
            st.dirty = true;
            return Verdict::Rejected {
                retry_after,
                code,
                message,
            };
        }
        live.window.push_back(now);
        live.inflight += 1;
        u.requests += 1;
        u.last_used = Some(now_rfc3339());
        st.dirty = true;
        Verdict::Admitted(Arc::new(Permit {
            keys: self.clone(),
            id: k.id,
            peer: k.peer,
            models: k.models,
        }))
    }
}

fn describe(k: &KeyRec) -> Value {
    json!({"id": k.id, "peer": k.peer, "created": k.created, "revoked": k.revoked, "quotas": k.quotas,
           "models": k.models})
}

impl Permit {
    /// May this key use the model? `asked` is the name in the request, `resolved` the id it comes to.
    pub fn allows(&self, asked: &str, resolved: &str) -> bool {
        self.models.is_empty()
            || self
                .models
                .iter()
                .any(|p| pattern_matches(p, asked) || pattern_matches(p, resolved))
    }

    pub fn allow_list(&self) -> &[String] {
        &self.models
    }

    /// The request ended: count its outcome and tokens.
    pub fn finish(&self, model: &str, ok: bool, prompt_tokens: u64, completion_tokens: u64) {
        let mut st = self.keys.st.lock();
        let u = st.usage.entry(self.id.clone()).or_default();
        let day = today();
        if u.day != day {
            u.day = day;
            u.tokens_today = 0;
        }
        if !ok {
            u.errors += 1;
        }
        u.prompt_tokens += prompt_tokens;
        u.completion_tokens += completion_tokens;
        u.tokens_today += prompt_tokens + completion_tokens;
        let m = u.by_model.entry(model.to_string()).or_default();
        m.requests += 1;
        m.tokens += prompt_tokens + completion_tokens;
        st.dirty = true;
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(l) = self.keys.st.lock().live.get_mut(&self.id) {
            l.inflight = l.inflight.saturating_sub(1);
        }
    }
}

/// Listening beyond this machine needs `allow_network = true` in config.toml and at least one working key.
pub fn check_bind(host: std::net::IpAddr, allow_network: bool, active_keys: usize) -> Result<(), String> {
    if host.is_loopback() {
        return Ok(());
    }
    if !allow_network {
        return Err(format!(
            "listening on {host} would open the hub to the network: set allow_network = true in config.toml \
             (or: model-mistress call config.set '{{\"settings\":{{\"allow_network\":true}}}}') and make a key for \
             each peer with `model-mistress keys create <peer>`"
        ));
    }
    if active_keys == 0 {
        return Err(format!(
            "listening on {host} needs at least one peer key: run `model-mistress keys create <peer>` first"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(k: &Arc<Keys>, peer: &str, q: Quotas, models: &[&str]) -> String {
        k.create(peer, q, models.iter().map(|m| m.to_string()).collect(), "hub")
            .unwrap()
            .1
    }

    fn admitted(v: Verdict) -> Arc<Permit> {
        match v {
            Verdict::Admitted(p) => p,
            Verdict::Rejected { code, .. } => panic!("rejected: {code}"),
            Verdict::Revoked => panic!("revoked"),
            Verdict::Unknown => panic!("unknown"),
        }
    }

    #[test]
    fn requests_per_minute_are_limited_with_a_retry_after() {
        let k = Keys::in_memory();
        let s = secret(
            &k,
            "alice",
            Quotas {
                requests_per_min: Some(2),
                ..Default::default()
            },
            &[],
        );
        let (_a, _b) = (admitted(k.check(&s)), admitted(k.check(&s)));
        match k.check(&s) {
            Verdict::Rejected { retry_after, code, .. } => {
                assert_eq!(code, "rate_limit_exceeded");
                assert!((1..=60).contains(&retry_after));
            }
            _ => panic!("the third request must be refused"),
        }
        // the refusal did not use up a slot, and is counted
        assert_eq!(k.usage(Some("alice"))[0]["rejected"], 1);
        assert_eq!(k.usage(Some("alice"))[0]["requests"], 2);
    }

    #[test]
    fn concurrent_requests_are_limited_until_the_permit_drops() {
        let k = Keys::in_memory();
        let s = secret(
            &k,
            "bob",
            Quotas {
                concurrent: Some(1),
                ..Default::default()
            },
            &[],
        );
        let p = admitted(k.check(&s));
        assert!(matches!(
            k.check(&s),
            Verdict::Rejected {
                code: "too_many_concurrent_requests",
                ..
            }
        ));
        drop(p);
        let _again = admitted(k.check(&s));
    }

    #[test]
    fn tokens_per_day_stop_the_next_request() {
        let k = Keys::in_memory();
        let s = secret(
            &k,
            "carol",
            Quotas {
                tokens_per_day: Some(100),
                ..Default::default()
            },
            &[],
        );
        admitted(k.check(&s)).finish("m", true, 60, 50); // crosses the line: served
        match k.check(&s) {
            Verdict::Rejected { retry_after, code, .. } => {
                assert_eq!(code, "token_quota_exceeded");
                assert!((1..=86_400).contains(&retry_after));
            }
            _ => panic!("refused until tomorrow"),
        }
        let u = &k.usage(None)[0];
        assert_eq!(
            (u["tokens_today"].as_u64(), u["prompt_tokens"].as_u64()),
            (Some(110), Some(60))
        );
        assert_eq!(u["by_model"]["m"]["tokens"], 110);
    }

    #[test]
    fn a_revoked_key_stops_at_once_and_keeps_its_usage() {
        let k = Keys::in_memory();
        let s = secret(&k, "dave", Quotas::default(), &[]);
        admitted(k.check(&s)).finish("m", true, 1, 1);
        assert_eq!(k.revoke("dave", "hub").unwrap(), 1);
        assert!(matches!(k.check(&s), Verdict::Revoked));
        assert_eq!(k.revoke("dave", "hub").unwrap(), 0);
        assert_eq!(k.active(), 0);
        assert_eq!(k.usage(Some("dave"))[0]["requests"], 1);
        assert!(matches!(k.check("mmk_nonsense"), Verdict::Unknown));
    }

    #[test]
    fn allow_lists_match_exact_names_and_prefixes() {
        let k = Keys::in_memory();
        let s = secret(&k, "erin", Quotas::default(), &["llama3.2:1b", "qwen*"]);
        let p = admitted(k.check(&s));
        assert!(p.allows("llama3.2:1b", "llama3.2:1b"));
        assert!(p.allows("LLAMA3.2:1B", "x"));
        assert!(p.allows("qwen", "qwen3:8b"));
        assert!(p.allows("q", "Qwen3:8b"));
        assert!(!p.allows("llama3.2", "llama3.1:8b"));
        let open = admitted(k.check(&secret(&k, "frank", Quotas::default(), &[])));
        assert!(open.allows("anything", "anything"));
    }

    #[test]
    fn keys_are_stored_hashed_and_survive_a_restart() {
        let home = crate::testing::TestDir::lazy("mm-keys");
        let k = Keys::open(&home);
        let (info, s) = k
            .create(
                "gina",
                Quotas {
                    concurrent: Some(3),
                    ..Default::default()
                },
                vec!["m".into()],
                "hub",
            )
            .unwrap();
        admitted(k.check(&s)).finish("m", true, 2, 3);
        k.flush();
        let file = std::fs::read_to_string(home.join("keys.json")).unwrap();
        assert!(!file.contains(&s), "the secret is never written");
        assert!(file.contains(&hash_of(&s)) && file.contains("gina"));
        let again = Keys::open(&home);
        assert_eq!(again.list()[0]["id"], info["id"]);
        assert_eq!(again.list()[0]["quotas"]["concurrent"], 3);
        assert_eq!(again.usage(None)[0]["completion_tokens"], 3);
        assert!(matches!(again.check(&s), Verdict::Admitted(_)));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn bad_keys_are_refused() {
        let k = Keys::in_memory();
        assert!(k.create("  ", Quotas::default(), vec![], "hub").is_err());
        assert!(k
            .create(
                "x",
                Quotas {
                    concurrent: Some(0),
                    ..Default::default()
                },
                vec![],
                "hub"
            )
            .is_err());
    }

    #[test]
    fn listening_beyond_loopback_needs_the_setting_and_a_key() {
        let lo: std::net::IpAddr = [127, 0, 0, 1].into();
        let any: std::net::IpAddr = [0, 0, 0, 0].into();
        assert!(check_bind(lo, false, 0).is_ok());
        assert!(check_bind("::1".parse().unwrap(), false, 0).is_ok());
        assert!(check_bind(any, false, 3).unwrap_err().contains("allow_network"));
        assert!(check_bind(any, true, 0).unwrap_err().contains("keys create"));
        assert!(check_bind(any, true, 1).is_ok());
    }

    fn peers(k: &Arc<Keys>) -> Vec<String> {
        k.list()
            .iter()
            .map(|v| v["peer"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn every_key_change_is_recorded_with_who_made_it() {
        let home = crate::testing::TestDir::lazy("mm-keys-log");
        let hub = Keys::open(&home);
        let (info, secret) = hub.create("henry", Quotas::default(), vec![], "hub").unwrap();
        assert_eq!(Keys::open(&home).revoke("henry", "offline").unwrap(), 1);
        let log = Keys::open(&home).log().unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(
            (log[0]["event"].as_str(), log[0]["via"].as_str()),
            (Some("created"), Some("hub"))
        );
        assert_eq!(
            (log[1]["event"].as_str(), log[1]["via"].as_str()),
            (Some("revoked"), Some("offline"))
        );
        assert_eq!(log[0]["key"], info["id"]);
        assert_eq!(log[1]["peer"], "henry");
        let file = std::fs::read_to_string(home.join("keys.json")).unwrap();
        assert!(!file.contains(&secret), "the log never holds a secret");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_hub_write_keeps_a_change_made_offline() {
        let home = crate::testing::TestDir::lazy("mm-keys-offline");
        let hub = Keys::open(&home);
        hub.create("ivy", Quotas::default(), vec![], "hub").unwrap();
        Keys::open(&home)
            .create("jack", Quotas::default(), vec![], "offline")
            .unwrap();
        // The hub writes a key of its own. It reads the file first, so the offline key is still there.
        hub.create("kim", Quotas::default(), vec![], "hub").unwrap();
        assert_eq!(peers(&hub), ["ivy", "jack", "kim"]);
        assert_eq!(peers(&Keys::open(&home)), ["ivy", "jack", "kim"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn an_offline_revoke_is_not_undone_by_a_hub_write() {
        let home = crate::testing::TestDir::lazy("mm-keys-revoke");
        let hub = Keys::open(&home);
        let (_, gone) = hub.create("lena", Quotas::default(), vec![], "hub").unwrap();
        assert_eq!(Keys::open(&home).revoke("lena", "offline").unwrap(), 1);
        // The hub still held the key as active. Its next write reads the file, so the revoke comes with it.
        hub.create("mo", Quotas::default(), vec![], "hub").unwrap();
        assert!(matches!(hub.check(&gone), Verdict::Revoked));
        assert!(matches!(Keys::open(&home).check(&gone), Verdict::Revoked));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_hub_takes_in_a_key_written_while_it_was_starting() {
        let home = crate::testing::TestDir::lazy("mm-keys-start");
        let hub = Keys::open(&home);
        let (_, secret) = Keys::open(&home)
            .create("nina", Quotas::default(), vec![], "offline")
            .unwrap();
        assert!(matches!(hub.check(&secret), Verdict::Unknown));
        hub.refresh();
        assert!(matches!(hub.check(&secret), Verdict::Admitted(_)));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn an_unreadable_keys_file_refuses_key_changes_and_is_not_overwritten() {
        let home = crate::testing::TestDir::lazy("mm-keys-broken");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("keys.json"), b"{ not json").unwrap();
        let k = Keys::open(&home);
        assert!(k.unreadable().unwrap().contains("does not parse"));
        assert!(k
            .create("oscar", Quotas::default(), vec![], "hub")
            .unwrap_err()
            .contains("no key was changed"));
        assert!(k.revoke("oscar", "hub").unwrap_err().contains("no key was changed"));
        assert!(k.log().is_err());
        assert_eq!(std::fs::read(home.join("keys.json")).unwrap(), b"{ not json".to_vec());
        let _ = std::fs::remove_dir_all(&home);
    }
}

//! The trace store: SQLite for traces, stages, events and specialist calls (`<home>/trace/traces.db`), and one compact
//! binary segment file per trace for the per-token records (`<home>/trace/segments/<id>.tok`).
//!
//! Segment file (little endian): `"MMTK"`, version u8 (1), flags u8 (0), reserved u16, then records until the end:
//!
//! ```text
//! token id u32 (u32::MAX = unknown) | logprob f32 | entropy f32 | phase u8 (0 answer, 1 reasoning)
//! | alternative count u8 | text length u16 | text bytes
//! | routing length u16 | routing bytes            (reserved: per-layer expert routing; empty today)
//! | per alternative: id u32 | logprob f32 | text length u16 | text bytes
//! ```
//!
//! Records are self-delimiting, so a file can be appended to while a request is running and a torn last record
//! (a crash) is simply ignored when reading.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use rusqlite::{params_from_iter, Connection, OptionalExtension};
use serde_json::{json, Value};

use super::TraceSettings;

pub const NO_ID: u32 = u32::MAX;
const MAGIC: &[u8; 4] = b"MMTK";
const VERSION: u8 = 1;

/// One alternative the model considered at a position.
#[derive(Debug, Clone, PartialEq)]
pub struct Alt {
    pub id: u32,
    pub text: String,
    pub logprob: f32,
}

/// One generated token, with what the model thought of it.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenRec {
    pub id: u32,
    pub text: String,
    pub logprob: f32,
    /// Entropy (nats) over the returned top-k alternatives; the tail beyond k is not counted.
    pub entropy: f32,
    /// 0 answer, 1 reasoning.
    pub phase: u8,
    pub alts: Vec<Alt>,
    /// Reserved: per-layer expert routing (an opaque blob until a producer defines it).
    pub routing: Vec<u8>,
}

impl TokenRec {
    pub fn to_json(&self, i: usize) -> Value {
        json!({"i": i, "id": if self.id == NO_ID { Value::Null } else { json!(self.id) }, "text": self.text,
               "logprob": self.logprob, "p": self.logprob.exp(), "entropy": self.entropy,
               "phase": if self.phase == 1 { "reasoning" } else { "answer" },
               "alts": self.alts.iter().map(|a| json!({"id": if a.id == NO_ID { Value::Null } else { json!(a.id) },
                      "text": a.text, "logprob": a.logprob, "p": a.logprob.exp()})).collect::<Vec<_>>(),
               "routing": if self.routing.is_empty() { Value::Null } else { json!(self.routing.len()) }})
    }
}

/// Entropy in nats of the probabilities in `logprobs` (the model's top-k), as they are.
pub fn entropy(logprobs: impl Iterator<Item = f32>) -> f32 {
    let mut h = 0.0f32;
    let mut sum = 0.0f32;
    for lp in logprobs {
        let p = lp.exp();
        sum += p;
        if p > 0.0 {
            h -= p * lp;
        }
    }
    if sum <= 0.0 {
        0.0
    } else {
        h.max(0.0)
    }
}

pub fn header() -> Vec<u8> {
    let mut v = MAGIC.to_vec();
    v.extend_from_slice(&[VERSION, 0, 0, 0]);
    v
}

fn put_text(out: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    let n = b.len().min(u16::MAX as usize);
    out.extend_from_slice(&(n as u16).to_le_bytes());
    out.extend_from_slice(&b[..n]);
}

pub fn encode(t: &TokenRec, out: &mut Vec<u8>) {
    out.extend_from_slice(&t.id.to_le_bytes());
    out.extend_from_slice(&t.logprob.to_le_bytes());
    out.extend_from_slice(&t.entropy.to_le_bytes());
    out.push(t.phase);
    out.push(t.alts.len().min(255) as u8);
    put_text(out, &t.text);
    let r = &t.routing[..t.routing.len().min(u16::MAX as usize)];
    out.extend_from_slice(&(r.len() as u16).to_le_bytes());
    out.extend_from_slice(r);
    for a in t.alts.iter().take(255) {
        out.extend_from_slice(&a.id.to_le_bytes());
        out.extend_from_slice(&a.logprob.to_le_bytes());
        put_text(out, &a.text);
    }
}

struct Cur<'a>(&'a [u8]);

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn text(&mut self) -> Option<String> {
        let n = self.u16()? as usize;
        Some(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}

/// Decode a segment file; a torn last record is dropped.
pub fn decode(bytes: &[u8]) -> Vec<TokenRec> {
    if bytes.len() < 8 || &bytes[..4] != MAGIC || bytes[4] != VERSION {
        return vec![];
    }
    let mut c = Cur(&bytes[8..]);
    let mut out = vec![];
    loop {
        let one = (|| {
            let id = c.u32()?;
            let logprob = c.f32()?;
            let entropy = c.f32()?;
            let phase = c.u8()?;
            let n = c.u8()?;
            let text = c.text()?;
            let rl = c.u16()? as usize;
            let routing = c.take(rl)?.to_vec();
            let mut alts = Vec::with_capacity(n as usize);
            for _ in 0..n {
                alts.push(Alt {
                    id: c.u32()?,
                    logprob: c.f32()?,
                    text: c.text()?,
                });
            }
            Some(TokenRec {
                id,
                text,
                logprob,
                entropy,
                phase,
                alts,
                routing,
            })
        })();
        match one {
            Some(t) => out.push(t),
            None => break,
        }
    }
    out
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// What `list` filters on.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub model: Option<String>,
    pub status: Option<String>,
    pub kind: Option<String>,
    pub parent: Option<String>,
    /// Only traces of this job (a pipeline step) or task (a factory task).
    pub job: Option<String>,
    pub task: Option<String>,
    /// Text found in the prompt, answer or reasoning.
    pub q: Option<String>,
    pub since_ms: Option<i64>,
    pub until_ms: Option<i64>,
    pub limit: usize,
    pub offset: usize,
    pub oldest_first: bool,
}

impl Filter {
    pub fn from_json(a: &Value) -> Self {
        let s = |k: &str| {
            a.get(k)
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let n = |k: &str| a.get(k).and_then(Value::as_i64);
        Self {
            model: s("model"),
            status: s("status"),
            kind: s("kind"),
            parent: s("parent"),
            job: s("job"),
            task: s("task"),
            q: s("q"),
            since_ms: n("since_ms"),
            until_ms: n("until_ms"),
            limit: n("limit").unwrap_or(50).clamp(1, 500) as usize,
            offset: n("offset").unwrap_or(0).max(0) as usize,
            oldest_first: s("order").as_deref() == Some("asc"),
        }
    }

    fn clause(&self) -> (String, Vec<rusqlite::types::Value>) {
        use rusqlite::types::Value as V;
        let mut w = vec![];
        let mut p = vec![];
        let mut eq = |col: &str, v: &Option<String>| {
            if let Some(v) = v {
                w.push(format!("{col} = ?"));
                p.push(V::Text(v.clone()));
            }
        };
        eq("model", &self.model);
        eq("status", &self.status);
        eq("kind", &self.kind);
        eq("parent_id", &self.parent);
        eq("job_id", &self.job);
        eq("task_id", &self.task);
        if let Some(q) = &self.q {
            w.push("(prompt LIKE ? ESCAPE '\\' OR answer LIKE ? ESCAPE '\\' OR reasoning LIKE ? ESCAPE '\\')".into());
            let like = format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
            for _ in 0..3 {
                p.push(V::Text(like.clone()));
            }
        }
        if let Some(t) = self.since_ms {
            w.push("started_ms >= ?".into());
            p.push(V::Integer(t));
        }
        if let Some(t) = self.until_ms {
            w.push("started_ms <= ?".into());
            p.push(V::Integer(t));
        }
        let sql = if w.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", w.join(" AND "))
        };
        (sql, p)
    }
}

pub struct Store {
    conn: Mutex<Connection>,
    pub dir: PathBuf,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS traces (
  id TEXT PRIMARY KEY, kind TEXT NOT NULL, model TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'running',
  error TEXT, started_ms INTEGER NOT NULL, ended_ms INTEGER, parent_id TEXT, job_id TEXT, task_id TEXT,
  request TEXT, prompt TEXT, rendered_prompt TEXT, answer TEXT, reasoning TEXT,
  params TEXT, timings TEXT, resources TEXT, adapters TEXT, meta TEXT,
  token_count INTEGER NOT NULL DEFAULT 0, bytes INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS traces_started ON traces(started_ms);
CREATE INDEX IF NOT EXISTS traces_model ON traces(model);
CREATE INDEX IF NOT EXISTS traces_parent ON traces(parent_id);
CREATE TABLE IF NOT EXISTS stages (
  id INTEGER PRIMARY KEY AUTOINCREMENT, trace_id TEXT NOT NULL, name TEXT NOT NULL,
  started_ms INTEGER NOT NULL, ended_ms INTEGER, meta TEXT
);
CREATE INDEX IF NOT EXISTS stages_trace ON stages(trace_id);
CREATE TABLE IF NOT EXISTS events (
  id INTEGER PRIMARY KEY AUTOINCREMENT, trace_id TEXT NOT NULL, ts_ms INTEGER NOT NULL,
  kind TEXT NOT NULL, stage TEXT, data TEXT
);
CREATE INDEX IF NOT EXISTS events_trace ON events(trace_id);
CREATE TABLE IF NOT EXISTS specialist_calls (
  id INTEGER PRIMARY KEY AUTOINCREMENT, trace_id TEXT NOT NULL, ts_ms INTEGER NOT NULL, specialist TEXT NOT NULL,
  input TEXT, output TEXT, ms INTEGER, ok INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS calls_trace ON specialist_calls(trace_id);
";

/// The text columns `patch` may set; a JSON value is stored as its JSON text.
const PATCHABLE: &[&str] = &[
    "model",
    "request",
    "prompt",
    "rendered_prompt",
    "answer",
    "reasoning",
    "params",
    "timings",
    "resources",
    "adapters",
    "meta",
];

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

fn parse(s: Option<String>) -> Value {
    s.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)
}

type R<T> = Result<T, String>;

fn e(x: impl std::fmt::Display) -> String {
    x.to_string()
}

impl Store {
    pub fn open(dir: &Path) -> R<Self> {
        fs::create_dir_all(dir.join("segments")).map_err(e)?;
        let conn = Connection::open(dir.join("traces.db")).map_err(e)?;
        let _: String = conn
            .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
            .map_err(e)?;
        conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 5000;")
            .map_err(e)?;
        conn.execute_batch(SCHEMA).map_err(e)?;
        for col in ["job_id", "task_id"] {
            let _ = conn.execute(&format!("ALTER TABLE traces ADD COLUMN {col} TEXT"), []);
            // older stores
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS traces_job ON traces(job_id); CREATE INDEX IF NOT EXISTS traces_task ON traces(task_id);",
        )
        .map_err(e)?;
        // a trace still running when the hub stopped will not be finished by anyone
        conn.execute(
            "UPDATE traces SET status = 'interrupted', ended_ms = ?1 WHERE status = 'running'",
            [now_ms()],
        )
        .map_err(e)?;
        Ok(Self {
            conn: Mutex::new(conn),
            dir: dir.to_path_buf(),
        })
    }

    #[cfg(test)]
    pub fn exec(&self, sql: &str) {
        self.conn.lock().execute(sql, []).unwrap();
    }

    fn segment(&self, id: &str) -> PathBuf {
        // ids are ours (hex and a prefix), but never let one name a path
        let safe: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        self.dir.join("segments").join(format!("{safe}.tok"))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        &self,
        id: &str,
        kind: &str,
        model: &str,
        request: &Value,
        parent: Option<&str>,
        meta: &Value,
        scope: &super::Scope,
    ) -> R<i64> {
        let t = now_ms();
        self.conn
            .lock()
            .execute(
                "INSERT INTO traces (id, kind, model, started_ms, request, parent_id, meta, job_id, task_id) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                rusqlite::params![id, kind, model, t, request.to_string(), parent, meta.to_string(), scope.job, scope.task],
            )
            .map_err(e)?;
        Ok(t)
    }

    /// Set columns named in `patch` (see `PATCHABLE`); other keys are ignored.
    pub fn patch(&self, id: &str, patch: &Value) -> R<()> {
        let Some(o) = patch.as_object() else { return Ok(()) };
        let mut sets = vec![];
        let mut vals: Vec<rusqlite::types::Value> = vec![];
        for (k, v) in o {
            if PATCHABLE.contains(&k.as_str()) {
                sets.push(format!("{k} = ?"));
                vals.push(match text_of(v) {
                    Some(t) => rusqlite::types::Value::Text(t),
                    None => rusqlite::types::Value::Null,
                });
            }
        }
        if sets.is_empty() {
            return Ok(());
        }
        vals.push(rusqlite::types::Value::Text(id.to_string()));
        self.conn
            .lock()
            .execute(
                &format!("UPDATE traces SET {} WHERE id = ?", sets.join(", ")),
                params_from_iter(vals),
            )
            .map_err(e)?;
        Ok(())
    }

    pub fn finish(&self, id: &str, status: &str, error: Option<&str>) -> R<()> {
        let c = self.conn.lock();
        let seg = fs::metadata(self.segment(id)).map(|m| m.len()).unwrap_or(0);
        c.execute(
            "UPDATE traces SET status = ?2, error = ?3, ended_ms = ?4, bytes = ?5 + COALESCE(LENGTH(request),0)
             + COALESCE(LENGTH(prompt),0) + COALESCE(LENGTH(rendered_prompt),0) + COALESCE(LENGTH(answer),0)
             + COALESCE(LENGTH(reasoning),0) + COALESCE(LENGTH(meta),0) + COALESCE(LENGTH(resources),0)
             + COALESCE((SELECT SUM(COALESCE(LENGTH(data),0) + 64) FROM events WHERE trace_id = ?1),0)
             + COALESCE((SELECT SUM(COALESCE(LENGTH(input),0) + COALESCE(LENGTH(output),0) + 64) FROM specialist_calls WHERE trace_id = ?1),0)
             WHERE id = ?1",
            rusqlite::params![id, status, error, now_ms(), seg as i64],
        )
        .map_err(e)?;
        Ok(())
    }

    pub fn stage_start(&self, id: &str, name: &str, at_ms: i64) -> R<i64> {
        let c = self.conn.lock();
        c.execute(
            "INSERT INTO stages (trace_id, name, started_ms) VALUES (?1,?2,?3)",
            rusqlite::params![id, name, at_ms],
        )
        .map_err(e)?;
        Ok(c.last_insert_rowid())
    }

    pub fn stage_end(&self, stage: i64, at_ms: i64, meta: &Value) -> R<()> {
        self.conn
            .lock()
            .execute(
                "UPDATE stages SET ended_ms = ?2, meta = ?3 WHERE id = ?1",
                rusqlite::params![stage, at_ms, if meta.is_null() { None } else { Some(meta.to_string()) }],
            )
            .map_err(e)?;
        Ok(())
    }

    pub fn add_event(&self, id: &str, ts: i64, kind: &str, stage: Option<&str>, data: &Value) -> R<()> {
        self.conn
            .lock()
            .execute(
                "INSERT INTO events (trace_id, ts_ms, kind, stage, data) VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params![id, ts, kind, stage, data.to_string()],
            )
            .map_err(e)?;
        Ok(())
    }

    pub fn add_call(&self, id: &str, specialist: &str, input: &Value, output: &Value, ms: u64, ok: bool) -> R<()> {
        self.conn
            .lock()
            .execute(
                "INSERT INTO specialist_calls (trace_id, ts_ms, specialist, input, output, ms, ok) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                rusqlite::params![id, now_ms(), specialist, input.to_string(), output.to_string(), ms as i64, ok],
            )
            .map_err(e)?;
        Ok(())
    }

    /// Append tokens to the trace's segment file; returns the number of tokens now in the trace.
    pub fn append_tokens(&self, id: &str, toks: &[TokenRec]) -> R<i64> {
        let path = self.segment(id);
        let fresh = !path.exists();
        let mut buf = if fresh { header() } else { vec![] };
        for t in toks {
            encode(t, &mut buf);
        }
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(e)?;
        f.write_all(&buf).map_err(e)?;
        let c = self.conn.lock();
        c.execute(
            "UPDATE traces SET token_count = token_count + ?2 WHERE id = ?1",
            rusqlite::params![id, toks.len() as i64],
        )
        .map_err(e)?;
        c.query_row("SELECT token_count FROM traces WHERE id = ?1", [id], |r| r.get(0))
            .map_err(e)
    }

    pub fn tokens(&self, id: &str) -> Vec<TokenRec> {
        fs::read(self.segment(id)).map(|b| decode(&b)).unwrap_or_default()
    }

    pub fn exists(&self, id: &str) -> bool {
        self.conn
            .lock()
            .query_row("SELECT 1 FROM traces WHERE id = ?1", [id], |_| Ok(()))
            .optional()
            .ok()
            .flatten()
            .is_some()
    }

    fn row_summary(r: &rusqlite::Row) -> rusqlite::Result<Value> {
        let started: i64 = r.get("started_ms")?;
        let ended: Option<i64> = r.get("ended_ms")?;
        let tokens: i64 = r.get("token_count")?;
        let timings = parse(r.get("timings")?);
        let adapters = parse(r.get("adapters")?);
        let prompt: Option<String> = r.get("prompt")?;
        let answer: Option<String> = r.get("answer")?;
        let cut = |s: Option<String>| s.map(|s| s.chars().take(200).collect::<String>());
        Ok(json!({
            "id": r.get::<_, String>("id")?, "kind": r.get::<_, String>("kind")?, "model": r.get::<_, String>("model")?,
            "status": r.get::<_, String>("status")?, "error": r.get::<_, Option<String>>("error")?,
            "started_ms": started, "ended_ms": ended, "duration_ms": ended.map(|e| e - started),
            "parent_id": r.get::<_, Option<String>>("parent_id")?,
            "job_id": r.get::<_, Option<String>>("job_id")?,
            "task_id": r.get::<_, Option<String>>("task_id")?,
            "token_count": tokens, "bytes": r.get::<_, i64>("bytes")?,
            "tokens_per_s": timings.get("predicted_per_second").cloned().unwrap_or(Value::Null),
            "adapters": adapters.as_array().map(|a| a.len()).unwrap_or(0),
            "prompt_preview": cut(prompt), "answer_preview": cut(answer),
        }))
    }

    pub fn list(&self, f: &Filter) -> R<(i64, Vec<Value>)> {
        let (w, p) = f.clause();
        let c = self.conn.lock();
        let total: i64 = c
            .query_row(
                &format!("SELECT COUNT(*) FROM traces{w}"),
                params_from_iter(p.clone()),
                |r| r.get(0),
            )
            .map_err(e)?;
        let order = if f.oldest_first { "ASC" } else { "DESC" };
        let mut st = c
            .prepare(&format!(
                "SELECT * FROM traces{w} ORDER BY started_ms {order}, id {order} LIMIT {} OFFSET {}",
                f.limit, f.offset
            ))
            .map_err(e)?;
        let rows = st
            .query_map(params_from_iter(p), Self::row_summary)
            .map_err(e)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(e)?;
        Ok((total, rows))
    }

    /// The whole trace except its tokens (those are in `tokens`).
    pub fn get(&self, id: &str, max_events: usize) -> R<Option<Value>> {
        let c = self.conn.lock();
        let base = c
            .query_row("SELECT * FROM traces WHERE id = ?1", [id], |r| {
                let mut v = Self::row_summary(r)?;
                let o = v.as_object_mut().unwrap();
                for k in ["request", "params", "timings", "resources", "adapters", "meta"] {
                    o.insert(k.into(), parse(r.get(k)?));
                }
                for k in ["prompt", "rendered_prompt", "answer", "reasoning"] {
                    o.insert(
                        k.into(),
                        r.get::<_, Option<String>>(k)?.map(Value::from).unwrap_or(Value::Null),
                    );
                }
                Ok(v)
            })
            .optional()
            .map_err(e)?;
        let Some(mut v) = base else { return Ok(None) };
        let stages = c
            .prepare("SELECT id, name, started_ms, ended_ms, meta FROM stages WHERE trace_id = ?1 ORDER BY id")
            .map_err(e)?
            .query_map([id], |r| {
                Ok(
                    json!({"id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "started_ms": r.get::<_, i64>(2)?,
                          "ended_ms": r.get::<_, Option<i64>>(3)?, "meta": parse(r.get(4)?)}),
                )
            })
            .map_err(e)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(e)?;
        let events = c
            .prepare("SELECT id, ts_ms, kind, stage, data FROM events WHERE trace_id = ?1 ORDER BY id LIMIT ?2")
            .map_err(e)?
            .query_map(rusqlite::params![id, max_events as i64], |r| {
                Ok(
                    json!({"id": r.get::<_, i64>(0)?, "ts_ms": r.get::<_, i64>(1)?, "kind": r.get::<_, String>(2)?,
                          "stage": r.get::<_, Option<String>>(3)?, "data": parse(r.get(4)?)}),
                )
            })
            .map_err(e)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(e)?;
        let calls = c
            .prepare("SELECT id, ts_ms, specialist, input, output, ms, ok FROM specialist_calls WHERE trace_id = ?1 ORDER BY id")
            .map_err(e)?
            .query_map([id], |r| {
                Ok(json!({"id": r.get::<_, i64>(0)?, "ts_ms": r.get::<_, i64>(1)?, "specialist": r.get::<_, String>(2)?,
                          "input": parse(r.get(3)?), "output": parse(r.get(4)?), "ms": r.get::<_, Option<i64>>(5)?,
                          "ok": r.get::<_, bool>(6)?}))
            })
            .map_err(e)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(e)?;
        let o = v.as_object_mut().unwrap();
        o.insert("stages".into(), json!(stages));
        o.insert("events".into(), json!(events));
        o.insert("specialist_calls".into(), json!(calls));
        Ok(Some(v))
    }

    pub fn delete(&self, id: &str) -> R<bool> {
        let c = self.conn.lock();
        for t in ["stages", "events", "specialist_calls"] {
            c.execute(&format!("DELETE FROM {t} WHERE trace_id = ?1"), [id])
                .map_err(e)?;
        }
        let n = c.execute("DELETE FROM traces WHERE id = ?1", [id]).map_err(e)?;
        let _ = fs::remove_file(self.segment(id));
        Ok(n > 0)
    }

    /// Delete traces older than the age limit, then the oldest until the total size is under the size limit.
    /// A trace still running is never deleted. Returns how many went.
    pub fn retain(&self, st: &TraceSettings) -> R<usize> {
        let mut gone: Vec<String> = vec![];
        {
            let c = self.conn.lock();
            if st.max_age_days > 0 {
                let cutoff = now_ms() - st.max_age_days as i64 * 86_400_000;
                let mut q = c
                    .prepare("SELECT id FROM traces WHERE started_ms < ?1 AND status != 'running'")
                    .map_err(e)?;
                gone.extend(
                    q.query_map([cutoff], |r| r.get::<_, String>(0))
                        .map_err(e)?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(e)?,
                );
            }
            if st.max_total_mb > 0 {
                let limit = st.max_total_mb as i64 * 1_048_576;
                let mut total: i64 = c
                    .query_row("SELECT COALESCE(SUM(bytes),0) FROM traces", [], |r| r.get(0))
                    .map_err(e)?;
                let mut q = c
                    .prepare("SELECT id, bytes FROM traces WHERE status != 'running' ORDER BY started_ms ASC")
                    .map_err(e)?;
                let rows = q
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                    .map_err(e)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(e)?;
                // what the age rule already removes does not count
                for (id, b) in &rows {
                    if gone.contains(id) {
                        total -= b;
                    }
                }
                for (id, b) in rows {
                    if total <= limit {
                        break;
                    }
                    if !gone.contains(&id) {
                        total -= b;
                        gone.push(id);
                    }
                }
            }
        }
        for id in &gone {
            self.delete(id)?;
        }
        Ok(gone.len())
    }

    /// Remove segment files that no trace owns (a delete that was cut short).
    pub fn sweep_orphans(&self) {
        if let Ok(rd) = fs::read_dir(self.dir.join("segments")) {
            for ent in rd.flatten() {
                let name = ent.file_name().to_string_lossy().to_string();
                if let Some(id) = name.strip_suffix(".tok") {
                    if !self.exists(id) {
                        let _ = fs::remove_file(ent.path());
                    }
                }
            }
        }
    }

    pub fn total_bytes(&self) -> i64 {
        self.conn
            .lock()
            .query_row("SELECT COALESCE(SUM(bytes),0) FROM traces", [], |r| r.get(0))
            .unwrap_or(0)
    }

    /// One JSON line per matching trace: the whole trace, with its tokens.
    pub fn export_jsonl(&self, f: &Filter, out: &mut dyn Write) -> R<usize> {
        let mut f = f.clone();
        f.limit = 100;
        f.offset = 0;
        let mut n = 0;
        loop {
            let (_, page) = self.list(&f)?;
            if page.is_empty() {
                break;
            }
            for s in &page {
                let id = s["id"].as_str().unwrap_or("");
                if let Some(mut t) = self.get(id, usize::MAX >> 1)? {
                    let toks: Vec<Value> = self.tokens(id).iter().enumerate().map(|(i, t)| t.to_json(i)).collect();
                    t["tokens"] = json!(toks);
                    writeln!(out, "{t}").map_err(e)?;
                    n += 1;
                }
            }
            f.offset += page.len();
        }
        Ok(n)
    }
}

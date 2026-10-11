//! The factory's state: `<home>/factory/factory.db` (SQLite, WAL). Every step of every task keeps what it needs here,
//! so `factory.task.resume` can continue after a crash or a restart.

use std::path::Path;

use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::engine::Fail;

pub type Res<T> = Result<T, Fail>;

fn db<E: std::fmt::Display>(e: E) -> Fail {
    Fail::new(500, "factory_db", format!("factory: {e}"))
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tasks (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, spec TEXT NOT NULL, state TEXT NOT NULL, step TEXT, error TEXT,
  created INTEGER NOT NULL, updated INTEGER NOT NULL, job TEXT, usage TEXT NOT NULL DEFAULT '{}',
  summary TEXT NOT NULL DEFAULT '{}', cancel INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS steps (
  task TEXT NOT NULL, name TEXT NOT NULL, status TEXT NOT NULL, started INTEGER, finished INTEGER,
  result TEXT, error TEXT, PRIMARY KEY (task, name)
);
CREATE TABLE IF NOT EXISTS plans (
  task TEXT PRIMARY KEY, rev INTEGER NOT NULL, plan TEXT NOT NULL, approved INTEGER NOT NULL DEFAULT 0,
  edited_by TEXT, updated INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS logs (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, task TEXT NOT NULL, at INTEGER NOT NULL, step TEXT, level TEXT, msg TEXT
);
CREATE INDEX IF NOT EXISTS logs_task ON logs(task, seq);
CREATE TABLE IF NOT EXISTS chunks (
  task TEXT NOT NULL, id TEXT NOT NULL, ord INTEGER NOT NULL, text TEXT NOT NULL, hash TEXT NOT NULL,
  origin TEXT NOT NULL, role TEXT NOT NULL DEFAULT 'doc', emb BLOB, PRIMARY KEY (task, id)
);
CREATE TABLE IF NOT EXISTS work (
  task TEXT NOT NULL, step TEXT NOT NULL, idx INTEGER NOT NULL, status TEXT NOT NULL, spec TEXT NOT NULL,
  result TEXT, PRIMARY KEY (task, step, idx)
);
CREATE TABLE IF NOT EXISTS records (
  task TEXT NOT NULL, id TEXT NOT NULL, ord INTEGER NOT NULL, json TEXT NOT NULL, status TEXT NOT NULL,
  reason TEXT, verified INTEGER NOT NULL DEFAULT 0, tagged INTEGER NOT NULL DEFAULT 0, hash TEXT NOT NULL DEFAULT '',
  score REAL NOT NULL DEFAULT 0, split TEXT NOT NULL DEFAULT 'train', emb BLOB, PRIMARY KEY (task, id)
);
CREATE TABLE IF NOT EXISTS review (
  id INTEGER PRIMARY KEY AUTOINCREMENT, task TEXT NOT NULL, kind TEXT NOT NULL, ref TEXT NOT NULL, reason TEXT,
  data TEXT NOT NULL DEFAULT '{}', state TEXT NOT NULL DEFAULT 'open', decision TEXT, decided_by TEXT, note TEXT,
  created INTEGER NOT NULL, decided INTEGER
);
CREATE INDEX IF NOT EXISTS review_task ON review(task, state);
CREATE TABLE IF NOT EXISTS feedback (tag TEXT NOT NULL, word TEXT NOT NULL, n REAL NOT NULL, PRIMARY KEY (tag, word));
CREATE TABLE IF NOT EXISTS artifacts (
  task TEXT NOT NULL, id TEXT NOT NULL, version TEXT NOT NULL, kind TEXT NOT NULL, stage TEXT NOT NULL,
  draft TEXT NOT NULL, key TEXT, content_id TEXT, imported INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (task, id, version)
);
CREATE TABLE IF NOT EXISTS evals (task TEXT NOT NULL, record TEXT NOT NULL, json TEXT NOT NULL, PRIMARY KEY (task, record));
";

pub fn now_ms() -> i64 {
    crate::km::now_ms()
}

fn js(s: Option<String>) -> Value {
    s.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null)
}

fn jstr(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

pub fn f32_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

#[allow(clippy::chunks_exact_to_as_chunks)]
pub fn bytes_f32(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[derive(Debug, Clone)]
pub struct TaskRow {
    pub id: String,
    pub name: String,
    pub spec: Value,
    pub state: String,
    pub step: Option<String>,
    pub error: Option<String>,
    pub created: i64,
    pub updated: i64,
    pub job: Option<String>,
    pub usage: Value,
    pub summary: Value,
    pub cancel: bool,
}

impl TaskRow {
    pub fn to_json(&self) -> Value {
        json!({"id": self.id, "name": self.name, "goal": self.spec["goal"], "state": self.state, "status": self.state,
               "step": self.step, "error": self.error, "created": self.created, "updated": self.updated,
               "created_ms": self.created, "updated_ms": self.updated, "job": self.job, "usage": self.usage,
               "summary": self.summary, "spec": self.spec})
    }
}

#[derive(Debug, Clone)]
pub struct StepRow {
    pub name: String,
    pub status: String,
    pub started: Option<i64>,
    pub finished: Option<i64>,
    pub result: Value,
    pub error: Option<String>,
}

impl StepRow {
    pub fn to_json(&self) -> Value {
        json!({"name": self.name, "status": self.status, "state": self.status, "started_ms": self.started, "started": self.started, "finished": self.finished,
               "result": self.result, "error": self.error})
    }
}

#[derive(Debug, Clone)]
pub struct ChunkRow {
    pub id: String,
    pub ord: i64,
    pub text: String,
    pub origin: Value,
    pub role: String,
    pub emb: Option<Vec<f32>>,
}

#[derive(Debug, Clone)]
pub struct RecordRow {
    pub id: String,
    pub ord: i64,
    pub json: Value,
    pub status: String,
    pub reason: Option<String>,
    pub verified: bool,
    pub tagged: bool,
    pub hash: String,
    pub score: f64,
    pub split: String,
    pub emb: Option<Vec<f32>>,
}

#[derive(Debug, Clone)]
pub struct ReviewRow {
    pub id: i64,
    pub task: String,
    pub kind: String,
    pub reference: String,
    pub reason: String,
    pub data: Value,
    pub state: String,
    pub decision: Option<String>,
    pub decided_by: Option<String>,
    pub note: Option<String>,
    pub created: i64,
    pub decided: Option<i64>,
}

impl ReviewRow {
    pub fn to_json(&self) -> Value {
        let (record_id, tag) = match self.kind.as_str() {
            "tag" => {
                let (r, t) = self.reference.split_once('|').unwrap_or((&self.reference, ""));
                (json!(r), json!(t))
            }
            "record" => (json!(self.reference), Value::Null),
            "vocab" => (self.data["example_record"].clone(), json!(self.reference)),
            _ => (Value::Null, Value::Null),
        };
        json!({"id": self.id, "type": self.kind, "kind": self.kind, "task": self.task, "ref": self.reference,
               "record_id": record_id, "tag": tag, "confidence": self.data["confidence"], "origin": self.data["origin"],
               "record": if self.kind == "record" { json!({"input": self.data["input"], "output": self.data["output"],
                          "verdict": self.data["verdict"], "score": self.data["score"]}) } else { Value::Null },
               "reason": self.reason, "data": self.data, "state": self.state, "decision": self.decision,
               "decided_by": self.decided_by, "note": self.note, "created": self.created, "decided": self.decided})
    }
}

#[derive(Debug, Clone)]
pub struct ArtifactRow {
    pub id: String,
    pub version: String,
    pub kind: String,
    pub stage: String,
    pub draft: Value,
    pub key: Option<String>,
    pub content_id: Option<String>,
    pub imported: bool,
}

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(dir: &Path) -> Res<Db> {
        std::fs::create_dir_all(dir).map_err(|e| Fail::new(500, "io", format!("{}: {e}", dir.display())))?;
        let conn = Connection::open(dir.join("factory.db")).map_err(db)?;
        conn.busy_timeout(std::time::Duration::from_secs(10)).map_err(db)?;
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        conn.execute_batch(SCHEMA).map_err(db)?;
        Ok(Db { conn: Mutex::new(conn) })
    }

    // ---- tasks ---------------------------------------------------------------------------------------------

    pub fn insert_task(&self, id: &str, name: &str, spec: &Value) -> Res<()> {
        let now = now_ms();
        self.conn
            .lock()
            .execute(
                "INSERT INTO tasks (id, name, spec, state, created, updated) VALUES (?1, ?2, ?3, 'created', ?4, ?4)",
                params![id, name, jstr(spec), now],
            )
            .map_err(db)?;
        Ok(())
    }

    fn task_of(r: &rusqlite::Row) -> rusqlite::Result<TaskRow> {
        Ok(TaskRow {
            id: r.get("id")?,
            name: r.get("name")?,
            spec: js(r.get("spec")?),
            state: r.get("state")?,
            step: r.get("step")?,
            error: r.get("error")?,
            created: r.get("created")?,
            updated: r.get("updated")?,
            job: r.get("job")?,
            usage: js(r.get("usage")?),
            summary: js(r.get("summary")?),
            cancel: r.get::<_, i64>("cancel")? != 0,
        })
    }

    pub fn task(&self, id: &str) -> Res<TaskRow> {
        self.conn
            .lock()
            .query_row("SELECT * FROM tasks WHERE id = ?1", [id], Self::task_of)
            .optional()
            .map_err(db)?
            .ok_or_else(|| Fail::not_found(format!("no factory task {id}")))
    }

    pub fn tasks(&self, state: Option<&str>, limit: usize) -> Res<Vec<TaskRow>> {
        let c = self.conn.lock();
        let mut st = c
            .prepare("SELECT * FROM tasks WHERE (?1 IS NULL OR state = ?1) ORDER BY created DESC, id LIMIT ?2")
            .map_err(db)?;
        let rows = st
            .query_map(params![state, limit as i64], Self::task_of)
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        Ok(rows)
    }

    pub fn set_state(&self, id: &str, state: &str, step: Option<&str>, error: Option<&str>) -> Res<()> {
        self.conn
            .lock()
            .execute(
                "UPDATE tasks SET state = ?2, step = COALESCE(?3, step), error = ?4, updated = ?5 WHERE id = ?1",
                params![id, state, step, error, now_ms()],
            )
            .map_err(db)?;
        Ok(())
    }

    pub fn set_job(&self, id: &str, job: Option<&str>) -> Res<()> {
        self.conn
            .lock()
            .execute(
                "UPDATE tasks SET job = ?2, updated = ?3 WHERE id = ?1",
                params![id, job, now_ms()],
            )
            .map_err(db)?;
        Ok(())
    }

    pub fn set_usage(&self, id: &str, usage: &Value) -> Res<()> {
        self.conn
            .lock()
            .execute("UPDATE tasks SET usage = ?2 WHERE id = ?1", params![id, jstr(usage)])
            .map_err(db)?;
        Ok(())
    }

    pub fn set_summary(&self, id: &str, summary: &Value) -> Res<()> {
        self.conn
            .lock()
            .execute("UPDATE tasks SET summary = ?2 WHERE id = ?1", params![id, jstr(summary)])
            .map_err(db)?;
        Ok(())
    }

    pub fn set_spec(&self, id: &str, spec: &Value) -> Res<()> {
        self.conn
            .lock()
            .execute("UPDATE tasks SET spec = ?2 WHERE id = ?1", params![id, jstr(spec)])
            .map_err(db)?;
        Ok(())
    }

    pub fn set_cancel(&self, id: &str, on: bool) -> Res<()> {
        self.conn
            .lock()
            .execute("UPDATE tasks SET cancel = ?2 WHERE id = ?1", params![id, on as i64])
            .map_err(db)?;
        Ok(())
    }

    pub fn cancel_requested(&self, id: &str) -> bool {
        self.conn
            .lock()
            .query_row("SELECT cancel FROM tasks WHERE id = ?1", [id], |r| r.get::<_, i64>(0))
            .map(|v| v != 0)
            .unwrap_or(false)
    }

    /// A task left `running` by a hub that is gone is `interrupted`: nothing is running it any more.
    pub fn interrupt_orphans(&self) -> Res<usize> {
        let c = self.conn.lock();
        let n = c
            .execute(
                "UPDATE tasks SET state = 'interrupted', updated = ?1 WHERE state = 'running'",
                [now_ms()],
            )
            .map_err(db)?;
        c.execute("UPDATE steps SET status = 'interrupted' WHERE status = 'running'", [])
            .map_err(db)?;
        Ok(n)
    }

    // ---- steps ---------------------------------------------------------------------------------------------

    fn step_of(r: &rusqlite::Row) -> rusqlite::Result<StepRow> {
        Ok(StepRow {
            name: r.get("name")?,
            status: r.get("status")?,
            started: r.get("started")?,
            finished: r.get("finished")?,
            result: js(r.get("result")?),
            error: r.get("error")?,
        })
    }

    pub fn step(&self, task: &str, name: &str) -> Option<StepRow> {
        self.conn
            .lock()
            .query_row(
                "SELECT * FROM steps WHERE task = ?1 AND name = ?2",
                params![task, name],
                Self::step_of,
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn steps(&self, task: &str) -> Vec<StepRow> {
        let c = self.conn.lock();
        let Ok(mut st) = c.prepare("SELECT * FROM steps WHERE task = ?1 ORDER BY rowid") else {
            return vec![];
        };
        st.query_map([task], Self::step_of)
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    pub fn set_step(
        &self,
        task: &str,
        name: &str,
        status: &str,
        result: Option<&Value>,
        error: Option<&str>,
    ) -> Res<()> {
        let now = now_ms();
        let finished = matches!(status, "done" | "failed" | "skipped" | "waiting").then_some(now);
        self.conn
            .lock()
            .execute(
                "INSERT INTO steps (task, name, status, started, finished, result, error) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(task, name) DO UPDATE SET status = ?3, finished = ?5,
                   started = COALESCE(steps.started, ?4), result = COALESCE(?6, steps.result), error = ?7",
                params![task, name, status, now, finished, result.map(jstr), error],
            )
            .map_err(db)?;
        Ok(())
    }

    /// Forget the state of the named steps (so they run again).
    pub fn reset_steps(&self, task: &str, names: &[&str]) -> Res<()> {
        let c = self.conn.lock();
        for n in names {
            c.execute("DELETE FROM steps WHERE task = ?1 AND name = ?2", params![task, n])
                .map_err(db)?;
        }
        Ok(())
    }

    // ---- plan ----------------------------------------------------------------------------------------------

    pub fn plan(&self, task: &str) -> Option<(i64, Value, bool)> {
        self.conn
            .lock()
            .query_row("SELECT rev, plan, approved FROM plans WHERE task = ?1", [task], |r| {
                Ok((r.get::<_, i64>(0)?, js(r.get(1)?), r.get::<_, i64>(2)? != 0))
            })
            .optional()
            .ok()
            .flatten()
    }

    pub fn put_plan(&self, task: &str, plan: &Value, approved: bool, by: &str) -> Res<i64> {
        let rev = self.plan(task).map(|p| p.0 + 1).unwrap_or(1);
        self.conn
            .lock()
            .execute(
                "INSERT INTO plans (task, rev, plan, approved, edited_by, updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(task) DO UPDATE SET rev = ?2, plan = ?3, approved = ?4, edited_by = ?5, updated = ?6",
                params![task, rev, jstr(plan), approved as i64, by, now_ms()],
            )
            .map_err(db)?;
        Ok(rev)
    }

    // ---- logs ----------------------------------------------------------------------------------------------

    pub fn log(&self, task: &str, step: &str, level: &str, msg: &str) {
        let _ = self.conn.lock().execute(
            "INSERT INTO logs (task, at, step, level, msg) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![task, now_ms(), step, level, msg],
        );
    }

    pub fn logs(&self, task: &str, limit: usize, since: i64) -> Vec<Value> {
        let c = self.conn.lock();
        let Ok(mut st) = c.prepare(
            "SELECT seq, at, step, level, msg FROM (SELECT * FROM logs WHERE task = ?1 AND seq > ?2 ORDER BY seq DESC LIMIT ?3) ORDER BY seq",
        ) else {
            return vec![];
        };
        st.query_map(params![task, since, limit as i64], |r| {
            Ok(json!({"seq": r.get::<_, i64>(0)?, "at": r.get::<_, i64>(1)?, "ts": r.get::<_, i64>(1)?, "step": r.get::<_, Option<String>>(2)?,
                      "level": r.get::<_, Option<String>>(3)?, "msg": r.get::<_, Option<String>>(4)?}))
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    // ---- chunks --------------------------------------------------------------------------------------------

    pub fn replace_chunks(&self, task: &str, chunks: &[ChunkRow]) -> Res<()> {
        let mut c = self.conn.lock();
        let tx = c.transaction().map_err(db)?;
        tx.execute("DELETE FROM chunks WHERE task = ?1", [task]).map_err(db)?;
        for ch in chunks {
            tx.execute(
                "INSERT INTO chunks (task, id, ord, text, hash, origin, role) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    task,
                    ch.id,
                    ch.ord,
                    ch.text,
                    crate::km::sha256_hex(ch.text.as_bytes()),
                    jstr(&ch.origin),
                    ch.role
                ],
            )
            .map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(())
    }

    pub fn chunks(&self, task: &str) -> Vec<ChunkRow> {
        let c = self.conn.lock();
        let Ok(mut st) = c.prepare("SELECT id, ord, text, origin, role, emb FROM chunks WHERE task = ?1 ORDER BY ord")
        else {
            return vec![];
        };
        st.query_map([task], |r| {
            Ok(ChunkRow {
                id: r.get(0)?,
                ord: r.get(1)?,
                text: r.get(2)?,
                origin: js(r.get(3)?),
                role: r.get(4)?,
                emb: r.get::<_, Option<Vec<u8>>>(5)?.map(|b| bytes_f32(&b)),
            })
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    pub fn set_chunk_embs(&self, task: &str, embs: &[(String, Vec<f32>)]) -> Res<()> {
        let mut c = self.conn.lock();
        let tx = c.transaction().map_err(db)?;
        for (id, e) in embs {
            tx.execute(
                "UPDATE chunks SET emb = ?3 WHERE task = ?1 AND id = ?2",
                params![task, id, f32_bytes(e)],
            )
            .map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(())
    }

    // ---- work items ----------------------------------------------------------------------------------------

    pub fn has_work(&self, task: &str, step: &str) -> bool {
        self.conn
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM work WHERE task = ?1 AND step = ?2",
                params![task, step],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false)
    }

    pub fn put_work(&self, task: &str, step: &str, items: &[Value]) -> Res<()> {
        let mut c = self.conn.lock();
        let tx = c.transaction().map_err(db)?;
        for (i, it) in items.iter().enumerate() {
            tx.execute(
                "INSERT OR IGNORE INTO work (task, step, idx, status, spec) VALUES (?1, ?2, ?3, 'pending', ?4)",
                params![task, step, i as i64, jstr(it)],
            )
            .map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(())
    }

    pub fn add_work(&self, task: &str, step: &str, idx: i64, item: &Value) -> Res<()> {
        self.conn
            .lock()
            .execute(
                "INSERT OR IGNORE INTO work (task, step, idx, status, spec) VALUES (?1, ?2, ?3, 'pending', ?4)",
                params![task, step, idx, jstr(item)],
            )
            .map_err(db)?;
        Ok(())
    }

    pub fn next_work(&self, task: &str, step: &str) -> Option<(i64, Value)> {
        self.conn
            .lock()
            .query_row(
                "SELECT idx, spec FROM work WHERE task = ?1 AND step = ?2 AND status = 'pending' ORDER BY idx LIMIT 1",
                params![task, step],
                |r| Ok((r.get::<_, i64>(0)?, js(r.get(1)?))),
            )
            .optional()
            .ok()
            .flatten()
    }

    /// (pending, done, failed)
    pub fn work_counts(&self, task: &str, step: &str) -> (i64, i64, i64) {
        let n = |status: &str| -> i64 {
            self.conn
                .lock()
                .query_row(
                    "SELECT COUNT(*) FROM work WHERE task = ?1 AND step = ?2 AND status = ?3",
                    params![task, step, status],
                    |r| r.get(0),
                )
                .unwrap_or(0)
        };
        (n("pending"), n("done"), n("failed"))
    }

    pub fn finish_work(&self, task: &str, step: &str, idx: i64, status: &str, result: &Value) -> Res<()> {
        self.conn
            .lock()
            .execute(
                "UPDATE work SET status = ?4, result = ?5 WHERE task = ?1 AND step = ?2 AND idx = ?3",
                params![task, step, idx, status, jstr(result)],
            )
            .map_err(db)?;
        Ok(())
    }

    pub fn clear_work(&self, task: &str, step: &str) -> Res<()> {
        self.conn
            .lock()
            .execute("DELETE FROM work WHERE task = ?1 AND step = ?2", params![task, step])
            .map_err(db)?;
        Ok(())
    }

    // ---- records -------------------------------------------------------------------------------------------

    fn record_of(r: &rusqlite::Row) -> rusqlite::Result<RecordRow> {
        Ok(RecordRow {
            id: r.get("id")?,
            ord: r.get("ord")?,
            json: js(r.get("json")?),
            status: r.get("status")?,
            reason: r.get("reason")?,
            verified: r.get::<_, i64>("verified")? != 0,
            tagged: r.get::<_, i64>("tagged")? != 0,
            hash: r.get("hash")?,
            score: r.get("score")?,
            split: r.get("split")?,
            emb: r.get::<_, Option<Vec<u8>>>("emb")?.map(|b| bytes_f32(&b)),
        })
    }

    fn put_record_tx(tx: &rusqlite::Transaction, task: &str, r: &RecordRow) -> Res<()> {
        tx.execute(
            "INSERT INTO records (task, id, ord, json, status, reason, verified, tagged, hash, score, split, emb)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(task, id) DO UPDATE SET json = ?4, status = ?5, reason = ?6, verified = ?7, tagged = ?8,
               hash = ?9, score = ?10, split = ?11, emb = ?12",
            params![
                task,
                r.id,
                r.ord,
                jstr(&r.json),
                r.status,
                r.reason,
                r.verified as i64,
                r.tagged as i64,
                r.hash,
                r.score,
                r.split,
                r.emb.as_ref().map(|e| f32_bytes(e))
            ],
        )
        .map_err(db)?;
        Ok(())
    }

    pub fn put_record(&self, task: &str, r: &RecordRow) -> Res<()> {
        self.put_records(task, std::slice::from_ref(r))
    }

    pub fn put_records(&self, task: &str, rows: &[RecordRow]) -> Res<()> {
        let mut c = self.conn.lock();
        let tx = c.transaction().map_err(db)?;
        for r in rows {
            Self::put_record_tx(&tx, task, r)?;
        }
        tx.commit().map_err(db)?;
        Ok(())
    }

    /// New records from one work item, and the item marked done, in one transaction.
    pub fn add_generated(&self, task: &str, step: &str, idx: i64, rows: &[RecordRow], result: &Value) -> Res<()> {
        let mut c = self.conn.lock();
        let tx = c.transaction().map_err(db)?;
        for r in rows {
            Self::put_record_tx(&tx, task, r)?;
        }
        tx.execute(
            "UPDATE work SET status = 'done', result = ?4 WHERE task = ?1 AND step = ?2 AND idx = ?3",
            params![task, step, idx, jstr(result)],
        )
        .map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(())
    }

    pub fn record(&self, task: &str, id: &str) -> Option<RecordRow> {
        self.conn
            .lock()
            .query_row(
                "SELECT * FROM records WHERE task = ?1 AND id = ?2",
                params![task, id],
                Self::record_of,
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn records(&self, task: &str, status: Option<&str>) -> Vec<RecordRow> {
        let c = self.conn.lock();
        let Ok(mut st) =
            c.prepare("SELECT * FROM records WHERE task = ?1 AND (?2 IS NULL OR status = ?2) ORDER BY ord")
        else {
            return vec![];
        };
        st.query_map(params![task, status], Self::record_of)
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    /// Records that are not rejected.
    pub fn usable_count(&self, task: &str) -> i64 {
        self.conn
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM records WHERE task = ?1 AND status != 'rejected'",
                [task],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }

    pub fn record_count(&self, task: &str) -> i64 {
        self.conn
            .lock()
            .query_row("SELECT COUNT(*) FROM records WHERE task = ?1", [task], |r| r.get(0))
            .unwrap_or(0)
    }

    // ---- review queue --------------------------------------------------------------------------------------

    pub fn add_review(&self, task: &str, kind: &str, reference: &str, reason: &str, data: &Value) -> Res<i64> {
        let c = self.conn.lock();
        // the same open item is not queued twice (a step run again)
        if let Ok(id) = c.query_row(
            "SELECT id FROM review WHERE task = ?1 AND kind = ?2 AND ref = ?3 AND state = 'open'",
            params![task, kind, reference],
            |r| r.get::<_, i64>(0),
        ) {
            return Ok(id);
        }
        c.execute(
            "INSERT INTO review (task, kind, ref, reason, data, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![task, kind, reference, reason, jstr(data), now_ms()],
        )
        .map_err(db)?;
        Ok(c.last_insert_rowid())
    }

    fn review_of(r: &rusqlite::Row) -> rusqlite::Result<ReviewRow> {
        Ok(ReviewRow {
            id: r.get("id")?,
            task: r.get("task")?,
            kind: r.get("kind")?,
            reference: r.get("ref")?,
            reason: r.get::<_, Option<String>>("reason")?.unwrap_or_default(),
            data: js(r.get("data")?),
            state: r.get("state")?,
            decision: r.get("decision")?,
            decided_by: r.get("decided_by")?,
            note: r.get("note")?,
            created: r.get("created")?,
            decided: r.get("decided")?,
        })
    }

    pub fn review_item(&self, id: i64) -> Res<ReviewRow> {
        self.conn
            .lock()
            .query_row("SELECT * FROM review WHERE id = ?1", [id], Self::review_of)
            .optional()
            .map_err(db)?
            .ok_or_else(|| Fail::not_found(format!("no review item {id}")))
    }

    pub fn review_list(
        &self,
        task: Option<&str>,
        kind: Option<&str>,
        state: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> (i64, Vec<ReviewRow>) {
        let c = self.conn.lock();
        let w = "WHERE (?1 IS NULL OR task = ?1) AND (?2 IS NULL OR kind = ?2) AND (?3 IS NULL OR state = ?3)";
        let total = c
            .query_row(
                &format!("SELECT COUNT(*) FROM review {w}"),
                params![task, kind, state],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0);
        let Ok(mut st) = c.prepare(&format!("SELECT * FROM review {w} ORDER BY id LIMIT ?4 OFFSET ?5")) else {
            return (total, vec![]);
        };
        let rows = st
            .query_map(params![task, kind, state, limit as i64, offset as i64], Self::review_of)
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default();
        (total, rows)
    }

    pub fn review_open_count(&self, task: &str) -> i64 {
        self.conn
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM review WHERE task = ?1 AND state = 'open'",
                [task],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }

    pub fn review_decide(&self, id: i64, state: &str, decision: &str, by: &str, note: Option<&str>) -> Res<()> {
        self.conn
            .lock()
            .execute(
                "UPDATE review SET state = ?2, decision = ?3, decided_by = ?4, note = ?5, decided = ?6 WHERE id = ?1",
                params![id, state, decision, by, note, now_ms()],
            )
            .map_err(db)?;
        Ok(())
    }

    // ---- what the owner's decisions taught the tag classifier ---------------------------------------------

    pub fn add_feedback(&self, tag: &str, words: &[String], delta: f64) {
        let mut c = self.conn.lock();
        let Ok(tx) = c.transaction() else { return };
        for w in words {
            let _ = tx.execute(
                "INSERT INTO feedback (tag, word, n) VALUES (?1, ?2, ?3) ON CONFLICT(tag, word) DO UPDATE SET n = n + ?3",
                params![tag, w, delta],
            );
        }
        let _ = tx.commit();
    }

    /// The summed feedback of `words` for every tag that has some.
    pub fn feedback(&self, words: &[String]) -> Vec<(String, f64)> {
        let c = self.conn.lock();
        let mut out: std::collections::BTreeMap<String, f64> = Default::default();
        let Ok(mut st) = c.prepare("SELECT tag, n FROM feedback WHERE word = ?1") else {
            return vec![];
        };
        for w in words {
            if let Ok(rows) = st.query_map([w], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))) {
                for (t, n) in rows.flatten() {
                    *out.entry(t).or_default() += n;
                }
            }
        }
        out.into_iter().collect()
    }

    // ---- built KMs -----------------------------------------------------------------------------------------

    pub fn put_artifact(&self, task: &str, a: &ArtifactRow) -> Res<()> {
        self.conn
            .lock()
            .execute(
                "INSERT INTO artifacts (task, id, version, kind, stage, draft, key, content_id, imported)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(task, id, version) DO UPDATE SET stage = ?5, draft = ?6, key = ?7, content_id = ?8, imported = ?9",
                params![
                    task,
                    a.id,
                    a.version,
                    a.kind,
                    a.stage,
                    jstr(&a.draft),
                    a.key,
                    a.content_id,
                    a.imported as i64
                ],
            )
            .map_err(db)?;
        Ok(())
    }

    pub fn clear_artifacts(&self, task: &str) -> Res<()> {
        self.conn
            .lock()
            .execute("DELETE FROM artifacts WHERE task = ?1", [task])
            .map_err(db)?;
        Ok(())
    }

    pub fn clear_artifacts_stage(&self, task: &str, stage: &str) -> Res<()> {
        self.conn
            .lock()
            .execute("DELETE FROM artifacts WHERE task = ?1 AND stage = ?2", params![task, stage])
            .map_err(db)?;
        Ok(())
    }

    pub fn artifacts(&self, task: &str) -> Vec<ArtifactRow> {
        let c = self.conn.lock();
        let Ok(mut st) = c.prepare(
            "SELECT id, version, kind, stage, draft, key, content_id, imported FROM artifacts WHERE task = ?1 ORDER BY rowid",
        ) else {
            return vec![];
        };
        st.query_map([task], |r| {
            Ok(ArtifactRow {
                id: r.get(0)?,
                version: r.get(1)?,
                kind: r.get(2)?,
                stage: r.get(3)?,
                draft: js(r.get(4)?),
                key: r.get(5)?,
                content_id: r.get(6)?,
                imported: r.get::<_, i64>(7)? != 0,
            })
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    // ---- evaluation rows -----------------------------------------------------------------------------------

    pub fn put_eval(&self, task: &str, record: &str, v: &Value) -> Res<()> {
        self.conn
            .lock()
            .execute(
                "INSERT INTO evals (task, record, json) VALUES (?1, ?2, ?3) ON CONFLICT(task, record) DO UPDATE SET json = ?3",
                params![task, record, jstr(v)],
            )
            .map_err(db)?;
        Ok(())
    }

    pub fn evals(&self, task: &str) -> Vec<(String, Value)> {
        let c = self.conn.lock();
        let Ok(mut st) = c.prepare("SELECT record, json FROM evals WHERE task = ?1 ORDER BY record") else {
            return vec![];
        };
        st.query_map([task], |r| Ok((r.get::<_, String>(0)?, js(r.get(1)?))))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    pub fn clear_evals(&self, task: &str) {
        let _ = self.conn.lock().execute("DELETE FROM evals WHERE task = ?1", [task]);
    }
}

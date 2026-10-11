//! The run store: SQLite at `<home>/forge/runs.db`. A run is one step of a pipeline; the store keeps its arguments,
//! status, progress, result, every metric the worker reported, its artifacts and its log. The database is opened on
//! first use, so a hub that never trains creates nothing.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::engine::Fail;

/// Log rows kept per run; later lines are counted, not stored (a progress bar can print thousands).
pub const MAX_LOG_ROWS: i64 = 20_000;

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn io(e: impl std::fmt::Display) -> Fail {
    Fail::new(500, "io", format!("run store: {e}"))
}

/// (step, value) pairs of one metric.
type Series = Vec<(Option<i64>, Option<f64>)>;

pub struct Store {
    path: PathBuf,
    conn: Mutex<Option<Connection>>,
}

#[derive(Default, Clone)]
pub struct Filter {
    pub step: Option<String>,
    pub status: Option<String>,
    pub limit: usize,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS runs(
  id TEXT PRIMARY KEY, step TEXT NOT NULL, args TEXT NOT NULL, status TEXT NOT NULL,
  started REAL NOT NULL, finished REAL, pid INTEGER, exit_code INTEGER,
  progress_done REAL, progress_total REAL, progress_note TEXT,
  result TEXT, error TEXT, error_code TEXT, log_dropped INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS runs_started ON runs(started);
CREATE TABLE IF NOT EXISTS metrics(
  run_id TEXT NOT NULL, name TEXT NOT NULL, step INTEGER, value REAL, ts REAL NOT NULL);
CREATE INDEX IF NOT EXISTS metrics_run ON metrics(run_id, name);
CREATE TABLE IF NOT EXISTS events(
  id INTEGER PRIMARY KEY AUTOINCREMENT, run_id TEXT NOT NULL, ts REAL NOT NULL, level TEXT NOT NULL, message TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS events_run ON events(run_id, id);
CREATE TABLE IF NOT EXISTS artifacts(
  run_id TEXT NOT NULL, name TEXT NOT NULL, path TEXT NOT NULL, bytes INTEGER, kind TEXT NOT NULL, ts REAL NOT NULL);
CREATE INDEX IF NOT EXISTS artifacts_run ON artifacts(run_id);
";

impl Store {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            conn: Mutex::new(None),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, Fail> {
        let mut g = self.conn.lock();
        if g.is_none() {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir).map_err(io)?;
            }
            let c = Connection::open(&self.path).map_err(io)?;
            c.busy_timeout(std::time::Duration::from_secs(5)).map_err(io)?;
            c.pragma_update(None, "journal_mode", "WAL").map_err(io)?;
            c.pragma_update(None, "synchronous", "NORMAL").map_err(io)?;
            c.execute_batch(SCHEMA).map_err(io)?;
            // a run left "running" by a hub that died did not finish
            c.execute(
                "UPDATE runs SET status='interrupted', finished=?1, error=COALESCE(error,'the hub stopped while it was running') \
                 WHERE status='running'",
                params![now()],
            )
            .map_err(io)?;
            *g = Some(c);
        }
        f(g.as_ref().expect("opened above")).map_err(io)
    }

    pub fn create_run(&self, id: &str, step: &str, args: &Value) -> Result<(), Fail> {
        self.with(|c| {
            c.execute(
                "INSERT INTO runs(id, step, args, status, started) VALUES(?1, ?2, ?3, 'running', ?4)",
                params![id, step, args.to_string(), now()],
            )
            .map(|_| ())
        })
    }

    pub fn set_pid(&self, id: &str, pid: Option<u32>) -> Result<(), Fail> {
        self.with(|c| {
            c.execute("UPDATE runs SET pid=?2 WHERE id=?1", params![id, pid])
                .map(|_| ())
        })
    }

    pub fn set_progress(&self, id: &str, done: f64, total: f64, note: &str) -> Result<(), Fail> {
        self.with(|c| {
            c.execute(
                "UPDATE runs SET progress_done=?2, progress_total=?3, progress_note=?4 WHERE id=?1",
                params![id, done, total, note],
            )
            .map(|_| ())
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn finish(
        &self,
        id: &str,
        status: &str,
        exit_code: Option<i32>,
        result: Option<&Value>,
        error: Option<&str>,
        error_code: Option<&str>,
    ) -> Result<(), Fail> {
        self.with(|c| {
            c.execute(
                "UPDATE runs SET status=?2, finished=?3, exit_code=?4, result=?5, error=?6, error_code=?7 WHERE id=?1",
                params![
                    id,
                    status,
                    now(),
                    exit_code,
                    result.map(Value::to_string),
                    error,
                    error_code
                ],
            )
            .map(|_| ())
        })
    }

    pub fn add_metric(&self, id: &str, name: &str, step: Option<i64>, value: Option<f64>) -> Result<(), Fail> {
        self.with(|c| {
            c.execute(
                "INSERT INTO metrics(run_id, name, step, value, ts) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![id, name, step, value, now()],
            )
            .map(|_| ())
        })
    }

    /// Returns false when the run's log is full (the line was only counted).
    pub fn add_log(&self, id: &str, level: &str, message: &str) -> Result<bool, Fail> {
        self.with(|c| {
            let n: i64 = c.query_row("SELECT COUNT(*) FROM events WHERE run_id=?1", params![id], |r| r.get(0))?;
            if n >= MAX_LOG_ROWS {
                c.execute("UPDATE runs SET log_dropped=log_dropped+1 WHERE id=?1", params![id])?;
                return Ok(false);
            }
            c.execute(
                "INSERT INTO events(run_id, ts, level, message) VALUES(?1, ?2, ?3, ?4)",
                params![id, now(), level, message],
            )?;
            Ok(true)
        })
    }

    pub fn add_artifact(&self, id: &str, name: &str, path: &str, bytes: Option<u64>, kind: &str) -> Result<(), Fail> {
        self.with(|c| {
            c.execute(
                "INSERT INTO artifacts(run_id, name, path, bytes, kind, ts) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, name, path, bytes.map(|b| b as i64), kind, now()],
            )
            .map(|_| ())
        })
    }

    fn run_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
        let parse = |s: Option<String>| s.and_then(|s| serde_json::from_str::<Value>(&s).ok());
        let done: Option<f64> = r.get("progress_done")?;
        let total: Option<f64> = r.get("progress_total")?;
        let started: f64 = r.get("started")?;
        let finished: Option<f64> = r.get("finished")?;
        Ok(json!({
            "run": r.get::<_, String>("id")?,
            "step": r.get::<_, String>("step")?,
            "status": r.get::<_, String>("status")?,
            "started": started,
            "finished": finished,
            "seconds": finished.map(|f| ((f - started) * 10.0).round() / 10.0),
            "pid": r.get::<_, Option<i64>>("pid")?,
            "exit_code": r.get::<_, Option<i64>>("exit_code")?,
            "progress": {"done": done, "total": total, "note": r.get::<_, Option<String>>("progress_note")?,
                         "percent": match (done, total) { (Some(d), Some(t)) if t > 0.0 => Some(((d / t) * 1000.0).round() / 10.0), _ => None }},
            "args": parse(r.get("args")?),
            "result": parse(r.get("result")?),
            "error": r.get::<_, Option<String>>("error")?,
            "error_code": r.get::<_, Option<String>>("error_code")?,
            "log_dropped": r.get::<_, i64>("log_dropped")?,
        }))
    }

    pub fn get_run(&self, id: &str) -> Result<Option<Value>, Fail> {
        self.with(|c| {
            c.query_row("SELECT * FROM runs WHERE id=?1", params![id], Self::run_row)
                .optional()
        })
    }

    /// Newest first; `args` and `result` are left out of the list (use get_run for them).
    pub fn list_runs(&self, f: &Filter) -> Result<Vec<Value>, Fail> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT * FROM runs WHERE (?1 IS NULL OR step=?1) AND (?2 IS NULL OR status=?2) \
                 ORDER BY started DESC, rowid DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![f.step, f.status, f.limit.max(1) as i64], Self::run_row)?;
            rows.map(|r| {
                r.map(|mut v| {
                    if let Some(o) = v.as_object_mut() {
                        o.remove("args");
                        o.remove("result");
                    }
                    v
                })
            })
            .collect()
        })
    }

    pub fn count_runs(&self) -> Result<i64, Fail> {
        self.with(|c| c.query_row("SELECT COUNT(*) FROM runs", [], |r| r.get(0)))
    }

    /// The result of the newest finished run of a step.
    pub fn last_result(&self, step: &str) -> Result<Option<(String, Value)>, Fail> {
        self.with(|c| {
            let row: Option<(String, String)> = c
                .query_row(
                    "SELECT id, result FROM runs WHERE step=?1 AND status='done' AND result IS NOT NULL \
                     ORDER BY started DESC, rowid DESC LIMIT 1",
                    params![step],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            Ok(row.and_then(|(id, s)| serde_json::from_str(&s).ok().map(|v| (id, v))))
        })
    }

    /// Per metric name: count, first, last, min, max, and at most `max_points` evenly spaced [step, value] points.
    /// `only` limits it to one name.
    pub fn metrics(&self, id: &str, only: Option<&str>, max_points: usize) -> Result<Value, Fail> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT name, step, value FROM metrics WHERE run_id=?1 AND (?2 IS NULL OR name=?2) ORDER BY rowid",
            )?;
            let mut by: std::collections::BTreeMap<String, Series> = Default::default();
            for r in stmt.query_map(params![id, only], |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?)))? {
                let (n, s, v) = r?;
                by.entry(n).or_default().push((s, v));
            }
            let mut out = serde_json::Map::new();
            for (name, pts) in by {
                let vals: Vec<f64> = pts.iter().filter_map(|p| p.1).collect();
                let max_points = max_points.max(2);
                let keep: Vec<usize> = if pts.len() <= max_points {
                    (0..pts.len()).collect()
                } else {
                    (0..max_points).map(|i| i * (pts.len() - 1) / (max_points - 1)).collect()
                };
                out.insert(
                    name,
                    json!({
                        "count": pts.len(),
                        "first": vals.first(),
                        "last": vals.last(),
                        "min": vals.iter().cloned().reduce(f64::min),
                        "max": vals.iter().cloned().reduce(f64::max),
                        "points": keep.iter().map(|&i| json!([pts[i].0.unwrap_or(i as i64), pts[i].1])).collect::<Vec<_>>(),
                    }),
                );
            }
            Ok(Value::Object(out))
        })
    }

    pub fn artifacts(&self, id: &str) -> Result<Vec<Value>, Fail> {
        self.with(|c| {
            let mut stmt = c.prepare("SELECT name, path, bytes, kind FROM artifacts WHERE run_id=?1 ORDER BY rowid")?;
            let rows = stmt.query_map(params![id], |r| {
                Ok(json!({"name": r.get::<_, String>(0)?, "path": r.get::<_, String>(1)?,
                          "bytes": r.get::<_, Option<i64>>(2)?, "kind": r.get::<_, String>(3)?}))
            })?;
            rows.collect()
        })
    }

    /// The last `n` log lines, oldest first.
    pub fn log_tail(&self, id: &str, n: usize) -> Result<Vec<Value>, Fail> {
        self.with(|c| {
            let mut stmt =
                c.prepare("SELECT ts, level, message FROM events WHERE run_id=?1 ORDER BY id DESC LIMIT ?2")?;
            let mut rows: Vec<Value> = stmt
                .query_map(params![id, n as i64], |r| {
                    Ok(json!({"ts": r.get::<_, f64>(0)?, "level": r.get::<_, String>(1)?,
                              "message": r.get::<_, String>(2)?}))
                })?
                .collect::<rusqlite::Result<_>>()?;
            rows.reverse();
            Ok(rows)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (Store, PathBuf) {
        let d = std::env::temp_dir().join(format!("mm-forge-store-{}", uuid::Uuid::new_v4()));
        (Store::new(d.join("forge").join("runs.db")), d)
    }

    #[test]
    fn nothing_is_created_until_first_use() {
        let (s, d) = store();
        assert!(!d.exists());
        assert_eq!(s.count_runs().unwrap(), 0);
        assert!(s.path().is_file());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_run_keeps_its_progress_result_metrics_artifacts_and_log() {
        let (s, d) = store();
        s.create_run("r1", "preflight", &json!({"seed": 1})).unwrap();
        s.set_pid("r1", Some(4242)).unwrap();
        s.set_progress("r1", 1.0, 4.0, "step one").unwrap();
        for i in 0..5 {
            s.add_metric("r1", "loss", Some(i), Some(5.0 - i as f64)).unwrap();
        }
        s.add_metric("r1", "loss", Some(5), None).unwrap(); // a NaN
        s.add_metric("r1", "lr", Some(0), Some(0.001)).unwrap();
        s.add_artifact("r1", "w", "some/file.gguf", Some(10), "gguf").unwrap();
        s.add_log("r1", "info", "one").unwrap();
        s.add_log("r1", "stderr", "two").unwrap();
        let v = s.get_run("r1").unwrap().unwrap();
        assert_eq!(v["status"], "running");
        assert_eq!(v["pid"], 4242);
        assert_eq!(v["progress"]["percent"], 25.0);
        assert_eq!(v["args"], json!({"seed": 1}));

        s.finish("r1", "done", Some(0), Some(&json!({"ok": true})), None, None)
            .unwrap();
        let v = s.get_run("r1").unwrap().unwrap();
        assert_eq!(
            (v["status"].as_str(), v["result"].clone()),
            (Some("done"), json!({"ok": true}))
        );
        assert!(v["seconds"].as_f64().is_some());

        let m = s.metrics("r1", None, 100).unwrap();
        assert_eq!(m["loss"]["count"], 6);
        assert_eq!(
            (m["loss"]["first"].as_f64(), m["loss"]["last"].as_f64()),
            (Some(5.0), Some(1.0))
        );
        assert_eq!(
            (m["loss"]["min"].as_f64(), m["loss"]["max"].as_f64()),
            (Some(1.0), Some(5.0))
        );
        assert_eq!(m["lr"]["count"], 1);
        assert_eq!(s.metrics("r1", Some("lr"), 100).unwrap().as_object().unwrap().len(), 1);
        assert_eq!(s.artifacts("r1").unwrap()[0]["kind"], "gguf");
        let log = s.log_tail("r1", 10).unwrap();
        assert_eq!(
            log.iter().map(|l| l["message"].as_str().unwrap()).collect::<Vec<_>>(),
            ["one", "two"]
        );
        assert_eq!(s.log_tail("r1", 1).unwrap()[0]["message"], "two");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn long_metric_series_are_thinned_evenly_keeping_both_ends() {
        let (s, d) = store();
        s.create_run("r", "x", &json!({})).unwrap();
        for i in 0..1000 {
            s.add_metric("r", "m", Some(i), Some(i as f64)).unwrap();
        }
        let m = s.metrics("r", None, 11).unwrap();
        let pts = m["m"]["points"].as_array().unwrap();
        assert_eq!(pts.len(), 11);
        assert_eq!(pts[0], json!([0, 0.0]));
        assert_eq!(pts[10], json!([999, 999.0]));
        assert_eq!(m["m"]["count"], 1000);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn runs_list_newest_first_with_filters_and_without_bulk() {
        let (s, d) = store();
        for (id, step) in [("a", "preflight"), ("b", "dusty_convert"), ("c", "preflight")] {
            s.create_run(id, step, &json!({"big": "args"})).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        s.finish("a", "done", Some(0), Some(&json!({"device": "gpu"})), None, None)
            .unwrap();
        s.finish("b", "failed", Some(1), None, Some("bad"), Some("failed"))
            .unwrap();
        let all = s
            .list_runs(&Filter {
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            all.iter().map(|r| r["run"].as_str().unwrap()).collect::<Vec<_>>(),
            ["c", "b", "a"]
        );
        assert!(all[0].get("args").is_none() && all[0].get("result").is_none());
        let pre = s
            .list_runs(&Filter {
                step: Some("preflight".into()),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(pre.len(), 2);
        let failed = s
            .list_runs(&Filter {
                status: Some("failed".into()),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!((failed.len(), failed[0]["error"].as_str()), (1, Some("bad")));
        assert_eq!(
            s.list_runs(&Filter {
                limit: 1,
                ..Default::default()
            })
            .unwrap()
            .len(),
            1
        );
        assert_eq!(
            s.last_result("preflight").unwrap().unwrap(),
            ("a".to_string(), json!({"device": "gpu"}))
        );
        assert!(s.last_result("nope").unwrap().is_none());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_run_left_running_by_a_dead_hub_is_marked_interrupted_and_data_survives() {
        let (s, d) = store();
        s.create_run("r", "x", &json!({})).unwrap();
        s.add_metric("r", "m", Some(1), Some(2.0)).unwrap();
        drop(s);
        let s2 = Store::new(d.join("forge").join("runs.db"));
        let v = s2.get_run("r").unwrap().unwrap();
        assert_eq!(v["status"], "interrupted");
        assert!(v["error"].as_str().unwrap().contains("hub stopped"));
        assert_eq!(s2.metrics("r", None, 10).unwrap()["m"]["count"], 1);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_log_is_capped_and_the_overflow_counted() {
        let (s, d) = store();
        s.create_run("r", "x", &json!({})).unwrap();
        s.with(|c| {
            // fill to the cap in one statement instead of 20000 calls
            c.execute(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < ?1) \
                 INSERT INTO events(run_id, ts, level, message) SELECT 'r', 0, 'info', 'x' FROM n",
                params![MAX_LOG_ROWS],
            )
            .map(|_| ())
        })
        .unwrap();
        assert!(!s.add_log("r", "info", "over").unwrap());
        assert_eq!(s.get_run("r").unwrap().unwrap()["log_dropped"], 1);
        let _ = std::fs::remove_dir_all(d);
    }
}

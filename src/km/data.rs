//! Data KMs (km-spec.md §8): training records you can view, edit, approve and reject.
//!
//! A published data KM version never changes, so edits go to a working copy per KM id (`<home>/km/data.db`), seeded
//! from the version it was opened at. Every write appends to an append-only history with the complete new state of
//! the record (its `rev`, who, when, why), so any earlier revision, and with it any training run, can be reproduced.
//! `commit` turns the working copy into a new KM version with a `parent` link; the old version stays as it was.

use std::path::Path;

use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use semver::Version;
use serde_json::{json, Map, Value};

use super::manifest::Manifest;
use super::pack::{self, hash_file};
use super::store::{Record, Store, Trust};
use super::{now_ms, Res};
use crate::engine::Fail;

pub const STATUSES: [&str; 3] = ["pending", "approved", "rejected"];

fn db<E: std::fmt::Display>(e: E) -> Fail {
    Fail::new(500, "km_data", format!("KM data: {e}"))
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS work (
  km TEXT PRIMARY KEY, base_version TEXT NOT NULL, dirty INTEGER NOT NULL DEFAULT 0, seeded_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS records (
  km TEXT NOT NULL, rid TEXT NOT NULL, ord INTEGER NOT NULL, rev INTEGER NOT NULL, status TEXT NOT NULL,
  json TEXT NOT NULL, updated INTEGER NOT NULL, PRIMARY KEY (km, rid)
);
CREATE TABLE IF NOT EXISTS history (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, km TEXT NOT NULL, rid TEXT NOT NULL, rev INTEGER NOT NULL,
  at INTEGER NOT NULL, action TEXT NOT NULL, actor TEXT, note TEXT, json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS history_rec ON history(km, rid, rev);
";

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub status: Option<String>,
    pub split: Option<String>,
    pub tag: Option<String>,
    pub task: Option<String>,
    /// At least this `labels.quality`.
    pub min_quality: Option<f64>,
    /// `source.kind` (teacher, owner, ...).
    pub source_kind: Option<String>,
    /// Substring of the record's JSON (case-insensitive).
    pub text: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

pub struct DataStore {
    conn: Mutex<Connection>,
}

fn valid_status(s: &str) -> Res<()> {
    if STATUSES.contains(&s) {
        Ok(())
    } else {
        Err(Fail::invalid(format!(
            "status {s:?} must be one of {}",
            STATUSES.join(", ")
        )))
    }
}

impl DataStore {
    pub fn open(home: &Path) -> Res<DataStore> {
        let dir = home.join("km");
        std::fs::create_dir_all(&dir).map_err(|e| Fail::new(500, "io", format!("{}: {e}", dir.display())))?;
        let conn = Connection::open(dir.join("data.db")).map_err(db)?;
        conn.busy_timeout(std::time::Duration::from_secs(10)).map_err(db)?;
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        conn.execute_batch(SCHEMA).map_err(db)?;
        Ok(DataStore { conn: Mutex::new(conn) })
    }

    /// `(base_version, dirty)` of the working copy of a KM id.
    pub fn work(&self, km: &str) -> Option<(String, bool)> {
        self.conn
            .lock()
            .query_row("SELECT base_version, dirty FROM work WHERE km = ?1", params![km], |r| {
                Ok((r.get(0)?, r.get::<_, i64>(1)? != 0))
            })
            .optional()
            .ok()
            .flatten()
    }

    /// Opens the working copy from a version's records file, unless one exists.
    pub fn ensure_seeded(&self, km: &str, version: &str, records: &Path) -> Res<()> {
        if self.work(km).is_some() {
            return Ok(());
        }
        let text =
            std::fs::read_to_string(records).map_err(|e| Fail::invalid(format!("{}: {e}", records.display())))?;
        let mut rows: Vec<(String, Value)> = vec![];
        let mut seen = std::collections::HashSet::new();
        for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            let mut v: Value =
                serde_json::from_str(line).map_err(|e| Fail::invalid(format!("records line {}: {e}", i + 1)))?;
            let Some(o) = v.as_object_mut() else {
                return Err(Fail::invalid(format!("records line {}: not a JSON object", i + 1)));
            };
            let rid = o
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| Fail::invalid(format!("records line {}: every record needs a string id", i + 1)))?
                .to_string();
            if !seen.insert(rid.clone()) {
                return Err(Fail::invalid(format!(
                    "records line {}: id {rid:?} appears twice",
                    i + 1
                )));
            }
            let status = o.get("status").and_then(Value::as_str).unwrap_or("pending").to_string();
            valid_status(&status).map_err(|e| Fail::invalid(format!("records line {}: {}", i + 1, e.message)))?;
            o.insert("status".into(), json!(status));
            let rev = o.get("rev").and_then(Value::as_u64).filter(|r| *r > 0).unwrap_or(1);
            o.insert("rev".into(), json!(rev));
            rows.push((rid, v));
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(db)?;
        let now = now_ms();
        for (ord, (rid, v)) in rows.iter().enumerate() {
            let rev = v["rev"].as_u64().unwrap_or(1) as i64;
            let status = v["status"].as_str().unwrap_or("pending");
            tx.execute(
                "INSERT INTO records(km, rid, ord, rev, status, json, updated) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![km, rid, ord as i64, rev, status, v.to_string(), now],
            )
            .map_err(db)?;
            tx.execute(
                "INSERT INTO history(km, rid, rev, at, action, actor, note, json) VALUES (?1,?2,?3,?4,'seed',NULL,?5,?6)",
                params![km, rid, rev, now, format!("from {km}@{version}"), v.to_string()],
            )
            .map_err(db)?;
        }
        tx.execute(
            "INSERT INTO work(km, base_version, dirty, seeded_at) VALUES (?1, ?2, 0, ?3)",
            params![km, version, now],
        )
        .map_err(db)?;
        tx.commit().map_err(db)
    }

    pub fn counts(&self, km: &str) -> Value {
        let conn = self.conn.lock();
        let mut out = json!({"pending": 0, "approved": 0, "rejected": 0, "total": 0});
        if let Ok(mut st) = conn.prepare("SELECT status, COUNT(*) FROM records WHERE km = ?1 GROUP BY status") {
            if let Ok(rows) = st.query_map(params![km], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))) {
                for (s, n) in rows.flatten() {
                    out[&s] = json!(n);
                    out["total"] = json!(out["total"].as_i64().unwrap_or(0) + n);
                }
            }
        }
        out
    }

    pub fn list(&self, km: &str, f: &Filter) -> Res<(Vec<Value>, usize)> {
        let conn = self.conn.lock();
        let mut st = conn
            .prepare("SELECT json FROM records WHERE km = ?1 AND (?2 IS NULL OR status = ?2) ORDER BY ord")
            .map_err(db)?;
        let rows = st
            .query_map(params![km, f.status], |r| r.get::<_, String>(0))
            .map_err(db)?;
        let needle = f.text.as_ref().map(|t| t.to_lowercase());
        let mut out = vec![];
        for r in rows {
            let s = r.map_err(db)?;
            let v: Value = serde_json::from_str(&s).map_err(db)?;
            if f.split.as_ref().is_some_and(|x| v["split"].as_str() != Some(x)) {
                continue;
            }
            if f.task.as_ref().is_some_and(|x| v["task"].as_str() != Some(x)) {
                continue;
            }
            if f.min_quality
                .is_some_and(|q| v["labels"]["quality"].as_f64().is_none_or(|x| x < q))
            {
                continue;
            }
            if f.source_kind
                .as_ref()
                .is_some_and(|k| v["source"]["kind"].as_str() != Some(k))
            {
                continue;
            }
            if let Some(t) = &f.tag {
                if !v["tags"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(t)))
                {
                    continue;
                }
            }
            if let Some(n) = &needle {
                if !s.to_lowercase().contains(n) {
                    continue;
                }
            }
            out.push(v);
        }
        let total = out.len();
        let limit = if f.limit == 0 { 100 } else { f.limit.min(1000) };
        Ok((out.into_iter().skip(f.offset).take(limit).collect(), total))
    }

    pub fn get(&self, km: &str, rid: &str, with_history: bool) -> Res<Option<Value>> {
        let conn = self.conn.lock();
        let cur: Option<String> = conn
            .query_row(
                "SELECT json FROM records WHERE km = ?1 AND rid = ?2",
                params![km, rid],
                |r| r.get(0),
            )
            .optional()
            .map_err(db)?;
        let Some(cur) = cur else { return Ok(None) };
        let mut v: Value = serde_json::from_str(&cur).map_err(db)?;
        if with_history {
            let mut st = conn
                .prepare(
                    "SELECT rev, at, action, actor, note, json FROM history WHERE km = ?1 AND rid = ?2 ORDER BY seq",
                )
                .map_err(db)?;
            let rows = st
                .query_map(params![km, rid], |r| {
                    let j: String = r.get(5)?;
                    Ok(
                        json!({"rev": r.get::<_, i64>(0)?, "at": r.get::<_, i64>(1)?, "action": r.get::<_, String>(2)?,
                              "by": r.get::<_, Option<String>>(3)?, "note": r.get::<_, Option<String>>(4)?,
                              "record": serde_json::from_str::<Value>(&j).unwrap_or(Value::Null)}),
                    )
                })
                .map_err(db)?;
            v["history"] = json!(rows.flatten().collect::<Vec<_>>());
        }
        Ok(Some(v))
    }

    /// Edits a record: the fields of `patch` replace those of the record (`id` and `rev` cannot be set), `rev` goes
    /// up by one, and the new state is appended to the history. With `create`, a missing record is made (rev 1).
    /// With `expect_rev`, the edit only happens if the record is still at that revision.
    #[allow(clippy::too_many_arguments)]
    pub fn edit(
        &self,
        km: &str,
        rid: &str,
        patch: &Value,
        by: &str,
        note: Option<&str>,
        create: bool,
        expect_rev: Option<u64>,
    ) -> Res<Value> {
        let patch = patch
            .as_object()
            .ok_or_else(|| Fail::invalid("the record changes must be a JSON object"))?;
        if let Some(s) = patch.get("status").and_then(Value::as_str) {
            valid_status(s)?;
        } else if patch.contains_key("status") {
            return Err(Fail::invalid("status must be a string"));
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(db)?;
        let cur: Option<String> = tx
            .query_row(
                "SELECT json FROM records WHERE km = ?1 AND rid = ?2",
                params![km, rid],
                |r| r.get(0),
            )
            .optional()
            .map_err(db)?;
        let (mut rec, rev, action, ord): (Map<String, Value>, u64, &str, i64) = match cur {
            Some(s) => {
                let v: Value = serde_json::from_str(&s).map_err(db)?;
                let old_rev = v["rev"].as_u64().unwrap_or(1);
                if let Some(e) = expect_rev {
                    if e != old_rev {
                        return Err(Fail::new(
                            409,
                            "rev_mismatch",
                            format!("record {rid} is at rev {old_rev}, not {e}; nothing was changed"),
                        ));
                    }
                }
                let ord = tx
                    .query_row(
                        "SELECT ord FROM records WHERE km = ?1 AND rid = ?2",
                        params![km, rid],
                        |r| r.get(0),
                    )
                    .map_err(db)?;
                (v.as_object().cloned().unwrap_or_default(), old_rev + 1, "edit", ord)
            }
            None if create => {
                let ord: i64 = tx
                    .query_row(
                        "SELECT COALESCE(MAX(ord) + 1, 0) FROM records WHERE km = ?1",
                        params![km],
                        |r| r.get(0),
                    )
                    .map_err(db)?;
                let mut m = Map::new();
                m.insert("status".into(), json!("pending"));
                (m, 1, "create", ord)
            }
            None => {
                return Err(Fail::not_found(format!(
                    "no record {rid} (give create: true to make it)"
                )))
            }
        };
        for (k, v) in patch {
            if k != "id" && k != "rev" {
                rec.insert(k.clone(), v.clone());
            }
        }
        rec.insert("id".into(), json!(rid));
        rec.insert("rev".into(), json!(rev));
        rec.insert("edited_by".into(), json!(by));
        let status = rec
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending")
            .to_string();
        let v = Value::Object(rec);
        let now = now_ms();
        tx.execute(
            "INSERT INTO records(km, rid, ord, rev, status, json, updated) VALUES (?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(km, rid) DO UPDATE SET rev = ?4, status = ?5, json = ?6, updated = ?7",
            params![km, rid, ord, rev as i64, status, v.to_string(), now],
        )
        .map_err(db)?;
        tx.execute(
            "INSERT INTO history(km, rid, rev, at, action, actor, note, json) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![km, rid, rev as i64, now, action, by, note, v.to_string()],
        )
        .map_err(db)?;
        tx.execute("UPDATE work SET dirty = 1 WHERE km = ?1", params![km])
            .map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(v)
    }

    /// Approves or rejects the given records (or, with `ids` empty, every record in `from` status). Each change is a
    /// revision. Returns the ids changed.
    pub fn set_status(
        &self,
        km: &str,
        ids: &[String],
        from: Option<&str>,
        status: &str,
        by: &str,
        note: Option<&str>,
    ) -> Res<Vec<String>> {
        valid_status(status)?;
        let targets: Vec<String> = if ids.is_empty() {
            let from =
                from.ok_or_else(|| Fail::invalid("give ids, or from (a status) to change every record in it"))?;
            valid_status(from)?;
            self.ids_with_status(km, from)?
        } else {
            ids.to_vec()
        };
        // check them all first so a typo changes nothing
        for rid in &targets {
            if self.get(km, rid, false)?.is_none() {
                return Err(Fail::not_found(format!("no record {rid}; nothing was changed")));
            }
        }
        let mut done = vec![];
        for rid in targets {
            let cur = self.get(km, &rid, false)?.unwrap_or(Value::Null);
            if cur["status"].as_str() == Some(status) {
                continue;
            }
            self.edit(km, &rid, &json!({"status": status}), by, note, false, None)?;
            done.push(rid);
        }
        Ok(done)
    }

    fn ids_with_status(&self, km: &str, status: &str) -> Res<Vec<String>> {
        let conn = self.conn.lock();
        let mut st = conn
            .prepare("SELECT rid FROM records WHERE km = ?1 AND status = ?2 ORDER BY ord")
            .map_err(db)?;
        let rows = st
            .query_map(params![km, status], |r| r.get::<_, String>(0))
            .map_err(db)?;
        rows.collect::<Result<_, _>>().map_err(db)
    }

    fn records_jsonl(&self, km: &str) -> Res<Vec<u8>> {
        let conn = self.conn.lock();
        let mut st = conn
            .prepare("SELECT json FROM records WHERE km = ?1 ORDER BY ord")
            .map_err(db)?;
        let rows = st.query_map(params![km], |r| r.get::<_, String>(0)).map_err(db)?;
        let mut out = vec![];
        for r in rows {
            out.extend(r.map_err(db)?.into_bytes());
            out.push(b'\n');
        }
        Ok(out)
    }

    fn mark_committed(&self, km: &str, version: &str) {
        let _ = self.conn.lock().execute(
            "UPDATE work SET base_version = ?2, dirty = 0 WHERE km = ?1",
            params![km, version],
        );
    }

    /// Makes the working copy a new KM version (`version`, else the newest installed with its patch number up by one)
    /// whose `parent` is the version the copy was based on. The old versions are untouched.
    pub fn commit(&self, store: &Store, km: &str, version: Option<&str>, trust: &Trust) -> Res<Record> {
        let (base, dirty) = self
            .work(km)
            .ok_or_else(|| Fail::not_found(format!("{km} has no working copy; list or edit its records first")))?;
        if !dirty && version.is_none() {
            return Err(Fail::new(
                409,
                "nothing_to_commit",
                format!("{km} has no edits since {base}"),
            ));
        }
        let parent = store.get(&format!("{km}@{base}"))?.ok_or_else(|| {
            Fail::not_found(format!(
                "{km}@{base}, the version this copy was made from, is not installed"
            ))
        })?;
        let new_version = match version {
            Some(v) => Version::parse(v).map_err(|e| Fail::invalid(format!("version {v:?}: {e}")))?,
            None => {
                let newest = store
                    .versions(km)?
                    .into_iter()
                    .next()
                    .map(|r| r.version())
                    .unwrap_or(parent.version());
                Version::new(newest.major, newest.minor, newest.patch + 1)
            }
        };
        let key = format!("{km}@{new_version}");
        if store.get(&key)?.is_some() {
            return Err(Fail::new(
                409,
                "version_exists",
                format!("{key} is already installed; give another version"),
            ));
        }
        let src = store.dir_of(&parent.key);
        let old = Manifest::from_file(&src.join("km.toml"))?;
        let records_path = old
            .payload_of("records")
            .map(|p| p.path.clone())
            .ok_or_else(|| Fail::invalid(format!("{} has no records payload", parent.key)))?;
        let bytes = self.records_jsonl(km)?;

        let stage = store
            .tmp_dir()
            .join(format!("commit-{}", uuid::Uuid::new_v4().simple()));
        let dir = stage.join(&key);
        let build = || -> Res<Manifest> {
            for name in old.allowed_files() {
                if name == "km.toml" || name == "SIGNATURE" || name == records_path {
                    continue;
                }
                let from = src.join(&name);
                if from.is_file() {
                    let to = dir.join(&name);
                    std::fs::create_dir_all(to.parent().unwrap_or(&dir))
                        .map_err(|e| Fail::new(500, "io", e.to_string()))?;
                    std::fs::copy(&from, &to).map_err(|e| Fail::new(500, "io", e.to_string()))?;
                }
            }
            let rp = dir.join(&records_path);
            std::fs::create_dir_all(rp.parent().unwrap_or(&dir)).map_err(|e| Fail::new(500, "io", e.to_string()))?;
            std::fs::write(&rp, &bytes).map_err(|e| Fail::new(500, "io", e.to_string()))?;
            let (sha, size) = hash_file(&rp).map_err(|e| Fail::new(500, "io", e.to_string()))?;
            let mut raw = old.raw.clone();
            raw.insert("version".into(), toml::Value::String(new_version.to_string()));
            raw.insert("parent".into(), toml::Value::String(parent.key.clone()));
            if let Ok(d) = chrono::Utc::now()
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
                .parse::<toml::value::Datetime>()
            {
                raw.insert("created".into(), toml::Value::Datetime(d));
            }
            if let Some(toml::Value::Array(items)) = raw.get_mut("payload") {
                for it in items.iter_mut() {
                    if let Some(t) = it.as_table_mut() {
                        if t.get("path").and_then(toml::Value::as_str) == Some(records_path.as_str()) {
                            t.insert("bytes".into(), toml::Value::Integer(size as i64));
                            t.insert("sha256".into(), toml::Value::String(sha.clone()));
                        }
                    }
                }
            }
            let text = toml::to_string(&raw).map_err(|e| Fail::new(500, "io", format!("km.toml: {e}")))?;
            std::fs::write(dir.join("km.toml"), &text).map_err(|e| Fail::new(500, "io", e.to_string()))?;
            let m = Manifest::parse(&text)?;
            pack::verify_dir(&dir, &m)?;
            Ok(m)
        };
        let m = match build() {
            Ok(m) => m,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&stage);
                return Err(e);
            }
        };
        let (rec, _) = store.install(&stage, &m, trust, 0)?;
        self.mark_committed(km, &new_version.to_string());
        Ok(rec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::km::pack::{SigState, Source};
    use crate::km::sample;

    fn own() -> Trust {
        Trust {
            source: Source::Own,
            sig: SigState::Unsigned,
            overridden: false,
        }
    }

    fn setup() -> (crate::testing::TestDir, Store, DataStore) {
        let home = crate::testing::temp_home();
        let s = Store::open(&home).unwrap();
        let d = DataStore::open(&home).unwrap();
        let dir = sample::data(
            &home.join("src"),
            "data.rust",
            "1.0.0",
            &[
                sample::record("r1", "how to spawn", "use tokio::spawn", "pending"),
                sample::record("r2", "what is Send", "a marker trait", "approved"),
                sample::record("r3", "bad one", "wrong", "pending"),
            ],
        );
        let m = Manifest::from_file(&dir.join("km.toml")).unwrap();
        let stage = s.stage_dir(&dir, &m).unwrap();
        s.install(&stage, &m, &own(), 0).unwrap();
        d.ensure_seeded(
            "data.rust",
            "1.0.0",
            &s.dir_of("data.rust@1.0.0").join("payload/records.jsonl"),
        )
        .unwrap();
        (home, s, d)
    }

    #[test]
    fn records_are_listed_and_filtered() {
        let (home, _, d) = setup();
        let (all, total) = d.list("data.rust", &Filter::default()).unwrap();
        assert_eq!((all.len(), total), (3, 3));
        let c = d.counts("data.rust");
        assert_eq!(
            (c["pending"].as_i64(), c["approved"].as_i64(), c["total"].as_i64()),
            (Some(2), Some(1), Some(3))
        );
        let (p, _) = d
            .list(
                "data.rust",
                &Filter {
                    status: Some("pending".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            p.iter().map(|r| r["id"].as_str().unwrap()).collect::<Vec<_>>(),
            vec!["r1", "r3"]
        );
        let (t, _) = d
            .list(
                "data.rust",
                &Filter {
                    text: Some("MARKER".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(t.len(), 1);
        let (g, _) = d
            .list(
                "data.rust",
                &Filter {
                    tag: Some("task:codegen".into()),
                    split: Some("train".into()),
                    task: Some("chat".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(g.len(), 3);
        let (none, _) = d
            .list(
                "data.rust",
                &Filter {
                    tag: Some("nope".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(none.is_empty());
        let (page, total) = d
            .list(
                "data.rust",
                &Filter {
                    limit: 1,
                    offset: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!((page[0]["id"].as_str(), total), (Some("r2"), 3));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn editing_bumps_rev_and_keeps_every_revision() {
        let (home, _, d) = setup();
        let v = d
            .edit(
                "data.rust",
                "r1",
                &json!({"output": "use tokio::spawn(async { .. })"}),
                "owner",
                Some("clearer"),
                false,
                None,
            )
            .unwrap();
        assert_eq!(v["rev"], 2);
        assert_eq!(v["edited_by"], "owner");
        assert_eq!(v["status"], "pending", "an edit alone does not change the status");
        let v = d
            .edit(
                "data.rust",
                "r1",
                &json!({"output": "third", "id": "evil", "rev": 99}),
                "owner",
                None,
                false,
                None,
            )
            .unwrap();
        assert_eq!(v["rev"], 3, "rev and id cannot be set by a patch");
        assert_eq!(v["id"], "r1");
        let full = d.get("data.rust", "r1", true).unwrap().unwrap();
        let hist = full["history"].as_array().unwrap();
        assert_eq!(hist.len(), 3);
        assert_eq!(hist[0]["action"], "seed");
        assert_eq!(hist[0]["record"]["output"], "use tokio::spawn");
        assert_eq!(hist[1]["record"]["output"], "use tokio::spawn(async { .. })");
        assert_eq!(hist[1]["note"], "clearer");
        assert_eq!(
            hist.iter().map(|h| h["rev"].as_i64().unwrap()).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(full["output"], "third");
        // expect_rev guards against a stale editor
        let e = d
            .edit(
                "data.rust",
                "r1",
                &json!({"output": "x"}),
                "owner",
                None,
                false,
                Some(1),
            )
            .unwrap_err();
        assert_eq!(e.code, "rev_mismatch");
        assert_eq!(d.get("data.rust", "r1", false).unwrap().unwrap()["rev"], 3);
        assert!(d
            .edit("data.rust", "r1", &json!({"status": "maybe"}), "o", None, false, None)
            .is_err());
        assert!(d
            .edit("data.rust", "r1", &json!("nope"), "o", None, false, None)
            .is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn creating_needs_the_flag_and_starts_at_rev_one() {
        let (home, _, d) = setup();
        assert_eq!(
            d.edit("data.rust", "r9", &json!({"output": "x"}), "o", None, false, None)
                .unwrap_err()
                .status,
            404
        );
        let v = d
            .edit(
                "data.rust",
                "r9",
                &json!({"output": "x", "input": []}),
                "o",
                None,
                true,
                None,
            )
            .unwrap();
        assert_eq!((v["rev"].as_u64(), v["status"].as_str()), (Some(1), Some("pending")));
        let (all, _) = d.list("data.rust", &Filter::default()).unwrap();
        assert_eq!(all.last().unwrap()["id"], "r9");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn approve_and_reject_are_revisions_and_check_ids_first() {
        let (home, _, d) = setup();
        let done = d
            .set_status("data.rust", &["r1".into()], None, "approved", "owner", None)
            .unwrap();
        assert_eq!(done, vec!["r1"]);
        assert_eq!(d.get("data.rust", "r1", false).unwrap().unwrap()["rev"], 2);
        // r2 is already approved: skipped, no new revision
        let done = d
            .set_status("data.rust", &["r2".into()], None, "approved", "owner", None)
            .unwrap();
        assert!(done.is_empty());
        assert_eq!(d.get("data.rust", "r2", false).unwrap().unwrap()["rev"], 1);
        // a typo in the list changes nothing
        let e = d
            .set_status(
                "data.rust",
                &["r3".into(), "r0".into()],
                None,
                "rejected",
                "owner",
                None,
            )
            .unwrap_err();
        assert_eq!(e.status, 404);
        assert_eq!(d.get("data.rust", "r3", false).unwrap().unwrap()["status"], "pending");
        // by status
        let done = d
            .set_status("data.rust", &[], Some("pending"), "rejected", "owner", Some("bulk"))
            .unwrap();
        assert_eq!(done, vec!["r3"]);
        assert!(d.set_status("data.rust", &[], None, "rejected", "o", None).is_err());
        let c = d.counts("data.rust");
        assert_eq!(
            (c["approved"].as_i64(), c["rejected"].as_i64(), c["pending"].as_i64()),
            (Some(2), Some(1), Some(0))
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn committing_makes_a_new_version_with_a_parent_and_leaves_the_old_one_alone() {
        let (home, s, d) = setup();
        let e = d.commit(&s, "data.rust", None, &own()).unwrap_err();
        assert_eq!(e.code, "nothing_to_commit");
        d.edit(
            "data.rust",
            "r1",
            &json!({"output": "better"}),
            "owner",
            None,
            false,
            None,
        )
        .unwrap();
        d.set_status("data.rust", &["r3".into()], None, "rejected", "owner", None)
            .unwrap();
        let before = std::fs::read(s.dir_of("data.rust@1.0.0").join("payload/records.jsonl")).unwrap();
        let rec = d.commit(&s, "data.rust", None, &own()).unwrap();
        assert_eq!(rec.key, "data.rust@1.0.1");
        assert_eq!(rec.parent.as_deref(), Some("data.rust@1.0.0"));
        // the old version is byte-for-byte what it was
        assert_eq!(
            std::fs::read(s.dir_of("data.rust@1.0.0").join("payload/records.jsonl")).unwrap(),
            before
        );
        // the new one carries the edits, verified
        let m = Manifest::from_file(&s.dir_of("data.rust@1.0.1").join("km.toml")).unwrap();
        pack::verify_dir(&s.dir_of("data.rust@1.0.1"), &m).unwrap();
        let text = std::fs::read_to_string(s.dir_of("data.rust@1.0.1").join("payload/records.jsonl")).unwrap();
        let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["output"], "better");
        assert_eq!(lines[0]["rev"], 2);
        assert_eq!(lines[2]["status"], "rejected");
        assert_eq!(m.parent.as_deref(), Some("data.rust@1.0.0"));
        assert!(m.unknown.is_empty(), "{:?}", m.unknown);
        // the working copy now builds on the new version, with nothing left to commit
        assert_eq!(d.work("data.rust"), Some(("1.0.1".into(), false)));
        assert_eq!(
            d.commit(&s, "data.rust", None, &own()).unwrap_err().code,
            "nothing_to_commit"
        );
        // an explicit version that exists is refused
        d.edit("data.rust", "r2", &json!({"output": "y"}), "o", None, false, None)
            .unwrap();
        assert_eq!(
            d.commit(&s, "data.rust", Some("1.0.0"), &own()).unwrap_err().code,
            "version_exists"
        );
        let rec = d.commit(&s, "data.rust", Some("2.0.0"), &own()).unwrap();
        assert_eq!(rec.parent.as_deref(), Some("data.rust@1.0.1"));
        assert_eq!(std::fs::read_dir(s.tmp_dir()).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn seeding_validates_the_records_file() {
        let home = crate::testing::temp_home();
        let d = DataStore::open(&home).unwrap();
        let f = home.join("r.jsonl");
        std::fs::write(&f, "{\"id\":\"a\"}\n{\"id\":\"a\"}\n").unwrap();
        assert!(d.ensure_seeded("k", "1.0.0", &f).unwrap_err().message.contains("twice"));
        std::fs::write(&f, "[1]\n").unwrap();
        assert!(d
            .ensure_seeded("k", "1.0.0", &f)
            .unwrap_err()
            .message
            .contains("not a JSON object"));
        std::fs::write(&f, "{\"id\":\"a\",\"status\":\"weird\"}\n").unwrap();
        assert!(d
            .ensure_seeded("k", "1.0.0", &f)
            .unwrap_err()
            .message
            .contains("status"));
        std::fs::write(&f, "{\"id\":\"a\"}\n").unwrap();
        d.ensure_seeded("k", "1.0.0", &f).unwrap();
        assert_eq!(
            d.get("k", "a", false).unwrap().unwrap()["status"],
            "pending",
            "defaults are filled in"
        );
        assert!(d.work("k").is_some());
        let _ = std::fs::remove_dir_all(&home);
    }
}
